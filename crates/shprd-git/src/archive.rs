//! Archive refs, bundles, folder copies and restore.
//!
//! The janitor archives before it deletes: a branch is first pointed at by its
//! own ref, `refs/shprd/archive/<branch without the namespace>` — so
//! `shprd/r1/t1` is held by `refs/shprd/archive/r1/t1` and a second attempt,
//! `shprd/r1/t1-retry`, by `refs/shprd/archive/r1/t1-retry` — and only then
//! removed. Two live branches for one task therefore never collide. The ref
//! holds the commits alive, so every deletion is reversible with [`restore`],
//! which rebuilds the task's conventional branch from its own ref. A retry
//! reuses one branch name for every attempt, so each attempt's commits are kept
//! under their own ref, named by [`archive_ref_for_attempt`]. A folder
//! that is not a git object — a run's review packets — is archived by
//! [`archive_dir`], which copies it aside before the janitor deletes it.

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
    /// There was nothing to archive at the path.
    #[error("{path:?} is not a folder")]
    NotAFolder {
        /// The path that was not a folder.
        path: PathBuf,
    },
    /// A folder being archived holds something that is neither a file nor a
    /// folder.
    ///
    /// Such an entry is refused rather than followed or skipped: following it
    /// could copy a file from outside the folder, and skipping it would write
    /// an archive that does not hold what it claims to.
    #[error("{path:?} is not a plain file or folder")]
    UnsupportedEntry {
        /// The entry that was neither.
        path: PathBuf,
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

/// The archive ref that holds one attempt's commits.
///
/// The first attempt keeps the task's own ref, [`archive_ref_name`], so
/// [`restore`] still rebuilds the branch from it. Every later attempt is
/// suffixed with its number, because each attempt's branch tip is a different
/// commit and an archive ref is never moved: without the suffix, a retry would
/// have to overwrite the attempt before it, and [`archive_to`] would refuse.
///
/// # Panics
///
/// Never: it is a format, and the ids are validated by the caller.
pub fn archive_ref_for_attempt(run: &str, task: &str, attempt: u32) -> String {
    match attempt {
        0 | 1 => archive_ref_name(run, task),
        n => format!("{}-a{n}", archive_ref_name(run, task)),
    }
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
    let ref_name = archive_ref_for_branch(layout, branch)?;
    archive_to(repo, branch, &ref_name)
}

/// Points `ref_name` at `branch`'s tip, under the same rule as [`archive`].
///
/// [`archive`] names the ref after the branch, which is one ref per task. A
/// retry replaces one attempt's branch and keeps each attempt's commits under
/// [`archive_ref_for_attempt`]'s name instead, so that attempt 2's archive does
/// not have to overwrite attempt 1's.
///
/// A ref outside [`ARCHIVE_PREFIX`] is refused: shprd writes inside its own
/// namespace and nowhere else.
pub fn archive_to(repo: &Path, branch: &str, ref_name: &str) -> Result<Archived, ArchiveError> {
    if !is_archive_ref(ref_name) {
        return Err(ArchiveError::InvalidId {
            what: "archive ref",
            id: ref_name.to_string(),
        });
    }
    let git = Git::new(repo);
    let wanted = git.rev_parse(branch)?;
    if let Some(found) = ref_sha(&git, ref_name)? {
        if found == wanted {
            return Ok(Archived {
                ref_name: ref_name.to_string(),
                sha: found,
            });
        }
        return Err(ArchiveError::ArchiveExists {
            ref_name: ref_name.to_string(),
            found,
            wanted,
        });
    }
    git.run(&["update-ref", ref_name, &wanted])?;
    Ok(Archived {
        ref_name: ref_name.to_string(),
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

/// Copies the folder `dir` to `out`, so the original can be deleted after.
///
/// The copy is built beside `out` and renamed into place only once it is
/// complete, so a failed copy never replaces an earlier archive. The caller
/// deletes `dir` only after this returns, which is what makes the copy the
/// archive that licenses the deletion.
///
/// Nothing is followed out of `dir`: an entry that is not a plain file or
/// folder is refused rather than copied, and an empty folder is archived as an
/// empty folder.
pub fn archive_dir(dir: &Path, out: &Path) -> Result<(), ArchiveError> {
    // The root is checked without following a link, exactly as every entry
    // inside it is, so an archive never reads through a replaced folder.
    match std::fs::symlink_metadata(dir) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => {
            return Err(ArchiveError::NotAFolder {
                path: dir.to_path_buf(),
            });
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(ArchiveError::NotAFolder {
                path: dir.to_path_buf(),
            });
        }
        Err(source) => {
            return Err(ArchiveError::Io {
                path: dir.to_path_buf(),
                source,
            });
        }
    }
    if let Some(parent) = out.parent().filter(|path| !path.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|source| ArchiveError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let staging = staging_path(out);
    remove_any(&staging)?;
    copy_tree(dir, &staging)?;
    remove_any(out)?;
    std::fs::rename(&staging, out).map_err(|source| ArchiveError::Io {
        path: out.to_path_buf(),
        source,
    })?;
    Ok(())
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

/// Where a folder archive is built before it is renamed into place.
///
/// It sits beside `out` on the same filesystem, so the rename is atomic and a
/// failed copy leaves nothing half-visible under the archive's own name.
fn staging_path(out: &Path) -> PathBuf {
    match out.file_name().and_then(|name| name.to_str()) {
        Some(name) => out.with_file_name(format!("{name}.partial")),
        None => out.with_extension("partial"),
    }
}

/// Removes `path`, whatever it is, treating a missing one as already gone.
fn remove_any(path: &Path) -> Result<(), ArchiveError> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(ArchiveError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let removed = if meta.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    removed.map_err(|source| ArchiveError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Copies every plain file and folder under `from` into `to`.
fn copy_tree(from: &Path, to: &Path) -> Result<(), ArchiveError> {
    std::fs::create_dir_all(to).map_err(|source| ArchiveError::Io {
        path: to.to_path_buf(),
        source,
    })?;
    let entries = std::fs::read_dir(from).map_err(|source| ArchiveError::Io {
        path: from.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| ArchiveError::Io {
            path: from.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path).map_err(|source| ArchiveError::Io {
            path: path.clone(),
            source,
        })?;
        let target = to.join(entry.file_name());
        if meta.is_dir() {
            copy_tree(&path, &target)?;
        } else if meta.is_file() {
            std::fs::copy(&path, &target).map_err(|source| ArchiveError::Io {
                path: path.clone(),
                source,
            })?;
        } else {
            return Err(ArchiveError::UnsupportedEntry { path });
        }
    }
    Ok(())
}
