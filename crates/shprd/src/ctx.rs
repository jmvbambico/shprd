//! Shared plumbing: repo root, log path, actor, and the check-then-append path
//! every writing verb goes through.

use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow};
use shprd_core::event::{Actor, Event, EventKind};
use shprd_core::log::{Clock, EventLog, SystemClock};
use shprd_core::rules::RuleError;
use shprd_core::state::RunState;

/// Environment variable holding the default actor.
const ACTOR_ENV: &str = "SHPRD_ACTOR";

/// Exit code for a command a transition rule refused.
pub const EXIT_REFUSED: u8 = 2;

/// Exit code for any other failure.
pub const EXIT_ERROR: u8 = 1;

/// A command's failure: a rule refusal or anything else.
#[derive(Debug)]
pub enum CliError {
    /// A transition rule refused the event; nothing was written.
    Refused(RuleError),
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
            Self::Refused(_) => EXIT_REFUSED,
            Self::Other(_) => EXIT_ERROR,
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
}

impl Ctx {
    /// Builds a context from the resolved root and the global options.
    pub fn new(root: PathBuf, actor: Actor, json: bool) -> Self {
        let log_path = root.join(".shprd").join("events.jsonl");
        Self {
            root,
            log_path,
            actor,
            json,
        }
    }

    /// The folded state of the run, or the empty state when no log exists yet.
    pub fn state(&self) -> Result<RunState, CliError> {
        if !self.log_path.exists() {
            return Ok(RunState::default());
        }
        let outcome = EventLog::read_all(&self.log_path)
            .map_err(|err| CliError::other(anyhow!(err).context("reading the event log")))?;
        RunState::fold(&outcome.events)
            .map_err(|err| CliError::other(anyhow!(err).context("folding the event log")))
    }

    /// Checks `kind` against the current state, then appends it.
    ///
    /// A refused event is never written. Returns the appended event.
    pub fn commit(&self, kind: EventKind) -> Result<Event, CliError> {
        let state = self.state()?;
        let clock = SystemClock;
        state
            .check(&kind, clock.now_ms())
            .map_err(CliError::Refused)?;
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
