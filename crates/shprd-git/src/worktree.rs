// The fresh start a new worktree branches from is adapted from shepherdr (MIT),
// mods/tasks/bin/herdr_task/launch.py (fresh_start), commit a3f327a.
// Copyright (c) 2026 Elias Stravik
// https://github.com/eliasstravik/shepherdr
//
//! Per-attempt git worktrees, and the only removal path shprd may take.
//!
//! A worktree is created on a branch inside the layout's namespace and removed
//! only when its branch still starts with that namespace, so shprd can never
//! delete a branch a human owns.

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::git::{Git, GitError};

/// Where worktrees live, and what their branches are called.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// Directory holding every worktree.
    pub root: PathBuf,
    /// Prefix every shprd branch carries.
    pub namespace: String,
}

impl Layout {
    /// The default layout for a home directory: `<home>/.shprd/wt`, branches
    /// prefixed `shprd/`.
    pub fn default_for_home(home: impl Into<PathBuf>) -> Self {
        Self {
            root: home.into().join(".shprd").join("wt"),
            namespace: "shprd/".to_string(),
        }
    }

    /// The branch a task attempt commits on: `<namespace><run>/<task>`.
    pub fn branch_name(&self, run: &str, task: &str) -> String {
        format!("{}{run}/{task}", self.namespace)
    }

    /// The directory a task attempt works in:
    /// `<root>/<repo_name>/<run>/<task>`.
    pub fn path(&self, repo_name: &str, run: &str, task: &str) -> PathBuf {
        self.root.join(repo_name).join(run).join(task)
    }

    /// True when `branch` belongs to shprd, and so may be removed by it.
    pub fn owns_branch(&self, branch: &str) -> bool {
        branch.starts_with(&self.namespace)
    }
}

/// A worktree shprd created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    /// Where it is on disk.
    pub path: PathBuf,
    /// The branch checked out in it.
    pub branch: String,
    /// The commit it was cut from: the base branch's fetched remote tip when
    /// the remote had one, else the local branch's.
    pub base_sha: String,
}

/// Why a worktree could not be created or removed.
#[derive(Debug, Error)]
pub enum WorktreeError {
    /// git failed.
    #[error(transparent)]
    Git(#[from] GitError),
    /// A run or task id is not a plain identifier.
    #[error("{what} {id:?} is not a usable identifier")]
    InvalidId {
        /// Which identifier was bad, e.g. `"run"`.
        what: &'static str,
        /// The value, as given.
        id: String,
    },
    /// A branch of that name already exists.
    #[error("branch {branch:?} already exists")]
    BranchExists {
        /// The branch name.
        branch: String,
    },
    /// Something already occupies the worktree path.
    #[error("worktree path {path:?} already exists")]
    PathExists {
        /// The path.
        path: PathBuf,
    },
    /// The branch does not belong to shprd, so shprd may not remove it.
    #[error("branch {branch:?} is outside the {namespace:?} namespace")]
    OutsideNamespace {
        /// The branch that was offered for removal.
        branch: String,
        /// shprd's own prefix.
        namespace: String,
    },
    /// The worktree path is not inside the layout root, so shprd may not
    /// remove it: it was not created by shprd.
    #[error("worktree path {path:?} is outside the root {root:?}")]
    OutsideRoot {
        /// The path that was offered for removal.
        path: PathBuf,
        /// shprd's worktree root.
        root: PathBuf,
    },
    /// A directory could not be created.
    #[error("could not create {path:?}: {source}")]
    Io {
        /// The path that failed.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// The worktree has uncommitted work.
    #[error("worktree {path:?} has uncommitted changes; pass force to discard them")]
    Dirty {
        /// The worktree path.
        path: PathBuf,
    },
}

/// The directories shprd writes into the root of a checkout, which belong to
/// the run and never to the repository: `.shprd/` (the log, config and asks)
/// and `.dagr/` (the projected graph).
pub const STATE_DIRS: [&str; 2] = [".shprd/", ".dagr/"];

/// The comment written above [`STATE_DIRS`] in git's exclude file.
const EXCLUDE_COMMENT: &str = "# shprd: the run's own state, never committed";

/// Creates a worktree for one task attempt of `run` in `repo`.
///
/// The branch starts where [`fresh_start`] says: the base branch's fetched
/// remote tip when the remote holds everything the local branch has, so a task
/// branches from the latest even when others merged on the forge. The
/// repository's own checkout is never moved.
///
/// Refuses when the branch or the path already exists: recreating over either
/// would either lose someone else's commits or adopt work shprd did not make.
pub fn create(
    repo: &Path,
    layout: &Layout,
    run: &str,
    task: &str,
    base: &str,
) -> Result<Worktree, WorktreeError> {
    check_id("run", run)?;
    check_id("task", task)?;
    let git = Git::new(repo);
    let branch = layout.branch_name(run, task);
    let repo_name = repo_name(repo)?;
    let path = layout.path(&repo_name, run, task);
    if git.branch_exists(&branch)? {
        return Err(WorktreeError::BranchExists { branch });
    }
    if path.exists() {
        return Err(WorktreeError::PathExists { path });
    }
    let base_sha = git.rev_parse(&fresh_start(&git, base))?;
    if let Some(parent) = path.parent()
        && let Err(source) = std::fs::create_dir_all(parent)
    {
        return Err(WorktreeError::Io {
            path: parent.to_path_buf(),
            source,
        });
    }
    let path_text = path_string(&path)?;
    git.run(&["worktree", "add", "-b", &branch, &path_text, &base_sha])?;
    // A worker must never see shprd's own state as an untracked change, and the
    // repository's tracked `.gitignore` is not shprd's to edit: git's own
    // per-repository exclude list is. A failure undoes the worktree, so a create
    // that returns an error leaves nothing behind.
    if let Err(err) = exclude_state_dirs(&path) {
        let _ = git.run(&["worktree", "remove", "--force", &path_text]);
        let _ = git.run(&["branch", "-D", &branch]);
        return Err(err);
    }
    Ok(Worktree {
        path,
        branch,
        base_sha,
    })
}

/// Adds [`STATE_DIRS`] to the repository's `info/exclude` for `worktree`.
///
/// The file is git's own per-repository exclude list
/// (`<git-common-dir>/info/exclude`): git never tracks it, every worktree of
/// the repository reads it, and the repository's `.gitignore` is left
/// untouched. Only the entries that are missing are added, so calling this on a
/// worktree that already excludes the state adds nothing, and a person's own
/// entries are kept exactly as they were.
pub fn exclude_state_dirs(worktree: &Path) -> Result<(), WorktreeError> {
    let git = Git::new(worktree);
    let listed = git.run(&["rev-parse", "--git-path", "info/exclude"])?;
    // git may print the path relative to the worktree or absolute; joining
    // keeps both readings.
    let path = worktree.join(listed.trim());
    let current = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(source) => return Err(WorktreeError::Io { path, source }),
    };
    let missing: Vec<String> = STATE_DIRS
        .iter()
        .map(|dir| format!("/{dir}"))
        .filter(|pattern| !current.lines().any(|line| line.trim() == pattern))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    let mut text = current;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    if !text.lines().any(|line| line.trim() == EXCLUDE_COMMENT) {
        text.push_str(EXCLUDE_COMMENT);
        text.push('\n');
    }
    for pattern in missing {
        text.push_str(&pattern);
        text.push('\n');
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| WorktreeError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    std::fs::write(&path, text).map_err(|source| WorktreeError::Io { path, source })
}

/// Whether the repository has a remote a landing could push to.
///
/// A repository with no remote lands locally: its task branches are merged into
/// the run's integration branch in shprd's own worktree, and nothing is pushed.
pub fn has_remote(git: &Git) -> bool {
    remote_of(git).is_some()
}

/// The remote a landing fetches from and pushes to: `origin` when the
/// repository has it, else the only remote it has, else `None`.
///
/// Shepherdr reads the remotes the same way, and a repository with no remote at
/// all simply branches from its local base.
pub(crate) fn remote_of(git: &Git) -> Option<String> {
    let remotes = git.run(&["remote"]).ok()?;
    let names: Vec<&str> = remotes
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .collect();
    if names.contains(&"origin") {
        return Some(String::from("origin"));
    }
    names.first().map(|name| (*name).to_string())
}

/// Where a new worktree on `base` starts.
///
/// The base is fetched from the remote first, and the remote's tip is used when
/// it holds everything the local branch has — which is the ordinary case, where
/// the local branch is only behind. The local branch is used instead when it has
/// work of its own that the remote lacks, when there is no remote, or when the
/// remote has no such branch: a task then branches from what the repository
/// actually has, and nothing is moved behind a human's back.
///
/// This is Shepherdr's `fresh_start` (MIT): a task branches from the latest
/// base, even when other work merged on the forge since the checkout was last
/// fetched.
pub fn fresh_start(git: &Git, base: &str) -> String {
    let Some(remote) = remote_of(git) else {
        return base.to_string();
    };
    if !git.branch_exists(base).unwrap_or(false) {
        return base.to_string();
    }
    let tracking = format!("refs/remotes/{remote}/{base}");
    let fetch = git.run(&[
        "fetch",
        "--quiet",
        "--no-tags",
        &remote,
        &format!("+refs/heads/{base}:{tracking}"),
    ]);
    if fetch.is_err() || git.rev_parse(&tracking).is_err() {
        return base.to_string();
    }
    let behind = git
        .run(&["merge-base", "--is-ancestor", base, &tracking])
        .is_ok();
    if behind {
        format!("{remote}/{base}")
    } else {
        base.to_string()
    }
}

/// Removes a worktree and, with it, the branch it has checked out.
///
/// Refuses any branch outside [`Layout::namespace`], so a branch a human made
/// by hand is never touched, and otherwise does exactly what
/// [`remove_worktree_only`] does. Only a caller whose ledger recorded the
/// branch may take this path; the janitor does not, because a worktree can
/// exist for a branch the ledger never recorded.
pub fn remove(
    repo: &Path,
    layout: &Layout,
    wt: &Worktree,
    force: bool,
) -> Result<(), WorktreeError> {
    if !layout.owns_branch(&wt.branch) {
        return Err(WorktreeError::OutsideNamespace {
            branch: wt.branch.clone(),
            namespace: layout.namespace.clone(),
        });
    }
    remove_worktree_only(repo, layout, &wt.path, force)?;
    Git::new(repo).run(&["branch", "-D", &wt.branch])?;
    Ok(())
}

/// Removes a worktree and leaves its branch alone.
///
/// Refuses a path outside [`Layout::root`], so a directory shprd did not
/// create is never deleted, and refuses a dirty worktree unless `force`. No
/// branch is ever deleted here: `git worktree remove` keeps the branch, and a
/// branch is only removed by a step the ledger planned from a recorded
/// `branch` resource.
pub fn remove_worktree_only(
    repo: &Path,
    layout: &Layout,
    path: &Path,
    force: bool,
) -> Result<(), WorktreeError> {
    if !inside_root(&layout.root, path) {
        return Err(WorktreeError::OutsideRoot {
            path: path.to_path_buf(),
            root: layout.root.clone(),
        });
    }
    if path.exists() && !Git::new(path).is_clean()? && !force {
        return Err(WorktreeError::Dirty {
            path: path.to_path_buf(),
        });
    }
    let git = Git::new(repo);
    let path_text = path_string(path)?;
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(&path_text);
    git.run(&args)?;
    Ok(())
}

/// Deletes one branch, treating one that is already gone as gone.
///
/// Refuses any branch outside [`Layout::namespace`], so a branch a human made
/// by hand is never deleted. This is the only branch deletion shprd has, and
/// callers reach it only for a branch their ledger recorded creating.
pub fn delete_branch(repo: &Path, layout: &Layout, branch: &str) -> Result<(), WorktreeError> {
    if !layout.owns_branch(branch) {
        return Err(WorktreeError::OutsideNamespace {
            branch: branch.to_string(),
            namespace: layout.namespace.clone(),
        });
    }
    let git = Git::new(repo);
    if !git.branch_exists(branch)? {
        return Ok(());
    }
    git.run(&["branch", "-D", branch])?;
    Ok(())
}

/// Rejects an id that is empty, uses anything but `[A-Za-z0-9._-]`, or
/// contains `..`.
fn check_id(what: &'static str, id: &str) -> Result<(), WorktreeError> {
    if valid_id(id) {
        return Ok(());
    }
    Err(WorktreeError::InvalidId {
        what,
        id: id.to_string(),
    })
}

/// True when `id` is usable as a run or task id: non-empty, only
/// `[A-Za-z0-9._-]`, and no `..`.
pub(crate) fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && !id.contains("..")
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// True when `path` lies inside `root`, compared by path components rather
/// than by string prefix.
///
/// Both sides are resolved first — symlinks and `..` dropped, for the longest
/// existing prefix of the path — so `/tmp/x/wt` and `/private/tmp/x/wt` are the
/// same place on a platform where `/tmp` is a symlink.
pub(crate) fn inside_root(root: &Path, path: &Path) -> bool {
    resolve(path).starts_with(resolve(root))
}

/// Replaces `path` with an absolute, symlink-free path, resolving as much of
/// it as exists and appending the rest unchanged.
fn resolve(path: &Path) -> PathBuf {
    let mut prefix = path.to_path_buf();
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Ok(mut resolved) = std::fs::canonicalize(&prefix) {
            for part in rest.iter().rev() {
                resolved.push(part);
            }
            return resolved;
        }
        match (
            prefix.file_name().map(|name| name.to_os_string()),
            prefix.parent(),
        ) {
            (Some(name), Some(parent)) => {
                rest.push(name);
                prefix = parent.to_path_buf();
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// The name of the repository directory, used to keep repos apart on disk.
fn repo_name(repo: &Path) -> Result<String, WorktreeError> {
    match repo.file_name().and_then(|name| name.to_str()) {
        Some(name) if !name.is_empty() => Ok(name.to_string()),
        _ => Err(WorktreeError::InvalidId {
            what: "repository",
            id: repo.display().to_string(),
        }),
    }
}

/// A path as git should see it.
fn path_string(path: &Path) -> Result<String, WorktreeError> {
    match path.to_str() {
        Some(text) => Ok(text.to_string()),
        None => Err(WorktreeError::InvalidId {
            what: "path",
            id: path.display().to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// A repository with one commit on `main`, and a home outside it.
    fn fixture() -> (tempfile::TempDir, tempfile::TempDir) {
        let repo = tempfile::tempdir().expect("repo dir");
        let home = tempfile::tempdir().expect("home dir");
        let git = Git::new(repo.path());
        git.run(&["init", "-b", "main"]).expect("git init");
        git.run(&["config", "user.name", "shprd tests"])
            .expect("user.name");
        git.run(&["config", "user.email", "tests@example.invalid"])
            .expect("user.email");
        git.run(&["config", "commit.gpgsign", "false"])
            .expect("gpgsign");
        std::fs::write(
            repo.path().join("calc.py"),
            "def add(a, b):\n    return a + b\n",
        )
        .expect("a seed file");
        git.run(&["add", "--", "calc.py"]).expect("git add");
        git.run(&["commit", "-m", "seed"]).expect("git commit");
        (repo, home)
    }

    /// The repository's own exclude file, as git points at it.
    fn exclude_of(repo: &Path) -> PathBuf {
        let listed = Git::new(repo)
            .run(&["rev-parse", "--git-path", "info/exclude"])
            .expect("git path");
        repo.join(listed.trim())
    }

    #[test]
    fn a_new_worktree_excludes_shprds_state_directories() {
        let (repo, home) = fixture();
        let layout = Layout::default_for_home(home.path());
        let created = create(repo.path(), &layout, "m9", "T1", "main").expect("a worktree");
        let text = std::fs::read_to_string(exclude_of(repo.path())).expect("the exclude file");
        for pattern in ["/.shprd/", "/.dagr/"] {
            assert!(
                text.lines().any(|line| line.trim() == pattern),
                "{pattern} is missing from:\n{text}"
            );
        }

        // What the engine and a worker's own verbs write there no longer shows
        // up as an untracked change in the worker's checkout.
        std::fs::create_dir_all(created.path.join(".shprd")).expect("state dir");
        std::fs::create_dir_all(created.path.join(".dagr")).expect("graph dir");
        std::fs::write(created.path.join(".shprd/events.jsonl"), "{}\n").expect("an event");
        std::fs::write(created.path.join(".dagr/run.json"), "{}\n").expect("a graph");
        assert!(
            Git::new(&created.path).is_clean().expect("git status"),
            "shprd's own state must not show as a change"
        );

        // The repository's tracked ignore file is never written: only git's own
        // untracked, per-repository exclude list is.
        assert!(
            !repo.path().join(".gitignore").exists(),
            "the repository's .gitignore is not shprd's to edit"
        );
    }

    #[test]
    fn a_second_worktree_does_not_duplicate_the_exclusions() {
        let (repo, home) = fixture();
        let layout = Layout::default_for_home(home.path());
        create(repo.path(), &layout, "m9", "T1", "main").expect("the first worktree");
        create(repo.path(), &layout, "m9", "T2", "main").expect("the second worktree");
        let text = std::fs::read_to_string(exclude_of(repo.path())).expect("the exclude file");
        for pattern in ["/.shprd/", "/.dagr/"] {
            assert_eq!(
                text.lines().filter(|line| line.trim() == pattern).count(),
                1,
                "{pattern} appears once in:\n{text}"
            );
        }
    }

    #[test]
    fn a_persons_own_exclude_entries_are_kept() {
        let (repo, home) = fixture();
        let path = exclude_of(repo.path());
        std::fs::create_dir_all(path.parent().expect("the info dir")).expect("info dir");
        std::fs::write(&path, "# mine\n*.local\n").expect("the person's entries");
        let layout = Layout::default_for_home(home.path());
        create(repo.path(), &layout, "m9", "T1", "main").expect("a worktree");
        let text = std::fs::read_to_string(&path).expect("the exclude file");
        assert!(text.contains("# mine\n*.local\n"), "{text}");
        assert!(text.contains("/.shprd/"), "{text}");
        assert!(text.contains("/.dagr/"), "{text}");
    }
}
