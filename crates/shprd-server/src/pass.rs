//! The last pass's outcome, kept where a person and a board can read it.
//!
//! The pass runs inside the service's process, but what it did has to outlive
//! one look: `shprd status` is another process, and `GET /api/state` may be
//! asked minutes after the pass ran. So every pass writes one small file into
//! shprd's state directory ([`LastPass::write`]), and both readers take it from
//! there ([`LastPass::read`]). A step that failed then shows on the board and in
//! the status line as well as in the service's own output.
//!
//! The file is a mirror, never the record: the log is what happened, so a write
//! that fails is a warning rather than a failed pass, and a decision never reads
//! this file.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::tick::{StepReport, TickReport};

/// The prefix of the record's file name in the state directory.
const PREFIX: &str = "last-tick-";

/// The record's file name for `repo`.
///
/// A state directory belongs to a home, and a home works on more than one
/// repository, so the record is filed per repository rather than in one shared
/// file. The name is a hash of the repository's path — short, and made of
/// characters a path need not be — and the record itself names its repository
/// too, so a reader can still check.
fn file_name(repo: &Path) -> String {
    // FNV-1a: a fixed, dependency-free hash. Nothing here needs to resist an
    // adversary, only to keep two checkouts' records apart.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in repo.display().to_string().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{PREFIX}{hash:016x}.json")
}

/// One step of the last pass that failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailedStep {
    /// The task the step was about; empty when it was the pass's own failure.
    pub task: String,
    /// What the step was: `start`, `review`, `verdict`, `land`, `send_back`, a
    /// ladder action, or `pass` when the pass could not read the run at all.
    pub step: String,
    /// Why it failed, one line.
    pub error: String,
}

impl FailedStep {
    /// The failed-step entry for one reported step.
    fn of(report: &StepReport) -> Self {
        Self {
            task: report.task.clone(),
            step: report.action.clone(),
            error: report.detail.clone(),
        }
    }
}

/// What the last pass did.
///
/// It is written after every pass, so it always describes the newest one — the
/// moment it finished, how many steps it ran, the run it advanced, and every
/// step that failed with the error the step was given.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastPass {
    /// The repository the pass advanced.
    ///
    /// A state directory belongs to a home, not to one repository, so this is
    /// what keeps a pass over another checkout from being read as this one's.
    pub repo: String,
    /// When the pass finished, ms since the Unix epoch.
    pub at_ms: u64,
    /// How many steps it ran, the failed ones included.
    pub steps: usize,
    /// The run it advanced, when the repository has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// Every step that failed, in plan order.
    pub failed: Vec<FailedStep>,
}

impl LastPass {
    /// The record of the pass that produced `report`.
    pub fn of(report: &TickReport, repo: &Path, at_ms: u64) -> Self {
        Self {
            repo: repo.display().to_string(),
            at_ms,
            steps: report.steps.len(),
            run: report.run.clone(),
            failed: report
                .steps
                .iter()
                .filter(|step| !step.ok)
                .map(FailedStep::of)
                .collect(),
        }
    }

    /// The record of a pass that could not even read the run.
    ///
    /// It is the one failure that ends a pass before any step runs, and it is
    /// exactly the one a person must hear about: nothing else will move.
    pub fn unreadable(repo: &Path, at_ms: u64, error: impl Into<String>) -> Self {
        Self {
            repo: repo.display().to_string(),
            at_ms,
            steps: 0,
            run: None,
            failed: vec![FailedStep {
                task: String::new(),
                step: String::from("pass"),
                error: error.into(),
            }],
        }
    }

    /// The newest pass recorded for `repo`, when there is one.
    ///
    /// A missing, unreadable or foreign record is `None`: this is a hint about
    /// the last pass, and no board or verb may fail because of it.
    pub fn read(state_dir: &Path, repo: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(state_dir.join(file_name(repo))).ok()?;
        let last: Self = serde_json::from_str(&text).ok()?;
        (Path::new(&last.repo) == repo).then_some(last)
    }

    /// Writes this record into `state_dir`, replacing the previous one.
    ///
    /// The state directory is made when it is not there yet (`shprd tick` runs
    /// without a service), and the file is replaced by a rename so a reader
    /// never sees half of one.
    pub fn write(&self, state_dir: &Path) -> io::Result<()> {
        std::fs::create_dir_all(state_dir)?;
        let name = file_name(Path::new(&self.repo));
        let path = state_dir.join(&name);
        let temporary: PathBuf = state_dir.join(format!("{name}.tmp"));
        let text = serde_json::to_string(self).map_err(io::Error::other)?;
        std::fs::write(&temporary, text)?;
        std::fs::rename(&temporary, path)
    }

    /// The one line `shprd status` prints about a pass that failed.
    ///
    /// It names the first failed step and how many others there were, and keeps
    /// the error to its first line so a page of a CLI's complaint cannot turn
    /// one warning into a screenful.
    pub fn warning(&self) -> String {
        let Some(first) = self.failed.first() else {
            return String::new();
        };
        let error = first.error.lines().next().unwrap_or_default().trim();
        let mut line = if first.task.is_empty() {
            format!("the last pass failed at `{}`: {error}", first.step)
        } else {
            format!(
                "the last pass failed at `{}` on {}: {error}",
                first.step, first.task
            )
        };
        let others = self.failed.len() - 1;
        if others > 0 {
            line.push_str(&format!(" (and {others} more)"));
        }
        line
    }
}

/// How many passes in a row one task's step has failed the same way.
///
/// A step that fails is the person's to fix — a scope that overlaps another
/// task, an integration branch nobody made — so the run repeats it, and every
/// pass writes one `WARN` line. That is right for the first pass and useless by
/// the tenth: nobody is reading a terminal, and `/api/state`'s `needs` stays
/// empty, so the board shows an unchanged row for ever.
///
/// This keeps the run of identical failures where the next pass can read it, so
/// the third one can be a card instead of another warning. It is a mirror of the
/// pass, not the record: the log is what happened, and a file that cannot be
/// written is a warning rather than a failed pass.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failures {
    /// The repository these failures happened in.
    pub repo: String,
    /// One entry per task and step that failed on the pass that wrote it, with
    /// how many passes in a row it has failed that way.
    pub runs: Vec<FailureRun>,
}

/// One task's step, and how long it has been failing the same way.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureRun {
    /// The task the step was about.
    pub task: String,
    /// The step's name: `start`, `review`, `verdict`, `land`, `send_back`.
    pub step: String,
    /// The error the step reported, as the pass recorded it.
    pub error: String,
    /// How many passes in a row failed this way, this one included.
    pub passes: u32,
}

/// How many passes a step must fail the same way before the run asks a person.
///
/// Three is enough to tell a transient failure — a host that was busy, a worktree
/// another writer held — from one that is a person's to fix, and short enough
/// that a run which cannot move does not sit quiet for a minute.
pub const REPEATS_BEFORE_A_CARD: u32 = 3;

impl Failures {
    /// The record the pass that produced `report` leaves, given the record the
    /// pass before it left.
    ///
    /// A step that failed on both passes with the same words is one run longer;
    /// a step that did not fail on this pass is forgotten, because the person
    /// fixed it or something else moved the task. A step that failed differently
    /// starts again at one: the error is what the card would name, so a new
    /// error is a new problem.
    pub fn next(previous: Option<&Self>, report: &TickReport, repo: &Path) -> Self {
        let failures: Vec<&StepReport> = report.steps.iter().filter(|step| !step.ok).collect();
        let runs = failures
            .iter()
            .map(|step| {
                let passes = previous
                    .and_then(|previous| {
                        previous.runs.iter().find(|run| {
                            run.task == step.task
                                && run.step == step.action
                                && run.error == step.detail
                        })
                    })
                    .map_or(1, |run| run.passes.saturating_add(1));
                FailureRun {
                    task: step.task.clone(),
                    step: step.action.clone(),
                    error: step.detail.clone(),
                    passes,
                }
            })
            .collect();
        Self {
            repo: repo.display().to_string(),
            runs,
        }
    }

    /// The run for one task's step on this pass, when it failed.
    pub fn run_of(&self, task: &str, step: &str) -> Option<&FailureRun> {
        self.runs
            .iter()
            .find(|run| run.task == task && run.step == step)
    }

    /// Whether `task`'s `step` has now failed the same way often enough for a
    /// person to be asked.
    pub fn needs_a_person(&self, task: &str, step: &str) -> bool {
        self.run_of(task, step)
            .is_some_and(|run| run.passes >= REPEATS_BEFORE_A_CARD)
    }

    /// The newest record written for `repo`, when there is one.
    pub fn read(state_dir: &Path, repo: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(state_dir.join(failures_file_name(repo))).ok()?;
        let found: Self = serde_json::from_str(&text).ok()?;
        (Path::new(&found.repo) == repo).then_some(found)
    }

    /// Writes this record into `state_dir`, replacing the previous one.
    pub fn write(&self, state_dir: &Path) -> io::Result<()> {
        std::fs::create_dir_all(state_dir)?;
        let name = failures_file_name(Path::new(&self.repo));
        let path = state_dir.join(&name);
        let temporary: PathBuf = state_dir.join(format!("{name}.tmp"));
        let text = serde_json::to_string(self).map_err(io::Error::other)?;
        std::fs::write(&temporary, text)?;
        std::fs::rename(&temporary, path)
    }
}

/// The failure record's file name for `repo`, beside the last pass's.
fn failures_file_name(repo: &Path) -> String {
    format!("repeats-{}", &file_name(repo)[PREFIX.len()..])
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{FailedStep, Failures, LastPass};
    use crate::tick::{StepReport, TickReport};

    /// A pass over `/repo` with two steps, one of them failed.
    fn report() -> TickReport {
        TickReport {
            run: Some(String::from("m9")),
            steps: vec![
                StepReport {
                    action: String::from("start"),
                    task: String::from("T1"),
                    ok: true,
                    detail: String::from("started T1"),
                    seq: Some(7),
                },
                StepReport {
                    action: String::from("start"),
                    task: String::from("T2"),
                    ok: false,
                    detail: String::from("no workspace \"shprd · repo\" yet"),
                    seq: None,
                },
            ],
            news: Vec::new(),
            interval_secs: None,
        }
    }

    #[test]
    fn a_pass_keeps_the_step_that_failed_and_its_error() {
        let last = LastPass::of(&report(), std::path::Path::new("/repo"), 42);
        assert_eq!(last.steps, 2);
        assert_eq!(last.run.as_deref(), Some("m9"));
        assert_eq!(
            last.failed,
            vec![FailedStep {
                task: String::from("T2"),
                step: String::from("start"),
                error: String::from("no workspace \"shprd · repo\" yet"),
            }]
        );
        assert_eq!(
            last.warning(),
            "the last pass failed at `start` on T2: no workspace \"shprd · repo\" yet"
        );
    }

    #[test]
    fn a_pass_that_worked_says_nothing() {
        let mut worked = report();
        worked.steps.retain(|step| step.ok);
        let last = LastPass::of(&worked, std::path::Path::new("/repo"), 42);
        assert!(last.failed.is_empty());
        assert!(last.warning().is_empty());
    }

    #[test]
    fn the_record_is_written_read_and_ignored_for_another_repo() {
        let dir = tempfile::tempdir().expect("temp dir");
        let state = dir.path().join("state");
        let last = LastPass::of(&report(), std::path::Path::new("/repo"), 42);
        last.write(&state).expect("write");
        assert_eq!(
            LastPass::read(&state, std::path::Path::new("/repo")),
            Some(last)
        );

        // A state directory serves a whole home, so another checkout's pass is
        // not this one's.
        assert!(LastPass::read(&state, std::path::Path::new("/other")).is_none());
        // And a missing one is no failure at all.
        assert!(LastPass::read(&dir.path().join("never"), std::path::Path::new("/repo")).is_none());
    }

    /// A pass over `/repo` that failed one step with `error`.
    fn one_failure(task: &str, step: &str, error: &str) -> TickReport {
        TickReport {
            run: Some(String::from("m9")),
            steps: vec![StepReport {
                action: step.to_string(),
                task: task.to_string(),
                ok: false,
                detail: error.to_string(),
                seq: None,
            }],
            news: Vec::new(),
            interval_secs: None,
        }
    }

    #[test]
    fn the_same_failure_counts_up_and_a_different_one_starts_again() {
        // Bug 3 of gate run #4: a step that failed every pass warned every pass
        // and never reached a person. The record is what lets the third pass be
        // a card.
        let repo = std::path::Path::new("/repo");
        let first = Failures::next(None, &one_failure("C2", "start", "scope overlaps T1"), repo);
        assert_eq!(first.run_of("C2", "start").map(|run| run.passes), Some(1));
        assert!(!first.needs_a_person("C2", "start"));

        let second = Failures::next(
            Some(&first),
            &one_failure("C2", "start", "scope overlaps T1"),
            repo,
        );
        assert_eq!(second.run_of("C2", "start").map(|run| run.passes), Some(2));
        assert!(!second.needs_a_person("C2", "start"));

        let third = Failures::next(
            Some(&second),
            &one_failure("C2", "start", "scope overlaps T1"),
            repo,
        );
        assert_eq!(third.run_of("C2", "start").map(|run| run.passes), Some(3));
        assert!(
            third.needs_a_person("C2", "start"),
            "three passes of the same failure is the person's"
        );

        // A different error is a different problem, so it starts again.
        let other = Failures::next(
            Some(&third),
            &one_failure("C2", "start", "no workspace yet"),
            repo,
        );
        assert_eq!(other.run_of("C2", "start").map(|run| run.passes), Some(1));
    }

    #[test]
    fn a_pass_that_worked_forgets_the_run_of_failures() {
        let repo = std::path::Path::new("/repo");
        let failed = Failures::next(None, &one_failure("C2", "start", "scope overlaps"), repo);
        let mut worked = one_failure("C2", "start", "scope overlaps");
        worked.steps[0].ok = true;
        let healed = Failures::next(Some(&failed), &worked, repo);
        assert!(healed.runs.is_empty(), "the failure was fixed: {healed:?}");
        assert!(!healed.needs_a_person("C2", "start"));
    }

    #[test]
    fn the_failure_record_is_written_read_and_ignored_for_another_repo() {
        let dir = tempfile::tempdir().expect("temp dir");
        let state = dir.path().join("state");
        let record = Failures::next(
            None,
            &one_failure("C2", "start", "scope overlaps"),
            std::path::Path::new("/repo"),
        );
        record.write(&state).expect("write");
        assert_eq!(
            Failures::read(&state, std::path::Path::new("/repo")),
            Some(record)
        );
        assert!(Failures::read(&state, std::path::Path::new("/other")).is_none());
    }

    #[test]
    fn a_pass_that_could_not_read_the_run_is_a_failure_too() {
        let last = LastPass::unreadable(
            std::path::Path::new("/repo"),
            42,
            "reading the event log: no such file",
        );
        assert_eq!(last.steps, 0);
        assert_eq!(
            last.warning(),
            "the last pass failed at `pass`: reading the event log: no such file"
        );
    }
}
