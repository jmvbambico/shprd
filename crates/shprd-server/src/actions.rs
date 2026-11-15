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

use shprd_core::ask::{Ask, AskError, AskQueue};
use shprd_core::event::{Actor, EventKind, status_line};
use shprd_core::log::{Clock, SystemClock};

use crate::dto::{Action, ActionReply, AskDto, AskListDto, AskReply, TaskDto};
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

/// Every ask still open for the run's tasks at `now_ms`, oldest first.
pub(crate) fn list_asks(repo: &Path, now_ms: u64) -> Result<AskListDto, ApiReject> {
    let (state, _) = ledger::load(repo).map_err(read_error)?;
    let mut asks: Vec<&shprd_core::state::AskRecord> = state
        .asks
        .values()
        .filter(|ask| ask.is_open_at(now_ms))
        .collect();
    asks.sort_by(|left, right| {
        left.created_at_ms
            .cmp(&right.created_at_ms)
            .then_with(|| left.task.cmp(&right.task))
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(AskListDto {
        asks: asks.into_iter().map(snapshot::ask).collect(),
    })
}

/// Answers one ask, exactly once, and records it in the log.
///
/// The queue file is claimed first: its atomic create-new answer is what makes
/// two cards racing for the same ask leave one winner. The event is appended
/// second, under the log's own lock, and if a rule refuses it the claimed
/// answer is put back so the card can be tried again.
pub(crate) fn answer_ask(
    repo: &Path,
    actor: &Actor,
    hub: &Hub,
    after_write: Option<&AfterWrite>,
    task: &str,
    ask: &str,
    answer: &str,
) -> Result<AskReply, ApiReject> {
    if answer.trim().is_empty() {
        return Err(ApiReject::bad_request(
            "an answer needs at least one character",
        ));
    }
    let queue = AskQueue::for_repo(repo);
    let now_ms = SystemClock.now_ms();
    let answered: Ask = queue.answer(task, ask, answer, now_ms).map_err(ask_error)?;
    let kind = EventKind::AskAnswered {
        task: task.to_string(),
        ask: ask.to_string(),
        answer: answer.to_string(),
    };
    let event = match ledger::commit(repo, actor, kind) {
        Ok(event) => event,
        Err(err) => {
            // Nothing was written: free the ask for another attempt.
            if let Ok(path) = queue.answer_path(task, ask) {
                let _ = std::fs::remove_file(path);
            }
            return Err(commit_error(err));
        }
    };
    hub.publish(event.seq);
    if let Some(step) = after_write {
        step.call(repo);
    }
    let (state, _) = ledger::load(repo).map_err(read_error)?;
    let asked: AskDto = state
        .asks
        .get(&(task.to_string(), ask.to_string()))
        .map(snapshot::ask)
        .unwrap_or_else(|| AskDto {
            id: answered.id,
            task: answered.task,
            kind: answered.kind.into(),
            text: answered.text,
            options: answered.options,
            created_at_ms: answered.created_at_ms,
            expires_at_ms: answered.expires_at_ms,
        });
    Ok(AskReply {
        ok: true,
        ask: asked,
    })
}

/// Maps an ask-queue failure to the status a client sees.
fn ask_error(err: AskError) -> ApiReject {
    match &err {
        AskError::UnknownAsk { .. } => ApiReject::new(
            axum::http::StatusCode::NOT_FOUND,
            crate::dto::ErrorCode::NotFound,
            err.to_string(),
        ),
        AskError::AlreadyAnswered { .. } | AskError::Expired { .. } => {
            ApiReject::refused(err.to_string())
        }
        AskError::InvalidName { .. } => ApiReject::bad_request(err.to_string()),
        AskError::Io { .. } | AskError::Parse { .. } | AskError::AlreadyRaised { .. } => {
            ApiReject::internal(err.to_string())
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
    let now_ms = SystemClock.now_ms();
    let folded: Option<TaskDto> = task
        .as_deref()
        .and_then(|task| state.tasks.get(task))
        .map(|task| snapshot::task_of(&state, task, now_ms));
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
