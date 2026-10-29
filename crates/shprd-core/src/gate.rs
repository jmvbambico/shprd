//! Gate decisions: what a review's findings mean, and what a failed review does
//! next.
//!
//! The reviewer only *describes* findings; the decision is made here, by pure
//! functions over the finding list and the folded [`Task`]. Nothing here reads
//! the log, the clock or the environment, so a verdict is reproducible.

use crate::event::{Finding, Severity};
use crate::state::Task;

/// The gate name review results are recorded under.
pub const REVIEW_GATE: &str = "review";

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

#[cfg(test)]
mod tests {
    use super::{Next, REVIEW_GATE, after_failure, failed_review_rounds, verdict};
    use crate::event::{Finding, Severity};
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
}
