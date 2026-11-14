//! What `POST /api/action` does, in one place.
//!
//! The handlers parse one [`Action`] and hand it here; every arm goes through
//! the same functions the CLI writes with — [`crate::ledger::commit`] checks the
//! transition rules and appends under the log's lock, and the fold is
//! `RunState::fold` — so a web client can never write a state the CLI would
//! refuse, and no verb is implemented twice.

use std::path::Path;

use shprd_core::event::{Actor, EventKind};

use crate::dto::{Action, ActionReply};
use crate::error::ApiReject;
use crate::hub::Hub;
use crate::ledger::{self, CommitError, ReadError};
use crate::snapshot;

/// Runs one action against a repository's log.
///
/// It blocks (a fold and an fsync), so callers run it on a blocking thread.
pub(crate) fn run(
    repo: &Path,
    actor: &Actor,
    hub: &Hub,
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
                task: task.clone(),
                title,
                scope,
                acceptance,
                tags,
                skills,
                deps,
            };
            let event = ledger::commit(repo, actor, kind).map_err(commit_error)?;
            hub.publish(event.seq);
            let (state, _) = ledger::load(repo).map_err(read_error)?;
            let folded = state
                .tasks
                .get(&task)
                .map(snapshot::task)
                .ok_or_else(|| ApiReject::unknown_task(&task))?;
            Ok(ActionReply {
                ok: true,
                seq: event.seq,
                task: Some(folded),
            })
        }
        Action::TaskStatus { task } => {
            let (state, _) = ledger::load(repo).map_err(read_error)?;
            let folded = state
                .tasks
                .get(&task)
                .map(snapshot::task)
                .ok_or_else(|| ApiReject::unknown_task(&task))?;
            Ok(ActionReply {
                ok: true,
                seq: state.last_seq,
                task: Some(folded),
            })
        }
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
