//! Shared plumbing: repo root, log path, actor, and the check-then-append path
//! every writing verb goes through.

use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow};
use shprd_agents::data::DataDir;
use shprd_core::event::{Actor, Event, EventKind};
use shprd_core::log::{AppendError, Clock, EventLog, SystemClock};
use shprd_core::rules::RuleError;
use shprd_core::state::RunState;
use shprd_git::worktree::Layout;

/// Environment variable holding the default actor.
const ACTOR_ENV: &str = "SHPRD_ACTOR";

/// Environment variable overriding the home directory shprd keeps state in.
const HOME_ENV: &str = "SHPRD_HOME";

/// Environment variable naming the repository root, for a caller whose working
/// directory is not inside it — a reviewer's packet folder, for one.
const REPO_ENV: &str = "SHPRD_REPO";

/// Environment variable naming the config home, which wins over
/// `<home>/.config`.
const CONFIG_HOME_ENV: &str = "XDG_CONFIG_HOME";

/// Environment variable naming the state home, which wins over
/// `<home>/.local/state`.
const STATE_HOME_ENV: &str = "XDG_STATE_HOME";

/// Environment variable naming the data home, which wins over
/// `<home>/.local/share`.
const DATA_HOME_ENV: &str = "XDG_DATA_HOME";

/// Exit code for a command a transition rule refused.
pub const EXIT_REFUSED: u8 = 2;

/// Exit code for any other failure.
pub const EXIT_ERROR: u8 = 1;

/// A command's failure: a rule refusal or anything else.
#[derive(Debug)]
pub enum CliError {
    /// A transition rule refused the event; nothing was written.
    Refused(RuleError),
    /// A change was refused because it would leave the state unusable; the
    /// caller undid it, and the process exits 2 as it does for a rule.
    Invalid(anyhow::Error),
    /// Anything else: I/O, a bad flag, a corrupt log.
    Other(anyhow::Error),
}

impl CliError {
    /// Wraps a non-refusal failure.
    pub fn other(err: impl Into<anyhow::Error>) -> Self {
        Self::Other(err.into())
    }

    /// The process exit code this failure maps to.
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Refused(_) | Self::Invalid(_) => EXIT_REFUSED,
            Self::Other(_) => EXIT_ERROR,
        }
    }
}

impl std::fmt::Display for CliError {
    /// The message the binary prints for this failure, without a prefix.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(rule) => write!(f, "{rule}"),
            Self::Invalid(source) => write!(f, "invalid: {source:#}"),
            Self::Other(source) => write!(f, "{source:#}"),
        }
    }
}

impl From<RuleError> for CliError {
    fn from(err: RuleError) -> Self {
        Self::Refused(err)
    }
}

/// Everything a verb needs to read and write the run log.
#[derive(Debug, Clone)]
pub struct Ctx {
    /// Repository root the run works on.
    pub root: PathBuf,
    /// Path of the event log: `<root>/.shprd/events.jsonl`.
    pub log_path: PathBuf,
    /// Who is acting.
    pub actor: Actor,
    /// Whether to print JSON on stdout.
    pub json: bool,
    /// Whether to skip the dagr sync that follows every write.
    pub no_dagr: bool,
    /// The home directory shprd's own files live under, as the binary resolved
    /// it at the wiring edge.
    ///
    /// It is carried here because the dagr projection reads the person's
    /// `graph.dagr` setting and looks in their herdr plugin folder, and the
    /// writes that project happen deep inside verbs ([`crate::cmd::emit`]).
    /// `None` is a verb that resolved no home: then only `$SHPRD_DAGR_BIN` and
    /// `PATH` can name a program, because the two probes that need a home were
    /// never given one.
    pub home: Option<PathBuf>,
}

impl Ctx {
    /// Builds a context from the resolved root and the global options.
    pub fn new(root: PathBuf, actor: Actor, json: bool, no_dagr: bool) -> Self {
        let log_path = root.join(".shprd").join("events.jsonl");
        Self {
            root,
            log_path,
            actor,
            json,
            no_dagr,
            home: None,
        }
    }

    /// The same context, carrying the home directory the binary resolved.
    ///
    /// The edge resolves `--home`, `$SHPRD_HOME`, then `$HOME` once and hands
    /// the answer down this way, so no verb and no library reads it for itself.
    pub fn with_home(mut self, home: Option<PathBuf>) -> Self {
        self.home = home;
        self
    }

    /// The same context, acting as `actor`.
    ///
    /// The watch records what the engine decides — a review verdict, a gate
    /// result — no matter who started it, so it commits those with an engine
    /// context rather than whatever `$SHPRD_ACTOR` says.
    pub fn as_actor(&self, actor: Actor) -> Self {
        Self {
            actor,
            ..self.clone()
        }
    }

    /// Refuses unless the actor this context acts as is a human.
    ///
    /// This is the one actor rule of every human verb (`sync`, `drop`,
    /// `override`, `approve`, `readmit`): the actor is what `--as` says, else
    /// what `$SHPRD_ACTOR` says, else `human`, resolved once at the edge, so
    /// the two spellings can never disagree between verbs. The terminal is the
    /// barrier that holds on top of it. `act` completes "only a human ..."
    /// and `advice` says what the reader does instead.
    pub fn require_human(&self, act: &str, advice: &str) -> Result<(), CliError> {
        if matches!(self.actor, Actor::Human) {
            return Ok(());
        }
        let name = actor_name(&self.actor);
        Err(CliError::Invalid(anyhow!(
            "only a human {act}, and the actor {ACTOR_ENV}={name} (or --as {name}) is not one: {advice}"
        )))
    }

    /// The folded state of the run, or the empty state when no log exists yet.
    pub fn state(&self) -> Result<RunState, CliError> {
        Ok(self.load()?.0)
    }

    /// The folded state and the events it was folded from.
    ///
    /// Both are read from the same pass over the log, so a caller that needs
    /// the projectors' view of the run (state plus log) never sees two
    /// versions of the same file.
    pub fn load(&self) -> Result<(RunState, Vec<Event>), CliError> {
        if !self.log_path.exists() {
            return Ok((RunState::default(), Vec::new()));
        }
        let outcome = EventLog::read_all(&self.log_path)
            .map_err(|err| CliError::other(anyhow!(err).context("reading the event log")))?;
        let state = RunState::fold(&outcome.events)
            .map_err(|err| CliError::other(anyhow!(err).context("folding the event log")))?;
        Ok((state, outcome.events))
    }

    /// Checks `kind` against the current state, and appends nothing.
    ///
    /// A verb that has an effect outside the log — creating a branch, removing
    /// a worktree — runs this first, so a refused event never leaves a side
    /// effect the ledger does not know about.
    pub fn check(&self, kind: &EventKind) -> Result<(), CliError> {
        let state = self.state()?;
        state
            .check_as(&self.actor, kind, SystemClock.now_ms())
            .map_err(CliError::Refused)
    }

    /// Checks `kind` against the current state, then appends it.
    ///
    /// A refused event is never written. The check runs with the log's lock
    /// held ([`EventLog::append_checked`]), so two writers that would both pass
    /// a check against the same state cannot both write: the second sees the
    /// first's event and is refused rather than corrupting a log that must
    /// fold.
    pub fn commit(&self, kind: EventKind) -> Result<Event, CliError> {
        // The first check is the cheap one, against the log as it stands: a
        // refused verb must leave no log file behind, and opening one to ask
        // would create it. The second, under the lock, is the one that makes
        // two writers safe.
        self.check(&kind)?;
        let clock = SystemClock;
        let mut log = EventLog::open(&self.log_path)
            .map_err(|err| CliError::other(anyhow!(err).context("opening the event log")))?;
        let actor = self.actor.clone();
        let checked = kind.clone();
        let event = log
            .append_checked(actor.clone(), kind, &clock, move |events| {
                let state = RunState::fold(events)?;
                state.check_as(&actor, &checked, SystemClock.now_ms())
            })
            .map_err(|err| match err {
                AppendError::Refused(refused) => CliError::Refused(refused),
                AppendError::Log(err) => {
                    CliError::other(anyhow!(err).context("appending to the event log"))
                }
            })?;
        Ok(event)
    }

    /// Appends a `TaskAdded` event, naming it the next free `T<n>` when `task`
    /// is `None`; the id is picked under the log's lock, so two adders at once
    /// never share one. `kind` must be a `TaskAdded`; its `task` is replaced.
    pub fn commit_task_added(
        &self,
        task: Option<String>,
        kind: EventKind,
    ) -> Result<Event, CliError> {
        let mut log = EventLog::open(&self.log_path)
            .map_err(|err| CliError::other(anyhow!(err).context("opening the event log")))?;
        let actor = self.actor.clone();
        log.append_built(self.actor.clone(), &SystemClock, move |events| {
            let mut kind = kind;
            if let EventKind::TaskAdded { task: slot, .. } = &mut kind {
                *slot = task.unwrap_or_else(|| shprd_core::event::next_task_id(events));
            }
            RunState::fold(events)?.check_as(&actor, &kind, SystemClock.now_ms())?;
            Ok(kind)
        })
        .map_err(|err| match err {
            AppendError::Refused(refused) => CliError::Refused(refused),
            AppendError::Log(err) => {
                CliError::other(anyhow!(err).context("appending to the event log"))
            }
        })
    }
}

/// Resolves the repo root: `--repo`, then `$SHPRD_REPO`, then the git top level.
///
/// The env var exists because a command run outside the repository still has to
/// name it: asking git only works when the working directory is inside one.
/// Whatever names the root — the flag, the variable or the working directory —
/// a linked git worktree resolves to the repository it was cut from, because
/// that is where the run's log, config and worktrees live; see [`run_repo`].
pub fn resolve_root(explicit: Option<&Path>) -> Result<PathBuf, CliError> {
    Ok(run_repo(&resolve_checkout(explicit)?))
}

/// The root `shprd serve` works on: the repository the named folder (or the
/// working directory) is inside, else the folder itself.
///
/// Every other verb needs a repository and refuses without one; the service
/// starts for a folder the person only just added, which becomes a repository at
/// their first chat message ([`crate::cmd::adopt`]). A folder inside a
/// repository serves that repository, never a second one.
pub fn resolve_serve_root(explicit: Option<&Path>) -> Result<PathBuf, CliError> {
    let named = explicit.map(Path::to_path_buf).or_else(|| {
        std::env::var_os(REPO_ENV)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    });
    let folder = match named {
        Some(path) => path,
        None => std::env::current_dir()
            .map_err(|err| CliError::other(anyhow!("the current folder cannot be read: {err}")))?,
    };
    Ok(serve_root_of(&folder))
}

/// Whether a `shprd serve` start names a repository: `--repo`, `$SHPRD_REPO`, or
/// a working directory inside one. The window's own launch names none, and the
/// service then waits to be told its repositories rather than serving whatever
/// folder it was started in.
pub fn serve_names_a_repo(explicit: Option<&Path>) -> bool {
    if explicit.is_some() || std::env::var_os(REPO_ENV).is_some_and(|value| !value.is_empty()) {
        return true;
    }
    let Ok(folder) = std::env::current_dir() else {
        return false;
    };
    let mut command = std::process::Command::new("git");
    command
        .arg("-C")
        .arg(folder)
        .args(["rev-parse", "--show-toplevel"]);
    shprd_core::spawn::output(&mut command)
        .ok()
        .is_some_and(|output| output.status.success())
}

/// The repository a folder belongs to, else the folder itself: what the one
/// service registers for a space's folder.
pub fn serve_root_of(folder: &Path) -> PathBuf {
    let mut command = std::process::Command::new("git");
    command
        .arg("-C")
        .arg(folder)
        .args(["rev-parse", "--show-toplevel"]);
    let top = shprd_core::spawn::output(&mut command)
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|text| !text.is_empty());
    match top {
        Some(top) => run_repo(Path::new(&top)),
        None => run_repo(folder),
    }
}

/// Resolves the checkout the caller works in, without leaving a worktree.
///
/// `--repo`, then `$SHPRD_REPO`, then the git top level of the working
/// directory. A verb that writes into the checkout it was pointed at —
/// `ask install` writes a program's hook there — needs the worktree itself, so
/// it takes this rather than [`resolve_root`].
pub fn resolve_checkout(explicit: Option<&Path>) -> Result<PathBuf, CliError> {
    let named = explicit.map(|path| path.to_path_buf()).or_else(|| {
        std::env::var_os(REPO_ENV)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    });
    match named {
        Some(path) => Ok(path),
        None => git_top_level(),
    }
}

/// The repository a checkout belongs to, with a linked worktree's main worktree
/// in place of the worktree itself.
///
/// A worker is launched with `$SHPRD_REPO` naming its own worktree, and its
/// working directory is that worktree too, so both resolve there. The run's log
/// is not there: it lives in the repository the worktree was cut from, whose
/// `.git` the worktree's `.git` file points at. `git rev-parse
/// --git-common-dir` names that `.git` and one parent step is the repository, so
/// a `shprd` run from a worktree reads and writes the same run the service does.
///
/// Anything that is not a linked worktree — a main worktree, a plain directory,
/// a path git refuses — is returned as it came.
fn run_repo(checkout: &Path) -> PathBuf {
    // A main worktree holds its own `.git` directory and a plain directory holds
    // none; only a linked worktree (or a submodule, which git answers for below)
    // holds the `.git` file this has to follow.
    let git = checkout.join(".git");
    if !git.is_file() {
        return checkout.to_path_buf();
    }
    let Ok(common) = git_common_dir(checkout) else {
        return checkout.to_path_buf();
    };
    // A linked worktree's common directory is `<repo>/.git`; a submodule's is
    // `<repo>/.git/modules/<name>`, which names no repository of its own.
    match common.file_name().and_then(|name| name.to_str()) {
        Some(".git") => common
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| checkout.to_path_buf()),
        _ => checkout.to_path_buf(),
    }
}

/// Asks git for the directory a checkout's common git data lives in.
///
/// The answer is relative to the directory git ran in, which is `checkout`.
fn git_common_dir(checkout: &Path) -> Result<PathBuf, CliError> {
    let mut command = std::process::Command::new("git");
    command
        .arg("-C")
        .arg(checkout)
        .args(["rev-parse", "--git-common-dir"]);
    let output = shprd_core::spawn::output(&mut command)
        .context("running `git rev-parse --git-common-dir`")
        .map_err(CliError::other)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(CliError::other(anyhow!(
            "{} is not in a git repository: {}",
            checkout.display(),
            stderr.trim()
        )));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(CliError::other(anyhow!(
            "git reported no common directory for {}",
            checkout.display()
        )));
    }
    let common = PathBuf::from(trimmed);
    Ok(if common.is_absolute() {
        common
    } else {
        checkout.join(common)
    })
}

/// Asks git for the top level of the current working directory.
fn git_top_level() -> Result<PathBuf, CliError> {
    let mut command = std::process::Command::new("git");
    command.args(["rev-parse", "--show-toplevel"]);
    let output = shprd_core::spawn::output(&mut command)
        .context("running `git rev-parse --show-toplevel`")
        .map_err(CliError::other)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(CliError::other(anyhow!(
            "not inside a git repository: {}",
            stderr.trim()
        )));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(CliError::other(anyhow!("git reported no repository root")));
    }
    Ok(PathBuf::from(trimmed))
}

/// Resolves the home directory shprd keeps its state and global config in.
///
/// `--home` wins, then `$SHPRD_HOME`, then `$HOME`.
pub fn resolve_home(home: Option<&Path>) -> Result<PathBuf, CliError> {
    let explicit = home.map(|path| path.to_path_buf()).or_else(|| {
        std::env::var_os(HOME_ENV)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    });
    match explicit {
        Some(path) => Ok(path),
        None => std::env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| CliError::other(anyhow!("no home directory: pass --home"))),
    }
}

/// Resolves the home directory an agent CLI runs with, which is the one its
/// session record is written under.
///
/// It is `$HOME` and nothing else: shprd never rewrites `HOME` for a launch (it
/// is a protected variable, [`shprd_agents::env`]), so the CLI that a spawn
/// starts writes its record under the same `$HOME` this process has. That is a
/// different directory from the engine's home ([`resolve_home`]), which may be
/// `SHPRD_HOME` or a `--home` flag and which no CLI writes a session under, and
/// it is why a chat that looked under the engine's home never found one.
///
/// `None` is a process with no `$HOME` at all. That is not a reason to fall back
/// to the engine's home — the guess would be wrong in exactly the case this
/// resolver exists for — so a caller says it was not told instead.
pub fn resolve_agent_home() -> Option<PathBuf> {
    env_value("HOME").map(PathBuf::from)
}

/// Resolves the XDG data home an agent CLI writes under: `$XDG_DATA_HOME` when
/// it is set, else `None` and the caller's own `<home>/.local/share` rule.
pub fn resolve_agent_data_home() -> Option<PathBuf> {
    env_value(DATA_HOME_ENV).map(PathBuf::from)
}

/// One environment variable's value, with an empty one read as unset.
///
/// This is the binary's only way of reading the variables below, so the rule —
/// "the environment is read once, at the edge, and carried in as a value" —
/// holds for every command: nothing in `cmd/`, and no library, looks one up for
/// itself.
fn env_value(name: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(name).filter(|value| !value.is_empty())
}

/// Resolves the configuration root: `$XDG_CONFIG_HOME`, else `<home>/.config`.
///
/// It is where the person's prompts live, and the rule is the XDG one: a set
/// variable wins outright, and an empty one is unset. It is resolved here, once,
/// rather than in [`shprd_core::prompts`], so a test that builds its own home
/// keeps its prompts there whatever the process's own XDG settings say.
pub fn resolve_config_home(home: &Path) -> PathBuf {
    shprd_core::prompts::config_home(home, env_value(CONFIG_HOME_ENV).as_deref())
}

/// Resolves the state home: `$XDG_STATE_HOME`, else `<home>/.local/state`.
///
/// The service's discovery files and the last pass's outcome live under it. The
/// same edge rule applies: one read here, a value everywhere else.
pub fn resolve_state_home(home: &Path) -> PathBuf {
    shprd_server::state_home(home, env_value(STATE_HOME_ENV).as_deref())
}

/// Resolves the data home: `$XDG_DATA_HOME`, else `<home>/.local/share`.
pub fn resolve_data_home(home: &Path) -> PathBuf {
    shprd_agents::harness::data_home(home, env_value(DATA_HOME_ENV).as_deref())
}

/// Resolves the data shprd ships: the harness table, the model catalogs, the
/// role templates and the skills tables.
///
/// `$SHPRD_DATA_DIR` when a person named it, else their own overrides under the
/// data home (`<XDG_DATA_HOME or ~/.local/share>/shprd/`) when it holds a
/// `harness/`, else the copy embedded in the binary at build time. That last
/// answer is why an installed `shprd` needs no data directory at all.
///
/// Read here, at the edge: the variables are read once and the answer is
/// carried in as a value, so no verb and no library looks one up, and a test
/// that names its own home never finds the developer's install.
pub fn resolve_data_dir() -> DataDir {
    let explicit = env_value(shprd_agents::harness::DATA_ENV).map(PathBuf::from);
    let data_home = resolve_home(None).ok().map(|home| resolve_data_home(&home));
    DataDir::resolve(explicit.as_deref(), data_home.as_deref())
}

/// Resolves the layout shprd's git state lives in for `repo`: the settings'
/// `tasks.archive_namespace` is the branch prefix and archive namespace, and
/// `tasks.branch` names the branches.
///
/// The worktree root, the namespace and the pattern all come from here, so a
/// janitor can never be pointed at someone else's branches by a stale flag, and
/// a spawn and the janitor never disagree about what a task's branch is called.
pub fn resolve_layout(home: Option<&Path>, repo: &Path) -> Result<Layout, CliError> {
    Ok(Layout::from_settings(&resolve_home(home)?, repo))
}

/// Parses `--as`, falling back to `$SHPRD_ACTOR`, then to `human`.
pub fn resolve_actor(explicit: Option<&str>) -> Result<Actor, CliError> {
    let raw = match explicit {
        Some(value) => value.to_string(),
        None => match std::env::var(ACTOR_ENV) {
            Ok(value) if !value.trim().is_empty() => value,
            _ => return Ok(Actor::Human),
        },
    };
    parse_actor(&raw)
}

/// The spelling [`parse_actor`] reads back for `actor`.
pub fn actor_name(actor: &Actor) -> String {
    match actor {
        Actor::Engine => "engine".to_string(),
        Actor::Orchestrator => "orchestrator".to_string(),
        Actor::Human => "human".to_string(),
        Actor::Agent { id } => format!("agent:{id}"),
    }
}

/// Parses one actor spelling.
pub fn parse_actor(raw: &str) -> Result<Actor, CliError> {
    match raw.trim() {
        "engine" => Ok(Actor::Engine),
        "orchestrator" => Ok(Actor::Orchestrator),
        "human" => Ok(Actor::Human),
        other => match other.strip_prefix("agent:") {
            Some(id) if !id.is_empty() => Ok(Actor::Agent { id: id.to_string() }),
            _ => Err(CliError::other(anyhow!(
                "unknown actor {other:?}: expected engine, orchestrator, human or agent:<ID>"
            ))),
        },
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::path::{Path, PathBuf};
    use std::process::Command;

    use tempfile::TempDir;

    use super::run_repo;

    /// Runs git in `dir`, asserting it succeeded, and returns its stdout.
    fn git(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git runs");
        assert!(
            output.status.success(),
            "git {args:?} in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    /// Whether git is on this machine; without it these tests have nothing to
    /// ask, and a skip is honest.
    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false)
    }

    /// A repository with one commit, and the temp directory that keeps it.
    fn repo() -> Option<(TempDir, PathBuf)> {
        if !git_available() {
            eprintln!("skipped: git is not on PATH");
            return None;
        }
        let temp = TempDir::new().expect("tempdir");
        let root = temp.path().join("repo");
        std::fs::create_dir_all(&root).expect("mkdir");
        git(&root, &["init", "-b", "main"]);
        git(&root, &["config", "user.name", "shprd tests"]);
        git(&root, &["config", "user.email", "tests@example.invalid"]);
        git(&root, &["config", "commit.gpgsign", "false"]);
        std::fs::write(root.join("a.txt"), "one\n").expect("write");
        git(&root, &["add", "--", "a.txt"]);
        git(&root, &["commit", "-m", "seed"]);
        Some((temp, root))
    }

    #[test]
    fn a_worktree_resolves_to_the_repository_it_was_cut_from() {
        let Some((temp, root)) = repo() else {
            return;
        };
        let worktree = temp.path().join("wt").join("T1");
        git(
            &root,
            &[
                "worktree",
                "add",
                &worktree.display().to_string(),
                "-b",
                "shprd/r1/T1",
            ],
        );
        assert!(
            worktree.join(".git").is_file(),
            "a linked worktree's .git is a file"
        );

        // git answers with the canonical path, so a `$TMPDIR` that is a symlink
        // (`/var` on macOS) comes back resolved.
        let expected = std::fs::canonicalize(&root).expect("canonicalize the repo");
        assert_eq!(run_repo(&worktree), expected);
    }

    #[test]
    fn a_main_worktree_resolves_to_itself() {
        let Some((_temp, root)) = repo() else {
            return;
        };
        assert_eq!(run_repo(&root), root);
    }

    #[test]
    fn a_directory_that_is_not_a_worktree_resolves_to_itself() {
        let temp = TempDir::new().expect("tempdir");
        assert_eq!(run_repo(temp.path()), temp.path());
    }
}
