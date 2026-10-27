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
    /// The commit it was cut from.
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

/// Creates a worktree for one task attempt of `run` in `repo`.
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
    let base_sha = git.rev_parse(base)?;
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
    Ok(Worktree {
        path,
        branch,
        base_sha,
    })
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
