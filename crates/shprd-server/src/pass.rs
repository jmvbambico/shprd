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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{FailedStep, LastPass};
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
