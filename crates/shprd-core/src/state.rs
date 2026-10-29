//! Run state: the fold of the event log, and the mutation each event causes.
//!
//! [`RunState::apply`] checks an event against the transition rules in
//! [`crate::rules`] and only then mutates. [`RunState::fold`] replays a whole
//! log under exactly the same rules, so an illegal log cannot quietly become
//! state.

use std::collections::BTreeMap;

use crate::event::{
    Actor, AgentId, Event, EventKind, Finding, ReportStatus, ResourceAction, ResourceKind,
    ReviewSource, TaskId,
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
    /// The tasks the finished run ended with unfinished, in log order.
    ///
    /// Non-empty only when a human ended the run anyway (`shprd down
    /// --abandon`): it names the work that was left behind, and the branches
    /// that keep it alive after the run is archived.
    pub abandoned: Vec<TaskId>,
    /// The last run-wide gate: the head it ran on, whether it passed, and the
    /// sequence number of the event that recorded it.
    ///
    /// Only the newest one is kept: the full history is the log's job.
    pub last_full_gate: Option<(String, bool, u64)>,
    /// The last base sync: the base commit that was merged in, the integration
    /// head it produced, and the sequence number that recorded it.
    ///
    /// Only the newest one is kept: the full history is the log's job. It says
    /// whether the run was ever brought up to date, which a later reader of a
    /// merged pull request wants to know.
    pub last_sync: Option<(String, String, u64)>,
    /// The run's pull request, once `shprd land` opened one.
    ///
    /// One run proposes one pull request, so the newest `PrOpened` wins and a
    /// merge only flips [`PrInfo::merged`].
    pub pr: Option<PrInfo>,
    /// The orchestrator shprd started, once `up` launched one.
    ///
    /// The process group it carries is what lets a later `up` recognise its
    /// own session: a pane whose foreground group is not this one was
    /// relaunched by the host, not by shprd.
    pub orchestrator: Option<OrchestratorInfo>,
    /// The watchdog shprd started, once `up` launched one.
    ///
    /// The watchdog is the run's automation rather than an agent: it turns a
    /// reviewer's findings into a verdict and hands off a dry worker. `up`
    /// keeps it alive across invocations, which is what the pane and its
    /// process group are for.
    pub watch: Option<WatchInfo>,
    /// The last log sequence the orchestrator was told about, if it ever was.
    ///
    /// The watchdog prompts the orchestrator's pane when something needs it to
    /// act, and records that delivery as this watermark. A restarted watchdog
    /// reads it back and so neither replays old notices nor drops new ones.
    /// `None` means nothing has been delivered yet: the next pass tells the
    /// orchestrator only about the state as it is now, never the log's history.
    pub notified_up_to_seq: Option<u64>,
    /// Sequence number of the last applied event, 0 before the first.
    pub last_seq: u64,
}

/// What the run knows about its pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrInfo {
    /// The forge's number for it.
    pub number: u64,
    /// The URL a human opens to read, approve or merge it.
    pub url: String,
    /// The branch it proposes to merge.
    pub head: String,
    /// The integration head a human approved in shprd, if one has.
    ///
    /// GitHub refuses a self-approval, so a solo maintainer's approval is
    /// recorded here instead. It names one head: an approval of an older head
    /// says nothing about the commits pushed since.
    pub approved_head: Option<String>,
    /// Whether the forge has merged it.
    pub merged: bool,
}

/// The orchestrator shprd started for this run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrchestratorInfo {
    /// The title of its tab.
    pub title: String,
    /// The pane it runs in.
    pub pane: String,
    /// The pane's foreground process group when shprd started it.
    pub process_group: u32,
}

/// The watchdog shprd started for this run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchInfo {
    /// The pane it runs in, inside the human's chat tab.
    pub pane: String,
    /// The pane's foreground process group when shprd started it, when the
    /// host could report one.
    pub process_group: Option<u32>,
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
    /// Why a human dropped it, once one did.
    ///
    /// Kept beside the status rather than looked up in the log, because every
    /// view that draws a task reads the folded state: the pull request's
    /// `## Dropped` section is the reason a reader is looking for.
    pub dropped_reason: Option<String>,
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
    /// A human decided the task will not be done.
    ///
    /// Terminal like [`TaskStatus::Integrated`]: the run is not waiting on it
    /// any more, so ending the run strands nothing. Unlike an integrated task,
    /// none of it is on the integration branch, so the run's pull request lists
    /// it rather than carrying it.
    Dropped,
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
    /// Reviews dispatched for this attempt, in order.
    pub reviews: Vec<ReviewRequest>,
}

/// A review handed to a reviewer for an attempt.
///
/// It records who reviewed and whether the review was independent, so a
/// landing decision reads the fact instead of guessing it from prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewRequest {
    /// The agent string the review was handed to.
    pub reviewer: AgentId,
    /// True when the reviewer shares the author's vendor.
    pub degraded: bool,
    /// The `seq` of the event that recorded this request.
    ///
    /// A restart records a second request for the same attempt, so the two are
    /// told apart by position: a verdict answers the request whose seq it
    /// carries, and only the latest one is still answerable.
    pub seq: u64,
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
    /// Provenance of a review verdict, when the gate was `review`.
    ///
    /// A review gate with no source was recorded by something other than the
    /// reviewer's packet, which is exactly what a landing decision must not
    /// read as an independent review.
    pub source: Option<ReviewSource>,
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

    /// Checks `kind` as `actor` caused it against the rules, applying nothing.
    ///
    /// This is the check a verb about to write wants: the rules read the actor
    /// — only the engine may record a review verdict — so a pre-check that
    /// assumed a human would refuse what the append would accept.
    pub fn check_as(&self, actor: &Actor, kind: &EventKind, now_ms: u64) -> Result<(), RuleError> {
        rules::check_as(self, actor, kind, now_ms)
    }

    /// Checks `event` against the rules, then applies it and records its seq.
    pub fn apply(&mut self, event: &Event) -> Result<(), RuleError> {
        rules::replay_as(self, &event.actor, &event.kind, event.at_ms)?;
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
                        dropped_reason: None,
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
                        reviews: Vec::new(),
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
                source,
                ..
            }
            | EventKind::GateFailed {
                task,
                gate,
                findings,
                source,
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
                            source: source.clone(),
                            seq: event.seq,
                        });
                    }
                }
            }
            EventKind::ReviewRequested {
                task,
                reviewer,
                degraded,
                ..
            } => {
                // A review dispatch never moves the task on: it stays Reported
                // until the review gate records its verdict.
                if let Some(entry) = self.tasks.get_mut(task)
                    && let Some(current) = entry.attempts.last_mut()
                {
                    current.reviews.push(ReviewRequest {
                        reviewer: reviewer.clone(),
                        degraded: *degraded,
                        seq: event.seq,
                    });
                }
            }
            EventKind::Integrated { task, .. } => {
                if let Some(entry) = self.tasks.get_mut(task) {
                    entry.status = TaskStatus::Integrated;
                }
            }
            EventKind::TaskDropped { task, reason } => {
                // Terminal, and the reason is kept with it: a view that draws
                // the task out of the folded state has to say why it is gone.
                if let Some(entry) = self.tasks.get_mut(task) {
                    entry.status = TaskStatus::Dropped;
                    entry.dropped_reason = Some(reason.clone());
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
            EventKind::FullGate { head, passed, .. } => {
                self.last_full_gate = Some((head.clone(), *passed, event.seq));
            }
            EventKind::PrOpened { number, url, head } => {
                // Refreshing the body is not a new approval. The sha it names
                // is what counts, and landing honours it only while that sha
                // is still the integration head.
                let approved_head = self.pr.as_ref().and_then(|pr| pr.approved_head.clone());
                self.pr = Some(PrInfo {
                    number: *number,
                    url: url.clone(),
                    head: head.clone(),
                    approved_head,
                    merged: false,
                });
            }
            EventKind::PrMerged { number, head } => {
                self.pr = Some(PrInfo {
                    number: *number,
                    url: self
                        .pr
                        .as_ref()
                        .map(|pr| pr.url.clone())
                        .unwrap_or_default(),
                    head: head.clone(),
                    approved_head: self.pr.as_ref().and_then(|pr| pr.approved_head.clone()),
                    merged: true,
                });
            }
            EventKind::Approved { head_sha, .. } => {
                if let Some(pr) = self.pr.as_mut() {
                    pr.approved_head = Some(head_sha.clone());
                }
            }
            EventKind::OrchestratorLaunched {
                title,
                pane,
                process_group,
            } => {
                self.orchestrator = Some(OrchestratorInfo {
                    title: title.clone(),
                    pane: pane.clone(),
                    process_group: *process_group,
                });
            }
            EventKind::WatchLaunched {
                pane,
                process_group,
            } => {
                self.watch = Some(WatchInfo {
                    pane: pane.clone(),
                    process_group: *process_group,
                });
            }
            EventKind::OrchestratorNotified { up_to_seq } => {
                self.notified_up_to_seq = Some(*up_to_seq);
            }
            EventKind::BaseSynced { base_sha, int_sha } => {
                self.last_sync = Some((base_sha.clone(), int_sha.clone(), event.seq));
            }
            EventKind::RunFinished { abandoned } => {
                self.finished = true;
                self.abandoned = abandoned.clone();
            }
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
        let state = match fold_extra(vec![ev(
            3,
            EventKind::RunFinished {
                abandoned: Vec::new(),
            },
        )]) {
            Ok(state) => state,
            Err(err) => panic!("finishing must fold: {err}"),
        };
        assert!(matches!(
            state.check(&resource(4, ResourceAction::Created, "wt/a").kind, 4_000),
            Err(RuleError::RunFinished)
        ));
    }

    #[test]
    fn a_run_finished_logged_before_abandonment_still_folds() {
        // `abandoned` arrived with `--abandon`. An old log's last event carries
        // no such field, so it must fold as a run that abandoned nothing
        // instead of failing to parse.
        let text = r#"{"seq":3,"at_ms":5,"actor":{"type":"engine"},"kind":"run_finished"}"#;
        let event: Event = serde_json::from_str(text).unwrap_or_else(|err| panic!("{err}"));
        let state = fold_extra(vec![event]).unwrap_or_else(|err| panic!("{err}"));
        assert!(state.finished);
        assert!(state.abandoned.is_empty());
    }

    #[test]
    fn removing_a_resource_after_the_run_finished_is_allowed() {
        let state = match fold_extra(vec![
            resource(3, ResourceAction::Created, "wt/a"),
            ev(
                4,
                EventKind::RunFinished {
                    abandoned: Vec::new(),
                },
            ),
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
    fn the_newest_full_gate_is_the_one_kept() {
        let failed = ev(
            3,
            EventKind::FullGate {
                head: "aaa111".to_string(),
                passed: false,
                steps: Vec::new(),
            },
        );
        let passed = ev(
            4,
            EventKind::FullGate {
                head: "bbb222".to_string(),
                passed: true,
                steps: Vec::new(),
            },
        );
        let state = match fold_extra(vec![failed, passed]) {
            Ok(state) => state,
            Err(err) => panic!("two full gates must fold: {err}"),
        };
        assert_eq!(
            state.last_full_gate,
            Some(("bbb222".to_string(), true, 4)),
            "the later event wins"
        );
    }

    #[test]
    fn the_newest_base_sync_is_the_one_kept() {
        let first = ev(
            3,
            EventKind::BaseSynced {
                base_sha: "aaa111".to_string(),
                int_sha: "bbb222".to_string(),
            },
        );
        let second = ev(
            4,
            EventKind::BaseSynced {
                base_sha: "ccc333".to_string(),
                int_sha: "ddd444".to_string(),
            },
        );
        let state = match fold_extra(vec![first, second]) {
            Ok(state) => state,
            Err(err) => panic!("two base syncs must fold: {err}"),
        };
        assert_eq!(
            state.last_sync,
            Some(("ccc333".to_string(), "ddd444".to_string(), 4)),
            "the later event wins"
        );
    }

    #[test]
    fn a_pull_request_is_opened_then_merged() {
        let state = match fold_extra(vec![
            ev(
                3,
                EventKind::PrOpened {
                    number: 7,
                    url: "https://example.invalid/pr/7".to_string(),
                    head: "shprd/r1/int".to_string(),
                },
            ),
            ev(
                4,
                EventKind::PrMerged {
                    number: 7,
                    head: "bbb222".to_string(),
                },
            ),
        ]) {
            Ok(state) => state,
            Err(err) => panic!("open then merge must fold: {err}"),
        };
        assert_eq!(
            state.pr,
            Some(PrInfo {
                number: 7,
                url: "https://example.invalid/pr/7".to_string(),
                head: "bbb222".to_string(),
                approved_head: None,
                merged: true,
            }),
            "the merge keeps the url and records the merged head"
        );
    }

    #[test]
    fn an_orchestrator_launch_records_its_process_group() {
        let state = match fold_extra(vec![ev(
            3,
            EventKind::OrchestratorLaunched {
                title: "★ claude·orchestrator".to_string(),
                pane: "w1:p2".to_string(),
                process_group: 88158,
            },
        )]) {
            Ok(state) => state,
            Err(err) => panic!("a launch must fold: {err}"),
        };
        assert_eq!(
            state.orchestrator,
            Some(OrchestratorInfo {
                title: "★ claude·orchestrator".to_string(),
                pane: "w1:p2".to_string(),
                process_group: 88158,
            })
        );
    }

    /// A `PrOpened` for pull request 7 at seq 3.
    fn opened(seq: u64) -> Event {
        ev(
            seq,
            EventKind::PrOpened {
                number: 7,
                url: "https://example.invalid/pr/7".to_string(),
                head: "shprd/r1/int".to_string(),
            },
        )
    }

    #[test]
    fn an_approval_records_the_head_it_named() {
        let state = match fold_extra(vec![
            opened(3),
            ev(
                4,
                EventKind::Approved {
                    number: 7,
                    head_sha: "bbb222".to_string(),
                },
            ),
        ]) {
            Ok(state) => state,
            Err(err) => panic!("open then approve must fold: {err}"),
        };
        let pr = match state.pr {
            Some(pr) => pr,
            None => panic!("the pull request must be in the state"),
        };
        assert_eq!(pr.approved_head, Some("bbb222".to_string()));
        assert!(!pr.merged, "an approval merges nothing by itself");
    }

    #[test]
    fn an_approval_without_an_opened_pull_request_is_refused() {
        let state = match fold_extra(vec![]) {
            Ok(state) => state,
            Err(err) => panic!("the bare run must fold: {err}"),
        };
        let kind = EventKind::Approved {
            number: 7,
            head_sha: "bbb222".to_string(),
        };
        assert!(matches!(
            state.check(&kind, 3_000),
            Err(RuleError::UnopenedPullRequest { number: 7 })
        ));
    }

    #[test]
    fn an_approval_of_a_merged_pull_request_is_refused() {
        let state = match fold_extra(vec![
            opened(3),
            ev(
                4,
                EventKind::PrMerged {
                    number: 7,
                    head: "bbb222".to_string(),
                },
            ),
        ]) {
            Ok(state) => state,
            Err(err) => panic!("open then merge must fold: {err}"),
        };
        let kind = EventKind::Approved {
            number: 7,
            head_sha: "bbb222".to_string(),
        };
        assert!(matches!(
            state.check(&kind, 5_000),
            Err(RuleError::MergedPullRequest { number: 7 })
        ));
    }

    #[test]
    fn an_orchestrator_cannot_approve() {
        let mut events = log("a", "no_main_push");
        events.truncate(2);
        events.push(opened(3));
        events.push(Event {
            seq: 4,
            at_ms: 4_000,
            actor: Actor::Orchestrator,
            kind: EventKind::Approved {
                number: 7,
                head_sha: "bbb222".to_string(),
            },
        });
        assert!(matches!(
            RunState::fold(&events),
            Err(RuleError::NotHuman {
                actor: Actor::Orchestrator
            })
        ));
    }

    #[test]
    fn a_merge_without_an_opened_pull_request_is_refused() {
        let state = match fold_extra(vec![]) {
            Ok(state) => state,
            Err(err) => panic!("the bare run must fold: {err}"),
        };
        let kind = EventKind::PrMerged {
            number: 7,
            head: "abc1234".to_string(),
        };
        assert!(matches!(
            state.check(&kind, 3_000),
            Err(RuleError::UnknownPullRequest { number: 7 })
        ));
    }

    #[test]
    fn a_merge_of_another_pull_request_is_refused() {
        let state = match fold_extra(vec![ev(
            3,
            EventKind::PrOpened {
                number: 7,
                url: "https://example.invalid/pr/7".to_string(),
                head: "shprd/r1/int".to_string(),
            },
        )]) {
            Ok(state) => state,
            Err(err) => panic!("opening a pull request must fold: {err}"),
        };
        let kind = EventKind::PrMerged {
            number: 8,
            head: "abc1234".to_string(),
        };
        assert!(matches!(
            state.check(&kind, 4_000),
            Err(RuleError::UnknownPullRequest { number: 8 })
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
