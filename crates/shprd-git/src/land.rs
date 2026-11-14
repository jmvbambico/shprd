// Per-task landing adapted from shepherdr (MIT), mods/tasks/bin/herdr_task/github.py,
// commit a3f327a.
// Copyright (c) 2026 Elias Stravik
// https://github.com/eliasstravik/shepherdr
//
//! Landing one task: its own branch, its own pull request, merged by policy.
//!
//! This is the per-task workflow Shepherdr runs (M8.3). A task's attempt
//! commits on its own branch, so landing it is: push the branch, open (or
//! refresh) a pull request into the run's base branch, then let the merge
//! policy decide — `ask` waits for a human, `checks` waits for the forge's
//! checks, `auto` merges at once.
//!
//! Every branch of the way is idempotent, because a landing is a verb a human
//! re-runs: a task whose work is already in its base is reported as such
//! without touching the forge, a pull request already open is refreshed rather
//! than duplicated, and the merge names the commit it means to merge.
//!
//! The old run-level integration branch is not written here. Its code
//! ([`crate::integrate`]) still compiles and still lands runs; M8.3 only adds
//! this path.

use std::path::Path;

use thiserror::Error;

use shprd_core::config::MergePolicy;
use shprd_core::event::{LandOutcome, ReportStatus};
use shprd_core::gate::REVIEW_GATE;
use shprd_core::state::Task;

use crate::forge::{Forge, ForgeError, Pr};
use crate::git::{Git, GitError};

/// The remote a run pushes to and compares its base branch with.
const REMOTE: &str = "origin";

/// One task's landing: what to push, what to propose it into, and how the pull
/// request reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Landing<'a> {
    /// The task's id, as the log names it.
    pub task: &'a str,
    /// The branch the task's attempt commits on.
    pub branch: &'a str,
    /// The base branch the pull request proposes to merge into.
    pub base: &'a str,
    /// The pull request's title.
    pub title: &'a str,
    /// The file holding the pull request's body.
    pub body: &'a Path,
    /// Whether the log says the branch has work of its own: the attempt
    /// reported a commit, or a pull request was opened for the branch.
    ///
    /// Git cannot tell a branch whose commits merged from one that never had
    /// any, once the tip is an ancestor of the base: both leave the tip inside
    /// the base's history. This is the fact that separates them, and it is only
    /// read in that case.
    pub has_work: bool,
}

/// Why a pull request waits instead of merging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitReason {
    /// The `ask` policy has no approval at this head yet.
    Approval,
    /// The `checks` policy is waiting for the forge's checks.
    Checks,
}

impl WaitReason {
    /// What a human has to do for the landing to go on.
    pub fn advice(self, task: &str) -> String {
        match self {
            WaitReason::Approval => format!(
                "waiting for your approval: run `shprd approve {task}`, or approve the pull request"
            ),
            WaitReason::Checks => {
                String::from("waiting for its checks: run `shprd land` again once they pass")
            }
        }
    }
}

/// What a landing did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Landed {
    /// The pull request was merged by this landing.
    Merged {
        /// The pull request that landed it.
        pr: Pr,
        /// The commit that landed.
        head: String,
    },
    /// The pull request was already merged when this landing looked.
    AlreadyMerged {
        /// The pull request, when this landing found it.
        pr: Option<Pr>,
        /// The commit that landed.
        head: String,
    },
    /// The branch's own work is already in the base branch as other commits:
    /// a squash merge, or a merge made by hand.
    SquashMerged {
        /// The base branch holding the work.
        base: String,
        /// The branch tip that was looked at.
        head: String,
    },
    /// The branch held no work of its own beyond its base.
    NothingToMerge {
        /// The base branch it had nothing to add to.
        base: String,
        /// The branch tip that was looked at.
        head: String,
    },
    /// The pull request is open, and its policy waits.
    Waiting {
        /// The pull request a human or the forge decides on.
        pr: Pr,
        /// What it waits for.
        reason: WaitReason,
        /// The branch tip it waits at.
        head: String,
    },
}

impl Landed {
    /// The pull request this landing touched, when it touched one.
    pub fn pr(&self) -> Option<&Pr> {
        match self {
            Landed::Merged { pr, .. } | Landed::Waiting { pr, .. } => Some(pr),
            Landed::AlreadyMerged { pr, .. } => pr.as_ref(),
            Landed::SquashMerged { .. } | Landed::NothingToMerge { .. } => None,
        }
    }

    /// The branch tip this landing looked at.
    pub fn head(&self) -> &str {
        match self {
            Landed::Merged { head, .. }
            | Landed::AlreadyMerged { head, .. }
            | Landed::SquashMerged { head, .. }
            | Landed::NothingToMerge { head, .. }
            | Landed::Waiting { head, .. } => head,
        }
    }

    /// The outcome to record, or `None` while the landing waits.
    ///
    /// A waiting landing has not ended: what it recorded is the pull request,
    /// and the outcome comes on the landing that merges or finds the work
    /// already in.
    pub fn outcome(&self) -> Option<LandOutcome> {
        match self {
            Landed::Merged { .. } => Some(LandOutcome::Merged),
            Landed::AlreadyMerged { .. } => Some(LandOutcome::AlreadyMerged),
            Landed::SquashMerged { .. } => Some(LandOutcome::SquashMerged),
            Landed::NothingToMerge { .. } => Some(LandOutcome::NothingToMerge),
            Landed::Waiting { .. } => None,
        }
    }

    /// The pull request's number, when one carried the outcome.
    pub fn number(&self) -> Option<u64> {
        self.pr().map(|pr| pr.number)
    }
}

/// Why a landing could not finish.
#[derive(Debug, Error)]
pub enum LandError {
    /// git failed.
    #[error(transparent)]
    Git(#[from] GitError),
    /// The forge failed.
    #[error(transparent)]
    Forge(#[from] ForgeError),
    /// The forge's pull request is at another commit than the branch's tip.
    #[error("the pull request is at head {found:?}, but the branch is at {expected:?}")]
    HeadMoved {
        /// The commit the branch is at.
        expected: String,
        /// The commit the forge reported.
        found: String,
    },
    /// The forge accepted the merge but does not report the pull request
    /// merged, so nothing may be recorded as landed.
    #[error("the forge did not report pull request {number} merged")]
    NotMerged {
        /// The pull request that was merged.
        number: u64,
    },
    /// The pull request's checks failed, so the work has to be fixed first.
    #[error("the pull request's checks failed: fix them and land again")]
    ChecksFailed {
        /// The pull request whose checks failed.
        number: u64,
    },
}

/// A landing that found the work already in the base cannot be told apart from
/// one that has to land it by type alone, so this says which it was.
enum Reached {
    /// Its own commits are in the base branch.
    AlreadyMerged,
    /// Its work is in the base branch as other commits.
    SquashMerged,
    /// Its branch has no work of its own.
    NothingToMerge,
}

/// Lands one task: pushes its branch, opens or refreshes its pull request, and
/// merges it as `policy` says.
///
/// `approved` says whether a human recorded an approval for the branch's
/// current tip: only the `ask` policy reads it, and only for that exact commit.
///
/// The work the base already holds is settled before the forge is asked
/// anything, so re-running a landing that finished pushes nothing, opens
/// nothing and merges nothing.
///
/// # Errors
///
/// Fails when git or the forge cannot be driven, when the forge's pull request
/// is not at the branch's tip, when its checks failed, or when the merge does
/// not settle.
pub fn land(
    forge: &dyn Forge,
    git: &Git,
    landing: &Landing<'_>,
    policy: MergePolicy,
    approved: bool,
) -> Result<Landed, LandError> {
    let head = git.head_sha()?;
    let base = base_revision(git, landing.base);
    if let Some(reached) = already_landed(git, &head, &base, landing.has_work)? {
        return Ok(landed_already(landing, reached, head));
    }
    push(git, landing.branch)?;
    let pr = open_or_refresh(forge, landing)?;
    let status = forge.status(pr.number)?;
    if status.merged {
        // The forge merged it between the push and this read: a fact about this
        // work only while it merged the very commit the branch is at.
        return if status.head_sha == head {
            Ok(Landed::AlreadyMerged { pr: Some(pr), head })
        } else {
            Err(LandError::HeadMoved {
                expected: head,
                found: status.head_sha,
            })
        };
    }
    if status.head_sha != head {
        return Err(LandError::HeadMoved {
            expected: head,
            found: status.head_sha,
        });
    }
    let reason = match policy {
        MergePolicy::Auto => None,
        MergePolicy::Ask if approved => None,
        MergePolicy::Ask => Some(WaitReason::Approval),
        MergePolicy::Checks => match status.checks_passed {
            Some(true) => None,
            Some(false) => {
                return Err(LandError::ChecksFailed { number: pr.number });
            }
            None => Some(WaitReason::Checks),
        },
    };
    match reason {
        Some(reason) => Ok(Landed::Waiting { pr, reason, head }),
        None => merge_now(forge, pr, head),
    }
}

/// The outcome for a task whose work the base already holds.
fn landed_already(landing: &Landing<'_>, reached: Reached, head: String) -> Landed {
    match reached {
        Reached::AlreadyMerged => Landed::AlreadyMerged { pr: None, head },
        Reached::SquashMerged => Landed::SquashMerged {
            base: landing.base.to_string(),
            head,
        },
        Reached::NothingToMerge => Landed::NothingToMerge {
            base: landing.base.to_string(),
            head,
        },
    }
}

/// The branch's own work, as the base already has it — or `None` when the
/// branch still has something to land.
///
/// Three questions, in the order that keeps them cheap and honest:
///
/// 1. Is its tip already in the base? Then its own commits are in the base as
///    they were made — or the branch never had any, which `has_work` says.
/// 2. Does the branch change anything at all since it left the base? If not,
///    there is nothing to merge.
/// 3. Does merging its tip into the base produce the base's own tree? Then its
///    change is already in the base as other commits: a squash merge, or a
///    merge made by hand.
///
/// A base without shared history with the branch cannot be compared, and
/// answers `None`: the forge is asked, and complains about what it finds.
fn already_landed(
    git: &Git,
    head: &str,
    base: &str,
    has_work: bool,
) -> Result<Option<Reached>, GitError> {
    let cut = match git.run(&["merge-base", base, head]) {
        Ok(cut) if !cut.is_empty() => cut,
        _ => return Ok(None),
    };
    if cut == head {
        // The tip is an ancestor of the base, so git sees no difference between
        // a branch whose commits merged and one that never moved past its base.
        // What is left is the log's word: a branch that reported a commit, or
        // that had a pull request opened for it, had work, and that work is in.
        return Ok(Some(if has_work {
            Reached::AlreadyMerged
        } else {
            Reached::NothingToMerge
        }));
    }
    // `diff --quiet` exits zero when the two ends hold the same tree, which is
    // the branch having made no change of its own since it left the base.
    if git.run(&["diff", "--quiet", &cut, head]).is_ok() {
        return Ok(Some(Reached::NothingToMerge));
    }
    if let Ok(tree) = git.run(&["merge-tree", "--write-tree", base, head]) {
        let base_tree = git.rev_parse(&format!("{base}^{{tree}}"))?;
        if tree.lines().next() == Some(base_tree.as_str()) {
            return Ok(Some(Reached::SquashMerged));
        }
    }
    Ok(None)
}

/// The revision to compare the branch with: the base fetched from the remote
/// first, so a merge made on the forge since is seen.
///
/// Best effort, like a hint: with no remote, no network or no such branch there
/// the local branch is used, and the comparison is as good as the checkout is.
/// It is a name git may fail to resolve, which the callers then read as "not
/// comparable".
fn base_revision(git: &Git, base: &str) -> String {
    let tracking = format!("refs/remotes/{REMOTE}/{base}");
    let fetch = git.run(&[
        "fetch",
        "--quiet",
        "--no-tags",
        REMOTE,
        &format!("+refs/heads/{base}:{tracking}"),
    ]);
    if fetch.is_ok() && git.rev_parse(&tracking).is_ok() {
        return tracking;
    }
    if git.rev_parse(&format!("refs/heads/{base}")).is_ok() {
        return format!("refs/heads/{base}");
    }
    base.to_string()
}

/// Pushes the task's branch, unless the remote already holds that exact tip.
///
/// The comparison is what makes a re-run cheap: a branch already pushed by an
/// earlier landing is not pushed again, which matters because every push runs
/// the remote's own hooks. The push itself may rewrite a branch whose attempt
/// was replaced, so it is a lease: another writer's commits are never
/// overwritten silently.
fn push(git: &Git, branch: &str) -> Result<(), GitError> {
    let head = git.head_sha()?;
    let there = git
        .run(&["ls-remote", REMOTE, &format!("refs/heads/{branch}")])
        .ok()
        .and_then(|out| {
            out.split_whitespace()
                .next()
                .map(|sha| sha == head.as_str())
        })
        .unwrap_or(false);
    if there {
        return Ok(());
    }
    git.run(&[
        "push",
        "--force-with-lease",
        "--set-upstream",
        REMOTE,
        branch,
    ])
    .map(|_| ())
}

/// The task's open pull request, with its body refreshed, or a new one.
///
/// A pull request a human or an agent opened by hand for the branch is the
/// task's: it is found by its head, so the landing never opens a second one
/// beside it.
fn open_or_refresh(forge: &dyn Forge, landing: &Landing<'_>) -> Result<Pr, ForgeError> {
    match forge.find_open(landing.branch)? {
        Some(pr) => {
            forge.edit_body(pr.number, landing.body)?;
            Ok(pr)
        }
        None => forge.create(landing.base, landing.branch, landing.title, landing.body),
    }
}

/// Merges the pull request at `head` and checks that the forge says so.
fn merge_now(forge: &dyn Forge, pr: Pr, head: String) -> Result<Landed, LandError> {
    forge.merge(pr.number, &head)?;
    // Asked again rather than assumed: a merge the forge reports as done is the
    // only thing that may be recorded as landed.
    let after = forge.status(pr.number)?;
    if after.merged {
        Ok(Landed::Merged { pr, head })
    } else {
        Err(LandError::NotMerged { number: pr.number })
    }
}

/// Why a task may not land yet, read off the log's own records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LandRefusal {
    /// The task has no attempt, so there is nothing to land.
    NoAttempt,
    /// The latest attempt has not handed in: it never reported.
    NoReport {
        /// The attempt number.
        attempt: u32,
    },
    /// The latest attempt's hand-in was blocked or failed, not completed.
    NotCompleted {
        /// The attempt number.
        attempt: u32,
        /// What it reported.
        status: ReportStatus,
    },
    /// The attempt was never sent for review.
    NoReview {
        /// The attempt number.
        attempt: u32,
    },
    /// Every review the attempt was sent for shared the author's vendor.
    DegradedReview {
        /// The attempt number.
        attempt: u32,
        /// The reviewers that were asked.
        reviewers: Vec<String>,
    },
    /// A human overrode the review gate: the decision is theirs, not a review.
    ReviewGateOverridden {
        /// The attempt number.
        attempt: u32,
        /// The reason the human gave.
        reason: String,
    },
    /// The review gate passed, but nothing proves the reviewer wrote it.
    UnprovenReview {
        /// The attempt number.
        attempt: u32,
    },
    /// The attempt has no passing review gate.
    NoPassedReview {
        /// The attempt number.
        attempt: u32,
    },
}

impl std::fmt::Display for LandRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LandRefusal::NoAttempt => write!(f, "it has no attempt to land"),
            LandRefusal::NoReport { attempt } => {
                write!(f, "attempt {attempt} never handed its work in")
            }
            LandRefusal::NotCompleted { attempt, status } => write!(
                f,
                "attempt {attempt} handed in as {} instead of completed",
                match status {
                    ReportStatus::Completed => "completed",
                    ReportStatus::Blocked => "blocked",
                    ReportStatus::Failed => "failed",
                }
            ),
            LandRefusal::NoReview { attempt } => {
                write!(f, "attempt {attempt} was never reviewed")
            }
            LandRefusal::DegradedReview { attempt, reviewers } => write!(
                f,
                "every review of attempt {attempt} was degraded ({}), so no independent review happened",
                reviewers.join(", ")
            ),
            LandRefusal::ReviewGateOverridden { attempt, reason } => write!(
                f,
                "a human overrode the review gate on attempt {attempt} ({reason})"
            ),
            LandRefusal::UnprovenReview { attempt } => write!(
                f,
                "attempt {attempt}'s review verdict has no reviewer provenance"
            ),
            LandRefusal::NoPassedReview { attempt } => {
                write!(f, "attempt {attempt} has no passed review gate")
            }
        }
    }
}

/// Why a task may not land yet, or `None` when it may.
///
/// Two things are required, and neither is a worker's own word:
///
/// - a passing hand-in: the latest attempt completed and reported;
/// - a cross-vendor review the engine recorded: a review dispatched to a
///   reviewer from another vendor, and a `review` gate that passed with the
///   provenance the engine writes. A degraded review, a human's override of the
///   review gate and a verdict nothing proves are all refusals.
///
/// The gates beyond review are not re-judged here: the task's status already
/// carries what they decided.
pub fn landing_refusal(task: &Task) -> Option<LandRefusal> {
    let Some(attempt) = task.latest_attempt() else {
        return Some(LandRefusal::NoAttempt);
    };
    let Some(report) = attempt.report.as_ref() else {
        return Some(LandRefusal::NoReport { attempt: attempt.n });
    };
    if !hand_in_passed(report.status) {
        return Some(LandRefusal::NotCompleted {
            attempt: attempt.n,
            status: report.status,
        });
    }
    if attempt.reviews.is_empty() {
        return Some(LandRefusal::NoReview { attempt: attempt.n });
    }
    if !attempt.reviews.iter().any(|review| !review.degraded) {
        return Some(LandRefusal::DegradedReview {
            attempt: attempt.n,
            reviewers: attempt
                .reviews
                .iter()
                .map(|review| review.reviewer.clone())
                .collect(),
        });
    }
    if let Some(record) = task
        .overrides
        .iter()
        .rev()
        .find(|record| record.rule == REVIEW_GATE)
    {
        return Some(LandRefusal::ReviewGateOverridden {
            attempt: attempt.n,
            reason: record.reason.clone(),
        });
    }
    match attempt
        .gates
        .iter()
        .rev()
        .find(|gate| gate.gate == REVIEW_GATE)
    {
        Some(gate) if gate.passed && gate.source.is_some() => None,
        Some(gate) if gate.passed => Some(LandRefusal::UnprovenReview { attempt: attempt.n }),
        _ => Some(LandRefusal::NoPassedReview { attempt: attempt.n }),
    }
}

/// Whether a report is a passing hand-in.
///
/// Today that is the attempt completing and reporting. M8.4 adds the worker's
/// own checks report — which gates it ran and how they went — to the hand-in,
/// and this is the function that grows: every landing asks here first.
fn hand_in_passed(status: ReportStatus) -> bool {
    status == ReportStatus::Completed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::{FakeForge, PrStatus};
    use shprd_core::event::{Actor, Event, EventKind, ReviewSource, Severity};
    use shprd_core::state::{Attempt, GateResult, Report, ReviewRequest, RunState, TaskStatus};
    use std::path::PathBuf;
    use std::process::Command;

    /// A repository, its bare `origin`, and a task branch with one commit.
    struct Fixture {
        /// Keeps the temp directory alive.
        _temp: tempfile::TempDir,
        /// The repository the branch belongs to.
        root: PathBuf,
        /// The worktree the task's attempt commits in.
        worktree: PathBuf,
        /// The head of the task's branch.
        head: String,
        /// The pull request body file.
        body: PathBuf,
    }

    impl Fixture {
        /// A repo on `main` pushed to a bare `origin`, plus `branch` cut from
        /// it with one commit.
        fn new(branch: &str) -> Fixture {
            let temp = match tempfile::tempdir() {
                Ok(temp) => temp,
                Err(err) => panic!("tempdir: {err}"),
            };
            let base = temp.path().to_path_buf();
            let root = base.join("repo");
            let origin = base.join("origin.git");
            let worktree = base.join("wt");
            let body = base.join("body.md");
            if let Err(err) = std::fs::write(&body, "# the task\n") {
                panic!("write the body: {err}");
            }
            if let Err(err) = std::fs::create_dir_all(&root) {
                panic!("mkdir {}: {err}", root.display());
            }
            let fixture = Fixture {
                _temp: temp,
                root,
                worktree,
                head: String::new(),
                body,
            };
            fixture.git_in(&base, &["init", "--bare", "origin.git"]);
            fixture.git(&["init", "-b", "main"]);
            fixture.git(&["config", "user.name", "shprd tests"]);
            fixture.git(&["config", "user.email", "tests@example.invalid"]);
            fixture.git(&["config", "commit.gpgsign", "false"]);
            fixture.git(&[
                "remote",
                "add",
                "origin",
                origin.to_str().unwrap_or("origin.git"),
            ]);
            fixture.commit(&["a.txt"], "one\n", "seed");
            fixture.git(&["push", "-u", "origin", "main"]);
            let path = fixture.worktree.clone();
            let worktree = path.display().to_string();
            fixture.git(&["worktree", "add", "-b", branch, &worktree, "main"]);
            fixture.commit_in(&path, &["a.txt"], "two\n", "the task's work");
            let mut fixture = fixture;
            fixture.head = fixture.git_in(&path, &["rev-parse", "HEAD"]);
            fixture
        }

        /// Runs git in the repository, asserting success.
        fn git(&self, args: &[&str]) -> String {
            self.git_in(&self.root.clone(), args)
        }

        /// Commits `body` to `files` in the repository.
        fn commit(&self, files: &[&str], body: &str, message: &str) {
            let root = self.root.clone();
            self.commit_in(&root, files, body, message);
        }

        /// Commits `body` to `files` in `dir`, asserting success.
        fn commit_in(&self, dir: &Path, files: &[&str], body: &str, message: &str) {
            for file in files {
                let path = dir.join(file);
                if let Err(err) = std::fs::write(&path, body) {
                    panic!("write {}: {err}", path.display());
                }
                self.git_in(dir, &["add", "--", file]);
            }
            self.git_in(dir, &["commit", "-m", message]);
        }

        /// Runs git in `dir`, asserting success, and returns its trimmed stdout.
        fn git_in(&self, dir: &Path, args: &[&str]) -> String {
            let output = match Command::new("git").args(args).current_dir(dir).output() {
                Ok(output) => output,
                Err(err) => panic!("git {args:?} in {}: {err}", dir.display()),
            };
            assert!(
                output.status.success(),
                "git {args:?} in {} failed: {}",
                dir.display(),
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        }

        /// The worktree's git runner.
        fn runner(&self) -> Git {
            Git::new(self.worktree.clone())
        }

        /// A landing of the task's branch into `main`.
        fn landing<'a>(&'a self, branch: &'a str) -> Landing<'a> {
            Landing {
                task: "T1",
                branch,
                base: "main",
                title: "T1: a task",
                body: &self.body,
                has_work: true,
            }
        }

        /// A forge that finds no pull request and reports `checks` at this tip.
        fn forge(&self, checks: Option<bool>) -> FakeForge {
            let forge = FakeForge::new();
            forge.set_status(
                1,
                PrStatus {
                    approved: false,
                    checks_passed: checks,
                    merged: false,
                    head_sha: self.head.clone(),
                },
            );
            forge
        }
    }

    /// Lands with the fixture's forge and runner.
    fn land_with(
        fixture: &Fixture,
        forge: &FakeForge,
        policy: MergePolicy,
        approved: bool,
    ) -> Result<Landed, LandError> {
        land(
            forge,
            &fixture.runner(),
            &fixture.landing("shprd/r1/T1"),
            policy,
            approved,
        )
    }

    #[test]
    fn auto_opens_a_pull_request_and_merges_it() {
        let fixture = Fixture::new("shprd/r1/T1");
        let forge = fixture.forge(Some(true));
        let landed = match land_with(&fixture, &forge, MergePolicy::Auto, false) {
            Ok(landed) => landed,
            Err(err) => panic!("auto must land: {err}"),
        };
        assert_eq!(
            landed,
            Landed::Merged {
                pr: Pr {
                    number: 1,
                    url: "https://example.invalid/pull/1".to_string(),
                    head: "shprd/r1/T1".to_string(),
                },
                head: fixture.head.clone(),
            }
        );
        assert_eq!(
            forge.calls(),
            vec![
                "find_open shprd/r1/T1".to_string(),
                format!(
                    "create main shprd/r1/T1 T1: a task {}",
                    fixture.body.display()
                ),
                "status 1".to_string(),
                format!("merge 1 {}", fixture.head),
                "status 1".to_string(),
            ]
        );
        // The branch is on the remote, which is what the pull request proposes.
        let pushed = fixture.git_in(
            &fixture.root.clone(),
            &["rev-parse", "refs/remotes/origin/shprd/r1/T1"],
        );
        assert_eq!(pushed, fixture.head);
    }

    #[test]
    fn ask_opens_the_pull_request_and_waits() {
        let fixture = Fixture::new("shprd/r1/T1");
        let forge = fixture.forge(Some(true));
        let landed = match land_with(&fixture, &forge, MergePolicy::Ask, false) {
            Ok(landed) => landed,
            Err(err) => panic!("ask must wait, not fail: {err}"),
        };
        match &landed {
            Landed::Waiting { pr, reason, head } => {
                assert_eq!(pr.number, 1);
                assert_eq!(*reason, WaitReason::Approval);
                assert_eq!(head, &fixture.head);
            }
            other => panic!("ask must wait: {other:?}"),
        }
        assert_eq!(
            forge.calls(),
            vec![
                "find_open shprd/r1/T1".to_string(),
                format!(
                    "create main shprd/r1/T1 T1: a task {}",
                    fixture.body.display()
                ),
                "status 1".to_string(),
            ],
            "nothing may be merged before a human approves"
        );
        assert!(landed.outcome().is_none(), "waiting is not an outcome");
    }

    #[test]
    fn ask_merges_once_the_human_approved_this_head() {
        let fixture = Fixture::new("shprd/r1/T1");
        let forge = fixture.forge(Some(true));
        let landed = match land_with(&fixture, &forge, MergePolicy::Ask, true) {
            Ok(landed) => landed,
            Err(err) => panic!("an approved landing must merge: {err}"),
        };
        assert!(matches!(landed, Landed::Merged { .. }), "{landed:?}");
        assert!(
            forge.calls().contains(&format!("merge 1 {}", fixture.head)),
            "{:?}",
            forge.calls()
        );
    }

    #[test]
    fn checks_waits_while_they_run() {
        let fixture = Fixture::new("shprd/r1/T1");
        let forge = fixture.forge(None);
        let landed = match land_with(&fixture, &forge, MergePolicy::Checks, false) {
            Ok(landed) => landed,
            Err(err) => panic!("running checks must wait: {err}"),
        };
        match landed {
            Landed::Waiting { reason, .. } => assert_eq!(reason, WaitReason::Checks),
            other => panic!("checks must wait: {other:?}"),
        }
        assert!(
            !forge.calls().iter().any(|call| call.starts_with("merge")),
            "{:?}",
            forge.calls()
        );
    }

    #[test]
    fn checks_merges_once_they_pass() {
        let fixture = Fixture::new("shprd/r1/T1");
        let forge = fixture.forge(Some(true));
        let landed = match land_with(&fixture, &forge, MergePolicy::Checks, false) {
            Ok(landed) => landed,
            Err(err) => panic!("green checks must merge: {err}"),
        };
        assert!(matches!(landed, Landed::Merged { .. }), "{landed:?}");
    }

    #[test]
    fn failing_checks_refuse_the_landing() {
        let fixture = Fixture::new("shprd/r1/T1");
        let forge = fixture.forge(Some(false));
        let err = match land_with(&fixture, &forge, MergePolicy::Checks, false) {
            Ok(landed) => panic!("failing checks must not land: {landed:?}"),
            Err(err) => err,
        };
        assert!(
            matches!(err, LandError::ChecksFailed { number: 1 }),
            "{err}"
        );
        assert!(
            !forge.calls().iter().any(|call| call.starts_with("merge")),
            "{:?}",
            forge.calls()
        );
    }

    #[test]
    fn a_branch_whose_commits_merged_is_already_merged() {
        let fixture = Fixture::new("shprd/r1/T1");
        // The work is in the base as its own commits, as a merge on the forge
        // leaves it; the local base is fetched for the comparison.
        fixture.git_in(&fixture.root.clone(), &["checkout", "main"]);
        fixture.git(&["merge", "--no-ff", "-m", "merge the task", "shprd/r1/T1"]);
        fixture.git(&["push", "origin", "main"]);
        let forge = fixture.forge(Some(true));
        let landed = match land_with(&fixture, &forge, MergePolicy::Auto, false) {
            Ok(landed) => landed,
            Err(err) => panic!("an already merged task is not an error: {err}"),
        };
        assert!(
            matches!(landed, Landed::AlreadyMerged { ref pr, .. } if pr.is_none()),
            "with nothing in the log, git alone cannot tell a merged branch from \
             one that never had work, so the caller's word decides: {landed:?}"
        );
        assert_eq!(landed.outcome(), Some(LandOutcome::AlreadyMerged));
        assert!(
            forge.calls().is_empty(),
            "a landed task asks the forge nothing: {:?}",
            forge.calls()
        );
    }

    #[test]
    fn a_squash_merged_branch_is_detected_by_its_tree() {
        let fixture = Fixture::new("shprd/r1/T1");
        // The same change on the base as a different commit: a squash merge.
        fixture.git_in(&fixture.root.clone(), &["checkout", "main"]);
        fixture.commit(&["a.txt"], "two\n", "the task's work, squashed");
        fixture.git(&["push", "origin", "main"]);
        let forge = fixture.forge(Some(true));
        let landed = match land_with(&fixture, &forge, MergePolicy::Auto, false) {
            Ok(landed) => landed,
            Err(err) => panic!("a squash-merged task is not an error: {err}"),
        };
        assert!(matches!(landed, Landed::SquashMerged { .. }), "{landed:?}");
        assert_eq!(landed.outcome(), Some(LandOutcome::SquashMerged));
        assert!(forge.calls().is_empty(), "{:?}", forge.calls());
    }

    #[test]
    fn a_branch_with_no_work_of_its_own_has_nothing_to_merge() {
        let fixture = Fixture::new("shprd/r1/T1");
        // The base moves on without the task, which never committed.
        let empty = fixture._temp.path().join("empty");
        let empty_text = empty.display().to_string();
        fixture.git_in(
            &fixture.root.clone(),
            &["worktree", "add", "-b", "x", &empty_text, "main"],
        );
        fixture.git(&["checkout", "main"]);
        fixture.commit(&["b.txt"], "base moved\n", "someone else's work");
        fixture.git(&["push", "origin", "main"]);
        let forge = fixture.forge(Some(true));
        let landed = match land(
            &forge,
            &Git::new(&empty),
            &Landing {
                task: "T1",
                branch: "x",
                base: "main",
                title: "T1: a task",
                body: &fixture.body,
                has_work: false,
            },
            MergePolicy::Auto,
            false,
        ) {
            Ok(landed) => landed,
            Err(err) => panic!("nothing to merge is not an error: {err}"),
        };
        assert!(
            matches!(landed, Landed::NothingToMerge { .. }),
            "{landed:?}"
        );
        assert_eq!(landed.outcome(), Some(LandOutcome::NothingToMerge));
        assert!(forge.calls().is_empty(), "{:?}", forge.calls());
    }

    #[test]
    fn an_open_pull_request_is_refreshed_rather_than_opened_again() {
        let fixture = Fixture::new("shprd/r1/T1");
        let forge = fixture.forge(Some(true));
        forge.add_open(Pr {
            number: 9,
            url: "https://example.invalid/pull/9".to_string(),
            head: "shprd/r1/T1".to_string(),
        });
        forge.set_status(
            9,
            PrStatus {
                approved: false,
                checks_passed: Some(true),
                merged: false,
                head_sha: fixture.head.clone(),
            },
        );
        let landed = match land_with(&fixture, &forge, MergePolicy::Auto, false) {
            Ok(landed) => landed,
            Err(err) => panic!("an open pull request must be reused: {err}"),
        };
        assert_eq!(landed.number(), Some(9));
        assert_eq!(
            forge.calls(),
            vec![
                "find_open shprd/r1/T1".to_string(),
                format!("edit_body 9 {}", fixture.body.display()),
                "status 9".to_string(),
                format!("merge 9 {}", fixture.head),
                "status 9".to_string(),
            ]
        );
    }

    #[test]
    fn a_pull_request_at_another_commit_is_refused() {
        let fixture = Fixture::new("shprd/r1/T1");
        let forge = fixture.forge(Some(true));
        forge.set_status(
            1,
            PrStatus {
                approved: false,
                checks_passed: Some(true),
                merged: false,
                head_sha: "deadbeef".to_string(),
            },
        );
        let err = match land_with(&fixture, &forge, MergePolicy::Auto, false) {
            Ok(landed) => panic!("a stale head must not merge: {landed:?}"),
            Err(err) => err,
        };
        assert!(
            matches!(err, LandError::HeadMoved { ref found, .. } if found == "deadbeef"),
            "{err}"
        );
        assert!(
            !forge.calls().iter().any(|call| call.starts_with("merge")),
            "{:?}",
            forge.calls()
        );
    }

    #[test]
    fn a_merge_the_forge_refuses_is_an_error() {
        let fixture = Fixture::new("shprd/r1/T1");
        let forge = fixture.forge(Some(true));
        forge.refuse_merges();
        let err = match land_with(&fixture, &forge, MergePolicy::Auto, false) {
            Ok(landed) => panic!("a refused merge must not land: {landed:?}"),
            Err(err) => err,
        };
        assert!(matches!(err, LandError::Forge(_)), "{err}");
    }

    #[test]
    fn a_merge_the_forge_does_not_report_settled_is_an_error() {
        let fixture = Fixture::new("shprd/r1/T1");
        let forge = fixture.forge(Some(true));
        // The fake takes the merge and leaves the pull request open: exactly a
        // forge that accepted the call without merging anything.
        forge.silence_merges();
        let err = match land(
            &forge,
            &fixture.runner(),
            &fixture.landing("shprd/r1/T1"),
            MergePolicy::Auto,
            false,
        ) {
            Ok(landed) => panic!("an unsettled merge must not land: {landed:?}"),
            Err(err) => err,
        };
        assert!(matches!(err, LandError::NotMerged { .. }), "{err}");
    }

    /// An attempt whose report, review and review gate are the given ones.
    fn attempt(status: ReportStatus, reviewed: bool, degraded: bool) -> Attempt {
        Attempt {
            n: 1,
            agent: "w1".to_string(),
            model: None,
            worktree: "/tmp/wt".to_string(),
            branch: "shprd/r1/T1".to_string(),
            report: Some(Report {
                status,
                summary: "done".to_string(),
                tests: vec!["cargo test: ok".to_string()],
                changed_paths: Vec::new(),
                commit: None,
                skills_used: Vec::new(),
            }),
            gates: Vec::new(),
            reviews: if reviewed {
                vec![ReviewRequest {
                    reviewer: "c1".to_string(),
                    degraded,
                    seq: 4,
                }]
            } else {
                Vec::new()
            },
        }
    }

    /// The provenance an engine-recorded review verdict carries.
    fn provenance() -> ReviewSource {
        ReviewSource {
            reviewer: "c1".to_string(),
            findings_sha256: "ba7816bf8f01cfea".to_string(),
            request_seq: 4,
        }
    }

    /// The task a landing is asked about, built from folding `extra` onto a
    /// reported attempt.
    fn task(attempt: Attempt, extra: Vec<Event>) -> Task {
        let mut events = vec![
            Event {
                seq: 1,
                at_ms: 1_000,
                actor: Actor::Engine,
                kind: EventKind::RunStarted {
                    run_id: "r1".to_string(),
                    repo: "/repo".to_string(),
                    base_branch: "main".to_string(),
                    integration_branch: "integration/m8".to_string(),
                },
            },
            Event {
                seq: 2,
                at_ms: 2_000,
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
                at_ms: 3_000,
                actor: Actor::Engine,
                kind: EventKind::Spawned {
                    task: "T1".to_string(),
                    attempt: 1,
                    agent: attempt.agent.clone(),
                    model: None,
                    worktree: attempt.worktree.clone(),
                    branch: attempt.branch.clone(),
                },
            },
            Event {
                seq: 4,
                at_ms: 4_000,
                actor: Actor::Engine,
                kind: EventKind::Reported {
                    task: "T1".to_string(),
                    attempt: 1,
                    status: match &attempt.report {
                        Some(report) => report.status,
                        None => ReportStatus::Completed,
                    },
                    summary: "done".to_string(),
                    tests: Vec::new(),
                    changed_paths: Vec::new(),
                    commit: None,
                    skills_used: Vec::new(),
                },
            },
        ];
        if !attempt.reviews.is_empty() {
            events.push(Event {
                seq: 5,
                at_ms: 5_000,
                actor: Actor::Engine,
                kind: EventKind::ReviewRequested {
                    task: "T1".to_string(),
                    attempt: 1,
                    reviewer: "c1".to_string(),
                    degraded: attempt.reviews[0].degraded,
                },
            });
        }
        for (n, gate) in attempt.gates.iter().enumerate() {
            let seq = 6 + u64::try_from(n).unwrap_or(0);
            let kind = if gate.passed {
                EventKind::GatePassed {
                    task: "T1".to_string(),
                    attempt: 1,
                    gate: gate.gate.clone(),
                    findings: gate.findings.clone(),
                    source: gate.source.clone(),
                }
            } else {
                EventKind::GateFailed {
                    task: "T1".to_string(),
                    attempt: 1,
                    gate: gate.gate.clone(),
                    findings: gate.findings.clone(),
                    source: gate.source.clone(),
                }
            };
            events.push(Event {
                seq,
                at_ms: seq * 1_000,
                actor: Actor::Engine,
                kind,
            });
        }
        events.extend(extra);
        match RunState::fold(&events) {
            Ok(state) => match state.tasks.get("T1") {
                Some(task) => task.clone(),
                None => panic!("the folded state must hold T1"),
            },
            Err(err) => panic!("the test log must fold: {err}"),
        }
    }

    /// A passing review gate carrying the engine's provenance.
    fn review_gate(passed: bool, proven: bool) -> GateResult {
        GateResult {
            gate: REVIEW_GATE.to_string(),
            passed,
            findings: Vec::new(),
            source: proven.then(provenance),
            seq: 6,
        }
    }

    #[test]
    fn a_task_that_handed_in_and_was_reviewed_independently_may_land() {
        let mut attempt = attempt(ReportStatus::Completed, true, false);
        attempt.gates.push(review_gate(true, true));
        let task = task(attempt, Vec::new());
        assert_eq!(task.status, TaskStatus::Passed);
        assert_eq!(landing_refusal(&task), None);
    }

    #[test]
    fn a_task_without_an_attempt_may_not_land() {
        let events = [
            Event {
                seq: 1,
                at_ms: 1_000,
                actor: Actor::Engine,
                kind: EventKind::RunStarted {
                    run_id: "r1".to_string(),
                    repo: "/repo".to_string(),
                    base_branch: "main".to_string(),
                    integration_branch: "integration/m8".to_string(),
                },
            },
            Event {
                seq: 2,
                at_ms: 2_000,
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
        let task = match RunState::fold(&events) {
            Ok(state) => match state.tasks.get("T1") {
                Some(task) => task.clone(),
                None => panic!("the task must exist"),
            },
            Err(err) => panic!("the log must fold: {err}"),
        };
        assert_eq!(landing_refusal(&task), Some(LandRefusal::NoAttempt));
    }

    #[test]
    fn a_task_that_did_not_complete_may_not_land() {
        let attempt = attempt(ReportStatus::Blocked, false, false);
        let task = task(attempt, Vec::new());
        assert_eq!(
            landing_refusal(&task),
            Some(LandRefusal::NotCompleted {
                attempt: 1,
                status: ReportStatus::Blocked,
            })
        );
    }

    #[test]
    fn a_task_never_reviewed_may_not_land() {
        let attempt = attempt(ReportStatus::Completed, false, false);
        let task = task(attempt, Vec::new());
        assert_eq!(
            landing_refusal(&task),
            Some(LandRefusal::NoReview { attempt: 1 })
        );
    }

    #[test]
    fn a_degraded_review_may_not_land() {
        let mut attempt = attempt(ReportStatus::Completed, true, true);
        attempt.gates.push(review_gate(true, true));
        let task = task(attempt, Vec::new());
        match landing_refusal(&task) {
            Some(LandRefusal::DegradedReview { reviewers, .. }) => {
                assert_eq!(reviewers, vec!["c1".to_string()]);
            }
            other => panic!("a degraded review is a refusal: {other:?}"),
        }
    }

    #[test]
    fn a_review_gate_without_provenance_may_not_land() {
        let mut attempt = attempt(ReportStatus::Completed, true, false);
        attempt.gates.push(review_gate(true, false));
        let task = task(attempt, Vec::new());
        assert_eq!(
            landing_refusal(&task),
            Some(LandRefusal::UnprovenReview { attempt: 1 })
        );
    }

    #[test]
    fn a_task_with_no_passed_review_gate_may_not_land() {
        let mut attempt = attempt(ReportStatus::Completed, true, false);
        attempt.gates.push(GateResult {
            gate: REVIEW_GATE.to_string(),
            passed: false,
            findings: vec![shprd_core::event::Finding {
                severity: Severity::Major,
                title: "the retry loop loses work".to_string(),
                location: None,
                detail: "a crash drops the batch".to_string(),
            }],
            source: Some(provenance()),
            seq: 6,
        });
        let task = task(attempt, Vec::new());
        assert_eq!(
            landing_refusal(&task),
            Some(LandRefusal::NoPassedReview { attempt: 1 })
        );
    }

    #[test]
    fn a_human_override_of_the_review_gate_may_not_land() {
        let mut attempt = attempt(ReportStatus::Completed, true, false);
        attempt.gates.push(review_gate(true, true));
        let task = task(
            attempt,
            vec![Event {
                seq: 9,
                at_ms: 9_000,
                actor: Actor::Human,
                kind: EventKind::Override {
                    task: "T1".to_string(),
                    rule: REVIEW_GATE.to_string(),
                    reason: "the reviewer was unavailable".to_string(),
                },
            }],
        );
        assert_eq!(
            landing_refusal(&task),
            Some(LandRefusal::ReviewGateOverridden {
                attempt: 1,
                reason: "the reviewer was unavailable".to_string(),
            })
        );
    }
}
