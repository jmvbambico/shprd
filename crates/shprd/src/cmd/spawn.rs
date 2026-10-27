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
use std::time::Duration;

use anyhow::anyhow;
use shprd_core::config::Skills;
use shprd_core::event::{Actor, EventKind, ResourceAction, ResourceKind};
use shprd_core::state::{RunState, Task};
use shprd_git::worktree::{self, Layout, Worktree};
use shprd_mux::{Mux, naming};
use shprd_roster::env::{EnvContext, LaunchEnv, Role as EnvRole, launch_env};
use shprd_roster::models::AgentRef;
use shprd_roster::skills::{CompatEntry, Role as SkillRole, Verdict};
use shprd_roster::spec::{HarnessSpec, LaunchSpec, Values, render_args};

use crate::ctx::{CliError, Ctx};

/// One agent CLI the spawn can launch.
#[derive(Debug, Clone)]
pub struct Program {
    /// The harness spec: its id is the program name, e.g. `claude`, and it
    /// carries both the launch recipe and the model rules an agent string is
    /// parsed against.
    pub spec: HarnessSpec,
    /// Whether the CLI is installed on this machine.
    pub installed: bool,
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
    /// How long a freshly launched worker gets to be detected as an agent
    /// before its brief counts as undelivered. Production reads it from the
    /// environment; tests set it explicitly.
    pub ready_wait: Duration,
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
    let brief_path = brief_path(&env.briefs_dir, &env.run, &task.id, attempt);
    let kind = spawned_event(&task.id, attempt, &pick, &worktree, &branch);
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
    // The workspace must exist before a worktree does, so a repo the human has
    // not opened in the multiplexer yet costs nothing to refuse.
    let workspace = workspace_of(mux, env)?;
    let created = create_worktree(env, &task.id)?;
    // From here on every failure undoes everything: the tab first, then the
    // worktree, and never an event.
    let opened = open_agent(
        mux,
        env,
        &workspace,
        &Launching {
            pick: &pick,
            task: &task.id,
            effort: req.effort.as_deref(),
            brief: &brief,
            brief_path: &brief_path,
            worktree: &created,
        },
    );
    let (tab_id, tab) = match opened {
        Ok(opened) => opened,
        Err(err) => {
            discard_worktree(env, &created);
            return Err(err);
        }
    };
    // The tab id goes next to the brief: a label is only 16 characters, so
    // two tasks can share one, and the id cannot.
    write_tab(
        &tab_path(&env.briefs_dir, &env.run, &task.id, attempt),
        &tab_id,
    );
    if let Err(err) = record_attempt(env, &task.id, attempt, &pick, &created) {
        close_tab(mux, &tab_id);
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

/// The repository's workspace, or an error telling the human to create it.
fn workspace_of(mux: &dyn Mux, env: &SpawnEnv<'_>) -> Result<shprd_mux::Workspace, CliError> {
    let name = naming::workspace_name(&env.repo_name);
    mux.find_workspace(&name)
        .map_err(|err| CliError::other(anyhow!(err).context("looking for the workspace")))?
        .ok_or_else(|| {
            CliError::Invalid(anyhow!("no workspace {name:?} yet: run `shprd up` first"))
        })
}

/// Opens the agent's tab and starts its CLI in it.
///
/// Returns the tab's id and title. A host that has no `system_prompt_args`
/// cannot take the brief as an argument, so the brief is typed in as the agent's
/// first turn instead.
/// What [`open_agent`] needs to start one attempt's CLI.
struct Launching<'a> {
    /// The agent that was picked.
    pick: &'a Pick,
    /// The task it works on.
    task: &'a str,
    /// The effort it runs at, when one was asked for.
    effort: Option<&'a str>,
    /// The brief the agent reads.
    brief: &'a str,
    /// The brief file's path.
    brief_path: &'a Path,
    /// The worktree the agent works in.
    worktree: &'a Worktree,
}

fn open_agent(
    mux: &dyn Mux,
    env: &SpawnEnv<'_>,
    workspace: &shprd_mux::Workspace,
    launching: &Launching<'_>,
) -> Result<(shprd_mux::TabId, String), CliError> {
    let Launching {
        pick,
        task,
        effort,
        brief,
        brief_path,
        worktree,
    } = *launching;
    let wanted = naming::tab_title(naming::Role::Worker, &pick.agent, task, false);
    let existing = mux
        .list_tabs(&workspace.id)
        .map_err(|err| CliError::other(anyhow!(err).context("listing tabs")))?
        .into_iter()
        .map(|tab| tab.title)
        .collect::<Vec<String>>();
    let title = naming::disambiguate(&existing, &wanted, task);
    let tab = mux
        .create_tab(&workspace.id, &title, &worktree.path)
        .map_err(|err| CliError::other(anyhow!(err).context("creating the agent tab")))?;
    // The tab exists from here on, so every failure closes it again: a tab left
    // open points at a worktree the failing spawn is about to remove.
    match start_in_tab(
        mux, env, pick, effort, brief, brief_path, worktree, task, &title, &tab,
    ) {
        Ok(()) => Ok((tab.id, title)),
        Err(err) => {
            close_tab(mux, &tab.id);
            Err(err)
        }
    }
}

/// Starts the agent's CLI in `tab` and hands it the brief.
///
/// A brief that does not arrive is reported, never fatal: the CLI is running,
/// and the human can resend the brief into the tab.
#[expect(clippy::too_many_arguments, reason = "one launch's whole input")]
fn start_in_tab(
    mux: &dyn Mux,
    env: &SpawnEnv<'_>,
    pick: &Pick,
    effort: Option<&str>,
    brief: &str,
    brief_path: &Path,
    worktree: &Worktree,
    task: &str,
    title: &str,
    tab: &shprd_mux::Tab,
) -> Result<(), CliError> {
    let (argv, brief_in_args) = launch_argv(
        &pick.launch,
        pick.model.as_deref(),
        effort,
        brief,
        brief_path,
    )?;
    let pairs = env_pairs(launch_env(
        &env.base_env,
        EnvRole::Worker,
        &EnvContext {
            actor: format!("agent:{}", pick.agent),
            task: Some(task.to_string()),
            run: Some(env.run.clone()),
            repo: worktree.path.clone(),
        },
    ));
    mux.run(&tab.root_pane, &argv, &pairs)
        .map_err(|err| CliError::other(anyhow!(err).context("starting the agent CLI")))?;
    if brief_in_args {
        return Ok(());
    }
    let delivery = super::launch::deliver_brief(mux, &tab.root_pane, brief, env.ready_wait);
    let Some(reason) = delivery.reason() else {
        return Ok(());
    };
    eprintln!(
        "warning: brief not delivered to {title}: {reason}; resend with: shprd agent prompt --task {task} \"$(cat {})\"",
        brief_path.display()
    );
    let escalation = EventKind::Escalated {
        task: Some(task.to_string()),
        body: "brief not delivered".to_string(),
        default_action: "resend the brief".to_string(),
    };
    if let Err(err) = env.ctx.commit(escalation) {
        eprintln!("warning: could not record the escalation: {err}");
    }
    Ok(())
}

/// Closes a tab this spawn opened, ignoring the failure for the same reason as
/// [`discard_worktree`].
fn close_tab(mux: &dyn Mux, tab: &shprd_mux::TabId) {
    if let Err(err) = mux.close_tab(tab) {
        eprintln!("warning: could not close tab {}: {err}", tab.as_str());
    }
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

/// The ledger entries a spawn writes, in order: its two resources, then the
/// attempt itself.
fn attempt_events(task: &str, attempt: u32, pick: &Pick, created: &Worktree) -> Vec<EventKind> {
    let mut events = resource_kinds(task, &created.path, created.branch.as_str()).to_vec();
    events.push(spawned_event(
        task,
        attempt,
        pick,
        &created.path,
        &created.branch,
    ));
    events
}

/// The `Spawned` event one attempt appends.
fn spawned_event(
    task: &str,
    attempt: u32,
    pick: &Pick,
    worktree: &Path,
    branch: &str,
) -> EventKind {
    EventKind::Spawned {
        task: task.to_string(),
        attempt,
        agent: pick.agent.clone(),
        model: pick.model.clone(),
        worktree: worktree.display().to_string(),
        branch: branch.to_string(),
    }
}

/// Appends `events` in order, returning the ones that landed when one fails.
///
/// The caller needs them: an event that landed and is not compensated would
/// leave the state holding a resource the spawn is about to take away.
fn append_in_order(
    events: impl IntoIterator<Item = EventKind>,
    mut commit: impl FnMut(EventKind) -> Result<(), CliError>,
) -> Result<(), (CliError, Vec<EventKind>)> {
    let mut landed: Vec<EventKind> = Vec::new();
    for event in events {
        if let Err(err) = commit(event.clone()) {
            return Err((err, landed));
        }
        landed.push(event);
    }
    Ok(())
}

/// Appends the `Removed` counterpart of every resource event a half-written
/// attempt already landed, so the run never keeps a live resource the caller is
/// about to delete.
fn compensate(env: &SpawnEnv<'_>, landed: &[EventKind]) {
    for event in landed {
        let EventKind::Resource {
            kind, name, task, ..
        } = event
        else {
            continue;
        };
        let removed = EventKind::Resource {
            action: ResourceAction::Removed,
            kind: *kind,
            name: name.clone(),
            task: task.clone(),
        };
        if let Err(err) = env.ctx.commit(removed) {
            eprintln!("warning: could not record that {name} is gone: {err}");
        }
    }
}

/// Appends the worktree, the branch and then the attempt itself.
///
/// Should any of the three be refused or fail, the resources that did land get
/// their `Removed` events before the error goes back to the caller, which takes
/// the worktree and the tab away.
fn record_attempt(
    env: &SpawnEnv<'_>,
    task: &str,
    attempt: u32,
    pick: &Pick,
    created: &Worktree,
) -> Result<(), CliError> {
    let events = attempt_events(task, attempt, pick, created);
    match append_in_order(events, |event| env.ctx.commit(event).map(|_| ())) {
        Ok(()) => Ok(()),
        Err((err, landed)) => {
            compensate(env, &landed);
            Err(err)
        }
    }
}

/// The task `id` names, or an error saying the run plan has no such task.
fn task_of<'a>(state: &'a RunState, id: &str) -> Result<&'a Task, CliError> {
    state.tasks.get(id).ok_or_else(|| {
        CliError::other(anyhow!(
            "no task {id:?} in this run: add it with `shprd agent task add`"
        ))
    })
}

/// The harness table the agent strings are parsed against.
///
/// [`AgentRef::parse`] takes borrowed specs, so this copies the ones the
/// environment already holds rather than making the caller hand both a program
/// list and a spec list.
fn spec_table(programs: &[Program]) -> Vec<HarnessSpec> {
    programs
        .iter()
        .map(|program| program.spec.clone())
        .collect()
}

/// The program the parser named, or an error when the data directory has no
/// spec for it.
fn program_of<'a>(programs: &'a [Program], agent: &str) -> Result<&'a Program, CliError> {
    programs
        .iter()
        .find(|program| program.spec.id == agent)
        .ok_or_else(|| {
            CliError::other(anyhow!(
                "no harness spec for {agent:?} in the data directory"
            ))
        })
}

/// Picks the agent for an attempt.
///
/// An explicit `--agent` wins even when its CLI is not installed, because the
/// human may be pointing at a wrapper script, but it must parse against the
/// harness table: a typo is refused here rather than after a worktree exists.
/// Without one, the first configured worker whose CLI *is* installed takes the
/// attempt, so an unattended run never opens an empty tab; a worker whose
/// string does not parse is skipped with a warning, the same way an uninstalled
/// one is.
fn choose_agent(
    explicit: Option<&str>,
    workers: &[String],
    programs: &[Program],
) -> Result<Pick, CliError> {
    let specs = spec_table(programs);
    if let Some(raw) = explicit {
        let parsed = AgentRef::parse(raw, &specs).map_err(|err| {
            CliError::Invalid(anyhow!(err).context(format!("`{raw}` is not a usable agent")))
        })?;
        let program = program_of(programs, &parsed.program)?;
        return Ok(Pick {
            agent: parsed.program,
            model: parsed.model,
            launch: program.spec.launch.clone(),
        });
    }
    for raw in workers {
        let parsed = match AgentRef::parse(raw, &specs) {
            Ok(parsed) => parsed,
            Err(err) => {
                eprintln!("warning: skipping configured worker `{raw}`: {err}");
                continue;
            }
        };
        let found = programs
            .iter()
            .find(|program| program.spec.id == parsed.program && program.installed);
        if let Some(program) = found {
            return Ok(Pick {
                agent: parsed.program,
                model: parsed.model,
                launch: program.spec.launch.clone(),
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
        let for_workers = entry.roles.contains(&SkillRole::Worker);
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
            installed: shprd_roster::detect::find_binary(&spec.detect.binary, &path_env).is_some(),
            spec: spec.clone(),
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
        ready_wait: super::launch::ready_wait(),
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

/// Sends one turn of input to the tab a task's agent runs in.
///
/// Only the tab the active run's log says this task's latest attempt opened is
/// reachable, so a prompt can never land in another run's agent.
pub fn prompt(mux: &dyn Mux, repo: &Path, task: &str, text: &str) -> Result<(), CliError> {
    let home = crate::ctx::resolve_home(None)?;
    // Reading the log needs the root; the actor and the flags only matter to a
    // verb that writes.
    let ctx = Ctx::new(repo.to_path_buf(), Actor::Human, false, true);
    prompt_in(mux, &home, &ctx, task, text)
}

/// [`prompt`], against an explicit home directory, so a test needs no
/// environment.
fn prompt_in(
    mux: &dyn Mux,
    home: &Path,
    ctx: &Ctx,
    task: &str,
    text: &str,
) -> Result<(), CliError> {
    let run = active_run(ctx)?;
    let repo = &ctx.root;
    let repo_name = repo
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .ok_or_else(|| CliError::other(anyhow!("{} has no directory name", repo.display())))?;
    let name = naming::workspace_name(&repo_name);
    let workspace = mux
        .find_workspace(&name)
        .map_err(|err| CliError::other(anyhow!(err).context("looking for the workspace")))?
        .ok_or_else(|| {
            CliError::Invalid(anyhow!("no workspace {name:?} yet: run `shprd up` first"))
        })?;
    let tabs = mux
        .list_tabs(&workspace.id)
        .map_err(|err| CliError::other(anyhow!(err).context("listing tabs")))?;
    let record = latest_tab_record(&home.join(".shprd").join("briefs"), &run, task)
        .ok_or_else(|| no_open_tab(task))?;
    let wanted = std::fs::read_to_string(&record).map_err(|err| {
        CliError::other(anyhow!(err).context(format!("reading {}", record.display())))
    })?;
    let tab = tabs
        .iter()
        .find(|tab| tab.id.as_str() == wanted.trim())
        .ok_or_else(|| no_open_tab(task))?;
    mux.prompt(&tab.root_pane, text)
        .map_err(|err| CliError::other(anyhow!(err).context("prompting the agent")))
}

/// The run the folded event log names as active.
///
/// A prompt outside a run has no tab to reach: the run id is half of every tab
/// record's path.
fn active_run(ctx: &Ctx) -> Result<String, CliError> {
    let state = ctx.state()?;
    state
        .run
        .as_ref()
        .map(|run| run.run_id.clone())
        .ok_or_else(|| {
            CliError::Invalid(anyhow!("no active run: start one with `shprd run start`"))
        })
}

/// The refusal a task with no reachable tab earns.
fn no_open_tab(task: &str) -> CliError {
    CliError::Invalid(anyhow!(
        "no open tab for task {task:?}: spawn it with `shprd agent spawn --task {task}`"
    ))
}

/// Where an attempt records the id of the tab it opened:
/// `<briefs>/<run>/<task>-a<attempt>.tab`, next to its brief.
fn tab_path(briefs: &Path, run: &str, task: &str, attempt: u32) -> PathBuf {
    briefs.join(run).join(format!("{task}-a{attempt}.tab"))
}

/// Records the tab an attempt opened, so a later prompt reaches that exact tab
/// rather than one whose truncated label happens to match.
///
/// A failure is only a warning: the agent is running, and a prompt that cannot
/// find the record says so.
fn write_tab(path: &Path, tab: &shprd_mux::TabId) {
    if let Some(parent) = path.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        eprintln!("warning: could not create {}: {err}", parent.display());
        return;
    }
    if let Err(err) = std::fs::write(path, tab.as_str()) {
        eprintln!("warning: could not write {}: {err}", path.display());
    }
}

/// The tab record of a task's latest attempt in one run.
///
/// Only `run`'s directory is read: a task id reused in a later run must not
/// resolve to the older run's tab. The attempt number is part of the file name,
/// so the highest one names the tab a prompt should reach.
fn latest_tab_record(briefs: &Path, run: &str, task: &str) -> Option<PathBuf> {
    let prefix = format!("{task}-a");
    let mut best: Option<(u32, PathBuf)> = None;
    for file in std::fs::read_dir(briefs.join(run)).ok()?.flatten() {
        let name = file.file_name().to_string_lossy().to_string();
        let attempt = name
            .strip_prefix(&prefix)
            .and_then(|rest| rest.strip_suffix(".tab"))
            .and_then(|digits| digits.parse::<u32>().ok());
        let Some(attempt) = attempt else {
            continue;
        };
        if best.as_ref().is_none_or(|(kept, _)| attempt > *kept) {
            best = Some((attempt, file.path()));
        }
    }
    best.map(|(_, path)| path)
}

/// Builds the argv the agent's CLI starts with.
///
/// The order is fixed by the launch spec: the base command, then the flags that
/// suppress approval prompts, then the model, the effort and finally the system
/// prompt. The second element of the result says whether the brief went in as an
/// argument, which is also what says whether it has to be typed in instead.
fn launch_argv(
    launch: &LaunchSpec,
    model: Option<&str>,
    effort: Option<&str>,
    brief: &str,
    brief_path: &Path,
) -> Result<(Vec<String>, bool), CliError> {
    let mut argv = launch.interactive.clone();
    if let Some(auto) = &launch.auto_approve {
        argv.extend(auto.iter().cloned());
    }
    let values = Values {
        model: model.map(str::to_string),
        effort: effort.map(str::to_string),
        prompt: Some(brief.to_string()),
        prompt_file: Some(brief_path.display().to_string()),
        session: None,
    };
    argv.extend(render(
        launch,
        super::launch::model_args(launch, model),
        &values,
    )?);
    if let (Some(effort_args), Some(_)) = (&launch.effort_args, effort) {
        argv.extend(render(launch, effort_args, &values)?);
    }
    let mut brief_in_args = false;
    if let Some(system_prompt_args) = &launch.system_prompt_args {
        argv.extend(render(launch, system_prompt_args, &values)?);
        brief_in_args = true;
    }
    Ok((argv, brief_in_args))
}

/// Renders one argument template, naming the template when it cannot be filled.
fn render(
    launch: &LaunchSpec,
    template: &[String],
    values: &Values,
) -> Result<Vec<String>, CliError> {
    render_args(template, values).map_err(|err| {
        CliError::other(anyhow!(err).context(format!(
            "filling the launch arguments {template:?} of {:?}",
            launch.interactive
        )))
    })
}

/// Flattens a launch environment into the pairs [`Mux::run`] takes: set entries
/// first, then the removals, both in name order.
fn env_pairs(diff: LaunchEnv) -> Vec<(String, Option<String>)> {
    let mut pairs: Vec<(String, Option<String>)> = diff
        .set
        .into_iter()
        .map(|(name, value)| (name, Some(value)))
        .collect();
    pairs.extend(diff.remove.into_iter().map(|name| (name, None)));
    pairs
}

#[cfg(test)]
mod flow_tests {
    use super::*;
    use std::process::Command;

    use shprd_core::event::Actor;
    use shprd_core::state::{ResourceRecord, ResourceStatus, TaskStatus};
    use shprd_mux::fake::{Call, FakeMux};
    use tempfile::TempDir;

    /// Runs one git command in `dir`, failing the test when it does.
    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output()
            .unwrap_or_else(|err| panic!("git {args:?} failed: {err}"));
        assert!(status.status.success(), "git {args:?}: {status:?}");
    }

    /// A harness spec for a fake CLI named `id`, parsed the way the data
    /// directory's specs are, so the tests exercise the real model rules.
    fn fake_spec(id: &str) -> HarnessSpec {
        fake_spec_with(id, false)
    }

    /// The same spec, but with the model rules `provider_qualified` sets, as
    /// `pi` has them.
    fn fake_spec_with(id: &str, provider_qualified: bool) -> HarnessSpec {
        let text = format!(
            r#"
id = "{id}"
name = "Fake {id}"
vendor = "pi"
verified_version = "1.0.0"
dry_patterns = []

[detect]
binary = "{id}"
version_args = ["--version"]
signin_hint = "fake"
signin_args = []

[launch]
interactive = ["{id}"]
model_args = ["--model", "{{model}}"]
verified = false

[models]
provider_qualified = {provider_qualified}

[records]
kind = "jsonl"
location = "~/.fake/{{session}}.jsonl"
errors = "error"
verified = true

[quota]
source = "none"

[skills]
home_dirs = []
project_dirs = []
"#
        );
        match toml::from_str::<HarnessSpec>(&text) {
            Ok(spec) => spec,
            Err(err) => panic!("the test spec is invalid: {err}"),
        }
    }

    /// A repository with one commit and the integration branch, and a home.
    struct Scratch {
        repo: TempDir,
        home: TempDir,
    }

    impl Scratch {
        fn new() -> Scratch {
            let repo = TempDir::new().unwrap_or_else(|err| panic!("{err}"));
            let home = TempDir::new().unwrap_or_else(|err| panic!("{err}"));
            git(repo.path(), &["init", "-q", "-b", "main"]);
            std::fs::write(repo.path().join("README.md"), "scratch\n").unwrap_or_default();
            git(repo.path(), &["add", "."]);
            git(repo.path(), &["commit", "-q", "-m", "first"]);
            git(repo.path(), &["branch", "integration/m4"]);
            Scratch { repo, home }
        }

        fn root(&self) -> &Path {
            self.repo.path()
        }

        /// The repo's directory name, which the workspace and worktree names use.
        fn repo_name(&self) -> String {
            self.root()
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_default()
        }

        /// A context with a started run and the task `T1` in its plan.
        fn ctx(&self) -> Ctx {
            let ctx = Ctx::new(self.root().to_path_buf(), Actor::Human, false, true);
            ctx.commit(EventKind::RunStarted {
                run_id: "t1".to_string(),
                repo: self.repo_name(),
                base_branch: "main".to_string(),
                integration_branch: "integration/m4".to_string(),
            })
            .unwrap_or_else(|err| panic!("{err}"));
            ctx.commit(EventKind::TaskAdded {
                task: "T1".to_string(),
                title: "Make spawn real".to_string(),
                scope: vec!["crates/shprd".to_string()],
                acceptance: vec!["spawn opens a tab".to_string()],
                tags: vec!["rust".to_string()],
                skills: Vec::new(),
                deps: Vec::new(),
            })
            .unwrap_or_else(|err| panic!("{err}"));
            ctx
        }

        /// The environment a spawn runs in, with a fake `sh` CLI.
        fn spawn_env<'a>(&self, ctx: &'a Ctx) -> SpawnEnv<'a> {
            let mut base_env = BTreeMap::new();
            base_env.insert("GH_TOKEN".to_string(), "secret".to_string());
            base_env.insert("PATH".to_string(), "/usr/bin".to_string());
            SpawnEnv {
                ctx,
                layout: Layout::default_for_home(self.home.path()),
                briefs_dir: self.home.path().join(".shprd").join("briefs"),
                run: "t1".to_string(),
                base_branch: "integration/m4".to_string(),
                repo_name: self.repo_name(),
                programs: vec![Program {
                    spec: fake_spec("sh"),
                    installed: true,
                }],
                workers: vec!["sh".to_string()],
                skills: Skills {
                    workers: vec!["tdd".to_string()],
                    ..Skills::default()
                },
                compat: Vec::new(),
                base_env,
                // No wait: a fake pane that never reports an agent would
                // otherwise stall every test that launches one.
                ready_wait: Duration::ZERO,
            }
        }
    }

    /// The harness table two of the agent-string cases parse against: a plain
    /// `sh` and a `pi` that needs its model provider-qualified.
    fn programs() -> Vec<Program> {
        vec![
            Program {
                spec: fake_spec("sh"),
                installed: true,
            },
            Program {
                spec: fake_spec_with("pi", true),
                installed: true,
            },
        ]
    }

    /// The refusal an explicit agent string earns, panicking when it is
    /// accepted instead.
    fn refusal(agent: Option<&str>) -> CliError {
        match choose_agent(agent, &[], &programs()) {
            Ok(pick) => panic!("{agent:?} was accepted as {}", pick.agent),
            Err(err) => err,
        }
    }

    #[test]
    fn an_agent_string_with_no_model_is_refused() {
        let refused = refusal(Some("sh/"));
        assert_eq!(refused.exit_code(), 2);
        let text = refused.to_string();
        assert!(text.contains("names no model"), "{text}");
    }

    #[test]
    fn a_model_that_names_no_provider_is_refused_with_the_hint() {
        let refused = refusal(Some("pi/deepseek-x"));
        assert_eq!(refused.exit_code(), 2);
        let text = refused.to_string();
        assert!(text.contains("needs a provider"), "{text}");
        assert!(text.contains("pi/<provider>/deepseek-x"), "{text}");
    }

    #[test]
    fn a_configured_worker_that_does_not_parse_is_skipped() {
        let workers = vec!["pi/bare".to_string(), "sh".to_string()];
        let pick = choose_agent(None, &workers, &programs()).unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(pick.agent, "sh");
    }

    fn request() -> SpawnRequest {
        SpawnRequest {
            task: "T1".to_string(),
            agent: None,
            effort: None,
        }
    }

    /// The pick a spawn makes for the fake `sh` worker.
    fn pick() -> Pick {
        Pick {
            agent: "sh".to_string(),
            model: None,
            launch: fake_spec("sh").launch,
        }
    }

    #[test]
    fn a_spawned_append_that_fails_leaves_no_live_resource() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.spawn_env(&ctx);
        let created = create_worktree(&env, "T1").unwrap_or_else(|err| panic!("{err}"));
        // The third append is the `Spawned` event; the two resources land.
        let mut commits = 0;
        let outcome = append_in_order(attempt_events("T1", 1, &pick(), &created), |event| {
            commits += 1;
            if commits == 3 {
                return Err(CliError::other(anyhow!("the log went away")));
            }
            env.ctx.commit(event).map(|_| ())
        });
        let (err, landed) = match outcome {
            Ok(()) => panic!("the attempt should not have landed"),
            Err(failed) => failed,
        };
        assert!(err.to_string().contains("the log went away"), "{err}");
        assert_eq!(landed.len(), 2, "only the two resources landed");
        compensate(&env, &landed);
        discard_worktree(&env, &created);

        let state = ctx.state().unwrap_or_else(|err| panic!("{err}"));
        let live: Vec<&ResourceRecord> = state
            .resources
            .values()
            .filter(|record| record.status == ResourceStatus::Live)
            .collect();
        assert!(live.is_empty(), "still live: {live:?}");
        assert!(!created.path.exists(), "the worktree is still there");
    }

    #[test]
    fn spawn_creates_a_worktree_and_records_it() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.spawn_env(&ctx);
        let mux = FakeMux::default().with_ready_agent();
        let workspace = mux
            .create_workspace(
                &naming::workspace_name(&scratch.repo_name()),
                scratch.root(),
            )
            .unwrap_or_else(|err| panic!("{err}"));

        let report = spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));

        assert_eq!(report.attempt, 1);
        assert_eq!(report.agent, "sh");
        assert_eq!(report.tab, "\u{25cf} sh\u{b7}T1");
        assert!(
            report.worktree.is_dir(),
            "no worktree at {:?}",
            report.worktree
        );
        assert_eq!(report.branch, "shprd/t1/T1");
        assert_eq!(report.skills, vec!["tdd"]);
        let brief = std::fs::read_to_string(&report.brief).unwrap_or_default();
        assert!(brief.contains("Make spawn real"), "{brief}");
        assert!(brief.contains("crates/shprd"), "{brief}");
        // The tab id is recorded beside the brief, so a later prompt reaches
        // this exact tab even when another task shares its 16-char label.
        let recorded = tab_path(
            &scratch.home.path().join(".shprd").join("briefs"),
            "t1",
            "T1",
            1,
        );
        let tab_id = std::fs::read_to_string(&recorded).unwrap_or_default();
        let tabs = mux
            .list_tabs(&workspace.id)
            .unwrap_or_else(|err| panic!("{err}"));
        assert!(
            tabs.iter().any(|tab| tab.id.as_str() == tab_id),
            "{tab_id:?} names no tab in {tabs:?}"
        );

        let state = ctx.state().unwrap_or_else(|err| panic!("{err}"));
        let task = state.tasks.get("T1").unwrap_or_else(|| panic!("no task"));
        assert_eq!(task.status, TaskStatus::Working);
        assert_eq!(task.attempts.len(), 1);
        let resources: Vec<(&ResourceKind, &str)> = state
            .resources
            .keys()
            .map(|(kind, name)| (kind, name.as_str()))
            .collect();
        assert!(resources.contains(&(
            &ResourceKind::Worktree,
            report.worktree.to_str().unwrap_or_default()
        )));
        assert!(resources.contains(&(&ResourceKind::Branch, "shprd/t1/T1")));

        let calls = mux.calls();
        let tab = calls
            .iter()
            .find_map(|call| match call {
                Call::CreateTab(owner, title, cwd) => {
                    Some((owner.clone(), title.clone(), cwd.clone()))
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("no tab was created: {calls:?}"));
        assert_eq!(tab.0, workspace.id);
        assert_eq!(tab.1, "\u{25cf} sh\u{b7}T1");
        assert_eq!(tab.2, report.worktree);
        let (argv, pairs) = calls
            .iter()
            .find_map(|call| match call {
                Call::Run(_, argv, pairs) => Some((argv.clone(), pairs.clone())),
                _ => None,
            })
            .unwrap_or_else(|| panic!("the CLI was not started: {calls:?}"));
        assert_eq!(argv, vec!["sh".to_string()]);
        assert!(pairs.contains(&("SHPRD_TASK".to_string(), Some("T1".to_string()))));
        assert!(pairs.contains(&("SHPRD_RUN".to_string(), Some("t1".to_string()))));
        assert!(pairs.contains(&("SHPRD_ACTOR".to_string(), Some("agent:sh".to_string()))));
        assert!(!pairs.contains(&("GH_TOKEN".to_string(), Some("secret".to_string()))));
        assert!(pairs.contains(&("GH_TOKEN".to_string(), None)));
        // The spec has no system-prompt arguments, so the brief is typed in.
        let prompt = calls
            .iter()
            .find_map(|call| match call {
                Call::Prompt(_, text) => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("the brief was never typed in: {calls:?}"));
        assert!(prompt.contains("Make spawn real"), "{prompt}");
    }

    /// A [`Mux`] that delegates everything but refuses to open tabs, which is
    /// how a host that lost the workspace looks from spawn's side.
    struct NoTabs(FakeMux);

    impl Mux for NoTabs {
        fn capabilities(&self) -> shprd_mux::Capabilities {
            self.0.capabilities()
        }
        fn find_workspace(&self, name: &str) -> shprd_mux::Result<Option<shprd_mux::Workspace>> {
            self.0.find_workspace(name)
        }
        fn create_workspace(
            &self,
            name: &str,
            cwd: &Path,
        ) -> shprd_mux::Result<shprd_mux::Workspace> {
            self.0.create_workspace(name, cwd)
        }
        fn focus_workspace(&self, workspace: &shprd_mux::WorkspaceId) -> shprd_mux::Result<()> {
            self.0.focus_workspace(workspace)
        }
        fn list_tabs(
            &self,
            workspace: &shprd_mux::WorkspaceId,
        ) -> shprd_mux::Result<Vec<shprd_mux::Tab>> {
            self.0.list_tabs(workspace)
        }
        fn create_tab(
            &self,
            _workspace: &shprd_mux::WorkspaceId,
            _title: &str,
            _cwd: &Path,
        ) -> shprd_mux::Result<shprd_mux::Tab> {
            Err(shprd_mux::MuxError::command(
                "create tab",
                "the host refused",
            ))
        }
        fn rename_tab(&self, tab: &shprd_mux::TabId, title: &str) -> shprd_mux::Result<()> {
            self.0.rename_tab(tab, title)
        }
        fn focus_tab(&self, tab: &shprd_mux::TabId) -> shprd_mux::Result<()> {
            self.0.focus_tab(tab)
        }
        fn close_tab(&self, tab: &shprd_mux::TabId) -> shprd_mux::Result<()> {
            self.0.close_tab(tab)
        }
        fn split(
            &self,
            pane: &shprd_mux::PaneId,
            direction: shprd_mux::Direction,
            ratio: f32,
            cwd: &Path,
        ) -> shprd_mux::Result<shprd_mux::PaneId> {
            self.0.split(pane, direction, ratio, cwd)
        }
        fn run(
            &self,
            pane: &shprd_mux::PaneId,
            argv: &[String],
            env: &[(String, Option<String>)],
        ) -> shprd_mux::Result<()> {
            self.0.run(pane, argv, env)
        }
        fn send_text(&self, pane: &shprd_mux::PaneId, text: &str) -> shprd_mux::Result<()> {
            self.0.send_text(pane, text)
        }
        fn prompt(&self, pane: &shprd_mux::PaneId, text: &str) -> shprd_mux::Result<()> {
            self.0.prompt(pane, text)
        }
        fn read(&self, pane: &shprd_mux::PaneId, lines: usize) -> shprd_mux::Result<String> {
            self.0.read(pane, lines)
        }
        fn agent_state(
            &self,
            pane: &shprd_mux::PaneId,
        ) -> shprd_mux::Result<shprd_mux::AgentState> {
            self.0.agent_state(pane)
        }
        fn next_events(
            &self,
            timeout: std::time::Duration,
        ) -> shprd_mux::Result<Vec<shprd_mux::MuxEvent>> {
            self.0.next_events(timeout)
        }
    }

    #[test]
    fn a_failed_tab_removes_the_worktree_and_records_nothing() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.spawn_env(&ctx);
        let mux = NoTabs(FakeMux::default().with_ready_agent());
        mux.create_workspace(
            &naming::workspace_name(&scratch.repo_name()),
            scratch.root(),
        )
        .unwrap_or_else(|err| panic!("{err}"));

        let failure = spawn(&mux, &env, &request());
        assert!(failure.is_err(), "spawn should have failed");
        assert_eq!(failure.map(|_| ()).map_err(|err| err.exit_code()), Err(1));

        let worktree = env.layout.path(&env.repo_name, "t1", "T1");
        assert!(
            !worktree.exists(),
            "worktree {} survived",
            worktree.display()
        );
        let state = ctx.state().unwrap_or_else(|err| panic!("{err}"));
        assert!(state.resources.is_empty(), "{:?}", state.resources);
        let task = state.tasks.get("T1").unwrap_or_else(|| panic!("no task"));
        assert_eq!(task.status, TaskStatus::Queued);
        assert!(task.attempts.is_empty());
    }

    #[test]
    fn a_brief_lands_once_the_host_detects_the_agent() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let mut env = scratch.spawn_env(&ctx);
        // The host needs two polls to call the pane an agent; the wait covers it.
        env.ready_wait = Duration::from_secs(5);
        let mux = FakeMux::default();
        let workspace = mux
            .create_workspace(
                &naming::workspace_name(&scratch.repo_name()),
                scratch.root(),
            )
            .unwrap_or_else(|err| panic!("{err}"));
        let pane = shprd_mux::PaneId::new("pane-1");
        mux.set_state_after(&pane, 2, shprd_mux::AgentState::Idle);
        assert!(
            mux.list_tabs(&workspace.id)
                .unwrap_or_else(|err| panic!("{err}"))
                .is_empty(),
            "the fake starts with no tabs"
        );

        let report = match spawn(&mux, &env, &request()) {
            Ok(report) => report,
            Err(err) => panic!("{err}"),
        };
        assert!(report.worktree.exists());
        let prompted = mux
            .calls()
            .into_iter()
            .filter_map(|call| match call {
                Call::Prompt(pane, text) => Some((pane, text)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(prompted.len(), 1, "the brief is typed in exactly once");
        assert!(
            prompted[0].1.contains("Make spawn real"),
            "{}",
            prompted[0].1
        );
        let state = ctx.state().unwrap_or_else(|err| panic!("{err}"));
        assert!(state.escalations.is_empty(), "{:?}", state.escalations);
    }

    #[test]
    fn a_brief_no_agent_ever_takes_is_a_warning_and_an_escalation() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.spawn_env(&ctx);
        // No ready agent and no wait: the plain `sh` this fake hosts is never
        // detected, which is the real OpenCode and Kilo case.
        let mux = FakeMux::default();
        mux.create_workspace(
            &naming::workspace_name(&scratch.repo_name()),
            scratch.root(),
        )
        .unwrap_or_else(|err| panic!("{err}"));

        let report = match spawn(&mux, &env, &request()) {
            Ok(report) => report,
            Err(err) => panic!("an undelivered brief must not fail the spawn: {err}"),
        };
        assert!(report.worktree.exists(), "the worktree stays");
        assert!(
            mux.calls()
                .iter()
                .all(|call| !matches!(call, Call::Prompt(..))),
            "nothing may be typed into an undetected pane"
        );
        let state = ctx.state().unwrap_or_else(|err| panic!("{err}"));
        let escalation = state
            .escalations
            .first()
            .unwrap_or_else(|| panic!("no escalation: {:?}", state.escalations));
        assert_eq!(escalation.task.as_deref(), Some("T1"));
        assert_eq!(escalation.body, "brief not delivered");
        assert_eq!(escalation.default_action, "resend the brief");
    }

    #[test]
    fn a_failing_run_closes_the_tab_and_removes_the_worktree() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.spawn_env(&ctx);
        let mux = FakeMux::default()
            .with_ready_agent()
            .failing_run("the host refused");
        let workspace = mux
            .create_workspace(
                &naming::workspace_name(&scratch.repo_name()),
                scratch.root(),
            )
            .unwrap_or_else(|err| panic!("{err}"));

        let failure = spawn(&mux, &env, &request());
        assert!(failure.is_err(), "spawn should have failed");

        let worktree = env.layout.path(&env.repo_name, "t1", "T1");
        assert!(
            !worktree.exists(),
            "worktree {} survived",
            worktree.display()
        );
        let tabs = mux
            .list_tabs(&workspace.id)
            .unwrap_or_else(|err| panic!("{err}"));
        assert!(tabs.is_empty(), "the tab it opened survived: {tabs:?}");
        assert!(
            mux.calls()
                .iter()
                .any(|call| matches!(call, Call::CloseTab(_))),
            "the tab must be closed, not abandoned"
        );
        let state = ctx.state().unwrap_or_else(|err| panic!("{err}"));
        assert!(state.resources.is_empty(), "{:?}", state.resources);
        let task = state.tasks.get("T1").unwrap_or_else(|| panic!("no task"));
        assert!(task.attempts.is_empty());
        assert!(state.escalations.is_empty(), "{:?}", state.escalations);
    }

    #[test]
    fn spawn_without_a_workspace_says_to_run_up_first() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.spawn_env(&ctx);
        let mux = FakeMux::default().with_ready_agent();

        let message = match spawn(&mux, &env, &request()) {
            Ok(report) => panic!("spawn should have failed, got {report:?}"),
            Err(err) => err.to_string(),
        };
        assert!(message.contains("shprd up"), "{message}");
    }

    /// Writes the tab record a spawn would have written for one attempt.
    fn record_tab(scratch: &Scratch, run: &str, task: &str, attempt: u32, tab: &shprd_mux::TabId) {
        let path = tab_path(
            &scratch.home.path().join(".shprd").join("briefs"),
            run,
            task,
            attempt,
        );
        write_tab(&path, tab);
        assert!(path.exists(), "{} was not written", path.display());
    }

    #[test]
    fn prompt_reaches_the_tab_its_own_task_spawned() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let mux = FakeMux::default().with_ready_agent();
        let workspace = mux
            .create_workspace(
                &naming::workspace_name(&scratch.repo_name()),
                scratch.root(),
            )
            .unwrap_or_else(|err| panic!("{err}"));
        // These two labels truncate to the same 16 characters, so the title
        // alone would send the second task's prompt to the first task's tab.
        let first = "task-identical-label-one";
        let second = "task-identical-label-two";
        assert_eq!(
            naming::truncate_label(first),
            naming::truncate_label(second)
        );
        let other = mux
            .create_tab(
                &workspace.id,
                &format!("\u{25cf} sh\u{b7}{}", naming::truncate_label(first)),
                scratch.root(),
            )
            .unwrap_or_else(|err| panic!("{err}"));
        let mine = mux
            .create_tab(
                &workspace.id,
                &format!("\u{25cf} sh\u{b7}{}", naming::truncate_label(second)),
                scratch.root(),
            )
            .unwrap_or_else(|err| panic!("{err}"));
        record_tab(&scratch, "t1", first, 1, &other.id);
        record_tab(&scratch, "t1", second, 1, &mine.id);

        prompt_in(&mux, scratch.home.path(), &ctx, second, "keep going")
            .unwrap_or_else(|err| panic!("{err}"));
        let prompted = mux
            .calls()
            .into_iter()
            .find_map(|call| match call {
                Call::Prompt(pane, text) => Some((pane, text)),
                _ => None,
            })
            .unwrap_or_else(|| panic!("nothing was prompted"));
        assert_eq!(prompted.0, mine.root_pane);
        assert_ne!(prompted.0, other.root_pane);
        assert_eq!(prompted.1, "keep going");

        // A task with no record, and a record whose tab is gone, both refuse
        // with the same message rather than prompting the wrong agent.
        let unrecorded = prompt_in(&mux, scratch.home.path(), &ctx, "T9", "hello");
        let message = match unrecorded {
            Ok(()) => panic!("a task with no tab record was prompted"),
            Err(err) => {
                assert_eq!(err.exit_code(), 2);
                err.to_string()
            }
        };
        assert!(message.contains("no open tab for task"), "{message}");

        // The latest attempt's tab is gone from the host: that is refused too,
        // never replaced by an older attempt's tab.
        write_tab(
            &tab_path(
                &scratch.home.path().join(".shprd").join("briefs"),
                "t1",
                first,
                2,
            ),
            &shprd_mux::TabId::new("tab-gone"),
        );
        let closed = prompt_in(&mux, scratch.home.path(), &ctx, first, "hello");
        assert_eq!(closed.map(|_| ()).map_err(|err| err.exit_code()), Err(2));
    }

    /// Two runs can each hold a tab record for the same task. A prompt belongs
    /// to the active run, even when an older run's record has the higher
    /// attempt number.
    #[test]
    fn prompt_ignores_an_older_runs_tab_record() {
        let scratch = Scratch::new();
        let ctx = Ctx::new(scratch.root().to_path_buf(), Actor::Human, false, true);
        ctx.commit(EventKind::RunStarted {
            run_id: "run-two".to_string(),
            repo: scratch.repo_name(),
            base_branch: "main".to_string(),
            integration_branch: "integration/m4".to_string(),
        })
        .unwrap_or_else(|err| panic!("{err}"));
        let mux = FakeMux::default().with_ready_agent();
        let workspace = mux
            .create_workspace(
                &naming::workspace_name(&scratch.repo_name()),
                scratch.root(),
            )
            .unwrap_or_else(|err| panic!("{err}"));
        let stale = mux
            .create_tab(&workspace.id, "stale", scratch.root())
            .unwrap_or_else(|err| panic!("{err}"));
        let current = mux
            .create_tab(&workspace.id, "current", scratch.root())
            .unwrap_or_else(|err| panic!("{err}"));
        // The older run kept retrying, so its attempt number is the higher one.
        record_tab(&scratch, "run-one", "api", 7, &stale.id);
        record_tab(&scratch, "run-two", "api", 1, &current.id);

        prompt_in(&mux, scratch.home.path(), &ctx, "api", "keep going")
            .unwrap_or_else(|err| panic!("{err}"));
        let prompted = mux
            .calls()
            .into_iter()
            .find_map(|call| match call {
                Call::Prompt(pane, _) => Some(pane),
                _ => None,
            })
            .unwrap_or_else(|| panic!("nothing was prompted"));
        assert_eq!(prompted, current.root_pane);
        assert_ne!(prompted, stale.root_pane);
    }

    /// With no run in the folded log there is no current run to prompt in.
    #[test]
    fn prompt_without_a_run_says_so() {
        let scratch = Scratch::new();
        let ctx = Ctx::new(scratch.root().to_path_buf(), Actor::Human, false, true);
        let mux = FakeMux::default().with_ready_agent();
        let message = match prompt_in(&mux, scratch.home.path(), &ctx, "api", "hello") {
            Ok(()) => panic!("a prompt with no active run was sent"),
            Err(err) => {
                assert_eq!(err.exit_code(), 2);
                err.to_string()
            }
        };
        assert!(message.contains("no active run"), "{message}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shprd_core::state::TaskStatus;
    use shprd_roster::skills::Role as SkillRole;

    /// A launch spec whose only model flag carries the `{model}` placeholder.
    fn launch() -> LaunchSpec {
        LaunchSpec {
            interactive: vec!["claude".to_string()],
            model_args: vec!["--model".to_string(), "{model}".to_string()],
            effort_args: None,
            auto_approve: None,
            system_prompt_args: None,
            resume_args: None,
            verified: true,
        }
    }

    #[test]
    fn a_task_without_a_model_gets_no_model_flags() {
        let brief = Path::new("/tmp/brief.md");
        let (argv, in_args) = launch_argv(&launch(), None, None, "brief", brief)
            .unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(argv, ["claude".to_string()]);
        assert!(!in_args);
    }

    #[test]
    fn a_task_with_a_model_gets_its_flags() {
        let brief = Path::new("/tmp/brief.md");
        let (argv, _) = launch_argv(&launch(), Some("opus"), None, "brief", brief)
            .unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(
            argv,
            [
                "claude".to_string(),
                "--model".to_string(),
                "opus".to_string()
            ]
        );
    }

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
