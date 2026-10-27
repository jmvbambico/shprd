//! Archive refs, bundles and restore.
//!
//! The janitor archives before it deletes: a branch is first pointed at by its
//! own ref, `refs/shprd/archive/<branch without the namespace>` — so
//! `shprd/r1/t1` is held by `refs/shprd/archive/r1/t1` and a second attempt,
//! `shprd/r1/t1-retry`, by `refs/shprd/archive/r1/t1-retry` — and only then
//! removed. Two live branches for one task therefore never collide. The ref
//! holds the commits alive, so every deletion is reversible with [`restore`],
//! which rebuilds the task's conventional branch from its own ref.

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::git::{Git, GitError};
use crate::worktree::{self, Layout};

/// Prefix every archive ref carries.
pub const ARCHIVE_PREFIX: &str = "refs/shprd/archive/";

/// An archive ref, and the commit it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Archived {
    /// The full ref name, e.g. `refs/shprd/archive/m1/M1.2`.
    pub ref_name: String,
    /// The commit the ref points at.
    pub sha: String,
}

/// Why an archive operation could not be completed.
#[derive(Debug, Error)]
pub enum ArchiveError {
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
    /// The archive ref already holds a different commit.
    #[error("archive ref {ref_name:?} already holds {found}, not {wanted}")]
    ArchiveExists {
        /// The ref that was already there.
        ref_name: String,
        /// The commit it holds.
        found: String,
        /// The commit that was offered.
        wanted: String,
    },
    /// The archive ref does not exist, so there is nothing to restore.
    #[error("no archive ref {ref_name:?}")]
    MissingArchive {
        /// The ref that was looked for.
        ref_name: String,
    },
    /// A branch of that name already exists.
    #[error("branch {branch:?} already exists")]
    BranchExists {
        /// The branch name.
        branch: String,
    },
    /// An output path could not be prepared.
    #[error("could not create {path:?}: {source}")]
    Io {
        /// The path that failed.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
}

/// The archive ref for one task of one run: `<refs/shprd/archive>/<run>/<task>`.
///
/// That is the ref of the task's conventional branch
/// `<namespace><run>/<task>`, so it is the ref [`restore`] reads. An extra
/// attempt like `<namespace><run>/<task>-retry` archives to its own ref, named
/// by [`archive_ref_for_branch`].
///
/// # Panics
///
/// Never: the ids are validated by [`restore`] before this is called, and the
/// format cannot fail.
pub fn archive_ref_name(run: &str, task: &str) -> String {
    format!("{ARCHIVE_PREFIX}{run}/{task}")
}

/// The archive ref that belongs to `branch`: `<refs/shprd/archive>/<branch
/// without the namespace>`, so `shprd/r1/t1` maps to
/// `refs/shprd/archive/r1/t1`.
///
/// Refuses a branch outside [`Layout::namespace`], and one whose remainder is
/// not a plain ref path, so shprd can only ever write inside its own archive
/// namespace.
pub fn archive_ref_for_branch(layout: &Layout, branch: &str) -> Result<String, ArchiveError> {
    let rest = match branch.strip_prefix(&layout.namespace) {
        Some(rest) if !rest.is_empty() => rest,
        _ => return Err(invalid_branch(branch)),
    };
    if rest.contains("..")
        || !rest
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'))
    {
        return Err(invalid_branch(branch));
    }
    Ok(format!("{ARCHIVE_PREFIX}{rest}"))
}

/// The error for a branch shprd may not archive.
fn invalid_branch(branch: &str) -> ArchiveError {
    ArchiveError::InvalidId {
        what: "branch",
        id: branch.to_string(),
    }
}

/// True when `name` is a ref shprd's own archive namespace holds.
pub fn is_archive_ref(name: &str) -> bool {
    name.starts_with(ARCHIVE_PREFIX)
}

/// Points `branch`'s archive ref at its tip.
///
/// Each branch archives to its own ref, so two live branches for one task never
/// share one. Re-archiving the same commit is a no-op; archiving a different
/// one is refused, because moving an archive ref would lose the recorded
/// history it exists to keep.
pub fn archive(repo: &Path, layout: &Layout, branch: &str) -> Result<Archived, ArchiveError> {
    let git = Git::new(repo);
    let wanted = git.rev_parse(branch)?;
    let ref_name = archive_ref_for_branch(layout, branch)?;
    if let Some(found) = ref_sha(&git, &ref_name)? {
        if found == wanted {
            return Ok(Archived {
                ref_name,
                sha: found,
            });
        }
        return Err(ArchiveError::ArchiveExists {
            ref_name,
            found,
            wanted,
        });
    }
    git.run(&["update-ref", &ref_name, &wanted])?;
    Ok(Archived {
        ref_name,
        sha: wanted,
    })
}

/// Writes a bundle of `<base>..<branch>` to `out`.
///
/// Returns `Ok(None)`, and writes no file, when the range is empty: there is
/// nothing to hand over, and an empty bundle would fail to apply.
pub fn bundle(
    repo: &Path,
    base: &str,
    branch: &str,
    out: &Path,
) -> Result<Option<PathBuf>, ArchiveError> {
    let git = Git::new(repo);
    let count = git.run(&["rev-list", "--count", &format!("{base}..{branch}")])?;
    if count.trim() == "0" {
        return Ok(None);
    }
    if let Some(parent) = out.parent()
        && let Err(source) = std::fs::create_dir_all(parent)
    {
        return Err(ArchiveError::Io {
            path: parent.to_path_buf(),
            source,
        });
    }
    let out_text = out_text(out)?;
    git.run(&["bundle", "create", &out_text, &format!("{base}..{branch}")])?;
    Ok(Some(out.to_path_buf()))
}

/// Recreates the task's branch at the commit its archive ref holds.
///
/// Refuses when the branch already exists: a restore never overwrites work.
/// Returns the branch name.
pub fn restore(
    repo: &Path,
    layout: &Layout,
    run: &str,
    task: &str,
) -> Result<String, ArchiveError> {
    check_id("run", run)?;
    check_id("task", task)?;
    let git = Git::new(repo);
    let ref_name = archive_ref_name(run, task);
    let sha = match ref_sha(&git, &ref_name)? {
        Some(sha) => sha,
        None => return Err(ArchiveError::MissingArchive { ref_name }),
    };
    let branch = layout.branch_name(run, task);
    if git.branch_exists(&branch)? {
        return Err(ArchiveError::BranchExists { branch });
    }
    git.run(&["branch", &branch, &sha])?;
    Ok(branch)
}

/// Deletes an archive ref. The commits survive as long as another ref or a
/// bundle holds them, which is why the janitor exports one before expiring.
pub fn expire(repo: &Path, ref_name: &str) -> Result<(), ArchiveError> {
    if !is_archive_ref(ref_name) {
        return Err(ArchiveError::InvalidId {
            what: "archive ref",
            id: ref_name.to_string(),
        });
    }
    Git::new(repo).run(&["update-ref", "-d", ref_name])?;
    Ok(())
}

/// The commit a ref points at, or `None` when it does not exist.
pub fn ref_sha(git: &Git, ref_name: &str) -> Result<Option<String>, ArchiveError> {
    if !is_archive_ref(ref_name) {
        return Err(ArchiveError::InvalidId {
            what: "archive ref",
            id: ref_name.to_string(),
        });
    }
    match git.run(&[
        "rev-parse",
        "--verify",
        "--quiet",
        &format!("{ref_name}^{{commit}}"),
    ]) {
        Ok(sha) if !sha.is_empty() => Ok(Some(sha)),
        Ok(_) => Ok(None),
        // `--quiet` turns a missing ref into a non-zero exit with no output.
        Err(GitError::Failed { .. }) => Ok(None),
        Err(err) => Err(ArchiveError::Git(err)),
    }
}

/// Rejects an id that is empty, uses anything but `[A-Za-z0-9._-]`, or
/// contains `..`.
fn check_id(what: &'static str, id: &str) -> Result<(), ArchiveError> {
    if worktree::valid_id(id) {
        return Ok(());
    }
    Err(ArchiveError::InvalidId {
        what,
        id: id.to_string(),
    })
}

/// A path as git should see it.
fn out_text(path: &Path) -> Result<String, ArchiveError> {
    match path.to_str() {
        Some(text) => Ok(text.to_string()),
        None => Err(ArchiveError::InvalidId {
            what: "path",
            id: path.display().to_string(),
        }),
    }
}
