//! Gate decisions: what a review's findings mean, and what a failed review does
//! next.
//!
//! The reviewer only *describes* findings; the decision is made here, by pure
//! functions over the finding list and the folded [`Task`]. Nothing here reads
//! the log, the clock or the environment, so a verdict is reproducible.

use crate::config::Review;
use crate::event::{EventKind, Finding, ReviewSource, Severity};
use crate::hash;
use crate::state::{GateResult, Task};

/// The gate name review results are recorded under.
pub const REVIEW_GATE: &str = "review";

/// What a human is asked to decide when a review escalates.
pub const ESCALATION_ACTION: &str = "decide whether to accept, re-scope or drop";

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
            body: format!("review failed {} times", failed_review_rounds(&after)),
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
        ESCALATION_ACTION, Next, REVIEW_GATE, after_failure, decide, failed_review_rounds,
        provenance, verdict,
    };
    use crate::config::Review;
    use crate::event::{EventKind, Finding, ReviewSource, Severity};
    use crate::state::{Attempt, GateResult, Task, TaskStatus};

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
                assert_eq!(body, "review failed 2 times");
                assert_eq!(default_action, ESCALATION_ACTION);
            }
            other => panic!("the last round asks for a human, got {other:?}"),
        }
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
}
