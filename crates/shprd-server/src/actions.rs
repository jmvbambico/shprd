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
use shprd_core::event::{Actor, EventKind, ResourceAction, ResourceKind, status_line};
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
            ensure_run(ctx)?;
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
        Action::TaskReopen { task } => reopen(ctx, &task),
        Action::TaskDrop { task, reason } => {
            let kind = EventKind::TaskDropped { task, reason };
            commit(ctx, kind)
        }
        Action::TaskDropMany { tasks, reason } => drop_many(ctx, tasks, reason),
        Action::TaskAccept { task } => accept(ctx, &task),
        Action::TaskResendBrief { task } => resend_brief(ctx, &task),
        Action::TaskReviewAgain { task } => review_again(ctx, &task),
        Action::NeedDismiss { seq } => commit(ctx, EventKind::NeedDismissed { need: seq }),
    }
}

/// Drops every task in `tasks` with one reason, skipping the ones the rules
/// refuse (already landed or dropped, or not on the board).
///
/// Each drop is its own event through [`commit`], so the log reads exactly as if
/// the person had dropped them one at a time. When none could be dropped the
/// whole action is refused with the first reason, so a button never reports
/// success for nothing.
fn drop_many(
    ctx: &Context<'_>,
    tasks: Vec<String>,
    reason: String,
) -> Result<ActionReply, ApiReject> {
    if tasks.is_empty() {
        return Err(ApiReject::bad_request(
            "a task.drop_many action needs at least one task",
        ));
    }
    if reason.trim().is_empty() {
        return Err(ApiReject::refused(
            "dropping tasks needs a reason: say why they are not wanted",
        ));
    }
    let mut dropped: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut seq = 0;
    let mut first_refusal: Option<ApiReject> = None;
    for task in tasks {
        if dropped.contains(&task)
            || skipped
                .iter()
                .any(|line| line.starts_with(&format!("{task}: ")))
        {
            continue;
        }
        let kind = EventKind::TaskDropped {
            task: task.clone(),
            reason: reason.clone(),
        };
        match commit(ctx, kind) {
            Ok(reply) => {
                seq = reply.seq;
                dropped.push(task);
            }
            Err(err) => {
                skipped.push(format!("{task}: {}", err.body.message));
                first_refusal.get_or_insert(err);
            }
        }
    }
    if dropped.is_empty() {
        return Err(first_refusal
            .unwrap_or_else(|| ApiReject::refused("none of those tasks could be dropped")));
    }
    Ok(ActionReply {
        ok: true,
        seq,
        task: None,
        dropped,
        skipped,
    })
}

/// Starts a run when the log has none, so the first task in a fresh repository
/// can be added without a CLI step.
fn ensure_run(ctx: &Context<'_>) -> Result<(), ApiReject> {
    let (folded, _) = ledger::load(ctx.repo).map_err(read_error)?;
    if folded.run.is_some() {
        return Ok(());
    }
    for event in default_run(ctx.repo).map_err(ApiReject::refused)? {
        commit(ctx, event)?;
    }
    Ok(())
}

/// The events that start a run with the defaults a person would type: the base
/// is the branch the repository has checked out, the integration branch is
/// `integration/<run id>` and is cut from the base when it does not exist, and
/// the run id is the start time. A directory that is not a git repository gets
/// a log-only run (the same as `shprd run start` there). A detached HEAD or a
/// repository with no commit has no base to name, so it is refused in plain
/// words.
///
/// The integration branch is created here, before the events are written; the
/// caller appends the returned events in order. Both the service's first task
/// and the CLI's `agent task add` start a run through this, so the coordinator
/// (which adds tasks with the CLI) never meets "no run has started".
pub fn default_run(repo: &Path) -> Result<Vec<EventKind>, String> {
    let git = Git::new(repo);
    let in_repo = git.run(&["rev-parse", "--git-dir"]).is_ok();
    let base = if in_repo {
        match git.current_branch() {
            Ok(Some(branch)) if git.rev_parse(&branch).is_ok() => branch,
            _ => {
                return Err(
                    "no run can start yet: the repository has no checked-out branch \
                     with a commit to base the work on"
                        .to_string(),
                );
            }
        }
    } else {
        "main".to_string()
    };
    let run_id = format!("r{}", SystemClock.now_ms() / 1000);
    let integration = format!("integration/{run_id}");
    let mut made = false;
    if in_repo && !git.branch_exists(&integration).unwrap_or(false) {
        git.run(&["branch", &integration, &base]).map_err(|err| {
            format!("could not create the integration branch {integration:?} from {base:?}: {err}")
        })?;
        made = true;
    }
    let mut events = vec![EventKind::RunStarted {
        run_id,
        repo: repo.to_string_lossy().to_string(),
        base_branch: base,
        integration_branch: integration.clone(),
    }];
    if made {
        events.push(EventKind::Resource {
            action: ResourceAction::Created,
            kind: ResourceKind::Branch,
            name: integration,
            task: None,
        });
    }
    Ok(events)
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

/// Starts a task's review again: closes its reviewer and delivers the review once more.
///
/// The engine records the new review request itself, which is what retires the
/// `review kickoff not delivered` card (`shprd_views::cards::card`); a refusal
/// (the reviewer's own words) leaves the card standing, with its button.
fn review_again(ctx: &Context<'_>, task: &str) -> Result<ActionReply, ApiReject> {
    let engine = ctx.engine.ok_or_else(|| {
        ApiReject::refused("this service was started without a host, so it cannot start a review")
    })?;
    let (folded, _) = ledger::load(ctx.repo).map_err(read_error)?;
    if !folded.tasks.contains_key(task) {
        return Err(ApiReject::unknown_task(task));
    }
    engine
        .review_again(ctx.repo, task)
        .map_err(ApiReject::refused)?;
    let seq = ledger::last_seq(ctx.repo)
        .map_err(|err| ApiReject::internal(format!("could not read the run log: {err}")))?;
    ctx.hub.publish(seq);
    folded_task(ctx.repo, task, seq)
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

/// Moves a handed-in task back to Working, as `shprd task reopen` does.
///
/// The rules decide: only a human, only a Reported or Passed task, only its
/// newest attempt. Nothing outside the log is touched: the worktree and branch
/// stay, and the worker hands in again.
fn reopen(ctx: &Context<'_>, task: &str) -> Result<ActionReply, ApiReject> {
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
                "{task} has no attempt to reopen: nothing has been started on it"
            ))
        })?;
    commit(
        ctx,
        EventKind::TaskReopened {
            task: task.to_string(),
            attempt,
        },
    )
}

/// Accepts a task's work at the head its branch is at, as `shprd approve` does.
///
/// The approval is pinned to a commit: what the person read is what they agreed
/// to, so a branch that moved after it has to be accepted again. Where
/// `tasks.merge` is not `ask`, nothing waits for an approval — the task lands
/// on its own — and recording one would be a decision with no effect, so the
/// action is refused instead. The exception is a task that touched a protected
/// path it did not declare: it waits for the person's Accept whatever the
/// policy ([`shprd_core::tick::Facts::protected`]), so its Accept is taken.
fn accept(ctx: &Context<'_>, task: &str) -> Result<ActionReply, ApiReject> {
    let (folded, _) = ledger::load(ctx.repo).map_err(read_error)?;
    let found = folded
        .tasks
        .get(task)
        .ok_or_else(|| ApiReject::unknown_task(task))?;
    if let Some(policy) = ctx.merge
        && policy != MergePolicy::Ask
        && shprd_git::changed::protected_for_task(ctx.repo, &folded, found).is_empty()
    {
        let word = shprd_core::settings::merge_name(policy);
        return Err(ApiReject::refused(format!(
            "nothing waits for an acceptance: tasks.merge is {word:?}, so {task} lands on its own"
        )));
    }
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
        // A scope card an older build raised is moot: declared paths are a
        // hint now, so no client draws one.
        .filter(|ask| !shprd_core::ask::is_scope_card(&ask.id))
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
    // A stuck review's `review again` needs a host to start the reviewer on.
    let review_again = ask.starts_with(shprd_core::review::STUCK_ASK_PREFIX)
        && answer
            .trim()
            .eq_ignore_ascii_case(shprd_core::review::REVIEW_AGAIN_CHOICE);
    if review_again && !ctx.engine.is_some_and(Engine::can_review_again) {
        return Err(ApiReject::refused(
            "this service was started without a host, so it cannot start a review",
        ));
    }
    // A proposal card's `confirm` is the person running the verb the card
    // names. The rules are asked first, so an answer the rules would refuse is
    // not consumed; a sync is no event alone, so it is run after the answer.
    let proposal = match proposal_of(&before, task, ask, answer)? {
        Some(Proposal::Event(kind)) => {
            before
                .check_as(ctx.actor, &kind, SystemClock.now_ms())
                .map_err(|err| ApiReject::refused(err.to_string()))?;
            Some(Proposal::Event(kind))
        }
        other => other,
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
            return Err(with_prefix(
                commit_error(err),
                "your answer was not recorded and the card is still open: ",
            ));
        }
    };
    ctx.hub.publish(event.seq);
    // The card's answer is the person's confirmation, so what it chose is
    // recorded as theirs: the override `take it as it is` means (the tick lands
    // the task on its own policy afterwards) or the drop.
    if let Some(kind) = followup {
        let acted = ledger::commit(ctx.repo, ctx.actor, kind).map_err(after_answer)?;
        ctx.hub.publish(acted.seq);
    }
    match proposal {
        Some(Proposal::Event(kind)) => {
            let acted = ledger::commit(ctx.repo, ctx.actor, kind).map_err(after_answer)?;
            ctx.hub.publish(acted.seq);
        }
        Some(Proposal::Sync) => {
            run_a_verb(ctx, "sync")?;
            // The verb wrote its own event from its own process.
            if let Ok((state, _)) = ledger::load(ctx.repo) {
                ctx.hub.publish(state.last_seq);
            }
        }
        Some(Proposal::RunClose) => {
            run_a_verb(ctx, "run close")?;
            if let Ok((state, _)) = ledger::load(ctx.repo) {
                ctx.hub.publish(state.last_seq);
            }
        }
        None => {}
    }
    if review_again && let Some(engine) = ctx.engine {
        // The answer is the person's word that the old reviewer is gone, so
        // the new one replaces it; a refusal is the review's own words.
        engine
            .review_again(ctx.repo, task)
            .map_err(ApiReject::refused)?;
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

/// What a confirmed proposal card runs.
enum Proposal {
    /// A verb that is one event, checked by the rules before the answer.
    Event(EventKind),
    /// `shprd sync`, which merges in the integration worktree.
    Sync,
    /// `shprd run close`, which ends the run once its work is done.
    ///
    /// It is the run's verb, not a task's, so it is not an event this service
    /// can append: the verb itself asks the rules, closes the tabs the run left
    /// open and records the refusal or the end. Running the binary's own verb is
    /// what keeps the button and `shprd run close` one implementation.
    RunClose,
}

/// The id every `shprd agent propose` card begins with.
const PROPOSE_ASK_PREFIX: &str = "propose-";

/// What a proposal card's confirming choice begins with.
const PROPOSE_CONFIRM: &str = "confirm ";

/// The first word of the run's close command, as its card's confirmation spells
/// it: `confirm run close`.
const CLOSE_WORD: &str = "run";

/// The verb a confirmed proposal card stands for, or `None` when `ask` is not
/// a proposal or `answer` is not its confirmation (`leave it`, anything else).
///
/// The card's shape is `shprd agent propose`'s: id `propose-<verb>-<n>`, a
/// `confirm <verb> [<subject>]` choice and a `reason: ` line in the text. The
/// answer must be the confirmation for the verb in the id, so a free-typed
/// answer cannot run another verb.
fn proposal_of(
    state: &shprd_core::state::RunState,
    task: &str,
    ask: &str,
    answer: &str,
) -> Result<Option<Proposal>, ApiReject> {
    let Some(rest) = ask.strip_prefix(PROPOSE_ASK_PREFIX) else {
        return Ok(None);
    };
    let verb = rest.split('-').next().unwrap_or_default();
    let Some(chosen) = answer.trim().strip_prefix(PROPOSE_CONFIRM) else {
        return Ok(None);
    };
    let (word, subject) = match chosen.trim().split_once(' ') {
        Some((word, subject)) => (word, subject.trim()),
        None => (chosen.trim(), ""),
    };
    // `run close` is two words in the command the card prints, and its id is
    // still `propose-close-<n>`: the id spells the verb's own word, and the
    // choice spells the command a person would type. So a close card matches on
    // either the two words or the one, and no other card's confirmation does.
    let matches_verb = word == verb || (word == CLOSE_WORD && subject == verb);
    if !matches_verb {
        return Ok(None);
    }
    let record = state
        .asks
        .get(&(task.to_string(), ask.to_string()))
        .ok_or_else(|| ApiReject::refused(format!("{ask} is not an ask of {task}")))?;
    // Only the choices the card offered count: the subject is the card's, not
    // free text.
    if !record.options.iter().any(|option| option == answer.trim()) {
        return Ok(None);
    }
    let reason = record
        .text
        .split_once("\nreason: ")
        .map(|(_, reason)| reason.trim().to_string())
        .unwrap_or_default();
    let found = state
        .tasks
        .get(task)
        .ok_or_else(|| ApiReject::unknown_task(task))?;
    let kind = match verb {
        "drop" => EventKind::TaskDropped {
            task: task.to_string(),
            reason,
        },
        "reopen" => EventKind::TaskReopened {
            task: task.to_string(),
            attempt: found
                .latest_attempt()
                .map(|attempt| attempt.n)
                .ok_or_else(|| ApiReject::refused(format!("{task} has no attempt to reopen")))?,
        },
        "override" => EventKind::Override {
            task: task.to_string(),
            rule: subject.to_string(),
            reason,
        },
        "readmit" => EventKind::Available {
            agent: subject.to_string(),
            reason,
        },
        "sync" => return Ok(Some(Proposal::Sync)),
        "close" => return Ok(Some(Proposal::RunClose)),
        other => {
            return Err(ApiReject::refused(format!(
                "{other:?} is not a verb a proposal can name"
            )));
        }
    };
    Ok(Some(Proposal::Event(kind)))
}

/// Runs one of the run's verbs as the person, through the engine the binary
/// wired.
///
/// A proposal card is the person's own confirmation, so what it runs is the
/// person's verb, by the binary the service was started from: the merge, the
/// close, the lock and the event are one implementation.
///
/// The service's binary is what runs it, not a copy of the verb here: a service
/// without one refuses rather than pretending, the same way it refuses a stop
/// with no host.
fn run_a_verb(ctx: &Context<'_>, verb: &str) -> Result<(), ApiReject> {
    let engine = ctx.engine.ok_or_else(|| {
        ApiReject::refused(format!(
            "this service was started without a host, so it cannot run `shprd {verb}`"
        ))
    })?;
    engine.run_verb(ctx.repo, verb).map_err(ApiReject::refused)
}

/// Answers one finding or decision of a task's review, and records it.
///
/// The answer is an event: the remark's own ask is raised in the log, then
/// answered through [`answer_ask`], so the first answer stands and the log is
/// the only copy. A finding or decision the review never listed is refused
/// before anything is written.
pub(crate) fn answer_remark(
    ctx: &Context<'_>,
    task: &str,
    request: &crate::dto::AnswerRemarkRequest,
) -> Result<AskReply, ApiReject> {
    use shprd_core::review::{self, RemarkAnswer};
    let remark = request.remark();
    let answer: RemarkAnswer = request.answer.into();
    let (state, _) = ledger::load(ctx.repo).map_err(read_error)?;
    if !state.tasks.contains_key(task) {
        return Err(ApiReject::unknown_task(task));
    }
    let report = ctx
        .engine
        .and_then(|engine| engine.report(ctx.repo, task, request.attempt))
        .ok_or_else(|| {
            ApiReject::refused(format!(
                "the review of {task} attempt {} has no report to answer",
                request.attempt
            ))
        })?;
    let title = review::remark_title(&report, request.attempt, remark)
        .map_err(|err| ApiReject::refused(err.to_string()))?;
    let id = review::remark_ask(request.attempt, remark);
    let already = state
        .asks
        .values()
        .find(|ask| ask.task == task && ask.id == id);
    if already.is_some_and(|ask| ask.answer.is_some()) {
        return Err(ApiReject::refused(format!(
            "{task}: that remark was already answered"
        )));
    }
    if already.is_none() {
        let now_ms = SystemClock.now_ms();
        let text = review::remark_text(task, remark, &title);
        let options = review::remark_options();
        let raised = EventKind::AskRaised {
            task: task.to_string(),
            ask: id.clone(),
            ask_kind: shprd_core::ask::AskKind::Question,
            text: text.clone(),
            options: options.clone(),
            created_at_ms: now_ms,
            expires_at_ms: None,
        };
        state
            .check_as(ctx.actor, &raised, now_ms)
            .map_err(|err| ApiReject::refused(err.to_string()))?;
        let queue = shprd_core::ask::AskQueue::for_repo(ctx.repo);
        let ask = shprd_core::ask::Ask::new(
            id.clone(),
            task.to_string(),
            shprd_core::ask::AskKind::Question,
            text,
            now_ms,
        )
        .with_options(options);
        match queue.raise(&ask) {
            Ok(()) | Err(shprd_core::ask::AskError::AlreadyRaised { .. }) => {}
            Err(err) => return Err(ask_error(err)),
        }
        let event = ledger::commit(ctx.repo, ctx.actor, raised).map_err(commit_error)?;
        ctx.hub.publish(event.seq);
    }
    answer_ask(ctx, task, &id, answer.text())
}

/// The decisions the review of `task`'s `attempt` listed, with the answers the
/// log holds: the data the "ready to land" card shows.
///
/// `attempt` defaults to the task's latest. A review with no readable report
/// lists none.
pub(crate) fn decisions_of(
    repo: &Path,
    engine: Option<&Engine>,
    task: &str,
    attempt: Option<u32>,
) -> Result<crate::dto::DecisionsDto, ApiReject> {
    let (state, _) = ledger::load(repo).map_err(read_error)?;
    let found = state
        .tasks
        .get(task)
        .ok_or_else(|| ApiReject::unknown_task(task))?;
    let attempt = attempt
        .or_else(|| found.attempts.last().map(|latest| latest.n))
        .unwrap_or(0);
    let decisions = engine
        .and_then(|engine| engine.report(repo, task, attempt))
        .map(|report| {
            shprd_core::review::decisions_to_confirm(&state, task, attempt, &report)
                .into_iter()
                .map(crate::dto::DecisionDto::from)
                .collect()
        })
        .unwrap_or_default();
    Ok(crate::dto::DecisionsDto { attempt, decisions })
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
        AskError::InvalidName { .. } | AskError::NotAChoice { .. } => {
            ApiReject::bad_request(err.to_string())
        }
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
            dropped: Vec::new(),
            skipped: Vec::new(),
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
        | EventKind::TaskReopened { task, .. }
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
        dropped: Vec::new(),
        skipped: Vec::new(),
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

/// A refusal of a step that follows a recorded answer: the answer stands, so
/// the person is told that and why the step it asked for did not happen.
fn after_answer(err: CommitError) -> ApiReject {
    with_prefix(
        commit_error(err),
        "your answer was recorded, but what it asked for was refused: ",
    )
}

/// The same refusal with `prefix` before its words.
fn with_prefix(mut reject: ApiReject, prefix: &str) -> ApiReject {
    reject.body.message = format!("{prefix}{}", reject.body.message);
    reject
}

/// Maps a read or fold failure. A log that does not fold is the service's
/// problem, not the client's.
fn read_error(err: ReadError) -> ApiReject {
    ApiReject::internal(err.to_string())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use shprd_core::ask::AskKind;
    use shprd_core::event::Event;
    use shprd_core::state::RunState;

    use super::*;

    /// A run with task `T1` and one proposal card on it.
    fn state_with(ask: &str, text: &str, options: &[&str]) -> RunState {
        let kinds = vec![
            EventKind::RunStarted {
                run_id: "r".to_string(),
                repo: "/tmp/repo".to_string(),
                base_branch: "main".to_string(),
                integration_branch: "integration/m1".to_string(),
            },
            EventKind::TaskAdded {
                task: "T1".to_string(),
                title: "a task".to_string(),
                scope: vec!["src".to_string()],
                acceptance: vec!["it works".to_string()],
                tags: Vec::new(),
                skills: Vec::new(),
                deps: Vec::new(),
            },
            EventKind::AskRaised {
                task: "T1".to_string(),
                ask: ask.to_string(),
                ask_kind: AskKind::Question,
                text: text.to_string(),
                options: options.iter().map(|o| (*o).to_string()).collect(),
                created_at_ms: 5,
                expires_at_ms: None,
            },
        ];
        let events: Vec<Event> = kinds
            .into_iter()
            .enumerate()
            .map(|(i, kind)| Event {
                seq: i as u64 + 1,
                at_ms: 1,
                actor: Actor::Engine,
                kind,
            })
            .collect();
        RunState::fold(&events).expect("fold")
    }

    #[test]
    fn confirming_a_drop_proposal_is_the_drop_with_the_cards_reason() {
        let state = state_with(
            "propose-drop-1",
            "T1: run `drop`?\nreason: stuck",
            &["confirm drop", "leave it"],
        );
        match proposal_of(&state, "T1", "propose-drop-1", "confirm drop").unwrap() {
            Some(Proposal::Event(EventKind::TaskDropped { task, reason })) => {
                assert_eq!(task, "T1");
                assert_eq!(reason, "stuck");
            }
            _ => panic!("a confirmed drop proposal is a drop"),
        }
    }

    #[test]
    fn leaving_it_or_typing_something_else_runs_nothing() {
        let state = state_with(
            "propose-drop-1",
            "T1: run `drop`?\nreason: stuck",
            &["confirm drop", "leave it"],
        );
        for answer in ["leave it", "yes", "confirm override x", "confirm drop now"] {
            assert!(
                proposal_of(&state, "T1", "propose-drop-1", answer)
                    .unwrap()
                    .is_none(),
                "{answer:?} must not run the verb"
            );
        }
    }

    #[test]
    fn an_override_proposal_carries_the_rule_the_card_offered() {
        let state = state_with(
            "propose-override-1",
            "T1: run `override r1`?\nreason: cosmetic",
            &["confirm override r1", "leave it"],
        );
        match proposal_of(&state, "T1", "propose-override-1", "confirm override r1").unwrap() {
            Some(Proposal::Event(EventKind::Override { rule, reason, .. })) => {
                assert_eq!(rule, "r1");
                assert_eq!(reason, "cosmetic");
            }
            _ => panic!("a confirmed override proposal is an override"),
        }
    }

    #[test]
    fn a_sync_proposal_is_run_as_a_verb_and_other_asks_are_not_proposals() {
        let state = state_with(
            "propose-sync-1",
            "T1: run `sync`?\nreason: base moved",
            &["confirm sync", "leave it"],
        );
        assert!(matches!(
            proposal_of(&state, "T1", "propose-sync-1", "confirm sync").unwrap(),
            Some(Proposal::Sync)
        ));
        assert!(
            proposal_of(&state, "T1", "scope-widen-1", "confirm drop")
                .unwrap()
                .is_none()
        );
    }

    /// A close proposal is the run's verb, so its confirmation is not an event
    /// this service can append: it is `shprd run close` in the person's own
    /// process, which is what
    /// [`answer_ask`] runs for [`Proposal::RunClose`].
    #[test]
    fn a_close_proposal_is_the_runs_close_and_nothing_else_runs_it() {
        let state = state_with(
            "propose-close-1",
            "T1: the run's work is done: run `run close`?\nreason: all landed\nthis ends the run \
             rather than the task, and a new run can start after it",
            &["confirm run close", "leave it"],
        );
        assert!(
            matches!(
                proposal_of(&state, "T1", "propose-close-1", "confirm run close").unwrap(),
                Some(Proposal::RunClose)
            ),
            "the close card's confirmation runs the run's close"
        );
        // Leaving it, or a confirmation that names something else, runs nothing.
        for answer in ["leave it", "confirm close", "confirm drop", "confirm run"] {
            assert!(
                proposal_of(&state, "T1", "propose-close-1", answer)
                    .unwrap()
                    .is_none(),
                "{answer:?} must not close the run"
            );
        }
        // The card's own words are the only ones that count: a free-typed
        // confirmation of a verb the card did not offer runs nothing.
        assert!(
            proposal_of(&state, "T1", "propose-drop-1", "confirm run close")
                .unwrap()
                .is_none()
        );
    }
}
