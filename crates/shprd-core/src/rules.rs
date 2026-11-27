//! Transition rules: which event may follow which state.
//!
//! The engine never trusts an agent. Before appending an event it calls
//! [`crate::state::RunState::check`], and an [`Err`] means the action is
//! refused rather than recorded.

use std::collections::BTreeSet;

use thiserror::Error;

use crate::event::{
    Actor, AgentId, CheckReport, ENGINE_STATUS_MAX_CHARS, EventKind, ReportStatus, ResourceAction,
    ResourceKind, ReviewSource, TASK_STATUS_MAX_CHARS, TaskId,
};
use crate::gate::REVIEW_GATE;
use crate::scope::{self, ScopeError};
use crate::state::{AgentInfo, ResourceStatus, ReviewRequest, RunState, Task, TaskStatus};

/// Why an event was refused.
#[derive(Debug, Error)]
pub enum RuleError {
    /// A run was already started; a run has exactly one start.
    #[error("the run has already started")]
    RunAlreadyStarted,
    /// No run has started yet, so nothing else can happen.
    #[error("no run has started")]
    NoRun,
    /// The run has finished; nothing may follow.
    #[error("the run has finished")]
    RunFinished,
    /// The task id is already taken.
    #[error("task {task:?} already exists")]
    DuplicateTask {
        /// The task.
        task: TaskId,
    },
    /// The task is not in the plan.
    #[error("task {task:?} does not exist")]
    UnknownTask {
        /// The task.
        task: TaskId,
    },
    /// The task has no title.
    #[error("task {task:?} has an empty title")]
    EmptyTitle {
        /// The task.
        task: TaskId,
    },
    /// A task already had a handover from this program.
    #[error("task {task:?} already had a handover from that program")]
    TaskAlreadyBackedUp {
        /// The task.
        task: TaskId,
    },
    /// A scope entry is not a usable repo-relative path.
    #[error("task {task:?} has a bad scope entry {path:?}: {source}")]
    BadScope {
        /// The task.
        task: TaskId,
        /// The entry, as given.
        path: String,
        /// Why it was rejected.
        source: ScopeError,
    },
    /// The task states no acceptance criteria.
    #[error("task {task:?} has empty acceptance criteria")]
    EmptyAcceptance {
        /// The task.
        task: TaskId,
    },
    /// A dependency names a task that is not in the plan.
    #[error("task {task:?} depends on unknown task {dep:?}")]
    UnknownDependency {
        /// The task.
        task: TaskId,
        /// The missing dependency.
        dep: TaskId,
    },
    /// The task is not in a status that allows the event.
    #[error("task {task:?} is {found:?}, which does not allow this event; expected {expected}")]
    WrongStatus {
        /// The task.
        task: TaskId,
        /// The status the rule needs, in words.
        expected: &'static str,
        /// The status the task actually has.
        found: TaskStatus,
    },
    /// A human dropped the task, so nothing may be appended for it.
    #[error("task {task:?} was dropped by a human: no agent may record work for it again")]
    TaskIsDropped {
        /// The task.
        task: TaskId,
    },
    /// A dependency has not been integrated yet.
    #[error("task {task:?} depends on {dep:?}, which has not been integrated")]
    UnmetDependency {
        /// The task.
        task: TaskId,
        /// The dependency still open.
        dep: TaskId,
    },
    /// The attempt number skipped or repeated.
    #[error("task {task:?} is on attempt {expected}, not {attempt}")]
    AttemptOutOfOrder {
        /// The task.
        task: TaskId,
        /// The attempt number in the event.
        attempt: u32,
        /// The attempt number the log expects.
        expected: u32,
    },
    /// The agent is on cooldown.
    #[error("agent {agent:?} is dry")]
    AgentDry {
        /// The agent.
        agent: AgentId,
    },
    /// The agent is not on cooldown, so there is nothing to clear.
    #[error("agent {agent:?} is not dry")]
    AgentNotDry {
        /// The agent.
        agent: AgentId,
    },
    /// A re-admission was recorded by something the run does not let clear a
    /// cooldown.
    #[error("{actor:?} cannot re-admit an agent: only the engine or a human can")]
    ReAdmitNotAllowed {
        /// Who tried.
        actor: Actor,
    },
    /// A base sync was recorded by something the run does not let run one.
    #[error("{actor:?} cannot sync the base: only a human, the orchestrator or the engine can")]
    SyncNotAllowed {
        /// Who tried.
        actor: Actor,
    },
    /// A new scope widening: declared paths are a hint, so there is nothing to
    /// widen. A log that already holds one still folds.
    #[error(
        "task {task:?}'s declared paths are a hint, not a limit, so a scope is no longer widened: a worker may change what the task needs"
    )]
    ScopeWideningRetired {
        /// The task.
        task: TaskId,
    },
    /// A changed path is not a usable repo-relative path.
    #[error("task {task:?} changed {path:?}, which is not a repo-relative path")]
    BadChangedPath {
        /// The task.
        task: TaskId,
        /// The path, as given.
        path: String,
    },
    /// A hand-in changes a path no task may commit: shprd's own state or a
    /// real environment file (see [`scope::forbidden`]).
    #[error(
        "task {task:?} changes {path:?}, which no task may commit because {why}: remove it from the branch and hand in again"
    )]
    ForbiddenPath {
        /// The task.
        task: TaskId,
        /// The path.
        path: String,
        /// Why the path is off limits, in plain words.
        why: &'static str,
    },
    /// The attempt has not reported yet.
    #[error("task {task:?} attempt {attempt} has not reported")]
    NotReported {
        /// The task.
        task: TaskId,
        /// The attempt.
        attempt: u32,
    },
    /// The attempt reported something other than completion.
    #[error("task {task:?} attempt {attempt} reported {status:?}, not Completed")]
    ReportNotCompleted {
        /// The task.
        task: TaskId,
        /// The attempt.
        attempt: u32,
        /// What it reported.
        status: ReportStatus,
    },
    /// The attempt's review already has a verdict, so it cannot be restarted.
    #[error(
        "task {task:?} attempt {attempt} already has a review verdict and cannot be reviewed again"
    )]
    ReviewDecided {
        /// The task.
        task: TaskId,
        /// The attempt.
        attempt: u32,
    },
    /// The attempt's review already has a verdict, so a second one is refused.
    ///
    /// It is [`RuleError::ReviewDecided`] for a verdict rather than a request:
    /// two readers of one packet (a watchdog and the service's pass, a person
    /// and the engine) both plan from a state that says the review is owed, and
    /// only the log under its lock can tell the second of them that the first
    /// has already decided it.
    #[error(
        "task {task:?} attempt {attempt} already has a review verdict: a review is decided once"
    )]
    ReviewAlreadyDecided {
        /// The task.
        task: TaskId,
        /// The attempt.
        attempt: u32,
    },
    /// Integration was requested for a task that has not passed its gates.
    #[error("task {task:?} is {found:?} and cannot be integrated")]
    NotPassed {
        /// The task.
        task: TaskId,
        /// The status it actually has.
        found: TaskStatus,
    },
    /// A completed hand-in did not report the gates it ran.
    ///
    /// The hand-in is the worker's claim that its work is tested. A claim with
    /// no checks recorded is exactly what the engine cannot verify, so it is
    /// refused rather than stored.
    #[error(
        "task {task:?} attempt {attempt} was handed in without its checks: name each gate it ran with `--check GATE|COMMAND|EXIT[|OUTPUT]`"
    )]
    HandInWithoutChecks {
        /// The task.
        task: TaskId,
        /// The attempt that was handed in.
        attempt: u32,
    },
    /// A hand-in reported a gate that did not pass.
    #[error(
        "task {task:?} attempt {attempt}: gate {gate:?} exited {exit:?}, so the hand-in is refused until it passes"
    )]
    HandInCheckFailed {
        /// The task.
        task: TaskId,
        /// The attempt that was handed in.
        attempt: u32,
        /// The gate that failed.
        gate: String,
        /// Its exit code, or `None` when a timeout killed it.
        exit: Option<i32>,
    },
    /// A checks report entry named no gate, or no command.
    #[error(
        "task {task:?} attempt {attempt}: a checks report entry needs a gate name and a command"
    )]
    EmptyCheck {
        /// The task.
        task: TaskId,
        /// The attempt that was handed in.
        attempt: u32,
    },
    /// A task has no attempt, so there is no hand-in to judge.
    #[error("task {task:?} has no attempt to hand in")]
    NoAttempt {
        /// The task.
        task: TaskId,
    },
    /// No review was ever handed out for the attempt.
    #[error(
        "task {task:?} attempt {attempt} has no review request, so no independent review happened"
    )]
    NoReviewRequest {
        /// The task.
        task: TaskId,
        /// The attempt that would be reviewed.
        attempt: u32,
    },
    /// The only review requested for the attempt shares the worker's vendor.
    #[error(
        "task {task:?} attempt {attempt}: reviewer {reviewer:?} shares the worker's vendor, so the review is not independent"
    )]
    ReviewNotIndependent {
        /// The task.
        task: TaskId,
        /// The attempt that was reviewed.
        attempt: u32,
        /// The reviewer that shared the vendor.
        reviewer: AgentId,
    },
    /// The attempt's newest review gate did not pass.
    #[error("task {task:?} attempt {attempt} has no passed review gate")]
    ReviewNotPassed {
        /// The task.
        task: TaskId,
        /// The attempt that was reviewed.
        attempt: u32,
    },
    /// A task's status line was empty after trimming.
    #[error("task {task:?} needs a non-empty status message")]
    EmptyStatusMessage {
        /// The task.
        task: TaskId,
    },
    /// A task's status line was longer than the cap.
    #[error("task {task:?} status message is {found} characters; the cap is {max}")]
    StatusMessageTooLong {
        /// The task.
        task: TaskId,
        /// How long the message was.
        found: usize,
        /// The cap it broke.
        max: usize,
    },
    /// A task status carried progress outside 0 to 100.
    #[error("task {task:?} progress is {progress}, which is not between 0 and 100")]
    ProgressOutOfRange {
        /// The task.
        task: TaskId,
        /// The value it carried.
        progress: u8,
    },
    /// An override did not name a rule or give a reason.
    #[error("override for task {task:?} needs a non-empty rule and reason")]
    EmptyOverride {
        /// The task, or empty for a run-wide override.
        task: TaskId,
    },
    /// An override was recorded by something that is not a human.
    ///
    /// The rule an override sets aside is the human's to set aside: an
    /// orchestrator or an agent that recorded its own would make every rule
    /// advisory, including a failed review. History keeps what it already
    /// recorded, like every other actor-bound rule.
    #[error("{actor:?} cannot override a rule: only a human can")]
    OverrideNotHuman {
        /// Who tried.
        actor: Actor,
    },
    /// An override named a rule the rules do not read.
    ///
    /// An override of a name nothing honours does nothing, so accepting it
    /// would let a typo record a decision that silently has no effect.
    #[error(
        "{rule:?} is not a rule an override can set aside; the rules an override can name are {expected:?}"
    )]
    UnknownOverrideRule {
        /// The rule the event named.
        rule: String,
        /// The rules an override may name.
        expected: &'static [&'static str],
    },
    /// A resource was created under a name that is already live or archived.
    #[error("{kind:?} {name:?} has already been created and not removed")]
    ResourceExists {
        /// The resource kind.
        kind: ResourceKind,
        /// The resource name.
        name: String,
    },
    /// A resource was archived or removed that the ledger does not know about.
    #[error("{kind:?} {name:?} was never created")]
    ResourceUnknown {
        /// The resource kind.
        kind: ResourceKind,
        /// The resource name.
        name: String,
    },
    /// A resource was archived or removed from a status that does not allow it.
    #[error("{kind:?} {name:?} is {found:?}, which does not allow this action")]
    WrongResourceStatus {
        /// The resource kind.
        kind: ResourceKind,
        /// The resource name.
        name: String,
        /// The status it actually has.
        found: ResourceStatus,
    },
    /// A resource event carried an empty name.
    #[error("{kind:?} needs a non-empty name")]
    EmptyResourceName {
        /// The resource kind.
        kind: ResourceKind,
    },
    /// A merge was recorded for a pull request the log has not seen opened.
    #[error("pull request #{number} was never opened, so it cannot be merged")]
    UnknownPullRequest {
        /// The number the event named.
        number: u64,
    },
    /// An approval named the run's pull request after it had already merged.
    #[error("pull request #{number} has already merged so it cannot be approved")]
    MergedPullRequest {
        /// The number of the pull request that already merged.
        number: u64,
    },
    /// An approval named a pull request the log has not seen opened.
    #[error("pull request #{number} was never opened, so it cannot be approved")]
    UnopenedPullRequest {
        /// The number the event named.
        number: u64,
    },
    /// An ask the log never saw raised cannot be answered.
    #[error("task {task:?} has no ask {ask:?}")]
    UnknownAsk {
        /// The task.
        task: TaskId,
        /// The ask.
        ask: crate::ask::AskId,
    },
    /// An ask already has an answer; a second one is refused.
    #[error("ask {ask:?} of task {task:?} was already answered")]
    AskAlreadyAnswered {
        /// The task.
        task: TaskId,
        /// The ask.
        ask: crate::ask::AskId,
    },
    /// The ask timed out before anyone answered it.
    #[error("ask {ask:?} of task {task:?} expired at {expires_at_ms}")]
    AskExpired {
        /// The task.
        task: TaskId,
        /// The ask.
        ask: crate::ask::AskId,
        /// When it stopped being answerable.
        expires_at_ms: u64,
    },
    /// The ask offers choices and the answer is none of them.
    #[error("{answer:?} is not one of the choices for ask {ask:?} of task {task:?}: {}", .options.join(", "))]
    AskNotAChoice {
        /// The task.
        task: TaskId,
        /// The ask.
        ask: crate::ask::AskId,
        /// What was answered.
        answer: String,
        /// The choices that were offered.
        options: Vec<String>,
    },
    /// An ask with that id was already raised for the task.
    #[error("ask {ask:?} of task {task:?} was already raised")]
    AskAlreadyRaised {
        /// The task.
        task: TaskId,
        /// The ask.
        ask: crate::ask::AskId,
    },
    /// An ask or its answer had no words.
    #[error("task {task:?}: an ask and its answer must say something")]
    EmptyAsk {
        /// The task.
        task: TaskId,
    },
    /// An approval was recorded by something that is not a human.
    #[error("{actor:?} cannot approve a pull request: only a human can")]
    NotHuman {
        /// Who tried.
        actor: Actor,
    },
    /// A review was skipped by something that is not the engine.
    #[error("{actor:?} cannot skip a review: only the engine can, when review is off")]
    NotEngine {
        /// Who tried.
        actor: Actor,
    },
    /// A dismissal came from something that is not a person.
    #[error("{actor:?} cannot dismiss a card: only a person can")]
    DismissNotHuman {
        /// Who tried.
        actor: Actor,
    },
    /// A dismissal named a card the run does not hold.
    #[error("there is no card number {seq} to dismiss")]
    UnknownNeed {
        /// The sequence number named.
        seq: u64,
    },
    /// A review verdict was recorded by something that is not the engine.
    ///
    /// Only the engine decides a review, from the reviewer's packet. An
    /// orchestrator or agent that recorded its own verdict would make the
    /// review meaningless, and a human who wants past a review uses
    /// [`EventKind::Override`], which says so in the log.
    #[error(
        "task {task:?}: a review verdict is the engine's to record, from the reviewer's packet"
    )]
    VerdictNotFromReviewer {
        /// The task.
        task: TaskId,
    },
    /// A review verdict carried no provenance.
    ///
    /// The source is what proves a reviewer wrote the findings, so a verdict
    /// without one is a claim about a review nothing can check.
    #[error("task {task:?}: a review verdict must name the reviewer and the findings it read")]
    UnprovenVerdict {
        /// The task.
        task: TaskId,
    },
    /// A review verdict answered a review request that is no longer the latest.
    ///
    /// A restart records a fresh request for the same attempt, possibly naming
    /// a different reviewer. A verdict written before that request is about
    /// work the current review has not looked at, so accepting it would back
    /// the pull request's passed marker with the review it replaced.
    #[error(
        "task {task:?} attempt {attempt}: a review verdict must answer the latest review request"
    )]
    StaleReviewVerdict {
        /// The task.
        task: TaskId,
        /// Attempt number.
        attempt: u32,
    },
    /// Integration was requested while the latest attempt has a failed gate and
    /// no override recorded after it.
    #[error("task {task:?} cannot be integrated: gate {gate:?} failed and no override followed")]
    FailedGate {
        /// The task.
        task: TaskId,
        /// The failed gate.
        gate: String,
    },
    /// A task was dropped by something that is not a human.
    ///
    /// Dropping is how a task leaves the run's work without landing: the human
    /// decided nobody will do it. An orchestrator or an agent that could drop a
    /// task could mark away the work it was given, so only a human may.
    #[error("{actor:?} cannot drop a task: only a human can")]
    DropNotHuman {
        /// Who tried.
        actor: Actor,
    },
    /// A dropped task carried no reason.
    #[error("task {task:?} was dropped without a reason, which is the only record of why")]
    EmptyDropReason {
        /// The task.
        task: TaskId,
    },
    /// The agent on an attempt was already stopped.
    ///
    /// A stop is about a terminal that is open, so a second one names an agent
    /// that is not there any more. It is refused with that fact rather than
    /// recorded, so the board says why the second press did nothing (M10 gate bug
    /// 1).
    #[error(
        "task {task:?} attempt {attempt} was already stopped: nobody is on it, and `task.retry` starts a new attempt"
    )]
    AlreadyStopped {
        /// The task.
        task: TaskId,
        /// The attempt that was stopped already.
        attempt: u32,
    },
    /// A run was abandoned by something that is not a human.
    ///
    /// `--abandon` ends a run that still has work in it: it is the human's
    /// decision to strand that work, and their name is what the log keeps for
    /// it. An agent that could abandon its own run would close the tabs and
    /// walk away from the task it was given.
    #[error("{actor:?} cannot abandon a run: only a human can")]
    AbandonNotHuman {
        /// Who tried.
        actor: Actor,
    },
    /// An abandon listed something other than exactly the unfinished tasks.
    ///
    /// The list is the record of what the run left behind, so it is checked
    /// against the work actually unfinished at that moment: a missing task
    /// under-reports what was stranded, and an extra or unknown one blames work
    /// that is done or never existed.
    #[error(
        "the abandoned list is not the run's unfinished work: missing {missing:?}, extra {extra:?}"
    )]
    AbandonListMismatch {
        /// Abandonable tasks the list does not name.
        missing: Vec<TaskId>,
        /// Tasks the list names that are not abandonable.
        extra: Vec<TaskId>,
    },
    /// A run was finished by something that may not finish one.
    ///
    /// A human ends a run, and so does the orchestrator, whose brief ends it
    /// with `shprd run finish` after the merge. An agent or the engine ending a
    /// run would close tabs nobody asked it to close.
    #[error("{actor:?} cannot finish a run: only a human or the orchestrator can")]
    FinishNotAllowed {
        /// Who tried.
        actor: Actor,
    },
    /// A run was finished while some of its tasks were not integrated.
    #[error("the run still has unfinished work: {tasks:?}")]
    RunUnfinished {
        /// The tasks that are not integrated.
        tasks: Vec<TaskId>,
    },
    /// A run was finished while the pull request carrying its work was open.
    #[error(
        "every task is integrated, but the run's pull request has not merged, so nothing carries the work yet"
    )]
    PullRequestUnmerged,
}

/// The gate a landing records as failed when the task's branch clashes with
/// work that landed after it was cut.
///
/// The planner reads it: a task whose attempt ends on this failed gate is sent
/// back for a fresh attempt at once, without spending a review round.
pub const LANDING_CONFLICT_GATE: &str = "integrate";

/// Whether `task`'s newest attempt ended on a landing that clashed with the
/// integration branch.
pub fn landing_conflict(task: &Task) -> bool {
    task.latest_attempt()
        .and_then(|attempt| attempt.gates.last())
        .is_some_and(|gate| gate.gate == LANDING_CONFLICT_GATE && !gate.passed)
}

/// Rule name a human overrides to integrate an attempt whose gate failed.
pub const INTEGRATE_AFTER_FAILED_GATE: &str = "integrate_after_failed_gate";

/// Every rule a human may override, by the name the log records.
///
/// An override whose name no rule reads decides nothing, so a typo would
/// record a decision that never takes effect. `shprd override` and the rule
/// below both read this list: adding a rule that honours an override means
/// adding its name here.
pub const OVERRIDE_RULES: &[&str] = &[INTEGRATE_AFTER_FAILED_GATE];

/// Whether `rule` names an override the rules honour.
pub fn is_override_rule(rule: &str) -> bool {
    OVERRIDE_RULES.contains(&rule)
}

/// True when `agent` is on cooldown at `now_ms`.
///
/// An agent is dry until its reset time; a dry agent with no reset time stays
/// dry until the log says otherwise.
pub(crate) fn is_dry(info: Option<&AgentInfo>, now_ms: u64) -> bool {
    match info {
        Some(info) if info.dry => match info.dry_until_ms {
            Some(until) => until > now_ms,
            None => true,
        },
        _ => false,
    }
}

/// Whether `agent` may be re-admitted: only an agent the log has on cooldown,
/// and only by the engine or a human.
///
/// The engine is the watchdog, which re-admits an agent once its reset time
/// passes. A human is the maintainer who decides the run may use the agent
/// again, with `shprd readmit`. An orchestrator or an agent that could clear a
/// cooldown would make a dry mark advisory, so neither may record one; history
/// keeps whatever it already recorded, like every other actor-bound rule.
fn check_available(
    state: &RunState,
    actor: &Actor,
    agent: &str,
    provenance: Provenance,
) -> Result<(), RuleError> {
    if provenance == Provenance::Required && !matches!(actor, Actor::Engine | Actor::Human) {
        return Err(RuleError::ReAdmitNotAllowed {
            actor: actor.clone(),
        });
    }
    match state.agents.get(agent) {
        Some(info) if info.dry => Ok(()),
        _ => Err(RuleError::AgentNotDry {
            agent: agent.to_string(),
        }),
    }
}

/// Rules for recording an override: a human, and a rule the run reads.
///
/// `SHPRD_ACTOR` is an environment variable, so a pane can claim to be a
/// human. What holds regardless is the terminal `shprd override` asks its
/// question at, which no agent's shell has; this rule is what refuses the
/// event outright from an orchestrator or an agent, so no verb of theirs can
/// talk a failed review past the run. The name is checked too: an override
/// nothing honours is a decision that does nothing, and a typo must be
/// refused rather than logged.
///
/// History is not re-judged, like every other actor-bound rule: a log written
/// before either check existed still folds.
fn check_override(
    state: &RunState,
    actor: &Actor,
    task: &str,
    rule: &str,
    reason: &str,
    provenance: Provenance,
) -> Result<(), RuleError> {
    if !task.is_empty() {
        task_in(state, task)?;
    }
    if rule.trim().is_empty() || reason.trim().is_empty() {
        return Err(RuleError::EmptyOverride {
            task: task.to_string(),
        });
    }
    if provenance == Provenance::Historical {
        return Ok(());
    }
    if !matches!(actor, Actor::Human) {
        return Err(RuleError::OverrideNotHuman {
            actor: actor.clone(),
        });
    }
    if !is_override_rule(rule) {
        return Err(RuleError::UnknownOverrideRule {
            rule: rule.to_string(),
            expected: OVERRIDE_RULES,
        });
    }
    Ok(())
}

/// Checks one event kind against `state` as of `now_ms`.
pub(crate) fn check(state: &RunState, kind: &EventKind, now_ms: u64) -> Result<(), RuleError> {
    check_as(state, &Actor::Human, kind, now_ms)
}

/// Checks one event kind caused by `actor` against `state` as of `now_ms`.
///
/// A bare [`check`] is the human's: a verb that consults the log before an
/// outside effect has no other actor to offer. A verb about to append passes
/// its own actor, which is what an approval's and a review verdict's rule read.
pub(crate) fn check_as(
    state: &RunState,
    actor: &Actor,
    kind: &EventKind,
    now_ms: u64,
) -> Result<(), RuleError> {
    check_with(state, actor, kind, now_ms, Provenance::Required)
}

/// Checks one event already in the log, as the fold replays it.
///
/// History is not re-judged: a review verdict recorded before provenance
/// existed would otherwise stop a run's log from folding at all. Every other
/// rule still holds, because a log that broke one of those was never valid.
pub(crate) fn replay_as(
    state: &RunState,
    actor: &Actor,
    kind: &EventKind,
    now_ms: u64,
) -> Result<(), RuleError> {
    check_with(state, actor, kind, now_ms, Provenance::Historical)
}

/// Whether a review verdict has to prove where it came from.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Provenance {
    /// A verdict being recorded now must be the engine's, with a source.
    Required,
    /// A verdict read back out of the log keeps whatever it was recorded with.
    Historical,
}

/// Checks one event kind caused by `actor`, with `provenance` in force.
fn check_with(
    state: &RunState,
    actor: &Actor,
    kind: &EventKind,
    now_ms: u64,
    provenance: Provenance,
) -> Result<(), RuleError> {
    if let EventKind::RunStarted { .. } = kind {
        return match state.run {
            None => Ok(()),
            Some(_) => Err(RuleError::RunAlreadyStarted),
        };
    }
    if state.run.is_none() {
        return Err(RuleError::NoRun);
    }
    if state.finished && !is_cleanup(kind) {
        return Err(RuleError::RunFinished);
    }
    match kind {
        EventKind::RunStarted { .. } => Ok(()),
        EventKind::TaskAdded {
            task,
            title,
            scope: paths,
            acceptance,
            deps,
            ..
        } => check_task_added(state, task, title, paths, acceptance, deps),
        EventKind::Spawned {
            task,
            attempt,
            agent,
            ..
        } => check_spawned(state, task, *attempt, agent, now_ms, provenance),
        EventKind::Reported {
            task,
            attempt,
            status,
            checks,
            changed_paths,
            ..
        } => check_reported(
            state,
            task,
            *attempt,
            *status,
            checks,
            changed_paths,
            provenance,
        ),
        EventKind::GatePassed {
            task,
            attempt,
            gate,
            source,
            ..
        }
        | EventKind::GateFailed {
            task,
            attempt,
            gate,
            source,
            ..
        } => {
            check_gate(state, task, *attempt, provenance)?;
            check_verdict(provenance, state, actor, task, *attempt, gate, source)
        }
        EventKind::ReviewRequested { task, attempt, .. } => {
            check_review_requested(state, task, *attempt, provenance)
        }
        EventKind::Integrated { task, .. } => check_integrated(state, task, provenance),
        EventKind::TaskStatus {
            task,
            message,
            progress,
        } => check_task_status(state, actor, task, message, *progress, provenance),
        EventKind::AskRaised {
            task,
            ask,
            text,
            created_at_ms,
            ..
        } => check_ask_raised(state, task, ask, text, *created_at_ms, provenance),
        EventKind::AskAnswered {
            task, ask, answer, ..
        } => check_ask_answered(state, task, ask, answer, now_ms, provenance),
        EventKind::TaskDropped { task, reason } => {
            check_task_dropped(state, actor, task, reason, provenance)
        }
        EventKind::TaskStopped { task, attempt } => {
            check_task_stopped(state, task, *attempt, provenance)
        }
        EventKind::TaskReopened { task, attempt } => {
            check_task_reopened(state, actor, task, *attempt, provenance)
        }
        EventKind::ReviewSkipped { task, attempt } => {
            check_review_skipped(state, actor, task, *attempt, provenance)
        }
        EventKind::ScopeWidened { task, .. } => check_scope_widened(state, task, provenance),
        EventKind::Dry { .. } => Ok(()),
        EventKind::Available { agent, .. } => check_available(state, actor, agent, provenance),
        EventKind::HandedOff { task, to_agent, .. } => {
            check_handed_off(state, task, to_agent, now_ms, provenance)
        }
        // The fallback ladder acts on an agent that is already running: the
        // task it names must be one the run still holds, and nothing else is
        // the log's to judge. A handover may only be recorded when the same task
        // has no started handover from that program yet, which is the once-per-line rule
        // read where the fact is written rather than trusted to the planner.
        EventKind::Nudged { task, .. } | EventKind::ProviderGaveUp { task, .. } => {
            check_not_dropped(state, task, provenance)
        }
        EventKind::Backup {
            task, from_agent, ..
        } => {
            check_not_dropped(state, task, provenance)?;
            if state.tasks.get(task).is_some_and(|entry| {
                entry.fallbacks.iter().any(|record| {
                    record.action == crate::event::FallbackAction::Backup
                        && record.agent == *from_agent
                        && record.started != Some(false)
                })
            }) {
                return Err(RuleError::TaskAlreadyBackedUp { task: task.clone() });
            }
            Ok(())
        }
        EventKind::Escalated { task, .. } => match task {
            Some(task) => task_in(state, task).map(|_| ()),
            None => Ok(()),
        },
        EventKind::NeedDismissed { need } => {
            if !matches!(actor, Actor::Human) {
                return Err(RuleError::DismissNotHuman {
                    actor: actor.clone(),
                });
            }
            if state.escalations.iter().any(|e| e.seq == *need) {
                Ok(())
            } else {
                Err(RuleError::UnknownNeed { seq: *need })
            }
        }
        EventKind::Override { task, rule, reason } => {
            check_override(state, actor, task, rule, reason, provenance)
        }
        EventKind::Resource {
            action,
            kind,
            name,
            task,
        } => check_resource(state, *action, *kind, name, task.as_deref()),
        EventKind::FullGate { .. } => Ok(()),
        // An open pull request may be re-recorded at any time: the run's
        // branch is pushed once, but its body is refreshed on every `land`.
        EventKind::PrOpened { .. } => Ok(()),
        EventKind::PrMerged { number, .. } => match &state.pr {
            Some(pr) if pr.number == *number => Ok(()),
            _ => Err(RuleError::UnknownPullRequest { number: *number }),
        },
        EventKind::Approved { number, head_sha } => check_approved(state, actor, *number, head_sha),
        EventKind::TaskPrOpened { task, .. } => check_not_dropped(state, task, provenance),
        EventKind::TaskApproved {
            task,
            number,
            head_sha,
        } => check_task_approved(state, actor, task, *number, head_sha),
        // Landing is a fact git and the forge reported: nothing here can judge
        // it, and the task it names must be one the run still holds.
        EventKind::TaskLanded { task, .. } => check_not_dropped(state, task, provenance),
        EventKind::OrchestratorLaunched { .. } => Ok(()),
        EventKind::WatchLaunched { .. } => Ok(()),
        // A delivery record moves nothing: it is the watchdog's own bookmark,
        // and any pass may record one.
        EventKind::OrchestratorNotified { .. } => Ok(()),
        EventKind::BaseSynced { .. } => check_base_synced(actor, provenance),
        // A baseline excuses failing gates, so a worker may not write one.
        EventKind::GateBaselined { .. } => check_base_synced(actor, provenance),
        EventKind::RunFinished { abandoned } => {
            check_run_finished(state, actor, abandoned, provenance)
        }
    }
}

/// Whether `kind` only cleans up, and so stays legal after the run finished.
///
/// The janitor has to be able to remove a finished run's worktrees, but
/// nothing may be created once the run is over.
fn is_cleanup(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::Resource {
            action: ResourceAction::Archived | ResourceAction::Removed,
            ..
        }
    )
}

/// Whether `actor` may record a base sync.
///
/// Merging the base into the run's own integration branch is operational
/// housekeeping: a human runs it by hand, and the orchestrator runs it before
/// the gate, which is the case this verb exists for. An agent never does: a
/// worker's own branch is cut from the base by the engine, and an agent that
/// could rewrite the integration branch would move the head the gate and the
/// pull request are pinned to.
fn check_base_synced(actor: &Actor, provenance: Provenance) -> Result<(), RuleError> {
    if provenance == Provenance::Required
        && !matches!(actor, Actor::Engine | Actor::Orchestrator | Actor::Human)
    {
        return Err(RuleError::SyncNotAllowed {
            actor: actor.clone(),
        });
    }
    Ok(())
}

/// Rules for a human's drop of a task.
///
/// Dropping is the human's decision alone: it takes a task out of the run's
/// work, and an agent that could take itself out of a run would walk away from
/// the task it was given, which is the same reason `--abandon` is the human's.
/// History is not re-judged, like every other actor-bound rule.
///
/// Only a task with work left to drop may be dropped: an integrated task's
/// commits are already on the integration branch, so calling it dropped would
/// make the pull request's `## Dropped` list disagree with what it carries, and
/// a dropped task is terminal already. Any other status may be dropped,
/// `Working` included — a human who decides a running task will not be done
/// stops it there rather than waiting for it to report.
///
/// The reason is required because it is the only record of why the work is
/// missing: the pull request lists it, and a blank one would say nothing.
fn check_task_dropped(
    state: &RunState,
    actor: &Actor,
    task: &str,
    reason: &str,
    provenance: Provenance,
) -> Result<(), RuleError> {
    if provenance == Provenance::Required && !matches!(actor, Actor::Human) {
        return Err(RuleError::DropNotHuman {
            actor: actor.clone(),
        });
    }
    let found = task_in(state, task)?;
    if matches!(found.status, TaskStatus::Integrated | TaskStatus::Dropped) {
        return Err(RuleError::WrongStatus {
            task: task.to_string(),
            expected: "a task that has not landed and is not already dropped",
            found: found.status,
        });
    }
    if reason.trim().is_empty() {
        return Err(RuleError::EmptyDropReason {
            task: task.to_string(),
        });
    }
    Ok(())
}

/// Rules for a person stopping a task's agent.
///
/// A stop is a fact about a terminal, and the only terminal it can be a fact
/// about is one that is still open: the task must have an attempt, that attempt
/// must be the newest one, its agent must not already have been stopped, and the
/// task must not have landed or been dropped. A second stop of the same attempt
/// is refused with the reason rather than recorded twice, because the second one
/// names an agent that is no longer there.
///
/// Any actor may record it: the person's own `task.stop` does, and so does the
/// engine when it closes a terminal nobody may use any more. What the log keeps
/// is who stopped it.
fn check_task_stopped(
    state: &RunState,
    task: &str,
    attempt: u32,
    provenance: Provenance,
) -> Result<(), RuleError> {
    check_not_dropped(state, task, provenance)?;
    let found = task_in(state, task)?;
    if found.status == TaskStatus::Integrated {
        return Err(RuleError::WrongStatus {
            task: task.to_string(),
            expected: "a task whose agent is still running",
            found: found.status,
        });
    }
    let current = expect_current_attempt(task, found, attempt)?;
    if current.stopped {
        return Err(RuleError::AlreadyStopped {
            task: task.to_string(),
            attempt,
        });
    }
    Ok(())
}

/// Rules for passing a task without a review because review is off.
///
/// Only the engine records it, for the newest attempt of a `Reported` task
/// whose hand-in completed with every check green and which no review was ever
/// requested for: a review that was asked for and never decided still has to
/// be decided.
fn check_review_skipped(
    state: &RunState,
    actor: &Actor,
    task: &str,
    attempt: u32,
    provenance: Provenance,
) -> Result<(), RuleError> {
    if provenance == Provenance::Required && !matches!(actor, Actor::Engine) {
        return Err(RuleError::NotEngine {
            actor: actor.clone(),
        });
    }
    check_not_dropped(state, task, provenance)?;
    let found = task_in(state, task)?;
    if found.status != TaskStatus::Reported {
        return Err(RuleError::WrongStatus {
            task: task.to_string(),
            expected: "Reported",
            found: found.status,
        });
    }
    expect_current_attempt(task, found, attempt)?;
    let current = found.latest_attempt().ok_or_else(|| RuleError::NoAttempt {
        task: task.to_string(),
    })?;
    let green = current.report.as_ref().is_some_and(|report| {
        report.status == ReportStatus::Completed
            && !report.checks.is_empty()
            && report.checks.iter().all(|check| check.exit_code == Some(0))
    });
    if !green || !current.reviews.is_empty() {
        return Err(RuleError::NotPassed {
            task: task.to_string(),
            found: found.status,
        });
    }
    Ok(())
}

/// Rules for reopening a handed-in task.
///
/// Only a human reopens: it throws away a review the engine may already have
/// run, which is the person's call. The task must be `Reported` or `Passed`
/// (anything else is already open, failed, landed or dropped), and the attempt
/// named must be the newest one.
fn check_task_reopened(
    state: &RunState,
    actor: &Actor,
    task: &str,
    attempt: u32,
    provenance: Provenance,
) -> Result<(), RuleError> {
    if provenance == Provenance::Required && !matches!(actor, Actor::Human) {
        return Err(RuleError::NotHuman {
            actor: actor.clone(),
        });
    }
    check_not_dropped(state, task, provenance)?;
    let found = task_in(state, task)?;
    if !matches!(found.status, TaskStatus::Reported | TaskStatus::Passed) {
        return Err(RuleError::WrongStatus {
            task: task.to_string(),
            expected: "Reported or Passed",
            found: found.status,
        });
    }
    expect_current_attempt(task, found, attempt)?;
    Ok(())
}

/// Rules for a human's approval of the run's pull request.
///
/// The approval must name the pull request the log knows, and that pull
/// request must still be open: an approval that arrives after the merge could
/// not have gated anything, so it is refused rather than recorded as if it
/// had. Such a late approval is refused as [`RuleError::MergedPullRequest`],
/// which says what actually happened; every other refusal, including a pull
/// request the log never opened, stays [`RuleError::UnopenedPullRequest`].
/// Only a human may give an approval: an agent that could approve its own work
/// would make the approval meaningless, which is the same reason a forge
/// refuses to let a pull request's author approve it.
fn check_approved(
    state: &RunState,
    actor: &Actor,
    number: u64,
    head_sha: &str,
) -> Result<(), RuleError> {
    if !matches!(actor, Actor::Human) {
        return Err(RuleError::NotHuman {
            actor: actor.clone(),
        });
    }
    match &state.pr {
        Some(pr) if pr.number == number && pr.merged => {
            Err(RuleError::MergedPullRequest { number })
        }
        Some(pr) if pr.number == number && !head_sha.trim().is_empty() => Ok(()),
        _ => Err(RuleError::UnopenedPullRequest { number }),
    }
}

/// Rules for a human's approval of a task's own pull request.
///
/// The same rule as [`check_approved`], read off the task: the approval is the
/// human's alone, it names the pull request the log holds for that task, and a
/// pull request that already merged could not have been gated by it. Asking
/// that the head be non-empty keeps an approval from naming nothing.
///
/// A local landing has no pull request, so number `0` with a head is the
/// approval of the task itself: there is nothing to open and nothing to merge
/// on a forge, and a human still has to say yes.
fn check_task_approved(
    state: &RunState,
    actor: &Actor,
    task: &str,
    number: u64,
    head_sha: &str,
) -> Result<(), RuleError> {
    if !matches!(actor, Actor::Human) {
        return Err(RuleError::NotHuman {
            actor: actor.clone(),
        });
    }
    let found = task_in(state, task)?;
    match &found.pr {
        Some(pr) if pr.number == number && pr.merged => {
            Err(RuleError::MergedPullRequest { number })
        }
        Some(pr) if pr.number == number && !head_sha.trim().is_empty() => Ok(()),
        None if number == 0 && !head_sha.trim().is_empty() => Ok(()),
        _ => Err(RuleError::UnopenedPullRequest { number }),
    }
}

/// Rules for the resource ownership ledger.
///
/// shprd may only ever clean up what it recorded creating, so an archive or a
/// removal has to name a resource this run created, and a creation has to be
/// either new or a re-creation after removal. An archive takes a resource out
/// of service, so it needs one that is still `Live`: archiving twice is a
/// mistake in the plan, not a no-op.
///
/// A [`ResourceKind::ReviewPackets`] removal is the one exception: the folder's
/// path comes from the layout and a validated run id, so the janitor's plan —
/// not the ledger — is what proves it is shprd's own. shprd wrote packets there
/// before it kept a record of them, and a run that has finished may record no
/// creation, so without this a folder would be unremovable forever, or removable
/// with nothing in the ledger to show it.
fn check_resource(
    state: &RunState,
    action: ResourceAction,
    kind: ResourceKind,
    name: &str,
    task: Option<&str>,
) -> Result<(), RuleError> {
    if name.trim().is_empty() {
        return Err(RuleError::EmptyResourceName { kind });
    }
    if let Some(task) = task {
        task_in(state, task)?;
    }
    let found = state
        .resources
        .get(&(kind, name.to_string()))
        .map(|record| record.status);
    match action {
        ResourceAction::Created => match found {
            Some(ResourceStatus::Live | ResourceStatus::Archived) => {
                Err(RuleError::ResourceExists {
                    kind,
                    name: name.to_string(),
                })
            }
            Some(ResourceStatus::Removed) | None => Ok(()),
        },
        ResourceAction::Archived => match found {
            Some(ResourceStatus::Live) => Ok(()),
            Some(found) => Err(RuleError::WrongResourceStatus {
                kind,
                name: name.to_string(),
                found,
            }),
            None => Err(RuleError::ResourceUnknown {
                kind,
                name: name.to_string(),
            }),
        },
        ResourceAction::Removed => match found {
            Some(ResourceStatus::Live | ResourceStatus::Archived) => Ok(()),
            Some(found) => Err(RuleError::WrongResourceStatus {
                kind,
                name: name.to_string(),
                found,
            }),
            // Nothing to close, but the removal is still the record of a
            // cleanup the plan proved was shprd's own.
            None if kind == ResourceKind::ReviewPackets => Ok(()),
            None => Err(RuleError::ResourceUnknown {
                kind,
                name: name.to_string(),
            }),
        },
    }
}

/// Borrows a task, or reports it as unknown.
fn task_in<'a>(state: &'a RunState, id: &str) -> Result<&'a Task, RuleError> {
    match state.tasks.get(id) {
        Some(task) => Ok(task),
        None => Err(RuleError::UnknownTask {
            task: id.to_string(),
        }),
    }
}

/// Rules for adding a planned task.
fn check_task_added(
    state: &RunState,
    task: &str,
    title: &str,
    paths: &[String],
    acceptance: &[String],
    deps: &[TaskId],
) -> Result<(), RuleError> {
    let id = task.to_string();
    if state.tasks.contains_key(task) {
        return Err(RuleError::DuplicateTask { task: id });
    }
    if title.trim().is_empty() {
        return Err(RuleError::EmptyTitle { task: id });
    }
    for path in paths {
        if let Err(source) = scope::normalize(path) {
            return Err(RuleError::BadScope {
                task: id,
                path: path.clone(),
                source,
            });
        }
    }
    if acceptance.iter().all(|line| line.trim().is_empty()) {
        return Err(RuleError::EmptyAcceptance { task: id });
    }
    for dep in deps {
        if !state.tasks.contains_key(dep) {
            return Err(RuleError::UnknownDependency {
                task: id,
                dep: dep.clone(),
            });
        }
    }
    Ok(())
}

/// Refuses a late event from a task a human already dropped.
///
/// A drop takes the task out of the run's work, and `drop` closes the task's
/// tabs before it records one. A worker that is somehow still running — a host
/// that failed to close the tab, a shell the agent left behind — must not be
/// able to append to a task nobody is waiting for: a report, a gate, a spawn, a
/// hand-off or an integration after the drop would put work back into a run that
/// has decided against it, and the janitor may already have archived the branch.
///
/// The same Provenance rule as the other actor-bound checks applies: this is a
/// rule about what may be *appended* now. A log being folded is read, not
/// re-judged, so the status rules alone decide what an old log already holds —
/// and they refuse these events for a Dropped task anyway, which is why the
/// guard stands down on replay rather than reporting a second reason.
fn check_not_dropped(
    state: &RunState,
    task: &str,
    provenance: Provenance,
) -> Result<(), RuleError> {
    if provenance == Provenance::Historical {
        return Ok(());
    }
    if task_in(state, task)?.status == TaskStatus::Dropped {
        return Err(RuleError::TaskIsDropped {
            task: task.to_string(),
        });
    }
    Ok(())
}

/// Rules for spawning an agent on a task attempt.
fn check_spawned(
    state: &RunState,
    task: &str,
    attempt: u32,
    agent: &AgentId,
    now_ms: u64,
    provenance: Provenance,
) -> Result<(), RuleError> {
    check_not_dropped(state, task, provenance)?;
    let found = task_in(state, task)?;
    if !matches!(found.status, TaskStatus::Queued | TaskStatus::Failed) {
        return Err(RuleError::WrongStatus {
            task: task.to_string(),
            expected: "Queued or Failed",
            found: found.status,
        });
    }
    for dep in &found.deps {
        let needed = task_in(state, dep)?;
        if needed.status != TaskStatus::Integrated {
            return Err(RuleError::UnmetDependency {
                task: task.to_string(),
                dep: dep.clone(),
            });
        }
    }
    let expected = u32::try_from(found.attempts.len())
        .unwrap_or(u32::MAX)
        .saturating_add(1);
    if attempt != expected {
        return Err(RuleError::AttemptOutOfOrder {
            task: task.to_string(),
            attempt,
            expected,
        });
    }
    if is_dry(state.agents.get(agent), now_ms) {
        return Err(RuleError::AgentDry {
            agent: agent.clone(),
        });
    }
    Ok(())
}

/// Rules for a scope widening: only history holds one.
///
/// Declared paths are a hint, not a limit, so nothing is widened any more. A
/// log written while widenings existed still folds: history is not re-judged.
fn check_scope_widened(
    state: &RunState,
    task: &str,
    provenance: Provenance,
) -> Result<(), RuleError> {
    check_not_dropped(state, task, provenance)?;
    task_in(state, task)?;
    if provenance == Provenance::Historical {
        return Ok(());
    }
    Err(RuleError::ScopeWideningRetired {
        task: task.to_string(),
    })
}

/// Rules for an agent reporting the outcome of an attempt.
///
/// A completed report is the worker's hand-in, so it must carry the checks the
/// worker ran: at least one gate, each with a name, a command and a passing
/// exit code. A hand-in without them, or with a failing gate, is refused rather
/// than stored, because the engine and the reviewer judge the work by what the
/// worker says it tested. A blocked or failed report is not a hand-in and needs
/// none.
///
/// History is not re-judged: a report written before hand-ins carried checks
/// still folds, and so does one whose checks are not shaped the way this rule
/// wants.
fn check_reported(
    state: &RunState,
    task: &str,
    attempt: u32,
    status: ReportStatus,
    checks: &[CheckReport],
    changed_paths: &[String],
    provenance: Provenance,
) -> Result<(), RuleError> {
    check_not_dropped(state, task, provenance)?;
    let found = task_in(state, task)?;
    if found.status != TaskStatus::Working {
        return Err(RuleError::WrongStatus {
            task: task.to_string(),
            expected: "Working",
            found: found.status,
        });
    }
    let current = expect_current_attempt(task, found, attempt)?;
    if current.report.is_some() {
        return Err(RuleError::WrongStatus {
            task: task.to_string(),
            expected: "an unreported attempt",
            found: found.status,
        });
    }
    // The declared scope is a hint, so a changed path outside it is the
    // review's to judge, not a refusal. Only the hard list refuses, and only for
    // a hand-in being written now: history is not re-judged.
    if provenance == Provenance::Historical {
        return Ok(());
    }
    for path in changed_paths {
        let Ok(normalized) = scope::normalize(path) else {
            return Err(RuleError::BadChangedPath {
                task: task.to_string(),
                path: path.clone(),
            });
        };
        if let Some(why) = scope::forbidden(&normalized) {
            return Err(RuleError::ForbiddenPath {
                task: task.to_string(),
                path: normalized,
                why,
            });
        }
    }
    if status != ReportStatus::Completed {
        return Ok(());
    }
    if checks.is_empty() {
        return Err(RuleError::HandInWithoutChecks {
            task: task.to_string(),
            attempt,
        });
    }
    for check in checks {
        if check.gate.trim().is_empty() || check.command.trim().is_empty() {
            return Err(RuleError::EmptyCheck {
                task: task.to_string(),
                attempt,
            });
        }
        if check.exit_code != Some(0)
            && crate::baseline::excuse(&state.gate_baselines, &check.command, &check.output_tail)
                .is_none()
        {
            return Err(RuleError::HandInCheckFailed {
                task: task.to_string(),
                attempt,
                gate: check.gate.clone(),
                exit: check.exit_code,
            });
        }
    }
    Ok(())
}

/// Rules for an agent's task status line.
///
/// Any actor may record one: this is how a worker says where it is, and it
/// never moves the task's status. The line is capped so a pasted agent message
/// cannot become an event, and progress stays inside 0 to 100. A dropped task
/// takes no status: the worker posting it was left behind by the drop.
fn check_task_status(
    state: &RunState,
    actor: &Actor,
    task: &str,
    message: &str,
    progress: Option<u8>,
    provenance: Provenance,
) -> Result<(), RuleError> {
    check_not_dropped(state, task, provenance)?;
    task_in(state, task)?;
    let trimmed = message.trim();
    if trimmed.is_empty() {
        return Err(RuleError::EmptyStatusMessage {
            task: task.to_string(),
        });
    }
    // The engine's own status says why a step waits, which a worker's
    // 40-character progress line has no room for.
    let max = if matches!(actor, Actor::Agent { .. }) {
        TASK_STATUS_MAX_CHARS
    } else {
        ENGINE_STATUS_MAX_CHARS
    };
    let found = trimmed.chars().count();
    if found > max {
        return Err(RuleError::StatusMessageTooLong {
            task: task.to_string(),
            found,
            max,
        });
    }
    if let Some(progress) = progress
        && progress > 100
    {
        return Err(RuleError::ProgressOutOfRange {
            task: task.to_string(),
            progress,
        });
    }
    Ok(())
}

/// Rules for raising an ask: it names a task the run holds, is new, and says
/// something.
fn check_ask_raised(
    state: &RunState,
    task: &str,
    ask: &str,
    text: &str,
    _created_at_ms: u64,
    provenance: Provenance,
) -> Result<(), RuleError> {
    task_in(state, task)?;
    check_not_dropped(state, task, provenance)?;
    if text.trim().is_empty() {
        return Err(RuleError::EmptyAsk {
            task: task.to_string(),
        });
    }
    if state
        .asks
        .contains_key(&(task.to_string(), ask.to_string()))
    {
        return Err(RuleError::AskAlreadyRaised {
            task: task.to_string(),
            ask: ask.to_string(),
        });
    }
    Ok(())
}

/// Rules for answering an ask: the log knows it, it has no answer yet, it has
/// not timed out, and the answer says something.
fn check_ask_answered(
    state: &RunState,
    task: &str,
    ask: &str,
    answer: &str,
    now_ms: u64,
    provenance: Provenance,
) -> Result<(), RuleError> {
    task_in(state, task)?;
    let Some(record) = state.asks.get(&(task.to_string(), ask.to_string())) else {
        return Err(RuleError::UnknownAsk {
            task: task.to_string(),
            ask: ask.to_string(),
        });
    };
    if record.answer.is_some() {
        return Err(RuleError::AskAlreadyAnswered {
            task: task.to_string(),
            ask: ask.to_string(),
        });
    }
    if let Some(until) = record.expires_at_ms
        && until <= now_ms
    {
        return Err(RuleError::AskExpired {
            task: task.to_string(),
            ask: ask.to_string(),
            expires_at_ms: until,
        });
    }
    if answer.trim().is_empty() {
        return Err(RuleError::EmptyAsk {
            task: task.to_string(),
        });
    }
    // An ask with choices takes one of them (trimmed, any case); free text is
    // for asks without choices. This holds for every writer of the ledger, but
    // only for a new answer: one already recorded folds as it was written, so a
    // log made before the check still replays.
    if provenance == Provenance::Required
        && !record.options.is_empty()
        && !record
            .options
            .iter()
            .any(|option| option.trim().eq_ignore_ascii_case(answer.trim()))
    {
        return Err(RuleError::AskNotAChoice {
            task: task.to_string(),
            ask: ask.to_string(),
            answer: answer.to_string(),
            options: record.options.clone(),
        });
    }
    Ok(())
}

/// Rules for recording a gate outcome: it needs a completed report.
fn check_gate(
    state: &RunState,
    task: &str,
    attempt: u32,
    provenance: Provenance,
) -> Result<(), RuleError> {
    check_not_dropped(state, task, provenance)?;
    let found = task_in(state, task)?;
    // A failed gate leaves the task Failed; a later gate on the same attempt is
    // still allowed, which is how a failed attempt gets re-gated after a fix.
    // A Passed task is gated too: integration runs after the task's own gates
    // passed, and a conflicting pick is recorded as a failed gate that sends
    // the task back to Failed.
    if !matches!(
        found.status,
        TaskStatus::Reported | TaskStatus::Failed | TaskStatus::Passed
    ) {
        return Err(RuleError::WrongStatus {
            task: task.to_string(),
            expected: "Reported, Failed or Passed",
            found: found.status,
        });
    }
    let current = expect_current_attempt(task, found, attempt)?;
    match &current.report {
        Some(report) if report.status == ReportStatus::Completed => Ok(()),
        Some(report) => Err(RuleError::ReportNotCompleted {
            task: task.to_string(),
            attempt,
            status: report.status,
        }),
        None => Err(RuleError::NotReported {
            task: task.to_string(),
            attempt,
        }),
    }
}

/// Rules for a review verdict: who may record one, and what must prove it.
///
/// A gate that is not the review gate is free: any actor may record lint or
/// tests. A review verdict is the engine's alone, because only the engine reads
/// the reviewer's packet, and it must carry the provenance that reading gave it.
///
/// The verdict must also answer the *latest* review request for the attempt.
/// Reading the reviewer's packet and appending the verdict are not one step,
/// and `shprd agent review --again` may record a fresh request in between,
/// possibly naming a different reviewer. The old review's verdict would then be
/// about work the current review never read, so it is refused rather than
/// backing the pull request's passed marker. Only the request's seq tells the
/// two apart: the same reviewer may be asked twice for one attempt.
///
/// History is not re-judged: a replayed verdict keeps whatever it was recorded
/// with. A log written before requests carried a seq cannot answer the
/// question, and one written before this rule holds verdicts this rule would
/// refuse, so replay keeps them all.
///
/// A review is decided once per attempt. Two deciders can both plan from a state
/// that says the review is owed — a `shprd watch` and the service's own pass, a
/// person and the engine — and the log under its lock is the only place that can
/// tell the second of them that the first already recorded the verdict. Without
/// this the same packet is folded twice into two gate results, which is what the
/// M9 gate's second run found: one `findings.json`, two `gate_passed` events.
/// A second review of the same attempt needs a new attempt, and a fresh request
/// on a decided attempt is refused by [`check_review_requested`] anyway.
fn check_verdict(
    provenance: Provenance,
    state: &RunState,
    actor: &Actor,
    task: &str,
    attempt: u32,
    gate: &str,
    source: &Option<ReviewSource>,
) -> Result<(), RuleError> {
    if gate != REVIEW_GATE || provenance == Provenance::Historical {
        return Ok(());
    }
    if !matches!(actor, Actor::Engine) {
        return Err(RuleError::VerdictNotFromReviewer {
            task: task.to_string(),
        });
    }
    let source = match source {
        Some(source)
            if !source.reviewer.trim().is_empty() && !source.findings_sha256.trim().is_empty() =>
        {
            source
        }
        _ => {
            return Err(RuleError::UnprovenVerdict {
                task: task.to_string(),
            });
        }
    };
    let current = state
        .tasks
        .get(task)
        .and_then(|found| found.attempts.iter().find(|current| current.n == attempt));
    if current.is_some_and(|current| current.gates.iter().any(|gate| gate.gate == REVIEW_GATE)) {
        return Err(RuleError::ReviewAlreadyDecided {
            task: task.to_string(),
            attempt,
        });
    }
    match latest_review(state, task, attempt) {
        Some(request)
            if request.reviewer == source.reviewer && request.seq == source.request_seq =>
        {
            Ok(())
        }
        _ => Err(RuleError::StaleReviewVerdict {
            task: task.to_string(),
            attempt,
        }),
    }
}

/// The newest review request the log holds for `task`'s attempt `attempt`.
///
/// `None` means no review was ever handed out for that attempt, which a
/// verdict cannot answer either: it would name a reviewer nothing dispatched.
fn latest_review<'a>(state: &'a RunState, task: &str, attempt: u32) -> Option<&'a ReviewRequest> {
    let current = state
        .tasks
        .get(task)?
        .attempts
        .iter()
        .find(|current| current.n == attempt)?;
    current.reviews.last()
}

/// Rules for recording a review dispatch.
///
/// Only a finished attempt can be reviewed: the task has to be Reported, and
/// the attempt named has to be the one that reported. Refusing here is what
/// makes `shprd agent review` open no reviewer tab for a task that has
/// nothing to review.
///
/// A second request for one attempt is allowed while the review is undecided,
/// which is what `shprd agent review --again` records when a brief never
/// arrived and the reviewer has to be started over. Once the review gate has a
/// verdict the attempt is decided, and a second request would be a review of
/// work the run has already judged: it is refused here rather than left to the
/// task's status alone, so the refusal names the real reason.
fn check_review_requested(
    state: &RunState,
    task: &str,
    attempt: u32,
    provenance: Provenance,
) -> Result<(), RuleError> {
    check_not_dropped(state, task, provenance)?;
    let found = task_in(state, task)?;
    let current = expect_current_attempt(task, found, attempt)?;
    if current.gates.iter().any(|gate| gate.gate == REVIEW_GATE) {
        return Err(RuleError::ReviewDecided {
            task: task.to_string(),
            attempt,
        });
    }
    if found.status != TaskStatus::Reported {
        return Err(RuleError::WrongStatus {
            task: task.to_string(),
            expected: "Reported",
            found: found.status,
        });
    }
    Ok(())
}

/// Whether a person accepted the newest failure of `task`.
///
/// It is an `integrate_after_failed_gate` override recorded after the gate that
/// failed, which is what the rule means: a human's word that this failure may
/// land. A failed task is `Failed` rather than `Passed`, so the acceptance is
/// also what lets a landing look at the work at all.
pub fn accepted_after_failure(task: &Task) -> bool {
    let Some(failed) = task
        .latest_attempt()
        .and_then(|attempt| attempt.gates.iter().rfind(|gate| !gate.passed))
    else {
        return false;
    };
    task.overrides
        .iter()
        .any(|record| record.rule == INTEGRATE_AFTER_FAILED_GATE && record.seq > failed.seq)
}

/// Rules for integrating a passed task: a failed gate needs a later override.
fn check_integrated(state: &RunState, task: &str, provenance: Provenance) -> Result<(), RuleError> {
    check_not_dropped(state, task, provenance)?;
    let found = task_in(state, task)?;
    if found.status != TaskStatus::Passed && !accepted_after_failure(found) {
        return Err(RuleError::NotPassed {
            task: task.to_string(),
            found: found.status,
        });
    }
    let failed = match found.attempts.last() {
        Some(current) => current.gates.iter().rfind(|gate| !gate.passed),
        None => None,
    };
    let failed = match failed {
        Some(gate) => gate,
        None => return Ok(()),
    };
    let overridden = found
        .overrides
        .iter()
        .any(|record| record.rule == INTEGRATE_AFTER_FAILED_GATE && record.seq > failed.seq);
    if overridden {
        Ok(())
    } else {
        Err(RuleError::FailedGate {
            task: task.to_string(),
            gate: failed.gate.clone(),
        })
    }
}

/// Whether a task is ready to land: a passing hand-in and an independent,
/// engine-recorded review both say so.
///
/// This is a pure function over the folded task, so the landing verbs, the
/// watchdog and the service can all ask the same question and get the same
/// answer with the same reason. It is the rule behind the review marker: a task
/// whose latest attempt was handed in with passing checks and whose review was
/// dispatched by the engine to a reviewer from another vendor, with the
/// engine's own verdict answering that request, is ready. Anything else is a
/// refusal naming what is missing.
///
/// The vendor question is answered by [`crate::event::EventKind::ReviewRequested`]
/// as it was recorded: the engine compared the reviewer's vendor with the
/// worker's when it dispatched the review and set `degraded` when they matched.
/// The review gate must carry [`ReviewSource`] provenance that answers that
/// same request, which is what proves the engine read the reviewer's packet
/// rather than a claim someone typed.
///
/// The task also has to be `Passed`: a task that failed a gate after its
/// review is not ready, however good the review was. The one exception is a
/// person's acceptance of that failure — an `integrate_after_failed_gate`
/// override recorded after it — because that is the decision the rule exists
/// for: work a reviewer rejected lands on a human's recorded word and degrades
/// the pull request's review marker.
pub fn ready_to_land(task: &Task) -> Result<(), RuleError> {
    let accepted = accepted_after_failure(task);
    if task.status != TaskStatus::Passed && !accepted {
        return Err(RuleError::NotPassed {
            task: task.id.clone(),
            found: task.status,
        });
    }
    let attempt = task.latest_attempt().ok_or_else(|| RuleError::NoAttempt {
        task: task.id.clone(),
    })?;
    match &attempt.report {
        Some(report) if report.status == ReportStatus::Completed => {
            if report.checks.is_empty() {
                return Err(RuleError::HandInWithoutChecks {
                    task: task.id.clone(),
                    attempt: attempt.n,
                });
            }
            if let Some(check) = report
                .checks
                .iter()
                .find(|check| check.exit_code != Some(0))
            {
                return Err(RuleError::HandInCheckFailed {
                    task: task.id.clone(),
                    attempt: attempt.n,
                    gate: check.gate.clone(),
                    exit: check.exit_code,
                });
            }
        }
        Some(report) => {
            return Err(RuleError::ReportNotCompleted {
                task: task.id.clone(),
                attempt: attempt.n,
                status: report.status,
            });
        }
        None => {
            return Err(RuleError::NotReported {
                task: task.id.clone(),
                attempt: attempt.n,
            });
        }
    }
    // A person who accepted the failure answered the review, so there is no
    // review requirement left to check: the hand-in above is all that is left.
    if accepted {
        return Ok(());
    }
    let request = attempt
        .reviews
        .last()
        .ok_or_else(|| RuleError::NoReviewRequest {
            task: task.id.clone(),
            attempt: attempt.n,
        })?;
    if request.degraded {
        return Err(RuleError::ReviewNotIndependent {
            task: task.id.clone(),
            attempt: attempt.n,
            reviewer: request.reviewer.clone(),
        });
    }
    let verdict = attempt
        .gates
        .iter()
        .rev()
        .find(|gate| gate.gate == REVIEW_GATE);
    match verdict {
        Some(gate) if gate.passed => match &gate.source {
            Some(source)
                if source.request_seq == request.seq && source.reviewer == request.reviewer =>
            {
                Ok(())
            }
            _ => Err(RuleError::UnprovenVerdict {
                task: task.id.clone(),
            }),
        },
        _ => Err(RuleError::ReviewNotPassed {
            task: task.id.clone(),
            attempt: attempt.n,
        }),
    }
}

/// Rules for handing a working task to another agent.
fn check_handed_off(
    state: &RunState,
    task: &str,
    to_agent: &AgentId,
    now_ms: u64,
    provenance: Provenance,
) -> Result<(), RuleError> {
    check_not_dropped(state, task, provenance)?;
    let found = task_in(state, task)?;
    if found.status != TaskStatus::Working {
        return Err(RuleError::WrongStatus {
            task: task.to_string(),
            expected: "Working",
            found: found.status,
        });
    }
    if is_dry(state.agents.get(to_agent), now_ms) {
        return Err(RuleError::AgentDry {
            agent: to_agent.clone(),
        });
    }
    Ok(())
}

/// The tasks a run cannot leave behind: every one not in a terminal state.
///
/// Integrated is terminal, because the work is on the integration branch and
/// the run's pull request is what carries it. Dropped is terminal too, because
/// a human decided the work is not coming: nothing is stranded, and a dropped
/// task must not block the run for ever. Queued, Working, Reported, in review
/// (passed or failed) and Failed are all work in flight or work stuck, which
/// ending the run would strand on a branch nobody watches.
///
/// `shprd run finish` names these in its refusal, and the rule refuses a finish
/// or an abandon that disagrees with them, so both read one definition.
pub fn unfinished_tasks(state: &RunState) -> Vec<TaskId> {
    state
        .tasks
        .iter()
        .filter(|(_, task)| !matches!(task.status, TaskStatus::Integrated | TaskStatus::Dropped))
        .map(|(id, _)| id.clone())
        .collect()
}

/// Whether ending the run would leave its landed work on a branch nothing
/// picks up: a task landed and the pull request carrying it has not merged.
///
/// A run with nothing integrated has nothing for a pull request to carry: every
/// task was dropped or none was planned, and refusing that run would ask for a
/// merge that `land` refuses to open.
///
/// A run whose base and integration branch are one branch has no pull request
/// to merge, now or later: landing wrote the work straight onto it.
pub fn unlanded(state: &RunState) -> bool {
    if state
        .run
        .as_ref()
        .is_some_and(|run| run.base_branch == run.integration_branch)
    {
        return false;
    }
    let integrated = state
        .tasks
        .values()
        .any(|task| task.status == TaskStatus::Integrated);
    integrated && state.pr.as_ref().is_none_or(|pr| !pr.merged)
}

/// What a human's `--abandon` may name: the work ending the run would leave.
///
/// That is every unfinished task, plus — when the run is [`unlanded`] — the
/// integrated tasks, whose work only the pull request that never merged would
/// have carried. Without the second part a run whose pull request a human
/// closed on purpose could never end: nothing is unfinished to abandon, and a
/// normal finish is refused because nothing carries the work.
///
/// Nothing is abandonable once every task landed and the pull request merged,
/// so `--abandon` there is a normal finish.
pub fn abandonable_tasks(state: &RunState) -> Vec<TaskId> {
    let mut tasks = unfinished_tasks(state);
    if unlanded(state) {
        tasks.extend(
            state
                .tasks
                .iter()
                .filter(|(_, task)| task.status == TaskStatus::Integrated)
                .map(|(id, _)| id.clone()),
        );
    }
    tasks
}

/// Rules for finishing a run.
///
/// Ending a run is not an agent's decision. A finish closes the run's tabs, so
/// an agent that could record one would walk away from whatever it had not
/// done, and the engine has no reason to end a run a human did not ask it to
/// end. A human may finish a run, and so may the orchestrator: its brief ends
/// the run with `shprd run finish` once the milestone merged. Both need the
/// run's work to be over — nothing unfinished, and the pull request merged —
/// because a finish that strands integrated work is the same loss as one that
/// strands a working task.
///
/// A human's `--abandon` is the one way past that, and the event's list is what
/// records it. The list must be exactly [`abandonable_tasks`]: the unfinished
/// work, plus the integrated tasks while the run is unlanded, because their
/// work is what the pull request that never merged would have carried. It is
/// the log's record of what the run left, so a list missing a task, naming one
/// that is safe or unknown, would make the record lie. Every task it names and
/// every task it leaves out is still checked here, so the refusal does not rest
/// on the verb being the only writer.
///
/// History is not re-judged, like every actor-bound rule: a log written before
/// this one holds finishes and abandons no human recorded, and folding a run is
/// not recording it.
fn check_run_finished(
    state: &RunState,
    actor: &Actor,
    abandoned: &[TaskId],
    provenance: Provenance,
) -> Result<(), RuleError> {
    if provenance == Provenance::Historical {
        return check_run_finished_historical(state, abandoned);
    }
    if !abandoned.is_empty() {
        if !matches!(actor, Actor::Human) {
            return Err(RuleError::AbandonNotHuman {
                actor: actor.clone(),
            });
        }
        return check_abandoned_list(state, abandoned);
    }
    if !matches!(actor, Actor::Human | Actor::Orchestrator) {
        return Err(RuleError::FinishNotAllowed {
            actor: actor.clone(),
        });
    }
    let unfinished = unfinished_tasks(state);
    if !unfinished.is_empty() {
        return Err(RuleError::RunUnfinished { tasks: unfinished });
    }
    if unlanded(state) {
        return Err(RuleError::PullRequestUnmerged);
    }
    Ok(())
}

/// The abandoned list must equal the run's abandonable tasks, as a set.
fn check_abandoned_list(state: &RunState, abandoned: &[TaskId]) -> Result<(), RuleError> {
    let expected: BTreeSet<TaskId> = abandonable_tasks(state).into_iter().collect();
    let listed: BTreeSet<TaskId> = abandoned.iter().cloned().collect();
    if listed == expected {
        return Ok(());
    }
    Err(RuleError::AbandonListMismatch {
        missing: expected.difference(&listed).cloned().collect(),
        extra: listed.difference(&expected).cloned().collect(),
    })
}

/// The finish rule as logs written before it were folded.
///
/// Those logs hold a finish any actor recorded, and an abandon whose list
/// nothing checked, so replay keeps both. The one check it did make — no task
/// Working — still holds, because a log that broke it was never valid.
fn check_run_finished_historical(state: &RunState, abandoned: &[TaskId]) -> Result<(), RuleError> {
    if !abandoned.is_empty() {
        return Ok(());
    }
    for (id, task) in &state.tasks {
        if task.status == TaskStatus::Working {
            return Err(RuleError::WrongStatus {
                task: id.clone(),
                expected: "no task Working",
                found: task.status,
            });
        }
    }
    Ok(())
}

/// Borrows the attempt an event claims to be about, or reports the mismatch.
fn expect_current_attempt<'a>(
    task: &str,
    found: &'a Task,
    attempt: u32,
) -> Result<&'a crate::state::Attempt, RuleError> {
    match found.attempts.last() {
        Some(current) if current.n == attempt => Ok(current),
        Some(current) => Err(RuleError::AttemptOutOfOrder {
            task: task.to_string(),
            attempt,
            expected: current.n,
        }),
        None => Err(RuleError::WrongStatus {
            task: task.to_string(),
            expected: "an attempt",
            found: found.status,
        }),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::event::{Actor, Event, Finding, Severity};

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_string()).collect()
    }

    fn ev(seq: u64, kind: EventKind) -> Event {
        Event {
            seq,
            at_ms: seq * 1000,
            actor: Actor::Engine,
            kind,
        }
    }

    fn started(seq: u64) -> Event {
        ev(
            seq,
            EventKind::RunStarted {
                run_id: "r1".to_string(),
                repo: "/repo".to_string(),
                base_branch: "main".to_string(),
                integration_branch: "integration/m1".to_string(),
            },
        )
    }

    fn added(seq: u64, id: &str, scope: &[&str], deps: &[&str]) -> Event {
        ev(
            seq,
            EventKind::TaskAdded {
                task: id.to_string(),
                title: format!("task {id}"),
                scope: strings(scope),
                acceptance: strings(&["it works"]),
                tags: Vec::new(),
                skills: Vec::new(),
                deps: strings(deps),
            },
        )
    }

    fn spawned(seq: u64, id: &str, attempt: u32, agent: &str) -> Event {
        ev(
            seq,
            EventKind::Spawned {
                task: id.to_string(),
                attempt,
                agent: agent.to_string(),
                model: Some("m1".to_string()),
                worktree: format!("/wt/{id}"),
                branch: format!("feature/{id}"),
            },
        )
    }

    fn reported(seq: u64, id: &str, attempt: u32, paths: &[&str]) -> Event {
        ev(
            seq,
            EventKind::Reported {
                task: id.to_string(),
                attempt,
                status: ReportStatus::Completed,
                summary: "done".to_string(),
                tests: strings(&["cargo test"]),
                checks: vec![check("tests", Some(0))],
                changed_paths: strings(paths),
                commit: Some("abc123".to_string()),
                skills_used: Vec::new(),
                proofs: Vec::new(),
            },
        )
    }

    fn reported_with(seq: u64, id: &str, attempt: u32, outcome: ReportStatus) -> Event {
        ev(
            seq,
            EventKind::Reported {
                task: id.to_string(),
                attempt,
                status: outcome,
                summary: "gave up".to_string(),
                tests: Vec::new(),
                checks: Vec::new(),
                changed_paths: strings(&["src/a"]),
                commit: None,
                skills_used: Vec::new(),
                proofs: Vec::new(),
            },
        )
    }

    /// One hand-in check, for a report that carries them.
    fn check(gate: &str, exit_code: Option<i32>) -> CheckReport {
        CheckReport {
            gate: gate.to_string(),
            command: format!("cargo {gate}"),
            exit_code,
            output_tail: "ok".to_string(),
            pre_existing: None,
        }
    }

    /// A completed report whose checks are `checks`.
    fn hand_in(seq: u64, id: &str, attempt: u32, checks: Vec<CheckReport>) -> Event {
        ev(
            seq,
            EventKind::Reported {
                task: id.to_string(),
                attempt,
                status: ReportStatus::Completed,
                summary: "done".to_string(),
                tests: Vec::new(),
                checks,
                changed_paths: strings(&["src/a"]),
                commit: None,
                skills_used: Vec::new(),
                proofs: Vec::new(),
            },
        )
    }

    fn gated(seq: u64, id: &str, attempt: u32, gate: &str, passed: bool) -> Event {
        let findings = if passed { Vec::new() } else { vec![finding()] };
        let kind = if passed {
            EventKind::GatePassed {
                task: id.to_string(),
                attempt,
                gate: gate.to_string(),
                findings,
                source: None,
            }
        } else {
            EventKind::GateFailed {
                task: id.to_string(),
                attempt,
                gate: gate.to_string(),
                findings,
                source: None,
            }
        };
        ev(seq, kind)
    }

    fn finding() -> Finding {
        Finding {
            severity: Severity::Major,
            title: "clippy warning".to_string(),
            location: None,
            detail: "unwrap used".to_string(),
        }
    }

    fn integrated(seq: u64, id: &str) -> Event {
        ev(
            seq,
            EventKind::Integrated {
                task: id.to_string(),
                commits: strings(&["abc123"]),
            },
        )
    }

    /// Records a human overriding `rule` for `id`.
    fn overrode(seq: u64, id: &str, rule: &str) -> Event {
        let mut event = ev(
            seq,
            EventKind::Override {
                task: id.to_string(),
                rule: rule.to_string(),
                reason: "shipping anyway".to_string(),
            },
        );
        event.actor = Actor::Human;
        event
    }

    fn dried(seq: u64, agent: &str, reset_at_ms: Option<u64>) -> Event {
        ev(
            seq,
            EventKind::Dry {
                agent: agent.to_string(),
                reason: "quota".to_string(),
                reset_at_ms,
            },
        )
    }

    fn available(seq: u64, agent: &str) -> Event {
        ev(
            seq,
            EventKind::Available {
                agent: agent.to_string(),
                reason: "quota reset".to_string(),
            },
        )
    }

    fn finished(seq: u64) -> Event {
        ev(
            seq,
            EventKind::RunFinished {
                abandoned: Vec::new(),
            },
        )
    }

    /// An event that ends the run and names what it left behind.
    fn abandoning(seq: u64, tasks: &[&str]) -> Event {
        ev(
            seq,
            EventKind::RunFinished {
                abandoned: strings(tasks),
            },
        )
    }

    /// A run whose work is in every state that is not integrated: `a` is
    /// reported, `b` was never ran, and `c` landed and merged, so only `a` and
    /// `b` are an abandon's to name.
    fn in_flight() -> Vec<Event> {
        vec![
            started(1),
            added(2, "a", &["src/a"], &[]),
            added(3, "b", &["src/b"], &[]),
            added(4, "c", &["src/c"], &[]),
            spawned(5, "c", 1, "agent-1"),
            reported(6, "c", 1, &["src/c/x.rs"]),
            review_requested(7, "c", 1, "codex/m1", false),
            passed_review(7, "c", 1, 7),
            integrated(8, "c"),
            ev(
                9,
                EventKind::PrOpened {
                    number: 7,
                    url: "https://example.invalid/pr/7".to_string(),
                    head: "shprd/r1/int".to_string(),
                },
            ),
            ev(
                10,
                EventKind::PrMerged {
                    number: 7,
                    head: "abc1234".to_string(),
                },
            ),
            spawned(11, "a", 1, "agent-2"),
            reported(12, "a", 1, &["src/a/x.rs"]),
        ]
    }

    /// A run whose two tasks both landed, and whose pull request never merged:
    /// nothing is unfinished, and nothing carries the work either.
    fn unmerged() -> Vec<Event> {
        let mut log = vec![
            started(1),
            added(2, "a", &["src/a"], &[]),
            added(3, "b", &["src/b"], &[]),
        ];
        for (seq, id, agent) in [(4, "a", "agent-1"), (8, "b", "agent-2")] {
            let changed = format!("src/{id}/x.rs");
            log.push(spawned(seq, id, 1, agent));
            log.push(reported(seq + 1, id, 1, &[changed.as_str()]));
            log.push(review_requested(seq + 2, id, 1, "codex/m1", false));
            log.push(passed_review(seq + 2, id, 1, seq + 2));
            log.push(integrated(seq + 3, id));
        }
        log.push(ev(
            12,
            EventKind::PrOpened {
                number: 7,
                url: "https://example.invalid/pr/7".to_string(),
                head: "shprd/r1/int".to_string(),
            },
        ));
        log
    }

    /// A run whose only task landed and whose pull request merged: nothing is
    /// left to finish.
    fn landed() -> Vec<Event> {
        vec![
            started(1),
            added(2, "a", &["src/a"], &[]),
            spawned(3, "a", 1, "agent-1"),
            reported(4, "a", 1, &["src/a/x.rs"]),
            review_requested(5, "a", 1, "codex/m1", false),
            passed_review(5, "a", 1, 5),
            integrated(6, "a"),
            ev(
                7,
                EventKind::PrOpened {
                    number: 7,
                    url: "https://example.invalid/pr/7".to_string(),
                    head: "shprd/r1/int".to_string(),
                },
            ),
            ev(
                8,
                EventKind::PrMerged {
                    number: 7,
                    head: "abc1234".to_string(),
                },
            ),
        ]
    }

    /// A finish that abandons nothing.
    fn finish_kind() -> EventKind {
        EventKind::RunFinished {
            abandoned: Vec::new(),
        }
    }

    fn full_gate(seq: u64, passed: bool) -> Event {
        ev(
            seq,
            EventKind::FullGate {
                head: "abc1234".to_string(),
                passed,
                steps: vec![],
            },
        )
    }

    /// A resource-ledger event for task `a`.
    fn res(seq: u64, action: ResourceAction, kind: ResourceKind, name: &str) -> Event {
        ev(
            seq,
            EventKind::Resource {
                action,
                kind,
                name: name.to_string(),
                task: Some("a".to_string()),
            },
        )
    }

    /// A run-level resource-ledger event, the way the janitor's review packet
    /// steps record them: no task owns the run's own folder.
    fn res_run(seq: u64, action: ResourceAction, kind: ResourceKind, name: &str) -> Event {
        ev(
            seq,
            EventKind::Resource {
                action,
                kind,
                name: name.to_string(),
                task: None,
            },
        )
    }

    /// The provenance a real verdict carries.
    fn provenance() -> ReviewSource {
        // The request `reviewable()` records at seq 5.
        provenance_for("codex/m1", 5)
    }

    /// The provenance a verdict answers `reviewer`'s request at `request_seq`.
    fn provenance_for(reviewer: &str, request_seq: u64) -> ReviewSource {
        ReviewSource {
            reviewer: reviewer.to_string(),
            findings_sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
                .to_string(),
            request_seq,
        }
    }

    /// A `review` gate result for task `a`, with `source` as its provenance.
    fn review_gate(passed: bool, source: Option<ReviewSource>) -> EventKind {
        let findings = if passed { Vec::new() } else { vec![finding()] };
        if passed {
            EventKind::GatePassed {
                task: "a".to_string(),
                attempt: 1,
                gate: "review".to_string(),
                findings,
                source,
            }
        } else {
            EventKind::GateFailed {
                task: "a".to_string(),
                attempt: 1,
                gate: "review".to_string(),
                findings,
                source,
            }
        }
    }

    /// An event caused by `actor`, not the engine.
    fn by(seq: u64, actor: Actor, kind: EventKind) -> Event {
        Event {
            seq,
            at_ms: seq * 1000,
            actor,
            kind,
        }
    }

    /// A run whose task `a` reported attempt 1 and waits for a gate.
    fn reviewable() -> Vec<Event> {
        vec![
            started(1),
            added(2, "a", &["src/a"], &[]),
            spawned(3, "a", 1, "agent-1"),
            reported(4, "a", 1, &["src/a/x.rs"]),
            review_requested(5, "a", 1, "codex/m1", false),
        ]
    }

    fn folded(events: &[Event]) -> RunState {
        match RunState::fold(events) {
            Ok(state) => state,
            Err(err) => panic!("fold failed: {err}"),
        }
    }

    fn refused(events: &[Event]) -> RuleError {
        match RunState::fold(events) {
            Ok(_) => panic!("fold should have been refused"),
            Err(err) => err,
        }
    }

    /// The refusal a verb would get for the last event: the state is folded from
    /// everything before it, and the last event is checked as one being
    /// appended now. History is read, not re-judged, so a rule that only holds
    /// for new appends (the hand-in checks) is only seen this way.
    fn refused_append(events: &[Event]) -> RuleError {
        let (last, earlier) = match events.split_last() {
            Some(split) => split,
            None => panic!("there is no event to append"),
        };
        let state = folded(earlier);
        match state.check_as(&last.actor, &last.kind, last.at_ms) {
            Ok(()) => panic!("the append should have been refused: {:?}", last.kind),
            Err(err) => err,
        }
    }

    fn status(state: &RunState, id: &str) -> TaskStatus {
        match state.tasks.get(id) {
            Some(task) => task.status,
            None => panic!("task {id} missing"),
        }
    }

    #[test]
    fn happy_path_folds_runs_tasks_and_gates() {
        let log = vec![
            started(1),
            added(2, "a", &["src/a"], &[]),
            added(3, "b", &["src/b"], &["a"]),
            spawned(4, "a", 1, "agent-1"),
            reported(5, "a", 1, &["src/a/x.rs"]),
            review_requested(6, "a", 1, "codex/m1", false),
            passed_review(6, "a", 1, 6),
            integrated(7, "a"),
            spawned(8, "b", 1, "agent-2"),
            reported(9, "b", 1, &["src/b"]),
            review_requested(10, "b", 1, "codex/m1", false),
            passed_review(10, "b", 1, 10),
            integrated(11, "b"),
            finished(12),
        ];
        let state = folded(&log);
        assert!(state.finished);
        assert_eq!(state.last_seq, 12);
        assert_eq!(status(&state, "a"), TaskStatus::Integrated);
        assert_eq!(status(&state, "b"), TaskStatus::Integrated);
        assert_eq!(state.agents.len(), 2);
        assert!(state.escalations.is_empty());
        match state.tasks.get("a") {
            Some(task) => {
                assert_eq!(task.scope, strings(&["src/a"]));
                assert_eq!(task.attempts.len(), 1);
                assert_eq!(task.attempts[0].gates.len(), 1);
                assert_eq!(task.attempts[0].gates[0].gate, "review");
                assert!(task.attempts[0].gates[0].passed);
            }
            None => panic!("task a missing"),
        }
        match state.run {
            Some(run) => assert_eq!(run.integration_branch, "integration/m1"),
            None => panic!("run missing"),
        }
    }

    #[test]
    fn event_before_run_started_is_refused() {
        assert!(matches!(
            refused(&[added(1, "a", &["src"], &[])]),
            RuleError::NoRun
        ));
        assert!(matches!(refused(&[finished(1)]), RuleError::NoRun));
    }

    #[test]
    fn run_starts_exactly_once() {
        assert!(matches!(
            refused(&[started(1), started(2)]),
            RuleError::RunAlreadyStarted
        ));
    }

    #[test]
    fn duplicate_task_is_refused() {
        assert!(matches!(
            refused(&[
                started(1),
                added(2, "a", &["src/a"], &[]),
                added(3, "a", &["src/b"], &[])
            ]),
            RuleError::DuplicateTask { .. }
        ));
    }

    #[test]
    fn task_needs_title_scope_acceptance() {
        let mut empty_title = added(2, "a", &["src"], &[]);
        if let EventKind::TaskAdded { title, .. } = &mut empty_title.kind {
            title.clear();
        }
        assert!(matches!(
            refused(&[started(1), empty_title]),
            RuleError::EmptyTitle { .. }
        ));
        // Declared paths are an optional hint.
        assert_eq!(
            status(&folded(&[started(1), added(2, "a", &[], &[])]), "a"),
            TaskStatus::Queued
        );
        assert!(matches!(
            refused(&[started(1), added(2, "a", &["/abs"], &[])]),
            RuleError::BadScope { .. }
        ));
        let mut no_acceptance = added(2, "a", &["src"], &[]);
        if let EventKind::TaskAdded { acceptance, .. } = &mut no_acceptance.kind {
            acceptance.clear();
        }
        assert!(matches!(
            refused(&[started(1), no_acceptance]),
            RuleError::EmptyAcceptance { .. }
        ));
    }

    #[test]
    fn dependencies_must_exist_and_land_first() {
        assert!(matches!(
            refused(&[started(1), added(2, "b", &["src/b"], &["ghost"])]),
            RuleError::UnknownDependency { .. }
        ));
        assert!(matches!(
            refused(&[
                started(1),
                added(2, "a", &["src/a"], &[]),
                added(3, "b", &["src/b"], &["a"]),
                spawned(4, "b", 1, "agent-1"),
            ]),
            RuleError::UnmetDependency { .. }
        ));
    }

    #[test]
    fn spawn_needs_the_next_attempt_number() {
        assert!(matches!(
            refused(&[
                started(1),
                added(2, "a", &["src/a"], &[]),
                spawned(3, "a", 2, "agent-1")
            ]),
            RuleError::AttemptOutOfOrder { .. }
        ));
    }

    #[test]
    fn spawn_needs_a_queued_or_failed_task() {
        assert!(matches!(
            refused(&[
                started(1),
                added(2, "a", &["src/a"], &[]),
                spawned(3, "a", 1, "agent-1"),
                spawned(4, "a", 2, "agent-2"),
            ]),
            RuleError::WrongStatus { .. }
        ));
    }

    #[test]
    fn overlapping_scopes_start_side_by_side() {
        let log = [
            started(1),
            added(2, "a", &["src"], &[]),
            added(3, "b", &["src/b.rs"], &[]),
            spawned(4, "a", 1, "agent-1"),
            spawned(5, "b", 1, "agent-2"),
        ];
        let state = folded(&log);
        assert_eq!(status(&state, "a"), TaskStatus::Working);
        assert_eq!(status(&state, "b"), TaskStatus::Working);
        // Appended now, not only folded: the spawn rule holds no lock either.
        assert!(
            folded(&log[..4])
                .check_as(&Actor::Engine, &log[4].kind, log[4].at_ms)
                .is_ok()
        );
    }

    #[test]
    fn a_send_back_starts_beside_an_overlapping_task_in_flight() {
        let log = [
            started(1),
            added(2, "a", &["src"], &[]),
            added(3, "b", &["src/b.rs"], &[]),
            spawned(4, "b", 1, "agent-2"),
            reported_with(5, "b", 1, ReportStatus::Failed),
            spawned(6, "a", 1, "agent-1"),
            spawned(7, "b", 2, "agent-2"),
        ];
        let state = folded(&log[..6]);
        assert!(
            state
                .check_as(&Actor::Engine, &log[6].kind, log[6].at_ms)
                .is_ok()
        );
        assert_eq!(status(&folded(&log), "b"), TaskStatus::Working);
    }

    #[test]
    fn non_prefix_scope_does_not_conflict() {
        let state = folded(&[
            started(1),
            added(2, "a", &["src"], &[]),
            added(3, "b", &["srcx"], &[]),
            spawned(4, "a", 1, "agent-1"),
            spawned(5, "b", 1, "agent-2"),
        ]);
        assert_eq!(status(&state, "a"), TaskStatus::Working);
        assert_eq!(status(&state, "b"), TaskStatus::Working);
    }

    #[test]
    fn a_hand_in_outside_its_declared_paths_is_recorded() {
        let log = [
            started(1),
            added(2, "a", &["src/a"], &[]),
            spawned(3, "a", 1, "agent-1"),
            reported(4, "a", 1, &["src/a/x.rs", "src/b/x.rs", "Cargo.lock"]),
        ];
        assert!(
            folded(&log[..3])
                .check_as(&log[3].actor, &log[3].kind, log[3].at_ms)
                .is_ok()
        );
        assert_eq!(status(&folded(&log), "a"), TaskStatus::Reported);
        assert!(matches!(
            refused_append(&[
                started(1),
                added(2, "a", &["src/a"], &[]),
                spawned(3, "a", 1, "agent-1"),
                reported(4, "a", 1, &["../escape"]),
            ]),
            RuleError::BadChangedPath { .. }
        ));
    }

    #[test]
    fn a_hand_in_that_commits_shprd_state_or_an_env_file_is_refused() {
        for path in [".shprd/events.jsonl", ".dagr/run.json", "web/.env.local"] {
            let log = [
                started(1),
                added(2, "a", &["src/a"], &[]),
                spawned(3, "a", 1, "agent-1"),
                reported(4, "a", 1, &["src/a/x.rs", path]),
            ];
            match refused_append(&log) {
                RuleError::ForbiddenPath { path: named, .. } => assert_eq!(named, path),
                other => panic!("{path}: expected ForbiddenPath, got {other:?}"),
            }
            // History is not re-judged: a log that already holds one folds.
            assert_eq!(status(&folded(&log), "a"), TaskStatus::Reported);
        }
        let ok = [
            started(1),
            added(2, "a", &["src/a"], &[]),
            spawned(3, "a", 1, "agent-1"),
            reported(4, "a", 1, &[".shprd/config.toml", ".env.example"]),
        ];
        assert!(
            folded(&ok[..3])
                .check_as(&ok[3].actor, &ok[3].kind, ok[3].at_ms)
                .is_ok()
        );
    }

    #[test]
    fn gate_needs_a_completed_report() {
        assert!(matches!(
            refused(&[
                started(1),
                added(2, "a", &["src/a"], &[]),
                spawned(3, "a", 1, "agent-1"),
                review_requested(4, "a", 1, "codex/m1", false),
                passed_review(4, "a", 1, 4),
            ]),
            RuleError::WrongStatus { .. }
        ));
        let blocked = ev(
            4,
            EventKind::Reported {
                task: "a".to_string(),
                attempt: 1,
                status: ReportStatus::Blocked,
                summary: "stuck".to_string(),
                tests: Vec::new(),
                checks: Vec::new(),
                changed_paths: strings(&["src/a"]),
                commit: None,
                skills_used: Vec::new(),
                proofs: Vec::new(),
            },
        );
        assert!(matches!(
            refused(&[
                started(1),
                added(2, "a", &["src/a"], &[]),
                spawned(3, "a", 1, "agent-1"),
                blocked,
                passed_review(5, "a", 1, 5),
            ]),
            RuleError::ReportNotCompleted { .. }
        ));
    }

    /// A completed hand-in without its checks is refused: the claim that the
    /// work is tested is exactly what the checks are for.
    #[test]
    fn a_hand_in_without_checks_is_refused() {
        assert!(matches!(
            refused_append(&[
                started(1),
                added(2, "a", &["src/a"], &[]),
                spawned(3, "a", 1, "agent-1"),
                hand_in(4, "a", 1, Vec::new()),
            ]),
            RuleError::HandInWithoutChecks { attempt: 1, .. }
        ));
        // A report that is not a hand-in needs none: blocked and failed work is
        // not claimed to be tested.
        let state = folded(&[
            started(1),
            added(2, "a", &["src/a"], &[]),
            spawned(3, "a", 1, "agent-1"),
            reported_with(4, "a", 1, ReportStatus::Blocked),
        ]);
        assert_eq!(status(&state, "a"), TaskStatus::Failed);
    }

    /// A failing gate that fails no worse than the base is accepted with its
    /// true exit code; a regression is refused; an agent cannot write a baseline.
    #[test]
    fn only_a_regression_refuses_a_failing_gate() {
        use crate::baseline::{BaselineEntry, Fingerprint};
        let baselined = |seq| {
            ev(
                seq,
                EventKind::GateBaselined {
                    base_sha: "abc".to_string(),
                    entries: vec![BaselineEntry {
                        command: "cargo clippy".to_string(),
                        passed: false,
                        exit_code: Some(1),
                        fingerprint: Fingerprint {
                            errors: Some(62),
                            files: Vec::new(),
                        },
                        base_sha: "abc".to_string(),
                    }],
                },
            )
        };
        let failing = |tail: &str| CheckReport {
            gate: "clippy".to_string(),
            command: "cargo clippy".to_string(),
            exit_code: Some(1),
            output_tail: tail.to_string(),
            pre_existing: None,
        };
        let run = |tail: &str| {
            let events = [
                started(1),
                baselined(2),
                added(3, "a", &["src/a"], &[]),
                spawned(4, "a", 1, "agent-1"),
                hand_in(5, "a", 1, vec![failing(tail)]),
            ];
            let (last, earlier) = events.split_last().expect("events");
            folded(earlier).check_as(&last.actor, &last.kind, last.at_ms)
        };
        assert!(run("62 errors").is_ok());
        assert!(matches!(
            run("70 errors"),
            Err(RuleError::HandInCheckFailed { .. })
        ));
        let forged = baselined(2);
        let state = folded(&[started(1)]);
        let agent = Actor::Agent {
            id: "agent-1".to_string(),
        };
        assert!(matches!(
            state.check_as(&agent, &forged.kind, 0),
            Err(RuleError::SyncNotAllowed { .. })
        ));
    }

    /// A hand-in that names a failing gate is refused, and the refusal names
    /// the gate and its exit code so the worker knows what to fix.
    #[test]
    fn a_hand_in_with_a_failing_check_is_refused() {
        let error = refused_append(&[
            started(1),
            added(2, "a", &["src/a"], &[]),
            spawned(3, "a", 1, "agent-1"),
            hand_in(
                4,
                "a",
                1,
                vec![check("format", Some(0)), check("clippy", Some(101))],
            ),
        ]);
        match error {
            RuleError::HandInCheckFailed { gate, exit, .. } => {
                assert_eq!(gate, "clippy");
                assert_eq!(exit, Some(101));
            }
            other => panic!("a failing check was accepted: {other:?}"),
        }
        // A check a timeout killed reports no exit code, which is not a pass.
        assert!(matches!(
            refused_append(&[
                started(1),
                added(2, "a", &["src/a"], &[]),
                spawned(3, "a", 1, "agent-1"),
                hand_in(4, "a", 1, vec![check("tests", None)]),
            ]),
            RuleError::HandInCheckFailed { exit: None, .. }
        ));
    }

    /// A checks report entry with no gate name or no command is refused: it
    /// records nothing a reviewer can act on.
    #[test]
    fn a_hand_in_check_needs_a_gate_and_a_command() {
        let mut nameless = check("tests", Some(0));
        nameless.gate = "  ".to_string();
        assert!(matches!(
            refused_append(&[
                started(1),
                added(2, "a", &["src/a"], &[]),
                spawned(3, "a", 1, "agent-1"),
                hand_in(4, "a", 1, vec![nameless]),
            ]),
            RuleError::EmptyCheck { .. }
        ));
        let mut empty = check("tests", Some(0));
        empty.command = String::new();
        assert!(matches!(
            refused_append(&[
                started(1),
                added(2, "a", &["src/a"], &[]),
                spawned(3, "a", 1, "agent-1"),
                hand_in(4, "a", 1, vec![empty]),
            ]),
            RuleError::EmptyCheck { .. }
        ));
    }

    /// A passing hand-in is stored with every gate it named.
    #[test]
    fn a_hand_in_with_passing_checks_folds_into_the_report() {
        let state = folded(&[
            started(1),
            added(2, "a", &["src/a"], &[]),
            spawned(3, "a", 1, "agent-1"),
            hand_in(
                4,
                "a",
                1,
                vec![check("format", Some(0)), check("tests", Some(0))],
            ),
        ]);
        assert_eq!(status(&state, "a"), TaskStatus::Reported);
        let report = match state
            .tasks
            .get("a")
            .and_then(|task| task.latest_attempt())
            .and_then(|attempt| attempt.report.as_ref())
        {
            Some(report) => report,
            None => panic!("the hand-in must be stored"),
        };
        let gates: Vec<&str> = report
            .checks
            .iter()
            .map(|check| check.gate.as_str())
            .collect();
        assert_eq!(gates, vec!["format", "tests"]);
    }

    /// A hand-in recorded before checks were required still folds: history is
    /// read, not re-judged.
    #[test]
    fn a_hand_in_recorded_before_checks_still_folds() {
        let mut old = reported(4, "a", 1, &["src/a"]);
        if let EventKind::Reported { checks, .. } = &mut old.kind {
            checks.clear();
        }
        let state = folded(&[
            started(1),
            added(2, "a", &["src/a"], &[]),
            spawned(3, "a", 1, "agent-1"),
            old,
        ]);
        assert_eq!(status(&state, "a"), TaskStatus::Reported);
    }

    /// A status line is a progress note: it folds onto the task without moving
    /// its status, and an empty, overlong or out-of-range one is refused.
    #[test]
    fn a_task_status_line_folds_without_moving_the_task() {
        let status_event = |seq: u64, message: &str, progress: Option<u8>| {
            ev(
                seq,
                EventKind::TaskStatus {
                    task: "a".to_string(),
                    message: message.to_string(),
                    progress,
                },
            )
        };
        let log = vec![
            started(1),
            added(2, "a", &["src/a"], &[]),
            spawned(3, "a", 1, "agent-1"),
            status_event(4, "reading the log", Some(25)),
        ];
        let state = folded(&log);
        assert_eq!(status(&state, "a"), TaskStatus::Working);
        let recorded = match state
            .tasks
            .get("a")
            .and_then(|task| task.last_status.as_ref())
        {
            Some(recorded) => recorded,
            None => panic!("the status line must fold"),
        };
        assert_eq!(recorded.message, "reading the log");
        assert_eq!(recorded.progress, Some(25));

        let before = folded(&log);
        let message_too_long = "x".repeat(TASK_STATUS_MAX_CHARS + 1);
        for (kind, expected) in [
            (
                EventKind::TaskStatus {
                    task: "a".to_string(),
                    message: "   ".to_string(),
                    progress: None,
                },
                "empty",
            ),
            (
                EventKind::TaskStatus {
                    task: "a".to_string(),
                    message: message_too_long,
                    progress: None,
                },
                "long",
            ),
            (
                EventKind::TaskStatus {
                    task: "a".to_string(),
                    message: "running the gates".to_string(),
                    progress: Some(101),
                },
                "progress",
            ),
        ] {
            let worker = Actor::Agent {
                id: "w".to_string(),
            };
            let error = match before.check_as(&worker, &kind, 5_000) {
                Ok(()) => panic!("a bad status line ({expected}) was accepted"),
                Err(err) => err,
            };
            match (expected, error) {
                ("empty", RuleError::EmptyStatusMessage { .. }) => {}
                ("long", RuleError::StatusMessageTooLong { found, max, .. }) => {
                    assert_eq!(found, TASK_STATUS_MAX_CHARS + 1);
                    assert_eq!(max, TASK_STATUS_MAX_CHARS);
                }
                ("progress", RuleError::ProgressOutOfRange { progress: 101, .. }) => {}
                (label, other) => panic!("{label} was refused with {other:?}"),
            }
        }
    }

    /// The engine may write a longer status line than a worker, to say why a
    /// step waits.
    #[test]
    fn the_engine_may_write_a_longer_status_line() {
        let state = folded(&reported_run());
        let kind = EventKind::TaskStatus {
            task: "a".to_string(),
            message: "x".repeat(TASK_STATUS_MAX_CHARS + 20),
            progress: None,
        };
        assert!(state.check_as(&Actor::Engine, &kind, 5_000).is_ok());
    }

    /// A status line for a task a human dropped is refused: the worker posting
    /// it was left behind by the drop.
    #[test]
    fn a_dropped_task_takes_no_status_line() {
        let mut log = reported_run();
        log.push(dropped(5, "not needed"));
        let state = folded(&log);
        let kind = EventKind::TaskStatus {
            task: "a".to_string(),
            message: "still going".to_string(),
            progress: None,
        };
        assert!(matches!(
            state.check_as(
                &Actor::Agent {
                    id: "agent-1".to_string()
                },
                &kind,
                7_000
            ),
            Err(RuleError::TaskIsDropped { .. })
        ));
    }

    /// A real review verdict (a pass) for `id`'s attempt, answering the request at `request_seq`.
    fn passed_review(seq: u64, id: &str, attempt: u32, request_seq: u64) -> Event {
        ev(
            seq,
            EventKind::GatePassed {
                task: id.to_string(),
                attempt,
                gate: "review".to_string(),
                findings: Vec::new(),
                source: Some(provenance_for("codex/m1", request_seq)),
            },
        )
    }

    fn review_requested(seq: u64, id: &str, attempt: u32, reviewer: &str, degraded: bool) -> Event {
        ev(
            seq,
            EventKind::ReviewRequested {
                task: id.to_string(),
                attempt,
                reviewer: reviewer.to_string(),
                degraded,
                note: None,
            },
        )
    }

    /// A review dispatch needs a reported attempt, and the one that reported.
    #[test]
    fn a_review_dispatch_needs_the_reported_attempt() {
        assert!(matches!(
            refused(&[
                started(1),
                added(2, "a", &["src/a"], &[]),
                spawned(3, "a", 1, "agent-1"),
                review_requested(4, "a", 1, "agent-2", false),
            ]),
            RuleError::WrongStatus { .. }
        ));
        assert!(matches!(
            refused(&[
                started(1),
                added(2, "a", &["src/a"], &[]),
                spawned(3, "a", 1, "agent-1"),
                reported(4, "a", 1, &["src/a"]),
                review_requested(5, "a", 2, "agent-2", false),
            ]),
            RuleError::AttemptOutOfOrder { .. }
        ));
        let state = folded(&[
            started(1),
            added(2, "a", &["src/a"], &[]),
            spawned(3, "a", 1, "agent-1"),
            reported(4, "a", 1, &["src/a"]),
            review_requested(5, "a", 1, "agent-2", true),
        ]);
        assert_eq!(status(&state, "a"), TaskStatus::Reported);
        let reviews = match state.tasks.get("a").and_then(|task| task.latest_attempt()) {
            Some(attempt) => attempt.reviews.clone(),
            None => panic!("the attempt is missing"),
        };
        assert_eq!(reviews.len(), 1);
        assert_eq!(reviews[0].reviewer, "agent-2");
        assert!(reviews[0].degraded);
    }

    /// A review may be requested twice for one undecided attempt, which is how
    /// a restart is recorded, and never a third time once it has a verdict.
    #[test]
    fn a_review_can_be_requested_again_until_it_is_decided() {
        let log = vec![
            started(1),
            added(2, "a", &["src/a"], &[]),
            spawned(3, "a", 1, "agent-1"),
            reported(4, "a", 1, &["src/a"]),
            review_requested(5, "a", 1, "agent-2", false),
            // The brief never arrived, so the review is started again.
            review_requested(6, "a", 1, "agent-2", false),
        ];
        let state = folded(&log);
        assert_eq!(status(&state, "a"), TaskStatus::Reported);
        let attempt = match state.tasks.get("a").and_then(|task| task.latest_attempt()) {
            Some(attempt) => attempt,
            None => panic!("the attempt is missing"),
        };
        assert_eq!(attempt.reviews.len(), 2, "one row per request");
        assert!(
            attempt.reviews.iter().all(|review| !review.degraded),
            "both requests describe the same, undegraded review"
        );
    }

    #[test]
    fn a_decided_review_cannot_be_requested_again() {
        // Both verdicts close the door: a pass and a fail are equally final for
        // this attempt, and a restart names the verdict rather than the status.
        for passed in [true, false] {
            let log = vec![
                started(1),
                added(2, "a", &["src/a"], &[]),
                spawned(3, "a", 1, "agent-1"),
                reported(4, "a", 1, &["src/a"]),
                review_requested(5, "a", 1, "agent-2", false),
                gated(6, "a", 1, "review", passed),
                review_requested(7, "a", 1, "agent-2", false),
            ];
            assert!(
                matches!(refused(&log), RuleError::ReviewDecided { .. }),
                "a verdict of passed={passed} must refuse a second request"
            );
        }
    }

    #[test]
    fn failed_or_blocked_report_can_be_retried() {
        for outcome in [ReportStatus::Failed, ReportStatus::Blocked] {
            let log = vec![
                started(1),
                added(2, "a", &["src/a"], &[]),
                spawned(3, "a", 1, "agent-1"),
                reported_with(4, "a", 1, outcome),
            ];
            let state = folded(&log);
            assert_eq!(status(&state, "a"), TaskStatus::Failed, "{outcome:?}");
            match state
                .tasks
                .get("a")
                .and_then(|task| task.attempts.first())
                .and_then(|attempt| attempt.report.as_ref())
            {
                Some(report) => assert_eq!(report.status, outcome, "the report is kept"),
                None => panic!("the report is recorded for {outcome:?}"),
            }

            let mut retried = log.clone();
            retried.push(spawned(5, "a", 2, "agent-1"));
            let retried = folded(&retried);
            assert_eq!(status(&retried, "a"), TaskStatus::Working, "{outcome:?}");
            assert_eq!(
                retried.tasks.get("a").map(|task| task.attempts.len()),
                Some(2),
                "a second attempt is recorded for {outcome:?}"
            );
        }
    }

    #[test]
    fn integrate_after_failed_gate_needs_an_override() {
        let base = [
            started(1),
            added(2, "a", &["src/a"], &[]),
            spawned(3, "a", 1, "agent-1"),
            reported(4, "a", 1, &["src/a/x.rs"]),
        ];
        let mut failed_then_passed = base.to_vec();
        failed_then_passed.push(review_requested(5, "a", 1, "codex/m1", false));
        failed_then_passed.push(gated(6, "a", 1, "clippy", false));
        assert!(matches!(
            refused(&[failed_then_passed.clone(), vec![integrated(6, "a")]].concat()),
            RuleError::NotPassed { .. }
        ));
        let mut clean = failed_then_passed.clone();
        clean.push(passed_review(7, "a", 1, 5));
        assert_eq!(status(&folded(&clean), "a"), TaskStatus::Passed);
        assert!(matches!(
            refused(&[clean.clone(), vec![integrated(8, "a")]].concat()),
            RuleError::FailedGate { .. }
        ));

        let mut overridden = clean.clone();
        overridden.push(overrode(8, "a", INTEGRATE_AFTER_FAILED_GATE));
        let overridden = [overridden, vec![integrated(9, "a")]].concat();
        assert_eq!(status(&folded(&overridden), "a"), TaskStatus::Integrated);
    }

    #[test]
    fn override_before_the_failed_gate_does_not_count() {
        let log = vec![
            started(1),
            added(2, "a", &["src/a"], &[]),
            overrode(3, "a", INTEGRATE_AFTER_FAILED_GATE),
            spawned(4, "a", 1, "agent-1"),
            reported(5, "a", 1, &["src/a/x.rs"]),
            review_requested(6, "a", 1, "codex/m1", false),
            gated(7, "a", 1, "clippy", false),
            passed_review(8, "a", 1, 6),
            integrated(9, "a"),
        ];
        assert!(matches!(refused(&log), RuleError::FailedGate { .. }));
    }

    #[test]
    fn override_needs_a_rule_and_a_reason() {
        assert!(matches!(
            refused(&[started(1), overrode(2, "a", INTEGRATE_AFTER_FAILED_GATE)]),
            RuleError::UnknownTask { .. }
        ));
        assert!(matches!(
            refused(&[
                started(1),
                added(2, "a", &["src/a"], &[]),
                overrode(3, "a", " ")
            ]),
            RuleError::EmptyOverride { .. }
        ));
    }

    /// A folded log holding one task whose attempt passed its gate.
    fn passed_task() -> RunState {
        folded(&[
            started(1),
            added(2, "a", &["src/a"], &[]),
            spawned(3, "a", 1, "agent-1"),
            reported(4, "a", 1, &["src/a/x.rs"]),
            review_requested(5, "a", 1, "codex/m1", false),
            passed_review(5, "a", 1, 5),
        ])
    }

    /// An override of `rule` for `task`, as a verb would offer it.
    fn override_kind(task: &str, rule: &str) -> EventKind {
        EventKind::Override {
            task: task.to_string(),
            rule: rule.to_string(),
            reason: "shipping anyway".to_string(),
        }
    }

    #[test]
    fn only_a_human_appends_an_override() {
        let state = passed_task();
        let kind = override_kind("a", INTEGRATE_AFTER_FAILED_GATE);
        let others = [
            Actor::Engine,
            Actor::Orchestrator,
            Actor::Agent {
                id: "w1".to_string(),
            },
        ];
        for actor in others {
            match check_as(&state, &actor, &kind, 6_000) {
                Err(RuleError::OverrideNotHuman { actor: named }) => {
                    assert_eq!(named, actor, "the refusal names who tried");
                }
                other => panic!("{actor:?} must not override, got {other:?}"),
            }
        }
        assert!(check_as(&state, &Actor::Human, &kind, 6_000).is_ok());
    }

    #[test]
    fn a_new_widening_is_refused_and_an_old_one_still_folds() {
        let state = passed_task();
        let kind = EventKind::ScopeWidened {
            task: "a".to_string(),
            paths: vec!["tests/x.py".to_string()],
        };
        assert!(matches!(
            check_as(&state, &Actor::Human, &kind, 6_000),
            Err(RuleError::ScopeWideningRetired { .. })
        ));
        let log = [started(1), added(2, "a", &["src/a"], &[]), ev(3, kind)];
        assert!(
            folded(&log).tasks["a"]
                .scope
                .contains(&"tests/x.py".to_string())
        );
    }

    #[test]
    fn an_old_scope_override_folds_and_a_new_one_is_refused() {
        let kind = EventKind::Override {
            task: "a".to_string(),
            rule: "accept_outside_scope".to_string(),
            reason: "paths=src/b/y.rs; why".to_string(),
        };
        assert!(matches!(
            check_as(&passed_task(), &Actor::Human, &kind, 6_000),
            Err(RuleError::UnknownOverrideRule { .. })
        ));
        let mut old = ev(3, kind);
        old.actor = Actor::Human;
        let state = folded(&[started(1), added(2, "a", &["src/a"], &[]), old]);
        assert_eq!(state.tasks["a"].overrides.len(), 1);
    }

    #[test]
    fn only_a_rule_the_run_reads_may_be_overridden() {
        let state = passed_task();
        assert!(matches!(
            check_as(
                &state,
                &Actor::Human,
                &override_kind("a", "integrate_after_failed_gate_typo"),
                6_000
            ),
            Err(RuleError::UnknownOverrideRule { .. })
        ));
    }

    #[test]
    fn an_old_override_still_folds_and_still_counts() {
        // History is not re-judged: an override an engine or an orchestrator
        // recorded before the rule existed still folds, and still lets the
        // task it named integrate past the gate that failed.
        let mut old = overrode(7, "a", INTEGRATE_AFTER_FAILED_GATE);
        old.actor = Actor::Orchestrator;
        let log = vec![
            started(1),
            added(2, "a", &["src/a"], &[]),
            spawned(3, "a", 1, "agent-1"),
            reported(4, "a", 1, &["src/a/x.rs"]),
            review_requested(5, "a", 1, "codex/m1", false),
            gated(6, "a", 1, "clippy", false),
            passed_review(7, "a", 1, 5),
            old,
            integrated(8, "a"),
        ];
        assert_eq!(status(&folded(&log), "a"), TaskStatus::Integrated);
    }

    #[test]
    fn an_old_override_of_a_name_nothing_reads_still_folds() {
        let mut old = overrode(3, "", "no_main_push");
        old.actor = Actor::Orchestrator;
        let log = vec![started(1), old];
        assert_eq!(folded(&log).run_overrides.len(), 1);
    }

    #[test]
    fn dry_agent_is_refused_until_reset() {
        // seq 3 dries agent-1 until seq 5 (5000 ms); spawns at seq 4 and 6.
        let blocked = vec![
            started(1),
            added(2, "a", &["src/a"], &[]),
            dried(3, "agent-1", Some(5000)),
            spawned(4, "a", 1, "agent-1"),
        ];
        assert!(matches!(refused(&blocked), RuleError::AgentDry { .. }));
        let allowed = vec![
            started(1),
            added(2, "a", &["src/a"], &[]),
            dried(3, "agent-1", Some(5000)),
            spawned(6, "a", 1, "agent-1"),
        ];
        assert_eq!(status(&folded(&allowed), "a"), TaskStatus::Working);
    }

    #[test]
    fn dry_agent_without_reset_stays_dry() {
        let log = vec![
            started(1),
            added(2, "a", &["src/a"], &[]),
            dried(3, "agent-1", None),
            spawned(4, "a", 1, "agent-1"),
        ];
        assert!(matches!(refused(&log), RuleError::AgentDry { .. }));
    }

    #[test]
    fn available_readmits_a_dry_agent() {
        let log = vec![
            started(1),
            added(2, "a", &["src/a"], &[]),
            dried(3, "agent-1", None),
            spawned(4, "a", 1, "agent-1"),
        ];
        assert!(matches!(refused(&log), RuleError::AgentDry { .. }));
        let readmitted = vec![
            started(1),
            added(2, "a", &["src/a"], &[]),
            dried(3, "agent-1", None),
            available(4, "agent-1"),
            spawned(5, "a", 1, "agent-1"),
        ];
        assert_eq!(status(&folded(&readmitted), "a"), TaskStatus::Working);
        assert!(!folded(&readmitted).agents["agent-1"].dry);
    }

    #[test]
    fn only_the_engine_or_a_human_may_re_admit_an_agent() {
        let state = folded(&[started(1), dried(2, "agent-1", None)]);
        let kind = available(3, "agent-1").kind;
        for actor in [Actor::Engine, Actor::Human] {
            assert!(
                state.check_as(&actor, &kind, 3_000).is_ok(),
                "{actor:?} may re-admit: the watchdog and a human both do"
            );
        }
        let agent = Actor::Agent {
            id: "agent-1".to_string(),
        };
        for actor in [Actor::Orchestrator, agent] {
            match state.check_as(&actor, &kind, 3_000) {
                Err(err) => assert!(matches!(err, RuleError::ReAdmitNotAllowed { .. }), "{err}"),
                Ok(()) => panic!("{actor:?} must not clear a cooldown"),
            }
        }
    }

    #[test]
    fn a_re_admission_still_needs_a_dry_agent() {
        let state = folded(&[started(1), added(2, "a", &["src/a"], &[])]);
        let kind = available(3, "agent-1").kind;
        assert!(matches!(
            state.check_as(&Actor::Human, &kind, 3_000),
            Err(RuleError::AgentNotDry { .. })
        ));
    }

    #[test]
    fn available_is_refused_for_an_agent_that_is_not_dry() {
        let log = vec![
            started(1),
            added(2, "a", &["src/a"], &[]),
            available(3, "agent-1"),
        ];
        assert!(matches!(refused(&log), RuleError::AgentNotDry { .. }));
    }

    #[test]
    fn handed_off_moves_the_latest_attempt() {
        let state = folded(&[
            started(1),
            added(2, "a", &["src/a"], &[]),
            spawned(3, "a", 1, "agent-1"),
            ev(
                4,
                EventKind::HandedOff {
                    task: "a".to_string(),
                    from_agent: "agent-1".to_string(),
                    to_agent: "agent-2".to_string(),
                    checkpoint: "carry on".to_string(),
                    reason: Some("rate limited".to_string()),
                },
            ),
        ]);
        match state.tasks.get("a") {
            Some(task) => match task.latest_attempt() {
                Some(attempt) => assert_eq!(attempt.agent, "agent-2"),
                None => panic!("attempt missing"),
            },
            None => panic!("task a missing"),
        }
    }

    #[test]
    fn an_escalation_remembers_whether_an_agent_wrote_it() {
        // SHPR-83: a coordinator's claim is an opinion, the engine's card a fact: the fold must say which.
        let mut by_agent = ev(
            3,
            EventKind::Escalated {
                task: Some("a".to_string()),
                body: "a worker never asked".to_string(),
                default_action: "wait".to_string(),
            },
        );
        by_agent.actor = Actor::Agent {
            id: "claude".to_string(),
        };
        let mut by_engine = by_agent.clone();
        by_engine.seq = 4;
        by_engine.actor = Actor::Engine;
        let state = folded(&[
            started(1),
            added(2, "a", &["src/a"], &[]),
            by_agent,
            by_engine,
        ]);
        let flags: Vec<bool> = state.escalations.iter().map(|e| e.by_agent).collect();
        assert_eq!(flags, vec![true, false]);
    }

    #[test]
    fn only_a_person_dismisses_a_card_that_exists() {
        let base = vec![
            started(1),
            added(2, "a", &["src/a"], &[]),
            ev(
                3,
                EventKind::Escalated {
                    task: Some("a".to_string()),
                    body: "which branch?".to_string(),
                    default_action: "use main".to_string(),
                },
            ),
        ];
        let dismiss = |seq: u64, actor: Actor| {
            let mut log = base.clone();
            log.push(Event {
                seq: 4,
                at_ms: 4000,
                actor,
                kind: EventKind::NeedDismissed { need: seq },
            });
            log
        };
        let state = folded(&dismiss(3, Actor::Human));
        assert!(state.dismissed_needs.contains(&3));
        let agent = Actor::Agent {
            id: "claude".to_string(),
        };
        assert!(matches!(
            refused_append(&dismiss(3, agent)),
            RuleError::DismissNotHuman { .. }
        ));
        assert!(matches!(
            refused_append(&dismiss(99, Actor::Human)),
            RuleError::UnknownNeed { seq: 99 }
        ));
    }

    #[test]
    fn escalation_is_recorded() {
        let state = folded(&[
            started(1),
            added(2, "a", &["src/a"], &[]),
            ev(
                3,
                EventKind::Escalated {
                    task: Some("a".to_string()),
                    body: "which branch?".to_string(),
                    default_action: "use main".to_string(),
                },
            ),
        ]);
        assert_eq!(state.escalations.len(), 1);
        match state.escalations.first() {
            Some(item) => assert_eq!(item.task.as_deref(), Some("a")),
            None => panic!("escalation missing"),
        }
    }

    #[test]
    fn finish_while_working_is_refused() {
        let log = vec![
            started(1),
            added(2, "a", &["src/a"], &[]),
            spawned(3, "a", 1, "agent-1"),
            finished(4),
        ];
        assert!(matches!(refused(&log), RuleError::WrongStatus { .. }));
    }

    #[test]
    fn abandoning_a_working_task_is_recorded_rather_than_refused() {
        // `--abandon` is a human's explicit choice to end the run anyway, so
        // the event carries the list of tasks it left; the fold keeps it.
        let abandoned = vec!["a".to_string()];
        let log = vec![
            started(1),
            added(2, "a", &["src/a"], &[]),
            spawned(3, "a", 1, "agent-1"),
            by(4, Actor::Human, abandoning(4, &["a"]).kind),
        ];
        let state = folded(&log);
        assert!(state.finished);
        assert_eq!(state.abandoned, abandoned);
    }

    #[test]
    fn a_base_sync_is_never_an_agent_s() {
        // Merging the base into the run's own branch moves the head the gate
        // and the pull request are pinned to, so a worker may not do it: the
        // orchestrator runs it before the gate, and a human by hand.
        let state = folded(&in_flight());
        let kind = EventKind::BaseSynced {
            base_sha: "abc1234".to_string(),
            int_sha: "def5678".to_string(),
        };
        for actor in [Actor::Human, Actor::Orchestrator, Actor::Engine] {
            state
                .check_as(&actor, &kind, 13_000)
                .unwrap_or_else(|err| panic!("{actor:?} may sync the base: {err}"));
        }
        let agent = Actor::Agent {
            id: "claude".to_string(),
        };
        match state.check_as(&agent, &kind, 13_000) {
            Err(err) => assert!(matches!(err, RuleError::SyncNotAllowed { .. }), "{err}"),
            Ok(()) => panic!("an agent must not sync the base"),
        }
    }

    #[test]
    fn only_a_human_abandons_a_run() {
        // Abandoning closes the run on work nobody finished. An agent that
        // could do it would walk away from its own task and shut the tabs,
        // which is the incident this rule exists for.
        let state = folded(&in_flight());
        let kind = abandoning(13, &["a", "b"]).kind;
        let others = [
            Actor::Orchestrator,
            Actor::Engine,
            Actor::Agent {
                id: "claude".to_string(),
            },
        ];
        for actor in others {
            match state.check_as(&actor, &kind, 13_000) {
                Err(RuleError::AbandonNotHuman { actor: who }) => assert_eq!(who, actor),
                other => panic!("{actor:?} abandoned the run: {other:?}"),
            }
        }
        state
            .check_as(&Actor::Human, &kind, 13_000)
            .unwrap_or_else(|err| panic!("the human was refused: {err}"));
    }

    #[test]
    fn an_abandon_must_name_exactly_the_unfinished_work() {
        // The list is the log's record of what the run left, so it is checked
        // against the work actually unfinished: nothing more, nothing less.
        let state = folded(&in_flight());
        // `c` landed, so it is not the abandon's to name; order is not the
        // question, the set is.
        state
            .check_as(&Actor::Human, &abandoning(13, &["b", "a"]).kind, 13_000)
            .unwrap_or_else(|err| panic!("the true list was refused: {err}"));
        // A task left out under-reports what was stranded.
        match state.check_as(&Actor::Human, &abandoning(13, &["a"]).kind, 13_000) {
            Err(RuleError::AbandonListMismatch { missing, extra }) => {
                assert_eq!(missing, strings(&["b"]));
                assert!(extra.is_empty(), "{extra:?}");
            }
            other => panic!("a short list was accepted: {other:?}"),
        }
        // A task that landed, or one no plan ever had, is blamed for nothing.
        for extra in ["c", "ghost"] {
            match state.check_as(
                &Actor::Human,
                &abandoning(13, &["a", "b", extra]).kind,
                13_000,
            ) {
                Err(RuleError::AbandonListMismatch {
                    missing,
                    extra: listed,
                }) => {
                    assert!(missing.is_empty(), "{missing:?}");
                    assert_eq!(listed, strings(&[extra]));
                }
                other => panic!("a list naming {extra:?} was accepted: {other:?}"),
            }
        }
    }

    #[test]
    fn only_a_human_or_the_orchestrator_finishes_a_run() {
        // The orchestrator's brief ends the run with `shprd run finish` after
        // the merge, so it may finish one; an agent and the engine never do.
        let state = folded(&landed());
        for actor in [Actor::Human, Actor::Orchestrator] {
            state
                .check_as(&actor, &finish_kind(), 9_000)
                .unwrap_or_else(|err| panic!("{actor:?} was refused: {err}"));
        }
        for actor in [
            Actor::Engine,
            Actor::Agent {
                id: "claude".to_string(),
            },
        ] {
            match state.check_as(&actor, &finish_kind(), 9_000) {
                Err(RuleError::FinishNotAllowed { actor: who }) => assert_eq!(who, actor),
                other => panic!("{actor:?} finished the run: {other:?}"),
            }
        }
    }

    #[test]
    fn a_finish_is_refused_while_the_work_is_unfinished() {
        // Even the orchestrator's finish needs the work over: a reported task
        // is work in review, and ending the run would strand it.
        let state = folded(&in_flight());
        match state.check_as(&Actor::Orchestrator, &finish_kind(), 13_000) {
            Err(RuleError::RunUnfinished { tasks }) => assert_eq!(tasks, strings(&["a", "b"])),
            other => panic!("a finish over unfinished work was accepted: {other:?}"),
        }
    }

    #[test]
    fn a_finish_is_refused_until_the_pull_request_merges() {
        // Every task landed, but the branch carries the work only once the
        // pull request merged; until then the run would end with nothing
        // carrying it.
        let mut log = landed();
        log.truncate(7);
        let state = folded(&log);
        assert!(matches!(
            state.check_as(&Actor::Orchestrator, &finish_kind(), 9_000),
            Err(RuleError::PullRequestUnmerged)
        ));
    }

    #[test]
    fn a_human_abandons_a_run_whose_pull_request_never_merged() {
        // Nothing is unfinished, so a normal finish is refused: no pull request
        // carries the work. `--abandon` names what that pull request would have
        // carried — every integrated task — and that is what the refusal tells
        // a human to run, so it has to work.
        let state = folded(&unmerged());
        assert!(unfinished_tasks(&state).is_empty());
        assert_eq!(abandonable_tasks(&state), strings(&["a", "b"]));
        match state.check_as(&Actor::Orchestrator, &finish_kind(), 13_000) {
            Err(RuleError::PullRequestUnmerged) => {}
            other => panic!("a finish over unmerged work was accepted: {other:?}"),
        }
        state
            .check_as(&Actor::Human, &abandoning(13, &["a", "b"]).kind, 13_000)
            .unwrap_or_else(|err| panic!("the abandon was refused: {err}"));
    }

    #[test]
    fn an_abandon_on_an_unmerged_run_must_name_every_landed_task() {
        // The integrated tasks are the record of what the run left on a branch
        // nothing carried, so leaving one out under-reports it.
        let state = folded(&unmerged());
        match state.check_as(&Actor::Human, &abandoning(13, &["a"]).kind, 13_000) {
            Err(RuleError::AbandonListMismatch { missing, extra }) => {
                assert_eq!(missing, strings(&["b"]));
                assert!(extra.is_empty(), "{extra:?}");
            }
            other => panic!("a short list was accepted: {other:?}"),
        }
        // An unmerged run is still not an agent's to abandon.
        for actor in [
            Actor::Orchestrator,
            Actor::Engine,
            Actor::Agent {
                id: "claude".to_string(),
            },
        ] {
            match state.check_as(&actor, &abandoning(13, &["a", "b"]).kind, 13_000) {
                Err(RuleError::AbandonNotHuman { actor: who }) => assert_eq!(who, actor),
                other => panic!("{actor:?} abandoned the run: {other:?}"),
            }
        }
    }

    #[test]
    fn nothing_is_abandonable_once_the_work_landed_and_merged() {
        // A merged run leaves nothing behind, so `--abandon` is a normal finish
        // rather than a list. So is a run with no tasks: nothing carries work.
        assert!(abandonable_tasks(&folded(&landed())).is_empty());
        assert!(abandonable_tasks(&folded(&[started(1)])).is_empty());
    }

    #[test]
    fn a_finish_no_human_recorded_still_folds() {
        // Old logs hold finishes any actor recorded, with lists nothing
        // checked. Folding a run is not recording it.
        let log = vec![by(4, Actor::Engine, abandoning(4, &["ghost"]).kind)];
        let mut full = vec![started(1), added(2, "a", &["src/a"], &[])];
        full.extend(log);
        let state = folded(&full);
        assert!(state.finished);
        let engine_finished = vec![started(1), by(2, Actor::Engine, finish_kind())];
        assert!(folded(&engine_finished).finished);
    }

    #[test]
    fn a_full_gate_needs_a_live_run() {
        assert!(matches!(refused(&[full_gate(1, true)]), RuleError::NoRun));
        let log = vec![started(1), full_gate(2, true), finished(3)];
        let state = folded(&log);
        assert!(state.finished);
        let after = vec![started(1), finished(2), full_gate(3, true)];
        assert!(matches!(refused(&after), RuleError::RunFinished));
    }

    #[test]
    fn nothing_follows_run_finished() {
        let log = vec![started(1), finished(2), added(3, "a", &["src/a"], &[])];
        assert!(matches!(refused(&log), RuleError::RunFinished));
    }

    #[test]
    fn archiving_needs_a_live_resource() {
        let created = res(
            3,
            ResourceAction::Created,
            ResourceKind::Branch,
            "shprd/r1/a",
        );
        let archived = res(
            4,
            ResourceAction::Archived,
            ResourceKind::Branch,
            "shprd/r1/a",
        );
        let head = vec![started(1), added(2, "a", &["src/a"], &[]), created];
        let state = folded(&[head.clone(), vec![archived.clone()]].concat());
        assert_eq!(
            state
                .resources
                .get(&(ResourceKind::Branch, "shprd/r1/a".to_string()))
                .map(|record| record.status),
            Some(ResourceStatus::Archived)
        );
        let again = res(
            5,
            ResourceAction::Archived,
            ResourceKind::Branch,
            "shprd/r1/a",
        );
        let log = [head, vec![archived, again]].concat();
        assert!(matches!(
            refused(&log),
            RuleError::WrongResourceStatus { .. }
        ));
    }

    #[test]
    fn removing_review_packets_the_ledger_never_held_is_allowed_after_the_run_finished() {
        // A review packet folder is shprd's own directory, named by the layout
        // and a validated run id. An earlier shprd never recorded it, and a
        // finished run cannot create a record, so the removal is the only way
        // to close the book on a folder the janitor archived and deleted.
        let log = vec![
            started(1),
            finished(2),
            res_run(
                3,
                ResourceAction::Removed,
                ResourceKind::ReviewPackets,
                "/home/.shprd/reviews/r1",
            ),
        ];
        let state = folded(&log);
        assert!(state.finished, "the removal is legal once the run is over");
        assert!(
            state.resources.is_empty(),
            "a folder that was never recorded has no record to close: {state:?}"
        );
    }

    #[test]
    fn archiving_review_packets_the_ledger_never_held_is_still_refused() {
        // Only removal is the record of a cleanup: archiving something the
        // ledger never held would claim shprd kept a record it never made.
        let log = vec![
            started(1),
            finished(2),
            res_run(
                3,
                ResourceAction::Archived,
                ResourceKind::ReviewPackets,
                "/home/.shprd/reviews/r1",
            ),
        ];
        assert!(matches!(refused(&log), RuleError::ResourceUnknown { .. }));
    }

    #[test]
    fn removing_any_other_kind_the_ledger_never_held_is_still_refused() {
        // The exception is review packets and nothing else: shprd still cleans
        // up only what it recorded creating, even after the run has finished,
        // where only the finished-run guard would have let a removal through.
        for kind in [
            ResourceKind::Worktree,
            ResourceKind::Branch,
            ResourceKind::RemoteBranch,
            ResourceKind::ArchiveRef,
            ResourceKind::Bundle,
        ] {
            let log = vec![
                started(1),
                finished(2),
                res_run(
                    3,
                    ResourceAction::Removed,
                    kind,
                    "/tmp/shprd/never-recorded",
                ),
            ];
            assert!(
                matches!(
                    refused(&log),
                    RuleError::ResourceUnknown { kind: refused, .. } if refused == kind
                ),
                "{kind:?} must keep the rule that shprd cleans up only what it recorded"
            );
        }
    }

    #[test]
    fn check_refuses_without_mutating() {
        let state = folded(&[started(1), added(2, "a", &["src/a"], &[])]);
        let kind = EventKind::Spawned {
            task: "a".to_string(),
            attempt: 1,
            agent: "agent-1".to_string(),
            model: None,
            worktree: "/wt/a".to_string(),
            branch: "feature/a".to_string(),
        };
        assert!(state.check(&kind, 2_000).is_ok());
        assert_eq!(state.last_seq, 2);
        assert_eq!(status(&state, "a"), TaskStatus::Queued);
        assert!(matches!(
            state.check(&integrated(3, "a").kind, 3_000),
            Err(RuleError::NotPassed { .. })
        ));
    }
    #[test]
    fn only_the_engine_records_a_review_verdict() {
        let state = folded(&reviewable());
        let others = [
            Actor::Orchestrator,
            Actor::Human,
            Actor::Agent {
                id: "claude".to_string(),
            },
        ];
        for actor in others {
            match state.check_as(&actor, &review_gate(true, Some(provenance())), 5_000) {
                Err(RuleError::VerdictNotFromReviewer { task }) => assert_eq!(task, "a"),
                other => panic!("{actor:?} recorded a verdict: {other:?}"),
            }
        }
        // The engine may, because it is the one that reads the packet.
        state
            .check_as(
                &Actor::Engine,
                &review_gate(true, Some(provenance())),
                5_000,
            )
            .unwrap_or_else(|err| panic!("the engine was refused: {err}"));
    }

    #[test]
    fn a_review_verdict_without_provenance_is_refused() {
        let state = folded(&reviewable());
        for passed in [true, false] {
            match state.check_as(&Actor::Engine, &review_gate(passed, None), 5_000) {
                Err(RuleError::UnprovenVerdict { task }) => assert_eq!(task, "a"),
                other => panic!("an unproven verdict was recorded (passed: {passed}): {other:?}"),
            }
        }
    }

    #[test]
    fn a_review_verdict_recorded_before_provenance_still_folds() {
        // Old logs hold verdicts no engine recorded and no source proves.
        // Replay keeps them: reading a run's history is not recording it.
        let mut log = reviewable();
        log.push(by(6, Actor::Orchestrator, review_gate(true, None)));
        let state = folded(&log);
        let gate = state
            .tasks
            .get("a")
            .and_then(|task| task.attempts.last())
            .and_then(|attempt| attempt.gates.last())
            .unwrap_or_else(|| panic!("no gate recorded: {state:?}"));
        assert!(gate.passed);
        assert_eq!(gate.source, None);
    }

    #[test]
    fn a_proven_review_verdict_folds_into_its_attempt() {
        let mut log = reviewable();
        log.push(ev(6, review_gate(true, Some(provenance()))));
        let state = folded(&log);
        let gate = state
            .tasks
            .get("a")
            .and_then(|task| task.attempts.last())
            .and_then(|attempt| attempt.gates.last())
            .unwrap_or_else(|| panic!("no gate recorded: {state:?}"));
        assert!(gate.passed);
        assert_eq!(gate.source, Some(provenance()));
        assert_eq!(status(&state, "a"), TaskStatus::Passed);
    }

    /// A verdict that answers a request a restart has superseded is refused.
    ///
    /// Reading the packet and appending the verdict are two steps, and
    /// `review --again` can record a fresh request in between. The old review's
    /// verdict then names the request it read, which is no longer the latest.
    #[test]
    fn a_verdict_for_a_superseded_review_request_is_refused() {
        let mut log = reviewable();
        log.push(review_requested(6, "a", 1, "codex/m1", false));
        let state = folded(&log);
        assert_eq!(state.last_seq, 6);
        match state.check_as(
            &Actor::Engine,
            &review_gate(true, Some(provenance())),
            7_000,
        ) {
            Err(RuleError::StaleReviewVerdict { task, attempt }) => {
                assert_eq!(task, "a");
                assert_eq!(attempt, 1);
            }
            other => panic!("a superseded request was answered: {other:?}"),
        }
        // The restart may name another reviewer, which the superseded verdict
        // names too: the request's seq is what refuses it either way.
        let mut log = reviewable();
        log.push(review_requested(6, "a", 1, "claude/m1", false));
        let state = folded(&log);
        assert!(matches!(
            state.check_as(
                &Actor::Engine,
                &review_gate(false, Some(provenance())),
                7_000
            ),
            Err(RuleError::StaleReviewVerdict { .. })
        ));
    }

    /// A verdict for the latest request is accepted, whoever it names.
    #[test]
    fn a_verdict_for_the_latest_review_request_is_accepted() {
        let mut log = reviewable();
        log.push(review_requested(6, "a", 1, "claude/m1", false));
        let state = folded(&log);
        state
            .check_as(
                &Actor::Engine,
                &review_gate(true, Some(provenance_for("claude/m1", 6))),
                7_000,
            )
            .unwrap_or_else(|err| panic!("the latest request's verdict was refused: {err}"));
        // The same reviewer asked twice is two requests, and only the newer one
        // is still answerable.
        let mut log = reviewable();
        log.push(review_requested(6, "a", 1, "codex/m1", false));
        let state = folded(&log);
        state
            .check_as(
                &Actor::Engine,
                &review_gate(true, Some(provenance_for("codex/m1", 6))),
                7_000,
            )
            .unwrap_or_else(|err| panic!("the latest request's verdict was refused: {err}"));
    }

    /// A verdict from before requests carried a seq still folds.
    ///
    /// The field defaults to `0`, which no request has, so such a verdict can
    /// never be recorded now; replay keeps it because a run's history is read,
    /// not re-judged, exactly as a verdict without provenance is kept. A second
    /// verdict on the same attempt is refused for the reason that matters most:
    /// the review already has one.
    #[test]
    fn a_legacy_verdict_without_a_request_seq_still_folds() {
        let mut log = reviewable();
        log.push(ev(
            6,
            review_gate(true, Some(provenance_for("codex/m1", 0))),
        ));
        let state = folded(&log);
        assert_eq!(status(&state, "a"), TaskStatus::Passed);
        // Recording the same thing now is refused, never accepted as a second
        // verdict: absence is a legacy reading, never a way past the rule.
        assert!(matches!(
            state.check_as(
                &Actor::Engine,
                &review_gate(true, Some(provenance_for("codex/m1", 0))),
                7_000
            ),
            Err(RuleError::ReviewAlreadyDecided { .. })
        ));
    }

    /// One review is decided once, whatever answers it.
    ///
    /// The M9 gate's second run found a `shprd watch` and the service's pass both
    /// deciding one reviewer's packet: both plan from a state that says the
    /// review is owed, and a second `gate_passed` answering the same request used
    /// to be accepted. The log under its lock is the only place that can tell the
    /// second decider the first has already recorded the verdict, so the rules
    /// are what refuse it.
    #[test]
    fn a_second_verdict_for_one_attempt_is_refused() {
        for first in [true, false] {
            for second in [true, false] {
                let mut log = reviewable();
                log.push(ev(
                    6,
                    review_gate(first, Some(provenance_for("codex/m1", 5))),
                ));
                let state = folded(&log);
                assert!(
                    matches!(
                        state.check_as(
                            &Actor::Engine,
                            &review_gate(second, Some(provenance_for("codex/m1", 5))),
                            7_000
                        ),
                        Err(RuleError::ReviewAlreadyDecided { .. })
                    ),
                    "a {second} verdict after a {first} one must be refused"
                );
            }
        }
        // A second attempt is a new review, so it is not refused: the refusal is
        // per attempt, not per task. A failed verdict is what sends the task back
        // to a worker, which is what a new attempt needs.
        let mut log = reviewable();
        log.push(ev(
            6,
            review_gate(false, Some(provenance_for("codex/m1", 5))),
        ));
        log.push(spawned(7, "a", 2, "agent-1"));
        log.push(reported(8, "a", 2, &["src/a/x.rs"]));
        log.push(review_requested(9, "a", 2, "codex/m1", false));
        let state = folded(&log);
        let second = EventKind::GatePassed {
            task: "a".to_string(),
            attempt: 2,
            gate: "review".to_string(),
            findings: Vec::new(),
            source: Some(provenance_for("codex/m1", 9)),
        };
        state
            .check_as(&Actor::Engine, &second, 10_000)
            .unwrap_or_else(|err| panic!("a new attempt's verdict was refused: {err}"));
    }

    /// A human's drop of task `a`, at `seq`.
    fn dropped(seq: u64, reason: &str) -> Event {
        by(
            seq,
            Actor::Human,
            EventKind::TaskDropped {
                task: "a".to_string(),
                reason: reason.to_string(),
            },
        )
    }

    /// A run whose task `a` reported and waits for a gate.
    fn reported_run() -> Vec<Event> {
        vec![
            started(1),
            added(2, "a", &["src/a"], &[]),
            spawned(3, "a", 1, "agent-1"),
            reported(4, "a", 1, &["src/a/x.rs"]),
        ]
    }

    #[test]
    fn a_human_drops_a_reported_task_and_it_leaves_the_unfinished_work() {
        let mut log = reported_run();
        log.push(dropped(5, "the human changed their mind"));
        let state = folded(&log);
        assert_eq!(status(&state, "a"), TaskStatus::Dropped);
        assert_eq!(
            state
                .tasks
                .get("a")
                .and_then(|task| task.dropped_reason.clone()),
            Some("the human changed their mind".to_string())
        );
        assert!(
            unfinished_tasks(&state).is_empty(),
            "a dropped task is not work the run still owes"
        );
        assert!(
            abandonable_tasks(&state).is_empty(),
            "nothing is integrated, so there is nothing an abandon would carry"
        );
    }

    /// A person stopping the agent on task `id` attempt `attempt`, at `seq`.
    fn stopped(seq: u64, id: &str, attempt: u32) -> Event {
        by(
            seq,
            Actor::Human,
            EventKind::TaskStopped {
                task: id.to_string(),
                attempt,
            },
        )
    }

    #[test]
    fn a_human_stops_the_agent_on_a_working_task_and_it_may_be_retried() {
        // Bug 1 of the M10 gate: the stop only closed the space, so the fold
        // kept the task Working for ever and a retry was refused. The event is
        // the fact that makes the task Failed — which a retry may replace — and
        // marks the attempt stopped.
        let mut log = reported_run();
        log.truncate(3);
        log.push(stopped(4, "a", 1));
        let state = folded(&log);
        assert_eq!(status(&state, "a"), TaskStatus::Failed);
        let attempt = state
            .tasks
            .get("a")
            .and_then(|task| task.latest_attempt())
            .expect("the attempt");
        assert!(attempt.stopped, "the attempt is marked stopped");
        // And the task is in the state a retry may replace, which is the whole
        // point: a new attempt's spawn is accepted.
        state
            .check_as(&Actor::Engine, &spawned(5, "a", 2, "agent-1").kind, 6_000)
            .unwrap_or_else(|err| panic!("the stopped attempt may be retried: {err}"));
    }

    #[test]
    fn a_second_stop_of_the_same_attempt_is_refused() {
        let mut log = reported_run();
        log.truncate(3);
        log.push(stopped(4, "a", 1));
        let state = folded(&log);
        match state.check_as(&Actor::Human, &stopped(5, "a", 1).kind, 6_000) {
            Err(RuleError::AlreadyStopped { task, attempt }) => {
                assert_eq!(task, "a");
                assert_eq!(attempt, 1);
            }
            other => panic!("a stopped attempt was stopped again: {other:?}"),
        }
    }

    #[test]
    fn a_task_nobody_started_cannot_be_stopped() {
        let state = folded(&[started(1), added(2, "a", &["src/a"], &[])]);
        match state.check_as(&Actor::Human, &stopped(3, "a", 1).kind, 4_000) {
            Err(RuleError::WrongStatus { found, .. }) => assert_eq!(found, TaskStatus::Queued),
            other => panic!("a queued task was stopped: {other:?}"),
        }
    }

    #[test]
    fn an_integrated_task_cannot_be_stopped() {
        let state = folded(&landed());
        match state.check_as(&Actor::Human, &stopped(9, "a", 1).kind, 10_000) {
            Err(RuleError::WrongStatus { found, .. }) => assert_eq!(found, TaskStatus::Integrated),
            other => panic!("an integrated task was stopped: {other:?}"),
        }
    }

    #[test]
    fn a_working_task_may_be_dropped() {
        // A human who decides a running task will not be done stops it there,
        // rather than waiting for the agent to report.
        let mut log = reported_run();
        log.truncate(3);
        let state = folded(&log);
        assert_eq!(status(&state, "a"), TaskStatus::Working);
        state
            .check_as(&Actor::Human, &dropped(4, "not needed").kind, 5_000)
            .unwrap_or_else(|err| panic!("a Working task may be dropped: {err}"));
    }

    #[test]
    fn an_orchestrator_cannot_drop_a_task() {
        // Dropping takes work out of the run: only the human decides that.
        let state = folded(&reported_run());
        match state.check_as(&Actor::Orchestrator, &dropped(5, "x").kind, 6_000) {
            Err(RuleError::DropNotHuman { actor }) => assert_eq!(actor, Actor::Orchestrator),
            other => panic!("an orchestrator dropped a task: {other:?}"),
        }
    }

    #[test]
    fn an_integrated_task_cannot_be_dropped() {
        // Its commits are on the integration branch: calling it dropped would
        // make the pull request's dropped list disagree with what it carries.
        let state = folded(&landed());
        match state.check_as(&Actor::Human, &dropped(9, "x").kind, 10_000) {
            Err(RuleError::WrongStatus { found, .. }) => assert_eq!(found, TaskStatus::Integrated),
            other => panic!("an integrated task was dropped: {other:?}"),
        }
    }

    #[test]
    fn a_dropped_task_cannot_be_dropped_again() {
        let mut log = reported_run();
        log.push(dropped(5, "first"));
        let state = folded(&log);
        match state.check_as(&Actor::Human, &dropped(6, "second").kind, 7_000) {
            Err(RuleError::WrongStatus { found, .. }) => assert_eq!(found, TaskStatus::Dropped),
            other => panic!("a dropped task was dropped twice: {other:?}"),
        }
    }

    #[test]
    fn a_late_report_from_a_dropped_task_s_agent_is_refused() {
        // The drop closes the worker's tab; an agent still running anyway must
        // not put the task back into the run's work.
        let actor = Actor::Agent {
            id: "agent-1".to_string(),
        };
        let mut log = reported_run();
        log.push(dropped(5, "changed their mind"));
        let state = folded(&log);
        let late = by(
            6,
            Actor::Agent {
                id: "agent-1".to_string(),
            },
            EventKind::Reported {
                task: "a".to_string(),
                attempt: 1,
                status: ReportStatus::Completed,
                summary: "still here".to_string(),
                tests: strings(&[]),
                checks: vec![check("tests", Some(0))],
                changed_paths: strings(&["src/a/x.rs"]),
                commit: Some("abc123".to_string()),
                skills_used: Vec::new(),
                proofs: Vec::new(),
            },
        );
        match state.check_as(&actor, &late.kind, 7_000) {
            Err(RuleError::TaskIsDropped { task }) => assert_eq!(task, "a"),
            other => panic!("a dropped task's agent recorded work: {other:?}"),
        }
        // Every other event a leftover worker could append is refused too.
        for kind in [
            EventKind::GatePassed {
                task: "a".to_string(),
                attempt: 1,
                gate: "tests".to_string(),
                findings: Vec::new(),
                source: None,
            },
            EventKind::ReviewRequested {
                task: "a".to_string(),
                attempt: 1,
                reviewer: "agent-2".to_string(),
                degraded: false,
                note: None,
            },
            EventKind::HandedOff {
                task: "a".to_string(),
                from_agent: "agent-1".to_string(),
                to_agent: "agent-2".to_string(),
                checkpoint: "the dropped attempt".to_string(),
                reason: Some("a leftover worker".to_string()),
            },
        ] {
            assert!(
                matches!(
                    state.check_as(&actor, &kind, 7_000),
                    Err(RuleError::TaskIsDropped { .. })
                ),
                "{kind:?} was recorded for a dropped task"
            );
        }
    }

    #[test]
    fn an_old_log_with_a_drop_still_folds() {
        // The guard is about appending, not about history: a log a human
        // already dropped a task in folds as it always did.
        let mut log = reported_run();
        log.push(dropped(5, "changed their mind"));
        let state = folded(&log);
        assert_eq!(status(&state, "a"), TaskStatus::Dropped);
    }

    #[test]
    fn a_drop_without_a_reason_is_refused() {
        let state = folded(&reported_run());
        assert!(matches!(
            state.check_as(&Actor::Human, &dropped(5, "  ").kind, 6_000),
            Err(RuleError::EmptyDropReason { .. })
        ));
    }

    #[test]
    fn a_dropped_task_does_not_block_a_normal_finish() {
        // Landed and merged, plus a task the human decided not to do: the run
        // owes nothing, so ending it is a normal finish.
        let mut log = landed();
        log.push(added(9, "b", &["src/b"], &[]));
        log.push(by(
            10,
            Actor::Human,
            EventKind::TaskDropped {
                task: "b".to_string(),
                reason: "dropped".to_string(),
            },
        ));
        let state = folded(&log);
        assert!(unfinished_tasks(&state).is_empty());
        assert!(!unlanded(&state), "the pull request merged");
        state
            .check_as(&Actor::Orchestrator, &finish_kind(), 11_000)
            .unwrap_or_else(|err| panic!("a finish was refused: {err}"));
    }

    #[test]
    fn a_run_whose_base_is_its_integration_branch_has_no_pull_request_to_wait_for() {
        // Landing wrote straight onto the branch the run was cut from: there is
        // no pull request, now or later, so the landed work is not unlanded.
        let mut log = vec![ev(
            1,
            EventKind::RunStarted {
                run_id: "r1".to_string(),
                repo: "/repo".to_string(),
                base_branch: "prototype".to_string(),
                integration_branch: "prototype".to_string(),
            },
        )];
        log.extend(landed().into_iter().skip(1).take(5));
        log.push(integrated(7, "a"));
        let state = folded(&log);
        assert!(unfinished_tasks(&state).is_empty());
        assert!(!unlanded(&state));
        state
            .check_as(&Actor::Human, &finish_kind(), 11_000)
            .unwrap_or_else(|err| panic!("a close was refused: {err}"));

        // With a separate integration branch the same work still waits for it.
        let mut separate = log.clone();
        separate[0] = started(1);
        assert!(unlanded(&folded(&separate)));
    }

    #[test]
    fn a_drop_recorded_before_the_rule_still_folds() {
        // History is read, not re-judged: an orchestrator's drop in an old log
        // folds, while recording the same thing now is refused.
        let mut log = reported_run();
        log.push(by(
            5,
            Actor::Orchestrator,
            EventKind::TaskDropped {
                task: "a".to_string(),
                reason: "legacy".to_string(),
            },
        ));
        assert_eq!(status(&folded(&log), "a"), TaskStatus::Dropped);
    }

    /// The task `a`, reported and reviewed, with `review` as its newest gate.
    fn ready_log(review: Event) -> Vec<Event> {
        let mut log = reviewable();
        log.push(review);
        log
    }

    /// A passed review gate with provenance for the request `reviewable` made.
    fn answering_verdict(seq: u64) -> Event {
        ev(seq, review_gate(true, Some(provenance_for("codex/m1", 5))))
    }

    #[test]
    fn a_handed_in_task_with_a_cross_vendor_review_is_ready_to_land() {
        let state = folded(&ready_log(answering_verdict(6)));
        assert_eq!(status(&state, "a"), TaskStatus::Passed);
        let task = match state.tasks.get("a") {
            Some(task) => task,
            None => panic!("task a must exist"),
        };
        ready_to_land(task).unwrap_or_else(|err| panic!("it is ready to land: {err}"));
    }

    #[test]
    fn a_task_without_a_passing_hand_in_is_not_ready_to_land() {
        // A report recorded before checks were required: it folds, because
        // history is read, but it cannot pass for a tested hand-in now.
        let mut log = reviewable();
        if let EventKind::Reported { checks, .. } = &mut log[3].kind {
            checks.clear();
        }
        log.push(answering_verdict(6));
        let state = folded(&log);
        let task = match state.tasks.get("a") {
            Some(task) => task,
            None => panic!("task a must exist"),
        };
        assert!(matches!(
            ready_to_land(task),
            Err(RuleError::HandInWithoutChecks { .. })
        ));
    }

    #[test]
    fn a_task_whose_hand_in_named_a_failing_check_is_not_ready_to_land() {
        let mut log = reviewable();
        if let EventKind::Reported { checks, .. } = &mut log[3].kind {
            *checks = vec![check("format", Some(0)), check("tests", Some(101))];
        }
        log.push(answering_verdict(6));
        let state = folded(&log);
        let task = match state.tasks.get("a") {
            Some(task) => task,
            None => panic!("task a must exist"),
        };
        assert!(matches!(
            ready_to_land(task),
            Err(RuleError::HandInCheckFailed {
                exit: Some(101),
                ..
            })
        ));
    }

    #[test]
    fn a_task_only_a_same_vendor_reviewer_looked_at_is_not_ready_to_land() {
        let mut log = vec![
            started(1),
            added(2, "a", &["src/a"], &[]),
            spawned(3, "a", 1, "agent-1"),
            reported(4, "a", 1, &["src/a/x.rs"]),
            review_requested(5, "a", 1, "agent-1", true),
        ];
        log.push(ev(6, review_gate(true, Some(provenance_for("agent-1", 5)))));
        let state = folded(&log);
        let task = match state.tasks.get("a") {
            Some(task) => task,
            None => panic!("task a must exist"),
        };
        assert!(matches!(
            ready_to_land(task),
            Err(RuleError::ReviewNotIndependent { reviewer, .. }) if reviewer == "agent-1"
        ));
    }

    #[test]
    fn a_task_that_was_never_reviewed_is_not_ready_to_land() {
        let state = folded(&[
            started(1),
            added(2, "a", &["src/a"], &[]),
            spawned(3, "a", 1, "agent-1"),
            reported(4, "a", 1, &["src/a/x.rs"]),
            gated(5, "a", 1, "lint", true),
        ]);
        let task = match state.tasks.get("a") {
            Some(task) => task,
            None => panic!("task a must exist"),
        };
        assert!(matches!(
            ready_to_land(task),
            Err(RuleError::NotPassed { .. })
        ));
    }

    #[test]
    fn a_review_verdict_nothing_proves_is_not_ready_to_land() {
        // A passed review gate recorded before provenance existed folds, but
        // it does not prove a reviewer wrote anything.
        let state = folded(&ready_log(ev(6, review_gate(true, None))));
        let task = match state.tasks.get("a") {
            Some(task) => task,
            None => panic!("task a must exist"),
        };
        assert!(matches!(
            ready_to_land(task),
            Err(RuleError::UnprovenVerdict { .. })
        ));
    }

    #[test]
    fn a_verdict_answering_a_superseded_request_is_not_ready_to_land() {
        // Two requests for one attempt, and a verdict that answers the first:
        // the current review never read the work, so nothing proves it.
        let mut log = vec![
            started(1),
            added(2, "a", &["src/a"], &[]),
            spawned(3, "a", 1, "agent-1"),
            reported(4, "a", 1, &["src/a/x.rs"]),
            review_requested(5, "a", 1, "codex/m1", false),
            review_requested(6, "a", 1, "codex/m1", false),
        ];
        log.push(ev(
            7,
            review_gate(true, Some(provenance_for("codex/m1", 5))),
        ));
        let state = folded(&log);
        let task = match state.tasks.get("a") {
            Some(task) => task,
            None => panic!("task a must exist"),
        };
        assert!(matches!(
            ready_to_land(task),
            Err(RuleError::UnprovenVerdict { .. })
        ));
    }

    #[test]
    fn a_task_whose_latest_review_failed_is_not_ready_to_land() {
        // A passing lint gate after the failed review does not settle it: the
        // task stays Failed, and readiness looks at the review itself.
        let mut log = reviewable();
        log.push(ev(
            6,
            review_gate(false, Some(provenance_for("codex/m1", 5))),
        ));
        log.push(gated(7, "a", 1, "lint", true));
        let state = folded(&log);
        assert_eq!(status(&state, "a"), TaskStatus::Failed);
        let task = match state.tasks.get("a") {
            Some(task) => task,
            None => panic!("task a must exist"),
        };
        assert!(matches!(
            ready_to_land(task),
            Err(RuleError::NotPassed { .. })
        ));
    }

    #[test]
    fn a_human_who_accepted_a_failed_review_lets_it_land() {
        // "Take it as it is", the second choice a spent review's card offers:
        // the override is the person's recorded word that the failure may land,
        // and it is what `shprd override --rule integrate_after_failed_gate`
        // writes after the card names it.
        let mut log = reviewable();
        log.push(ev(
            6,
            review_gate(false, Some(provenance_for("codex/m1", 5))),
        ));
        let state = folded(&log);
        assert!(matches!(
            ready_to_land(state.tasks.get("a").expect("task a")),
            Err(RuleError::NotPassed { .. })
        ));

        log.push(by(
            7,
            Actor::Human,
            EventKind::Override {
                task: "a".to_string(),
                rule: INTEGRATE_AFTER_FAILED_GATE.to_string(),
                reason: "the reviewer was wrong about the batch".to_string(),
            },
        ));
        let state = folded(&log);
        ready_to_land(state.tasks.get("a").expect("task a"))
            .unwrap_or_else(|err| panic!("the person accepted it: {err}"));

        // An override recorded before the failure accepts nothing: it was about
        // a different one.
        let mut early = reviewable();
        early.push(by(
            6,
            Actor::Human,
            EventKind::Override {
                task: "a".to_string(),
                rule: INTEGRATE_AFTER_FAILED_GATE.to_string(),
                reason: "an older word".to_string(),
            },
        ));
        early.push(ev(
            7,
            review_gate(false, Some(provenance_for("codex/m1", 5))),
        ));
        let state = folded(&early);
        assert!(matches!(
            ready_to_land(state.tasks.get("a").expect("task a")),
            Err(RuleError::NotPassed { .. })
        ));
    }

    #[test]
    fn a_task_still_in_flight_is_not_ready_to_land() {
        let state = folded(&[
            started(1),
            added(2, "a", &["src/a"], &[]),
            spawned(3, "a", 1, "agent-1"),
        ]);
        let task = match state.tasks.get("a") {
            Some(task) => task,
            None => panic!("task a must exist"),
        };
        assert!(matches!(
            ready_to_land(task),
            Err(RuleError::NotPassed { .. })
        ));
    }

    #[test]
    fn a_gate_that_is_not_a_review_is_any_actor_s_to_record() {
        // The rule is about review verdicts only: a human recording a lint
        // result is a fact about lint, not a claim about a review.
        let mut log = reviewable();
        log.push(by(
            6,
            Actor::Human,
            EventKind::GatePassed {
                task: "a".to_string(),
                attempt: 1,
                gate: "lint".to_string(),
                findings: Vec::new(),
                source: None,
            },
        ));
        // Not a verdict: the task stays Reported until a review decides it.
        assert_eq!(status(&folded(&log), "a"), TaskStatus::Reported);
    }

    /// Item 1: a review that never started never passes a task. Worker gates
    /// and a verdict with no request behind it leave the task Reported.
    #[test]
    fn a_task_whose_review_never_started_never_passes() {
        let mut log = reviewable();
        log.pop(); // the review request: none was ever recorded
        log.push(gated(5, "a", 1, "lint", true));
        log.push(gated(6, "a", 1, "server-test", true));
        assert_eq!(status(&folded(&log), "a"), TaskStatus::Reported);
        // A verdict no request answers is refused where it is recorded.
        let state = folded(&log);
        assert!(matches!(
            state.check_as(&Actor::Engine, &passed_review(7, "a", 1, 5).kind, 7_000),
            Err(RuleError::StaleReviewVerdict { .. })
        ));
    }

    /// Invariant: whatever sequence of events the rules accept as written
    /// (each checked as its actor, as the engine does), a Passed task has a
    /// recorded review request and a passed review verdict for its attempt.
    #[test]
    fn no_path_reaches_passed_without_a_review_and_its_verdict() {
        let pool: Vec<(Actor, Event)> = vec![
            (
                Actor::Engine,
                review_requested(5, "a", 1, "codex/m1", false),
            ),
            (
                Actor::Engine,
                review_requested(6, "a", 1, "codex/m2", false),
            ),
            (
                Actor::Agent {
                    id: "agent-1".to_string(),
                },
                gated(7, "a", 1, "lint", true),
            ),
            (Actor::Human, gated(7, "a", 1, "lint", true)),
            (Actor::Engine, gated(7, "a", 1, "lint", false)),
            (Actor::Engine, passed_review(8, "a", 1, 5)),
            (
                Actor::Agent {
                    id: "agent-1".to_string(),
                },
                passed_review(8, "a", 1, 5),
            ),
            (Actor::Engine, passed_review(8, "a", 1, 6)),
            (
                Actor::Engine,
                ev(8, review_gate(false, Some(provenance_for("codex/m1", 5)))),
            ),
            (Actor::Human, reopened(9, "a", 1)),
        ];
        let mut frontier: Vec<RunState> = vec![folded(&[
            started(1),
            added(2, "a", &["src/a"], &[]),
            spawned(3, "a", 1, "agent-1"),
            reported(4, "a", 1, &["src/a/x.rs"]),
        ])];
        let mut reached = 0;
        for _ in 0..5 {
            let mut next = Vec::new();
            for state in &frontier {
                for (actor, event) in &pool {
                    // A step the rules refuse is not a path the engine can take.
                    if state.check_as(actor, &event.kind, 9_000).is_err() {
                        continue;
                    }
                    let mut after = state.clone();
                    let mut event = event.clone();
                    event.actor = actor.clone();
                    if after.apply(&event).is_err() {
                        continue;
                    }
                    if let Some(task) = after.tasks.get("a")
                        && task.status == TaskStatus::Passed
                    {
                        reached += 1;
                        let attempt = task.latest_attempt();
                        let Some(attempt) = attempt else {
                            panic!("a passed task has an attempt")
                        };
                        assert!(!attempt.reviews.is_empty(), "Passed with no review request");
                        assert!(
                            attempt.gates.iter().any(|g| g.gate == "review" && g.passed),
                            "Passed with no review verdict"
                        );
                    }
                    next.push(after);
                }
            }
            frontier = next;
        }
        assert!(reached > 0, "the invariant never met a Passed task");
    }

    fn skipped(seq: u64, id: &str, attempt: u32, actor: Actor) -> Event {
        let mut event = ev(
            seq,
            EventKind::ReviewSkipped {
                task: id.to_string(),
                attempt,
            },
        );
        event.actor = actor;
        event
    }

    /// Review off: a clean hand-in with no review request passes on its checks,
    /// through a recorded `ReviewSkipped` (engine only), and the result is
    /// Passed. A review that was requested and never decided cannot be skipped.
    #[test]
    fn review_off_passes_a_clean_hand_in_on_its_checks() {
        let base = vec![
            started(1),
            added(2, "a", &["src/a"], &[]),
            spawned(3, "a", 1, "agent-1"),
            reported(4, "a", 1, &["src/a/x.rs"]),
        ];
        let mut log = base.clone();
        log.push(skipped(5, "a", 1, Actor::Engine));
        assert_eq!(status(&folded(&log), "a"), TaskStatus::Passed);
        // A passing worker gate alone is still not a pass.
        let mut gated_only = base.clone();
        gated_only.push(gated(5, "a", 1, "lint", true));
        assert_eq!(status(&folded(&gated_only), "a"), TaskStatus::Reported);
        // Only the engine records the skip.
        for actor in [
            Actor::Human,
            Actor::Agent {
                id: "agent-1".to_string(),
            },
        ] {
            let mut by_other = base.clone();
            by_other.push(skipped(5, "a", 1, actor));
            assert!(matches!(
                refused_append(&by_other),
                RuleError::NotEngine { .. }
            ));
        }
        // A review that was asked for cannot be skipped: it must be decided.
        let mut asked = base.clone();
        asked.push(review_requested(5, "a", 1, "codex/m1", false));
        asked.push(skipped(6, "a", 1, Actor::Engine));
        assert!(matches!(
            refused_append(&asked),
            RuleError::NotPassed { .. }
        ));
    }

    fn reopened(seq: u64, id: &str, attempt: u32) -> Event {
        ev(
            seq,
            EventKind::TaskReopened {
                task: id.to_string(),
                attempt,
            },
        )
    }

    /// Item 2: a human reopens a Reported or Passed task, for the same attempt;
    /// the hand-in and the review are cleared so a new hand-in is reviewed afresh.
    #[test]
    fn a_human_reopens_a_handed_in_task_for_the_same_attempt() {
        for (extra, from) in [
            (Vec::new(), TaskStatus::Reported),
            (vec![passed_review(6, "a", 1, 5)], TaskStatus::Passed),
        ] {
            let mut log = reviewable();
            log.extend(extra);
            assert_eq!(status(&folded(&log), "a"), from);
            let kind = reopened(9, "a", 1).kind;
            let state = folded(&log);
            assert!(state.check_as(&Actor::Human, &kind, 9_000).is_ok());
            log.push(reopened(9, "a", 1));
            let state = folded(&log);
            assert_eq!(status(&state, "a"), TaskStatus::Working);
            let attempt = state.tasks.get("a").and_then(|t| t.latest_attempt());
            let Some(attempt) = attempt else {
                panic!("the attempt stays")
            };
            assert_eq!(attempt.n, 1);
            assert!(
                attempt.report.is_none() && attempt.gates.is_empty() && attempt.reviews.is_empty()
            );
            // The worker hands in again, and the review runs afresh.
            log.push(reported(10, "a", 1, &["src/a/x.rs"]));
            log.push(review_requested(11, "a", 1, "codex/m1", false));
            log.push(passed_review(12, "a", 1, 11));
            assert_eq!(status(&folded(&log), "a"), TaskStatus::Passed);
        }
    }

    #[test]
    fn only_a_human_reopens_and_only_a_handed_in_task() {
        let state = folded(&reviewable());
        let kind = reopened(9, "a", 1).kind;
        for actor in [Actor::Engine, Actor::Orchestrator] {
            assert!(matches!(
                state.check_as(&actor, &kind, 9_000),
                Err(RuleError::NotHuman { .. })
            ));
        }
        // Wrong attempt.
        assert!(
            state
                .check_as(&Actor::Human, &reopened(9, "a", 2).kind, 9_000)
                .is_err()
        );
        // A working task is already open.
        let working = folded(&[
            started(1),
            added(2, "a", &["src/a"], &[]),
            spawned(3, "a", 1, "agent-1"),
        ]);
        assert!(matches!(
            working.check_as(&Actor::Human, &kind, 9_000),
            Err(RuleError::WrongStatus { .. })
        ));
    }

    fn ask_raised(seq: u64, id: &str, ask: &str) -> Event {
        ev(
            seq,
            EventKind::AskRaised {
                task: id.to_string(),
                ask: ask.to_string(),
                ask_kind: crate::ask::AskKind::Permission,
                text: "Run: cargo test".to_string(),
                options: vec!["Allow".to_string(), "Deny".to_string()],
                created_at_ms: 1_000,
                expires_at_ms: None,
            },
        )
    }

    fn ask_answered(seq: u64, id: &str, ask: &str) -> Event {
        ev(
            seq,
            EventKind::AskAnswered {
                task: id.to_string(),
                ask: ask.to_string(),
                answer: "allow".to_string(),
            },
        )
    }

    #[test]
    fn an_ask_is_raised_against_a_task_the_run_holds() {
        let state = folded(&[
            started(1),
            added(2, "a", &["src/a"], &[]),
            ask_raised(3, "a", "q1"),
        ]);
        let record = state
            .asks
            .get(&("a".to_string(), "q1".to_string()))
            .expect("the ask is in the fold");
        assert_eq!(record.text, "Run: cargo test");
        assert!(record.answer.is_none());

        let mut log = vec![started(1), added(2, "a", &["src/a"], &[])];
        log.push(ask_raised(3, "missing", "q1"));
        assert!(matches!(
            RunState::fold(&log),
            Err(RuleError::UnknownTask { .. })
        ));
    }

    #[test]
    fn an_ask_can_only_be_raised_once() {
        let mut log = vec![
            started(1),
            added(2, "a", &["src/a"], &[]),
            ask_raised(3, "a", "q1"),
        ];
        log.push(ask_raised(4, "a", "q1"));
        assert!(matches!(
            RunState::fold(&log),
            Err(RuleError::AskAlreadyRaised { .. })
        ));
    }

    #[test]
    fn an_answer_to_an_unknown_ask_is_refused() {
        let state = folded(&[started(1), added(2, "a", &["src/a"], &[])]);
        let kind = EventKind::AskAnswered {
            task: "a".to_string(),
            ask: "never-raised".to_string(),
            answer: "allow".to_string(),
        };
        assert!(matches!(
            state.check_as(
                &Actor::Agent {
                    id: "claude".to_string()
                },
                &kind,
                2_000
            ),
            Err(RuleError::UnknownAsk { .. })
        ));
    }

    #[test]
    fn an_ask_is_answered_only_once() {
        let mut log = vec![
            started(1),
            added(2, "a", &["src/a"], &[]),
            ask_raised(3, "a", "q1"),
            ask_answered(4, "a", "q1"),
        ];
        let state = folded(&log);
        assert_eq!(
            state
                .asks
                .get(&("a".to_string(), "q1".to_string()))
                .and_then(|record| record.answer.as_ref())
                .map(|answer| answer.text.as_str()),
            Some("allow")
        );
        log.push(ask_answered(5, "a", "q1"));
        assert!(matches!(
            RunState::fold(&log),
            Err(RuleError::AskAlreadyAnswered { .. })
        ));
    }

    /// A recorded answer that is off its card's choices still folds, because a
    /// log written before the choice check must replay; a new off-list answer is
    /// still refused.
    #[test]
    fn a_recorded_off_list_answer_folds_but_a_new_one_is_refused() {
        let raise = |seq: u64, ask: &str| {
            ev(
                seq,
                EventKind::AskRaised {
                    task: "a".to_string(),
                    ask: ask.to_string(),
                    ask_kind: crate::ask::AskKind::Question,
                    text: "Confirm the override?".to_string(),
                    options: vec![
                        "confirm override integrate_after_failed_gate".to_string(),
                        "leave it".to_string(),
                    ],
                    created_at_ms: 1_000,
                    expires_at_ms: None,
                },
            )
        };
        let answer = |seq: u64, ask: &str, text: &str| {
            ev(
                seq,
                EventKind::AskAnswered {
                    task: "a".to_string(),
                    ask: ask.to_string(),
                    answer: text.to_string(),
                },
            )
        };
        let state = folded(&[
            started(1),
            added(2, "a", &["src/a"], &[]),
            raise(3, "propose-override-1"),
            answer(4, "propose-override-1", "confirm override"),
        ]);
        let recorded = state
            .asks
            .get(&("a".to_string(), "propose-override-1".to_string()))
            .and_then(|record| record.answer.as_ref())
            .map(|answer| answer.text.as_str());
        assert_eq!(recorded, Some("confirm override"));
        let open = folded(&[
            started(1),
            added(2, "a", &["src/a"], &[]),
            raise(3, "propose-override-2"),
        ]);
        let kind = EventKind::AskAnswered {
            task: "a".to_string(),
            ask: "propose-override-2".to_string(),
            answer: "confirm override".to_string(),
        };
        assert!(matches!(
            open.check_as(&Actor::Human, &kind, 2_000),
            Err(RuleError::AskNotAChoice { .. })
        ));
    }

    #[test]
    fn a_question_that_says_nothing_is_refused() {
        let mut log = vec![started(1), added(2, "a", &["src/a"], &[])];
        log.push(ev(
            3,
            EventKind::AskRaised {
                task: "a".to_string(),
                ask: "q1".to_string(),
                ask_kind: crate::ask::AskKind::Question,
                text: "  ".to_string(),
                options: Vec::new(),
                created_at_ms: 1_000,
                expires_at_ms: None,
            },
        ));
        assert!(matches!(
            RunState::fold(&log),
            Err(RuleError::EmptyAsk { .. })
        ));
    }
}
