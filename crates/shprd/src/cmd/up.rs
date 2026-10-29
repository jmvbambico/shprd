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
//! `down` finishes the run. Neither verb closes a tab a human is working in: a
//! human's shell is not shprd's to kill. The one exception is the reviewer
//! tabs the run opened itself, which `down` closes once the run's last event is
//! recorded — a reviewer has no work to finish once its task is decided, and
//! every other tab the run recorded is closed with them.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, anyhow};
use shprd_core::config::Config;
use shprd_core::event::{EventKind, ResourceAction, ResourceKind};
use shprd_core::state::{ResourceStatus, RunState};
use shprd_git::integrate::{self, IntegrationWorktree};
use shprd_git::worktree::Layout;
use shprd_mux::naming::{self, Role as TabRole};
use shprd_mux::{AgentState, Direction, Mux, MuxError, PaneId, Tab, TabId, Workspace};
use shprd_roster::env::{self as launch_env_mod, EnvContext, Role as LaunchRole};
use shprd_roster::models::AgentRef;
use shprd_roster::spec::{BlockingPrompt, HarnessSpec, Values, render_args};

use crate::cmd::spawn;
use crate::ctx::{CliError, Ctx};

/// The tab title the human's own tab carries.
const CHAT_TITLE: &str = "shprd·chat";

/// The label the orchestrator tab carries after its role glyph and CLI id.
const ORCHESTRATOR_LABEL: &str = "orchestrator";

/// How long `up` keeps asking the host whether an existing orchestrator tab
/// still has an agent in it.
///
/// A restored workspace keeps the tab but not always the session: herdr's
/// `--resume` can fail and leave a bare shell, which only the absence of an
/// agent tells apart from a live one.
const DETECT_WAIT: Duration = Duration::from_secs(5);

/// The step between [`Mux::agent_state`] polls while detecting that agent.
const DETECT_POLL: Duration = Duration::from_millis(250);

/// The directory under `.shprd` a finished run's log is moved to.
const RUNS_DIR: &str = "runs";

/// The paths shprd writes inside the repository, which git must not see.
const EXCLUDES: [&str; 2] = [".shprd/", ".dagr/"];

/// The comment the shprd entries are appended under.
const EXCLUDE_COMMENT: &str = "# shprd run state";

/// The argv the chat tab runs: a live status view that never starts a shell.
///
/// The status comes from this very binary rather than from `PATH`, so the tab
/// reports the run `up` just laid out even when an older `shprd` is installed.
/// The tab never hands over to `$SHELL`: a login shell re-runs the user's rc
/// file, whose banner and errors make the placeholder look broken.
fn chat_argv() -> Vec<String> {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("shprd"));
    vec![
        "sh".to_string(),
        "-c".to_string(),
        format!(
            "while :; do clear; {} status; printf '\\n  shprd·chat arrives in M7 · talk to the ★ orchestrator tab · refreshes every 5 s\\n'; sleep 5; done",
            posix_quote(&exe)
        ),
    ]
}

/// Quotes `path` as one word for a POSIX shell, so a path with spaces or
/// quotes reaches the shell as a single argument.
fn posix_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

/// The fraction of the chat tab the watchdog's pane takes.
///
/// A third of the tab is enough for the one line the watchdog prints when it
/// acts, and leaves the human's own view the majority of the tab.
const WATCH_RATIO: f32 = 0.3;

/// The argv the watchdog pane runs: this binary, watching.
///
/// The program comes from `current_exe` rather than from `PATH`, so the pane
/// runs the watchdog of the build that laid the run out, not an installed
/// `shprd` an upgrade left behind.
fn watch_argv() -> Vec<String> {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("shprd"));
    vec![exe.to_string_lossy().into_owned(), "watch".to_string()]
}

/// The environment the watchdog runs under: the engine's, for this run.
///
/// The watchdog records verdicts and hands workers off, so it acts as the
/// engine, and it gets the environment the engine's own processes get: the
/// engine's credentials and none of a worker's prompt suppression.
fn watch_env(env: &UpEnv, run_id: &str) -> Vec<(String, Option<String>)> {
    launch_env_mod::launch_env(
        &env.process_env,
        LaunchRole::Engine,
        &EnvContext {
            actor: "engine".to_string(),
            task: None,
            run: Some(run_id.to_string()),
            repo: env.ctx.root.clone(),
        },
    )
    .as_pairs()
}

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
    /// How long a freshly launched orchestrator gets to be detected as an agent
    /// before its brief counts as undelivered. Production reads it from the
    /// environment; tests set it explicitly.
    pub ready_wait: Duration,
    /// How long `up` waits for an orchestrator tab it did not create to report
    /// an agent before it relaunches the orchestrator in that tab. Production
    /// uses [`DETECT_WAIT`]; tests set it explicitly.
    pub detect_wait: Duration,
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
    // Excludes land first: an earlier run's `.shprd/` or `.dagr/` is already on
    // disk, and any check that reads the checkout would otherwise see it as
    // dirty. `preconditions` owns the "not a repository" answer, so the git
    // call that resolves the exclude file waits for a real one.
    if repo.join(".git").exists() {
        ensure_git_excludes(&repo)?;
    }
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
    } else if let Some(starter) = starter_tab(&tabs, created_workspace) {
        // A host makes a starter tab with every workspace. Adopting it keeps
        // the workspace at one chat tab instead of leaving a stray one beside
        // ours. Only a workspace `up` just created is ever retitled.
        mux.rename_tab(&starter.id, CHAT_TITLE)
            .map_err(|err| CliError::other(anyhow!(err).context("renaming the starter tab")))?;
        mux.run(&starter.root_pane, &chat_argv(), &[])
            .map_err(|err| CliError::other(anyhow!(err).context("starting the chat tab")))?;
        (
            Tab {
                title: CHAT_TITLE.to_string(),
                ..starter
            },
            true,
        )
    } else {
        let tab = mux
            .create_tab(&workspace.id, CHAT_TITLE, &repo)
            .map_err(|err| CliError::other(anyhow!(err).context("creating the chat tab")))?;
        mux.run(&tab.root_pane, &chat_argv(), &[])
            .map_err(|err| CliError::other(anyhow!(err).context("starting the chat tab")))?;
        (tab, true)
    };
    let (orchestrator_tab, created_orchestrator_tab) =
        ensure_orchestrator_tab(mux, env, &workspace, &tabs, &brief_path, &run_id)?;
    ensure_watch_pane(mux, env, &chat_tab, &run_id)?;
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
/// answer: nothing else is touched, and no tab is closed. Once the event is in,
/// the reviewer tabs the run still has open are closed with it.
pub fn down(mux: &dyn Mux, ctx: &Ctx, home: Option<&Path>) -> Result<(), CliError> {
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
    spawn::close_run_tabs(mux, ctx, home);
    close_watch_pane(mux, ctx);
    crate::cmd::emit(ctx, &event)
}

/// Closes the watchdog's pane, as the run's last act.
///
/// The watchdog watches a run that is over, so it has nothing left to do. A
/// pane the host no longer knows counts as closed, and a refusal is warned
/// about rather than fatal: the run's last event is already recorded.
pub(crate) fn close_watch_pane(mux: &dyn Mux, ctx: &Ctx) {
    let pane = match ctx.state().ok().and_then(|state| state.watch) {
        Some(watch) => PaneId::new(watch.pane),
        None => return,
    };
    spawn::close_pane(mux, &pane);
}

/// `shprd down` against the host the human is working in.
pub fn down_verb(ctx: &Ctx, home: Option<&Path>) -> Result<(), CliError> {
    down(&spawn::real_mux(), ctx, home)
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

/// Keeps shprd's own state out of `git status`.
///
/// shprd writes `.shprd/` and `.dagr/` inside the repository. Untracked, they
/// make a clean checkout look dirty and block integration, so whichever of the
/// two is missing is appended to the repository's own exclude file. That file
/// is `git rev-parse --git-path info/exclude`, which is where git keeps
/// per-repository ignores and what a worktree resolves to its own git dir —
/// `.gitignore` belongs to the human and is never touched.
fn ensure_git_excludes(repo: &Path) -> Result<(), CliError> {
    let raw = git_stdout(repo, &["rev-parse", "--git-path", "info/exclude"])?;
    let resolved = PathBuf::from(raw.trim());
    let path = if resolved.is_absolute() {
        resolved
    } else {
        repo.join(resolved)
    };
    let existing = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => {
            return Err(CliError::other(
                anyhow!(err).context(format!("reading {}", path.display())),
            ));
        }
    };
    let present: Vec<&str> = existing.lines().map(str::trim).collect();
    let missing: Vec<&str> = EXCLUDES
        .iter()
        .copied()
        .filter(|line| !present.contains(line))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    let mut text = existing.clone();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    if !present.contains(&EXCLUDE_COMMENT) {
        text.push_str(EXCLUDE_COMMENT);
        text.push('\n');
    }
    for line in missing {
        text.push_str(line);
        text.push('\n');
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| CliError::other(anyhow!(err).context("creating the info directory")))?;
    }
    std::fs::write(&path, text)
        .map_err(|err| CliError::other(anyhow!(err).context(format!("writing {}", path.display()))))
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

/// Archives a finished run, reuses the run in flight, or starts a fresh one,
/// and opens its integration worktree either way.
///
/// Returns the run id, whether it was resumed, and its integration branch.
fn begin_run(
    ctx: &Ctx,
    home: &Path,
    repo_name: &str,
    base_branch: &str,
) -> Result<RunBoot, CliError> {
    let mut state = ctx.state()?;
    if state.finished {
        // A finished run's log refuses the next `RunStarted`, so it is archived
        // first and the run after it starts in a fresh, empty log.
        archive_finished_run(ctx, &state)?;
        state = ctx.state()?;
    }
    let boot = match resume_target(&state) {
        Some(run) => RunBoot {
            run_id: run.run_id,
            resumed: true,
            base_branch: run.base_branch,
            integration_branch: run.integration_branch,
        },
        None => {
            let run_id = new_run_id();
            let integration_branch = integration_branch(&run_id);
            ctx.commit(EventKind::RunStarted {
                run_id: run_id.clone(),
                repo: ctx.root.to_string_lossy().to_string(),
                base_branch: base_branch.to_string(),
                integration_branch: integration_branch.clone(),
            })?;
            RunBoot {
                run_id,
                resumed: false,
                base_branch: base_branch.to_string(),
                integration_branch,
            }
        }
    };
    // Opening the worktree is not part of starting the run: it happens on every
    // `up`, so a log that says the run started while the worktree never landed
    // is healed by the retry instead of being resumed as if it were whole.
    ensure_integration(ctx, home, repo_name, &boot, &state)?;
    Ok(boot)
}

/// Moves a finished run's log aside so `up` can start the next run.
///
/// A repository keeps one live log, `.shprd/events.jsonl`, and a log whose run
/// has finished refuses the next `RunStarted`. The old log is renamed under
/// `.shprd/runs/` while its own lock is held, so no other shprd process is
/// appending to it as it moves, and an archive already there is never
/// overwritten: the name grows a `-2`, `-3`... suffix instead.
///
/// Returns where the log went, or `None` when the run is not finished.
fn archive_finished_run(ctx: &Ctx, state: &RunState) -> Result<Option<PathBuf>, CliError> {
    let Some(run) = state.run.as_ref().filter(|_| state.finished) else {
        return Ok(None);
    };
    let parent = ctx.log_path.parent().ok_or_else(|| {
        CliError::other(anyhow!(
            "no directory to archive into: {}",
            ctx.log_path.display()
        ))
    })?;
    let dir = parent.join(RUNS_DIR);
    std::fs::create_dir_all(&dir)
        .map_err(|err| CliError::other(anyhow!(err).context("creating the run archive")))?;
    let target = archive_target(&dir, &run.run_id);
    let file = OpenOptions::new()
        .read(true)
        .append(true)
        .open(&ctx.log_path)
        .map_err(|err| CliError::other(anyhow!(err).context("opening the event log")))?;
    file.lock()
        .map_err(|err| CliError::other(anyhow!(err).context("locking the event log")))?;
    let moved = std::fs::rename(&ctx.log_path, &target)
        .map_err(|err| CliError::other(anyhow!(err).context("archiving the event log")));
    let unlocked = file
        .unlock()
        .map_err(|err| CliError::other(anyhow!(err).context("unlocking the event log")));
    moved?;
    unlocked?;
    let shown = target.strip_prefix(&ctx.root).unwrap_or(&target);
    println!(
        "archived finished run {} to {}",
        run.run_id,
        shown.display()
    );
    Ok(Some(target))
}

/// Where `run_id`'s log is archived: `<dir>/<run_id>.jsonl`, or the first free
/// `-2`, `-3`... suffix when that name is taken.
fn archive_target(dir: &Path, run_id: &str) -> PathBuf {
    let first = dir.join(format!("{run_id}.jsonl"));
    if !first.exists() {
        return first;
    }
    (2..)
        .map(|n| dir.join(format!("{run_id}-{n}.jsonl")))
        .find(|path| !path.exists())
        .unwrap_or(first)
}

/// Opens the run's integration worktree and records the resources it owns.
///
/// [`integrate::open_integration`] creates the worktree when it is missing and
/// reuses it when it is there, so this is safe to call on every `up`.
fn ensure_integration(
    ctx: &Ctx,
    home: &Path,
    repo_name: &str,
    boot: &RunBoot,
    state: &RunState,
) -> Result<(), CliError> {
    let layout = Layout::default_for_home(home);
    let worktree = integrate::open_integration(
        &ctx.root,
        &layout,
        repo_name,
        &boot.run_id,
        &boot.base_branch,
    )
    .map_err(|err| CliError::other(anyhow!(err).context("opening the integration worktree")))?;
    record_integration(ctx, &layout, repo_name, &boot.run_id, &worktree, state)
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

/// The layout segment that names the integration worktree's directory.
const INTEGRATION_PATH_TASK: &str = "_int";

/// The layout segment that names the integration branch.
const INTEGRATION_BRANCH_TASK: &str = "int";

/// Appends the resources the run's integration worktree owns.
///
/// A resource is recorded when the ledger does not already hold it live *and*
/// the worktree is provably this run's own: its path is the layout's
/// `<root>/<repo>/<run>/_int` and its branch `<namespace><run>/int`. Whether
/// this call created it does not matter — a crash between creating the
/// worktree and appending its events leaves it unrecorded, and adoption is what
/// heals that and what licenses the janitor to remove it. Anything that does
/// not match the layout belongs to a human and is left alone.
fn record_integration(
    ctx: &Ctx,
    layout: &Layout,
    repo_name: &str,
    run_id: &str,
    worktree: &IntegrationWorktree,
    state: &RunState,
) -> Result<(), CliError> {
    let ours = worktree.path == layout.path(repo_name, run_id, INTEGRATION_PATH_TASK)
        && worktree.branch == layout.branch_name(run_id, INTEGRATION_BRANCH_TASK);
    if !ours {
        return Ok(());
    }
    for (kind, name) in [
        (
            ResourceKind::Worktree,
            worktree.path.to_string_lossy().to_string(),
        ),
        (ResourceKind::Branch, worktree.branch.clone()),
    ] {
        let live = state
            .resources
            .get(&(kind, name.clone()))
            .is_some_and(|record| record.status == ResourceStatus::Live);
        if live {
            continue;
        }
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
        ready_wait: super::launch::ready_wait(),
        detect_wait: DETECT_WAIT,
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

/// The starter tab a host made with a workspace `up` just created, if any.
///
/// The tab is only ever adopted from a freshly created workspace, and only when
/// it is the sole tab there: in a workspace `up` did not create, a lone tab
/// belongs to a human and is left exactly as it is.
fn starter_tab(tabs: &[Tab], created_workspace: bool) -> Option<Tab> {
    match tabs {
        [only] if created_workspace => Some(only.clone()),
        _ => None,
    }
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

/// Opens the orchestrator's tab unless it is already there and running.
///
/// An existing tab with an agent in it is never recreated and never
/// re-prompted: a human may be mid-conversation in it, and a second start
/// would replace the session they are typing into. A tab without an agent is
/// the other case, and the one a herdr restart leaves behind: the workspace
/// and its tabs come back, herdr's `--resume` fails, and the pane holds a bare
/// shell. That pane gets the orchestrator started in it, so a resumed run is
/// never left without one.
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
        if matches!(pane_fate(mux, &found.root_pane), PaneFate::Gone) {
            // The tab outlived its pane: a host that lost the pane but kept the
            // tab leaves nothing to start a program in and nothing to split
            // off. Reopening the tab under the same title puts an orchestrator
            // back in the run, which is what the pane being gone needs.
            let (tab, created) =
                reopen_orchestrator_tab(mux, env, workspace, &found, &plan, brief_path)?;
            eprintln!(
                "relaunched the orchestrator in {}: its pane was gone",
                plan.title
            );
            return Ok((tab, created));
        }
        if started_elsewhere(mux, env, &found.root_pane) {
            // The pane's process is not the one shprd started: a host that
            // restored the tab with its own `--resume`, with none of the
            // environment, model or auto-approve flag shprd launches with, and
            // whose every action would be logged as a human's. Nothing typed
            // into that pane reaches a shell — `run` types a shell line, which
            // a live agent would read as a prompt — so a fresh pane is split
            // off it and the orchestrator started there instead.
            let (tab, created) =
                replace_orchestrator_pane(mux, env, workspace, &found, &plan, brief_path)?;
            eprintln!(
                "relaunched the orchestrator in {}: its process was not started by shprd",
                plan.title
            );
            return Ok((tab, created));
        }
        if orchestrator_is_running(mux, &found.root_pane, env.detect_wait) {
            return Ok((found, false));
        }
        // The tab outlived the agent: the run has no orchestrator at all, so
        // one is started where the old one was instead of in a second tab.
        launch_orchestrator(mux, &found.root_pane, &plan, env, brief_path)?;
        eprintln!("relaunched the orchestrator in {}", plan.title);
        return Ok((found, false));
    }
    let tab = mux
        .create_tab(&workspace.id, &plan.title, &env.ctx.root)
        .map_err(|err| CliError::other(anyhow!(err).context("creating the orchestrator tab")))?;
    launch_orchestrator(mux, &tab.root_pane, &plan, env, brief_path)?;
    Ok((tab, true))
}

/// Puts the orchestrator back into a tab whose process is not shprd's.
///
/// The tab keeps its place in the tab bar: closing and reopening it would move
/// it to the end, and `up` promises the orchestrator sits right after the chat
/// tab. So a fresh pane is split off the foreign one, the orchestrator starts
/// there, and only then is the foreign pane closed — closing it first would
/// leave the tab empty, and a host closes a tab with nothing left in it.
///
/// The route is chosen before anything is split. A host that cannot close a
/// pane ([`shprd_mux::Capabilities::close_pane`]) would end up with two
/// processes in the tab, so it takes the older route instead: close the whole
/// tab and reopen it. A host that promised the capability and refused it at
/// runtime gets the same fallback, and if even that fails `up` fails rather
/// than report a tab that still runs a process shprd did not start.
fn replace_orchestrator_pane(
    mux: &dyn Mux,
    env: &UpEnv,
    workspace: &Workspace,
    found: &Tab,
    plan: &OrchestratorPlan,
    brief_path: &Path,
) -> Result<(Tab, bool), CliError> {
    if !mux.capabilities().close_pane {
        return reopen_orchestrator_tab(mux, env, workspace, found, plan, brief_path);
    }
    let fresh = match mux.split(&found.root_pane, Direction::Down, 0.5, &env.ctx.root) {
        Ok(pane) => pane,
        // [`MuxError::Unsupported`] is the trait's "this host cannot split",
        // the one capability signal a host has for it.
        Err(MuxError::Unsupported(_)) => {
            return reopen_orchestrator_tab(mux, env, workspace, found, plan, brief_path);
        }
        Err(err) => {
            return Err(CliError::other(
                anyhow!(err).context("splitting the orchestrator tab"),
            ));
        }
    };
    launch_orchestrator(mux, &fresh, plan, env, brief_path)?;
    match mux.close_pane(&found.root_pane) {
        // A pane the host no longer knows is already gone.
        Ok(()) | Err(MuxError::NotFound { .. }) => Ok((
            Tab {
                root_pane: fresh,
                ..found.clone()
            },
            false,
        )),
        Err(err) => {
            // The foreign process is still running beside the new orchestrator.
            // The only thing left that is certain to take it down is closing
            // the whole tab, so the tab loses its place rather than keeping a
            // second, unlabelled agent alive.
            eprintln!(
                "warning: could not close the orchestrator's old pane {}: {err}; \
                 reopening the tab instead",
                found.root_pane
            );
            reopen_orchestrator_tab(mux, env, workspace, found, plan, brief_path).map_err(
                |fallback| {
                    CliError::other(anyhow!(
                        "could not close the foreign orchestrator pane {} ({err}) and could not \
                         reopen its tab either ({fallback}); that pane still runs a process \
                         shprd did not start, so close it by hand",
                        found.root_pane
                    ))
                },
            )
        }
    }
}

/// Closes the orchestrator's tab and reopens it under the same title.
///
/// The fallback for a host that cannot split or cannot close a single pane.
/// The reopened tab is new to the host and lands at the end of the tab bar,
/// which is why it is not the first choice.
fn reopen_orchestrator_tab(
    mux: &dyn Mux,
    env: &UpEnv,
    workspace: &Workspace,
    found: &Tab,
    plan: &OrchestratorPlan,
    brief_path: &Path,
) -> Result<(Tab, bool), CliError> {
    match mux.close_tab(&found.id) {
        // A tab the host already dropped is the state this wanted.
        Ok(()) | Err(MuxError::NotFound { .. }) => {}
        Err(err) => {
            return Err(CliError::other(
                anyhow!(err).context("closing the foreign orchestrator tab"),
            ));
        }
    }
    let tab = mux
        .create_tab(&workspace.id, &plan.title, &env.ctx.root)
        .map_err(|err| CliError::other(anyhow!(err).context("reopening the orchestrator tab")))?;
    launch_orchestrator(mux, &tab.root_pane, plan, env, brief_path)?;
    Ok((tab, true))
}

/// Starts the orchestrator in `pane` and hands it the brief.
///
/// A fresh tab and a relaunched one go through here, so both start the same
/// program with the same argv, environment and brief for the current run.
fn launch_orchestrator(
    mux: &dyn Mux,
    pane: &PaneId,
    plan: &OrchestratorPlan,
    env: &UpEnv,
    brief_path: &Path,
) -> Result<(), CliError> {
    mux.run(pane, &plan.argv, &plan.env)
        .map_err(|err| CliError::other(anyhow!(err).context("starting the orchestrator")))?;
    if let Some(brief) = &plan.inline_prompt
        && let Some(reason) =
            super::launch::deliver_brief(mux, pane, brief, &plan.blocking_prompts, env.ready_wait)
                .reason()
    {
        // The orchestrator is running either way, so a brief that never lands
        // is a message to the human, not a failed `up`: they can paste it in.
        eprintln!(
            "warning: brief not delivered to {}: {reason}; resend the brief from: {}",
            plan.title,
            brief_path.display()
        );
    }
    // Recorded last, once the brief has had its wait: by then the pane's
    // foreground group is the agent's, not the shell's the line was typed at.
    record_process_group(mux, env, &plan.title, pane);
    Ok(())
}

/// Records the process group shprd launched the orchestrator under.
///
/// A host that cannot tell, or a pane that is already gone, records nothing:
/// there is no identity to compare against later, and pretending there is one
/// would make the next `up` relaunch a session shprd did start.
fn record_process_group(mux: &dyn Mux, env: &UpEnv, title: &str, pane: &PaneId) {
    let group = match mux.pane_process_group(pane) {
        Ok(Some(group)) => group,
        Ok(None) | Err(_) => return,
    };
    let kind = EventKind::OrchestratorLaunched {
        title: title.to_string(),
        pane: pane.as_str().to_string(),
        process_group: group,
    };
    if let Err(err) = env.ctx.commit(kind) {
        // The orchestrator is running either way; a missing record only means
        // the next `up` cannot recognise it, which is a message to the human.
        eprintln!("warning: could not record the orchestrator's process group: {err}");
    }
}

/// Whether the process in `pane` is not the one shprd started for this run.
///
/// Only a group the host actually reported can make a pane foreign. `None`
/// means the host cannot tell, which is not evidence against the tab, and a
/// pane the host has forgotten is handled before this: there is no process in
/// it to be a stranger's, and the tab is reopened rather than split.
fn started_elsewhere(mux: &dyn Mux, env: &UpEnv, pane: &PaneId) -> bool {
    let PaneFate::Group(observed) = pane_fate(mux, pane) else {
        return false;
    };
    let recorded = env
        .ctx
        .state()
        .ok()
        .and_then(|state| state.orchestrator)
        .map(|orchestrator| orchestrator.process_group);
    recorded != Some(observed)
}

/// Whether an orchestrator is still running in `pane`, polling for up to
/// `wait`.
///
/// Only an answer that is not [`AgentState::Unknown`] counts as running, and
/// [`DETECT_POLL`] spaces the polls so a CLI that is merely slow to be detected
/// is not mistaken for an absent one. Everything else — a detected agent, or a
/// host that cannot tell — leaves the tab alone: replacing a session a human
/// may be typing into is worse than a stale tab.
fn orchestrator_is_running(mux: &dyn Mux, pane: &PaneId, wait: Duration) -> bool {
    let start = Instant::now();
    loop {
        match mux.agent_state(pane) {
            Ok(AgentState::Unknown) => {}
            Ok(_) | Err(_) => return true,
        }
        let waited = start.elapsed();
        if waited >= wait {
            return false;
        }
        std::thread::sleep(DETECT_POLL.min(wait - waited));
    }
}

/// Starts the watchdog in the chat tab, unless it is already running there.
///
/// `up` owns the watchdog the way it owns the orchestrator: `shprd watch` is
/// what turns a reviewer's findings into a verdict and hands off a dry worker,
/// and a run whose watchdog is not running cannot settle a review at all. So the
/// pane is split off the human's chat tab, never off an agent's tab, and it is
/// only ever started when the log has no live watchdog of this run's.
///
/// A host that cannot split is not a failed `up`: the run works without a
/// watchdog, so the human is told to start one and `up` carries on.
///
/// A replacement is only ever started where the old pane can be taken down. Two
/// watchdogs in one chat tab would both record verdicts and both hand off
/// workers, which is worse than a run with none, so a host that cannot close a
/// pane is told so instead of being split twice.
fn ensure_watch_pane(mux: &dyn Mux, env: &UpEnv, chat: &Tab, run_id: &str) -> Result<(), CliError> {
    let stale = match watch_pane_state(mux, env) {
        WatchPane::Running => return Ok(()),
        // Nothing shprd can point at, so there is no pane to take down.
        WatchPane::Missing => None,
        WatchPane::Foreign(pane) => Some(pane),
    };
    if let Some(stale) = &stale
        && !mux.capabilities().close_pane
    {
        // The pane stays whether or not a second watchdog starts in it, so no
        // second watchdog starts.
        eprintln!("{}", not_shprds_watch_pane(stale));
        return Ok(());
    }
    let pane = match mux.split(&chat.root_pane, Direction::Down, WATCH_RATIO, &env.ctx.root) {
        Ok(pane) => pane,
        // [`MuxError::Unsupported`] is the trait's "this host cannot split".
        Err(MuxError::Unsupported(_)) => {
            no_watch_pane();
            return Ok(());
        }
        Err(err) => {
            return Err(CliError::other(
                anyhow!(err).context("splitting the watchdog pane"),
            ));
        }
    };
    if let Err(err) = mux.run(&pane, &watch_argv(), &watch_env(env, run_id)) {
        // The pane is there but holds nothing shprd started: the same advice as
        // a host without splits, and the run still works.
        eprintln!("warning: could not start the watchdog: {err}");
        no_watch_pane();
        // The pane is empty of anything shprd started, so it goes rather than
        // sitting in the human's tab.
        if mux.capabilities().close_pane {
            spawn::close_pane(mux, &pane);
        }
        return Ok(());
    }
    // The stale pane is closed only now, once a new watchdog is running, so the
    // chat tab never holds a run with no watchdog. A pane the host no longer
    // knows is already gone, and a host that cannot close a single pane keeps
    // the stale one: closing the whole chat tab is not an option, because the
    // human is working in it.
    if let Some(stale) = stale
        && stale != pane
    {
        match mux.close_pane(&stale) {
            // A pane the host forgot is already closed.
            Ok(()) | Err(MuxError::NotFound { .. }) => {}
            Err(err) => {
                // Two watchdogs is the one outcome `up` must not leave behind,
                // so the replacement goes rather than the pane it could not
                // retire. The record stays on the old pane, so the next `up`
                // sees the same obstacle and says so again instead of splitting
                // a third watchdog beside it.
                eprintln!("warning: could not close the watchdog pane {stale}: {err}");
                if spawn::close_pane(mux, &pane) {
                    eprintln!("{}", not_shprds_watch_pane(&stale));
                    return Ok(());
                }
                // Neither pane can be taken down, so both watchdogs are
                // running. The replacement is the one `up` can still point at,
                // and the record follows it: left on the stale pane, every
                // later `up` would split a third. The run's watch lock is what
                // keeps only one of the two doing any work.
                eprintln!("{}", two_watch_panes(&stale, &pane));
                record_watch(mux, env, &pane);
                return Ok(());
            }
        }
    }
    record_watch(mux, env, &pane);
    Ok(())
}

/// The advice a human gets when `up` could not leave a watchdog running.
fn no_watch_pane() {
    eprintln!("{NO_WATCH_ADVICE}");
}

/// The line `up` prints when a watchdog pane stands in the way and the host
/// cannot take it down.
fn not_shprds_watch_pane(pane: &PaneId) -> String {
    format!(
        "the watchdog pane {} is not shprd's and this host cannot close it: close it, then run \
         shprd watch",
        pane.as_str()
    )
}

/// The line `up` prints when it could retire neither watchdog pane, and both
/// are running.
fn two_watch_panes(stale: &PaneId, replacement: &PaneId) -> String {
    format!(
        "the watchdog panes {} and {} cannot be closed: two watchdogs are running, and only one \
         of them does any work — close the pane you do not want, then run shprd watch",
        stale.as_str(),
        replacement.as_str()
    )
}

/// The line `up` prints when it could not start the watchdog itself.
const NO_WATCH_ADVICE: &str = "start the watchdog yourself: shprd watch";

/// What `up` found where the log says the watchdog's pane was.
#[derive(Debug)]
enum WatchPane {
    /// Nothing shprd can point at: no record, or a pane the host has forgotten.
    /// The watchdog is not running, and there is no pane to take down.
    Missing,
    /// The recorded pane still holds the watchdog shprd started in it.
    Running,
    /// The recorded pane is there, but its process is not the one shprd
    /// started: a replacement means closing this pane, not just splitting.
    Foreign(PaneId),
}

/// Whether the watchdog the log recorded is still shprd's.
///
/// A pane the log never recorded, or one the host no longer knows, is a
/// watchdog that is not running: the pane is gone, so its process is, and `up`
/// replaces it. A *live* pane whose host cannot report a process group is the
/// other answer, and no evidence against the pane, so it stays: splitting a
/// second watchdog off the chat tab on every `up` would pile panes up in the
/// human's tab.
fn watch_pane_state(mux: &dyn Mux, env: &UpEnv) -> WatchPane {
    let Some(recorded) = env.ctx.state().ok().and_then(|state| state.watch) else {
        return WatchPane::Missing;
    };
    let pane = PaneId::new(recorded.pane);
    match pane_fate(mux, &pane) {
        PaneFate::Gone => WatchPane::Missing,
        PaneFate::Unknown => WatchPane::Running,
        PaneFate::Group(group) => match recorded.process_group {
            Some(own) if own != group => WatchPane::Foreign(pane),
            _ => WatchPane::Running,
        },
    }
}

/// What the host says about a pane `up` asked about.
///
/// The last two answers are the ones `up` must not confuse: a pane the host has
/// forgotten has no process left to lead a watchdog, while a live pane with no
/// group to report is a host that cannot tell, which is never evidence against
/// the pane.
#[derive(Debug)]
enum PaneFate {
    /// The host has no pane under that id: its process is gone.
    Gone,
    /// The pane is there, and the host reported its foreground process group.
    Group(u32),
    /// The pane is there, but the host cannot say what runs in it.
    Unknown,
}

/// Asks the host which process is in `pane`.
fn pane_fate(mux: &dyn Mux, pane: &PaneId) -> PaneFate {
    match mux.pane_process_group(pane) {
        Ok(Some(group)) => PaneFate::Group(group),
        Ok(None) => PaneFate::Unknown,
        Err(MuxError::NotFound { .. }) => PaneFate::Gone,
        // Any other failure is a host that could not answer, which says nothing
        // about whether the pane is still there.
        Err(_) => PaneFate::Unknown,
    }
}

/// Records the pane shprd started the watchdog in.
///
/// The process group is recorded only when the host can report one, and a
/// missing group is recorded as `None` rather than as no record at all: the
/// pane is what a later `up` checks, and the group only refines the answer.
fn record_watch(mux: &dyn Mux, env: &UpEnv, pane: &PaneId) {
    let group = match mux.pane_process_group(pane) {
        Ok(Some(group)) => Some(group),
        Ok(None) | Err(_) => None,
    };
    let kind = EventKind::WatchLaunched {
        pane: pane.as_str().to_string(),
        process_group: group,
    };
    if let Err(err) = env.ctx.commit(kind) {
        // The watchdog is running either way; a missing record only means the
        // next `up` cannot recognise it, which is a message to the human.
        eprintln!("warning: could not record the watchdog's pane: {err}");
    }
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
    /// The prompts the CLI waits on a human to answer before it reads anything.
    blocking_prompts: Vec<BlockingPrompt>,
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
        // The flags sit where `spawn` puts a worker's: right after the base
        // command, before the model, the effort and the brief.
        if env.config.roster.orchestrator_auto_approve
            && let Some(auto) = &spec.launch.auto_approve
        {
            argv.extend(auto.iter().cloned());
        }
        argv.extend(render(
            super::launch::model_args(&spec.launch, agent.model.as_deref()),
            &values,
            "model_args",
        )?);
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
            blocking_prompts: spec.blocking_prompts.clone(),
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

/// The `## How a run works` section of the brief: the order a run actually
/// happens in, from planning a task to landing the pull request.
const FLOW: &str = r#"## How a run works

1. **You never write code, never push, never merge.** Workers write code; the
   engine pushes; the human approves.

2. **Plan.** Split the human's ask into tasks whose scopes do not overlap:
   `shprd agent task add --id <ID> --title "…" --scope <path>… \
   --acceptance "<testable criterion>"… [--tag feature|bug|ui|docs|perf] \
   [--dep <ID>]`.

3. **Start each task.** `shprd agent spawn --task <ID>`: shprd picks the
   worker, the worktree and the tab. Independent tasks can run at the same
   time.

4. **Follow up: the watchdog messages you.** Workers report themselves, and
   `shprd up` already runs the watchdog (`shprd watch`) in a pane of the chat
   tab, so do not start another one. That watchdog prompts you when a task
   reports, when a review decides, and when something needs the human: act on
   those messages. Check `shprd status` and `shprd brief` when in doubt, and
   send a worker a message with `shprd agent prompt --task <ID> "…"`. The
   engine records verdicts, so never record a review verdict yourself. A stuck
   review is step 5's job: restart it first, and escalate only if the restart
   is stuck too.

5. **Review.** When a task is reported: `shprd agent review --task <ID>`. The
   reviewer writes its findings and the engine records the verdict; code
   decides. If the review fails, start the next attempt with
   `shprd agent spawn --task <ID>`: the new brief carries the findings. After
   repeated failures shprd escalates to the human. If a review is stuck or its
   brief never arrived, restart it with
   `shprd agent review --task <ID> --again`; escalate only if that restart is
   stuck too, and never type into the reviewer's tab yourself.

6. **Integrate.** When a task passed review: `shprd agent integrate --task <ID>`.
   A conflict sends it back like a failed review.

7. **Land.** When every task is integrated: `shprd gate` (the repository's
   full checks), then `shprd land`, and tell the human the pull request URL.
   Only after the human approves: `shprd land --merge`, then
   `shprd janitor --apply`, then `shprd down`.

8. **Escalate.** When a human must decide: `shprd agent escalate [--task <ID>]
   --body "…" --default "what you will do if nobody answers"`.
"#;
/// The first rule a brief carries when the orchestrator runs with
/// auto-approve: with no prompt left, the pull request is the gate.
const AUTO_APPROVE_RULE: &str = "You run without permission prompts. The pull \
request is the human's gate: never run git push, gh pr merge, gh pr review, or \
any command that merges or approves; only shprd land and shprd land --merge, \
and land --merge only after the human says the PR is approved.";

/// The brief's text: who you are, where the branches are, how a run flows,
/// and which skills to load.
fn brief_text(env: &UpEnv, run_id: &str, base_branch: &str) -> String {
    let skills = if env.config.skills.orchestrator.is_empty() {
        "none configured".to_string()
    } else {
        env.config.skills.orchestrator.join(", ")
    };
    let repo = env.ctx.root.display();
    let rules = if env.config.roster.orchestrator_auto_approve {
        format!("## Rules\n\n1. {AUTO_APPROVE_RULE}\n\n")
    } else {
        String::new()
    };
    format!(
        "# Orchestrator brief\n\n\
You are the orchestrator for run `{run_id}` in `{repo}`.\n\n\
## Branches\n\n\
- base: `{base}`\n\
- integration: `{integration}`\n\n\
{rules}{flow}\n## Skills\n\n\
Configured for you: {skills}\n\n\
Load them before planning; they carry the conventions this repository runs on.\n",
        base = base_branch,
        integration = integration_branch(run_id),
        flow = FLOW,
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
        /// Whether the orchestrator launches with auto-approve.
        auto_approve: bool,
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
                auto_approve: false,
            })
        }

        /// Turns auto-approve on for this fixture's orchestrator.
        fn auto_approve(mut self) -> Self {
            self.auto_approve = true;
            self
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
                    orchestrator_auto_approve: self.auto_approve,
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
                ready_wait: Duration::ZERO,
                detect_wait: Duration::ZERO,
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

    /// Runs one git command in `dir` and returns its stdout.
    fn git_text(dir: &StdPath, args: &[&str]) -> Result<String> {
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
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// `git status --porcelain` in `dir`, so a test sees the checkout as a
    /// human would.
    fn status(dir: &StdPath) -> Result<String> {
        git_text(dir, &["status", "--porcelain"])
    }

    /// Where git keeps the repo-local exclude file, resolved the way git
    /// resolves it so a worktree's exclude is the one found.
    fn exclude_path(dir: &StdPath) -> Result<PathBuf> {
        let text = git_text(dir, &["rev-parse", "--git-path", "info/exclude"])?;
        let path = PathBuf::from(text.trim());
        Ok(if path.is_absolute() {
            path
        } else {
            dir.join(path)
        })
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

    /// The pane a tab owns, as the fake host lists it.
    fn pane_of(mux: &FakeMux, tab: &TabId) -> Result<PaneId> {
        let listed = mux
            .list_tabs(&shprd_mux::WorkspaceId::new("ws-1"))
            .map_err(|err| anyhow!("{err}"))?;
        let found = listed
            .into_iter()
            .find(|candidate| &candidate.id == tab)
            .context("the tab exists")?;
        Ok(found.root_pane)
    }

    /// Where the run's integration worktree belongs on disk.
    fn integration_worktree(fixture: &Fixture, run_id: &str) -> PathBuf {
        fixture
            .home
            .path()
            .join(".shprd")
            .join("wt")
            .join("repo")
            .join(run_id)
            .join("_int")
    }

    /// A fake host that also makes the starter tab a real host makes with
    /// every new workspace, as herdr does (titled with the tab's number).
    fn mux_with_starter_tab() -> FakeMux {
        mux().with_starter_tab()
    }

    #[test]
    fn first_up_adopts_the_starter_tab_as_the_chat_tab() -> Result<()> {
        let fixture = Fixture::new(Some("claude/claude-opus-5-5"))?;
        let env = fixture.env()?;
        let mux = mux_with_starter_tab();
        let report = up(&mux, &env).map_err(failed)?;

        assert!(report.created_workspace);
        assert!(
            report.created_chat_tab,
            "the starter tab became the chat tab"
        );
        // The starter tab is retitled and the chat placeholder runs in its pane,
        // so no second tab is created for the human.
        assert_eq!(
            created_titles(&mux),
            vec!["★ claude·orchestrator".to_string()]
        );
        let renamed = mux.calls().into_iter().find_map(|call| match call {
            Call::RenameTab(tab, title) => Some((tab, title)),
            _ => None,
        });
        let (tab, title) = renamed.context("the starter tab was renamed")?;
        assert_eq!(title, "shprd·chat");
        assert_eq!(tab, report.chat_tab);
        assert_eq!(started_argvs(&mux)[0], chat_argv());

        // The host agrees: one chat tab and one orchestrator tab, nothing else.
        let listed = mux
            .list_tabs(&shprd_mux::WorkspaceId::new("ws-1"))
            .map_err(|err| anyhow!("{err}"))?;
        let titles: Vec<&str> = listed.iter().map(|tab| tab.title.as_str()).collect();
        assert_eq!(titles, ["shprd·chat", "★ claude·orchestrator"]);

        // A second `up` finds the adopted tab by title and leaves it alone.
        let second = up(&mux, &env).map_err(failed)?;
        assert!(!second.created_chat_tab);
        assert_eq!(second.chat_tab, report.chat_tab);
        assert_eq!(
            mux.calls()
                .into_iter()
                .filter(|call| matches!(call, Call::RenameTab(..)))
                .count(),
            1,
            "a tab up did not create is never retitled"
        );
        Ok(())
    }

    /// A fake host that claims every capability.
    ///
    /// Its panes report an agent the host has already detected, which is the
    /// steady state a second `up` meets. A test about detection scripts the
    /// pane's state itself, or takes [`mux_without_agent`].
    fn mux() -> FakeMux {
        mux_without_agent().with_ready_agent()
    }

    /// A fake host whose unscripted panes never report an agent, which is what
    /// a pane a failed `--resume` left behind looks like.
    fn mux_without_agent() -> FakeMux {
        FakeMux::new(Capabilities {
            split_ratio: true,
            pane_grants: true,
            close_pane: true,
            human_ask: true,
            resumable_events: true,
            agent_state: true,
        })
    }

    #[test]
    fn a_model_less_orchestrator_starts_without_model_args() -> Result<()> {
        let fixture = Fixture::new(Some("claude"))?;
        let env = fixture.env()?;
        let mux = mux();
        let report = up(&mux, &env).map_err(failed)?;

        let argvs = started_argvs(&mux);
        assert_eq!(argvs.len(), 3, "chat, orchestrator, watchdog: {argvs:?}");
        assert_eq!(argvs[2], watch_argv());
        assert_eq!(
            argvs[1],
            vec![
                "claude".to_string(),
                "--effort".to_string(),
                "high".to_string(),
                "--append-system-prompt-file".to_string(),
                report.brief_path.display().to_string(),
            ],
            "an orchestrator with no model gets no model flags"
        );
        Ok(())
    }

    #[test]
    fn auto_approve_puts_the_flag_before_the_model_and_the_brief() -> Result<()> {
        let fixture = Fixture::new(Some("claude"))?.auto_approve();
        let env = fixture.env()?;
        let mux = mux();
        let report = up(&mux, &env).map_err(failed)?;

        let argvs = started_argvs(&mux);
        assert_eq!(
            argvs[1][..2],
            [
                "claude".to_string(),
                "--dangerously-skip-permissions".to_string()
            ],
            "the flag sits right after the command: {argvs:?}"
        );
        assert!(
            argvs[1].contains(&report.brief_path.display().to_string()),
            "the brief still goes in as an argument: {argvs:?}"
        );
        Ok(())
    }

    #[test]
    fn without_auto_approve_the_orchestrator_keeps_its_prompts() -> Result<()> {
        let fixture = Fixture::new(Some("claude"))?;
        let env = fixture.env()?;
        let mux = mux();
        up(&mux, &env).map_err(failed)?;

        let argvs = started_argvs(&mux);
        assert!(
            !argvs[1]
                .iter()
                .any(|arg| arg == "--dangerously-skip-permissions"),
            "no flag unless the config asks for one: {argvs:?}"
        );
        Ok(())
    }

    #[test]
    fn the_brief_states_the_pr_gate_when_auto_approve_is_on() -> Result<()> {
        let plain = Fixture::new(Some("claude"))?;
        let plain_env = plain.env()?;
        let plain_mux = mux();
        let report = up(&plain_mux, &plain_env).map_err(failed)?;
        let text = std::fs::read_to_string(&report.brief_path)?;
        assert!(
            !text.contains("You run without permission prompts"),
            "{text}"
        );

        let auto = Fixture::new(Some("claude"))?.auto_approve();
        let auto_env = auto.env()?;
        let auto_mux = mux();
        let report = up(&auto_mux, &auto_env).map_err(failed)?;
        let text = std::fs::read_to_string(&report.brief_path)?;
        assert!(
            text.contains("1. You run without permission prompts."),
            "the extra rule leads the brief: {text}"
        );
        Ok(())
    }

    #[test]
    fn an_orchestrator_never_detected_still_leaves_up_green() -> Result<()> {
        let fixture = Fixture::new(Some("claude"))?;
        let mut env = fixture.env()?;
        // A CLI with no system-prompt flag takes the brief as its first turn,
        // and this host never reports the pane as an agent: that is what a
        // plain `sh`, OpenCode or Kilo looks like until detection lands.
        for spec in &mut env.specs {
            spec.launch.system_prompt_args = None;
        }
        let mux = mux_without_agent();
        let report = up(&mux, &env).map_err(failed)?;

        assert!(
            report.created_orchestrator_tab,
            "up must still report success"
        );
        assert!(
            mux.calls()
                .iter()
                .all(|call| !matches!(call, Call::Prompt(..))),
            "nothing may be typed into an undetected pane"
        );
        // The human's resend path needs the brief on disk.
        assert!(
            report.brief_path.exists(),
            "{}",
            report.brief_path.display()
        );
        Ok(())
    }

    #[test]
    fn the_chat_placeholder_runs_this_binary() {
        let argv = chat_argv();
        let exe = match std::env::current_exe() {
            Ok(exe) => exe,
            Err(err) => panic!("the running binary has a path: {err}"),
        };
        assert_eq!(argv[0], "sh");
        assert_eq!(argv[1], "-c");
        let script = &argv[2];
        assert_eq!(
            script,
            &format!(
                "while :; do clear; '{}' status; printf '\\n  shprd·chat arrives in M7 · talk to the ★ orchestrator tab · refreshes every 5 s\\n'; sleep 5; done",
                exe.display()
            )
        );
        assert!(script.starts_with("while :; do clear; "), "{script}");
        assert!(script.contains(" status; "), "{script}");
        assert!(script.ends_with("; sleep 5; done"), "{script}");
        assert!(
            !script.contains("SHELL"),
            "the placeholder must not hand over to the user's login shell: {script}"
        );
        assert!(
            exe.is_absolute(),
            "{exe:?} is not an absolute path, so the tab would fall back to PATH"
        );
    }

    #[test]
    fn a_path_with_quotes_survives_as_one_shell_word() {
        assert_eq!(
            posix_quote(StdPath::new("/tmp/it's here/shprd")),
            "'/tmp/it'\\''s here/shprd'"
        );
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

        // The chat placeholder, the orchestrator CLI with the brief in its
        // system prompt and the run in its environment, then the watchdog.
        let argvs = started_argvs(&mux);
        assert_eq!(argvs.len(), 3, "chat, orchestrator, watchdog: {argvs:?}");
        assert_eq!(argvs[2], watch_argv());
        assert_eq!(argvs[0], chat_argv());
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
        assert!(brief.contains("## How a run works"), "{brief}");
        assert!(brief.contains("shprd agent review"), "{brief}");
        assert!(brief.contains("shprd land --merge"), "{brief}");
        assert!(
            brief.contains("`shprd up` already runs the watchdog"),
            "the brief says up owns the watchdog: {brief}"
        );
        assert!(
            brief.contains("never record a review verdict yourself"),
            "the brief forbids hand-recorded verdicts: {brief}"
        );
        assert!(
            brief.contains("`shprd agent review --task <ID> --again`"),
            "the brief says how to restart a stuck review: {brief}"
        );
        assert!(
            brief.contains("restart it first, and escalate only if the restart"),
            "a stuck review is restarted before it is escalated: {brief}"
        );
        assert!(
            !brief.contains("instead of deciding it"),
            "no step escalates a stuck review instead of restarting it: {brief}"
        );
        assert!(!brief.contains("--commit"), "{brief}");
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

    /// The orchestrator's argv as the first `up` started it.
    fn orchestrator_argv(mux: &FakeMux) -> Result<Vec<String>> {
        started_argvs(mux)
            .into_iter()
            .find(|argv| argv.first().map(String::as_str) == Some("claude"))
            .context("the first up started the orchestrator")
    }

    #[test]
    fn an_orchestrator_tab_without_an_agent_is_relaunched_in_its_pane() -> Result<()> {
        let fixture = Fixture::new(Some("claude/claude-opus-5-5"))?;
        let env = fixture.env()?;
        let mux = mux();
        let first = up(&mux, &env).map_err(failed)?;
        let pane = pane_of(&mux, &first.orchestrator_tab)?;
        let launched = orchestrator_argv(&mux)?;
        // A herdr restart restored the tab but not the agent: the pane holds a
        // bare shell, which is what `Unknown` after the whole wait means.
        mux.set_state(&pane, AgentState::Unknown);
        let before = mux.calls().len();
        let second = up(&mux, &env).map_err(failed)?;

        assert!(
            !second.created_orchestrator_tab,
            "the orchestrator tab is reused, not recreated"
        );
        assert_eq!(second.orchestrator_tab, first.orchestrator_tab);
        let later = mux.calls()[before..].to_vec();
        let runs: Vec<(PaneId, Vec<String>)> = later
            .iter()
            .filter_map(|call| match call {
                Call::Run(run_pane, argv, _) => Some((run_pane.clone(), argv.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(runs.len(), 1, "exactly one launch: {later:?}");
        assert_eq!(runs[0].0, pane, "the orchestrator restarts in its own pane");
        assert_eq!(
            runs[0].1, launched,
            "a relaunch uses the same launch plan as a fresh up"
        );
        assert!(
            later
                .iter()
                .all(|call| !matches!(call, Call::CreateTab(..))),
            "no second tab is made: {later:?}"
        );
        Ok(())
    }

    #[test]
    fn an_orchestrator_tab_with_an_agent_is_left_alone() -> Result<()> {
        let fixture = Fixture::new(Some("claude/claude-opus-5-5"))?;
        let env = fixture.env()?;
        let mux = mux();
        let first = up(&mux, &env).map_err(failed)?;
        let pane = pane_of(&mux, &first.orchestrator_tab)?;
        // An agent that is idle is an agent a human may be talking to.
        mux.set_state(&pane, AgentState::Idle);
        let before = mux.calls().len();
        let second = up(&mux, &env).map_err(failed)?;

        assert!(!second.created_orchestrator_tab);
        let later = mux.calls()[before..].to_vec();
        assert!(
            later.iter().all(|call| !matches!(
                call,
                Call::Run(..) | Call::CreateTab(..) | Call::Prompt(..)
            )),
            "a running orchestrator is not restarted: {later:?}"
        );
        Ok(())
    }

    #[test]
    fn a_matching_process_group_leaves_the_orchestrator_alone() -> Result<()> {
        let fixture = Fixture::new(Some("claude/claude-opus-5-5"))?;
        let env = fixture.env()?;
        // Every pane answers with the same group, so the second `up` sees the
        // process the first one started.
        let mux = mux().with_process_group(88158);
        let first = up(&mux, &env).map_err(failed)?;
        assert_eq!(
            env.ctx
                .state()
                .map_err(failed)?
                .orchestrator
                .map(|orchestrator| orchestrator.process_group),
            Some(88158),
            "the first up records the group it launched under"
        );
        mux.set_state(&pane_of(&mux, &first.orchestrator_tab)?, AgentState::Idle);
        let before = mux.calls().len();
        let second = up(&mux, &env).map_err(failed)?;

        assert!(!second.created_orchestrator_tab);
        assert_eq!(second.orchestrator_tab, first.orchestrator_tab);
        let later = mux.calls()[before..].to_vec();
        assert!(
            later.iter().all(|call| !matches!(
                call,
                Call::Run(..)
                    | Call::CreateTab(..)
                    | Call::CloseTab(..)
                    | Call::SendText(..)
                    | Call::Prompt(..)
            )),
            "a session shprd started is not restarted: {later:?}"
        );
        Ok(())
    }

    #[test]
    fn an_orchestrator_pane_the_host_forgot_is_reopened_with_one() -> Result<()> {
        let fixture = Fixture::new(Some("claude/claude-opus-5-5"))?;
        let env = fixture.env()?;
        let mux = mux().with_process_group(88158);
        let first = up(&mux, &env).map_err(failed)?;
        let gone = pane_of(&mux, &first.orchestrator_tab)?;
        // A human closed the orchestrator's pane; the host kept the tab.
        mux.close_pane(&gone).map_err(|err| anyhow!("{err}"))?;
        let launched = orchestrator_argv(&mux)?;
        let before = mux.calls().len();
        let second = up(&mux, &env).map_err(failed)?;

        let later = mux.calls()[before..].to_vec();
        let runs: Vec<(PaneId, Vec<String>)> = later
            .iter()
            .filter_map(|call| match call {
                Call::Run(run_pane, argv, _) => Some((run_pane.clone(), argv.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(runs.len(), 1, "exactly one launch: {later:?}");
        assert_eq!(
            runs[0].1, launched,
            "the relaunch uses the same launch plan as a fresh up"
        );
        assert_ne!(runs[0].0, gone, "a pane the host forgot is never reused");
        assert!(
            later
                .iter()
                .any(|call| call == &Call::CloseTab(first.orchestrator_tab.clone())),
            "the husk tab goes rather than keeping a pane nothing runs in: {later:?}"
        );
        assert!(second.created_orchestrator_tab, "the tab was reopened");
        assert_ne!(second.orchestrator_tab, first.orchestrator_tab);
        Ok(())
    }

    #[test]
    fn a_pane_started_by_the_host_is_replaced_in_the_same_tab() -> Result<()> {
        let fixture = Fixture::new(Some("claude/claude-opus-5-5"))?;
        let env = fixture.env()?;
        let mux = mux().with_process_group(88158);
        let first = up(&mux, &env).map_err(failed)?;
        let old_pane = pane_of(&mux, &first.orchestrator_tab)?;
        // herdr restored the tab with its own `claude --resume`: a live agent,
        // and a process group shprd never launched.
        mux.set_state(&old_pane, AgentState::Idle);
        mux.set_process_group(&old_pane, Some(90001));
        let before = mux.calls().len();
        let second = up(&mux, &env).map_err(failed)?;

        assert_eq!(
            second.orchestrator_tab, first.orchestrator_tab,
            "the tab keeps its id, and so its place in the tab bar"
        );
        assert!(
            !second.created_orchestrator_tab,
            "the tab was reused, not recreated"
        );
        let later = mux.calls()[before..].to_vec();
        assert!(
            later
                .iter()
                .all(|call| !matches!(call, Call::CloseTab(..) | Call::CreateTab(..))),
            "the tab is never closed and reopened: {later:?}"
        );
        assert!(
            later
                .iter()
                .all(|call| !matches!(call, Call::SendText(..) | Call::Prompt(..))),
            "nothing is typed into the stranger: {later:?}"
        );

        let plan = orchestrator_argv(&mux)?;
        let split = later
            .iter()
            .position(|call| matches!(call, Call::Split(pane, ..) if pane == &old_pane))
            .context("the foreign pane is split")?;
        let run = later
            .iter()
            .position(|call| matches!(call, Call::Run(_, argv, _) if argv == &plan))
            .context("the orchestrator is launched")?;
        let fresh = match &later[run] {
            Call::Run(pane, ..) => pane.clone(),
            _ => unreachable!("position found a run"),
        };
        let closed = later
            .iter()
            .position(|call| call == &Call::ClosePane(old_pane.clone()))
            .context("the foreign pane is closed")?;
        assert!(
            split < run && run < closed,
            "split, launch, then close the stranger: {later:?}"
        );
        assert_ne!(fresh, old_pane, "the foreign process is not reused");
        assert_eq!(
            env.ctx
                .state()
                .map_err(failed)?
                .orchestrator
                .map(|orchestrator| orchestrator.pane),
            Some(fresh.as_str().to_string()),
            "the relaunch records the pane it started"
        );
        Ok(())
    }

    #[test]
    fn a_run_with_no_recorded_group_does_not_trust_the_tab() -> Result<()> {
        let fixture = Fixture::new(Some("claude/claude-opus-5-5"))?;
        let env = fixture.env()?;
        // A host that cannot tell while the first `up` runs records nothing.
        let mux = mux();
        let first = up(&mux, &env).map_err(failed)?;
        assert!(
            env.ctx.state().map_err(failed)?.orchestrator.is_none(),
            "no group was recorded"
        );
        let old_pane = pane_of(&mux, &first.orchestrator_tab)?;
        mux.set_state(&old_pane, AgentState::Idle);
        mux.set_process_group(&old_pane, Some(777));
        let before = mux.calls().len();
        let second = up(&mux, &env).map_err(failed)?;

        assert_eq!(
            second.orchestrator_tab, first.orchestrator_tab,
            "an unrecorded pane is replaced without moving the tab"
        );
        assert!(!second.created_orchestrator_tab);
        let later = mux.calls()[before..].to_vec();
        assert!(
            later.contains(&Call::ClosePane(old_pane.clone())),
            "the unclaimed pane is closed: {later:?}"
        );
        assert!(
            later
                .iter()
                .all(|call| !matches!(call, Call::CloseTab(..) | Call::CreateTab(..))),
            "the tab is kept: {later:?}"
        );
        Ok(())
    }

    #[test]
    fn a_host_without_splits_reopens_the_orchestrator_tab_instead() -> Result<()> {
        let fixture = Fixture::new(Some("claude/claude-opus-5-5"))?;
        let env = fixture.env()?;
        let mux = mux().with_process_group(88158).without_split();
        let first = up(&mux, &env).map_err(failed)?;
        let old_pane = pane_of(&mux, &first.orchestrator_tab)?;
        mux.set_state(&old_pane, AgentState::Idle);
        mux.set_process_group(&old_pane, Some(90001));
        let before = mux.calls().len();
        let second = up(&mux, &env).map_err(failed)?;

        assert!(
            second.created_orchestrator_tab,
            "the fallback reopens the tab, so up made it"
        );
        let later = mux.calls()[before..].to_vec();
        assert!(
            later.contains(&Call::CloseTab(first.orchestrator_tab)),
            "the tab is closed: {later:?}"
        );
        assert!(
            later
                .iter()
                .any(|call| matches!(call, Call::CreateTab(_, title, _) if title.ends_with(ORCHESTRATOR_LABEL))),
            "and reopened under the same title: {later:?}"
        );
        Ok(())
    }

    #[test]
    fn a_host_without_pane_closes_reopens_the_orchestrator_tab() -> Result<()> {
        let fixture = Fixture::new(Some("claude/claude-opus-5-5"))?;
        let env = fixture.env()?;
        let mux = mux().with_process_group(88158).without_close_pane();
        let first = up(&mux, &env).map_err(failed)?;
        let old_pane = pane_of(&mux, &first.orchestrator_tab)?;
        mux.set_state(&old_pane, AgentState::Idle);
        mux.set_process_group(&old_pane, Some(90001));
        let before = mux.calls().len();
        let second = up(&mux, &env).map_err(failed)?;

        assert!(
            second.created_orchestrator_tab,
            "a host that cannot close the pane takes the reopen route"
        );
        let later = mux.calls()[before..].to_vec();
        assert!(
            later.iter().all(|call| !matches!(call, Call::Split(..))),
            "nothing is split into a tab that cannot be cleaned up: {later:?}"
        );
        assert!(
            later.contains(&Call::CloseTab(first.orchestrator_tab)),
            "the tab is closed, and the foreign process with it: {later:?}"
        );
        assert!(
            later
                .iter()
                .any(|call| matches!(call, Call::CreateTab(_, title, _) if title.ends_with(ORCHESTRATOR_LABEL))),
            "and reopened under the same title: {later:?}"
        );
        Ok(())
    }

    #[test]
    fn a_pane_close_that_fails_after_the_launch_takes_the_tab_with_it() -> Result<()> {
        let fixture = Fixture::new(Some("claude/claude-opus-5-5"))?;
        let env = fixture.env()?;
        let mux = mux()
            .with_process_group(88158)
            .failing_close_pane("the host lost the pane");
        let first = up(&mux, &env).map_err(failed)?;
        let old_pane = pane_of(&mux, &first.orchestrator_tab)?;
        mux.set_state(&old_pane, AgentState::Idle);
        mux.set_process_group(&old_pane, Some(90001));
        let before = mux.calls().len();
        let second = up(&mux, &env).map_err(failed)?;

        assert!(
            second.created_orchestrator_tab,
            "the fallback reopens the tab rather than leave the stranger running"
        );
        assert_ne!(
            second.orchestrator_tab, first.orchestrator_tab,
            "the replacement is a new tab, not the one with two processes"
        );
        let later = mux.calls()[before..].to_vec();
        assert!(
            later.contains(&Call::ClosePane(old_pane.clone())),
            "closing the one pane is tried first: {later:?}"
        );
        assert!(
            later.contains(&Call::CloseTab(first.orchestrator_tab)),
            "and the whole tab goes when that close fails: {later:?}"
        );
        let plan = orchestrator_argv(&mux)?;
        assert!(
            later
                .iter()
                .any(|call| matches!(call, Call::Run(_, argv, _) if argv == &plan)),
            "the orchestrator runs in the reopened tab: {later:?}"
        );
        Ok(())
    }

    #[test]
    fn a_host_that_cannot_tell_keeps_todays_relaunch() -> Result<()> {
        let fixture = Fixture::new(Some("claude/claude-opus-5-5"))?;
        let env = fixture.env()?;
        let mux = mux_without_agent();
        let first = up(&mux, &env).map_err(failed)?;
        let pane = pane_of(&mux, &first.orchestrator_tab)?;
        mux.set_state(&pane, AgentState::Unknown);
        mux.set_process_group(&pane, None);
        let before = mux.calls().len();
        let second = up(&mux, &env).map_err(failed)?;

        assert_eq!(second.orchestrator_tab, first.orchestrator_tab);
        let later = mux.calls()[before..].to_vec();
        assert!(
            later.iter().all(|call| !matches!(
                call,
                Call::CloseTab(..) | Call::CreateTab(..) | Call::SendText(..)
            )),
            "a host that cannot tell keeps the tab: {later:?}"
        );
        assert!(
            later
                .iter()
                .any(|call| matches!(call, Call::Run(run_pane, ..) if run_pane == &pane)),
            "the orchestrator restarts in its own pane: {later:?}"
        );
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
    fn a_resumed_run_re_opens_a_missing_integration_worktree() -> Result<()> {
        let fixture = Fixture::new(Some("claude/claude-opus-5-5"))?;
        let env = fixture.env()?;
        let mux = mux();
        // A log that says the run started and no worktree on disk: the shape a
        // crash between the two leaves behind.
        let run_id = new_run_id();
        env.ctx
            .commit(EventKind::RunStarted {
                run_id: run_id.clone(),
                repo: env.ctx.root.to_string_lossy().to_string(),
                base_branch: "main".to_string(),
                integration_branch: integration_branch(&run_id),
            })
            .map_err(failed)?;
        let integration = integration_worktree(&fixture, &run_id);
        assert!(
            !integration.exists(),
            "{} started out missing",
            integration.display()
        );

        let report = up(&mux, &env).map_err(failed)?;
        assert!(report.resumed, "the log already held a run");
        assert_eq!(report.run_id, run_id);
        assert!(integration.is_dir(), "{} is missing", integration.display());
        assert_eq!(env.ctx.state().map_err(failed)?.resources.len(), 2);

        // The second `up` finds both resources live, so it records nothing new.
        up(&mux, &env).map_err(failed)?;
        assert_eq!(
            env.ctx.state().map_err(failed)?.resources.len(),
            2,
            "a repeated up recorded the same resources twice"
        );
        Ok(())
    }

    /// A crash between "the worktree is on disk" and "the ledger knows it"
    /// leaves a run whose integration worktree exists with `created: false`.
    /// The next `up` must adopt it: it is this run's own, at the layout's own
    /// path, and the janitor needs the record to own it.
    #[test]
    fn an_unrecorded_integration_worktree_is_adopted() -> Result<()> {
        let fixture = Fixture::new(Some("claude/claude-opus-5-5"))?;
        let env = fixture.env()?;
        let mux = mux();
        let run_id = new_run_id();
        env.ctx
            .commit(EventKind::RunStarted {
                run_id: run_id.clone(),
                repo: env.ctx.root.to_string_lossy().to_string(),
                base_branch: "main".to_string(),
                integration_branch: integration_branch(&run_id),
            })
            .map_err(failed)?;
        // The worktree lands without its events: what a crash between the two
        // leaves behind.
        let layout = Layout::default_for_home(&env.home);
        let int =
            integrate::open_integration(&env.ctx.root, &layout, &env.repo_name, &run_id, "main")
                .map_err(|err| anyhow!("{err}"))?;
        assert!(int.created, "the fixture worktree started out missing");
        assert_eq!(
            env.ctx.state().map_err(failed)?.resources.len(),
            0,
            "the ledger started out empty"
        );

        let report = up(&mux, &env).map_err(failed)?;
        assert!(report.resumed, "the log already held a run");
        assert_eq!(report.run_id, run_id);
        let (state, events) = env.ctx.load().map_err(failed)?;
        assert_eq!(state.resources.len(), 2, "the adoption recorded both");
        assert_eq!(resource_events(&events), 2, "both resources were recorded");

        // The second `up` finds both live, so it records nothing new.
        up(&mux, &env).map_err(failed)?;
        let (again, events) = env.ctx.load().map_err(failed)?;
        assert_eq!(
            again.resources.len(),
            2,
            "a repeated up recorded duplicates"
        );
        assert_eq!(resource_events(&events), 2, "a repeated up wrote events");
        Ok(())
    }

    /// How many `Resource` events a loaded log holds.
    fn resource_events(events: &[shprd_core::event::Event]) -> usize {
        events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::Resource { .. }))
            .count()
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

    /// A started program: the pane it runs in, and the environment it got.
    type WatchRun = (PaneId, Vec<(String, Option<String>)>);

    /// Every run of the watchdog's argv, with the pane and environment it got.
    fn watch_runs(mux: &FakeMux) -> Vec<WatchRun> {
        mux.calls()
            .into_iter()
            .filter_map(|call| match call {
                Call::Run(pane, argv, env) if argv == watch_argv() => Some((pane, env)),
                _ => None,
            })
            .collect()
    }

    /// Every pane `up` split off another, with the pane it came from.
    fn splits(mux: &FakeMux) -> Vec<(PaneId, Direction, f32)> {
        mux.calls()
            .into_iter()
            .filter_map(|call| match call {
                Call::Split(pane, direction, ratio, _) => Some((pane, direction, ratio)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn up_runs_the_watchdog_in_a_pane_of_the_chat_tab() -> Result<()> {
        let fixture = Fixture::new(Some("claude/claude-opus-5-5"))?;
        let env = fixture.env()?;
        let mux = mux().with_process_group(4242);
        let report = up(&mux, &env).map_err(failed)?;

        // The pane is split off the chat tab, at the bottom of it: the tab the
        // human types in, not the orchestrator's.
        let chat_pane = pane_of(&mux, &report.chat_tab)?;
        assert_eq!(
            splits(&mux),
            vec![(chat_pane.clone(), Direction::Down, WATCH_RATIO)],
            "the watchdog gets its own pane at the bottom of the chat tab"
        );

        let runs = watch_runs(&mux);
        assert_eq!(runs.len(), 1, "{:?}", mux.calls());
        let (pane, pairs) = &runs[0];
        assert!(
            splits(&mux).iter().any(|(parent, ..)| parent == &chat_pane),
            "the watchdog runs in the pane just split off the chat tab"
        );
        assert_eq!(
            pairs
                .iter()
                .find(|(name, _)| name == "SHPRD_ACTOR")
                .map(|(_, value)| value.as_deref()),
            Some(Some("engine")),
            "the watchdog acts as the engine: {pairs:?}"
        );
        assert_eq!(
            pairs
                .iter()
                .find(|(name, _)| name == "SHPRD_RUN")
                .map(|(_, value)| value.as_deref()),
            Some(Some(report.run_id.as_str())),
            "the watchdog works on this run: {pairs:?}"
        );

        // The run records the pane, so the next `up` and the run's end can
        // find it again.
        let recorded = env
            .ctx
            .state()
            .map_err(failed)?
            .watch
            .context("the watchdog's pane is recorded")?;
        assert_eq!(recorded.pane, pane.as_str());
        assert_eq!(recorded.process_group, Some(4242));
        Ok(())
    }

    #[test]
    fn a_second_up_leaves_a_healthy_watchdog_alone() -> Result<()> {
        let fixture = Fixture::new(Some("claude/claude-opus-5-5"))?;
        let env = fixture.env()?;
        let mux = mux().with_process_group(4242);
        up(&mux, &env).map_err(failed)?;
        let pane = env
            .ctx
            .state()
            .map_err(failed)?
            .watch
            .context("the watchdog's pane is recorded")?
            .pane;
        let before = mux.calls().len();
        up(&mux, &env).map_err(failed)?;

        assert_eq!(
            splits(&mux).len(),
            1,
            "a live watchdog is never split a second time: {:?}",
            mux.calls()
        );
        assert_eq!(watch_runs(&mux).len(), 1, "{:?}", mux.calls());
        assert!(
            mux.calls()[before..]
                .iter()
                .all(|call| !matches!(call, Call::ClosePane(..))),
            "a healthy pane is never closed"
        );
        assert_eq!(
            env.ctx.state().map_err(failed)?.watch.map(|w| w.pane),
            Some(pane)
        );
        Ok(())
    }

    #[test]
    fn a_watchdog_pane_whose_process_changed_is_relaunched() -> Result<()> {
        let fixture = Fixture::new(Some("claude/claude-opus-5-5"))?;
        let env = fixture.env()?;
        let mux = mux().with_process_group(4242);
        up(&mux, &env).map_err(failed)?;
        // A host that restored the workspace started its own process in the
        // pane, which is not the watchdog shprd launched.
        let stale = env
            .ctx
            .state()
            .map_err(failed)?
            .watch
            .context("the watchdog's pane is recorded")?
            .pane;
        mux.set_process_group(&PaneId::new(stale.clone()), Some(999));
        up(&mux, &env).map_err(failed)?;

        let runs = watch_runs(&mux);
        assert_eq!(
            runs.len(),
            2,
            "the watchdog is started again: {:?}",
            mux.calls()
        );
        let fresh = env
            .ctx
            .state()
            .map_err(failed)?
            .watch
            .context("the watchdog's pane is recorded")?;
        assert_ne!(fresh.pane, stale, "the new watchdog is in a new pane");
        assert!(
            mux.calls()
                .iter()
                .any(|call| matches!(call, Call::ClosePane(pane) if pane.as_str() == stale)),
            "the foreign pane is closed once the watchdog runs again: {:?}",
            mux.calls()
        );
        Ok(())
    }

    #[test]
    fn a_watchdog_pane_the_host_forgot_is_relaunched() -> Result<()> {
        let fixture = Fixture::new(Some("claude/claude-opus-5-5"))?;
        let env = fixture.env()?;
        let mux = mux().with_process_group(4242);
        up(&mux, &env).map_err(failed)?;
        let gone = env
            .ctx
            .state()
            .map_err(failed)?
            .watch
            .context("the watchdog's pane is recorded")?
            .pane;
        mux.close_pane(&PaneId::new(gone))
            .map_err(|err| anyhow!("{err}"))?;
        up(&mux, &env).map_err(failed)?;

        assert_eq!(
            watch_runs(&mux).len(),
            2,
            "a vanished pane is a watchdog that is not running: {:?}",
            mux.calls()
        );
        Ok(())
    }

    #[test]
    fn a_live_watchdog_pane_with_no_group_to_report_is_left_alone() -> Result<()> {
        let fixture = Fixture::new(Some("claude/claude-opus-5-5"))?;
        let env = fixture.env()?;
        let mux = mux().with_process_group(4242);
        up(&mux, &env).map_err(failed)?;
        let pane = env
            .ctx
            .state()
            .map_err(failed)?
            .watch
            .context("the watchdog's pane is recorded")?
            .pane;
        // The host still has the pane, but cannot say which process leads it.
        mux.set_process_group(&PaneId::new(pane.clone()), None);
        up(&mux, &env).map_err(failed)?;

        assert_eq!(
            splits(&mux).len(),
            1,
            "a host that cannot tell is no evidence against the pane: {:?}",
            mux.calls()
        );
        assert_eq!(watch_runs(&mux).len(), 1, "{:?}", mux.calls());
        assert_eq!(
            env.ctx
                .state()
                .map_err(failed)?
                .watch
                .map(|watch| watch.pane),
            Some(pane),
            "the recorded pane stays the one shprd is watching"
        );
        Ok(())
    }

    #[test]
    fn a_host_that_cannot_close_the_stale_watchdog_starts_no_second_one() -> Result<()> {
        let fixture = Fixture::new(Some("claude/claude-opus-5-5"))?;
        let env = fixture.env()?;
        let mux = mux().with_process_group(4242).without_close_pane();
        up(&mux, &env).map_err(failed)?;
        let stale = env
            .ctx
            .state()
            .map_err(failed)?
            .watch
            .context("the watchdog's pane is recorded")?
            .pane;
        // A host that restored the workspace runs its own process in the pane.
        mux.set_process_group(&PaneId::new(stale.clone()), Some(999));
        let before = mux.calls().len();
        up(&mux, &env).map_err(failed)?;

        let later = mux.calls()[before..].to_vec();
        assert!(
            later.iter().all(|call| !matches!(call, Call::Split(..))),
            "the foreign pane cannot be closed, so nothing is split beside it: {later:?}"
        );
        assert_eq!(watch_runs(&mux).len(), 1, "{:?}", mux.calls());
        assert_eq!(
            env.ctx
                .state()
                .map_err(failed)?
                .watch
                .map(|watch| watch.pane),
            Some(stale.clone()),
            "no new watchdog is recorded"
        );
        // The human is told what to do about the pane in the way.
        let line = not_shprds_watch_pane(&PaneId::new(stale));
        assert!(line.contains("cannot close it"), "{line}");
        assert!(line.contains("shprd watch"), "{line}");
        Ok(())
    }

    #[test]
    fn a_new_watchdog_whose_stale_pane_will_not_close_is_taken_back_down() -> Result<()> {
        let fixture = Fixture::new(Some("claude/claude-opus-5-5"))?;
        let env = fixture.env()?;
        let mux = mux().with_process_group(4242);
        up(&mux, &env).map_err(failed)?;
        let stale = env
            .ctx
            .state()
            .map_err(failed)?
            .watch
            .context("the watchdog's pane is recorded")?
            .pane;
        let stale = PaneId::new(stale);
        // The host restored the workspace into the recorded pane, and will not
        // let go of it, while any other pane closes as asked.
        mux.set_process_group(&stale, Some(999));
        mux.failing_close_of(&stale, "the pane is busy");
        let before = mux.calls().len();
        up(&mux, &env).map_err(failed)?;

        let later = mux.calls()[before..].to_vec();
        let runs = watch_runs(&mux);
        assert_eq!(runs.len(), 2, "a replacement is started: {later:?}");
        let fresh = runs[1].0.clone();
        assert_ne!(fresh, stale, "the replacement is in a new pane");
        let closed = later
            .iter()
            .position(|call| call == &Call::ClosePane(stale.clone()))
            .context("the stale pane is closed")?;
        let closed_again = later
            .iter()
            .position(|call| call == &Call::ClosePane(fresh.clone()))
            .context("the replacement is taken back down")?;
        assert!(
            closed < closed_again,
            "the replacement goes only after the stale pane refuses: {later:?}"
        );
        // The end state, not just the attempt: the pane holding the watchdog is
        // the recorded one, and the pane that was taken back down is gone.
        assert!(
            mux.pane_process_group(&fresh).is_err(),
            "the replacement is closed, not merely asked to close: {:?}",
            mux.calls()
        );
        assert_eq!(
            env.ctx
                .state()
                .map_err(failed)?
                .watch
                .map(|watch| watch.pane),
            Some(stale.as_str().to_string()),
            "the record stays on the pane that is still there"
        );
        Ok(())
    }

    #[test]
    fn two_watchdog_panes_that_both_refuse_to_close_record_the_one_that_runs() -> Result<()> {
        let fixture = Fixture::new(Some("claude/claude-opus-5-5"))?;
        let env = fixture.env()?;
        let mux = mux()
            .with_process_group(4242)
            .failing_close_pane("the pane is busy");
        up(&mux, &env).map_err(failed)?;
        let stale = PaneId::new(
            env.ctx
                .state()
                .map_err(failed)?
                .watch
                .context("the watchdog's pane is recorded")?
                .pane,
        );
        mux.set_process_group(&stale, Some(999));
        let before = mux.calls().len();
        up(&mux, &env).map_err(failed)?;

        let later = mux.calls()[before..].to_vec();
        let runs = watch_runs(&mux);
        assert_eq!(runs.len(), 2, "a replacement is started: {later:?}");
        let fresh = runs[1].0.clone();
        assert_ne!(fresh, stale);
        // Both panes are still there, so both watchdogs are: the record follows
        // the one `up` can point at from now on.
        assert_eq!(
            env.ctx
                .state()
                .map_err(failed)?
                .watch
                .map(|watch| watch.pane),
            Some(fresh.as_str().to_string()),
            "the record names the replacement, not the pane that would not go"
        );
        // The human is told about both panes.
        let line = two_watch_panes(&stale, &fresh);
        assert!(line.contains(stale.as_str()), "{line}");
        assert!(line.contains(fresh.as_str()), "{line}");
        assert!(line.contains("shprd watch"), "{line}");

        // And no later `up` piles a third watchdog beside them.
        let before = mux.calls().len();
        up(&mux, &env).map_err(failed)?;
        let afterwards = mux.calls()[before..].to_vec();
        assert!(
            afterwards
                .iter()
                .all(|call| !matches!(call, Call::Split(..))),
            "the recorded pane is running, so nothing is split: {afterwards:?}"
        );
        assert_eq!(
            watch_runs(&mux).len(),
            2,
            "no third watchdog is launched: {:?}",
            mux.calls()
        );
        Ok(())
    }

    #[test]
    fn a_host_without_splits_still_leaves_up_green() -> Result<()> {
        let fixture = Fixture::new(Some("claude/claude-opus-5-5"))?;
        let env = fixture.env()?;
        let mux = mux().without_split();
        let report = up(&mux, &env).map_err(failed)?;

        assert!(report.created_orchestrator_tab, "up must still succeed");
        assert!(watch_runs(&mux).is_empty(), "there is no pane to run it in");
        assert_eq!(
            env.ctx.state().map_err(failed)?.watch,
            None,
            "a watchdog that never started is not recorded"
        );
        // The human is told how to start one, and the advice names the verb.
        assert!(NO_WATCH_ADVICE.contains("shprd watch"), "{NO_WATCH_ADVICE}");
        Ok(())
    }

    #[test]
    fn down_closes_the_watchdog_pane() -> Result<()> {
        let fixture = Fixture::new(Some("claude/claude-opus-5-5"))?;
        let env = fixture.env()?;
        let mux = mux().with_process_group(4242);
        up(&mux, &env).map_err(failed)?;
        let pane = env
            .ctx
            .state()
            .map_err(failed)?
            .watch
            .context("the watchdog's pane is recorded")?
            .pane;
        down(&mux, &env.ctx, Some(&env.home)).map_err(failed)?;

        assert!(
            mux.calls()
                .iter()
                .any(|call| matches!(call, Call::ClosePane(closed) if closed.as_str() == pane)),
            "run end closes the watchdog's pane: {:?}",
            mux.calls()
        );
        Ok(())
    }

    #[test]
    fn down_counts_a_watchdog_pane_already_gone_as_closed() -> Result<()> {
        let fixture = Fixture::new(Some("claude/claude-opus-5-5"))?;
        let env = fixture.env()?;
        let mux = mux().with_process_group(4242);
        up(&mux, &env).map_err(failed)?;
        let pane = env
            .ctx
            .state()
            .map_err(failed)?
            .watch
            .context("the watchdog's pane is recorded")?
            .pane;
        mux.close_pane(&PaneId::new(pane))
            .map_err(|err| anyhow!("{err}"))?;
        down(&mux, &env.ctx, Some(&env.home)).map_err(failed)?;
        Ok(())
    }

    #[test]
    fn down_refuses_while_a_task_is_working() -> Result<()> {
        let fixture = Fixture::new(Some("claude/claude-opus-5-5"))?;
        let env = fixture.env()?;
        working_task(&env.ctx)?;
        let err = down(&mux(), &env.ctx, Some(&env.home))
            .err()
            .context("down must refuse")?;
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
        down(&mux(), &env.ctx, Some(&env.home)).map_err(failed)?;
        assert!(env.ctx.state().map_err(failed)?.finished);
        Ok(())
    }

    #[test]
    fn a_finished_run_is_archived_so_up_starts_the_next_one() -> Result<()> {
        let fixture = Fixture::new(Some("claude/claude-opus-5-5"))?;
        let env = fixture.env()?;
        let mux = mux();
        let first = up(&mux, &env).map_err(failed)?;
        down(&mux, &env.ctx, Some(&env.home)).map_err(failed)?;
        let second = up(&mux, &env).map_err(failed)?;

        assert!(!second.resumed, "a finished run is never resumed");
        assert_ne!(second.run_id, first.run_id, "up must mint a new run");
        let archive = fixture
            .repo
            .path()
            .join(".shprd")
            .join("runs")
            .join(format!("{}.jsonl", first.run_id));
        assert!(archive.is_file(), "{} is missing", archive.display());
        let (state, events) = env.ctx.load().map_err(failed)?;
        assert_eq!(
            state.run.as_ref().map(|run| run.run_id.clone()),
            Some(second.run_id.clone()),
            "the fresh log holds the new run"
        );
        assert!(!state.finished, "the new run is in flight");
        assert_eq!(
            events.first().map(|event| event.seq),
            Some(1),
            "the fresh log starts at seq 1"
        );
        assert!(
            matches!(
                events.first().map(|event| &event.kind),
                Some(EventKind::RunStarted { .. })
            ),
            "{events:?}"
        );
        Ok(())
    }

    #[test]
    fn an_archived_run_is_never_overwritten() -> Result<()> {
        let fixture = Fixture::new(Some("claude/claude-opus-5-5"))?;
        let env = fixture.env()?;
        let mux = mux();
        let first = up(&mux, &env).map_err(failed)?;
        down(&mux, &env.ctx, Some(&env.home)).map_err(failed)?;
        let runs = fixture.repo.path().join(".shprd").join("runs");
        std::fs::create_dir_all(&runs)?;
        let taken = runs.join(format!("{}.jsonl", first.run_id));
        std::fs::write(&taken, "# an earlier archive\n")?;
        up(&mux, &env).map_err(failed)?;
        let second = runs.join(format!("{}-2.jsonl", first.run_id));
        assert!(second.is_file(), "{} is missing", second.display());
        assert_eq!(std::fs::read_to_string(&taken)?, "# an earlier archive\n");
        Ok(())
    }

    #[test]
    fn run_end_closes_the_tabs_it_recorded_and_leaves_the_humans_tabs() -> Result<()> {
        let fixture = Fixture::new(Some("claude/claude-opus-5-5"))?;
        let env = fixture.env()?;
        let mux = mux();
        let report = up(&mux, &env).map_err(failed)?;
        let workspace = mux
            .find_workspace(&naming::workspace_name(&env.repo_name))
            .map_err(|err| anyhow!("{err}"))?
            .context("up must leave a workspace")?;
        // One tab per attempt record, as a spawn and a review would leave them.
        let worker = mux
            .create_tab(&workspace.id, "worker", fixture.repo.path())
            .map_err(|err| anyhow!("{err}"))?;
        let reviewer = mux
            .create_tab(&workspace.id, "reviewer", fixture.repo.path())
            .map_err(|err| anyhow!("{err}"))?;
        let briefs = env.home.join(".shprd").join("briefs").join(&report.run_id);
        std::fs::create_dir_all(&briefs)?;
        std::fs::write(briefs.join("T1-a1.tab"), worker.id.as_str())?;
        std::fs::write(briefs.join("T1-a1-review.tab"), reviewer.id.as_str())?;

        down(&mux, &env.ctx, Some(&env.home)).map_err(failed)?;

        let open = mux
            .list_tabs(&workspace.id)
            .map_err(|err| anyhow!("{err}"))?;
        let ids: Vec<String> = open.iter().map(|tab| tab.id.to_string()).collect();
        assert!(
            !ids.contains(&worker.id.to_string()),
            "a worker tab outlived the run: {ids:?}"
        );
        assert!(
            !ids.contains(&reviewer.id.to_string()),
            "a reviewer tab outlived the run: {ids:?}"
        );
        // The two tabs shprd never recorded are the human's and the agent's.
        assert!(
            ids.contains(&report.chat_tab.to_string()),
            "run end closed the chat tab: {ids:?}"
        );
        assert!(
            ids.contains(&report.orchestrator_tab.to_string()),
            "run end closed the orchestrator tab: {ids:?}"
        );
        Ok(())
    }

    #[test]
    fn up_keeps_its_run_state_out_of_git_status() -> Result<()> {
        let fixture = Fixture::new(Some("claude/claude-opus-5-5"))?;
        let env = fixture.env()?;
        let mux = mux();
        up(&mux, &env).map_err(failed)?;
        std::fs::create_dir_all(fixture.repo.path().join(".dagr"))?;
        assert!(fixture.repo.path().join(".shprd").is_dir());
        assert_eq!(status(fixture.repo.path())?, "", "the checkout looks dirty");
        up(&mux, &env).map_err(failed)?;
        let exclude = exclude_path(fixture.repo.path())?;
        let text = std::fs::read_to_string(&exclude)?;
        assert_eq!(text.matches(".shprd/").count(), 1, "{text}");
        assert_eq!(text.matches(".dagr/").count(), 1, "{text}");
        assert_eq!(text.matches("# shprd run state").count(), 1, "{text}");
        Ok(())
    }

    /// Leaves stale, unignored run state in `repo`, as an earlier run would.
    fn stale_run_state(repo: &StdPath) -> Result<()> {
        std::fs::create_dir_all(repo.join(".shprd/runs"))?;
        std::fs::write(repo.join(".shprd/runs/x.jsonl"), "{}\n")?;
        std::fs::create_dir_all(repo.join(".dagr"))?;
        std::fs::write(repo.join(".dagr/run.json"), "{}\n")?;
        Ok(())
    }

    #[test]
    fn up_excludes_stale_run_state_before_reading_the_checkout() -> Result<()> {
        let fixture = Fixture::new(Some("claude/claude-opus-5-5"))?;
        let repo = fixture.repo.path();
        stale_run_state(repo)?;
        let env = fixture.env()?;
        let mux = mux();
        up(&mux, &env).map_err(failed)?;
        assert_eq!(status(repo)?, "", "stale run state made the checkout dirty");
        let exclude = exclude_path(repo)?;
        let text = std::fs::read_to_string(&exclude)?;
        assert_eq!(text.matches(".shprd/").count(), 1, "{text}");
        assert_eq!(text.matches(".dagr/").count(), 1, "{text}");
        up(&mux, &env).map_err(failed)?;
        let text = std::fs::read_to_string(&exclude)?;
        assert_eq!(
            text.matches(".shprd/").count(),
            1,
            "second up duplicated: {text}"
        );
        assert_eq!(
            text.matches(".dagr/").count(),
            1,
            "second up duplicated: {text}"
        );
        Ok(())
    }

    /// A checkout `up` refuses still gets its excludes, so the state an
    /// earlier run left behind never counts as dirty while a human fixes it.
    #[test]
    fn a_refused_checkout_still_gets_its_excludes() -> Result<()> {
        let fixture = Fixture::new(Some("claude/claude-opus-5-5"))?;
        let repo = fixture.repo.path();
        stale_run_state(repo)?;
        git(repo, &["checkout", "--detach"])?;
        let env = fixture.env()?;
        let mux = mux();
        let err = up(&mux, &env).err().context("up must refuse")?;
        assert_eq!(err.exit_code(), 2, "{err}");
        let text = std::fs::read_to_string(exclude_path(repo)?)?;
        assert_eq!(text.matches(".shprd/").count(), 1, "{text}");
        assert_eq!(text.matches(".dagr/").count(), 1, "{text}");
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
            ready_wait: Duration::ZERO,
            detect_wait: Duration::ZERO,
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
        // Exactly two tabs, no stray starter tab beside them: the chat tab
        // adopted the one the host made with the workspace.
        let titles: Vec<&str> = listed.iter().map(|tab| tab.title.as_str()).collect();
        assert_eq!(
            titles,
            ["shprd·chat", "★ sh·orchestrator"],
            "the workspace does not have exactly the two tabs up lays out"
        );
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
