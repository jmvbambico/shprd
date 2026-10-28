//! Transition rules: which event may follow which state.
//!
//! The engine never trusts an agent. Before appending an event it calls
//! [`crate::state::RunState::check`], and an [`Err`] means the action is
//! refused rather than recorded.

use thiserror::Error;

use crate::event::{Actor, AgentId, EventKind, ReportStatus, ResourceAction, ResourceKind, TaskId};
use crate::gate::REVIEW_GATE;
use crate::scope::{self, ScopeError};
use crate::state::{AgentInfo, ResourceStatus, RunState, Task, TaskStatus};

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
    /// Integration was requested while the latest attempt has a failed gate and
    /// no override recorded after it.
    #[error("task {task:?} cannot be integrated: gate {gate:?} failed and no override followed")]
    FailedGate {
        /// The task.
        task: TaskId,
        /// The failed gate.
        gate: String,
    },
}

/// Rule name a human overrides to integrate an attempt whose gate failed.
pub const INTEGRATE_AFTER_FAILED_GATE: &str = "integrate_after_failed_gate";

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

/// Whether `agent` may be re-admitted: only an agent the log has on cooldown.
fn check_available(state: &RunState, agent: &str) -> Result<(), RuleError> {
    match state.agents.get(agent) {
        Some(info) if info.dry => Ok(()),
        _ => Err(RuleError::AgentNotDry {
            agent: agent.to_string(),
        }),
    }
}

/// Checks one event kind against `state` as of `now_ms`.
pub(crate) fn check(state: &RunState, kind: &EventKind, now_ms: u64) -> Result<(), RuleError> {
    check_as(state, &Actor::Human, kind, now_ms)
}

/// Checks one event kind caused by `actor` against `state` as of `now_ms`.
///
/// A bare [`check`] is the human's: a verb that consults the log before an
/// outside effect has no other actor to offer. Folding an event passes the
/// actor the log actually records, which is what an approval's rule reads.
pub(crate) fn check_as(
    state: &RunState,
    actor: &Actor,
    kind: &EventKind,
    now_ms: u64,
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
        EventKind::GatePassed { task, attempt, .. }
        | EventKind::GateFailed { task, attempt, .. } => check_gate(state, task, *attempt),
        EventKind::ReviewRequested { task, attempt, .. } => {
            check_review_requested(state, task, *attempt)
        }
        EventKind::Integrated { task, .. } => check_integrated(state, task),
        EventKind::Dry { .. } => Ok(()),
        EventKind::Available { agent, .. } => check_available(state, agent),
        EventKind::HandedOff { task, to_agent, .. } => {
            check_handed_off(state, task, to_agent, now_ms)
        }
        EventKind::Escalated { task, .. } => match task {
            Some(task) => task_in(state, task).map(|_| ()),
            None => Ok(()),
        },
        EventKind::Override { task, rule, reason } => {
            if !task.is_empty() {
                task_in(state, task)?;
            }
            if rule.trim().is_empty() || reason.trim().is_empty() {
                return Err(RuleError::EmptyOverride { task: task.clone() });
            }
            Ok(())
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
        EventKind::RunFinished {} => check_run_finished(state),
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

/// Rules for finishing a run: nothing may still be in flight.
fn check_run_finished(state: &RunState) -> Result<(), RuleError> {
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

    fn overrode(seq: u64, id: &str, rule: &str) -> Event {
        ev(
            seq,
            EventKind::Override {
                task: id.to_string(),
                rule: rule.to_string(),
                reason: "shipping anyway".to_string(),
            },
        )
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
        ev(seq, EventKind::RunFinished {})
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
}
