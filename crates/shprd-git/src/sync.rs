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

/// The freshest commit of `base` a new task worktree should be cut from.
///
/// `base`'s remote-tracking ref is fetched first (best effort). When it holds
/// commits the local `base` does not, and the local ref is behind it rather
/// than apart from it, the tracking ref's commit is the synced tip: a task
/// redone after a landing merged on the forge starts from that landing, not
/// from a stale local ref. Otherwise `base` itself is the answer. The local ref
/// is never moved.
pub fn freshest_base(git: &Git, base: &str) -> String {
    refresh_base(git, base);
    let tracking = format!("refs/remotes/{}", remote_ref(base));
    let (Ok(local), Ok(remote)) = (git.rev_parse(base), git.rev_parse(&tracking)) else {
        return base.to_string();
    };
    if local == remote {
        return base.to_string();
    }
    match git.run(&["merge-base", "--is-ancestor", &local, &remote]) {
        Ok(_) => remote,
        Err(_) => base.to_string(),
    }
}

/// The commit a task branch was cut from: the start of the branch's own change.
///
/// Diffing a task against this commit gives only what the task did. The local
/// `base` ref is not enough on its own: a landing made through a pull request
/// moves `origin/<base>` and leaves the local ref where it was, so a branch cut
/// after that landing shares a *newer* commit with `origin/<base>` than with the
/// stale local ref, and diffing from the stale merge base would show the landed
/// work as the task's own.
///
/// So the merge base is taken against both the local `base` and its
/// remote-tracking ref, and the nearer one (the one the other's merge base is an
/// ancestor of) wins. A candidate that already holds the branch's tip is set
/// aside while another does not: once the task has merged on the forge,
/// `origin/<base>` contains it and would answer with the tip itself, an empty
/// change; the local ref still names where the task started.
///
/// `fetch` refreshes the tracking ref first, silently (offline, or no `origin`,
/// still answers from what is local). Read-only callers pass `false`.
///
/// The local `base` is never moved: it may be a checked-out branch that is
/// someone's to update, and a merge base needs no write.
///
/// # Errors
///
/// Fails when neither ref shares a commit with `branch`.
pub fn attempt_base(git: &Git, base: &str, branch: &str, fetch: bool) -> Result<String, GitError> {
    if fetch {
        refresh_base(git, base);
    }
    let tracking = format!("refs/remotes/{}", remote_ref(base));
    let mut found: Vec<(String, bool)> = Vec::new();
    let mut first_err = None;
    for candidate in [base, tracking.as_str()] {
        if git
            .run(&[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{candidate}^{{commit}}"),
            ])
            .is_err()
        {
            continue;
        }
        match git.run(&["merge-base", candidate, branch]) {
            Ok(out) => {
                let mb = out.lines().next().unwrap_or("").trim().to_string();
                if mb.is_empty() {
                    continue;
                }
                let holds_tip = is_ancestor(git, branch, candidate)?;
                found.push((mb, holds_tip));
            }
            Err(err) => first_err = first_err.or(Some(err)),
        }
    }
    let merged_somewhere = found.iter().any(|(_, tip)| *tip);
    let open: Vec<&(String, bool)> = found.iter().filter(|(_, tip)| !tip).collect();
    let pool: Vec<&(String, bool)> = if open.is_empty() {
        found.iter().collect()
    } else {
        open
    };
    let mut best: Option<&str> = None;
    for (mb, _) in pool {
        best = match best {
            Some(cur) if !is_ancestor(git, cur, mb)? => Some(cur),
            _ => Some(mb.as_str()),
        };
    }
    if merged_somewhere
        && let Some(cut) = creation_point(git, branch)
        && is_ancestor(git, &cut, branch)?
        && match best {
            Some(cur) => is_ancestor(git, cur, &cut)?,
            None => true,
        }
    {
        // A candidate holds the whole branch, so its merge base is the tip and
        // the others may be stale: the branch's own reflog still names the
        // commit it was cut from.
        return Ok(cut);
    }
    match best {
        Some(mb) => Ok(mb.to_string()),
        None => match first_err {
            Some(err) => Err(err),
            None => git.run(&["merge-base", base, branch]),
        },
    }
}

/// The commit `branch` was created at, from its reflog's oldest entry.
///
/// Best effort: a branch whose reflog is gone or was recreated answers `None`.
fn creation_point(git: &Git, branch: &str) -> Option<String> {
    let log = git
        .run(&[
            "reflog",
            "show",
            "--format=%H",
            &format!("refs/heads/{branch}"),
        ])
        .ok()?;
    let oldest = log.lines().last()?.trim().to_string();
    if oldest.is_empty() {
        None
    } else {
        Some(oldest)
    }
}

/// True when `ancestor` is an ancestor of (or equal to) `rev`.
fn is_ancestor(git: &Git, ancestor: &str, rev: &str) -> Result<bool, GitError> {
    match git.run(&["merge-base", "--is-ancestor", ancestor, rev]) {
        Ok(_) => Ok(true),
        Err(GitError::Failed { code: Some(1), .. }) => Ok(false),
        Err(err) => Err(err),
    }
}

/// Fetches `base` into its remote-tracking ref and returns that commit.
fn fetch_base(git: &Git, base: &str) -> Result<String, GitError> {
    let tracking = format!("refs/remotes/{}", remote_ref(base));
    let refspec = format!("+refs/heads/{base}:{tracking}");
    git.run(&["fetch", REMOTE, &refspec])?;
    git.rev_parse(&tracking)
}

/// The remote-tracking ref of `base`.
pub(crate) fn remote_ref(base: &str) -> String {
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

#[cfg(test)]
mod attempt_base_tests {
    use super::*;
    use std::path::Path;
    use std::process::Command;

    /// Runs git in `dir`, asserting success, and returns trimmed stdout.
    fn run(dir: &Path, args: &[&str]) -> String {
        let output = match Command::new("git").args(args).current_dir(dir).output() {
            Ok(output) => output,
            Err(err) => panic!("git {args:?}: {err}"),
        };
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    /// Commits a file, returning the new commit.
    fn commit(dir: &Path, file: &str, body: &str) -> String {
        if let Err(err) = std::fs::write(dir.join(file), body) {
            panic!("write {file}: {err}");
        }
        run(dir, &["add", "--", file]);
        run(dir, &["commit", "-m", file]);
        run(dir, &["rev-parse", "HEAD"])
    }

    /// A repo with an `origin` where `origin/integration/m11` holds a landed
    /// commit that the local `integration/m11` lacks, and `task` was cut after
    /// it. Returns the temp dir, the repo path, the seed, the landed commit and
    /// the task's own commit.
    fn stale_local_base() -> (
        tempfile::TempDir,
        std::path::PathBuf,
        String,
        String,
        String,
    ) {
        let temp = match tempfile::tempdir() {
            Ok(temp) => temp,
            Err(err) => panic!("tempdir: {err}"),
        };
        let repo = temp.path().join("repo");
        if let Err(err) = std::fs::create_dir_all(&repo) {
            panic!("mkdir: {err}");
        }
        run(temp.path(), &["init", "--bare", "origin.git"]);
        run(&repo, &["init", "-b", "integration/m11"]);
        run(&repo, &["config", "user.name", "tests"]);
        run(&repo, &["config", "user.email", "tests@example.invalid"]);
        run(&repo, &["config", "commit.gpgsign", "false"]);
        run(&repo, &["remote", "add", "origin", "../origin.git"]);
        let seed = commit(&repo, "seed.txt", "seed\n");
        let landed = commit(&repo, "landed.txt", "landed\n");
        run(&repo, &["push", "-u", "origin", "integration/m11"]);
        run(&repo, &["checkout", "-b", "task"]);
        let own = commit(&repo, "own.txt", "own\n");
        // A landing through a pull request moves only origin's ref.
        run(&repo, &["branch", "-f", "integration/m11", &seed]);
        (temp, repo, seed, landed, own)
    }

    #[test]
    fn a_base_ahead_on_origin_is_where_the_task_started() {
        let (_temp, repo, _seed, landed, _own) = stale_local_base();
        let git = Git::new(&repo);
        let from = match attempt_base(&git, "integration/m11", "task", false) {
            Ok(from) => from,
            Err(err) => panic!("must resolve: {err}"),
        };
        assert_eq!(from, landed, "the stale local ref must not win");
        let diff = run(&repo, &["diff", "--name-only", &format!("{from}..task")]);
        assert_eq!(diff, "own.txt");
    }

    #[test]
    fn fetching_first_finds_a_landing_the_tracking_ref_has_not_seen() {
        let (temp, repo, _seed, landed, _own) = stale_local_base();
        // Forget what origin said, as a clone that never fetched since.
        run(
            &repo,
            &["update-ref", "-d", "refs/remotes/origin/integration/m11"],
        );
        let git = Git::new(&repo);
        let stale = match attempt_base(&git, "integration/m11", "task", false) {
            Ok(from) => from,
            Err(err) => panic!("must resolve: {err}"),
        };
        assert_ne!(stale, landed);
        let fresh = match attempt_base(&git, "integration/m11", "task", true) {
            Ok(from) => from,
            Err(err) => panic!("must resolve: {err}"),
        };
        assert_eq!(fresh, landed);
        drop(temp);
    }

    #[test]
    fn offline_falls_back_to_the_local_base() {
        let (_temp, repo, seed, _landed, _own) = stale_local_base();
        run(
            &repo,
            &["remote", "set-url", "origin", "/nonexistent/origin.git"],
        );
        run(
            &repo,
            &["update-ref", "-d", "refs/remotes/origin/integration/m11"],
        );
        let git = Git::new(&repo);
        let from = match attempt_base(&git, "integration/m11", "task", true) {
            Ok(from) => from,
            Err(err) => panic!("a failed fetch must not fail the diff: {err}"),
        };
        assert_eq!(from, seed);
    }

    #[test]
    fn a_task_already_merged_on_origin_still_shows_its_own_change() {
        let (_temp, repo, _seed, landed, own) = stale_local_base();
        // The forge merged the task (a merge commit): origin's base holds the tip.
        run(
            &repo,
            &["checkout", "-q", "--detach", "origin/integration/m11"],
        );
        run(&repo, &["merge", "--no-ff", "-m", "merge task", &own]);
        let merged = run(&repo, &["rev-parse", "HEAD"]);
        run(
            &repo,
            &["update-ref", "refs/remotes/origin/integration/m11", &merged],
        );
        let git = Git::new(&repo);
        let from = match attempt_base(&git, "integration/m11", "task", false) {
            Ok(from) => from,
            Err(err) => panic!("must resolve: {err}"),
        };
        assert_eq!(from, landed, "neither empty nor carrying landed work");
        let diff = run(&repo, &["diff", "--name-only", &format!("{from}..task")]);
        assert_eq!(diff, "own.txt");
    }
}
