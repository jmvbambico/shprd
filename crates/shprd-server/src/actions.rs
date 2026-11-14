//! What `POST /api/action` does, in one place.
//!
//! The handlers parse one [`Action`] and hand it here; every arm goes through
//! the same functions the CLI writes with — [`crate::ledger::commit`] checks the
//! transition rules and appends under the log's lock, `shprd_core::event::status_line`
//! caps a progress line exactly as `shprd task status` does, and the fold is
//! `RunState::fold` — so a web client can never write a state the CLI would
//! refuse, and no verb is implemented twice. After a write the binary's own
//! [`crate::service::AfterWrite`] step runs (the CLI's dagr projection), so the
//! two write paths also leave the same files behind.

use std::path::Path;

use shprd_core::event::{Actor, EventKind, status_line};

use crate::dto::{Action, ActionReply, TaskDto};
use crate::error::ApiReject;
use crate::hub::Hub;
use crate::ledger::{self, CommitError, ReadError};
use crate::service::AfterWrite;
use crate::snapshot;

/// Runs one action against a repository's log.
///
/// It blocks (a fold and an fsync), so callers run it on a blocking thread.
pub(crate) fn run(
    repo: &Path,
    actor: &Actor,
    hub: &Hub,
    after_write: Option<&AfterWrite>,
    action: Action,
) -> Result<ActionReply, ApiReject> {
    match action {
        Action::TaskAdd {
            task,
            title,
            scope,
            acceptance,
            tags,
            skills,
            deps,
        } => {
            let kind = EventKind::TaskAdded {
                task,
                title,
                scope,
                acceptance,
                tags,
                skills,
                deps,
            };
            commit(repo, actor, hub, after_write, kind)
        }
        Action::TaskStatus {
            task,
            msg,
            progress,
        } => {
            if msg.trim().is_empty() {
                return Err(ApiReject::bad_request(
                    "a task.status action needs a non-empty \"msg\"",
                ));
            }
            if let Some(progress) = progress
                && progress > 100
            {
                return Err(ApiReject::bad_request(format!(
                    "progress is a percentage from 0 to 100, not {progress}"
                )));
            }
            let kind = EventKind::TaskStatus {
                task,
                message: status_line(&msg),
                progress,
            };
            commit(repo, actor, hub, after_write, kind)
        }
    }
}

/// Appends one event and answers with the task it was about, folded again.
///
/// Every arm above ends here, so a new action cannot forget the rule check, the
/// wake-up, the binary's after-write step, or the fold the reply carries.
fn commit(
    repo: &Path,
    actor: &Actor,
    hub: &Hub,
    after_write: Option<&AfterWrite>,
    kind: EventKind,
) -> Result<ActionReply, ApiReject> {
    let task = event_task(&kind);
    let event = ledger::commit(repo, actor, kind).map_err(commit_error)?;
    hub.publish(event.seq);
    if let Some(step) = after_write {
        step.call(repo);
    }
    let (state, _) = ledger::load(repo).map_err(read_error)?;
    let folded: Option<TaskDto> = task
        .as_deref()
        .and_then(|task| state.tasks.get(task))
        .map(snapshot::task);
    Ok(ActionReply {
        ok: true,
        seq: event.seq,
        task: folded,
    })
}

/// The task an event is about, when it names one.
fn event_task(kind: &EventKind) -> Option<String> {
    match kind {
        EventKind::TaskAdded { task, .. } | EventKind::TaskStatus { task, .. } => {
            Some(task.clone())
        }
        _ => None,
    }
}

/// Maps an append failure to the status a client sees.
fn commit_error(err: CommitError) -> ApiReject {
    match err {
        // A rule refused it, so nothing was written: the client may fix the
        // request, and a retry of the same one will fail the same way.
        CommitError::Refused(rule) => ApiReject::refused(rule.to_string()),
        CommitError::Read(read) => read_error(read),
        CommitError::Log(log) => ApiReject::from(log),
    }
}

/// Maps a read or fold failure. A log that does not fold is the service's
/// problem, not the client's.
fn read_error(err: ReadError) -> ApiReject {
    ApiReject::internal(err.to_string())
}
