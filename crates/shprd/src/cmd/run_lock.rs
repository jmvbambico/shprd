//! The run's exclusive integration lock, shared by every verb that writes the
//! integration worktree or judges it.
//!
//! `integrate` cherry-picks onto the branch, `sync` merges the base into it,
//! `gate` reads its head and runs commands in it, and `land` reads the head it
//! pushes. Any two of those at once race over one worktree and one index, and a
//! reader can see a half-merged tree as if it were a result. So they all take
//! this lock: one run, one writer, at a time.

use std::path::{Path, PathBuf};

use anyhow::Context;

use crate::ctx::CliError;

/// One run's exclusive integration lock, released when it is dropped.
///
/// The lock is an advisory `flock` on a file under `<home>/.shprd/locks`, so it
/// is held by this process and not by a stray directory or pid file: the OS
/// drops it when the process exits, however it exits.
pub struct RunLock {
    /// The open file the lock sits on. Closing it releases the lock.
    _file: std::fs::File,
}

/// Takes the run's integration lock, blocking until it is free.
///
/// Two verbs on the same run would otherwise write the same worktree at the
/// same time, and both would see the head the other is about to move. A caller
/// therefore takes this before it reads the head, so whatever landed while it
/// waited is part of what it works on.
pub fn lock_run(home: &Path, repo_name: &str, run: &str) -> Result<RunLock, CliError> {
    let path = lock_path(home, repo_name, run);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating {}", dir.display()))
            .map_err(CliError::other)?;
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))
        .map_err(CliError::other)?;
    file.lock()
        .with_context(|| format!("locking {}", path.display()))
        .map_err(CliError::other)?;
    Ok(RunLock { _file: file })
}

/// The file the run's lock sits on.
///
/// The repository name and run id are in the name so two repos, or two runs of
/// one repo, never wait on each other.
fn lock_path(home: &Path, repo_name: &str, run: &str) -> PathBuf {
    home.join(".shprd")
        .join("locks")
        .join(format!("{repo_name}-{run}-int.lock"))
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    /// A temp home, kept alive by the returned directory.
    fn home() -> (tempfile::TempDir, PathBuf) {
        match tempfile::TempDir::new() {
            Ok(temp) => {
                let path = temp.path().to_path_buf();
                (temp, path)
            }
            Err(err) => panic!("tempdir: {err}"),
        }
    }

    /// The lock is named for the repo and the run, so runs cannot collide.
    #[test]
    fn the_lock_file_is_named_for_the_repo_and_the_run() {
        let (_temp, home) = home();
        let first = lock_path(&home, "repo", "r1");
        let other_run = lock_path(&home, "repo", "r2");
        let other_repo = lock_path(&home, "other", "r1");
        assert_ne!(first, other_run, "two runs must not share a lock");
        assert_ne!(first, other_repo, "two repos must not share a lock");
        assert_eq!(
            first,
            home.join(".shprd").join("locks").join("repo-r1-int.lock")
        );
    }

    /// A second holder blocks until the first is dropped.
    #[test]
    fn a_second_holder_waits_for_the_first_to_release() {
        let (_temp, home) = home();
        let held = match lock_run(&home, "repo", "r1") {
            Ok(lock) => lock,
            Err(err) => panic!("first lock: {err:#}"),
        };
        let (tx, rx) = mpsc::channel();
        let waiting = std::thread::spawn({
            let home = home.clone();
            move || {
                let second = lock_run(&home, "repo", "r1");
                let _ = tx.send(second.is_ok());
            }
        });
        match rx.recv_timeout(Duration::from_millis(300)) {
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            other => panic!("the second holder did not wait: {other:?}"),
        }
        drop(held);
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(true) => {}
            other => panic!("the second holder never got the lock: {other:?}"),
        }
        if let Err(err) = waiting.join() {
            panic!("the waiting thread panicked: {err:?}");
        }
    }
}
