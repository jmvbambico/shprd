//! The engine's own cleanup of finished tasks.
//!
//! A task that landed or was dropped leaves a worktree (with its build output),
//! a branch and sometimes a window behind. `shprd janitor --apply` removes them
//! by hand; this module is the part the tick runs by itself, so the disk does
//! not fill up while nobody types it.
//!
//! What is decided here is only *which* of the janitor's actions may run on this
//! pass ([`eligible`]) and how a removal that keeps failing is counted
//! ([`CleanFailures`]). The plan and the removals are `shprd_git::janitor`'s.
//! The decisions are pure, so they are tested without a repository.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use shprd_core::state::{RunState, TaskStatus};
use shprd_git::janitor::{Action, MILLIS_PER_DAY};

/// How many passes in a row a removal may fail before the coordinator is told.
pub const FAILURES_BEFORE_A_CARD: u32 = 3;

/// The prefix of the failure record's file name in the state directory.
const PREFIX: &str = "clean-";

/// The tasks the engine may clean up on this pass.
///
/// A task qualifies once it landed or a human dropped it, nothing is running for
/// it (`live`, which holds the tasks with a worker or a reviewer window open) and
/// no card of its own is still waiting for an answer. A task that is working,
/// held back or waiting on the person never qualifies.
pub fn eligible(state: &RunState, live: &BTreeSet<String>, now_ms: u64) -> BTreeSet<String> {
    state
        .tasks
        .values()
        .filter(|task| matches!(task.status, TaskStatus::Integrated | TaskStatus::Dropped))
        .filter(|task| !live.contains(&task.id))
        .filter(|task| {
            !state
                .asks
                .values()
                .any(|ask| ask.task == task.id && ask.is_open_at(now_ms))
        })
        .map(|task| task.id.clone())
        .collect()
}

/// The tasks that are finished but still have a window open: the pass closes
/// those windows and cleans the task on the next one.
pub fn lingering(state: &RunState, live: &BTreeSet<String>) -> BTreeSet<String> {
    state
        .tasks
        .values()
        .filter(|task| matches!(task.status, TaskStatus::Integrated | TaskStatus::Dropped))
        .filter(|task| live.contains(&task.id))
        .map(|task| task.id.clone())
        .collect()
}

/// Keeps only the janitor actions the engine runs by itself: archive, remove the
/// worktree, delete the branch — and only for a task in `tasks`. Expiring an
/// archive ref, the remote and the run's own integration resources stay with
/// `shprd janitor`.
pub fn only_for(actions: Vec<Action>, tasks: &BTreeSet<String>) -> Vec<Action> {
    actions
        .into_iter()
        .filter(|action| {
            matches!(
                action,
                Action::ArchiveRef { .. }
                    | Action::RemoveWorktree { .. }
                    | Action::DeleteBranch { .. }
            ) && action.task().is_some_and(|task| tasks.contains(task))
        })
        .collect()
}

/// A removal that failed, and how many passes in a row it has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanFailure {
    /// The task whose cleanup it was.
    pub task: String,
    /// The step, as the janitor describes it: it names the path or branch.
    pub step: String,
    /// What git (or the filesystem) said.
    pub error: String,
    /// Passes in a row this step has failed.
    pub passes: u32,
}

/// The failing removals of one repository, kept between passes in the state
/// directory.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanFailures {
    /// The repository these belong to.
    pub repo: String,
    /// One entry per step that failed on the last pass that ran cleanup.
    pub failures: Vec<CleanFailure>,
}

impl CleanFailures {
    /// The record after a pass in which `failed` steps (`(task, step, error)`)
    /// failed. A step that did not fail again is forgotten; one that did counts
    /// one more pass, whatever git's words were this time.
    pub fn next(&self, repo: &Path, failed: &[(String, String, String)]) -> Self {
        let failures = failed
            .iter()
            .map(|(task, step, error)| {
                let passes = self
                    .failures
                    .iter()
                    .find(|old| &old.task == task && &old.step == step)
                    .map_or(1, |old| old.passes.saturating_add(1));
                CleanFailure {
                    task: task.clone(),
                    step: step.clone(),
                    error: error.clone(),
                    passes,
                }
            })
            .collect();
        Self {
            repo: repo.display().to_string(),
            failures,
        }
    }

    /// The failures that reached the count on this very pass: the card is raised
    /// once, not on every pass after it.
    pub fn reached_the_count(&self) -> impl Iterator<Item = &CleanFailure> {
        self.failures
            .iter()
            .filter(|failure| failure.passes == FAILURES_BEFORE_A_CARD)
    }

    /// The record written for `repo`, or an empty one.
    pub fn read(state_dir: &Path, repo: &Path) -> Self {
        let name = crate::pass::sibling_file_name(repo, PREFIX);
        std::fs::read_to_string(state_dir.join(name))
            .ok()
            .and_then(|text| serde_json::from_str::<Self>(&text).ok())
            .filter(|found| Path::new(&found.repo) == repo)
            .unwrap_or_default()
    }

    /// Writes the record into `state_dir`, replacing the previous one.
    pub fn write(&self, state_dir: &Path) -> io::Result<()> {
        std::fs::create_dir_all(state_dir)?;
        let name = crate::pass::sibling_file_name(Path::new(&self.repo), PREFIX);
        let temporary = state_dir.join(format!("{name}.tmp"));
        let text = serde_json::to_string(self).map_err(io::Error::other)?;
        std::fs::write(&temporary, text)?;
        std::fs::rename(&temporary, state_dir.join(name))
    }
}

/// The card's words for a removal that kept failing: what, where and why.
pub fn card_body(failure: &CleanFailure) -> String {
    format!(
        "shprd could not clean up {} after {} tries: {}. Git said: {}",
        failure.task,
        failure.passes,
        failure.step,
        failure.error.lines().next().unwrap_or_default().trim()
    )
}

/// Removes the review packet folders under `reviews` that nothing has touched for
/// `days` days, except `keep` (the folders of runs still working). Returns the
/// folders it removed. `days == 0` means never.
///
/// Only direct children that are real folders are removed — a symlink is never
/// followed — and a folder that cannot be removed is skipped, to be tried again.
pub fn expire_review_packets(
    reviews: &Path,
    now: SystemTime,
    days: u32,
    keep: &BTreeSet<String>,
) -> Vec<PathBuf> {
    if days == 0 {
        return Vec::new();
    }
    let window = Duration::from_millis(u64::from(days).saturating_mul(MILLIS_PER_DAY));
    let Ok(entries) = std::fs::read_dir(reviews) else {
        return Vec::new();
    };
    let mut removed = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.is_dir() || keep.contains(&name) {
            continue;
        }
        // The newest touch anywhere inside, so a run that is still writing
        // packets (even another run's) is never taken for an old one.
        let old = newest_touch(&path, meta.modified().ok())
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > window);
        if old && std::fs::remove_dir_all(&path).is_ok() {
            removed.push(path);
        }
    }
    removed
}

/// The most recent modification time of `dir` or anything inside it (symlinks
/// are not followed). `None` when no time can be read.
fn newest_touch(dir: &Path, own: Option<SystemTime>) -> Option<SystemTime> {
    let mut newest = own;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return newest;
    };
    for entry in entries.flatten() {
        let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
            continue;
        };
        let mut time = meta.modified().ok();
        if meta.is_dir() {
            time = newest_touch(&entry.path(), time);
        }
        newest = match (newest, time) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
    }
    newest
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn a_failing_step_counts_up_and_a_fixed_one_is_forgotten() {
        let repo = Path::new("/repo");
        let failed = vec![("T1".to_string(), "remove worktree".to_string(), "x".into())];
        let one = CleanFailures::default().next(repo, &failed);
        assert_eq!(one.failures[0].passes, 1);
        let two = one.next(repo, &failed);
        let three = two.next(repo, &failed);
        assert_eq!(three.reached_the_count().count(), 1);
        assert_eq!(three.next(repo, &failed).reached_the_count().count(), 0);
        assert!(three.next(repo, &[]).failures.is_empty());
    }

    #[test]
    fn the_record_survives_a_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Path::new("/repo");
        let record = CleanFailures::default().next(repo, &[("T1".into(), "s".into(), "e".into())]);
        record.write(dir.path()).unwrap();
        assert_eq!(CleanFailures::read(dir.path(), repo), record);
        assert_eq!(
            CleanFailures::read(dir.path(), Path::new("/other")),
            CleanFailures::default()
        );
    }

    #[test]
    fn a_folder_with_a_recent_packet_inside_stays_though_its_own_time_is_old() {
        let dir = tempfile::tempdir().unwrap();
        let day = Duration::from_secs(86_400);
        let run = dir.path().join("other-run");
        std::fs::create_dir_all(run.join("T2-a1")).unwrap();
        std::fs::write(run.join("T2-a1/findings.json"), "{}").unwrap();
        std::fs::File::open(&run)
            .unwrap()
            .set_modified(SystemTime::now() - day * 40)
            .unwrap();
        let removed = expire_review_packets(dir.path(), SystemTime::now(), 14, &BTreeSet::new());
        assert!(removed.is_empty() && run.exists());
    }

    #[test]
    fn old_review_packets_go_and_recent_or_active_ones_stay() {
        let dir = tempfile::tempdir().unwrap();
        let day = Duration::from_secs(86_400);
        let make = |name: &str, age_days: u64| {
            let path = dir.path().join(name);
            std::fs::create_dir_all(path.join("T1-a1")).unwrap();
            std::fs::write(path.join("T1-a1/findings.json"), "{}").unwrap();
            let then = SystemTime::now() - day * age_days as u32;
            for inner in [
                path.join("T1-a1/findings.json"),
                path.join("T1-a1"),
                path.clone(),
            ] {
                std::fs::File::open(inner)
                    .unwrap()
                    .set_modified(then)
                    .unwrap();
            }
            path
        };
        let old = make("old", 40);
        let recent = make("recent", 2);
        let active = make("active", 40);
        let keep = BTreeSet::from([String::from("active")]);
        let removed = expire_review_packets(dir.path(), SystemTime::now(), 14, &keep);
        assert_eq!(removed, vec![old.clone()]);
        assert!(!old.exists() && recent.exists() && active.exists());
        // `0` means never.
        let never = expire_review_packets(dir.path(), SystemTime::now() + day * 900, 0, &keep);
        assert!(never.is_empty() && recent.exists());
    }
}
