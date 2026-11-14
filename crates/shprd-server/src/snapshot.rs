//! The snapshot projector: the fold in, the board's view out.
//!
//! It is a pure function of [`RunState`] and the moment it is read — no I/O, no
//! clock read of its own — so it is tested with a folded log and a timestamp and
//! nothing else. `GET /api/state` folds the log with the same [`RunState::fold`]
//! the CLI uses and hands the result here.

use shprd_core::state::RunState;
use shprd_views::status;

use crate::dto::{
    AgentDto, AskDto, AttemptDto, DerivedStatusDto, FindingDto, GateDto, NeedDto, NeedKindDto,
    ReportDto, ReviewDto, RunDto, SnapshotDto, TaskDerivedDto, TaskDto, TaskProgressDto,
};

/// Folds a run into the snapshot a client draws, as of `now_ms`.
pub fn snapshot(state: &RunState, now_ms: u64) -> SnapshotDto {
    SnapshotDto {
        seq: state.last_seq,
        run: state.run.as_ref().map(|run| RunDto {
            id: run.run_id.clone(),
            repo: run.repo.clone(),
            base_branch: run.base_branch.clone(),
            integration_branch: run.integration_branch.clone(),
        }),
        tasks: state
            .tasks
            .values()
            .map(|task| task_of(state, task, now_ms))
            .collect(),
        agents: state
            .agents
            .iter()
            .map(|(id, info)| AgentDto {
                id: id.clone(),
                dry: info.dry,
                dry_reason: info.dry_reason.clone(),
                dry_until_ms: info.dry_until_ms,
            })
            .collect(),
        needs: state
            .escalations
            .iter()
            .filter(|escalation| still_needs_a_person(state, escalation))
            .map(|escalation| NeedDto {
                task: escalation.task.clone(),
                body: escalation.body.clone(),
                default_action: escalation.default_action.clone(),
                seq: escalation.seq,
            })
            .collect(),
        asks: open_asks(state, now_ms),
        finished: state.finished,
    }
}

/// Whether an escalation is still the board's to show.
///
/// A task that landed or was dropped has nobody left to ask: the escalation that
/// held it back is history, and a card that kept it would ask a person to
/// approve work that is already in. An escalation about no single task (a run's
/// own) is always kept: the fold has nothing to resolve it against.
fn still_needs_a_person(state: &RunState, escalation: &shprd_core::state::Escalation) -> bool {
    let Some(task) = escalation.task.as_deref() else {
        return true;
    };
    state.tasks.get(task).is_none_or(|task| {
        !matches!(
            task.status,
            shprd_core::state::TaskStatus::Integrated | shprd_core::state::TaskStatus::Dropped
        )
    })
}

/// Every ask still open at `now_ms`, oldest first.
///
/// A hook that timed out leaves its ask behind; it is no longer open, so it must
/// not appear here, or the two endpoints would disagree and a card would offer an
/// ask that answers with `Expired`.
fn open_asks(state: &RunState, now_ms: u64) -> Vec<AskDto> {
    let mut asks: Vec<&shprd_core::state::AskRecord> = state
        .asks
        .values()
        .filter(|ask| ask.is_open_at(now_ms))
        .collect();
    asks.sort_by(|left, right| {
        left.created_at_ms
            .cmp(&right.created_at_ms)
            .then_with(|| left.task.cmp(&right.task))
    });
    asks.into_iter().map(ask).collect()
}

/// One ask as a card draws it.
pub(crate) fn ask(record: &shprd_core::state::AskRecord) -> AskDto {
    AskDto {
        id: record.id.clone(),
        task: record.task.clone(),
        kind: record.kind.into(),
        text: record.text.clone(),
        options: record.options.clone(),
        created_at_ms: record.created_at_ms,
        expires_at_ms: record.expires_at_ms,
    }
}

/// One task, with its attempts in the order they were made, and the word a
/// person sees for it at `now_ms`.
///
/// The derived word comes from the same projector `shprd status` prints
/// ([`shprd_views::status::reading`]), so a card and the CLI agree while an ask
/// is open. A task the fold has no signal for — impossible for one in the plan
/// — reads as nothing started rather than failing the whole snapshot.
pub(crate) fn task_of(state: &RunState, task: &shprd_core::state::Task, now_ms: u64) -> TaskDto {
    let derived = status::reading(state, &task.id, now_ms).unwrap_or_default();
    TaskDto {
        id: task.id.clone(),
        title: task.title.clone(),
        status: task.status.into(),
        derived: TaskDerivedDto {
            status: DerivedStatusDto::from(derived.status),
            need: derived.needs.map(NeedKindDto::from),
            reason: derived.reason,
        },
        scope: task.scope.clone(),
        acceptance: task.acceptance.clone(),
        tags: task.tags.clone(),
        skills: task.skills.clone(),
        deps: task.deps.clone(),
        dropped_reason: task.dropped_reason.clone(),
        last_status: task.last_status.as_ref().map(|status| TaskProgressDto {
            message: status.message.clone(),
            progress: status.progress,
            seq: status.seq,
        }),
        attempts: task.attempts.iter().map(attempt).collect(),
    }
}

/// One attempt, with its report, gates and reviews.
fn attempt(attempt: &shprd_core::state::Attempt) -> AttemptDto {
    AttemptDto {
        n: attempt.n,
        agent: attempt.agent.clone(),
        model: attempt.model.clone(),
        worktree: attempt.worktree.clone(),
        branch: attempt.branch.clone(),
        report: attempt.report.as_ref().map(report),
        gates: attempt
            .gates
            .iter()
            .map(|gate| GateDto {
                gate: gate.gate.clone(),
                passed: gate.passed,
                findings: gate
                    .findings
                    .iter()
                    .map(|finding| FindingDto {
                        severity: finding.severity.into(),
                        title: finding.title.clone(),
                        location: finding.location.clone(),
                        detail: finding.detail.clone(),
                    })
                    .collect(),
                proven: gate.source.is_some(),
            })
            .collect(),
        reviews: attempt
            .reviews
            .iter()
            .map(|review| ReviewDto {
                reviewer: review.reviewer.clone(),
                degraded: review.degraded,
            })
            .collect(),
    }
}

/// What an agent reported about an attempt.
fn report(report: &shprd_core::state::Report) -> ReportDto {
    ReportDto {
        status: report.status.into(),
        summary: report.summary.clone(),
        tests: report.tests.clone(),
        changed_paths: report.changed_paths.clone(),
        commit: report.commit.clone(),
        skills_used: report.skills_used.clone(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{RunState, snapshot};
    use crate::dto::{DerivedStatusDto, NeedKindDto, TaskStatusDto};
    use shprd_core::ask::AskKind;
    use shprd_core::event::{Actor, Event, EventKind};
    use shprd_core::state::{AskAnswerRecord, AskRecord};

    /// The moment the tests read the snapshot at.
    const NOW: u64 = 1_700_000_000_000;

    fn record(id: &str, expires_at_ms: Option<u64>, answer: Option<&str>) -> AskRecord {
        AskRecord {
            id: id.to_string(),
            task: "T1".to_string(),
            kind: AskKind::Permission,
            text: "Run: cargo test".to_string(),
            options: Vec::new(),
            created_at_ms: 1_000,
            expires_at_ms,
            raised_seq: 1,
            answer: answer.map(|text| AskAnswerRecord {
                text: text.to_string(),
                seq: 2,
            }),
        }
    }

    /// A run with one queued task `T1`.
    fn run_with_a_task() -> RunState {
        let events = vec![
            Event {
                seq: 1,
                at_ms: NOW,
                actor: Actor::Engine,
                kind: EventKind::RunStarted {
                    run_id: "r1".to_string(),
                    repo: "/repo/shprd".to_string(),
                    base_branch: "main".to_string(),
                    integration_branch: "integration/m9".to_string(),
                },
            },
            Event {
                seq: 2,
                at_ms: NOW,
                actor: Actor::Engine,
                kind: EventKind::TaskAdded {
                    task: "T1".to_string(),
                    title: "a task".to_string(),
                    scope: vec!["a.txt".to_string()],
                    acceptance: vec!["it works".to_string()],
                    tags: Vec::new(),
                    skills: Vec::new(),
                    deps: Vec::new(),
                },
            },
        ];
        RunState::fold(&events).expect("the fixture log folds")
    }

    #[test]
    fn a_landed_task_keeps_no_needs_you_card() {
        // The M9 gate found `/api/state` still listing the escalation that held a
        // task back after the task had landed: a board reading `needs` would ask
        // a person to approve work that is already in.
        let mut state = run_with_a_task();
        state.escalations.push(shprd_core::state::Escalation {
            task: Some("T1".to_string()),
            body: "T1 is ready to land: run `shprd approve T1`".to_string(),
            default_action: "wait for a human to approve T1".to_string(),
            seq: 3,
        });
        let open = snapshot(&state, NOW);
        assert_eq!(open.needs.len(), 1, "the task still waits on a person");

        // Landing the task resolves it: nothing is left to decide.
        state
            .tasks
            .get_mut("T1")
            .expect("the fixture's task")
            .status = shprd_core::state::TaskStatus::Integrated;
        let landed = snapshot(&state, NOW);
        assert!(
            landed.needs.is_empty(),
            "a landed task needs nobody: {:?}",
            landed.needs
        );

        // A run-wide escalation is not a task's, so it stays.
        state.escalations.push(shprd_core::state::Escalation {
            task: None,
            body: "the orchestrator is waiting on its own prompt".to_string(),
            default_action: "answer it in its tab".to_string(),
            seq: 4,
        });
        let run_wide = snapshot(&state, NOW);
        assert_eq!(run_wide.needs.len(), 1, "{:?}", run_wide.needs);
        assert!(run_wide.needs[0].task.is_none());
    }

    #[test]
    fn a_timed_out_ask_is_not_in_the_snapshot() {
        // The endpoint lists what is still open at the moment it is read, so a
        // hook that timed out must drop out here just as it does from
        // `GET /api/asks` — otherwise a card would offer an ask that answers
        // with `Expired`.
        let mut state = RunState::default();
        state.asks.insert(
            ("T1".to_string(), "open".to_string()),
            record("open", None, None),
        );
        state.asks.insert(
            ("T1".to_string(), "expired".to_string()),
            record("expired", Some(5_000), None),
        );
        state.asks.insert(
            ("T1".to_string(), "answered".to_string()),
            record("answered", None, Some("allow")),
        );
        let snap = snapshot(&state, 6_000);
        let ids: Vec<&str> = snap.asks.iter().map(|ask| ask.id.as_str()).collect();
        assert_eq!(ids, vec!["open"]);
    }

    #[test]
    fn an_open_ask_makes_the_tasks_derived_word_needs_you() {
        // The bug the M9 gate found: `GET /api/state` carried no derived word,
        // so a card an agent was waiting on was invisible on the board.
        let mut state = run_with_a_task();
        let before = snapshot(&state, NOW);
        assert_eq!(before.tasks[0].derived.status, DerivedStatusDto::ToDo);
        assert_eq!(before.tasks[0].derived.need, None);
        assert_eq!(
            before.tasks[0].status,
            TaskStatusDto::Queued,
            "the stored state stays its own field"
        );

        state.asks.insert(
            ("T1".to_string(), "ask-1".to_string()),
            record("ask-1", None, None),
        );
        let open = snapshot(&state, NOW);
        assert_eq!(open.tasks[0].derived.status, DerivedStatusDto::NeedsYou);
        assert_eq!(open.tasks[0].derived.need, Some(NeedKindDto::Asks));
        assert_eq!(
            open.tasks[0].derived.reason.as_deref(),
            Some("Run: cargo test")
        );

        // The answer takes it back out.
        state.asks.insert(
            ("T1".to_string(), "ask-1".to_string()),
            record("ask-1", None, Some("allow")),
        );
        let answered = snapshot(&state, NOW);
        assert_eq!(answered.tasks[0].derived.status, DerivedStatusDto::ToDo);
        assert_eq!(answered.tasks[0].derived.need, None);
    }

    #[test]
    fn a_working_task_is_not_called_stopped_by_the_board() {
        // The snapshot reads the log alone and has no terminal to look at, so
        // it must not turn "no overlay" into "no agent left".
        let events = vec![
            Event {
                seq: 1,
                at_ms: NOW,
                actor: Actor::Engine,
                kind: EventKind::RunStarted {
                    run_id: "r1".to_string(),
                    repo: "/repo/shprd".to_string(),
                    base_branch: "main".to_string(),
                    integration_branch: "integration/m9".to_string(),
                },
            },
            Event {
                seq: 2,
                at_ms: NOW,
                actor: Actor::Engine,
                kind: EventKind::TaskAdded {
                    task: "T1".to_string(),
                    title: "a task".to_string(),
                    scope: vec!["a.txt".to_string()],
                    acceptance: vec!["it works".to_string()],
                    tags: Vec::new(),
                    skills: Vec::new(),
                    deps: Vec::new(),
                },
            },
            Event {
                seq: 3,
                at_ms: NOW,
                actor: Actor::Engine,
                kind: EventKind::Spawned {
                    task: "T1".to_string(),
                    attempt: 1,
                    agent: "claude".to_string(),
                    model: None,
                    worktree: "/wt/T1".to_string(),
                    branch: "shprd/r1/T1".to_string(),
                },
            },
        ];
        let state = RunState::fold(&events).expect("the fixture log folds");
        let snap = snapshot(&state, NOW);
        assert_eq!(snap.tasks[0].derived.status, DerivedStatusDto::Working);
        assert_eq!(snap.tasks[0].derived.need, None);
    }
}
