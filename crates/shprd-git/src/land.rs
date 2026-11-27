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
//! Whether a task *may* land is not decided here: `shprd_core::rules::ready_to_land`
//! answers that from the log (a passing hand-in with its checks, and an
//! independent review the engine recorded), and the landing verb asks it before
//! it calls [`land`].
//!
//! The old run-level integration branch is not written here. Its code
//! ([`crate::integrate`]) still compiles and still lands runs; M8.3 only adds
//! this path.
//!
//! A repository with no remote, or with no forge to drive, lands through
//! [`land_local`] instead: the same checks and the same merge policy, but the
//! task's branch is merged into the run's integration branch inside shprd's own
//! integration worktree. The user's checkout is never touched. The merge is
//! pushed afterwards only to a remote that is not GitHub, and only as a courtesy:
//! a refusal leaves the merge standing.

use std::path::Path;

use thiserror::Error;

use shprd_core::config::MergePolicy;
use shprd_core::event::LandOutcome;

use crate::forge::{Forge, ForgeError, Pr};
use crate::git::{Git, GitError};
use crate::integrate::IntegrationWorktree;
use crate::worktree::{remote_hosts, remote_of};

/// The remote named when a repository has none to pick from: every failure
/// then says which remote git could not reach.
const DEFAULT_REMOTE: &str = "origin";

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

/// One task's landing when the repository has no forge.
///
/// It names the same branch and the same work as [`Landing`], but the target is
/// the run's integration branch in shprd's own worktree rather than a pull
/// request on a remote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalLanding<'a> {
    /// The task's id, as the log names it.
    pub task: &'a str,
    /// The branch the task's attempt commits on.
    pub branch: &'a str,
    /// The run's integration branch: what the task's work is merged into.
    pub base: &'a str,
    /// Whether the log says the branch has work of its own, as [`Landing::has_work`]
    /// says it.
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

    /// The same advice for a local landing, which has no pull request to
    /// approve on a forge.
    pub fn local_advice(self, task: &str) -> String {
        match self {
            WaitReason::Approval => {
                format!("waiting for your approval: run `shprd approve {task}`")
            }
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

/// What a local landing did.
///
/// It mirrors [`Landed`] without the pull request: a repository with no forge
/// has no pull request, so the integration branch is the only thing that moved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalLanded {
    /// The task's branch was merged into the integration branch.
    Merged {
        /// The integration branch's head after the merge.
        head: String,
    },
    /// The branch's own commits were already in the integration branch.
    AlreadyMerged {
        /// The branch tip that was looked at.
        head: String,
    },
    /// The branch's work is in the integration branch as other commits.
    SquashMerged {
        /// The integration branch holding the work.
        base: String,
        /// The branch tip that was looked at.
        head: String,
    },
    /// The branch held no work of its own beyond its integration branch.
    NothingToMerge {
        /// The integration branch it had nothing to add to.
        base: String,
        /// The branch tip that was looked at.
        head: String,
    },
    /// The policy waits before anything is merged.
    Waiting {
        /// What the landing waits for.
        reason: WaitReason,
        /// The branch tip that waits.
        head: String,
    },
}

impl LocalLanded {
    /// The outcome to record, or `None` while the landing waits.
    pub fn outcome(&self) -> Option<LandOutcome> {
        match self {
            LocalLanded::Merged { .. } => Some(LandOutcome::Merged),
            LocalLanded::AlreadyMerged { .. } => Some(LandOutcome::AlreadyMerged),
            LocalLanded::SquashMerged { .. } => Some(LandOutcome::SquashMerged),
            LocalLanded::NothingToMerge { .. } => Some(LandOutcome::NothingToMerge),
            LocalLanded::Waiting { .. } => None,
        }
    }

    /// The branch tip this landing looked at.
    pub fn head(&self) -> &str {
        match self {
            LocalLanded::Merged { head }
            | LocalLanded::AlreadyMerged { head }
            | LocalLanded::SquashMerged { head, .. }
            | LocalLanded::NothingToMerge { head, .. }
            | LocalLanded::Waiting { head, .. } => head,
        }
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
    /// The integration worktree has uncommitted work, so merging there would
    /// mix someone else's changes into the task's landing.
    #[error("integration worktree {path:?} has uncommitted changes")]
    Dirty {
        /// The worktree that is not clean.
        path: std::path::PathBuf,
    },
    /// A local merge conflicted. The integration worktree was put back on the
    /// head it had before the attempt.
    #[error("merging {commit} into the integration branch conflicted in {} file(s)", files.len())]
    Conflict {
        /// The commit that could not be merged.
        commit: String,
        /// The files git reported as unmerged.
        files: Vec<String>,
        /// The pull request that carries the stale branch, when the conflict
        /// was found on a forge landing; a local landing has none.
        pr: Option<u64>,
    },
    /// A failed merge could not be undone, so the integration branch is not
    /// where the caller may assume it is.
    #[error("integration worktree is not back at {expected:?} after a failed merge")]
    NotRestored {
        /// The head that should have been restored.
        expected: String,
    },
}

impl LandError {
    /// The failure's real cause for a log or a record: the command, its exit
    /// code and the last lines of its stderr, never shown to a person.
    pub fn cause(&self) -> String {
        tail_lines(&self.to_string())
    }

    /// The failure as one plain sentence a person can read: no command line, no
    /// forge advice, no "run X".
    pub fn plain(&self, task: &str) -> String {
        if matches!(self, LandError::Conflict { .. }) {
            return conflict_words(task);
        }
        let reason = match self {
            LandError::Git(_) => String::from("git could not finish the merge"),
            LandError::Forge(ForgeError::Failed { stderr, .. }) => forge_reason(stderr),
            LandError::Forge(_) => String::from("the code host could not be reached"),
            LandError::HeadMoved { .. } => {
                String::from("the task changed after it was accepted, so it needs accepting again")
            }
            LandError::NotMerged { .. } => String::from("the code host did not confirm the merge"),
            LandError::ChecksFailed { .. } => String::from("its checks failed"),
            LandError::Dirty { .. } => {
                String::from("the integration branch has changes nobody saved")
            }
            LandError::Conflict { .. } => conflict_words(task),
            LandError::NotRestored { .. } => {
                String::from("a failed merge could not be undone cleanly")
            }
        };
        format!("Couldn't merge {task}: {reason}")
    }
}

/// What a person is told when a task's branch clashes with work that landed
/// after it was cut: the cause, and that it is already being put right.
pub fn conflict_words(task: &str) -> String {
    format!(
        "{task} clashed with work that landed after it; it is being redone on top of the latest"
    )
}

/// How many trailing lines of a failure's own text a record keeps.
const CAUSE_LINES: usize = 20;

/// The last [`CAUSE_LINES`] lines of `text`: what a record keeps of a command's
/// complaint, so one failure cannot fill a file.
pub fn tail_lines(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let from = lines.len().saturating_sub(CAUSE_LINES);
    lines[from..].join("\n")
}

/// The finding a clashing landing records on the failed `integrate` gate: the
/// part of the new attempt's brief that says to carry the earlier commits over
/// and resolve these files. `files` empty means the forge said so without
/// naming any.
pub fn conflict_finding(base: &str, files: &[String]) -> shprd_core::event::Finding {
    let which = if files.is_empty() {
        String::from("the files it touches")
    } else {
        files.join(", ")
    };
    shprd_core::event::Finding {
        severity: shprd_core::event::Severity::Major,
        title: String::from(
            "clashed with work that landed after it; it is being redone on top of the latest",
        ),
        location: files.first().cloned(),
        detail: format!(
            "clashes with {base} in {which}. Bring the previous attempt's commits onto the \
             latest {base} (merge it, or cherry-pick the archive named above), resolve \
             keeping both sides' work, hand in again"
        ),
    }
}

/// What a forge client's own complaint comes to, in plain words.
///
/// The forge's own error is never swallowed: a complaint shprd cannot name
/// (a missing repository, a read-only account) is shown as GitHub said it.
fn forge_reason(stderr: &str) -> String {
    let said = stderr.to_ascii_lowercase();
    let real = forge_words(stderr);
    if said.contains("known github host") || said.contains("not a github") {
        String::from("this repository has no GitHub remote")
    } else if said.contains("auth") || said.contains("log in") || said.contains("login") {
        if real.is_empty() {
            String::from("GitHub is not signed in on this machine")
        } else {
            format!("GitHub is not signed in on this machine (GitHub said: {real})")
        }
    } else if real.is_empty() {
        String::from("GitHub did not accept it and gave no reason")
    } else {
        format!("GitHub said: {real}")
    }
}

/// The most telling line of a forge client's stderr: the last non-empty one,
/// with the `gh:` prefix, backticks and runs of spaces taken out, cut at 240
/// characters. Empty when it said nothing.
fn forge_words(stderr: &str) -> String {
    let line = stderr
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .unwrap_or_default();
    let line = line.strip_prefix("gh:").unwrap_or(line).replace('`', "");
    let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
    match line.char_indices().nth(240) {
        Some((at, _)) => format!("{}...", &line[..at]),
        None => line,
    }
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
        None => {
            // A branch that clashes with what landed after it can never merge:
            // say so, with the files, rather than let the forge refuse it.
            if let Some(files) = trial_conflicts(git, &base, &head) {
                return Err(LandError::Conflict {
                    commit: head,
                    files,
                    pr: Some(pr.number),
                });
            }
            let number = pr.number;
            match merge_now(forge, pr, head.clone()) {
                Err(LandError::Forge(refused)) => {
                    // The forge works out "mergeable" lazily, so a refusal is
                    // the moment it may finally know.
                    match clash(forge, git, &base, number, &head) {
                        Some(files) => Err(LandError::Conflict {
                            commit: head,
                            files,
                            pr: Some(number),
                        }),
                        None => Err(LandError::Forge(refused)),
                    }
                }
                other => other,
            }
        }
    }
}

/// The files `head` would conflict in against `base`, when it would: a trial
/// merge git runs without a worktree, or else the forge's own word that the
/// pull request conflicts (then no file is known). `None` means it merges
/// cleanly, or that nobody can say.
fn clash(forge: &dyn Forge, git: &Git, base: &str, number: u64, head: &str) -> Option<Vec<String>> {
    trial_conflicts(git, base, head)
        .or_else(|| (forge.mergeable(number).ok().flatten() == Some(false)).then(Vec::new))
}

/// The ref `branch` would clash with if it landed on `base` now, and the files
/// it would conflict in, or `None` when it merges cleanly or git cannot tell.
///
/// It is asked before a review is requested, so a task that clashes with work
/// that landed after it was cut goes back at once, before any review round or
/// Accept is spent on it. The base is the freshest one known: the local ref and
/// `origin/<base>` (fetched first, best effort) are both tried, so a landing
/// merged on the forge is not missed. The ref is the one the worker has to
/// bring its work onto: `origin/<base>` when only the forge holds the landing.
pub fn clash_before_review(
    repo: &std::path::Path,
    base: &str,
    branch: &str,
) -> Option<(String, Vec<String>)> {
    let git = Git::new(repo);
    crate::sync::refresh_base(&git, base);
    let tracking = format!("refs/remotes/{}", crate::sync::remote_ref(base));
    let mut bases = vec![base.to_string()];
    if git
        .run(&[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{tracking}^{{commit}}"),
        ])
        .is_ok()
    {
        bases.push(tracking);
    }
    bases.into_iter().find_map(|candidate| {
        let files = trial_conflicts(&git, &candidate, branch)?;
        let named = candidate
            .strip_prefix("refs/remotes/")
            .map_or_else(|| candidate.clone(), str::to_string);
        Some((named, files))
    })
}

/// The files a merge of `head` into `base` would leave unmerged, found with
/// `git merge-tree`, which writes objects but touches no worktree and no ref.
/// `None` when the merge is clean or git could not tell.
fn trial_conflicts(git: &Git, base: &str, head: &str) -> Option<Vec<String>> {
    let mut command = std::process::Command::new("git");
    command
        .args(["merge-tree", "--write-tree", "--name-only", base, head])
        .current_dir(git.dir())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null());
    let output = shprd_core::spawn::output(&mut command).ok()?;
    if output.status.code() != Some(1) {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    // The first line is the tree; the unmerged names follow until a blank line.
    Some(
        text.lines()
            .skip(1)
            .take_while(|line| !line.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

/// Lands one task into its integration branch, with no forge involved.
///
/// The worktree is the run's own integration worktree, already checked out on
/// `landing.base`; the task's branch is merged into it there, never in the
/// user's checkout. The decision is the same one [`land`] makes: work the base
/// already holds is reported as such, `ask` waits for an approval at this head,
/// `checks` waits for the task's own checks report, and a landing the policy
/// allows is merged as a fast-forward when it can be, else as a merge commit.
///
/// `checks_passed` is the task's local checks report: a repository with no
/// forge has no CI to ask, and the hand-in's checks are the only word there is.
///
/// A conflicting merge is undone before it returns: the worktree is left on the
/// head it started from, and the error names the files git could not merge.
///
/// # Errors
///
/// Fails when git cannot be driven, when the integration worktree is dirty,
/// when the merge conflicts, or when a failed merge cannot be undone.
pub fn land_local(
    int: &IntegrationWorktree,
    landing: &LocalLanding<'_>,
    policy: MergePolicy,
    approved: bool,
    checks_passed: bool,
) -> Result<LocalLanded, LandError> {
    let git = Git::new(&int.path);
    if !git.is_clean()? {
        return Err(LandError::Dirty {
            path: int.path.clone(),
        });
    }
    let head = git.rev_parse(landing.branch)?;
    let base = git.head_sha()?;
    if let Some(reached) = already_landed(&git, &head, &base, landing.has_work)? {
        return Ok(match reached {
            Reached::AlreadyMerged => LocalLanded::AlreadyMerged { head },
            Reached::SquashMerged => LocalLanded::SquashMerged {
                base: landing.base.to_string(),
                head,
            },
            Reached::NothingToMerge => LocalLanded::NothingToMerge {
                base: landing.base.to_string(),
                head,
            },
        });
    }
    let reason = match policy {
        MergePolicy::Auto => None,
        MergePolicy::Ask if approved => None,
        MergePolicy::Ask => Some(WaitReason::Approval),
        MergePolicy::Checks if checks_passed => None,
        MergePolicy::Checks => Some(WaitReason::Checks),
    };
    if let Some(reason) = reason {
        return Ok(LocalLanded::Waiting { reason, head });
    }
    let merged = merge_local(&git, &base, &head, landing)?;
    push_integration(&git, landing.base);
    Ok(LocalLanded::Merged { head: merged })
}

/// Pushes the integration branch to the repository's remote, when it has one.
///
/// Best effort and never forced: a repository with no remote has nothing to
/// push to, and a remote that refuses (a diverged branch, a hook, no network)
/// leaves the merge where it is, because the merge is already the person's
/// decision and is recorded locally. Nothing here is an error.
fn push_integration(git: &Git, branch: &str) {
    let Some(remote) = remote_of(git) else {
        return;
    };
    // A GitHub remote without a signed-in client lands locally too, and the
    // person never agreed to put the integration branch on GitHub that way.
    let to_github = git
        .run(&["remote", "get-url", &remote])
        .ok()
        .and_then(|url| crate::worktree::host_of_url(&url))
        .is_some_and(|host| host.contains("github"));
    if to_github {
        return;
    }
    let _ = git.run(&["push", &remote, &format!("HEAD:refs/heads/{branch}")]);
}

/// Whether the repository has a remote at a GitHub host, or at a host `gh` is
/// signed in for, where a landing must go through the forge whether or not
/// `gh` answers right now.
///
/// A landing that cannot reach the forge fails; it never falls back to merging
/// in a local worktree, which moves the local integration ref apart from the
/// remote's.
pub fn forge_expected(git: &Git) -> bool {
    forge_expected_with(git, |host| gh_authed(git.dir(), host))
}

/// [`forge_expected`] with the forge-client check given: a GitHub remote always
/// counts; a remote at any other host counts when `authed` says a forge client
/// can drive that host. `authed` is never asked for a GitHub host.
pub fn forge_expected_with(git: &Git, authed: impl Fn(&str) -> bool) -> bool {
    // The configured URL, not `get-url`: a `url.<base>.insteadOf` rewrite is
    // a transport detail, and the remote still names GitHub.
    let Ok(names) = git.run(&["remote"]) else {
        return false;
    };
    let hosts: Vec<String> = names
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .filter_map(|name| {
            git.run(&["config", "--get", &format!("remote.{name}.url")])
                .ok()
        })
        .filter_map(|url| crate::worktree::host_of_url(&url))
        .map(|host| host.to_ascii_lowercase())
        .collect();
    hosts.iter().any(|host| host.contains("github")) || hosts.iter().any(|host| authed(host))
}

/// Whether `gh` is installed and signed in for `host`, asked in `dir`.
///
/// `gh auth status` answers from the local configuration, so it makes no
/// network call; anything it cannot answer counts as "no forge".
pub fn gh_authed(dir: &Path, host: &str) -> bool {
    std::process::Command::new("gh")
        .args(["auth", "status", "--hostname", host])
        .current_dir(dir)
        .env("GH_PROMPT_DISABLED", "1")
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Whether landing in `git`'s repository goes through a forge's pull request.
///
/// Only a remote at a host `authed` says a forge client can drive counts:
/// a repository with no remote, a remote that is a local path or a bare
/// repository, or one at a host the forge client does not know, lands locally.
/// `authed` is asked once per distinct remote host, never for a hostless remote.
pub fn uses_forge(git: &Git, authed: impl Fn(&str) -> bool) -> bool {
    remote_hosts(git).iter().any(|host| authed(host))
}

/// Merges `head` into the integration worktree, fast-forwarding when it can.
///
/// A branch cut from the integration branch merges as a fast-forward, which is
/// the ordinary case and leaves the branch's own commits as they were made. A
/// branch that has moved apart gets a merge commit, so neither side's history is
/// rewritten. A conflict is undone and reported, never left in the worktree.
fn merge_local(
    git: &Git,
    base: &str,
    head: &str,
    landing: &LocalLanding<'_>,
) -> Result<String, LandError> {
    let can_fast_forward = git
        .run(&["merge-base", "--is-ancestor", base, head])
        .is_ok();
    let message = format!(
        "{}: merge {} into {}",
        landing.task, landing.branch, landing.base
    );
    let result = if can_fast_forward {
        git.run(&["merge", "--ff-only", head])
    } else {
        git.run(&["merge", "--no-ff", "--no-edit", "-m", &message, head])
    };
    if result.is_err() {
        return Err(undo_failed_merge(git, base, head));
    }
    Ok(git.head_sha()?)
}

/// Collects the unmerged files of a failed merge, undoes it, and returns what
/// went wrong.
///
/// The worktree is put back on `base` and the result is verified rather than
/// assumed, so a caller that sees [`LandError::Conflict`] can trust the
/// integration branch is where it was.
fn undo_failed_merge(git: &Git, base: &str, commit: &str) -> LandError {
    let files = match git.run(&["diff", "--name-only", "-z", "--diff-filter=U"]) {
        Ok(raw) => split_nul(&raw),
        Err(_) => Vec::new(),
    };
    // `--abort` restores what git recorded for itself; the reset that follows
    // is authoritative, and both are needed because a merge that failed before
    // starting leaves no state to abort.
    let _ = git.run(&["merge", "--abort"]);
    let restored = git.run(&["reset", "--hard", base]).is_ok()
        && git.head_sha().ok().as_deref() == Some(base)
        && git.is_clean().unwrap_or(false);
    if !restored {
        return LandError::NotRestored {
            expected: base.to_string(),
        };
    }
    LandError::Conflict {
        commit: commit.to_string(),
        files,
        pr: None,
    }
}

/// Splits NUL-separated git output into fields, dropping the empty tail.
fn split_nul(text: &str) -> Vec<String> {
    text.split('\0')
        .filter(|field| !field.is_empty())
        .map(str::to_string)
        .collect()
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
    let remote = remote_of(git).unwrap_or_else(|| DEFAULT_REMOTE.to_string());
    let tracking = format!("refs/remotes/{remote}/{base}");
    let fetch = git.run(&[
        "fetch",
        "--quiet",
        "--no-tags",
        &remote,
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
    let remote = remote_of(git).unwrap_or_else(|| DEFAULT_REMOTE.to_string());
    let there = git
        .run(&["ls-remote", &remote, &format!("refs/heads/{branch}")])
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
        &remote,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::{FakeForge, PrStatus};
    use crate::worktree::Layout;
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

    /// A repository with no remote, an `integration/m9` branch, one task branch
    /// cut from it, and shprd's own integration worktree.
    struct LocalFixture {
        /// Keeps the temp directory alive.
        _temp: tempfile::TempDir,
        /// The user's checkout.
        root: PathBuf,
        /// The task's worktree.
        task: PathBuf,
        /// The task branch's head.
        task_head: String,
        /// The integration worktree.
        int: IntegrationWorktree,
        /// The seed commit `main` and `integration/m9` started from.
        seed: String,
    }

    impl LocalFixture {
        /// Builds the fixture, with `task_change` written on the task branch.
        fn new(task_change: &str) -> LocalFixture {
            let temp = match tempfile::tempdir() {
                Ok(temp) => temp,
                Err(err) => panic!("tempdir: {err}"),
            };
            let base = temp.path().to_path_buf();
            let root = base.join("repo");
            let home = base.join("home");
            let task = base.join("wt");
            if let Err(err) = std::fs::create_dir_all(&root) {
                panic!("mkdir {}: {err}", root.display());
            }
            let fixture = LocalFixture {
                _temp: temp,
                root: root.clone(),
                task,
                task_head: String::new(),
                int: IntegrationWorktree {
                    path: base.join("unused"),
                    branch: String::new(),
                    created: false,
                },
                seed: String::new(),
            };
            fixture.git(&["init", "-q", "-b", "main"]);
            fixture.git(&["config", "user.name", "shprd tests"]);
            fixture.git(&["config", "user.email", "tests@example.invalid"]);
            fixture.git(&["config", "commit.gpgsign", "false"]);
            fixture.commit(&fixture.root.clone(), "a.txt", "one\n", "seed");
            fixture.git(&["branch", "integration/m9"]);
            let seed = fixture.rev(&fixture.root, "main");
            let path = fixture.task.clone();
            let path_text = path.display().to_string();
            fixture.git(&[
                "worktree",
                "add",
                "-b",
                "shprd/r1/T1",
                &path_text,
                "integration/m9",
            ]);
            fixture.commit(&path, "a.txt", task_change, "the task's work");
            let layout = Layout::default_for_home(&home);
            let int = match crate::integrate::open_integration_branch(
                &root,
                &layout,
                "repo",
                "r1",
                "integration/m9",
                "main",
            ) {
                Ok(int) => int,
                Err(err) => panic!("opening the integration worktree: {err}"),
            };
            let mut fixture = fixture;
            fixture.task_head = fixture.rev(&fixture.task.clone(), "HEAD");
            fixture.int = int;
            fixture.seed = seed;
            fixture
        }

        /// Runs git in the repository, asserting success.
        fn git(&self, args: &[&str]) -> String {
            self.git_in(&self.root.clone(), args)
        }

        /// Runs git in `dir`, asserting success.
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

        /// Resolves `rev` in `dir`.
        fn rev(&self, dir: &Path, rev: &str) -> String {
            self.git_in(dir, &["rev-parse", rev])
        }

        /// Commits `body` to `file` in `dir`.
        fn commit(&self, dir: &Path, file: &str, body: &str, message: &str) {
            let path = dir.join(file);
            if let Err(err) = std::fs::write(&path, body) {
                panic!("write {}: {err}", path.display());
            }
            self.git_in(dir, &["add", "--", file]);
            self.git_in(dir, &["commit", "-q", "-m", message]);
        }

        /// A landing of `branch` into `integration/m9`.
        fn landing<'a>(&'a self, branch: &'a str) -> LocalLanding<'a> {
            LocalLanding {
                task: "T1",
                branch,
                base: "integration/m9",
                has_work: true,
            }
        }

        /// The integration branch's head in the repository.
        fn integration_head(&self) -> String {
            self.rev(&self.root.clone(), "refs/heads/integration/m9")
        }
    }

    #[test]
    fn a_local_auto_landing_fast_forwards_the_integration_branch() {
        let fixture = LocalFixture::new("the task's work\n");
        let landed = match land_local(
            &fixture.int,
            &fixture.landing("shprd/r1/T1"),
            MergePolicy::Auto,
            false,
            true,
        ) {
            Ok(landed) => landed,
            Err(err) => panic!("auto must land locally: {err}"),
        };
        assert_eq!(
            landed,
            LocalLanded::Merged {
                head: fixture.task_head.clone(),
            }
        );
        assert_eq!(fixture.integration_head(), fixture.task_head);
        assert_eq!(
            fixture.git_in(&fixture.int.path, &["rev-parse", "--abbrev-ref", "HEAD"]),
            "integration/m9",
            "the merge happened in shprd's own integration worktree"
        );
        assert_eq!(
            fixture.rev(&fixture.root.clone(), "HEAD"),
            fixture.seed,
            "the user's checkout never moved"
        );
        assert_eq!(
            fixture.git_in(&fixture.root.clone(), &["status", "--porcelain"]),
            "",
            "the user's checkout stayed clean"
        );
    }

    #[test]
    fn a_local_ask_waits_and_merges_once_approved() {
        let fixture = LocalFixture::new("the task's work\n");
        let waiting = match land_local(
            &fixture.int,
            &fixture.landing("shprd/r1/T1"),
            MergePolicy::Ask,
            false,
            true,
        ) {
            Ok(landed) => landed,
            Err(err) => panic!("ask waits, it does not fail: {err}"),
        };
        assert_eq!(
            waiting,
            LocalLanded::Waiting {
                reason: WaitReason::Approval,
                head: fixture.task_head.clone(),
            }
        );
        assert!(waiting.outcome().is_none());
        assert_eq!(fixture.integration_head(), fixture.seed);

        let merged = match land_local(
            &fixture.int,
            &fixture.landing("shprd/r1/T1"),
            MergePolicy::Ask,
            true,
            true,
        ) {
            Ok(landed) => landed,
            Err(err) => panic!("an approved landing must merge: {err}"),
        };
        assert!(matches!(merged, LocalLanded::Merged { .. }), "{merged:?}");
        assert_eq!(fixture.integration_head(), fixture.task_head);
    }

    #[test]
    fn a_local_checks_landing_reads_the_hand_ins_report() {
        let fixture = LocalFixture::new("the task's work\n");
        let waiting = match land_local(
            &fixture.int,
            &fixture.landing("shprd/r1/T1"),
            MergePolicy::Checks,
            false,
            false,
        ) {
            Ok(landed) => landed,
            Err(err) => panic!("checks waits, it does not fail: {err}"),
        };
        assert!(matches!(
            waiting,
            LocalLanded::Waiting {
                reason: WaitReason::Checks,
                ..
            }
        ));
        assert_eq!(fixture.integration_head(), fixture.seed);

        let merged = match land_local(
            &fixture.int,
            &fixture.landing("shprd/r1/T1"),
            MergePolicy::Checks,
            false,
            true,
        ) {
            Ok(landed) => landed,
            Err(err) => panic!("passing local checks must land: {err}"),
        };
        assert!(matches!(merged, LocalLanded::Merged { .. }), "{merged:?}");
    }

    #[test]
    fn a_conflicting_local_merge_is_undone() {
        let fixture = LocalFixture::new("the task's work\n");
        // The first task lands, so the integration branch now holds its change.
        match land_local(
            &fixture.int,
            &fixture.landing("shprd/r1/T1"),
            MergePolicy::Auto,
            false,
            true,
        ) {
            Ok(_) => {}
            Err(err) => panic!("the first landing must work: {err}"),
        }
        let before = fixture.rev(&fixture.int.path, "HEAD");
        // A second branch cut from the old integration head writes the same
        // file differently: merging it must conflict.
        let other = fixture._temp.path().join("other");
        let other_text = other.display().to_string();
        fixture.git(&[
            "worktree",
            "add",
            "-b",
            "shprd/r1/T2",
            &other_text,
            &fixture.seed,
        ]);
        fixture.commit(&other, "a.txt", "someone else's work\n", "the other task");
        let other_head = fixture.rev(&other, "HEAD");
        let landing = LocalLanding {
            task: "T2",
            branch: "shprd/r1/T2",
            base: "integration/m9",
            has_work: true,
        };
        let err = match land_local(&fixture.int, &landing, MergePolicy::Auto, false, true) {
            Ok(landed) => panic!("a conflicting merge must not land: {landed:?}"),
            Err(err) => err,
        };
        match &err {
            LandError::Conflict { files, .. } => {
                assert_eq!(files, &vec!["a.txt".to_string()]);
            }
            other => panic!("expected a conflict, got: {other}"),
        }
        assert_eq!(
            fixture.rev(&fixture.int.path, "HEAD"),
            before,
            "the integration branch is back where it was"
        );
        assert_eq!(
            fixture.git_in(&fixture.int.path, &["status", "--porcelain"]),
            "",
            "the integration worktree is clean after the undo"
        );
        assert_eq!(
            fixture.rev(&fixture.root.clone(), "refs/heads/shprd/r1/T2"),
            other_head,
            "the task's branch was never touched"
        );
    }

    #[test]
    fn a_repo_with_no_remote_uses_no_forge() {
        let fixture = LocalFixture::new("the task's work\n");
        let git = Git::new(&fixture.root);
        assert!(!uses_forge(&git, |host| panic!("asked about {host}")));
    }

    #[test]
    fn a_local_bare_remote_uses_no_forge_and_never_asks_one() {
        let fixture = Fixture::new("shprd/r1/T1");
        let git = Git::new(&fixture.root);
        assert!(!uses_forge(&git, |host| panic!("asked about {host}")));
    }

    #[test]
    fn a_non_github_url_uses_no_forge() {
        let fixture = Fixture::new("shprd/r1/T1");
        let git = Git::new(&fixture.root);
        fixture.git(&["remote", "set-url", "origin", "git@git.example.org:o/r.git"]);
        let asked = std::cell::RefCell::new(Vec::new());
        let forge = uses_forge(&git, |host| {
            asked.borrow_mut().push(host.to_string());
            false
        });
        assert!(!forge);
        assert_eq!(asked.into_inner(), vec!["git.example.org".to_string()]);
    }

    #[test]
    fn a_github_remote_keeps_the_pull_request_path() {
        let fixture = Fixture::new("shprd/r1/T1");
        let git = Git::new(&fixture.root);
        fixture.git(&["remote", "set-url", "origin", "https://github.com/o/r.git"]);
        assert!(uses_forge(&git, |host| host == "github.com"));
        // Not signed in: nothing can drive it, so it lands locally.
        assert!(!uses_forge(&git, |_| false));
    }

    #[test]
    fn remote_urls_are_read_for_their_host() {
        use crate::worktree::host_of_url;
        assert_eq!(
            host_of_url("https://GitHub.com/o/r"),
            Some("github.com".into())
        );
        assert_eq!(
            host_of_url("ssh://git@ghe.corp:2222/o/r"),
            Some("ghe.corp".into())
        );
        assert_eq!(
            host_of_url("git@github.com:o/r.git"),
            Some("github.com".into())
        );
        assert_eq!(host_of_url("/Users/x/scratchpad-remote.git"), None);
        assert_eq!(host_of_url("../remote.git"), None);
        assert_eq!(host_of_url("file:///tmp/remote.git"), None);
        assert_eq!(host_of_url("C:\\repos\\r"), None);
    }

    #[test]
    fn a_land_failure_reads_as_one_plain_sentence() {
        let raw = LandError::Forge(ForgeError::Failed {
            args: vec!["pr".into(), "list".into(), "--head".into(), "x".into()],
            code: Some(1),
            stderr: "none of the git remotes configured for this repository point to a known GitHub host. To tell gh about a new GitHub host, please use `gh auth login`".into(),
        });
        let line = raw.plain("T1");
        assert_eq!(
            line,
            "Couldn't merge T1: this repository has no GitHub remote"
        );
        let conflict = LandError::Conflict {
            commit: "abc".into(),
            files: vec!["a".into(), "b".into()],
            pr: None,
        };
        assert_eq!(
            conflict.plain("T2"),
            "T2 clashed with work that landed after it; it is being redone on top of the latest"
        );
        for text in [line, conflict.plain("T2")] {
            assert!(
                !text.contains('`') && !text.contains("gh ") && !text.contains("run "),
                "{text}"
            );
        }
    }

    #[test]
    fn a_land_failure_shows_the_forges_own_words() {
        for said in [
            "GraphQL: Could not resolve to a Repository with the name 'o/r'. (repository)",
            "pull request create failed: GraphQL: o (createPullRequest)\nmust be a collaborator (createPullRequest)",
        ] {
            let line = LandError::Forge(ForgeError::Failed {
                args: vec!["pr".into(), "create".into()],
                code: Some(1),
                stderr: said.into(),
            })
            .plain("T1");
            let last = said.lines().last().unwrap_or_default();
            assert!(line.contains(last), "{line}");
            assert!(!line.contains("did not accept it"), "{line}");
        }
        let silent = LandError::Forge(ForgeError::Failed {
            args: vec![],
            code: Some(1),
            stderr: String::new(),
        })
        .plain("T1");
        assert!(silent.contains("gave no reason"), "{silent}");
    }

    #[test]
    fn an_accepted_local_landing_is_pushed_to_a_remote_that_takes_it() {
        let fixture = LocalFixture::new("the task's work\n");
        let bare = fixture
            .root
            .parent()
            .map(|p| p.join("remote.git"))
            .unwrap_or_default();
        fixture.git_in(
            &fixture.root,
            &["init", "--bare", "-q", bare.to_str().unwrap_or("")],
        );
        fixture.git_in(
            &fixture.root,
            &["remote", "add", "origin", bare.to_str().unwrap_or("")],
        );
        match land_local(
            &fixture.int,
            &fixture.landing("shprd/r1/T1"),
            MergePolicy::Ask,
            true,
            true,
        ) {
            Ok(LocalLanded::Merged { .. }) => {}
            other => panic!("expected a merge, got {other:?}"),
        }
        let there = fixture.git_in(&bare, &["rev-parse", "integration/m9"]);
        assert_eq!(there, fixture.task_head);
    }

    #[test]
    fn a_remote_that_refuses_the_push_does_not_undo_the_merge() {
        let fixture = LocalFixture::new("the task's work\n");
        fixture.git_in(
            &fixture.root,
            &["remote", "add", "origin", "/nonexistent/remote.git"],
        );
        match land_local(
            &fixture.int,
            &fixture.landing("shprd/r1/T1"),
            MergePolicy::Ask,
            true,
            true,
        ) {
            Ok(LocalLanded::Merged { .. }) => {}
            other => panic!("expected a merge, got {other:?}"),
        }
        assert_eq!(fixture.integration_head(), fixture.task_head);
    }
}
