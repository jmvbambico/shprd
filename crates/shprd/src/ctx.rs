//! Shared plumbing: repo root, log path, actor, and the check-then-append path
//! every writing verb goes through.

use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow};
use shprd_core::event::{Actor, Event, EventKind};
use shprd_core::log::{Clock, EventLog, SystemClock};
use shprd_core::rules::RuleError;
use shprd_core::state::RunState;
use shprd_git::worktree::Layout;

/// Environment variable holding the default actor.
const ACTOR_ENV: &str = "SHPRD_ACTOR";

/// Environment variable overriding the home directory shprd keeps state in.
const HOME_ENV: &str = "SHPRD_HOME";

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
        }
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
            .check(kind, SystemClock.now_ms())
            .map_err(CliError::Refused)
    }

    /// Checks `kind` against the current state, then appends it.
    ///
    /// A refused event is never written. Returns the appended event.
    pub fn commit(&self, kind: EventKind) -> Result<Event, CliError> {
        self.check(&kind)?;
        let clock = SystemClock;
        let mut log = EventLog::open(&self.log_path)
            .map_err(|err| CliError::other(anyhow!(err).context("opening the event log")))?;
        let event = log
            .append(self.actor.clone(), kind, &clock)
            .map_err(|err| CliError::other(anyhow!(err).context("appending to the event log")))?;
        Ok(event)
    }
}

/// Resolves the repo root: the explicit flag, else the git top level.
pub fn resolve_root(explicit: Option<&Path>) -> Result<PathBuf, CliError> {
    match explicit {
        Some(path) => Ok(path.to_path_buf()),
        None => git_top_level(),
    }
}

/// Asks git for the top level of the current working directory.
fn git_top_level() -> Result<PathBuf, CliError> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
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

/// Resolves the layout shprd's git state lives in, with `namespace` as the
/// branch prefix and archive namespace.
///
/// The worktree root and the namespace both come from here, so a janitor can
/// never be pointed at someone else's branches by a stale flag.
pub fn resolve_layout(home: Option<&Path>, namespace: &str) -> Result<Layout, CliError> {
    let mut layout = Layout::default_for_home(resolve_home(home)?);
    layout.namespace = namespace.to_string();
    Ok(layout)
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
