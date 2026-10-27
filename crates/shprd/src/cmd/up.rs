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
    let branch = git_stdout(repo, &["symbolic-ref", "--short", "HEAD"])?;
    if branch.trim().is_empty() {
        return Err(CliError::Invalid(anyhow!(
            "HEAD is detached: check out the branch to base the run on"
        )));
    }
    if config.roster.orchestrator.is_none() {
        return Err(CliError::Invalid(anyhow!(
            "no orchestrator configured: run `shprd set orchestrator <agent>`"
        )));
    }
    Ok(branch.trim().to_string())
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
    let env = build_env(ctx, home)?;
    let mux = shprd_mux_herdr::HerdrMux::new();
    let report = up(&mux, &env)?;
    print_report(ctx, &report)
}

/// Collects the config, the harness specs and the process environment.
fn build_env(ctx: &Ctx, home: Option<&Path>) -> Result<UpEnv, CliError> {
    let home = crate::ctx::resolve_home(home)?;
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
    let global = home.join(".config").join("shprd").join("config.toml");
    let repo = ctx.root.join(".shprd").join("config.toml");
    let config = shprd_core::config::load(Some(&global), Some(&repo))
        .map_err(|err| CliError::other(anyhow!(err).context("reading the config")))?;
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
