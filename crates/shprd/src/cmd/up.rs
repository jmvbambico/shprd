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
use shprd_mux::Mux;
use shprd_roster::spec::HarnessSpec;

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
    let _ = mux;
    let (run_id, resumed, integration_branch) =
        begin_run(&env.ctx, &env.home, &env.repo_name, &base_branch)?;
    Ok(UpReport {
        run_id,
        resumed,
        base_branch,
        integration_branch,
        created_workspace: false,
        created_chat_tab: false,
        created_orchestrator_tab: false,
        brief_path: env.home.join(".shprd").join("briefs"),
    })
}

/// Runs `shprd down`: the run's last event.
///
/// A task that is still working refuses the event, and the refusal is the whole
/// answer: nothing else is touched, and no tab is closed.
pub fn down(ctx: &Ctx) -> Result<(), CliError> {
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
) -> Result<(String, bool, String), CliError> {
    let state = ctx.state()?;
    if let Some(run) = resume_target(&state) {
        return Ok((run.run_id, true, run.integration_branch));
    }
    let run_id = new_run_id();
    let integration_branch = format!("shprd/{run_id}/int");
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
    Ok((run_id, false, integration_branch))
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
        (ResourceKind::Worktree, worktree.path.to_string_lossy().to_string()),
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
