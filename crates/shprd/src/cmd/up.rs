//! `shprd up` and `shprd down`: the run's lifecycle, and the workspace that
//! hosts it.
//!
//! `up` is the one verb a human runs to start working on a repository. It
//! starts or resumes the run, opens the integration worktree, and lays out the
//! repository workspace: one chat tab for the human and one orchestrator tab
//! for the agent that plans. Everything it does to the multiplexer goes through
//! [`Mux`], so a test drives it with [`shprd_mux::fake::FakeMux`] and the
//! binary drives it with a real host adapter.
//!
//! `down` finishes the run. Neither verb closes a tab: a human's shell is not
//! shprd's to kill.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, anyhow};
use shprd_core::config::Config;
use shprd_core::event::{EventKind, ResourceAction, ResourceKind};
use shprd_core::state::RunState;
use shprd_git::integrate::{self, IntegrationWorktree};
use shprd_git::worktree::Layout;
use shprd_mux::naming::{self, Role as TabRole};
use shprd_mux::{Mux, Tab, TabId, Workspace};
use shprd_roster::env::{self as launch_env_mod, EnvContext, Role as LaunchRole};
use shprd_roster::models::AgentRef;
use shprd_roster::spec::{HarnessSpec, Values, render_args};

use crate::ctx::{CliError, Ctx};

/// The tab title the human's own tab carries.
const CHAT_TITLE: &str = "shprd·chat";

/// The label the orchestrator tab carries after its role glyph and CLI id.
const ORCHESTRATOR_LABEL: &str = "orchestrator";

/// The argv the chat tab runs: print the status, then hand over to a shell.
const CHAT_ARGV: [&str; 3] = ["sh", "-c", "shprd status; exec \"${SHELL:-sh}\""];

/// Everything `up` needs that does not come from the multiplexer.
///
/// The fields are the whole environment the verb reads, so a test builds one
/// without touching the real config, the real home directory or the real
/// process environment.
#[derive(Debug, Clone)]
pub struct UpEnv {
    /// The run log's context: repo root, actor and log path.
    pub ctx: Ctx,
    /// The repository's directory name, used in the workspace name and the
    /// integration worktree's path.
    pub repo_name: String,
    /// Home directory holding `.shprd`: worktrees, briefs.
    pub home: PathBuf,
    /// The resolved configuration.
    pub config: Config,
    /// The harness specs, used to build the orchestrator's argv.
    pub specs: Vec<HarnessSpec>,
    /// The process environment the orchestrator is launched under.
    pub process_env: BTreeMap<String, String>,
}

/// What `up` did or found already done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpReport {
    /// The run that is now in flight.
    pub run_id: String,
    /// True when an unfinished run was resumed instead of started.
    pub resumed: bool,
    /// The branch the run was cut from.
    pub base_branch: String,
    /// The branch landed work is integrated on.
    pub integration_branch: String,
    /// The human's own tab, focused when `up` is done.
    pub chat_tab: TabId,
    /// The orchestrator agent's tab.
    pub orchestrator_tab: TabId,
    /// True when this call created the workspace.
    pub created_workspace: bool,
    /// True when this call created the human's chat tab.
    pub created_chat_tab: bool,
    /// True when this call created the orchestrator tab.
    pub created_orchestrator_tab: bool,
    /// Where the orchestrator brief was written.
    pub brief_path: PathBuf,
}

/// Runs `shprd up`: the run's lifecycle, then the workspace layout.
pub fn up(mux: &dyn Mux, env: &UpEnv) -> Result<UpReport, CliError> {
    let repo = env.ctx.root.clone();
    let base_branch = preconditions(&repo, &env.config)?;
    let boot = begin_run(&env.ctx, &env.home, &env.repo_name, &base_branch)?;
    let run_id = boot.run_id;
    let resumed = boot.resumed;
    let integration_branch = boot.integration_branch;
    let base_branch = boot.base_branch;
    let (workspace, created_workspace) =
        find_or_create_workspace(mux, naming::workspace_name(&env.repo_name), &repo)?;
    let brief_path = write_brief(env, &run_id, &base_branch)?;
    let tabs = mux
        .list_tabs(&workspace.id)
        .map_err(|err| CliError::other(anyhow!(err).context("listing tabs")))?;
    let (chat_tab, created_chat_tab) = if let Some(found) = titled(&tabs, CHAT_TITLE) {
        (found, false)
    } else {
        let tab = mux
            .create_tab(&workspace.id, CHAT_TITLE, &repo)
            .map_err(|err| CliError::other(anyhow!(err).context("creating the chat tab")))?;
        let argv: Vec<String> = CHAT_ARGV.iter().map(|arg| (*arg).to_string()).collect();
        mux.run(&tab.root_pane, &argv, &[])
            .map_err(|err| CliError::other(anyhow!(err).context("starting the chat tab")))?;
        (tab, true)
    };
    let (orchestrator_tab, created_orchestrator_tab) =
        ensure_orchestrator_tab(mux, env, &workspace, &tabs, &brief_path, &run_id)?;
    mux.focus_tab(&chat_tab.id)
        .map_err(|err| CliError::other(anyhow!(err).context("focusing the chat tab")))?;
    Ok(UpReport {
        run_id,
        resumed,
        base_branch,
        integration_branch,
        chat_tab: chat_tab.id,
        orchestrator_tab: orchestrator_tab.id,
        created_workspace,
        created_chat_tab,
        created_orchestrator_tab,
        brief_path,
    })
}

/// Runs `shprd down`: the run's last event.
///
/// A task that is still working refuses the event, and the refusal is the whole
/// answer: nothing else is touched, and no tab is closed.
pub fn down(ctx: &Ctx) -> Result<(), CliError> {
    let state = ctx.state()?;
    if state.run.is_none() {
        return Err(CliError::Invalid(anyhow!(
            "no run in flight: run `shprd up` first"
        )));
    }
    if state.finished {
        return Err(CliError::Invalid(anyhow!(
            "the run already finished; run `shprd up` to start the next one"
        )));
    }
    let event = ctx.commit(EventKind::RunFinished {})?;
    crate::cmd::emit(ctx, &event)
}

/// Checks the three things `up` cannot work without, in the order a human
/// fixes them.
fn preconditions(repo: &Path, config: &Config) -> Result<String, CliError> {
    if !repo.join(".git").exists() {
        return Err(CliError::Invalid(anyhow!(
            "{} is not a git repository: run `shprd up` inside one",
            repo.display()
        )));
    }
    // `rev-parse` answers `HEAD` for a detached HEAD instead of failing, so a
    // human mid-bisect gets the reason a human can act on.
    let branch = git_stdout(repo, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let branch = branch.trim();
    if branch.is_empty() || branch == "HEAD" {
        return Err(CliError::Invalid(anyhow!(
            "HEAD is detached: check out the branch to base the run on"
        )));
    }
    if config.roster.orchestrator.is_none() {
        return Err(CliError::Invalid(anyhow!(
            "no orchestrator configured: run `shprd set orchestrator <agent>`"
        )));
    }
    Ok(branch.to_string())
}

/// Runs one git command in `repo` and returns its stdout.
fn git_stdout(repo: &Path, args: &[&str]) -> Result<String, CliError> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .context("running git")
        .map_err(CliError::other)?;
    if !output.status.success() {
        return Err(CliError::Invalid(anyhow!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Reuses the run in flight, or starts a fresh one and opens its integration
/// worktree.
///
/// Returns the run id, whether it was resumed, and its integration branch.
fn begin_run(
    ctx: &Ctx,
    home: &Path,
    repo_name: &str,
    base_branch: &str,
) -> Result<RunBoot, CliError> {
    let state = ctx.state()?;
    if let Some(run) = resume_target(&state) {
        return Ok(RunBoot {
            run_id: run.run_id,
            resumed: true,
            base_branch: run.base_branch,
            integration_branch: run.integration_branch,
        });
    }
    let run_id = new_run_id();
    let integration_branch = integration_branch(&run_id);
    ctx.commit(EventKind::RunStarted {
        run_id: run_id.clone(),
        repo: ctx.root.to_string_lossy().to_string(),
        base_branch: base_branch.to_string(),
        integration_branch: integration_branch.clone(),
    })?;
    let layout = Layout::default_for_home(home);
    let worktree = integrate::open_integration(&ctx.root, &layout, repo_name, &run_id, base_branch)
        .map_err(|err| CliError::other(anyhow!(err).context("opening the integration worktree")))?;
    record_integration(ctx, &worktree)?;
    Ok(RunBoot {
        run_id,
        resumed: false,
        base_branch: base_branch.to_string(),
        integration_branch,
    })
}

/// What `up` learned about the run before touching the multiplexer.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RunBoot {
    /// The run now in flight.
    run_id: String,
    /// True when an unfinished run was resumed.
    resumed: bool,
    /// The branch the run was cut from.
    base_branch: String,
    /// The branch landed work is integrated on.
    integration_branch: String,
}

/// The integration branch a run carries: `shprd/<run>/int`.
fn integration_branch(run_id: &str) -> String {
    format!("shprd/{run_id}/int")
}

/// The run `up` should continue, if there is one still in flight.
fn resume_target(state: &RunState) -> Option<shprd_core::state::RunInfo> {
    if state.finished {
        return None;
    }
    state.run.clone()
}

/// Appends the two resources a fresh integration worktree owns.
///
/// Only what this call created is recorded: the ledger is what licenses the
/// janitor to remove something, so a worktree that was already there is not
/// shprd's to claim again.
fn record_integration(ctx: &Ctx, worktree: &IntegrationWorktree) -> Result<(), CliError> {
    if !worktree.created {
        return Ok(());
    }
    for (kind, name) in [
        (
            ResourceKind::Worktree,
            worktree.path.to_string_lossy().to_string(),
        ),
        (ResourceKind::Branch, worktree.branch.clone()),
    ] {
        ctx.commit(EventKind::Resource {
            action: ResourceAction::Created,
            kind,
            name,
            task: None,
        })?;
    }
    Ok(())
}

/// A run id: `r` and six lowercase hex digits drawn from the clock and the pid.
///
/// Time alone collides when two runs start in the same millisecond, and the pid
/// alone collides across containers, so the two are mixed. No dependency, and
/// no randomness shprd has to seed.
fn new_run_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0_u128, |elapsed| elapsed.as_nanos());
    let pid = u128::from(std::process::id());
    let mixed = nanos.wrapping_mul(0x517c_c1b7_2722_0a95) ^ pid.wrapping_mul(0x9e37_79b9);
    format!("r{:06x}", mixed & 0xff_ffff)
}

/// The binary's `up`: build the real environment, drive the real host, print
/// what happened.
pub fn up_verb(ctx: &Ctx, home: Option<&Path>) -> Result<(), CliError> {
    let home = crate::ctx::resolve_home(home)?;
    // Decide the one refusal a human can fix before loading the harness table:
    // on a fresh machine the missing orchestrator is the real problem, and the
    // missing spec is only a symptom of it.
    if load_config(ctx, &home)?.roster.orchestrator.is_none() {
        return Err(CliError::Invalid(anyhow!(
            "no orchestrator configured: run `shprd set orchestrator <agent>`"
        )));
    }
    let env = build_env(ctx, home)?;
    let mux = shprd_mux_herdr::HerdrMux::new();
    let report = up(&mux, &env)?;
    print_report(ctx, &report)
}

/// Collects the config, the harness specs and the process environment.
fn build_env(ctx: &Ctx, home: PathBuf) -> Result<UpEnv, CliError> {
    let repo_name = ctx
        .root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| {
            CliError::other(anyhow!(
                "cannot name the repository: {} has no last path component",
                ctx.root.display()
            ))
        })?;
    let config = load_config(ctx, &home)?;
    let dir = crate::cmd::roster::resolve_data_dir()?;
    let specs = shprd_roster::spec::load_dir(&dir.join("harness")).map_err(|err| {
        CliError::other(anyhow!(err).context(format!("loading {}", dir.display())))
    })?;
    Ok(UpEnv {
        ctx: ctx.clone(),
        repo_name,
        home,
        config,
        specs,
        process_env: std::env::vars().collect(),
    })
}

/// The merged global and repository config.
fn load_config(ctx: &Ctx, home: &Path) -> Result<Config, CliError> {
    let global = home.join(".config").join("shprd").join("config.toml");
    let repo = ctx.root.join(".shprd").join("config.toml");
    shprd_core::config::load(Some(&global), Some(&repo))
        .map_err(|err| CliError::other(anyhow!(err).context("reading the config")))
}

/// Prints what `up` did: JSON when asked, else one short human line.
fn print_report(ctx: &Ctx, report: &UpReport) -> Result<(), CliError> {
    if ctx.json {
        let value = serde_json::json!({
            "run": report.run_id,
            "resumed": report.resumed,
            "base": report.base_branch,
            "integration": report.integration_branch,
            "brief": report.brief_path.to_string_lossy(),
        });
        let text = serde_json::to_string(&value)
            .map_err(|err| CliError::other(anyhow!(err).context("serializing the report")))?;
        println!("{text}");
    } else {
        let verb = if report.resumed { "resumed" } else { "started" };
        println!(
            "{verb} run {} on {} (integration {})",
            report.run_id, report.base_branch, report.integration_branch
        );
    }
    Ok(())
}

/// The first tab carrying `title`, if any.
///
/// Titles are shprd's own interface, so they are the identity a resumed `up`
/// keys on: a tab found by title is the tab a previous `up` created.
fn titled(tabs: &[Tab], title: &str) -> Option<Tab> {
    tabs.iter().find(|tab| tab.title == title).cloned()
}

/// Finds the repository's workspace, or creates it with its first pane in the
/// repository root.
fn find_or_create_workspace(
    mux: &dyn Mux,
    name: String,
    repo: &Path,
) -> Result<(Workspace, bool), CliError> {
    if let Some(found) = mux
        .find_workspace(&name)
        .map_err(|err| CliError::other(anyhow!(err).context("finding the workspace")))?
    {
        return Ok((found, false));
    }
    let created = mux
        .create_workspace(&name, repo)
        .map_err(|err| CliError::other(anyhow!(err).context("creating the workspace")))?;
    Ok((created, true))
}

/// Opens the orchestrator's tab unless it is already there.
///
/// An existing tab is never recreated and never re-prompted: a human may be
/// mid-conversation in it, and a second start would replace the session they
/// are typing into.
fn ensure_orchestrator_tab(
    mux: &dyn Mux,
    env: &UpEnv,
    workspace: &Workspace,
    tabs: &[Tab],
    brief_path: &Path,
    run_id: &str,
) -> Result<(Tab, bool), CliError> {
    let plan = OrchestratorPlan::resolve(env, brief_path, run_id)?;
    if let Some(found) = titled(tabs, &plan.title) {
        return Ok((found, false));
    }
    let tab = mux
        .create_tab(&workspace.id, &plan.title, &env.ctx.root)
        .map_err(|err| CliError::other(anyhow!(err).context("creating the orchestrator tab")))?;
    mux.run(&tab.root_pane, &plan.argv, &plan.env)
        .map_err(|err| CliError::other(anyhow!(err).context("starting the orchestrator")))?;
    if let Some(prompt) = &plan.inline_prompt {
        mux.prompt(&tab.root_pane, prompt)
            .map_err(|err| CliError::other(anyhow!(err).context("prompting the orchestrator")))?;
    }
    Ok((tab, true))
}

/// Everything one orchestrator launch needs: the title shprd finds it by
/// later, the process to run, and how the brief reaches the agent.
#[derive(Debug, Clone)]
struct OrchestratorPlan {
    /// The tab title, stable across `up` invocations.
    title: String,
    /// The program and its arguments.
    argv: Vec<String>,
    /// The environment diff as `run` takes it.
    env: Vec<(String, Option<String>)>,
    /// The brief, when the CLI has no system-prompt flag to carry it.
    inline_prompt: Option<String>,
}

impl OrchestratorPlan {
    /// Builds the plan from the config, the harness specs and the brief.
    fn resolve(env: &UpEnv, brief_path: &Path, run_id: &str) -> Result<Self, CliError> {
        let line = env.config.roster.orchestrator.clone().ok_or_else(|| {
            CliError::Invalid(anyhow!(
                "no orchestrator configured: run `shprd set orchestrator <agent>`"
            ))
        })?;
        let agent = AgentRef::parse(&line, &env.specs)
            .map_err(|err| CliError::other(anyhow!(err).context("reading the orchestrator")))?;
        let spec = env
            .specs
            .iter()
            .find(|spec| spec.id == agent.program)
            .ok_or_else(|| CliError::other(anyhow!("no harness spec for {:?}", agent.program)))?;
        let effort = env.config.roster.effort.get(&line).cloned();
        let brief = std::fs::read_to_string(brief_path)
            .map_err(|err| CliError::other(anyhow!(err).context("reading the brief")))?;
        let mut values = Values {
            model: agent.model.clone(),
            effort: effort.clone(),
            prompt: None,
            prompt_file: None,
            session: None,
        };
        let mut argv = spec.launch.interactive.clone();
        argv.extend(render(&spec.launch.model_args, &values, "model_args")?);
        if let Some(template) = &spec.launch.effort_args
            && effort.is_some()
        {
            argv.extend(render(template, &values, "effort_args")?);
        }
        let mut inline_prompt = Some(brief.clone());
        if let Some(template) = &spec.launch.system_prompt_args {
            values.prompt = Some(brief);
            values.prompt_file = Some(brief_path.to_string_lossy().into_owned());
            argv.extend(render(template, &values, "system_prompt_args")?);
            inline_prompt = None;
        }
        Ok(Self {
            title: naming::tab_title(TabRole::Orchestrator, &spec.id, ORCHESTRATOR_LABEL, false),
            argv,
            env: launch_env_mod::launch_env(
                &env.process_env,
                LaunchRole::Orchestrator,
                &EnvContext {
                    actor: "orchestrator".to_string(),
                    task: None,
                    run: Some(run_id.to_string()),
                    repo: env.ctx.root.clone(),
                },
            )
            .as_pairs(),
            inline_prompt,
        })
    }
}

/// Renders one argument template, naming it in the error when it cannot be.
fn render(template: &[String], values: &Values, what: &str) -> Result<Vec<String>, CliError> {
    if template.is_empty() {
        return Ok(Vec::new());
    }
    render_args(template, values)
        .map_err(|err| CliError::other(anyhow!(err).context(format!("rendering {what}"))))
}

/// Writes the orchestrator's brief and returns its path.
///
/// The brief is rewritten on every `up` so a resumed run gets the current one,
/// even though the tab that reads it is only started once.
fn write_brief(env: &UpEnv, run_id: &str, base_branch: &str) -> Result<PathBuf, CliError> {
    let dir = env.home.join(".shprd").join("briefs").join(run_id);
    std::fs::create_dir_all(&dir).map_err(|err| {
        CliError::other(anyhow!(err).context(format!("creating {}", dir.display())))
    })?;
    let path = dir.join("orchestrator.md");
    std::fs::write(&path, brief_text(env, run_id, base_branch)).map_err(|err| {
        CliError::other(anyhow!(err).context(format!("writing {}", path.display())))
    })?;
    Ok(path)
}

/// The brief's text: who you are, where the branches are, and which verbs the
/// engine validates.
fn brief_text(env: &UpEnv, run_id: &str, base_branch: &str) -> String {
    let skills = if env.config.skills.orchestrator.is_empty() {
        "none configured".to_string()
    } else {
        env.config.skills.orchestrator.join(", ")
    };
    let repo = env.ctx.root.display();
    format!(
        "# Orchestrator brief\n\n\
You are the orchestrator for run `{run_id}` in `{repo}`.\n\n\
## Branches\n\n\
- base: `{base}`\n\
- integration: `{integration}`\n\n\
## Your verbs\n\n\
Every verb writes one event to the run log, and the engine checks it against\n\
the transition rules: a verb that does not fit the current state is refused\n\
and nothing is written. Record what you do; do not keep state in your head.\n\n\
- `shprd agent task add --id M4.1 --title \"…\" --scope <path> --acceptance <text>`\n\
- `shprd agent spawn --task M4.1 --attempt 1 --agent claude/claude-opus-5-5 --worktree <path> --branch feature/m4-1`\n\
- `shprd agent report --task M4.1 --attempt 1 --status completed --summary \"…\"`\n\
- `shprd agent gate pass --task M4.1 --attempt 1 --gate lint`\n\
- `shprd agent integrate --task M4.1 --commit <sha>`\n\
- `shprd agent escalate --body \"…\" --default \"…\"` when a human must decide\n\
- `shprd status` to see where every task sits\n\n\
## Skills\n\n\
Configured for you: {skills}\n\n\
Load them before planning; they carry the conventions this repository runs on.\n",
        base = base_branch,
        integration = integration_branch(run_id),
    )
}

#[cfg(test)]
mod tests {
    use std::path::Path as StdPath;

    use anyhow::{Context, Result};
    use shprd_core::config::{Config, Skills};
    use shprd_core::event::Actor;
    use shprd_mux::Capabilities;
    use shprd_mux::fake::{Call, FakeMux};
    use shprd_roster::spec::load_dir;
    use tempfile::TempDir;

    use super::*;

    /// Turns a verb's failure into an [`anyhow::Error`], which is what a test
    /// returns so a failing assertion prints the verb's own message.
    fn failed(err: CliError) -> anyhow::Error {
        anyhow!("{err}")
    }

    /// A temp repository, a temp home and the environment `up` reads.
    struct Fixture {
        /// Temp home: worktrees and briefs land here.
        home: TempDir,
        /// Temp repository, the run's root.
        repo: TempDir,
        /// Configured as the orchestrator, when the test wants one.
        orchestrator: Option<String>,
    }

    impl Fixture {
        /// A committed repository on `main`, so HEAD names a branch.
        fn new(orchestrator: Option<&str>) -> Result<Self> {
            let repo = TempDir::new()?;
            git(repo.path(), &["init", "-b", "main"])?;
            git(repo.path(), &["add", "-A"])?;
            git(
                repo.path(),
                &[
                    "-c",
                    "user.name=shprd test",
                    "-c",
                    "user.email=shprd@example.com",
                    "commit",
                    "--allow-empty",
                    "-m",
                    "first",
                ],
            )?;
            Ok(Self {
                home: TempDir::new()?,
                repo,
                orchestrator: orchestrator.map(str::to_string),
            })
        }

        /// The environment `up` takes, with the real bundled harness table.
        fn env(&self) -> Result<UpEnv> {
            let data = StdPath::new(env!("CARGO_MANIFEST_DIR")).join("../../data/harness");
            let config = Config {
                roster: shprd_core::config::Roster {
                    orchestrator: self.orchestrator.clone(),
                    effort: self
                        .orchestrator
                        .iter()
                        .map(|line| (line.clone(), "high".to_string()))
                        .collect(),
                    ..Default::default()
                },
                skills: Skills::default(),
                ..Default::default()
            };
            Ok(UpEnv {
                ctx: Ctx::new(self.repo.path().to_path_buf(), Actor::Human, false, true),
                repo_name: "repo".to_string(),
                home: self.home.path().to_path_buf(),
                config,
                specs: load_dir(&data)?,
                process_env: [("PATH".to_string(), "/usr/bin:/bin".to_string())]
                    .into_iter()
                    .collect(),
            })
        }
    }

    /// Runs one git command in `dir`, failing the test when git refuses.
    fn git(dir: &StdPath, args: &[&str]) -> Result<()> {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .context("running git")?;
        anyhow::ensure!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    }

    /// The tabs the fake was asked to create, in order.
    fn created_titles(mux: &FakeMux) -> Vec<String> {
        mux.calls()
            .into_iter()
            .filter_map(|call| match call {
                Call::CreateTab(_, title, _) => Some(title),
                _ => None,
            })
            .collect()
    }

    /// The argv of every program the fake was asked to start.
    fn started_argvs(mux: &FakeMux) -> Vec<Vec<String>> {
        mux.calls()
            .into_iter()
            .filter_map(|call| match call {
                Call::Run(_, argv, _) => Some(argv),
                _ => None,
            })
            .collect()
    }

    /// A fake host that claims every capability.
    fn mux() -> FakeMux {
        FakeMux::new(Capabilities {
            split_ratio: true,
            pane_grants: true,
            human_ask: true,
            resumable_events: true,
            agent_state: true,
        })
    }

    #[test]
    fn first_up_creates_the_workspace_and_starts_a_run() -> Result<()> {
        let fixture = Fixture::new(Some("claude/claude-opus-5-5"))?;
        let env = fixture.env()?;
        let mux = mux();
        let report = up(&mux, &env).map_err(failed)?;

        assert!(!report.resumed, "a fresh log cannot resume");
        assert!(report.created_workspace, "the workspace is new");
        assert!(report.created_chat_tab, "the chat tab is new");
        assert!(
            report.created_orchestrator_tab,
            "the orchestrator tab is new"
        );
        assert_eq!(report.base_branch, "main");
        assert_eq!(
            report.integration_branch,
            format!("shprd/{}/int", report.run_id)
        );
        assert_eq!(report.run_id.len(), 7);
        assert!(report.run_id.starts_with('r'), "{}", report.run_id);
        assert!(
            report.run_id[1..].chars().all(|c| c.is_ascii_hexdigit()),
            "{}",
            report.run_id
        );

        // One workspace named after the repo, two tabs: the human's and the
        // orchestrator's, in that order.
        assert!(mux.calls().iter().any(|call| matches!(
            call,
            Call::CreateWorkspace(name, cwd)
                if name == "shprd · repo" && cwd == fixture.repo.path()
        )));
        assert_eq!(
            created_titles(&mux),
            vec![
                "shprd·chat".to_string(),
                "★ claude·orchestrator".to_string()
            ]
        );

        // The chat placeholder and the orchestrator CLI, with the brief in its
        // system prompt and the run in its environment.
        let argvs = started_argvs(&mux);
        assert_eq!(argvs.len(), 2);
        assert_eq!(argvs[0], CHAT_ARGV.map(str::to_string).to_vec());
        assert_eq!(
            argvs[1][..5],
            ["claude", "--model", "claude-opus-5-5", "--effort", "high"]
        );
        assert_eq!(argvs[1][5], "--append-system-prompt-file");
        assert!(argvs[1][6].ends_with("orchestrator.md"));
        let orchestrator_env = mux
            .calls()
            .into_iter()
            .filter_map(|call| match call {
                Call::Run(_, argv, env) if argv.first().map(String::as_str) == Some("claude") => {
                    Some(env)
                }
                _ => None,
            })
            .next()
            .context("the orchestrator was started")?;
        assert!(orchestrator_env.contains(&("SHPRD_RUN".to_string(), Some(report.run_id.clone()))));

        // The brief is on disk, and the run owns an integration worktree and
        // the branch checked out in it.
        let brief = std::fs::read_to_string(&report.brief_path)?;
        assert!(brief.contains(&report.run_id));
        assert!(brief.contains("shprd agent spawn"), "{brief}");
        assert!(report.brief_path.ends_with("orchestrator.md"));
        let integration = fixture
            .home
            .path()
            .join(".shprd")
            .join("wt")
            .join("repo")
            .join(&report.run_id)
            .join("_int");
        assert!(integration.is_dir(), "{} is missing", integration.display());
        assert_eq!(env.ctx.state().map_err(failed)?.resources.len(), 2);

        // The chat tab is focused last, so the human lands where they type.
        assert!(matches!(
            mux.calls().last(),
            Some(Call::FocusTab(tab)) if tab == &report.chat_tab
        ));
        Ok(())
    }

    #[test]
    fn second_up_resumes_without_touching_the_host() -> Result<()> {
        let fixture = Fixture::new(Some("claude/claude-opus-5-5"))?;
        let env = fixture.env()?;
        let mux = mux();
        let first = up(&mux, &env).map_err(failed)?;
        let before = mux.calls().len();
        let second = up(&mux, &env).map_err(failed)?;

        assert!(second.resumed, "an unfinished run is resumed");
        assert_eq!(second.run_id, first.run_id);
        assert_eq!(second.integration_branch, first.integration_branch);
        assert!(!second.created_workspace);
        assert!(!second.created_chat_tab);
        assert!(!second.created_orchestrator_tab);
        let later = mux.calls()[before..].to_vec();
        assert!(
            later.iter().all(|call| !matches!(
                call,
                Call::CreateWorkspace(..) | Call::CreateTab(..) | Call::Run(..) | Call::Prompt(..)
            )),
            "second up changed the host: {later:?}"
        );
        assert!(
            later
                .iter()
                .any(|call| matches!(call, Call::FindWorkspace(_))),
            "second up should look for the workspace: {later:?}"
        );
        Ok(())
    }

    #[test]
    fn up_refuses_without_an_orchestrator() -> Result<()> {
        let fixture = Fixture::new(None)?;
        let env = fixture.env()?;
        let mux = mux();
        let err = up(&mux, &env).err().context("up must refuse")?;
        assert_eq!(err.exit_code(), 2, "{err}");
        assert!(err.to_string().contains("shprd set orchestrator"), "{err}");
        assert!(mux.calls().is_empty(), "nothing may reach the host");
        assert!(
            env.ctx.state().map_err(failed)?.run.is_none(),
            "nothing may be written"
        );
        Ok(())
    }

    /// Starts a run, then adds one task and spawns an agent for it.
    fn working_task(ctx: &Ctx) -> Result<()> {
        ctx.commit(EventKind::RunStarted {
            run_id: "rabc123".to_string(),
            repo: "/tmp/repo".to_string(),
            base_branch: "main".to_string(),
            integration_branch: "shprd/rabc123/int".to_string(),
        })
        .map_err(failed)?;
        ctx.commit(EventKind::TaskAdded {
            task: "M4.1".to_string(),
            title: "up and down".to_string(),
            scope: vec!["crates/shprd".to_string()],
            acceptance: vec!["up works".to_string()],
            tags: Vec::new(),
            skills: Vec::new(),
            deps: Vec::new(),
        })
        .map_err(failed)?;
        ctx.commit(EventKind::Spawned {
            task: "M4.1".to_string(),
            attempt: 1,
            agent: "claude/claude-opus-5-5".to_string(),
            model: None,
            worktree: "/tmp/repo/wt".to_string(),
            branch: "feature/m4-up".to_string(),
        })
        .map_err(failed)?;
        Ok(())
    }

    #[test]
    fn down_refuses_while_a_task_is_working() -> Result<()> {
        let fixture = Fixture::new(Some("claude/claude-opus-5-5"))?;
        let env = fixture.env()?;
        working_task(&env.ctx)?;
        let err = down(&env.ctx).err().context("down must refuse")?;
        assert_eq!(err.exit_code(), 2, "{err}");
        assert!(err.to_string().contains("Working"), "{err}");
        assert!(
            !env.ctx.state().map_err(failed)?.finished,
            "a refused finish writes nothing"
        );
        Ok(())
    }

    #[test]
    fn down_finishes_a_run_with_nothing_in_flight() -> Result<()> {
        let fixture = Fixture::new(Some("claude/claude-opus-5-5"))?;
        let env = fixture.env()?;
        env.ctx
            .commit(EventKind::RunStarted {
                run_id: "rabc123".to_string(),
                repo: "/tmp/repo".to_string(),
                base_branch: "main".to_string(),
                integration_branch: "shprd/rabc123/int".to_string(),
            })
            .map_err(failed)?;
        down(&env.ctx).map_err(failed)?;
        assert!(env.ctx.state().map_err(failed)?.finished);
        Ok(())
    }

    #[test]
    fn up_refuses_on_a_detached_head() -> Result<()> {
        let fixture = Fixture::new(Some("claude/claude-opus-5-5"))?;
        git(fixture.repo.path(), &["checkout", "--detach"])?;
        let env = fixture.env()?;
        let mux = mux();
        let err = up(&mux, &env).err().context("up must refuse")?;
        assert_eq!(err.exit_code(), 2, "{err}");
        assert!(err.to_string().contains("detached"), "{err}");
        assert!(mux.calls().is_empty());
        Ok(())
    }

    /// A private `herdr server`, in a fresh directory, stopped on drop.
    ///
    /// The test is `#[ignore]` because it starts a real server. It is isolated
    /// twice over: `HOME` and `HERDR_SOCKET_PATH` both point into the temp
    /// directory, so it cannot see or touch the human's own multiplexer.
    struct Scratch {
        /// Temp home: config, briefs, worktrees and the server's state.
        home: TempDir,
        /// The private server's socket.
        socket: PathBuf,
        /// The server process, stopped on drop.
        server: Option<std::process::Child>,
    }

    impl Scratch {
        /// Starts a server and waits until it answers.
        fn start() -> Result<Self> {
            let home = TempDir::new()?;
            let socket = home.path().join("h.sock");
            let log = std::fs::File::create(home.path().join("server.log"))?;
            let child = std::process::Command::new("herdr")
                .arg("server")
                .env("HOME", home.path())
                .env("HERDR_SOCKET_PATH", &socket)
                .stdout(std::process::Stdio::null())
                .stderr(log)
                .spawn()
                .context("herdr must be installed to run this test")?;
            let scratch = Self {
                home,
                socket,
                server: Some(child),
            };
            let mux = scratch.mux();
            let started = std::time::Instant::now();
            while mux.find_workspace("does-not-exist").is_err() {
                anyhow::ensure!(
                    started.elapsed() < std::time::Duration::from_secs(20),
                    "the herdr server never answered: {}",
                    std::fs::read_to_string(scratch.home.path().join("server.log"))
                        .unwrap_or_default()
                );
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            Ok(scratch)
        }

        /// An adapter pointed at this scratch server, never at the human's.
        fn mux(&self) -> shprd_mux_herdr::HerdrMux {
            shprd_mux_herdr::HerdrMux::new().with_environment(&self.socket, self.home.path())
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            if let Some(mut server) = self.server.take() {
                let _ = std::process::Command::new("herdr")
                    .args(["server", "stop"])
                    .env("HOME", self.home.path())
                    .env("HERDR_SOCKET_PATH", &self.socket)
                    .status();
                let _ = server.kill();
                let _ = server.wait();
            }
        }
    }

    /// A harness spec for `sh`: a fake orchestrator CLI that starts a shell.
    ///
    /// Its only model flag is a `-c` that execs a plain shell, and it has no
    /// system-prompt flag, so the pane runs an interactive `sh` and the brief
    /// stays in the file. That way no real agent CLI is ever started.
    fn sh_spec_dir(home: &StdPath) -> Result<PathBuf> {
        let source =
            StdPath::new(env!("CARGO_MANIFEST_DIR")).join("../../data/harness/claude.toml");
        let text = std::fs::read_to_string(&source)?;
        let mut kept: Vec<String> = Vec::new();
        for line in text.lines() {
            let dropped = [
                "effort_args",
                "model_args",
                "auto_approve",
                "system_prompt_args",
            ]
            .iter()
            .any(|key| line.trim_start().starts_with(key));
            if dropped {
                continue;
            }
            if line.starts_with("id = ") {
                kept.push("id = \"sh\"".to_string());
                continue;
            }
            if line.starts_with("interactive = ") {
                kept.push("interactive = [\"sh\"]".to_string());
                kept.push("model_args = [\"-c\", \"exec sh\", \"{model}\"]".to_string());
                kept.push("system_prompt_args = []".to_string());
                continue;
            }
            kept.push(line.to_string());
        }
        let dir = home.join("data").join("harness");
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("sh.toml"), kept.join("\n"))?;
        Ok(dir)
    }

    /// `up` twice against an isolated server leaves exactly two tabs.
    ///
    /// Run by hand with `cargo test -p shprd --bin shprd -- --ignored`.
    #[test]
    #[ignore = "starts a real herdr server; run by hand"]
    fn up_twice_lays_out_one_workspace_on_a_private_server() -> Result<()> {
        let scratch = Scratch::start()?;
        let repo = scratch.home.path().join("repo");
        std::fs::create_dir_all(&repo)?;
        git(&repo, &["init", "-b", "main"])?;
        git(
            &repo,
            &[
                "-c",
                "user.name=shprd test",
                "-c",
                "user.email=shprd@example.com",
                "commit",
                "--allow-empty",
                "-m",
                "first",
            ],
        )?;

        let data = sh_spec_dir(scratch.home.path())?;
        let env = UpEnv {
            ctx: Ctx::new(repo.clone(), Actor::Human, false, true),
            repo_name: "repo".to_string(),
            home: scratch.home.path().to_path_buf(),
            config: Config {
                roster: shprd_core::config::Roster {
                    orchestrator: Some("sh/fake-model".to_string()),
                    ..Default::default()
                },
                skills: Skills::default(),
                ..Default::default()
            },
            specs: load_dir(&data)?,
            process_env: std::env::vars().collect(),
        };
        let mux = scratch.mux();
        let first = up(&mux, &env).map_err(failed)?;
        assert!(first.created_workspace && first.created_chat_tab);
        let second = up(&mux, &env).map_err(failed)?;
        assert!(second.resumed);
        assert_eq!(second.run_id, first.run_id);

        // The host itself agrees: one chat tab, one orchestrator tab.
        let listed = mux
            .list_tabs(&shprd_mux::WorkspaceId::new(
                mux.find_workspace("shprd · repo")
                    .map_err(|err| anyhow!("{err}"))?
                    .context("the workspace exists")?
                    .id
                    .as_str(),
            ))
            .map_err(|err| anyhow!("{err}"))?;
        for title in ["shprd·chat", "★ sh·orchestrator"] {
            let count = listed.iter().filter(|tab| tab.title == title).count();
            assert_eq!(count, 1, "{title} appears {count} times in {listed:?}");
        }
        let output = std::process::Command::new("herdr")
            .args(["tab", "list"])
            .env("HOME", scratch.home.path())
            .env("HERDR_SOCKET_PATH", &scratch.socket)
            .output()?;
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        for title in ["shprd·chat", "★ sh·orchestrator"] {
            assert_eq!(
                text.matches(title).count(),
                1,
                "herdr tab list shows {title} {} times:\n{text}",
                text.matches(title).count()
            );
        }
        Ok(())
    }
}
