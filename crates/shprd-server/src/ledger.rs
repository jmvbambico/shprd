//! The run's event log, as the service reads and appends it.
//!
//! The log and the fold belong to `shprd-core`, and so do the transition rules:
//! this module only points them at a repository, so the service writes exactly
//! what `shprd agent task add` writes and reads exactly what the CLI reads.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use shprd_core::event::{Actor, Event, EventKind};
use shprd_core::log::{AppendError, Clock, EventLog, LogError, SystemClock};
use shprd_core::rules::RuleError;
use shprd_core::state::RunState;

use crate::dto::EventNoticeDto;

/// Where a repository keeps its run log.
pub fn log_path(repo: &Path) -> PathBuf {
    repo.join(".shprd").join("events.jsonl")
}

/// A failure reading or folding the log.
#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    /// The log file could not be read.
    #[error(transparent)]
    Log(#[from] LogError),
    /// The log does not fold under the transition rules.
    #[error("the event log does not fold: {0}")]
    Fold(#[from] RuleError),
}

/// A failure appending an event.
#[derive(Debug, thiserror::Error)]
pub enum CommitError {
    /// A transition rule refused the event; nothing was written.
    #[error(transparent)]
    Refused(#[from] RuleError),
    /// The log could not be read.
    #[error(transparent)]
    Read(#[from] ReadError),
    /// The log could not be written.
    #[error(transparent)]
    Log(#[from] LogError),
}

/// What the file looked like the last time it was read.
///
/// A long poll re-reads the log only when this changes, so a client waiting for
/// a change costs a `stat` per tick rather than a parse of the whole log.
pub type Stamp = (u64, Option<SystemTime>);

/// The current stamp of a repository's log, or `None` before the first event.
pub fn stamp(repo: &Path) -> std::io::Result<Option<Stamp>> {
    match std::fs::metadata(log_path(repo)) {
        Ok(meta) => Ok(Some((meta.len(), meta.modified().ok()))),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

/// Folds a repository's log, and returns the events it was folded from.
///
/// A repository with no log yet is an empty run, not an error: a service can be
/// started before the first task is added.
pub fn load(repo: &Path) -> Result<(RunState, Vec<Event>), ReadError> {
    let path = log_path(repo);
    if !path.exists() {
        return Ok((RunState::default(), Vec::new()));
    }
    let outcome = EventLog::read_all(&path)?;
    let state = RunState::fold(&outcome.events)?;
    Ok((state, outcome.events))
}

/// The sequence of the log's last event, or 0 before the first.
pub fn last_seq(repo: &Path) -> Result<u64, ReadError> {
    Ok(load(repo)?.0.last_seq)
}

/// The most events one change answer carries.
///
/// Why the cap exists: a client that starts at `since=0` on a long run would
/// otherwise get the whole log in one body. It answers in chunks instead, each
/// naming the sequence it reaches, so the next call carries on from there.
pub const MAX_CHANGE_EVENTS: usize = 200;

/// Every event after `since`, in log order, up to [`MAX_CHANGE_EVENTS`].
///
/// A `since` past the end is an empty answer, not an error: a client that is
/// ahead of the log (a restarted run, a stale file) simply waits.
pub fn read_since(repo: &Path, since: u64) -> Result<Vec<Event>, ReadError> {
    let (_, events) = load(repo)?;
    Ok(events
        .into_iter()
        .filter(|event| event.seq > since)
        .take(MAX_CHANGE_EVENTS)
        .collect())
}

/// Turns events into the notices a change notification carries.
pub fn notices(events: &[Event]) -> Result<Vec<EventNoticeDto>, serde_json::Error> {
    events
        .iter()
        .map(|event| {
            // The event is internally tagged, so its own JSON already names the
            // kind and, on the kinds that have one, the task: no second table of
            // variant names to drift from the log.
            let value = serde_json::to_value(event)?;
            Ok(EventNoticeDto {
                seq: event.seq,
                at_ms: event.at_ms,
                kind: value
                    .get("kind")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
                task: value
                    .get("task")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
            })
        })
        .collect()
}

/// Checks `kind` against the folded state, then appends it, all under the
/// log's lock.
///
/// The check and the write used to be separate, so two writers that both saw
/// the same state could both append an event only one of which the rules allow
/// (two `ask_raised` for one ask, for instance), leaving a log that no reader
/// can fold. [`EventLog::append_checked`] holds the lock across both, so the
/// second writer sees the first's event and is refused. Every writing verb in
/// the CLI goes through the same sequence.
pub fn commit(repo: &Path, actor: &Actor, kind: EventKind) -> Result<Event, CommitError> {
    // Check once against the log as it stands, so a refused write leaves no
    // log file behind, and again under the log's lock below, so two writers
    // cannot both pass a check against the same state.
    let clock = SystemClock;
    let state = load(repo)?.0;
    state.check_as(actor, &kind, clock.now_ms())?;
    let mut log = EventLog::open(&log_path(repo))?;
    let checked = kind.clone();
    let check_actor = actor.clone();
    let event = log
        .append_checked(actor.clone(), kind, &clock, move |events| {
            let state = RunState::fold(events)?;
            state.check_as(&check_actor, &checked, SystemClock.now_ms())
        })
        .map_err(|err| match err {
            AppendError::Refused(refused) => CommitError::Refused(refused),
            AppendError::Log(err) => CommitError::Log(err),
        })?;
    Ok(event)
}
