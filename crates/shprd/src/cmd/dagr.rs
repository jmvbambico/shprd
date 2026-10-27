//! `shprd dagr sync`: project the run as a dagr v3 document and write it.
//!
//! The document is `<root>/.dagr/run.json`. It is checked with a `dagr` binary
//! when one is available, and written unchecked (with one note on stderr) when
//! it is not, so a run without dagr installed still leaves a document behind.

use std::path::{Path, PathBuf};

use anyhow::anyhow;
use shprd_core::log::{Clock, SystemClock};
use shprd_views::dagr::{ViewError, project, write_atomic};

use crate::ctx::{CliError, Ctx};

/// Environment variable naming the checker to use.
pub const CHECKER_ENV: &str = "SHPRD_DAGR_BIN";

/// Name of the checker to look for on `PATH`.
const CHECKER_NAME: &str = "dagr";

/// Where the document is written: `<root>/.dagr/run.json`.
pub fn run_json_path(root: &Path) -> PathBuf {
    root.join(".dagr").join("run.json")
}

/// The checker to validate with: `$SHPRD_DAGR_BIN`, else `dagr` on `PATH`,
/// else nothing.
///
/// The `PATH` search is done here rather than by the OS because the choice has
/// to be observable: a caller can tell whether the document was checked.
pub fn find_checker() -> Option<PathBuf> {
    if let Some(value) = std::env::var_os(CHECKER_ENV) {
        let path = PathBuf::from(value);
        if !path.as_os_str().is_empty() {
            return Some(path);
        }
    }
    search_path()
}

/// The first executable `<dir>/dagr` on `PATH`.
fn search_path() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let candidate = dir.join(CHECKER_NAME);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

/// Whether `path` is an executable file.
#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(meta) => meta.is_file() && meta.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

/// Whether `path` is a file.
#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|meta| meta.is_file())
        .unwrap_or(false)
}

/// The document projected from the current log, or `None` when the run has
/// not started yet.
fn project_run(ctx: &Ctx) -> Result<Option<serde_json::Value>, CliError> {
    let (state, events) = ctx.load()?;
    let now_ms = SystemClock.now_ms();
    match project(&state, &events, now_ms) {
        Ok(doc) => Ok(Some(doc)),
        Err(ViewError::NoRun) => Ok(None),
        Err(err) => Err(CliError::other(anyhow!(err).context("projecting the run"))),
    }
}

/// Projects the run and writes `<root>/.dagr/run.json`, returning the path it
/// wrote, or `None` when the run has not started.
///
/// Writes unchecked and prints `dagr not found; wrote unchecked` on stderr when
/// no checker is available. A checker that rejects the document is an error
/// carrying its findings, and leaves the previous document in place.
///
/// # Errors
///
/// [`CliError::Other`] when the log cannot be read, the projection cannot be
/// written, or the checker rejects it.
pub fn sync(ctx: &Ctx) -> Result<Option<PathBuf>, CliError> {
    let Some(doc) = project_run(ctx)? else {
        return Ok(None);
    };
    let path = run_json_path(&ctx.root);
    let checker = find_checker();
    match write_atomic(&doc, &path, checker.as_deref()) {
        Ok(()) => {
            if checker.is_none() {
                eprintln!("dagr not found; wrote unchecked");
            }
            Ok(Some(path))
        }
        Err(err) => Err(CliError::other(
            anyhow!(err).context(format!("syncing {}", path.display())),
        )),
    }
}

/// The body of `shprd dagr sync`: sync, and report where the document went.
///
/// With no run yet there is nothing to project, so this says so on stderr and
/// succeeds.
pub fn sync_verb(ctx: &Ctx) -> Result<(), CliError> {
    let Some(path) = sync(ctx)? else {
        eprintln!("no run yet; nothing to project");
        return Ok(());
    };
    if ctx.json {
        let doc = serde_json::json!({
            "path": path.display().to_string(),
        });
        let text = serde_json::to_string(&doc)
            .map_err(|err| CliError::other(anyhow!(err).context("serializing the result")))?;
        println!("{text}");
    } else {
        println!("wrote {}", path.display());
    }
    Ok(())
}
