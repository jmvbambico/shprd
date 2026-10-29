//! Transition rules: which event may follow which state.
//!
//! The engine never trusts an agent. Before appending an event it calls
//! [`crate::state::RunState::check`], and an [`Err`] means the action is
//! refused rather than recorded.

use std::collections::BTreeSet;

use thiserror::Error;

use crate::event::{
    Actor, AgentId, EventKind, ReportStatus, ResourceAction, ResourceKind, ReviewSource, TaskId,
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
    /// The task declares no scope.
    #[error("task {task:?} has an empty scope")]
    EmptyScope {
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
    /// The task's scope clashes with another task's.
    #[error("task {task:?} path {path:?} overlaps task {other:?}")]
    ScopeConflict {
        /// The task being spawned.
        task: TaskId,
        /// The other task holding the scope.
        other: TaskId,
        /// The clashing path.
        path: String,
    },
    /// A changed path is not usable, or not inside the task's scope.
    #[error("task {task:?} changed {path:?}, which is not inside its scope")]
    PathOutOfScope {
        /// The task.
        task: TaskId,
        /// The path, as given.
        path: String,
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
    /// Integration was requested for a task that has not passed its gates.
    #[error("task {task:?} is {found:?} and cannot be integrated")]
    NotPassed {
        /// The task.
        task: TaskId,
        /// The status it actually has.
        found: TaskStatus,
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
    /// An approval was recorded by something that is not a human.
    #[error("{actor:?} cannot approve a pull request: only a human can")]
    NotHuman {
        /// Who tried.
        actor: Actor,
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
    /// with `shprd down` after the merge. An agent or the engine ending a run
    /// would close tabs nobody asked it to close.
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
        } => check_spawned(state, task, *attempt, agent, now_ms),
        EventKind::Reported {
            task,
            attempt,
            changed_paths,
            ..
        } => check_reported(state, task, *attempt, changed_paths),
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
            check_gate(state, task, *attempt)?;
            check_verdict(provenance, state, actor, task, *attempt, gate, source)
        }
        EventKind::ReviewRequested { task, attempt, .. } => {
            check_review_requested(state, task, *attempt)
        }
        EventKind::Integrated { task, .. } => check_integrated(state, task),
        EventKind::Dry { .. } => Ok(()),
        EventKind::Available { agent, .. } => check_available(state, actor, agent, provenance),
        EventKind::HandedOff { task, to_agent, .. } => {
            check_handed_off(state, task, to_agent, now_ms)
        }
        EventKind::Escalated { task, .. } => match task {
            Some(task) => task_in(state, task).map(|_| ()),
            None => Ok(()),
        },
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
        EventKind::OrchestratorLaunched { .. } => Ok(()),
        EventKind::WatchLaunched { .. } => Ok(()),
        // A delivery record moves nothing: it is the watchdog's own bookmark,
        // and any pass may record one.
        EventKind::OrchestratorNotified { .. } => Ok(()),
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
    if paths.is_empty() {
        return Err(RuleError::EmptyScope { task: id });
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

/// Rules for spawning an agent on a task attempt.
fn check_spawned(
    state: &RunState,
    task: &str,
    attempt: u32,
    agent: &AgentId,
    now_ms: u64,
) -> Result<(), RuleError> {
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
    for (other_id, other) in &state.tasks {
        let busy = matches!(
            other.status,
            TaskStatus::Working | TaskStatus::Reported | TaskStatus::Passed
        );
        if other_id == task || !busy {
            continue;
        }
        if let Some(path) = first_overlap(&found.scope, &other.scope) {
            return Err(RuleError::ScopeConflict {
                task: task.to_string(),
                other: other_id.clone(),
                path,
            });
        }
    }
    Ok(())
}

/// The first entry of `a` that clashes with an entry of `b`.
fn first_overlap(a: &[String], b: &[String]) -> Option<String> {
    a.iter()
        .find(|left| scope::overlaps(std::slice::from_ref(left), b))
        .cloned()
}

/// Rules for an agent reporting the outcome of an attempt.
fn check_reported(
    state: &RunState,
    task: &str,
    attempt: u32,
    changed_paths: &[String],
) -> Result<(), RuleError> {
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
    for path in changed_paths {
        let normalized = match scope::normalize(path) {
            Ok(normalized) => normalized,
            Err(_) => {
                return Err(RuleError::PathOutOfScope {
                    task: task.to_string(),
                    path: path.clone(),
                });
            }
        };
        if !scope::within(&normalized, &found.scope) {
            return Err(RuleError::PathOutOfScope {
                task: task.to_string(),
                path: path.clone(),
            });
        }
    }
    Ok(())
}

/// Rules for recording a gate outcome: it needs a completed report.
fn check_gate(state: &RunState, task: &str, attempt: u32) -> Result<(), RuleError> {
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
fn check_review_requested(state: &RunState, task: &str, attempt: u32) -> Result<(), RuleError> {
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

/// Rules for integrating a passed task: a failed gate needs a later override.
fn check_integrated(state: &RunState, task: &str) -> Result<(), RuleError> {
    let found = task_in(state, task)?;
    if found.status != TaskStatus::Passed {
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

/// Rules for handing a working task to another agent.
fn check_handed_off(
    state: &RunState,
    task: &str,
    to_agent: &AgentId,
    now_ms: u64,
) -> Result<(), RuleError> {
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
/// the run's pull request is what carries it. Queued, Working, Reported, in
/// review (passed or failed) and Failed are all work in flight or work stuck,
/// which ending the run would strand on a branch nobody watches.
///
/// `shprd down` names these in its refusal, and the rule refuses a finish or an
/// abandon that disagrees with them, so both read one definition.
pub fn unfinished_tasks(state: &RunState) -> Vec<TaskId> {
    state
        .tasks
        .iter()
        .filter(|(_, task)| task.status != TaskStatus::Integrated)
        .map(|(id, _)| id.clone())
        .collect()
}

/// Whether ending the run would leave its landed work on a branch nothing
/// picks up: tasks exist and the pull request carrying them has not merged.
pub fn unlanded(state: &RunState) -> bool {
    !state.tasks.is_empty() && state.pr.as_ref().is_none_or(|pr| !pr.merged)
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
/// the run with `shprd down` once the milestone merged. Both need the run's
/// work to be over — nothing unfinished, and the pull request merged — for the
/// same reason `shprd down` refuses, because a finish that strands integrated
/// work is the same loss as one that strands a working task.
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
                changed_paths: strings(paths),
                commit: Some("abc123".to_string()),
                skills_used: Vec::new(),
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
                changed_paths: strings(&["src/a"]),
                commit: None,
                skills_used: Vec::new(),
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
            gated(7, "c", 1, "clippy", true),
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
            log.push(gated(seq + 2, id, 1, "clippy", true));
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
            gated(5, "a", 1, "clippy", true),
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
            gated(6, "a", 1, "clippy", true),
            integrated(7, "a"),
            spawned(8, "b", 1, "agent-2"),
            reported(9, "b", 1, &["src/b"]),
            gated(10, "b", 1, "clippy", true),
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
                assert_eq!(task.attempts[0].gates[0].gate, "clippy");
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
        assert!(matches!(
            refused(&[started(1), added(2, "a", &[], &[])]),
            RuleError::EmptyScope { .. }
        ));
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
    fn overlapping_scope_spawn_is_refused() {
        assert!(matches!(
            refused(&[
                started(1),
                added(2, "a", &["src"], &[]),
                added(3, "b", &["src/b.rs"], &[]),
                spawned(4, "a", 1, "agent-1"),
                spawned(5, "b", 1, "agent-2"),
            ]),
            RuleError::ScopeConflict { .. }
        ));
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
    fn changed_path_must_stay_inside_scope() {
        assert!(matches!(
            refused(&[
                started(1),
                added(2, "a", &["src/a"], &[]),
                spawned(3, "a", 1, "agent-1"),
                reported(4, "a", 1, &["src/b/x.rs"]),
            ]),
            RuleError::PathOutOfScope { .. }
        ));
        assert!(matches!(
            refused(&[
                started(1),
                added(2, "a", &["src/a"], &[]),
                spawned(3, "a", 1, "agent-1"),
                reported(4, "a", 1, &["../escape"]),
            ]),
            RuleError::PathOutOfScope { .. }
        ));
    }

    #[test]
    fn gate_needs_a_completed_report() {
        assert!(matches!(
            refused(&[
                started(1),
                added(2, "a", &["src/a"], &[]),
                spawned(3, "a", 1, "agent-1"),
                gated(4, "a", 1, "clippy", true),
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
                changed_paths: strings(&["src/a"]),
                commit: None,
                skills_used: Vec::new(),
            },
        );
        assert!(matches!(
            refused(&[
                started(1),
                added(2, "a", &["src/a"], &[]),
                spawned(3, "a", 1, "agent-1"),
                blocked,
                gated(5, "a", 1, "clippy", true),
            ]),
            RuleError::ReportNotCompleted { .. }
        ));
    }

    fn review_requested(seq: u64, id: &str, attempt: u32, reviewer: &str, degraded: bool) -> Event {
        ev(
            seq,
            EventKind::ReviewRequested {
                task: id.to_string(),
                attempt,
                reviewer: reviewer.to_string(),
                degraded,
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
        failed_then_passed.push(gated(5, "a", 1, "clippy", false));
        assert!(matches!(
            refused(&[failed_then_passed.clone(), vec![integrated(6, "a")]].concat()),
            RuleError::NotPassed { .. }
        ));
        let mut clean = failed_then_passed.clone();
        clean.push(gated(6, "a", 1, "clippy", true));
        assert_eq!(status(&folded(&clean), "a"), TaskStatus::Passed);
        assert!(matches!(
            refused(&[clean.clone(), vec![integrated(7, "a")]].concat()),
            RuleError::FailedGate { .. }
        ));

        let mut overridden = clean.clone();
        overridden.push(overrode(7, "a", INTEGRATE_AFTER_FAILED_GATE));
        let overridden = [overridden, vec![integrated(8, "a")]].concat();
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
            gated(6, "a", 1, "clippy", false),
            gated(7, "a", 1, "clippy", true),
            integrated(8, "a"),
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
            gated(5, "a", 1, "clippy", true),
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
            gated(5, "a", 1, "clippy", false),
            gated(6, "a", 1, "clippy", true),
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
        // The orchestrator's brief ends the run with `shprd down` after the
        // merge, so it may finish one; an agent and the engine never do.
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
    /// not re-judged, exactly as a verdict without provenance is kept.
    #[test]
    fn a_legacy_verdict_without_a_request_seq_still_folds() {
        let mut log = reviewable();
        log.push(ev(
            6,
            review_gate(true, Some(provenance_for("codex/m1", 0))),
        ));
        let state = folded(&log);
        assert_eq!(status(&state, "a"), TaskStatus::Passed);
        // Recording the same thing now is refused: absence is a legacy reading,
        // never a way past the rule.
        assert!(matches!(
            state.check_as(
                &Actor::Engine,
                &review_gate(true, Some(provenance_for("codex/m1", 0))),
                7_000
            ),
            Err(RuleError::StaleReviewVerdict { .. })
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
        assert_eq!(status(&folded(&log), "a"), TaskStatus::Passed);
    }
}
