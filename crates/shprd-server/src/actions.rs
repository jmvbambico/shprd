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
//!
//! Three of the verbs are not an event alone: starting an attempt, starting
//! another one and stopping an agent are the binary's spawn path and the host's
//! close, passed in as an [`Engine`], and the stop also appends the
//! [`EventKind::TaskStopped`] the fold needs to know the attempt has no agent
//! any more. A service without an engine refuses them, which is what a test that
//! only drives the router sees.

use std::path::Path;

use shprd_core::config::MergePolicy;
use shprd_core::event::{Actor, EventKind, status_line};
use shprd_core::log::{Clock, SystemClock};
use shprd_core::state::Task;
use shprd_git::git::Git;

use crate::dto::{Action, ActionReply, AskDto, AskListDto, AskReply, TaskDto};
use crate::engine::Engine;
use crate::error::ApiReject;
use crate::hub::Hub;
use crate::ledger::{self, CommitError, ReadError};
use crate::service::AfterWrite;
use crate::snapshot;

/// Everything one action runs against.
///
/// It is a struct rather than seven arguments because every arm needs the same
/// seven, and an arm that forgot one would be a verb that quietly wrote less
/// than the CLI does.
pub(crate) struct Context<'a> {
    /// The repository whose run log is written.
    pub(crate) repo: &'a Path,
    /// Who the action acts as: the person who started the service.
    pub(crate) actor: &'a Actor,
    /// The service's change counter, so a write wakes every waiting client.
    pub(crate) hub: &'a Hub,
    /// The binary's step after a write, when it supplied one.
    pub(crate) after_write: Option<&'a AfterWrite>,
    /// The binary's spawn and close, when this service has a host.
    pub(crate) engine: Option<&'a Engine>,
    /// The merge policy this service resolved from the settings, when it could.
    ///
    /// `None` means "not known here": the service was started without a home
    /// directory, so accepting is left to the rules alone rather than refused
    /// for a reason nobody could check.
    pub(crate) merge: Option<MergePolicy>,
}

/// Runs one action against a repository's log.
///
/// It blocks (a fold and an fsync), so callers run it on a blocking thread.
pub(crate) fn run(ctx: &Context<'_>, action: Action) -> Result<ActionReply, ApiReject> {
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
            commit(ctx, kind)
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
            commit(ctx, kind)
        }
        Action::TaskStart { task } => spawn(ctx, &task, "start"),
        Action::TaskRetry { task } => spawn(ctx, &task, "retry"),
        Action::TaskStop { task } => stop(ctx, &task),
        Action::TaskDrop { task, reason } => {
            let kind = EventKind::TaskDropped { task, reason };
            commit(ctx, kind)
        }
        Action::TaskAccept { task } => accept(ctx, &task),
        Action::TaskResendBrief { task } => resend_brief(ctx, &task),
    }
}

/// Delivers a task's latest brief to its agent again and records that it went.
///
/// The record is a progress line, which is what retires a `brief not delivered`
/// card (`shprd_views::cards::card`): the card clears only once the delivery
/// succeeded, so a refusal leaves it standing with its button.
fn resend_brief(ctx: &Context<'_>, task: &str) -> Result<ActionReply, ApiReject> {
    let engine = ctx.engine.ok_or_else(|| {
        ApiReject::refused("this service was started without a host, so it cannot deliver a brief")
    })?;
    let (folded, _) = ledger::load(ctx.repo).map_err(read_error)?;
    if !folded.tasks.contains_key(task) {
        return Err(ApiReject::unknown_task(task));
    }
    engine.resend(ctx.repo, task).map_err(ApiReject::refused)?;
    let kind = EventKind::TaskStatus {
        task: task.to_string(),
        message: status_line("brief resent"),
        progress: None,
    };
    commit(ctx, kind)
}

/// Starts an attempt at `task` through the binary's spawn path.
///
/// The transition rules are the spawn's own: they refuse a task that already
/// landed, one whose dependencies have not landed, one another task's scope
/// overlaps and a second attempt while the first is unfinished. Nothing is
/// appended here — the spawn appends its own `spawned` event — so the reply is
/// the task as the log now reads it.
fn spawn(ctx: &Context<'_>, task: &str, verb: &str) -> Result<ActionReply, ApiReject> {
    let engine = ctx.engine.ok_or_else(|| {
        ApiReject::refused(format!(
            "this service was started without a host, so it cannot {verb} a task's agent"
        ))
    })?;
    engine.start(ctx.repo, task).map_err(ApiReject::refused)?;
    let seq = ledger::last_seq(ctx.repo)
        .map_err(|err| ApiReject::internal(format!("could not read the run log: {err}")))?;
    ctx.hub.publish(seq);
    folded_task(ctx.repo, task, seq)
}

/// Stops whatever is running on `task`: closes its space on the host, then
/// records that the attempt's agent is gone.
///
/// Closing the space is only half of it. The host forgets a closed space, so a
/// stop that wrote nothing left the fold saying `working` for ever and every
/// retry refused (M10 gate bug 1). The event is what makes the task read
/// *Needs you · Agent stopped* and what makes `task.retry` allowed (Shepherdr's
/// rule, M7a §5). A task that already handed in keeps its status: the hand-in is
/// still the engine's to review and land, and closing its tab takes nothing away
/// from it.
fn stop(ctx: &Context<'_>, task: &str) -> Result<ActionReply, ApiReject> {
    let engine = ctx.engine.ok_or_else(|| {
        ApiReject::refused(
            "this service was started without a host, so it cannot stop a task's agent",
        )
    })?;
    // A stop names a task the run holds: closing the space of a task nobody
    // added would be closing whatever else happens to be labelled that way.
    let (folded, _) = ledger::load(ctx.repo).map_err(read_error)?;
    let found = folded
        .tasks
        .get(task)
        .ok_or_else(|| ApiReject::unknown_task(task))?;
    let attempt = found
        .latest_attempt()
        .map(|attempt| attempt.n)
        .ok_or_else(|| {
            ApiReject::refused(format!(
                "{task} has no agent to stop: nothing has been started on it"
            ))
        })?;
    // The rules are asked before the host is: a stop they would refuse must not
    // close a space first and then leave the log saying the agent is still
    // working.
    let kind = EventKind::TaskStopped {
        task: task.to_string(),
        attempt,
    };
    folded
        .check_as(ctx.actor, &kind, SystemClock.now_ms())
        .map_err(|err| ApiReject::refused(err.to_string()))?;
    engine.stop(ctx.repo, task).map_err(ApiReject::refused)?;
    commit(ctx, kind)
}

/// Accepts a task's work at the head its branch is at, as `shprd approve` does.
///
/// The approval is pinned to a commit: what the person read is what they agreed
/// to, so a branch that moved after it has to be accepted again. Where
/// `tasks.merge` is not `ask`, nothing waits for an approval at all — the task
/// lands on its own — and recording one would be a decision with no effect, so
/// the action is refused instead.
fn accept(ctx: &Context<'_>, task: &str) -> Result<ActionReply, ApiReject> {
    if let Some(policy) = ctx.merge
        && policy != MergePolicy::Ask
    {
        let word = shprd_core::settings::merge_name(policy);
        return Err(ApiReject::refused(format!(
            "nothing waits for an acceptance: tasks.merge is {word:?}, so {task} lands on its own"
        )));
    }
    let (folded, _) = ledger::load(ctx.repo).map_err(read_error)?;
    let found = folded
        .tasks
        .get(task)
        .ok_or_else(|| ApiReject::unknown_task(task))?;
    let number = found.pr.as_ref().map(|pr| pr.number).unwrap_or(0);
    let head = head_of(ctx.repo, found)?;
    let kind = EventKind::TaskApproved {
        task: task.to_string(),
        number,
        head_sha: head,
    };
    commit(ctx, kind)
}

/// The commit a task's branch is at, which is what an acceptance names.
///
/// The attempt's worktree is asked first, exactly as `shprd approve` asks it, so
/// the approval names the commit landing reads; a worktree the janitor already
/// removed falls back to the branch in the repository. A branch nobody can read
/// is refused with the path that was tried, because that is the thing to look
/// at.
fn head_of(repo: &Path, task: &Task) -> Result<String, ApiReject> {
    let attempt = task.latest_attempt().ok_or_else(|| {
        ApiReject::refused(format!(
            "{} has no attempt to accept: nothing has been written on its branch",
            task.id
        ))
    })?;
    let mut tried = Vec::new();
    for place in [Some(Path::new(&attempt.worktree)), Some(repo)] {
        let Some(dir) = place else { continue };
        if !dir.is_dir() {
            tried.push(format!("{} (missing)", dir.display()));
            continue;
        }
        let head = if dir == repo {
            Git::new(dir).rev_parse(&attempt.branch)
        } else {
            Git::new(dir).head_sha()
        };
        match head {
            Ok(head) if !head.trim().is_empty() => return Ok(head.trim().to_string()),
            Ok(_) => tried.push(format!("{} (no commit)", dir.display())),
            Err(err) => tried.push(format!("{} ({err})", dir.display())),
        }
    }
    Err(ApiReject::refused(format!(
        "could not read the head of {}'s branch {}: {}",
        task.id,
        attempt.branch,
        tried.join(", ")
    )))
}

/// Every ask still open for the run's tasks at `now_ms`, oldest first.
pub(crate) fn list_asks(repo: &Path, now_ms: u64) -> Result<AskListDto, ApiReject> {
    let (state, _) = ledger::load(repo).map_err(read_error)?;
    let mut asks: Vec<&shprd_core::state::AskRecord> = state
        .asks
        .values()
        .filter(|ask| ask.is_open_at(now_ms))
        .filter(|ask| {
            state
                .tasks
                .get(&ask.task)
                .is_none_or(|task| !ask.is_retired_by_newer_attempt(task))
        })
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
    ctx: &Context<'_>,
    task: &str,
    ask: &str,
    answer: &str,
) -> Result<AskReply, ApiReject> {
    if answer.trim().is_empty() {
        return Err(ApiReject::bad_request(
            "an answer needs at least one character",
        ));
    }
    // A spent review's card acts on its answer, and the rules are asked before
    // anything is written: an answer whose consequence they would refuse (a
    // task already dropped, say) must not be consumed and then do nothing.
    let (before, _) = ledger::load(ctx.repo).map_err(read_error)?;
    let followup = before
        .tasks
        .get(task)
        .and_then(|found| shprd_core::gate::rounds_spent_followup(found, ask, answer));
    if let Some(kind) = &followup {
        before
            .check_as(ctx.actor, kind, SystemClock.now_ms())
            .map_err(|err| ApiReject::refused(err.to_string()))?;
    }
    // A refused hand-in's card widens the scope or sends the task back. Both
    // are checked before the answer is consumed, for the same reason.
    let hand_in = shprd_core::gate::hand_in_refused_choice(ask, answer);
    let widening = match &hand_in {
        Some(shprd_core::gate::HandInChoice::Widen(paths)) => {
            let kind = EventKind::ScopeWidened {
                task: task.to_string(),
                paths: paths.clone(),
            };
            before
                .check_as(ctx.actor, &kind, SystemClock.now_ms())
                .map_err(|err| ApiReject::refused(err.to_string()))?;
            Some(kind)
        }
        Some(shprd_core::gate::HandInChoice::SendBack) => {
            if ctx.engine.is_none() {
                return Err(ApiReject::refused(
                    "this service was started without a host, so it cannot start another attempt",
                ));
            }
            None
        }
        None => None,
    };
    let queue = shprd_core::ask::AskQueue::for_repo(ctx.repo);
    let now_ms = SystemClock.now_ms();
    let answered: shprd_core::ask::Ask =
        queue.answer(task, ask, answer, now_ms).map_err(ask_error)?;
    let kind = EventKind::AskAnswered {
        task: task.to_string(),
        ask: ask.to_string(),
        answer: answer.to_string(),
    };
    let event = match ledger::commit(ctx.repo, ctx.actor, kind) {
        Ok(event) => event,
        Err(err) => {
            // Nothing was written: free the ask for another attempt.
            if let Ok(path) = queue.answer_path(task, ask) {
                let _ = std::fs::remove_file(path);
            }
            return Err(commit_error(err));
        }
    };
    ctx.hub.publish(event.seq);
    // The card's answer is the person's confirmation, so what it chose is
    // recorded as theirs: the override `take it as it is` means (the tick lands
    // the task on its own policy afterwards) or the drop.
    if let Some(kind) = followup {
        let acted = ledger::commit(ctx.repo, ctx.actor, kind).map_err(commit_error)?;
        ctx.hub.publish(acted.seq);
    }
    if let Some(kind) = widening {
        // The scope is wider now; the worker is told to hand in again, which
        // is the brief it was given with the new scope. A host that cannot
        // deliver it leaves the line below to say what is waiting.
        let widened = ledger::commit(ctx.repo, ctx.actor, kind).map_err(commit_error)?;
        ctx.hub.publish(widened.seq);
        let note = EventKind::TaskStatus {
            task: task.to_string(),
            message: status_line("scope widened: hand in again"),
            progress: None,
        };
        commit(ctx, note)?;
        if let Some(engine) = ctx.engine {
            let _ = engine.resend(ctx.repo, task);
        }
    }
    if matches!(hand_in, Some(shprd_core::gate::HandInChoice::SendBack)) {
        // The same two verbs the Stop and Retry buttons run.
        stop(ctx, task)?;
        spawn(ctx, task, "retry")?;
    }
    if let Some(step) = ctx.after_write {
        step.call(ctx.repo);
    }
    let (state, _) = ledger::load(ctx.repo).map_err(read_error)?;
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
fn ask_error(err: shprd_core::ask::AskError) -> ApiReject {
    use shprd_core::ask::AskError;
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
pub(crate) fn commit(ctx: &Context<'_>, kind: EventKind) -> Result<ActionReply, ApiReject> {
    let task = event_task(&kind);
    let event = ledger::commit(ctx.repo, ctx.actor, kind).map_err(commit_error)?;
    ctx.hub.publish(event.seq);
    if let Some(step) = ctx.after_write {
        step.call(ctx.repo);
    }
    match task {
        Some(task) => folded_task(ctx.repo, &task, event.seq),
        None => Ok(ActionReply {
            ok: true,
            seq: event.seq,
            task: None,
        }),
    }
}

/// The task an event is about, when it names one.
fn event_task(kind: &EventKind) -> Option<String> {
    match kind {
        EventKind::TaskAdded { task, .. }
        | EventKind::TaskStatus { task, .. }
        | EventKind::TaskDropped { task, .. }
        | EventKind::TaskStopped { task, .. }
        | EventKind::TaskApproved { task, .. } => Some(task.clone()),
        _ => None,
    }
}

/// One reply: the log's sequence, and the task it names as the log now reads it.
fn folded_task(repo: &Path, task: &str, seq: u64) -> Result<ActionReply, ApiReject> {
    let (folded, _) = ledger::load(repo).map_err(read_error)?;
    let now_ms = SystemClock.now_ms();
    let folded: Option<TaskDto> = folded
        .tasks
        .get(task)
        .map(|found| snapshot::task_of(&folded, found, now_ms));
    Ok(ActionReply {
        ok: true,
        seq,
        task: folded,
    })
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
