//! `shprd agent spawn` and `shprd agent prompt`: put an agent on a task.
//!
//! Spawning is the one verb that has effects outside the log, so all of it lives
//! in [`spawn`], which takes the [`Mux`] as a parameter: the binary hands it a
//! real adapter, and the tests hand it a [`shprd_mux::fake::FakeMux`]. Nothing in
//! this module reads the environment or starts a process of its own.
//!
//! A spawn is one attempt at one task: it picks an agent, selects the skills
//! that attempt must load, writes the brief, creates the worktree, opens the
//! agent's tab, starts the CLI there and only then records the attempt. Every
//! step after the worktree undoes the earlier ones when it fails, so a refused
//! or broken spawn leaves neither a worktree nor an event behind.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::anyhow;
use shprd_core::config::Skills;
use shprd_core::event::{EventKind, ResourceAction, ResourceKind};
use shprd_core::state::{RunState, Task};
use shprd_git::worktree::{self, Layout, Worktree};
use shprd_mux::{Mux, naming};
use shprd_roster::skills::{CompatEntry, Role as SkillRole, Verdict};
use shprd_roster::spec::LaunchSpec;

use crate::ctx::{CliError, Ctx};

/// One agent CLI the spawn can launch.
#[derive(Debug, Clone)]
pub struct Program {
    /// The spec id, which is also the program name, e.g. `claude`.
    pub id: String,
    /// Whether the CLI is installed on this machine.
    pub installed: bool,
    /// How to start it and how to pass it a model, an effort and a prompt.
    pub launch: LaunchSpec,
}

/// Everything [`spawn`] needs from the caller's environment.
///
/// It is plain data plus the run log, so a test builds one without touching a
/// real home directory, a real config file or a real CLI.
pub struct SpawnEnv<'a> {
    /// The run log: where events are checked and appended.
    pub ctx: &'a Ctx,
    /// The worktree root and branch namespace.
    pub layout: Layout,
    /// The directory brief files are written under, `<home>/.shprd/briefs`.
    pub briefs_dir: PathBuf,
    /// The run this task belongs to.
    pub run: String,
    /// The branch a worktree is cut from: the run's integration branch.
    pub base_branch: String,
    /// The repository's directory name, which the worktree path carries.
    pub repo_name: String,
    /// Every CLI the machine has a spec for, in spec order.
    pub programs: Vec<Program>,
    /// The configured worker agents, in fallback order.
    pub workers: Vec<String>,
    /// `[skills]` from the config: the skills every worker is given.
    pub skills: Skills,
    /// The curated skill compatibility list.
    pub compat: Vec<CompatEntry>,
    /// The environment the launched CLI would inherit.
    pub base_env: BTreeMap<String, String>,
}

/// What the caller asked for.
#[derive(Debug, Clone)]
pub struct SpawnRequest {
    /// The task to work on.
    pub task: String,
    /// The agent to use, as `<program>/<model>`; `None` picks a worker.
    pub agent: Option<String>,
    /// The effort level for this attempt, when one was asked for.
    pub effort: Option<String>,
}

/// What a successful spawn did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnReport {
    /// The task that got the agent.
    pub task: String,
    /// The attempt number this spawn recorded.
    pub attempt: u32,
    /// The program that was launched, e.g. `claude`.
    pub agent: String,
    /// The model it was launched with, when one was chosen.
    pub model: Option<String>,
    /// The worktree the agent works in.
    pub worktree: PathBuf,
    /// The branch the agent commits on.
    pub branch: String,
    /// The title of the tab the agent runs in.
    pub tab: String,
    /// The brief file the agent was pointed at.
    pub brief: PathBuf,
    /// The skills the attempt must load, in the order they were selected.
    pub skills: Vec<String>,
}

/// The agent a spawn picked, with everything needed to start it.
#[derive(Debug, Clone)]
struct Pick {
    /// The program name, which is the harness spec id.
    agent: String,
    /// The model to select, when the agent string carried one.
    model: Option<String>,
    /// How to start the program.
    launch: LaunchSpec,
}

/// Spawns an agent for one task attempt.
///
/// The order is deliberate: every rule is checked and every decision is made
/// before the first side effect, and the effects are undone in reverse if a
/// later one fails.
pub fn spawn(
    mux: &dyn Mux,
    env: &SpawnEnv<'_>,
    req: &SpawnRequest,
) -> Result<SpawnReport, CliError> {
    let state = env.ctx.state()?;
    let task = task_of(&state, &req.task)?;
    let pick = choose_agent(req.agent.as_deref(), &env.workers, &env.programs)?;
    let attempt = task.attempts.len() as u32 + 1;
    let worktree = env.layout.path(&env.repo_name, &env.run, &task.id);
    let branch = env.layout.branch_name(&env.run, &task.id);
    let skills = select_skills(&env.skills, &env.compat, &task.tags);
    let tab = naming::tab_title(naming::Role::Worker, &pick.agent, &task.id, false);
    let brief_path = brief_path(&env.briefs_dir, &env.run, &task.id, attempt);
    let kind = EventKind::Spawned {
        task: task.id.clone(),
        attempt,
        agent: pick.agent.clone(),
        model: pick.model.clone(),
        worktree: worktree.display().to_string(),
        branch: branch.clone(),
    };
    // Refuse before any effect: a refused spawn must leave nothing behind.
    env.ctx.check(&kind)?;
    for resource in resource_kinds(&task.id, &worktree, &branch) {
        env.ctx.check(&resource)?;
    }
    let brief = render_brief(&BriefInput {
        task,
        run: &env.run,
        attempt,
        agent: &pick.agent,
        model: pick.model.as_deref(),
        branch: &branch,
        worktree: &worktree.display().to_string(),
        skills: &skills,
    });
    write_brief(&brief_path, &brief)?;
    let created = create_worktree(env, &task.id)?;
    let _ = mux;
    // The log is written last, so a failure here leaves nothing recorded and the
    // worktree is taken back out.
    if let Err(err) = record_attempt(env, &task.id, attempt, &pick, &created) {
        discard_worktree(env, &created);
        return Err(err);
    }
    Ok(SpawnReport {
        task: task.id.clone(),
        attempt,
        agent: pick.agent,
        model: pick.model,
        worktree: created.path,
        branch: created.branch,
        tab,
        brief: brief_path,
        skills,
    })
}

/// The ledger entries a spawn writes for its worktree and branch.
fn resource_kinds(task: &str, worktree: &Path, branch: &str) -> [EventKind; 2] {
    [
        EventKind::Resource {
            action: ResourceAction::Created,
            kind: ResourceKind::Worktree,
            name: worktree.display().to_string(),
            task: Some(task.to_string()),
        },
        EventKind::Resource {
            action: ResourceAction::Created,
            kind: ResourceKind::Branch,
            name: branch.to_string(),
            task: Some(task.to_string()),
        },
    ]
}

/// Creates the attempt's worktree from the integration branch.
fn create_worktree(env: &SpawnEnv<'_>, task: &str) -> Result<Worktree, CliError> {
    worktree::create(&env.ctx.root, &env.layout, &env.run, task, &env.base_branch).map_err(|err| {
        CliError::other(anyhow!(err).context(format!("creating a worktree for {task}")))
    })
}

/// Removes a worktree this spawn created, ignoring the failure: the caller is
/// already returning a more useful error.
fn discard_worktree(env: &SpawnEnv<'_>, created: &Worktree) {
    if let Err(err) = worktree::remove(&env.ctx.root, &env.layout, created, true) {
        eprintln!(
            "warning: could not remove {}: {err}",
            created.path.display()
        );
    }
}

/// Appends the worktree, the branch and then the attempt itself.
fn record_attempt(
    env: &SpawnEnv<'_>,
    task: &str,
    attempt: u32,
    pick: &Pick,
    created: &Worktree,
) -> Result<(), CliError> {
    for resource in resource_kinds(task, &created.path, &created.branch) {
        env.ctx.commit(resource)?;
    }
    env.ctx.commit(EventKind::Spawned {
        task: task.to_string(),
        attempt,
        agent: pick.agent.clone(),
        model: pick.model.clone(),
        worktree: created.path.display().to_string(),
        branch: created.branch.clone(),
    })?;
    Ok(())
}

/// The task `id` names, or an error saying the run plan has no such task.
fn task_of<'a>(state: &'a RunState, id: &str) -> Result<&'a Task, CliError> {
    state.tasks.get(id).ok_or_else(|| {
        CliError::other(anyhow!(
            "no task {id:?} in this run: add it with `shprd agent task add`"
        ))
    })
}

/// The agent string for a worker: `<program>`, or `<program>/<model>`.
fn parse_agent(raw: &str) -> (String, Option<String>) {
    let raw = raw.trim();
    match raw.split_once('/') {
        Some((program, model)) if !model.is_empty() => {
            (program.to_string(), Some(model.to_string()))
        }
        _ => (raw.to_string(), None),
    }
}

/// Picks the agent for an attempt.
///
/// An explicit `--agent` wins even when its CLI is not installed, because the
/// human may be pointing at a wrapper script; without one, the first configured
/// worker whose CLI *is* installed takes the attempt, so an unattended run never
/// opens an empty tab.
fn choose_agent(
    explicit: Option<&str>,
    workers: &[String],
    programs: &[Program],
) -> Result<Pick, CliError> {
    if let Some(raw) = explicit {
        let (agent, model) = parse_agent(raw);
        let program = programs
            .iter()
            .find(|program| program.id == agent)
            .ok_or_else(|| {
                CliError::other(anyhow!(
                    "no harness spec for {agent:?} in the data directory"
                ))
            })?;
        return Ok(Pick {
            agent,
            model,
            launch: program.launch.clone(),
        });
    }
    for raw in workers {
        let (agent, model) = parse_agent(raw);
        if let Some(program) = programs
            .iter()
            .find(|program| program.id == agent && program.installed)
        {
            return Ok(Pick {
                agent,
                model,
                launch: program.launch.clone(),
            });
        }
    }
    let configured: Vec<&str> = workers.iter().map(String::as_str).collect();
    Err(CliError::other(anyhow!(
        "no installed worker CLI: pass --agent, or set one with `shprd set workers` \
         (configured: {})",
        if configured.is_empty() {
            "none".to_string()
        } else {
            configured.join(", ")
        }
    )))
}

/// The skills an attempt must load.
///
/// That is every configured worker skill, plus every curated skill whose
/// verdict is `allow`, whose roles include the worker and whose tags intersect
/// the task's tags. A skill the compatibility list denies is never included,
/// even when the config names it, because the curated verdict is the maintainer's
/// decision about whether an unattended agent may run it at all.
pub fn select_skills(skills: &Skills, compat: &[CompatEntry], tags: &[String]) -> Vec<String> {
    let denied: Vec<&str> = compat
        .iter()
        .filter(|entry| entry.verdict == Verdict::Deny)
        .map(|entry| entry.name.as_str())
        .collect();
    let mut chosen: Vec<String> = Vec::new();
    let push = |name: &str, chosen: &mut Vec<String>| {
        if !denied.contains(&name) && !chosen.iter().any(|kept| kept == name) {
            chosen.push(name.to_string());
        }
    };
    for name in &skills.workers {
        push(name, &mut chosen);
    }
    for entry in compat {
        let tagged = entry.tags.iter().any(|tag| tags.contains(tag));
        let for_workers = entry.roles.iter().any(|role| *role == SkillRole::Worker);
        if entry.verdict == Verdict::Allow && for_workers && tagged {
            push(&entry.name, &mut chosen);
        }
    }
    chosen
}

/// Where a brief file lives: `<briefs>/<run>/<task>-a<attempt>.md`.
fn brief_path(briefs: &Path, run: &str, task: &str, attempt: u32) -> PathBuf {
    briefs.join(run).join(format!("{task}-a{attempt}.md"))
}

/// Writes a brief file, creating its directory first.
fn write_brief(path: &Path, text: &str) -> Result<(), CliError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| {
            CliError::other(anyhow!(err).context(format!("creating {}", parent.display())))
        })?;
    }
    std::fs::write(path, text)
        .map_err(|err| CliError::other(anyhow!(err).context(format!("writing {}", path.display()))))
}

/// What a brief says, as data, so the rendering is testable on its own.
pub struct BriefInput<'a> {
    /// The task the attempt works on.
    pub task: &'a Task,
    /// The run id.
    pub run: &'a str,
    /// The attempt number.
    pub attempt: u32,
    /// The program the attempt runs.
    pub agent: &'a str,
    /// The model it runs, when one was chosen.
    pub model: Option<&'a str>,
    /// The branch the agent commits on.
    pub branch: &'a str,
    /// The worktree the agent works in.
    pub worktree: &'a str,
    /// The skills the attempt must load first.
    pub skills: &'a [String],
}

/// Renders the brief an agent reads before its first turn.
///
/// It states only what the agent cannot look up faster itself: where it works,
/// what "done" means, what it may touch, which skills to load, and how to report.
pub fn render_brief(input: &BriefInput<'_>) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {} {}\n\n", input.task.id, input.task.title));
    out.push_str(&format!(
        "run `{}`, attempt {}, agent `{}`{}\n",
        input.run,
        input.attempt,
        input.agent,
        match input.model {
            Some(model) => format!(" ({model})"),
            None => String::new(),
        }
    ));
    out.push_str(&format!(
        "branch `{}` in worktree `{}`\n\n",
        input.branch, input.worktree
    ));
    out.push_str("## Acceptance criteria\n\n");
    for item in &input.task.acceptance {
        out.push_str(&format!("- {item}\n"));
    }
    out.push_str("\n## Scope — change only these paths\n\n");
    for path in &input.task.scope {
        out.push_str(&format!("- {path}\n"));
    }
    out.push_str("\n## Skills to load first\n\n");
    if input.skills.is_empty() {
        out.push_str("- none\n");
    }
    for skill in input.skills {
        out.push_str(&format!("- {skill}\n"));
    }
    out.push_str("\n## Rules\n\n");
    out.push_str(&format!(
        "- work only in `{}`; change nothing outside the scope above\n",
        input.worktree
    ));
    out.push_str(&format!(
        "- commit on your branch `{}`; never push, never open a pull request\n",
        input.branch
    ));
    out.push_str("- run the worker gates before you report: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`\n");
    out.push_str(&format!(
        "- report when you are done: `shprd agent report --task {t} --attempt {a} --status <completed|blocked|failed> --summary <one line>` \
         plus `--test <cmd>` for each command you ran, `--changed <path>` for each path you changed, `--commit <sha>` for your commit \
         and `--skill-used <name>` for each skill above you actually used\n",
        t = input.task.id,
        a = input.attempt
    ));
    out
}

/// Builds the environment a spawn runs in from the process environment, the
/// config and the run log.
///
/// Everything that touches the machine is resolved here, once, so [`spawn`]
/// itself stays pure logic plus the worktree and the multiplexer.
pub fn build_env<'a>(ctx: &'a Ctx, home: Option<&Path>) -> Result<SpawnEnv<'a>, CliError> {
    let home = crate::ctx::resolve_home(home)?;
    let data = crate::cmd::roster::resolve_data_dir()?;
    let specs = shprd_roster::spec::load_dir(&data.join("harness")).map_err(|err| {
        CliError::other(anyhow!(err).context(format!("loading {}", data.display())))
    })?;
    let path_env = std::env::var("PATH").unwrap_or_default();
    let programs: Vec<Program> = specs
        .iter()
        .map(|spec| Program {
            id: spec.id.clone(),
            installed: shprd_roster::detect::find_binary(&spec.detect.binary, &path_env).is_some(),
            launch: spec.launch.clone(),
        })
        .collect();
    let global = home.join(".config").join("shprd").join("config.toml");
    let repo_config = ctx.root.join(".shprd").join("config.toml");
    let config = shprd_core::config::load(Some(&global), Some(&repo_config))
        .map_err(|err| CliError::other(anyhow!(err).context("loading the shprd config")))?;
    let compat = read_compat(&data.join("skills").join("compat.toml"))?;
    let state = ctx.state()?;
    let run = state
        .run
        .as_ref()
        .ok_or_else(|| CliError::other(anyhow!("no run yet: start one with `shprd run start`")))?;
    let repo_name = ctx
        .root
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .ok_or_else(|| CliError::other(anyhow!("{} has no directory name", ctx.root.display())))?;
    let base_env = std::env::vars().collect();
    Ok(SpawnEnv {
        ctx,
        layout: Layout::default_for_home(&home),
        briefs_dir: home.join(".shprd").join("briefs"),
        run: run.run_id.clone(),
        base_branch: run.integration_branch.clone(),
        repo_name,
        programs,
        workers: config.roster.workers.clone(),
        skills: config.skills.clone(),
        compat,
        base_env,
    })
}

/// Reads the curated compatibility list, treating a missing file as an empty
/// list: a checkout without `data/skills` still spawns, it just adds no
/// tag-selected skills.
fn read_compat(path: &Path) -> Result<Vec<CompatEntry>, CliError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let list = shprd_roster::skills::CompatList::load(path).map_err(|err| {
        CliError::other(anyhow!(err).context(format!("loading {}", path.display())))
    })?;
    Ok(list.skills)
}

/// The multiplexer the binary drives: the real herdr the human is working in.
pub fn real_mux() -> shprd_mux_herdr::HerdrMux {
    shprd_mux_herdr::HerdrMux::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use shprd_core::state::TaskStatus;
    use shprd_roster::skills::Role as SkillRole;

    /// A task with the fields the brief and the skill selection read.
    fn task(id: &str, tags: &[&str]) -> Task {
        Task {
            id: id.to_string(),
            title: "Make spawn real".to_string(),
            scope: vec!["crates/shprd".to_string()],
            acceptance: vec!["spawn opens a tab".to_string()],
            tags: tags.iter().map(|tag| (*tag).to_string()).collect(),
            skills: Vec::new(),
            deps: Vec::new(),
            status: TaskStatus::Queued,
            attempts: Vec::new(),
            overrides: Vec::new(),
        }
    }

    /// One curated skill entry.
    fn compat(name: &str, verdict: Verdict, roles: &[SkillRole], tags: &[&str]) -> CompatEntry {
        CompatEntry {
            name: name.to_string(),
            source: "someone/skills".to_string(),
            verdict,
            roles: roles.to_vec(),
            tags: tags.iter().map(|tag| (*tag).to_string()).collect(),
            reason: String::new(),
            reviewed_hash: "abc".to_string(),
        }
    }

    #[test]
    fn skills_combine_config_and_tagged_allow_entries() {
        let skills = Skills {
            workers: vec!["tdd".to_string(), "simplify".to_string()],
            ..Skills::default()
        };
        let compat = [
            compat(
                "frontend-design",
                Verdict::Allow,
                &[SkillRole::Worker],
                &["ui"],
            ),
            compat(
                "writing-plans",
                Verdict::Allow,
                &[SkillRole::Orchestrator],
                &["ui"],
            ),
            compat(
                "rust-gates",
                Verdict::Allow,
                &[SkillRole::Worker],
                &["rust"],
            ),
            compat("tdd", Verdict::Allow, &[SkillRole::Worker], &["rust"]),
        ];
        let chosen = select_skills(&skills, &compat, &["ui".to_string()]);
        assert_eq!(chosen, vec!["tdd", "simplify", "frontend-design"]);
    }

    #[test]
    fn skills_never_include_a_denied_entry() {
        let skills = Skills {
            workers: vec!["brainstorming".to_string()],
            ..Skills::default()
        };
        let compat = [
            compat(
                "brainstorming",
                Verdict::Deny,
                &[SkillRole::Worker],
                &["ux"],
            ),
            compat("opt-in-only", Verdict::OptIn, &[SkillRole::Worker], &["ux"]),
        ];
        assert!(select_skills(&skills, &compat, &["ux".to_string()]).is_empty());
    }

    #[test]
    fn brief_states_scope_rules_and_skills() {
        let skills = vec!["tdd".to_string()];
        let plan = task("M4.4", &["rust"]);
        let text = render_brief(&BriefInput {
            task: &plan,
            run: "m4",
            attempt: 2,
            agent: "claude",
            model: Some("claude-opus-5-5"),
            branch: "shprd/m4/M4.4",
            worktree: "/wt/m4/M4.4",
            skills: &skills,
        });
        for needle in [
            "# M4.4 Make spawn real",
            "attempt 2",
            "claude-opus-5-5",
            "## Acceptance criteria",
            "spawn opens a tab",
            "## Scope — change only these paths",
            "crates/shprd",
            "## Skills to load first",
            "- tdd",
            "never push",
            "shprd agent report --task M4.4 --attempt 2",
            "--test",
            "--changed",
            "--commit",
            "--skill-used",
            "shprd/m4/M4.4",
            "/wt/m4/M4.4",
        ] {
            assert!(
                text.contains(needle),
                "brief is missing {needle:?}:\n{text}"
            );
        }
    }
}
