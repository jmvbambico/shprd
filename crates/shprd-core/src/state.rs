//! Run state: the fold of the event log, and the mutation each event causes.
//!
//! [`RunState::apply`] checks an event against the transition rules in
//! [`crate::rules`] and only then mutates. [`RunState::fold`] replays a whole
//! log under exactly the same rules, so an illegal log cannot quietly become
//! state.

use std::collections::BTreeMap;

use crate::event::{
    AgentId, Event, EventKind, Finding, ReportStatus, ResourceAction, ResourceKind, TaskId,
};
use crate::rules::{self, RuleError};
use crate::scope;

/// Everything known about a run.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RunState {
    /// The run, once it has started.
    pub run: Option<RunInfo>,
    /// Tasks, keyed by id.
    pub tasks: BTreeMap<TaskId, Task>,
    /// Agents seen in this run, keyed by id.
    pub agents: BTreeMap<AgentId, AgentInfo>,
    /// Escalations raised during the run, in order.
    pub escalations: Vec<Escalation>,
    /// Rule overrides that name no task, so they apply to the whole run.
    ///
    /// Per-task overrides live on [`Task::overrides`]; these have no task to
    /// hang off, and dropping them would lose the only record of a run-wide
    /// override. Newest last, in event order.
    pub run_overrides: Vec<OverrideRecord>,
    /// Resources shprd created for this run, keyed by kind and name.
    ///
    /// This ledger is what makes cleanup safe: the janitor may only remove
    /// what shprd itself recorded here, never whatever happens to look like
    /// its own work.
    pub resources: BTreeMap<(ResourceKind, String), ResourceRecord>,
    /// Whether the run has finished.
    pub finished: bool,
    /// Sequence number of the last applied event, 0 before the first.
    pub last_seq: u64,
}

/// The run's identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunInfo {
    /// Run identifier.
    pub run_id: String,
    /// Repository the run works on.
    pub repo: String,
    /// Branch the run was cut from.
    pub base_branch: String,
    /// Branch landed work is integrated on.
    pub integration_branch: String,
}

/// A task in the run plan, plus its progress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    /// Task id.
    pub id: TaskId,
    /// Human-readable title.
    pub title: String,
    /// Normalized repo-relative paths the task may change.
    pub scope: Vec<String>,
    /// What must be true when the task is done.
    pub acceptance: Vec<String>,
    /// Free-form tags.
    pub tags: Vec<String>,
    /// Skills the agent is told to load.
    pub skills: Vec<String>,
    /// Tasks that must be integrated first.
    pub deps: Vec<TaskId>,
    /// Where the task is in its lifecycle.
    pub status: TaskStatus,
    /// Attempts made so far, in order.
    pub attempts: Vec<Attempt>,
    /// Rules a human overrode for this task, in order.
    pub overrides: Vec<OverrideRecord>,
}

/// Where a task sits in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    /// Added to the plan, no agent has run it.
    Queued,
    /// An agent is working on it.
    Working,
    /// The agent reported a completed outcome for the current attempt.
    Reported,
    /// Its gates passed.
    Passed,
    /// It failed, or an attempt reported blocked or failed, and it may be
    /// retried on a new attempt.
    Failed,
    /// Its work landed on the integration branch.
    Integrated,
}

/// One attempt at a task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
    /// Attempt number, starting at 1.
    pub n: u32,
    /// The agent that ran it.
    pub agent: AgentId,
    /// Model the agent ran on.
    pub model: Option<String>,
    /// Worktree the agent worked in.
    pub worktree: String,
    /// Branch the agent committed on.
    pub branch: String,
    /// The agent's report, once it has reported.
    pub report: Option<Report>,
    /// Gates run against this attempt, in order.
    pub gates: Vec<GateResult>,
}

/// What an agent reported about an attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// Outcome of the attempt.
    pub status: ReportStatus,
    /// What the agent says it did.
    pub summary: String,
    /// Test commands the agent ran, with their result.
    pub tests: Vec<String>,
    /// Normalized paths the agent changed.
    pub changed_paths: Vec<String>,
    /// Commit the agent made, if any.
    pub commit: Option<String>,
    /// Skills the agent used.
    pub skills_used: Vec<String>,
}

/// The outcome of one gate against one attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateResult {
    /// Gate name.
    pub gate: String,
    /// Whether it passed.
    pub passed: bool,
    /// What it found.
    pub findings: Vec<Finding>,
    /// Sequence number of the event that recorded it.
    pub seq: u64,
}

/// A rule a human overrode, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverrideRecord {
    /// The rule that was overridden.
    pub rule: String,
    /// Why it was overridden.
    pub reason: String,
    /// Sequence number of the event that recorded it.
    pub seq: u64,
}

/// Where a resource is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceStatus {
    /// It exists and is in service.
    Live,
    /// It exists but has been taken out of service.
    Archived,
    /// It has been deleted.
    Removed,
}

/// What the run knows about one resource it created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceRecord {
    /// The task it belongs to, when it belongs to one.
    pub task: Option<TaskId>,
    /// Where it is in its lifecycle.
    pub status: ResourceStatus,
    /// Sequence number of the event that created it.
    pub created_seq: u64,
}

/// What is known about an agent.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgentInfo {
    /// Whether the agent is on cooldown.
    pub dry: bool,
    /// When it is expected back, ms since the Unix epoch.
    pub dry_until_ms: Option<u64>,
    /// Why it is dry.
    pub dry_reason: Option<String>,
}

/// Something that needs a human decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Escalation {
    /// The task it concerns, when it is not run-wide.
    pub task: Option<TaskId>,
    /// What needs deciding.
    pub body: String,
    /// The action taken if nobody answers.
    pub default_action: String,
    /// Sequence number of the event that recorded it.
    pub seq: u64,
}

impl Task {
    /// The attempt last spawned for this task, if any.
    pub fn latest_attempt(&self) -> Option<&Attempt> {
        self.attempts.last()
    }
}

impl RunState {
    /// Replays a whole log from the initial state.
    ///
    /// Every event goes through [`RunState::apply`], so a log that breaks a
    /// transition rule fails the fold with that rule's error.
    pub fn fold(events: &[Event]) -> Result<RunState, RuleError> {
        let mut state = RunState::default();
        for event in events {
            state.apply(event)?;
        }
        Ok(state)
    }

    /// Whether `kind` is allowed in this state as of `now_ms`.
    ///
    /// The engine calls this before appending an event: an error means the
    /// action is refused, not recorded.
    pub fn check(&self, kind: &EventKind, now_ms: u64) -> Result<(), RuleError> {
        rules::check(self, kind, now_ms)
    }

    /// Checks `event` against the rules, then applies it and records its seq.
    pub fn apply(&mut self, event: &Event) -> Result<(), RuleError> {
        rules::check(self, &event.kind, event.at_ms)?;
        self.mutate(event);
        self.last_seq = event.seq;
        Ok(())
    }

    /// Applies an event that has already passed the rules.
    fn mutate(&mut self, event: &Event) {
        match &event.kind {
            EventKind::RunStarted {
                run_id,
                repo,
                base_branch,
                integration_branch,
            } => {
                self.run = Some(RunInfo {
                    run_id: run_id.clone(),
                    repo: repo.clone(),
                    base_branch: base_branch.clone(),
                    integration_branch: integration_branch.clone(),
                });
            }
            EventKind::TaskAdded {
                task,
                title,
                scope: paths,
                acceptance,
                tags,
                skills,
                deps,
            } => {
                self.tasks.insert(
                    task.clone(),
                    Task {
                        id: task.clone(),
                        title: title.clone(),
                        scope: paths
                            .iter()
                            .filter_map(|path| scope::normalize(path).ok())
                            .collect(),
                        acceptance: acceptance.clone(),
                        tags: tags.clone(),
                        skills: skills.clone(),
                        deps: deps.clone(),
                        status: TaskStatus::Queued,
                        attempts: Vec::new(),
                        overrides: Vec::new(),
                    },
                );
            }
            EventKind::Spawned {
                task,
                attempt,
                agent,
                model,
                worktree,
                branch,
            } => {
                self.agents.entry(agent.clone()).or_default();
                if let Some(entry) = self.tasks.get_mut(task) {
                    entry.status = TaskStatus::Working;
                    entry.attempts.push(Attempt {
                        n: *attempt,
                        agent: agent.clone(),
                        model: model.clone(),
                        worktree: worktree.clone(),
                        branch: branch.clone(),
                        report: None,
                        gates: Vec::new(),
                    });
                }
            }
            EventKind::Reported {
                task,
                status,
                summary,
                tests,
                changed_paths,
                commit,
                skills_used,
                ..
            } => {
                // Only a completed report moves the task on to gating; a
                // blocked or failed one leaves it retryable, so the task goes
                // Failed even though the report itself is recorded below.
                let next = if *status == ReportStatus::Completed {
                    TaskStatus::Reported
                } else {
                    TaskStatus::Failed
                };
                if let Some(entry) = self.tasks.get_mut(task) {
                    entry.status = next;
                    if let Some(current) = entry.attempts.last_mut() {
                        current.report = Some(Report {
                            status: *status,
                            summary: summary.clone(),
                            tests: tests.clone(),
                            changed_paths: changed_paths
                                .iter()
                                .filter_map(|path| scope::normalize(path).ok())
                                .collect(),
                            commit: commit.clone(),
                            skills_used: skills_used.clone(),
                        });
                    }
                }
            }
            EventKind::GatePassed {
                task,
                gate,
                findings,
                ..
            }
            | EventKind::GateFailed {
                task,
                gate,
                findings,
                ..
            } => {
                let passed = matches!(event.kind, EventKind::GatePassed { .. });
                if let Some(entry) = self.tasks.get_mut(task) {
                    entry.status = if passed {
                        TaskStatus::Passed
                    } else {
                        TaskStatus::Failed
                    };
                    if let Some(current) = entry.attempts.last_mut() {
                        current.gates.push(GateResult {
                            gate: gate.clone(),
                            passed,
                            findings: findings.clone(),
                            seq: event.seq,
                        });
                    }
                }
            }
            EventKind::Integrated { task, .. } => {
                if let Some(entry) = self.tasks.get_mut(task) {
                    entry.status = TaskStatus::Integrated;
                }
            }
            EventKind::Dry {
                agent,
                reason,
                reset_at_ms,
            } => {
                let info = self.agents.entry(agent.clone()).or_default();
                info.dry = true;
                info.dry_until_ms = *reset_at_ms;
                info.dry_reason = Some(reason.clone());
            }
            EventKind::Available { agent, .. } => {
                let info = self.agents.entry(agent.clone()).or_default();
                info.dry = false;
                info.dry_until_ms = None;
                info.dry_reason = None;
            }
            EventKind::HandedOff { task, to_agent, .. } => {
                if let Some(entry) = self.tasks.get_mut(task)
                    && let Some(current) = entry.attempts.last_mut()
                {
                    current.agent = to_agent.clone();
                }
            }
            EventKind::Escalated {
                task,
                body,
                default_action,
            } => {
                self.escalations.push(Escalation {
                    task: task.clone(),
                    body: body.clone(),
                    default_action: default_action.clone(),
                    seq: event.seq,
                });
            }
            EventKind::Override { task, rule, reason } => {
                // An override with an empty task is run-wide: it belongs to the
                // run, not to any one task.
                let record = OverrideRecord {
                    rule: rule.clone(),
                    reason: reason.clone(),
                    seq: event.seq,
                };
                if task.is_empty() {
                    self.run_overrides.push(record);
                } else if let Some(entry) = self.tasks.get_mut(task) {
                    entry.overrides.push(record);
                }
            }
            EventKind::Resource {
                action,
                kind,
                name,
                task,
            } => {
                let key = (*kind, name.clone());
                match action {
                    ResourceAction::Created => {
                        self.resources.insert(
                            key,
                            ResourceRecord {
                                task: task.clone(),
                                status: ResourceStatus::Live,
                                created_seq: event.seq,
                            },
                        );
                    }
                    ResourceAction::Archived => {
                        if let Some(record) = self.resources.get_mut(&key) {
                            record.status = ResourceStatus::Archived;
                        }
                    }
                    ResourceAction::Removed => {
                        if let Some(record) = self.resources.get_mut(&key) {
                            record.status = ResourceStatus::Removed;
                        }
                    }
                }
            }
            EventKind::RunFinished {} => self.finished = true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Actor;
    use crate::event::ResourceAction;

    /// An event with `kind` at position `seq`.
    fn ev(seq: u64, kind: EventKind) -> Event {
        Event {
            seq,
            at_ms: seq * 1000,
            actor: Actor::Human,
            kind,
        }
    }

    /// A started run, a task `a`, and an override of `rule` on `task`
    /// (run-wide when `task` is empty).
    fn log(task: &str, rule: &str) -> Vec<Event> {
        vec![
            ev(
                1,
                EventKind::RunStarted {
                    run_id: "r1".to_string(),
                    repo: "/repo".to_string(),
                    base_branch: "main".to_string(),
                    integration_branch: "integration/m1".to_string(),
                },
            ),
            ev(
                2,
                EventKind::TaskAdded {
                    task: "a".to_string(),
                    title: "task a".to_string(),
                    scope: vec!["src/a".to_string()],
                    acceptance: vec!["it works".to_string()],
                    tags: Vec::new(),
                    skills: Vec::new(),
                    deps: Vec::new(),
                },
            ),
            ev(
                3,
                EventKind::Override {
                    task: task.to_string(),
                    rule: rule.to_string(),
                    reason: "shipping anyway".to_string(),
                },
            ),
        ]
    }

    #[test]
    fn a_run_wide_override_lands_on_the_run() {
        let folded = RunState::fold(&log("", "no_main_push"));
        let state = match folded {
            Ok(state) => state,
            Err(err) => panic!("a run-wide override must fold: {err}"),
        };
        assert_eq!(state.run_overrides.len(), 1, "{state:?}");
        assert_eq!(state.run_overrides[0].rule, "no_main_push");
        assert_eq!(state.run_overrides[0].reason, "shipping anyway");
        assert_eq!(state.run_overrides[0].seq, 3);
        let task = match state.tasks.get("a") {
            Some(task) => task,
            None => panic!("task a must exist"),
        };
        assert!(task.overrides.is_empty(), "{task:?}");
    }

    #[test]
    fn a_task_override_still_lands_on_the_task() {
        let folded = RunState::fold(&log("a", "integrate_after_failed_gate"));
        let state = match folded {
            Ok(state) => state,
            Err(err) => panic!("a task override must fold: {err}"),
        };
        assert!(state.run_overrides.is_empty(), "{state:?}");
        let task = match state.tasks.get("a") {
            Some(task) => task,
            None => panic!("task a must exist"),
        };
        assert_eq!(task.overrides.len(), 1, "{task:?}");
        assert_eq!(task.overrides[0].rule, "integrate_after_failed_gate");
        assert_eq!(task.overrides[0].seq, 3);
    }

    /// A resource event on `name`.
    fn resource(seq: u64, action: ResourceAction, name: &str) -> Event {
        ev(
            seq,
            EventKind::Resource {
                action,
                kind: ResourceKind::Worktree,
                name: name.to_string(),
                task: Some("a".to_string()),
            },
        )
    }

    /// A run `a` with one task and the given extra events, folded.
    fn fold_extra(extra: Vec<Event>) -> Result<RunState, RuleError> {
        let mut events = log("a", "no_main_push");
        events.truncate(2);
        events.extend(extra);
        RunState::fold(&events)
    }

    #[test]
    fn a_resource_can_be_created_removed_and_created_again() {
        let state = match fold_extra(vec![
            resource(3, ResourceAction::Created, "wt/a"),
            resource(4, ResourceAction::Removed, "wt/a"),
            resource(5, ResourceAction::Created, "wt/a"),
        ]) {
            Ok(state) => state,
            Err(err) => panic!("create, remove, re-create must fold: {err}"),
        };
        let key = (ResourceKind::Worktree, "wt/a".to_string());
        let record = match state.resources.get(&key) {
            Some(record) => record,
            None => panic!("the resource must be in the ledger: {state:?}"),
        };
        assert_eq!(record.status, ResourceStatus::Live);
        assert_eq!(
            record.created_seq, 5,
            "the record tracks the newest creation"
        );
        assert_eq!(record.task.as_deref(), Some("a"));
    }

    #[test]
    fn removing_an_unknown_resource_is_refused() {
        let state = match fold_extra(vec![resource(3, ResourceAction::Created, "wt/a")]) {
            Ok(state) => state,
            Err(err) => panic!("one resource must fold: {err}"),
        };
        assert!(matches!(
            state.check(
                &EventKind::Resource {
                    action: ResourceAction::Removed,
                    kind: ResourceKind::Worktree,
                    name: "wt/other".to_string(),
                    task: None,
                },
                4_000,
            ),
            Err(RuleError::ResourceUnknown { .. })
        ));
    }

    #[test]
    fn archiving_then_removing_a_resource_is_allowed() {
        let state = match fold_extra(vec![
            resource(3, ResourceAction::Created, "wt/a"),
            resource(4, ResourceAction::Archived, "wt/a"),
        ]) {
            Ok(state) => state,
            Err(err) => panic!("archive must fold: {err}"),
        };
        let key = (ResourceKind::Worktree, "wt/a".to_string());
        assert_eq!(
            state.resources.get(&key).map(|record| record.status),
            Some(ResourceStatus::Archived)
        );
        assert!(
            state
                .check(&resource(5, ResourceAction::Removed, "wt/a").kind, 5_000)
                .is_ok()
        );
    }

    #[test]
    fn creating_a_resource_after_the_run_finished_is_refused() {
        let state = match fold_extra(vec![ev(3, EventKind::RunFinished {})]) {
            Ok(state) => state,
            Err(err) => panic!("finishing must fold: {err}"),
        };
        assert!(matches!(
            state.check(&resource(4, ResourceAction::Created, "wt/a").kind, 4_000),
            Err(RuleError::RunFinished)
        ));
    }

    #[test]
    fn removing_a_resource_after_the_run_finished_is_allowed() {
        let state = match fold_extra(vec![
            resource(3, ResourceAction::Created, "wt/a"),
            ev(4, EventKind::RunFinished {}),
        ]) {
            Ok(state) => state,
            Err(err) => panic!("a finished run must keep its ledger: {err}"),
        };
        assert!(
            state
                .check(&resource(5, ResourceAction::Removed, "wt/a").kind, 5_000)
                .is_ok(),
            "the janitor must be able to clean up a finished run"
        );
        assert!(matches!(
            state.check(&resource(6, ResourceAction::Archived, "wt/a").kind, 6_000),
            Ok(())
        ));
    }

    #[test]
    fn a_resource_event_must_name_a_known_task() {
        let state = match fold_extra(vec![]) {
            Ok(state) => state,
            Err(err) => panic!("the bare run must fold: {err}"),
        };
        let kind = EventKind::Resource {
            action: ResourceAction::Created,
            kind: ResourceKind::Branch,
            name: "shprd/r1/a".to_string(),
            task: Some("ghost".to_string()),
        };
        assert!(matches!(
            state.check(&kind, 3_000),
            Err(RuleError::UnknownTask { .. })
        ));
    }
}
