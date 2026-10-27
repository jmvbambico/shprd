//! Run state: the fold of the event log, and the mutation each event causes.
//!
//! [`RunState::apply`] checks an event against the transition rules in
//! [`crate::rules`] and only then mutates. [`RunState::fold`] replays a whole
//! log under exactly the same rules, so an illegal log cannot quietly become
//! state.

use std::collections::BTreeMap;

use crate::event::{AgentId, Event, EventKind, Finding, ReportStatus, TaskId};
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
    /// The agent reported an outcome for the current attempt.
    Reported,
    /// Its gates passed.
    Passed,
    /// It failed, and may be retried on a new attempt.
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
                if let Some(entry) = self.tasks.get_mut(task) {
                    entry.status = TaskStatus::Reported;
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
                // A run-wide override names no task, so it has no home in the
                // per-task override list yet.
                if let Some(entry) = self.tasks.get_mut(task) {
                    entry.overrides.push(OverrideRecord {
                        rule: rule.clone(),
                        reason: reason.clone(),
                        seq: event.seq,
                    });
                }
            }
            EventKind::RunFinished {} => self.finished = true,
        }
    }
}
