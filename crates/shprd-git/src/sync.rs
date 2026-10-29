//! Bringing the base branch into a run's integration branch.
//!
//! The base moves under a long run — a pull request merged meanwhile — and the
//! integration branch is left behind the branch it proposes to merge into. A
//! merge, not a rebase: a task's commits keep the ids the pull request body
//! lists, which is what makes the body a record of what landed.
//!
//! Only the run's own worktree changes. The base, the user's checkout and every
//! task branch are read and never written, so this needs no terminal and no
//! approval: it is housekeeping inside the run.

use std::path::PathBuf;

use thiserror::Error;

use crate::git::{Git, GitError};
use crate::integrate::IntegrationWorktree;

/// The remote shprd fetches the base from and pushes a run's branch to.
///
/// The same remote `shprd land` pushes to and the janitor deletes from, so a
/// run has one place it talks about itself.
pub const REMOTE: &str = "origin";

/// What a sync did to the integration branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncOutcome {
    /// The integration branch already contains the base's head: nothing moved,
    /// and nothing is recorded.
    UpToDate {
        /// The integration head, unchanged.
        head: String,
    },
    /// A merge commit joined the base into the integration branch.
    Merged {
        /// The base commit that was merged in.
        base_sha: String,
        /// The integration head after the merge.
        int_sha: String,
    },
}

/// Why a sync could not be done.
#[derive(Debug, Error)]
pub enum SyncError {
    /// git failed.
    #[error(transparent)]
    Git(#[from] GitError),
    /// The integration worktree has uncommitted work.
    #[error("integration worktree {path:?} has uncommitted changes")]
    Dirty {
        /// The worktree that is not clean.
        path: PathBuf,
    },
    /// The merge conflicted, and was undone.
    #[error("{base} conflicts with {branch} in {} file(s)", files.len())]
    Conflict {
        /// The base commit that was merged in.
        base: String,
        /// The integration branch.
        branch: String,
        /// The files git reported as unmerged.
        files: Vec<String>,
        /// The worktree a human resolves it in, when they choose to.
        path: PathBuf,
    },
    /// A conflicted merge could not be undone.
    #[error("integration worktree is not back at {expected:?} after a conflicted merge")]
    NotRestored {
        /// The head that should have been restored.
        expected: String,
    },
}

/// Fetches `base` from [`REMOTE`] and merges it into the integration branch.
///
/// The refspec names the remote-tracking ref explicitly: a bare `fetch origin
/// main` only guarantees `FETCH_HEAD`, and merging a stale tracking ref would
/// report a sync that never happened.
///
/// A merge that conflicts is aborted and verified, so a caller that sees
/// [`SyncError::Conflict`] can trust the worktree is exactly where it was.
pub fn sync_base(int: &IntegrationWorktree, base: &str) -> Result<SyncOutcome, SyncError> {
    let git = Git::new(&int.path);
    if !git.is_clean()? {
        return Err(SyncError::Dirty {
            path: int.path.clone(),
        });
    }
    let base_sha = fetch_base(&git, base)?;
    if contains(&git, &base_sha)? {
        return Ok(SyncOutcome::UpToDate {
            head: git.head_sha()?,
        });
    }
    let head = git.head_sha()?;
    let message = format!("Merge {REMOTE}/{base} into {}", int.branch);
    let args = [
        "merge",
        "--no-ff",
        "--no-edit",
        "-m",
        message.as_str(),
        &base_sha,
    ];
    if let Err(err) = git.run(&args) {
        return Err(undo_failed_merge(&git, &head, &base_sha, int, err));
    }
    Ok(SyncOutcome::Merged {
        base_sha,
        int_sha: git.head_sha()?,
    })
}

/// True when `base`'s remote-tracking ref has commits the integration branch
/// does not.
///
/// This is the hint `shprd land` gives instead of syncing: it reads the local
/// remote-tracking ref and never fetches, so a caller that wants the remote's
/// newest answer calls [`refresh_base`] first. Landing must not need the
/// network to tell a human what to do next, so the two are separate.
pub fn base_moved(git: &Git, base: &str) -> Result<bool, GitError> {
    let tracking = format!("refs/remotes/{}", remote_ref(base));
    if git
        .run(&["rev-parse", "--verify", "--quiet", &tracking])
        .is_err()
    {
        // No remote-tracking ref: nothing says the base moved.
        return Ok(false);
    }
    Ok(!contains(git, &tracking)?)
}

/// Fetches `base` into its remote-tracking ref, ignoring a failure.
///
/// `shprd land` calls this before it reads the tracking ref, so a pull request
/// merged since some earlier command last fetched is still noticed. The failure
/// is silent on purpose: offline, or a repository with no `origin`, still has
/// the local ref to read, and a hint must never fail the verb that asked for it.
pub fn refresh_base(git: &Git, base: &str) {
    let _ = fetch_base(git, base);
}

/// Fetches `base` into its remote-tracking ref and returns that commit.
fn fetch_base(git: &Git, base: &str) -> Result<String, GitError> {
    let tracking = format!("refs/remotes/{}", remote_ref(base));
    let refspec = format!("+refs/heads/{base}:{tracking}");
    git.run(&["fetch", REMOTE, &refspec])?;
    git.rev_parse(&tracking)
}

/// The remote-tracking ref of `base`.
fn remote_ref(base: &str) -> String {
    format!("{REMOTE}/{base}")
}

/// True when `rev` is an ancestor of HEAD.
///
/// `merge-base --is-ancestor` exits 1 for "no", which is an answer, not a
/// failure; every other non-zero exit is a real error.
fn contains(git: &Git, rev: &str) -> Result<bool, GitError> {
    match git.run(&["merge-base", "--is-ancestor", rev, "HEAD"]) {
        Ok(_) => Ok(true),
        Err(GitError::Failed { code: Some(1), .. }) => Ok(false),
        Err(err) => Err(err),
    }
}

/// Collects the unmerged files of a failed merge, undoes it, and says why.
///
/// The head and the tree are verified rather than assumed, so a half-merged
/// worktree is reported instead of being left behind as if it were clean.
fn undo_failed_merge(
    git: &Git,
    head: &str,
    base_sha: &str,
    int: &IntegrationWorktree,
    cause: GitError,
) -> SyncError {
    let files = match git.run(&["diff", "--name-only", "-z", "--diff-filter=U"]) {
        Ok(raw) => split_nul(&raw),
        Err(_) => Vec::new(),
    };
    let _ = git.run(&["merge", "--abort"]);
    let restored = git.head_sha().ok().as_deref() == Some(head) && git.is_clean().unwrap_or(false);
    if !restored {
        return SyncError::NotRestored {
            expected: head.to_string(),
        };
    }
    if files.is_empty() {
        // No unmerged path: the merge never started, so the reason is the
        // command's own failure and calling it a conflict would hide it.
        return SyncError::Git(cause);
    }
    SyncError::Conflict {
        base: base_sha.to_string(),
        branch: int.branch.clone(),
        files,
        path: int.path.clone(),
    }
}

/// Splits NUL-separated git output into fields, dropping the empty tail.
fn split_nul(text: &str) -> Vec<String> {
    text.split('\0')
        .filter(|field| !field.is_empty())
        .map(str::to_string)
        .collect()
}
