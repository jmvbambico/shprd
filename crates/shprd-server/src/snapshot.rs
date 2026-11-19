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
    PrStateDto, ReportDto, ReviewDto, RunDto, SnapshotDto, TaskDerivedDto, TaskDto, TaskPrDto,
    TaskProgressDto,
};

/// What `GET /api/state` answers with when the repository has no run yet.
///
/// A service that starts before `shprd run start` must not look like a run that
/// is merely empty, so the board says what to do instead of drawing nothing.
pub const NO_RUN_HINT: &str = "no run yet: start one with `shprd run start`";

/// Folds a run into the snapshot a client draws, as of `now_ms`.
///
/// The last pass's outcome is not in the log — a pass is the engine's own act,
/// not an event — so [`SnapshotDto::last_tick`] is left to the caller that can
/// read it ([`crate::pass`]). Everything else here is the fold's.
pub fn snapshot(state: &RunState, now_ms: u64) -> SnapshotDto {
    SnapshotDto {
        seq: state.last_seq,
        run: state.run.as_ref().map(|run| RunDto {
            id: run.run_id.clone(),
            repo: run.repo.clone(),
            base_branch: run.base_branch.clone(),
            integration_branch: run.integration_branch.clone(),
        }),
        last_tick: None,
        hint: state.run.is_none().then(|| NO_RUN_HINT.to_string()),
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
            .filter_map(|escalation| {
                shprd_views::cards::card(state, escalation).map(|card| NeedDto {
                    task: escalation.task.clone(),
                    body: card.body,
                    default_action: card.default_action,
                    seq: escalation.seq,
                })
            })
            .collect(),
        asks: open_asks(state, now_ms),
        finished: state.finished,
        spaces: Vec::new(),
        live_agents: Vec::new(),
        role_titles: Default::default(),
    }
}

/// Whether the card `body` names is about the attempt the task is on.
///
/// A card for a review that spent its rounds names its attempt, and it is
/// history once a newer attempt exists: the failure it named has been answered,
/// by a person's "retry" or by a spawn someone ran by hand.
fn card_is_current(task: &shprd_core::state::Task, body: &str) -> bool {
    match shprd_core::gate::card_attempt(&task.id, body) {
        Some(attempt) => task
            .latest_attempt()
            .is_none_or(|latest| latest.n <= attempt),
        None => true,
    }
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
        // A new attempt retires what the old one left open.
        .filter(|ask| {
            state
                .tasks
                .get(&ask.task)
                .is_none_or(|task| !ask.is_retired_by_newer_attempt(task))
        })
        // A card about a failed review is answered by a newer attempt, exactly
        // as its escalation is: one decision, one card on either list.
        .filter(|ask| {
            state
                .tasks
                .get(&ask.task)
                .is_none_or(|task| card_is_current(task, &ask.text))
        })
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
        pr: task.pr.as_ref().map(task_pr),
        space: None,
        phase: task
            .tags
            .iter()
            .find_map(|tag| tag.strip_prefix("phase:"))
            .map(|word| word.trim().to_string())
            .filter(|word| !word.is_empty()),
    }
}

/// One task's pull request, as its card draws it.
fn task_pr(pr: &shprd_core::state::PrInfo) -> TaskPrDto {
    TaskPrDto {
        number: pr.number,
        url: pr.url.clone(),
        head: pr.head.clone(),
        state: if pr.merged {
            PrStateDto::Merged
        } else {
            PrStateDto::Open
        },
        approved: pr.approved_head.is_some(),
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
        checks: report
            .checks
            .iter()
            .map(|check| crate::dto::CheckDto {
                gate: check.gate.clone(),
                command: check.command.clone(),
                exit_code: check.exit_code,
                output_tail: check.output_tail.clone(),
            })
            .collect(),
        changed_paths: report.changed_paths.clone(),
        commit: report.commit.clone(),
        skills_used: report.skills_used.clone(),
        proofs: report.proofs.clone(),
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

    #[test]
    fn a_report_carries_the_proof_it_named() {
        let report = shprd_core::state::Report {
            status: shprd_core::event::ReportStatus::Completed,
            summary: "s".to_string(),
            tests: Vec::new(),
            checks: Vec::new(),
            changed_paths: Vec::new(),
            commit: None,
            skills_used: Vec::new(),
            proofs: vec![".shprd/proof/T5-usage.txt".to_string()],
        };
        assert_eq!(
            super::report(&report).proofs,
            vec![".shprd/proof/T5-usage.txt".to_string()]
        );
    }

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
    fn a_task_that_landed_through_a_pull_request_carries_it() {
        // The task PR card reads this and nothing else: a number, a state and a
        // link. A task that landed locally has none, and the board must be able
        // to tell that apart from "a pull request nobody opened yet".
        let mut state = run_with_a_task();
        let pr = shprd_core::state::PrInfo {
            number: 12,
            url: "https://example.invalid/pr/12".to_string(),
            head: "shprd/r1/T1".to_string(),
            approved_head: None,
            merged: false,
        };
        state.tasks.get_mut("T1").expect("the fixture's task").pr = Some(pr);
        let open = snapshot(&state, NOW);
        let task = &open.tasks[0];
        let carried = task.pr.as_ref().expect("the pull request is carried");
        assert_eq!(carried.number, 12);
        assert_eq!(carried.state, crate::dto::PrStateDto::Open);
        assert_eq!(carried.url, "https://example.invalid/pr/12");
        assert!(!carried.approved, "nothing approved it yet");

        let merged = state
            .tasks
            .get_mut("T1")
            .expect("the fixture's task")
            .pr
            .as_mut()
            .expect("the pull request");
        merged.merged = true;
        merged.approved_head = Some("abc1234".to_string());
        let landed = snapshot(&state, NOW);
        let carried = landed.tasks[0].pr.as_ref().expect("still carried");
        assert_eq!(carried.state, crate::dto::PrStateDto::Merged);
        assert!(carried.approved);

        // A local landing opens no pull request at all.
        state.tasks.get_mut("T1").expect("the fixture's task").pr = None;
        let local = snapshot(&state, NOW);
        assert!(local.tasks[0].pr.is_none());
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
    fn a_card_about_an_attempt_a_newer_one_replaced_is_history() {
        // A review that spent its rounds raises a card naming its attempt. Once
        // a newer attempt exists — a person's "retry", or a spawn by hand — the
        // failure it named has been answered, so both its lists drop it.
        let mut state = run_with_a_task();
        state
            .tasks
            .get_mut("T1")
            .expect("the fixture's task")
            .attempts
            .push(shprd_core::state::Attempt {
                n: 1,
                agent: String::from("claude"),
                model: None,
                worktree: String::from("/wt/T1"),
                branch: String::from("shprd/r1/T1"),
                spawn_seq: Some(1),
                stopped: false,
                report: None,
                gates: Vec::new(),
                reviews: Vec::new(),
            });
        let body = shprd_core::gate::rounds_spent_body(
            state.tasks.get("T1").expect("the fixture's task"),
            1,
            2,
        );
        state.escalations.push(shprd_core::state::Escalation {
            task: Some("T1".to_string()),
            body: body.clone(),
            default_action: String::from("retry, take it, or drop it"),
            seq: 3,
        });
        state.asks.insert(
            ("T1".to_string(), shprd_core::gate::rounds_spent_ask(1)),
            AskRecord {
                id: shprd_core::gate::rounds_spent_ask(1),
                task: String::from("T1"),
                kind: AskKind::Question,
                text: body,
                options: shprd_core::gate::ROUNDS_SPENT_OPTIONS
                    .iter()
                    .map(|choice| (*choice).to_string())
                    .collect(),
                created_at_ms: 1_000,
                expires_at_ms: None,
                raised_seq: 4,
                answer: None,
            },
        );
        let waiting = snapshot(&state, NOW);
        assert_eq!(waiting.needs.len(), 1, "{:?}", waiting.needs);
        assert_eq!(waiting.asks.len(), 1, "{:?}", waiting.asks);

        // A newer attempt answers both: the card is about attempt 1, and the
        // task is on attempt 2.
        state
            .tasks
            .get_mut("T1")
            .expect("the fixture's task")
            .attempts
            .push(shprd_core::state::Attempt {
                n: 2,
                agent: String::from("claude"),
                model: None,
                worktree: String::from("/wt/T1"),
                branch: String::from("shprd/r1/T1"),
                spawn_seq: Some(6),
                stopped: false,
                report: None,
                gates: Vec::new(),
                reviews: Vec::new(),
            });
        let answered = snapshot(&state, NOW);
        assert!(answered.needs.is_empty(), "{:?}", answered.needs);
        assert!(answered.asks.is_empty(), "{:?}", answered.asks);
    }

    #[test]
    fn a_card_about_a_send_back_that_did_not_start_is_history_too() {
        // A card raised because a failed attempt could not be sent back names
        // that attempt as well, so the same rule drops it once a newer attempt
        // has answered it.
        let mut state = run_with_a_task();
        state
            .tasks
            .get_mut("T1")
            .expect("the fixture's task")
            .attempts
            .push(shprd_core::state::Attempt {
                n: 1,
                agent: String::from("claude"),
                model: None,
                worktree: String::from("/wt/T1"),
                branch: String::from("shprd/r1/T1"),
                spawn_seq: Some(1),
                stopped: false,
                report: None,
                gates: Vec::new(),
                reviews: Vec::new(),
            });
        state.escalations.push(shprd_core::state::Escalation {
            task: Some("T1".to_string()),
            body: format!(
                "{}no workspace",
                shprd_core::gate::send_back_failure_prefix("T1", 1)
            ),
            default_action: String::from("spawn it by hand"),
            seq: 3,
        });
        let waiting = snapshot(&state, NOW);
        assert_eq!(waiting.needs.len(), 1, "{:?}", waiting.needs);

        state
            .tasks
            .get_mut("T1")
            .expect("the fixture's task")
            .attempts
            .push(shprd_core::state::Attempt {
                n: 2,
                agent: String::from("claude"),
                model: None,
                worktree: String::from("/wt/T1"),
                branch: String::from("shprd/r1/T1"),
                spawn_seq: Some(6),
                stopped: false,
                report: None,
                gates: Vec::new(),
                reviews: Vec::new(),
            });
        let answered = snapshot(&state, NOW);
        assert!(answered.needs.is_empty(), "{:?}", answered.needs);
    }

    #[test]
    fn an_answered_card_names_the_verb_and_leaves_the_ask_list() {
        // Bug 1 of gate run #4: answering a spent-review card with `drop it`
        // consumed the ask, and the card stayed on `needs` pointing at it, so
        // every click on the card was refused. The card now says the verb the
        // answer leaves, and it stops needing a person once that verb's record
        // is in the log.
        let mut state = run_with_a_task();
        state
            .tasks
            .get_mut("T1")
            .expect("the fixture's task")
            .attempts
            .push(shprd_core::state::Attempt {
                n: 1,
                agent: String::from("claude"),
                model: None,
                worktree: String::from("/wt/T1"),
                branch: String::from("shprd/r1/T1"),
                spawn_seq: Some(1),
                stopped: false,
                report: None,
                gates: vec![shprd_core::state::GateResult {
                    gate: String::from(shprd_core::gate::REVIEW_GATE),
                    passed: false,
                    findings: vec![shprd_core::event::Finding {
                        severity: shprd_core::event::Severity::Major,
                        title: String::from("No evidence that test_calc.py still passes"),
                        location: None,
                        detail: String::new(),
                    }],
                    source: None,
                    seq: 3,
                }],
                reviews: Vec::new(),
            });
        let body = shprd_core::gate::rounds_spent_body(
            state.tasks.get("T1").expect("the fixture's task"),
            1,
            1,
        );
        state.escalations.push(shprd_core::state::Escalation {
            task: Some("T1".to_string()),
            body: body.clone(),
            default_action: String::from(shprd_core::gate::ESCALATION_ACTION),
            seq: 4,
        });
        state.asks.insert(
            ("T1".to_string(), shprd_core::gate::rounds_spent_ask(1)),
            AskRecord {
                id: shprd_core::gate::rounds_spent_ask(1),
                task: String::from("T1"),
                kind: AskKind::Question,
                text: body,
                options: shprd_core::gate::ROUNDS_SPENT_OPTIONS
                    .iter()
                    .map(|choice| (*choice).to_string())
                    .collect(),
                created_at_ms: 1_000,
                expires_at_ms: None,
                raised_seq: 5,
                answer: Some(AskAnswerRecord {
                    text: String::from("drop it"),
                    seq: 6,
                }),
            },
        );

        let answered = snapshot(&state, NOW);
        assert_eq!(answered.needs.len(), 1, "{:?}", answered.needs);
        let card = &answered.needs[0];
        assert!(!card.body.contains("answer the card"), "{}", card.body);
        assert!(
            card.body.contains("you answered `drop it`"),
            "{}",
            card.body
        );
        assert!(!card.body.contains("shprd "), "{}", card.body);
        assert!(
            answered.asks.is_empty(),
            "the ask was consumed by the answer: {:?}",
            answered.asks
        );

        // Dropping it is the verb the card named: once it is in the log there
        // is nothing left to ask.
        state
            .tasks
            .get_mut("T1")
            .expect("the fixture's task")
            .status = shprd_core::state::TaskStatus::Dropped;
        let dropped = snapshot(&state, NOW);
        assert!(dropped.needs.is_empty(), "{:?}", dropped.needs);
    }

    #[test]
    fn a_task_with_an_open_card_reads_needs_you_on_the_board_too() {
        // Bug 4 of gate run #4: the row said Working while the card sat in
        // `needs`. The derived word and the card now come from one place.
        let mut state = run_with_a_task();
        state.escalations.push(shprd_core::state::Escalation {
            task: Some("T1".to_string()),
            body: String::from("T1: the engine's review did not start: no workspace yet"),
            default_action: String::from("create the workspace"),
            seq: 3,
        });
        let board = snapshot(&state, NOW);
        assert_eq!(board.needs.len(), 1, "{:?}", board.needs);
        let row = board
            .tasks
            .iter()
            .find(|task| task.id == "T1")
            .expect("T1's row");
        assert_eq!(
            row.derived.status,
            DerivedStatusDto::NeedsYou,
            "{:?}",
            row.derived
        );
        assert_eq!(row.derived.need, Some(NeedKindDto::Attention));
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
