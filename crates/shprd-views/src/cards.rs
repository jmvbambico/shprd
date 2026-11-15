//! The needs-you card an escalation draws, and whether it still holds.
//!
//! An escalation is a log line: it records what needs deciding at the moment it
//! was raised. What a board draws is that line read against everything the log
//! has recorded since, so a card a person already answered is drawn as the
//! answer left it, and a card about work that has since landed or been dropped
//! is not drawn at all.
//!
//! It is one projector for two readers — `/api/state`'s `needs` list
//! ([`crate::status`]'s caller in the service) and the derived row's reason
//! ([`crate::derived`]) — so the board's card and `shprd status` cannot disagree
//! about whether a person is needed.

use shprd_core::gate;
use shprd_core::state::{Escalation, RunState, TaskStatus};

/// One card as a board draws it: what it says, and what happens if nobody
/// answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Card {
    /// What needs deciding, as the reader should see it now.
    pub body: String,
    /// The action taken if nobody answers.
    pub default_action: String,
}

/// The card `escalation` draws against `state`, or `None` when it is history.
///
/// A card is history when nothing is left for the person to decide:
///
/// - its task landed or was dropped, so the work it held back is decided;
/// - a newer attempt answered it, which is how a "retry" a person gave — or a
///   spawn someone ran by hand — retires a card about a failed review;
/// - it offered a spent review's choices and those were answered with a verb
///   the person then ran (see [`answered`]).
///
/// A card about no single task (a run's own) is always kept: the fold has
/// nothing to resolve it against.
pub fn card(state: &RunState, escalation: &Escalation) -> Option<Card> {
    let Some(id) = escalation.task.as_deref() else {
        return Some(Card {
            body: escalation.body.clone(),
            default_action: escalation.default_action.clone(),
        });
    };
    let Some(task) = state.tasks.get(id) else {
        // A card about a task the log no longer holds is the log's to keep: it
        // was raised about something, and dropping it would lose the record.
        return Some(Card {
            body: escalation.body.clone(),
            default_action: escalation.default_action.clone(),
        });
    };
    if matches!(task.status, TaskStatus::Integrated | TaskStatus::Dropped) {
        return None;
    }
    // A card about an attempt names it, and a newer attempt is the answer to the
    // failure it named.
    if let Some(attempt) = gate::card_attempt(id, &escalation.body)
        && task
            .latest_attempt()
            .is_some_and(|latest| latest.n > attempt)
    {
        return None;
    }
    if let Some(attempt) = spent_review_attempt(id, &escalation.body)
        && let Some(answer) = gate::rounds_spent_answer(state, task, attempt)
    {
        return answered(task, attempt, escalation, &answer);
    }
    Some(Card {
        body: escalation.body.clone(),
        default_action: escalation.default_action.clone(),
    })
}

/// The attempt a spent review's card names, when `body` is one of those.
///
/// A send-back that did not start is a card too, but it offers no choices and
/// has no ask behind it, so the answer rule never applies to it.
fn spent_review_attempt(task: &str, body: &str) -> Option<u32> {
    let attempt = gate::card_attempt(task, body)?;
    let marker = format!("{task}: its review of attempt {attempt} failed ");
    body.starts_with(&marker).then_some(attempt)
}

/// What an answered spent-review card draws now, or `None` once its verb ran.
///
/// The ask that offered the choices has been consumed by the answer, so a card
/// that kept pointing at it would draw buttons every click of which is refused.
/// Two answers leave the engine's own work:
///
/// - "retry with another round" is a round the engine spends by itself, so the
///   card stands down at once and the new attempt takes over;
/// - any other answer is a decision the run records and the person carries out
///   with a verb of their own, so the card keeps needing them — with the verb
///   named — until that verb's record is in the log.
fn answered(
    task: &shprd_core::state::Task,
    attempt: u32,
    escalation: &Escalation,
    answer: &str,
) -> Option<Card> {
    if !gate::answer_leaves_a_verb(answer) {
        return None;
    }
    // The verb the card names is already in the log, so there is nothing left
    // for the person to do: dropping it is the task's `Dropped` status (read
    // above) and taking it as it is is the override.
    if gate::rounds_spent_verb_ran(task, answer) {
        return None;
    }
    Some(Card {
        body: gate::answered_rounds_body(task, attempt, gate::failed_review_rounds(task), answer),
        default_action: escalation.default_action.clone(),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use shprd_core::ask::AskKind;
    use shprd_core::event::{Finding, Severity};
    use shprd_core::gate;
    use shprd_core::state::{
        AskAnswerRecord, AskRecord, Attempt, Escalation, GateResult, RunState, Task, TaskStatus,
    };

    use super::card;

    /// One attempt that recorded a failed review gate.
    fn attempt(n: u32) -> Attempt {
        Attempt {
            n,
            agent: String::from("w1"),
            model: None,
            worktree: String::from("/wt/T1"),
            branch: String::from("shprd/r1/T1"),
            spawn_seq: Some(1),
            report: None,
            gates: vec![GateResult {
                gate: String::from(gate::REVIEW_GATE),
                passed: false,
                findings: vec![Finding {
                    severity: Severity::Major,
                    title: String::from("No evidence that test_calc.py still passes"),
                    location: None,
                    detail: String::new(),
                }],
                source: None,
                seq: 4,
            }],
            reviews: Vec::new(),
        }
    }

    /// A task holding `attempts`, in the given status.
    fn task(status: TaskStatus, attempts: Vec<Attempt>) -> Task {
        Task {
            id: String::from("T1"),
            title: String::from("a task"),
            scope: Vec::new(),
            acceptance: Vec::new(),
            tags: Vec::new(),
            skills: Vec::new(),
            deps: Vec::new(),
            status,
            attempts,
            overrides: Vec::new(),
            dropped_reason: None,
            pr: None,
            approved_head: None,
            last_status: None,
            nothing_to_merge: None,
            fallbacks: Vec::new(),
        }
    }

    /// A run holding `T1` and its spent-review card, as the failure leaves it.
    fn spent_run(answer: Option<&str>) -> (RunState, Escalation) {
        let body = gate::rounds_spent_body(&task(TaskStatus::Failed, vec![attempt(1)]), 1, 2);
        let mut state = RunState::default();
        state.tasks.insert(
            String::from("T1"),
            task(TaskStatus::Failed, vec![attempt(1)]),
        );
        state.asks.insert(
            (String::from("T1"), gate::rounds_spent_ask(1)),
            AskRecord {
                id: gate::rounds_spent_ask(1),
                task: String::from("T1"),
                kind: AskKind::Question,
                text: body.clone(),
                options: gate::ROUNDS_SPENT_OPTIONS
                    .iter()
                    .map(|choice| (*choice).to_string())
                    .collect(),
                created_at_ms: 1,
                expires_at_ms: None,
                raised_seq: 3,
                answer: answer.map(|text| AskAnswerRecord {
                    text: text.to_string(),
                    seq: 5,
                }),
            },
        );
        let escalation = Escalation {
            task: Some(String::from("T1")),
            body,
            default_action: String::from(gate::ESCALATION_ACTION),
            seq: 4,
        };
        state.escalations.push(escalation.clone());
        (state, escalation)
    }

    #[test]
    fn an_unanswered_card_is_drawn_as_raised() {
        let (state, escalation) = spent_run(None);
        let drawn = card(&state, &escalation).expect("the card still needs a person");
        assert_eq!(drawn.body, escalation.body);
        assert!(
            drawn.body.contains("answer the card `review-out-1`"),
            "{}",
            drawn.body
        );
    }

    #[test]
    fn a_card_answered_with_drop_it_names_the_drop_verb_and_not_the_ask() {
        // Bug 1 of gate run #4: the ask is consumed by the answer, so a card
        // that kept pointing at it drew buttons every click of which is refused.
        let (state, escalation) = spent_run(Some("drop it"));
        let drawn = card(&state, &escalation).expect("the person still has to drop it");
        assert!(!drawn.body.contains("answer the card"), "{}", drawn.body);
        assert!(
            drawn.body.contains("you answered `drop it`"),
            "{}",
            drawn.body
        );
        assert!(
            drawn.body.contains("`shprd drop T1 --reason …`"),
            "{}",
            drawn.body
        );
    }

    #[test]
    fn a_card_answered_with_drop_it_goes_once_the_drop_is_recorded() {
        let (mut state, escalation) = spent_run(Some("drop it"));
        // The `shprd drop` verb records a `TaskDropped`, which folds into this.
        state.tasks.get_mut("T1").expect("the task").status = TaskStatus::Dropped;
        assert!(
            card(&state, &escalation).is_none(),
            "the verb the card named has run"
        );
    }

    #[test]
    fn a_card_answered_with_take_it_as_it_is_goes_once_the_override_is_recorded() {
        let (mut state, escalation) = spent_run(Some("take it as it is"));
        let waiting = card(&state, &escalation).expect("the person still has to accept it");
        assert!(
            waiting
                .body
                .contains("`shprd override --task T1 --rule integrate_after_failed_gate"),
            "{}",
            waiting.body
        );
        state.tasks.get_mut("T1").expect("the task").overrides.push(
            shprd_core::state::OverrideRecord {
                rule: String::from(shprd_core::rules::INTEGRATE_AFTER_FAILED_GATE),
                reason: String::from("the failure is cosmetic"),
                seq: 6,
            },
        );
        assert!(card(&state, &escalation).is_none());
    }

    #[test]
    fn a_card_answered_with_retry_stands_down_at_once() {
        // A retry is the engine's own work: the round it buys starts another
        // attempt, which is what retires the card.
        let (state, escalation) = spent_run(Some("retry with another round"));
        assert!(card(&state, &escalation).is_none());
    }

    #[test]
    fn a_card_a_newer_attempt_answered_is_history() {
        let (mut state, escalation) = spent_run(None);
        state
            .tasks
            .get_mut("T1")
            .expect("the task")
            .attempts
            .push(Attempt { n: 2, ..attempt(2) });
        assert!(card(&state, &escalation).is_none());
    }

    #[test]
    fn a_card_about_work_already_in_is_history() {
        for status in [TaskStatus::Integrated, TaskStatus::Dropped] {
            let (mut state, escalation) = spent_run(None);
            state.tasks.get_mut("T1").expect("the task").status = status;
            assert!(card(&state, &escalation).is_none(), "{status:?}");
        }
    }

    #[test]
    fn a_run_wide_card_is_always_kept() {
        let mut state = RunState::default();
        let escalation = Escalation {
            task: None,
            body: String::from("the orchestrator is waiting on its own prompt"),
            default_action: String::from("answer it in its tab"),
            seq: 1,
        };
        let drawn = card(&state, &escalation).expect("a run's own card is never resolved");
        assert_eq!(drawn.body, escalation.body);
        // It stays even once the run is finished: nothing else could clear it.
        state.finished = true;
        assert!(card(&state, &escalation).is_some());
    }

    #[test]
    fn a_card_about_a_step_that_keeps_failing_leaves_once_the_task_moves_on() {
        // A repeated failure names the attempt it happened in, so the card is
        // about that attempt and not about the task for ever: a new attempt
        // after the person fixed the cause retires it.
        let mut state = RunState::default();
        state.tasks.insert(
            String::from("C2"),
            task(TaskStatus::Queued, vec![attempt(1)]),
        );
        let escalation = Escalation {
            task: Some(String::from("C2")),
            body: String::from(
                "C2: its `start` step has failed the same way 3 passes in a row (attempt 1): \
                 task \"C2\" path \"calc.py\" overlaps task \"C1\"",
            ),
            default_action: String::from("narrow C2's scope"),
            seq: 4,
        };
        assert!(card(&state, &escalation).is_some(), "the card still holds");

        state
            .tasks
            .get_mut("C2")
            .expect("the task")
            .attempts
            .push(Attempt { n: 2, ..attempt(2) });
        assert!(
            card(&state, &escalation).is_none(),
            "the attempt the card was about is behind the task"
        );
    }

    #[test]
    fn a_send_back_card_is_not_treated_as_a_spent_review() {
        // A send-back that did not start offers no choices and has no ask, so
        // the answer rule never applies to it: it leaves once a newer attempt
        // exists, and not before.
        let mut state = RunState::default();
        state.tasks.insert(
            String::from("T1"),
            task(TaskStatus::Failed, vec![attempt(1)]),
        );
        let escalation = Escalation {
            task: Some(String::from("T1")),
            body: format!(
                "{}no workspace \"shprd · repo\" yet",
                gate::send_back_failure_prefix("T1", 1)
            ),
            default_action: String::from("spawn it by hand"),
            seq: 9,
        };
        let drawn = card(&state, &escalation).expect("the card still holds");
        assert_eq!(drawn.body, escalation.body);
    }
}
