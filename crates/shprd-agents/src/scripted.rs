//! Test-only helpers shared by the roster's scripted-CLI tests.
//!
//! Every test here stands a shell script in for a real agent CLI and then runs
//! it. Writing such a script straight to the path that is about to be executed
//! is what makes Linux CI flaky with `ETXTBSY`: another test thread's child can
//! inherit the open write descriptor between its `fork` and its `execve`, and
//! the kernel then refuses to execute the script for a few milliseconds. So the
//! script is written to a temporary name beside its destination, made
//! executable there, and renamed into place: the path that is executed is never
//! open for writing.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// The last temporary-script number this process handed out.
static LAST_TEMP: AtomicU64 = AtomicU64::new(0);

/// Writes `body` to `path` as an executable file, through a temporary name in
/// the same directory and a rename.
///
/// The rename stays inside one directory, so it is atomic and cannot cross a
/// filesystem, and the executable path is only ever reachable once it is
/// complete. The temporary file is removed the moment it has been renamed.
pub(crate) fn write_executable(path: &Path, body: &str) {
    let temp = temp_name(path);
    if let Err(error) = std::fs::write(&temp, body) {
        panic!("writing {} failed: {error}", temp.display());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::Permissions::from_mode(0o755);
        if let Err(error) = std::fs::set_permissions(&temp, mode) {
            panic!("chmod {} failed: {error}", temp.display());
        }
    }
    if let Err(error) = std::fs::rename(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        panic!(
            "renaming {} to {} failed: {error}",
            temp.display(),
            path.display()
        );
    }
}

/// A path beside `path`, unusable by any other write in this process or any
/// other, and removed by [`write_executable`] once it is renamed.
fn temp_name(path: &Path) -> PathBuf {
    let number = LAST_TEMP.fetch_add(1, Ordering::Relaxed);
    let name = match path.file_name().and_then(|name| name.to_str()) {
        Some(name) => format!(".{name}.{}.{number}.tmp", std::process::id()),
        None => format!(".script.{}.{number}.tmp", std::process::id()),
    };
    match path.parent() {
        Some(parent) => parent.join(name),
        None => PathBuf::from(name),
    }
}
