//! Landing a task's verified commits on a run's integration branch.
//!
//! The integration branch is worked on in a worktree of its own, at
//! `<root>/<repo>/<run>/_int`, never in the user's checkout: a cherry-pick that
//! stops on a conflict rewrites the working tree, and shprd is not allowed to
//! do that anywhere a human might be working.

use std::fs::File;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::git::{Git, GitError};
use crate::worktree::{Layout, WorktreeError};

/// The task id that names the integration branch.
const INTEGRATION_BRANCH_TASK: &str = "int";

/// The task id that names the integration worktree's directory. It is not a
/// real task id, so it cannot collide with one.
const INTEGRATION_PATH_TASK: &str = "_int";

/// The integration worktree of one run, and whether this call made it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationWorktree {
    /// Where it is on disk: `<root>/<repo>/<run>/_int`.
    pub path: PathBuf,
    /// The branch checked out in it: `<namespace><run>/int` for the run-level
    /// path, or the run's declared integration branch for a local landing.
    pub branch: String,
    /// True when [`open_integration`] created it, false when it was already
    /// there.
    pub created: bool,
}

/// What a finished task claims: its branch, the base it was cut from, the
/// commits it says it made, and the paths it was allowed to change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskCommits {
    /// The task's branch.
    pub branch: String,
    /// The commit the task's branch was cut from.
    pub base_sha: String,
    /// The commits the task claims, oldest first. Abbreviated ids are accepted
    /// and resolved.
    pub claimed: Vec<String>,
    /// The paths the task declared: a hint for its worker, never a limit.
    pub scope: Vec<String>,
}

/// What landed on the integration branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Integrated {
    /// The integration branch's head after the picks.
    pub new_head: String,
    /// The commits created on the integration branch, oldest first.
    pub picked: Vec<String>,
}

/// Why an integration could not be opened or performed.
#[derive(Debug, Error)]
pub enum IntegrateError {
    /// git failed.
    #[error(transparent)]
    Git(#[from] GitError),
    /// A worktree operation failed.
    #[error(transparent)]
    Worktree(#[from] WorktreeError),
    /// The integration worktree has uncommitted work.
    #[error("integration worktree {path:?} has uncommitted changes")]
    Dirty {
        /// The worktree that is not clean.
        path: PathBuf,
    },
    /// The branch and its worktree exist only one at a time, or the path is on
    /// another branch.
    #[error("integration worktree is in a half state at {path:?} ({branch:?}): {detail}")]
    BadState {
        /// The path that was inspected.
        path: PathBuf,
        /// The branch that should be there.
        branch: String,
        /// What was actually found.
        detail: String,
    },
    /// The commits on the branch are not the commits the task claimed.
    #[error("claimed {} commit(s) but {branch} has {}", claimed.len(), expected.len())]
    ClaimMismatch {
        /// The commits the branch actually has, oldest first.
        expected: Vec<String>,
        /// The commits the task claimed, resolved, in the order given.
        claimed: Vec<String>,
        /// The branch that was inspected.
        branch: String,
    },
    /// The branch has no commits beyond its base.
    #[error("branch {branch:?} has no commits beyond {base:?}")]
    NothingToIntegrate {
        /// The task's branch.
        branch: String,
        /// The base it was cut from.
        base: String,
    },
    /// A cherry-pick failed. The integration worktree is back on the head it
    /// had before the attempt.
    #[error("cherry-pick of {commit} conflicted in {} file(s)", files.len())]
    Conflict {
        /// The commit that failed.
        commit: String,
        /// The files git reported as unmerged.
        files: Vec<String>,
    },
    /// A failed cherry-pick could not be undone.
    #[error("integration worktree is not back at {expected:?} after a failed cherry-pick")]
    NotRestored {
        /// The head that should have been restored.
        expected: String,
    },
}

/// Makes sure the integration worktree excludes shprd's own state.
///
/// An integration worktree an older build made has no `info/exclude` entries, so
/// a landing that reuses it would see `.shprd/` and `.dagr/` as untracked work.
/// The repair is idempotent and costs one file read, so every open makes it.
fn repair_state_excluded(path: &Path) -> Result<(), IntegrateError> {
    crate::worktree::ensure_state_excluded(path)?;
    Ok(())
}

/// Opens, and on first use creates, the integration worktree of `run`.
///
/// Both the branch `<namespace><run>/int` and the path
/// `<root>/<repo_name>/<run>/_int` are created together or not at all, so a
/// second call finds both and reports `created: false`. Anything in between —
/// a branch with no worktree, a path with no branch, or a worktree sitting on
/// another branch — is refused rather than repaired: shprd cannot tell whether
/// a human is behind it.
pub fn open_integration(
    repo: &Path,
    layout: &Layout,
    repo_name: &str,
    run: &str,
    base: &str,
) -> Result<IntegrationWorktree, IntegrateError> {
    let git = Git::new(repo);
    let branch = layout.branch_name(run, INTEGRATION_BRANCH_TASK);
    let path = layout.path(repo_name, run, INTEGRATION_PATH_TASK);
    let branch_exists = git.branch_exists(&branch)?;
    let path_exists = path.exists();
    match (branch_exists, path_exists) {
        (true, true) => {
            let checked_out = Git::new(&path).current_branch()?;
            if checked_out.as_deref() != Some(branch.as_str()) {
                return Err(IntegrateError::BadState {
                    path,
                    branch,
                    detail: format!("it is on {checked_out:?}"),
                });
            }
            repair_state_excluded(&path)?;
            Ok(IntegrationWorktree {
                path,
                branch,
                created: false,
            })
        }
        (false, false) => {
            let base_sha = git.rev_parse(base)?;
            if let Some(parent) = path.parent()
                && let Err(source) = std::fs::create_dir_all(parent)
            {
                return Err(WorktreeError::Io {
                    path: parent.to_path_buf(),
                    source,
                }
                .into());
            }
            let path_text = path_text(&path)?;
            git.run(&["worktree", "add", "-b", &branch, &path_text, &base_sha])?;
            repair_state_excluded(&path)?;
            Ok(IntegrationWorktree {
                path,
                branch,
                created: true,
            })
        }
        (true, false) => Err(IntegrateError::BadState {
            path,
            branch,
            detail: "the branch exists but its worktree does not".to_string(),
        }),
        (false, true) => Err(IntegrateError::BadState {
            path,
            branch,
            detail: "the path exists but its branch does not".to_string(),
        }),
    }
}

/// Opens, and on first use creates, a worktree for the run's own integration
/// branch — the branch the run's attempts are cut from.
///
/// Unlike [`open_integration`], the branch is named by the run rather than made
/// up here, and it may already exist: `integration/m9` is a branch a human can
/// have made before the run started. Both the branch and the worktree are
/// created together or not at all, and the worktree is at
/// `<root>/<repo_name>/<run>/_int`, never in the user's checkout.
///
/// # Errors
///
/// Fails when git cannot be driven, when the path is occupied by a worktree on
/// another branch, or when the branch is missing but its path is not.
pub fn open_integration_branch(
    repo: &Path,
    layout: &Layout,
    repo_name: &str,
    run: &str,
    branch: &str,
    base: &str,
) -> Result<IntegrationWorktree, IntegrateError> {
    let git = Git::new(repo);
    let path = layout.path(repo_name, run, INTEGRATION_PATH_TASK);
    let branch_exists = git.branch_exists(branch)?;
    let path_exists = path.exists();
    match (branch_exists, path_exists) {
        (true, true) => {
            let at = Git::new(&path);
            match at.current_branch()? {
                Some(on) if on == branch => {}
                // Between lands the worktree is detached so the person's own
                // checkout may hold the branch; a landing takes it back, and git
                // refuses (with its own reason) when the person holds it now.
                None => {
                    at.run(&["switch", branch])?;
                }
                Some(on) => {
                    return Err(IntegrateError::BadState {
                        path,
                        branch: branch.to_string(),
                        detail: format!("it is on {:?}", Some(on)),
                    });
                }
            }
            repair_state_excluded(&path)?;
            Ok(IntegrationWorktree {
                path,
                branch: branch.to_string(),
                created: false,
            })
        }
        (true, false) => {
            create_parent(&path)?;
            let path_text = path_text(&path)?;
            git.run(&["worktree", "add", &path_text, branch])?;
            repair_state_excluded(&path)?;
            Ok(IntegrationWorktree {
                path,
                branch: branch.to_string(),
                created: true,
            })
        }
        (false, false) => {
            let base_sha = git.rev_parse(base)?;
            create_parent(&path)?;
            let path_text = path_text(&path)?;
            git.run(&["worktree", "add", "-b", branch, &path_text, &base_sha])?;
            repair_state_excluded(&path)?;
            Ok(IntegrationWorktree {
                path,
                branch: branch.to_string(),
                created: true,
            })
        }
        (false, true) => Err(IntegrateError::BadState {
            path,
            branch: branch.to_string(),
            detail: "the path exists but its branch does not".to_string(),
        }),
    }
}

/// Lets go of the integration branch: detaches the worktree's `HEAD` at the
/// branch's tip.
///
/// Git lets one worktree hold a branch, so a worktree left on the integration
/// branch keeps the person from `git switch`ing to it in their own checkout. A
/// landing calls this when it is done, and [`open_integration_branch`] takes the
/// branch back for the next one.
///
/// # Errors
///
/// Fails when git cannot detach the worktree.
pub fn release_integration(int: &IntegrationWorktree) -> Result<(), IntegrateError> {
    Git::new(&int.path).run(&["switch", "--detach"])?;
    Ok(())
}

/// One run's exclusive integration lock, released when it is dropped.
///
/// Every verb that writes the integration worktree takes it — `integrate`
/// cherry-picks onto the branch, `sync` merges the base into it, `gate` runs
/// commands in it, `land` reads the head it pushes, `approve` reads the head it
/// stamps — and so does the tick's idle sweep, because detaching `HEAD` under a
/// landing is the same race as merging under one. The lock is an advisory lock
/// on a file under `<home>/.shprd/locks`, so it is held by the process and not by
/// a stale file: the OS drops it however the process ends.
pub struct RunLock {
    /// The open file the lock sits on. Closing it releases the lock.
    _file: File,
}

impl RunLock {
    /// A lock on a file this caller has already locked itself.
    ///
    /// For a lock whose name the binary owns rather than the library — the
    /// start lock is the CLI's alone — so both still release by dropping the
    /// file, rather than by two implementations of the same advisory lock.
    #[must_use]
    pub fn from_locked(file: File) -> Self {
        Self { _file: file }
    }
}

/// The file the lock of `run` in `repo_name` sits on.
///
/// The repository name and the run id are in the name, so two repositories, or
/// two runs of one repository, never wait on each other.
#[must_use]
pub fn lock_path(home: &Path, repo_name: &str, run: &str) -> PathBuf {
    home.join(".shprd")
        .join("locks")
        .join(format!("{repo_name}-{run}-int.lock"))
}

/// Takes the run's integration lock, blocking until it is free.
///
/// # Errors
///
/// Fails when the lock file cannot be opened or locked.
pub fn lock_run(home: &Path, repo_name: &str, run: &str) -> std::io::Result<RunLock> {
    let file = open_lock(lock_path(home, repo_name, run))?;
    file.lock()?;
    Ok(RunLock { _file: file })
}

/// Takes the run's integration lock, or answers `None` when something holds it.
///
/// For a caller that must not wait — the tick's idle sweep runs inside a pass,
/// and a landing may hold the lock for as long as a gate takes. `None` means "a
/// verb is working on this worktree right now", which is the one thing the
/// sweep has to stay out of the way of.
///
/// # Errors
///
/// Fails when the lock file cannot be opened, or for any locking error other
/// than the lock being held.
pub fn try_lock_run(home: &Path, repo_name: &str, run: &str) -> std::io::Result<Option<RunLock>> {
    let file = open_lock(lock_path(home, repo_name, run))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(RunLock { _file: file })),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(err)) => Err(err),
    }
}

/// Opens (making its folder and the file itself) the lock `path` names.
fn open_lock(path: PathBuf) -> std::io::Result<File> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
}

/// Detaches the integration worktree at `path` when it still sits on a branch.
///
/// For the tick's idle sweep: a base sync (or any other command) re-takes the
/// branch, and nothing else lets it go until the next landing. Does nothing
/// when the path is not a worktree, is already detached, or has uncommitted
/// work (a command may be mid-merge there). Returns true when it detached.
///
/// The caller must hold the run's integration lock ([`try_lock_run`]) across
/// this call: a clean worktree on the branch is also what a landing that has not
/// merged yet looks like, and detaching `HEAD` under it would land the merge on
/// a detached head while the branch never moves.
pub fn release_if_held(path: &Path) -> bool {
    if !path.join(".git").exists() {
        return false;
    }
    let git = Git::new(path);
    match (git.current_branch(), git.is_clean()) {
        (Ok(Some(_)), Ok(true)) => git.run(&["switch", "--detach"]).is_ok(),
        _ => false,
    }
}

/// Brings the local `branch` back to the remote's copy of it, when the two differ.
///
/// With a forge, the remote is where a branch lands, so a local ref that is
/// ahead of it or apart from it is a landing that never reached the forge. The
/// remote is fetched first (best effort; offline leaves what is known), and the
/// ref is then moved with `git branch -f`. The commits stay reachable on the task
/// branches and in the reflog. A branch that is checked out in a worktree is left
/// alone, because moving it would change that worktree's files; so is a
/// repository with no forge or a branch the remote does not have. Returns
/// whether the ref moved.
pub fn realign_with_remote(repo: &Path, branch: &str) -> bool {
    matches!(
        realign_outcome(repo, branch, |host| crate::land::gh_authed(repo, host)),
        Realign::Moved
    )
}

/// What [`realign_outcome`] did to the local integration ref.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Realign {
    /// Nothing to do: no forge, no remote copy, already equal, or the branch is
    /// checked out in a worktree.
    Unchanged,
    /// The local ref moved to the remote's copy.
    Moved,
    /// The local ref holds nothing the remote lacks (equal to it, or only
    /// commits it has by content), so no diverged-ref condition holds.
    Aligned,
    /// The local ref holds commits the remote does not have, even by content.
    /// It is left as it is; these are the local-only commits (`sha subject`).
    Kept(Vec<String>),
}

/// [`realign_with_remote`] with the forge-client check given (see
/// [`crate::land::forge_expected_with`]), reporting what happened.
///
/// A local ref is moved back only when every commit it has that the remote
/// lacks is already on the remote by content (`git cherry` shows only `-`
/// lines: a squash or rebase landing carried them over as new commits). Any
/// other local-only commit is work that exists nowhere else, so the ref is kept
/// and the commits are named in [`Realign::Kept`].
pub fn realign_outcome(repo: &Path, branch: &str, authed: impl Fn(&str) -> bool) -> Realign {
    let git = Git::new(repo);
    // Only a remote with a forge behind it (GitHub, or a host a forge client
    // can drive) is one a landing goes through; any other remote's local
    // branch may hold work that is not on it yet.
    if !crate::land::forge_expected_with(&git, authed) {
        return Realign::Unchanged;
    }
    let Some(remote) = crate::worktree::remote_of(&git) else {
        return Realign::Unchanged;
    };
    let _ = git.run(&["fetch", "--quiet", &remote, branch]);
    let tracking = format!("refs/remotes/{remote}/{branch}");
    let local = format!("refs/heads/{branch}");
    let (Ok(theirs), Ok(ours)) = (git.rev_parse(&tracking), git.rev_parse(&local)) else {
        return Realign::Unchanged;
    };
    if theirs == ours {
        return Realign::Aligned;
    }
    // `git cherry <upstream> <head>`: `+` is a commit of head whose change the
    // upstream lacks, `-` one it already has by content.
    let Ok(cherry) = git.run(&["cherry", &tracking, &local]) else {
        return Realign::Unchanged;
    };
    let only_here: Vec<String> = cherry
        .lines()
        .filter_map(|line| line.strip_prefix("+ "))
        .map(|sha| {
            let sha = sha.trim();
            let subject = git
                .run(&["log", "-1", "--format=%s", sha])
                .unwrap_or_default();
            format!("{} {}", &sha[..sha.len().min(7)], subject.trim())
        })
        .collect();
    if !only_here.is_empty() {
        return Realign::Kept(only_here);
    }
    let held = git
        .run(&["worktree", "list", "--porcelain"])
        .is_ok_and(|list| list.lines().any(|line| line == format!("branch {local}")));
    if held {
        return Realign::Aligned;
    }
    if git.run(&["branch", "-f", branch, &theirs]).is_ok() {
        Realign::Moved
    } else {
        Realign::Unchanged
    }
}

/// Creates the directory a worktree will sit in.
fn create_parent(path: &Path) -> Result<(), IntegrateError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    if let Err(source) = std::fs::create_dir_all(parent) {
        return Err(WorktreeError::Io {
            path: parent.to_path_buf(),
            source,
        }
        .into());
    }
    Ok(())
}

/// A path as git should see it.
fn path_text(path: &Path) -> Result<String, WorktreeError> {
    match path.to_str() {
        Some(text) => Ok(text.to_string()),
        None => Err(WorktreeError::InvalidId {
            what: "path",
            id: path.display().to_string(),
        }),
    }
}

/// Lands a task's commits on the integration branch.
///
/// Everything is checked before anything is picked: the worktree must be
/// clean and the branch must contain exactly the commits the task claimed.
/// Only then are the commits cherry-picked, oldest first, with `-x` so each one records
/// where it came from.
///
/// A pick that fails leaves the integration worktree on the head it started
/// from and reports [`IntegrateError::Conflict`]; the task's branch is never
/// touched.
pub fn integrate(
    int: &IntegrationWorktree,
    task: &TaskCommits,
) -> Result<Integrated, IntegrateError> {
    let git = Git::new(&int.path);
    if !git.is_clean()? {
        return Err(IntegrateError::Dirty {
            path: int.path.clone(),
        });
    }
    let tip = git.rev_parse(&task.branch)?;
    let expected = commits_between(&git, &task.base_sha, &tip)?;
    if expected.is_empty() {
        return Err(IntegrateError::NothingToIntegrate {
            branch: task.branch.clone(),
            base: task.base_sha.clone(),
        });
    }
    let claimed = resolve_all(&git, &task.claimed)?;
    if claimed != expected {
        return Err(IntegrateError::ClaimMismatch {
            expected,
            claimed,
            branch: task.branch.clone(),
        });
    }
    // The task's declared scope is a hint, not a limit: what the branch changes
    // was judged by its review, so nothing here refuses a path.
    let head = git.head_sha()?;
    let mut picked = Vec::with_capacity(expected.len());
    for commit in &expected {
        if git.run(&["cherry-pick", "-x", commit]).is_err() {
            return Err(undo_failed_pick(&git, &head, commit));
        }
        picked.push(git.head_sha()?);
    }
    Ok(Integrated {
        new_head: git.head_sha()?,
        picked,
    })
}

/// Collects the unmerged files of a failed pick, undoes it, and returns what
/// went wrong.
///
/// The worktree is put back on `head` and the result is verified rather than
/// assumed, so a caller that sees [`IntegrateError::Conflict`] can trust the
/// branch is where it was.
fn undo_failed_pick(git: &Git, head: &str, commit: &str) -> IntegrateError {
    let files = match git.run(&["diff", "--name-only", "-z", "--diff-filter=U"]) {
        Ok(raw) => split_nul(&raw),
        Err(_) => Vec::new(),
    };
    // `--abort` restores what git recorded for itself; the reset that follows
    // is authoritative, and both are needed because an empty pick leaves no
    // sequencer to abort.
    let _ = git.run(&["cherry-pick", "--abort"]);
    let restored = git.run(&["reset", "--hard", head]).is_ok()
        && git.head_sha().ok().as_deref() == Some(head)
        && git.is_clean().unwrap_or(false);
    if !restored {
        return IntegrateError::NotRestored {
            expected: head.to_string(),
        };
    }
    IntegrateError::Conflict {
        commit: commit.to_string(),
        files,
    }
}

/// The commits in `base..tip`, oldest first.
fn commits_between(git: &Git, base: &str, tip: &str) -> Result<Vec<String>, GitError> {
    let range = format!("{base}..{tip}");
    Ok(lines(&git.run(&["rev-list", "--reverse", &range])?))
}

/// Resolves each revision to a full commit id, keeping the order given.
fn resolve_all(git: &Git, revs: &[String]) -> Result<Vec<String>, GitError> {
    revs.iter().map(|rev| git.rev_parse(rev)).collect()
}

/// Non-empty lines of git output.
fn lines(text: &str) -> Vec<String> {
    text.split('\n')
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

/// Splits NUL-separated git output into fields, dropping the empty tail.
fn split_nul(text: &str) -> Vec<String> {
    text.split('\0')
        .filter(|field| !field.is_empty())
        .map(str::to_string)
        .collect()
}
