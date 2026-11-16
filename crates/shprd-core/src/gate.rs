//! Gate decisions: what a review's findings mean, and what a failed review does
//! next.
//!
//! The reviewer only *describes* findings; the decision is made here, by pure
//! functions over the finding list and the folded [`Task`]. Nothing here reads
//! the log, the clock or the environment, so a verdict is reproducible.

use crate::config::Review;
use crate::event::{EventKind, Finding, ReviewSource, Severity};
use crate::hash;
use crate::rules::INTEGRATE_AFTER_FAILED_GATE;
use crate::state::{GateResult, RunState, Task, TaskStatus};

/// The gate name review results are recorded under.
pub const REVIEW_GATE: &str = "review";

/// What a human is asked to decide when a review escalates.
///
/// It is the person's own call because each answer is a human's verb: another
/// attempt is an `agent spawn`, taking the work as it is is an `override` of
/// the review gate, and dropping it is a `drop`.
pub const ESCALATION_ACTION: &str = "retry with another round, take it as it is, or drop it";

/// How many finding titles a card repeats before it stops naming the rest.
const CARD_FINDINGS: usize = 3;

/// What one review's findings mean for an attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    /// True when no finding blocks integration.
    pub passed: bool,
    /// The blocking findings, in the order the reviewer reported them.
    pub blocking: Vec<Finding>,
}

/// How bad `severity` is: critical is worst, trivial is least.
fn rank(severity: Severity) -> u8 {
    match severity {
        Severity::Critical => 3,
        Severity::Major => 2,
        Severity::Minor => 1,
        Severity::Trivial => 0,
    }
}

/// Decides a review from its findings.
///
/// A finding blocks when its severity is at least as bad as `must_fix`; every
/// other finding is advice. No findings is always a pass.
pub fn verdict(findings: &[Finding], must_fix: Severity) -> Verdict {
    let blocking: Vec<Finding> = findings
        .iter()
        .filter(|finding| rank(finding.severity) >= rank(must_fix))
        .cloned()
        .collect();
    Verdict {
        passed: blocking.is_empty(),
        blocking,
    }
}

/// How many attempts failed the `review` gate.
///
/// An attempt counts once, however many review gates it recorded.
pub fn failed_review_rounds(task: &Task) -> u32 {
    let rounds = task
        .attempts
        .iter()
        .filter(|attempt| {
            attempt
                .gates
                .iter()
                .any(|gate| gate.gate == REVIEW_GATE && !gate.passed)
        })
        .count();
    u32::try_from(rounds).unwrap_or(u32::MAX)
}

/// The newest review gate `task`'s newest attempt recorded, when it failed.
///
/// A review that passed is not a failure the run has to answer, and an attempt
/// whose last review passed has nothing left to send back.
pub fn latest_failed_review(task: &Task) -> Option<&GateResult> {
    let attempt = task.latest_attempt()?;
    let gate = attempt
        .gates
        .iter()
        .rev()
        .find(|gate| gate.gate == REVIEW_GATE)?;
    (!gate.passed).then_some(gate)
}

/// What happens after a review round fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    /// Send the task back to an agent for another attempt.
    SendBack,
    /// Stop retrying and ask a human.
    Escalate,
}

/// Decides the next step after a failed review, given the allowed `rounds`.
///
/// The task escalates once its failed review rounds reach the allowance, so
/// `rounds` is the last round that still sends work back.
pub fn after_failure(task: &Task, rounds: u32) -> Next {
    if failed_review_rounds(task) >= rounds {
        Next::Escalate
    } else {
        Next::SendBack
    }
}

/// The provenance a review verdict carries: who reviewed, and what it read.
///
/// The reviewer is the one the review dispatch recorded, never the actor that
/// decided the review: the engine decides, but the finding's author is the
/// reviewer. The hash pins the exact bytes folded into the gate result, so a
/// reader can tell whether the packet still says what the verdict decided.
///
/// `None` means the attempt has no review request to answer — a verdict that
/// names no reviewer is not a review's.
pub fn provenance(task: &Task, attempt: u32, bytes: &[u8]) -> Option<ReviewSource> {
    let current = task.attempts.iter().find(|current| current.n == attempt)?;
    let request = current.reviews.last()?;
    Some(ReviewSource {
        // The request's own seq travels with the verdict, so a request recorded
        // after this read supersedes it instead of being answered by it.
        request_seq: request.seq,
        reviewer: request.reviewer.clone(),
        findings_sha256: hash::sha256_hex(bytes),
    })
}

/// The choices the card for a review that spent its rounds offers.
///
/// They are the person's own: another attempt is an `agent spawn`, taking the
/// work as it is is an `override` of the review gate, and dropping it is a
/// `drop`. The first is the one the engine can carry out itself once it is
/// answered.
pub const ROUNDS_SPENT_OPTIONS: [&str; 3] =
    ["retry with another round", "take it as it is", "drop it"];

/// The choice that spends one more review round.
pub const RETRY_CHOICE: &str = ROUNDS_SPENT_OPTIONS[0];

/// What every ask id that offers such a card's choices begins with.
pub const ROUNDS_SPENT_ASK_PREFIX: &str = "review-out-";

/// The id of the ask that offers a spent review's choices for `attempt`.
///
/// It names the attempt, so one failure has one card and the attempt after it
/// is a new one.
pub fn rounds_spent_ask(attempt: u32) -> String {
    format!("{ROUNDS_SPENT_ASK_PREFIX}{attempt}")
}

/// The words every escalation about a review that spent its rounds begins with.
///
/// It is the marker that makes one card one card: the verdict step, the planner
/// and the watchdog all write it, and every one of them compares it before
/// writing, so one failure is told once. It names the attempt, so a second
/// failure after another round is a new card rather than a repeat.
pub fn rounds_spent_prefix(task: &str, attempt: u32, rounds: u32) -> String {
    format!("{task}: its review of attempt {attempt} failed {rounds} time(s) and no round is left")
}

/// The attempt a card's body names, when `body` is one about `task`.
///
/// It is how a board tells a card that still holds from the history of a
/// failure a newer attempt has answered. Every card about an attempt names it,
/// so one rule covers a review that spent its rounds, a send-back that did not
/// start, and a step that keeps failing.
pub fn card_attempt(task: &str, body: &str) -> Option<u32> {
    let named = |prefix: &str, tail: &str| -> Option<u32> {
        let rest = body.strip_prefix(prefix)?;
        let (digits, rest) = rest.split_once(' ')?;
        let attempt: u32 = digits.parse().ok()?;
        rest.starts_with(tail).then_some(attempt)
    };
    if !body.starts_with(task) {
        return None;
    }
    named(&format!("{task}: its review of attempt "), "failed ")
        .or_else(|| named(&format!("{task}: sending attempt "), "back did not start: "))
        // A step that keeps failing names its attempt in parentheses, after the
        // words that say what failed: `… 3 passes in a row (attempt 2): …`.
        .or_else(|| tagged_attempt(body))
}

/// The attempt a body marks with `(attempt N)`, when it does.
fn tagged_attempt(body: &str) -> Option<u32> {
    let (_, rest) = body.split_once("(attempt ")?;
    let (digits, rest) = rest.split_once(')')?;
    rest.starts_with(':').then(|| digits.parse().ok())?
}

/// The card a review that spent its rounds puts in front of a person.
///
/// It says what failed and what the person can do about it, so a board can draw
/// it without knowing anything about reviews, and names the verb behind each
/// choice so the card answers itself.
pub fn rounds_spent_body(task: &Task, attempt: u32, rounds: u32) -> String {
    let found = findings_line(task);
    let id = &task.id;
    format!(
        "{}: {found}; answer the card `{}`: retry with another round, take it as it is, or drop \
         it (by hand: `shprd agent spawn --task {id}`, `shprd override --task {id} --rule \
         integrate_after_failed_gate --reason …` then `shprd land {id}`, or `shprd drop {id} \
         --reason …`)",
        rounds_spent_prefix(id, attempt, rounds),
        rounds_spent_ask(attempt),
    )
}

/// The findings a card repeats, as one phrase, taken from the newest failure.
fn findings_line(task: &Task) -> String {
    latest_failed_review(task)
        .map(|gate| {
            gate.findings
                .iter()
                .take(CARD_FINDINGS)
                .map(|finding| finding.title.trim())
                .filter(|title| !title.is_empty())
                .collect::<Vec<_>>()
                .join("; ")
        })
        .filter(|found| !found.is_empty())
        .unwrap_or_else(|| String::from("the reviewer wrote no finding"))
}

/// The card a spent review puts in front of a person once they answered it.
///
/// The ask that offered the choices has been consumed -- answering it is what
/// takes it out of `/api/asks` -- so a card that kept pointing at it would draw
/// buttons every click of which is refused. The answer is the person's
/// decision; what is left is the verb it names, so the card says that verb and
/// nothing else. It keeps the review's own prefix, so a board still reads it as
/// the same card about the same attempt.
///
/// `answer` is the answer as the log recorded it, so `drop it` reads as *you
/// answered `drop it`: run `shprd drop T1 --reason …`*.
pub fn answered_rounds_body(task: &Task, attempt: u32, rounds: u32, answer: &str) -> String {
    let id = &task.id;
    let found = findings_line(task);
    let choice = answer.trim();
    let lower = choice.to_ascii_lowercase();
    let verb = if lower.starts_with("drop") {
        format!("`shprd drop {id} --reason …`")
    } else if lower.starts_with("take") {
        format!(
            "`shprd override --task {id} --rule integrate_after_failed_gate --reason …` then \
             `shprd land {id}`"
        )
    } else {
        // An answer in the person's own words names no choice, so the card
        // names every by-hand verb instead of guessing which one they meant.
        format!(
            "`shprd agent spawn --task {id}`, or `shprd override --task {id} --rule \
             integrate_after_failed_gate --reason …` then `shprd land {id}`, or `shprd drop {id} \
             --reason …`"
        )
    };
    format!(
        "{}: {found}; you answered `{choice}`, so run {verb}",
        rounds_spent_prefix(id, attempt, rounds),
    )
}

/// Whether a card's answer asks for one more review round.
///
/// The card's first choice is "retry with another round", and a person who types
/// "retry" or the whole choice is asking for exactly that. Every other answer is
/// a decision the run records and the person carries out with their own verb.
pub fn wants_another_round(answer: &str) -> bool {
    answer.trim().to_ascii_lowercase().starts_with("retry")
}

/// The answer a spent review's card was given, when a person answered it.
///
/// The card's ask id names its attempt, so one failure has one answer and a
/// later failure after another round reads its own ask.
pub fn rounds_spent_answer(state: &RunState, task: &Task, attempt: u32) -> Option<String> {
    let ask = state
        .asks
        .get(&(task.id.clone(), rounds_spent_ask(attempt)))?;
    ask.answer.as_ref().map(|answer| answer.text.clone())
}

/// Whether a spent review's answer leaves the person a verb of their own.
///
/// `drop it` and `take it as it is` are the two choices the run cannot carry
/// out: another attempt is the engine's work, but dropping and accepting are a
/// person's verbs. So an answered card whose answer is one of those keeps
/// needing the person until that verb's record is in the log.
pub fn answer_leaves_a_verb(answer: &str) -> bool {
    let choice = answer.trim().to_ascii_lowercase();
    choice.starts_with("drop") || choice.starts_with("take")
}

/// Whether the verb a spent review's answer named has been run for `task`.
///
/// `drop it` leaves a `drop`, which the fold records as the task's `Dropped`
/// status; `take it as it is` leaves an `integrate_after_failed_gate` override
/// said after the gate that failed, which is the rule that accepts one. An
/// answer in the person's own words leaves every by-hand verb, so any of them
/// having run settles it: the card's job was to name what was left, and a
/// person who did something else with the task has answered it.
pub fn rounds_spent_verb_ran(task: &Task, answer: &str) -> bool {
    let lower = answer.trim().to_ascii_lowercase();
    if lower.starts_with("drop") {
        return task.status == TaskStatus::Dropped;
    }
    let accepted = !task.overrides.is_empty()
        && task.overrides.iter().any(|record| {
            record.rule == INTEGRATE_AFTER_FAILED_GATE
                && latest_failed_review(task).is_none_or(|gate| record.seq > gate.seq)
        });
    if lower.starts_with("take") {
        return accepted;
    }
    accepted || task.status == TaskStatus::Dropped
}

/// How many review rounds a person granted `task` by answering its cards.
///
/// Each answered "retry" is one more round the task may spend, so the allowance
/// a review runs under is the configured one plus this. It is read off the fold,
/// so a granted round survives a restart exactly as the setting does.
pub fn granted_rounds(state: &RunState, task: &Task) -> u32 {
    let granted = state
        .asks
        .values()
        .filter(|ask| ask.task == task.id && ask.id.starts_with(ROUNDS_SPENT_ASK_PREFIX))
        .filter(|ask| {
            ask.answer
                .as_ref()
                .is_some_and(|answer| wants_another_round(&answer.text))
        })
        .count();
    u32::try_from(granted).unwrap_or(u32::MAX)
}

/// The review rounds `task` may spend: the configured allowance, plus one for
/// every round a person granted by answering its card.
pub fn allowance(state: &RunState, task: &Task, configured: u32) -> u32 {
    configured.saturating_add(granted_rounds(state, task))
}

/// The words every escalation about a send-back that did not start begins with.
///
/// It names the attempt, so a second failure of the same attempt is one card and
/// the attempt after it is a new one.
pub fn send_back_failure_prefix(task: &str, attempt: u32) -> String {
    format!("{task}: sending attempt {attempt} back did not start: ")
}

/// What recording a review's verdict comes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    /// True when no finding blocked integration.
    pub passed: bool,
    /// How many findings blocked, in the order the reviewer reported them.
    pub blocking: usize,
    /// The events to record, in the order they must be appended: the gate
    /// result, then — when the failed round was the last one allowed — the ask
    /// for a human.
    pub events: Vec<EventKind>,
}

/// Turns a review's findings into the events the run records for them.
///
/// This is the review's whole decision, so every actor that decides one — the
/// `agent review-verdict` verb, the watchdog and the service's tick — records
/// the same events for the same file. `task` is the attempt's task as the caller
/// read it, before any of these events were appended.
pub fn decide(
    task: &Task,
    attempt: u32,
    findings: Vec<Finding>,
    source: ReviewSource,
    policy: Review,
) -> Decision {
    let decided = verdict(&findings, policy.must_fix);
    if decided.passed {
        // A pass keeps every finding it was given: minor advice is still worth
        // printing in the pull request, so an empty list would lose it.
        return Decision {
            passed: true,
            blocking: 0,
            events: vec![EventKind::GatePassed {
                task: task.id.clone(),
                attempt,
                gate: REVIEW_GATE.to_string(),
                findings,
                source: Some(source),
            }],
        };
    }
    let blocking = decided.blocking.len();
    let mut events = vec![EventKind::GateFailed {
        task: task.id.clone(),
        attempt,
        gate: REVIEW_GATE.to_string(),
        findings: findings.clone(),
        source: Some(source.clone()),
    }];
    // What happens next is read off the task as this failure leaves it, which is
    // why the failed gate is folded in here rather than left to the caller.
    let mut after = task.clone();
    if let Some(current) = after
        .attempts
        .iter_mut()
        .find(|current| current.n == attempt)
    {
        current.gates.push(GateResult {
            gate: REVIEW_GATE.to_string(),
            passed: false,
            findings,
            source: Some(source),
            seq: 0,
        });
    }
    if after_failure(&after, policy.rounds) == Next::Escalate {
        events.push(EventKind::Escalated {
            task: Some(task.id.clone()),
            body: rounds_spent_body(&after, attempt, failed_review_rounds(&after)),
            default_action: ESCALATION_ACTION.to_string(),
        });
    }
    Decision {
        passed: false,
        blocking,
        events,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{
        ESCALATION_ACTION, Next, REVIEW_GATE, ROUNDS_SPENT_OPTIONS, after_failure,
        answer_leaves_a_verb, answered_rounds_body, card_attempt, decide, failed_review_rounds,
        latest_failed_review, provenance, rounds_spent_answer, rounds_spent_ask, rounds_spent_body,
        rounds_spent_prefix, rounds_spent_verb_ran, send_back_failure_prefix, verdict,
    };
    use crate::config::Review;
    use crate::event::{EventKind, Finding, ReviewSource, Severity};
    use crate::rules::INTEGRATE_AFTER_FAILED_GATE;
    use crate::state::{Attempt, GateResult, RunState, Task, TaskStatus};

    /// A finding of a given severity with placeholder text.
    fn finding(severity: Severity) -> Finding {
        Finding {
            severity,
            title: String::from("a finding"),
            location: None,
            detail: String::from("do something about it"),
        }
    }

    /// One gate result for the named gate.
    fn gate(name: &str, passed: bool) -> GateResult {
        GateResult {
            gate: String::from(name),
            passed,
            findings: Vec::new(),
            source: None,
            seq: 1,
        }
    }

    /// An attempt with the given gates.
    fn attempt(n: u32, gates: Vec<GateResult>) -> Attempt {
        Attempt {
            n,
            agent: String::from("w1"),
            model: None,
            worktree: String::from("/tmp/wt"),
            branch: String::from("feature/x"),
            report: None,
            gates,
            reviews: Vec::new(),
            spawn_seq: None,
            stopped: false,
        }
    }

    /// A task with the given attempts, as a failed review leaves it.
    fn task(attempts: Vec<Attempt>) -> Task {
        Task {
            id: String::from("T1"),
            title: String::from("a task"),
            scope: Vec::new(),
            acceptance: Vec::new(),
            tags: Vec::new(),
            skills: Vec::new(),
            deps: Vec::new(),
            status: TaskStatus::Failed,
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

    /// Every severity, worst first.
    const ALL: [Severity; 4] = [
        Severity::Critical,
        Severity::Major,
        Severity::Minor,
        Severity::Trivial,
    ];

    #[test]
    fn severity_at_or_above_must_fix_blocks() {
        for (found, severity) in ALL.iter().enumerate() {
            for (floor, must_fix) in ALL.iter().enumerate() {
                let blocks = found <= floor;
                let decided = verdict(&[finding(*severity)], *must_fix);
                assert_eq!(
                    decided.passed, !blocks,
                    "{severity:?} against {must_fix:?} should block == {blocks}"
                );
                assert_eq!(decided.blocking.len(), usize::from(blocks));
            }
        }
    }

    #[test]
    fn no_findings_pass_even_at_the_strictest_setting() {
        let decided = verdict(&[], Severity::Trivial);
        assert!(decided.passed);
        assert!(decided.blocking.is_empty());
    }

    #[test]
    fn only_blocking_findings_are_reported_in_order() {
        let findings = [
            finding(Severity::Minor),
            finding(Severity::Critical),
            finding(Severity::Major),
        ];
        let decided = verdict(&findings, Severity::Major);
        assert!(!decided.passed);
        let kept: Vec<Severity> = decided
            .blocking
            .iter()
            .map(|finding| finding.severity)
            .collect();
        assert_eq!(kept, vec![Severity::Critical, Severity::Major]);
    }

    #[test]
    fn failed_review_rounds_counts_attempts_not_gates() {
        let task = task(vec![
            attempt(1, vec![gate(REVIEW_GATE, false), gate(REVIEW_GATE, false)]),
            attempt(2, vec![gate(REVIEW_GATE, false)]),
            attempt(3, vec![gate(REVIEW_GATE, true)]),
        ]);
        assert_eq!(failed_review_rounds(&task), 2);
    }

    #[test]
    fn non_review_gates_do_not_count_as_rounds() {
        let task = task(vec![attempt(
            1,
            vec![gate("lint", false), gate(REVIEW_GATE, true)],
        )]);
        assert_eq!(failed_review_rounds(&task), 0);
    }

    #[test]
    fn rounds_send_back_until_the_allowance_is_spent() {
        let one = task(vec![attempt(1, vec![gate(REVIEW_GATE, false)])]);
        assert_eq!(after_failure(&one, 2), Next::SendBack);
        assert_eq!(after_failure(&one, 1), Next::Escalate);
        let two = task(vec![
            attempt(1, vec![gate(REVIEW_GATE, false)]),
            attempt(2, vec![gate(REVIEW_GATE, false)]),
        ]);
        assert_eq!(after_failure(&two, 2), Next::Escalate);
        assert_eq!(after_failure(&two, 3), Next::SendBack);
        let none = task(Vec::new());
        assert_eq!(after_failure(&none, 1), Next::SendBack);
        assert_eq!(after_failure(&none, 0), Next::Escalate);
    }

    /// A review source naming `request_seq` and the bytes it decided.
    fn source(request_seq: u64, bytes: &[u8]) -> ReviewSource {
        ReviewSource {
            reviewer: String::from("reviewer"),
            findings_sha256: crate::hash::sha256_hex(bytes),
            request_seq,
        }
    }

    /// A task whose one attempt was reported and reviewed, waiting on a verdict.
    fn reviewed_task() -> Task {
        let mut attempt = attempt(1, Vec::new());
        attempt.reviews.push(crate::state::ReviewRequest {
            reviewer: String::from("reviewer"),
            degraded: false,
            seq: 7,
        });
        let mut reviewed = task(vec![attempt]);
        reviewed.status = TaskStatus::Reported;
        reviewed
    }

    /// The policy the tests decide under: a major finding blocks, two rounds.
    fn policy() -> Review {
        Review {
            must_fix: Severity::Major,
            rounds: 2,
        }
    }

    #[test]
    fn a_clean_review_decides_one_passing_gate() {
        let reviewed = reviewed_task();
        let decided = decide(&reviewed, 1, Vec::new(), source(7, b"[]"), policy());
        assert!(decided.passed);
        assert_eq!(decided.blocking, 0);
        assert_eq!(decided.events.len(), 1, "{:?}", decided.events);
        match &decided.events[0] {
            EventKind::GatePassed {
                task,
                attempt,
                gate,
                findings,
                source,
            } => {
                assert_eq!(task, "T1");
                assert_eq!(*attempt, 1);
                assert_eq!(gate, REVIEW_GATE);
                assert!(findings.is_empty());
                assert_eq!(source.as_ref().map(|source| source.request_seq), Some(7));
            }
            other => panic!("a pass is a passed gate, got {other:?}"),
        }
    }

    #[test]
    fn a_blocking_review_fails_and_sends_the_task_back() {
        let reviewed = reviewed_task();
        let decided = decide(
            &reviewed,
            1,
            vec![finding(Severity::Critical)],
            source(7, b"[{}]"),
            policy(),
        );
        assert!(!decided.passed);
        assert_eq!(decided.blocking, 1);
        assert_eq!(decided.events.len(), 1, "one round is still a send-back");
        assert!(matches!(decided.events[0], EventKind::GateFailed { .. }));
    }

    #[test]
    fn the_last_allowed_round_asks_a_human_too() {
        // An attempt that already spent one of the two rounds: this failure is
        // the second, so a person is asked in the same decision.
        let mut reviewed = reviewed_task();
        reviewed
            .attempts
            .insert(0, attempt(1, vec![gate(REVIEW_GATE, false)]));
        reviewed.attempts.last_mut().expect("an attempt").n = 2;
        let decided = decide(
            &reviewed,
            2,
            vec![finding(Severity::Critical)],
            source(7, b"[{}]"),
            policy(),
        );
        assert!(!decided.passed);
        assert_eq!(decided.events.len(), 2, "{:?}", decided.events);
        match &decided.events[1] {
            EventKind::Escalated {
                task,
                body,
                default_action,
            } => {
                assert_eq!(task.as_deref(), Some("T1"));
                let prefix = rounds_spent_prefix("T1", 2, 2);
                assert!(body.starts_with(&prefix), "{body}");
                assert!(body.contains("a finding"), "it names what failed: {body}");
                assert!(body.contains("retry with another round"), "{body}");
                assert!(body.contains("shprd agent spawn --task T1"), "{body}");
                assert!(body.contains("shprd drop T1"), "{body}");
                assert_eq!(default_action, ESCALATION_ACTION);
            }
            other => panic!("the last round asks for a human, got {other:?}"),
        }
    }

    #[test]
    fn a_card_names_the_findings_it_can_and_stops() {
        // A reviewer that wrote four findings gets three named, so one verbose
        // review cannot spill the whole board.
        let mut failed = attempt(1, Vec::new());
        failed.gates.push(GateResult {
            gate: String::from(REVIEW_GATE),
            passed: false,
            findings: (1..=4)
                .map(|n| Finding {
                    severity: Severity::Major,
                    title: format!("finding {n}"),
                    location: None,
                    detail: String::new(),
                })
                .collect(),
            source: None,
            seq: 5,
        });
        let body = rounds_spent_body(&task(vec![failed]), 1, 2);
        assert!(body.contains("finding 1; finding 2; finding 3"), "{body}");
        assert!(!body.contains("finding 4"), "{body}");
    }

    #[test]
    fn a_passing_review_has_no_failure_to_answer() {
        let passed = attempt(1, vec![gate(REVIEW_GATE, true)]);
        assert!(latest_failed_review(&task(vec![passed])).is_none());
        let failed = attempt(1, vec![gate(REVIEW_GATE, false)]);
        assert!(latest_failed_review(&task(vec![failed])).is_some());
    }

    #[test]
    fn a_card_names_the_attempt_every_kind_of_card_is_about() {
        // The staleness rule a board reads is one rule for every card about an
        // attempt: a review that spent its rounds, and a send-back that did not
        // start.
        let spent = rounds_spent_prefix("T1", 3, 2);
        assert_eq!(card_attempt("T1", &spent), Some(3));
        let failed = send_back_failure_prefix("T1", 4);
        assert_eq!(card_attempt("T1", &failed), Some(4));
        // A step that keeps failing names its attempt in parentheses: a card
        // about one attempt leaves the board once the task is on another.
        let repeated = "T1: its `start` step has failed the same way 3 passes in a row (attempt 2): \
                        task \"T1\" path \"calc.py\" overlaps task \"T0\"";
        assert_eq!(card_attempt("T1", repeated), Some(2));
        // Anything else is not a card about an attempt, and a card for another
        // task is not this task's.
        assert_eq!(card_attempt("T1", "T1 is ready to land"), None);
        assert_eq!(card_attempt("T2", &spent), None);
        assert_eq!(
            card_attempt("T1", "T1: sending attempt x back did not start: "),
            None
        );
        // A body that is not this task's at all is never its card.
        assert_eq!(
            card_attempt("T1", "T2: its `start` step failed (attempt 1): x"),
            None
        );
    }

    #[test]
    fn provenance_pins_the_review_the_verdict_answers() {
        let reviewed = reviewed_task();
        let pinned = provenance(&reviewed, 1, b"[]").expect("the request is there");
        assert_eq!(pinned.request_seq, 7);
        assert_eq!(pinned.reviewer, "reviewer");
        assert_eq!(pinned.findings_sha256, crate::hash::sha256_hex(b"[]"));
        assert!(
            provenance(&reviewed, 2, b"[]").is_none(),
            "an attempt nobody reviewed has no provenance"
        );
    }

    /// A task whose one attempt spent its rounds, as the failure leaves it.
    fn spent(task: &str, attempt: u32) -> Task {
        let mut failed = attempt_of(attempt, vec![gate(REVIEW_GATE, false)]);
        failed.gates[0].findings = vec![Finding {
            severity: Severity::Major,
            title: String::from("No evidence that test_calc.py still passes"),
            location: None,
            detail: String::new(),
        }];
        let mut found = task_of(task, vec![failed]);
        found.status = TaskStatus::Failed;
        found
    }

    /// One attempt of a task, with the given gates.
    fn attempt_of(n: u32, gates: Vec<GateResult>) -> Attempt {
        attempt(n, gates)
    }

    /// A task under the given id with the given attempts.
    fn task_of(id: &str, attempts: Vec<Attempt>) -> Task {
        Task {
            id: id.to_string(),
            ..task(attempts)
        }
    }

    /// A run whose ask `id` of `task` was answered with `answer`.
    fn answered_state(task: &str, id: &str, answer: Option<&str>) -> RunState {
        let mut state = RunState::default();
        state.asks.insert(
            (task.to_string(), id.to_string()),
            crate::state::AskRecord {
                id: id.to_string(),
                task: task.to_string(),
                kind: crate::ask::AskKind::Question,
                text: String::from("what now"),
                options: ROUNDS_SPENT_OPTIONS
                    .iter()
                    .map(|choice| (*choice).to_string())
                    .collect(),
                created_at_ms: 1,
                expires_at_ms: None,
                raised_seq: 1,
                answer: answer.map(|text| crate::state::AskAnswerRecord {
                    text: text.to_string(),
                    seq: 2,
                }),
            },
        );
        state
    }

    #[test]
    fn an_answered_card_names_the_verb_the_answer_leaves() {
        // Bug 1 of gate run #4: a card answered with `drop it` kept pointing at
        // an ask that had been consumed, so every click on it was refused. Once
        // the answer is known the card stops offering choices and says the verb
        // that is left.
        let found = spent("T1", 2);
        let drop = answered_rounds_body(&found, 2, 2, "drop it");
        assert!(drop.starts_with(&rounds_spent_prefix("T1", 2, 2)), "{drop}");
        assert!(!drop.contains("answer the card"), "{drop}");
        assert!(drop.contains("you answered `drop it`"), "{drop}");
        assert!(drop.contains("`shprd drop T1 --reason …`"), "{drop}");
        assert!(
            !drop.contains("`shprd agent spawn --task T1`"),
            "one verb, not every one: {drop}"
        );
        // It names the failure it is about, exactly as the offered card did.
        assert!(
            drop.contains("No evidence that test_calc.py still passes"),
            "{drop}"
        );

        let take = answered_rounds_body(&found, 2, 2, "take it as it is");
        assert!(take.contains("you answered `take it as it is`"), "{take}");
        assert!(
            take.contains("`shprd override --task T1 --rule integrate_after_failed_gate"),
            "{take}"
        );
        assert!(take.contains("`shprd land T1`"), "{take}");
    }

    #[test]
    fn an_answer_in_the_persons_own_words_names_every_by_hand_verb() {
        let found = spent("T1", 1);
        let body = answered_rounds_body(&found, 1, 1, "I will look at it tomorrow");
        assert!(
            body.contains("you answered `I will look at it tomorrow`"),
            "{body}"
        );
        assert!(body.contains("`shprd agent spawn --task T1`"), "{body}");
        assert!(body.contains("`shprd drop T1 --reason …`"), "{body}");
    }

    #[test]
    fn only_drop_and_take_leave_the_person_a_verb() {
        assert!(answer_leaves_a_verb("drop it"));
        assert!(answer_leaves_a_verb("  Take it as it is "));
        // A retry is the engine's own work, so the card stands down at once.
        assert!(!answer_leaves_a_verb("retry with another round"));
        assert!(!answer_leaves_a_verb("retry"));
    }

    #[test]
    fn the_answer_a_retry_card_was_given_is_read_off_the_fold() {
        let found = spent("T1", 2);
        let id = rounds_spent_ask(2);
        let open = answered_state("T1", &id, None);
        assert_eq!(
            rounds_spent_answer(&open, &found, 2),
            None,
            "nobody answered"
        );
        let answered = answered_state("T1", &id, Some("drop it"));
        assert_eq!(
            rounds_spent_answer(&answered, &found, 2).as_deref(),
            Some("drop it")
        );
        // The ask names its attempt, so another failure reads its own answer.
        assert_eq!(rounds_spent_answer(&answered, &found, 1), None);
    }

    #[test]
    fn the_card_a_person_answered_stands_down_once_its_verb_ran() {
        let mut found = spent("T1", 1);
        assert!(
            !rounds_spent_verb_ran(&found, "drop it"),
            "nothing was run yet"
        );
        found.status = TaskStatus::Dropped;
        assert!(rounds_spent_verb_ran(&found, "drop it"));

        let mut found = spent("T1", 1);
        assert!(!rounds_spent_verb_ran(&found, "take it as it is"));
        found.overrides.push(crate::state::OverrideRecord {
            rule: String::from(INTEGRATE_AFTER_FAILED_GATE),
            reason: String::from("the failure is cosmetic"),
            seq: 9,
        });
        assert!(rounds_spent_verb_ran(&found, "take it as it is"));
        // A drop settles any answer, including one in the person's own words.
        let mut dropped = spent("T1", 1);
        dropped.status = TaskStatus::Dropped;
        assert!(rounds_spent_verb_ran(
            &dropped,
            "I will look at it tomorrow"
        ));
    }
}
