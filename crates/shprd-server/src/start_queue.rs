//! Starts that waited for a worker slot.
//!
//! A start the `tasks.max_running` cap holds back — a retry, a start action,
//! `shprd agent spawn` — is not refused: the task is written here and the pass
//! starts it as soon as a slot is free. The file is one task id per line in the
//! service's state directory, so the wait survives a restart.

use std::fs;
use std::path::{Path, PathBuf};

/// What a start's answer begins with when the cap held it back.
///
/// The spawn path words its answer with this prefix and the pass reads it back
/// to tell a start that waited from one that happened.
pub const QUEUED_PREFIX: &str = "queued:";

/// The file under `state_dir` that holds the waiting starts.
fn path(state_dir: &Path) -> PathBuf {
    state_dir.join("start-queue")
}

/// The tasks waiting for a slot, oldest first. An unreadable file is no queue.
pub fn waiting(state_dir: &Path) -> Vec<String> {
    fs::read_to_string(path(state_dir))
        .map(|text| {
            text.lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Writes the queue as it is now; an empty queue removes the file.
fn store(state_dir: &Path, tasks: &[String]) -> std::io::Result<()> {
    if tasks.is_empty() {
        return match fs::remove_file(path(state_dir)) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err),
            _ => Ok(()),
        };
    }
    fs::create_dir_all(state_dir)?;
    fs::write(path(state_dir), format!("{}\n", tasks.join("\n")))
}

/// Puts `task` at the back of the queue, once.
pub fn enqueue(state_dir: &Path, task: &str) -> std::io::Result<()> {
    let mut tasks = waiting(state_dir);
    if tasks.iter().any(|queued| queued == task) {
        return Ok(());
    }
    tasks.push(task.to_string());
    store(state_dir, &tasks)
}

/// Takes `task` out of the queue.
pub fn dequeue(state_dir: &Path, task: &str) -> std::io::Result<()> {
    let tasks = waiting(state_dir);
    let kept: Vec<String> = tasks
        .iter()
        .filter(|queued| *queued != task)
        .cloned()
        .collect();
    if kept.len() == tasks.len() {
        return Ok(());
    }
    store(state_dir, &kept)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn a_task_is_queued_once_and_leaves_in_order() {
        let dir = tempfile::tempdir().expect("a dir");
        assert!(waiting(dir.path()).is_empty());
        enqueue(dir.path(), "T1").expect("queued");
        enqueue(dir.path(), "T2").expect("queued");
        enqueue(dir.path(), "T1").expect("queued again");
        assert_eq!(waiting(dir.path()), ["T1", "T2"]);
        dequeue(dir.path(), "T1").expect("left");
        assert_eq!(waiting(dir.path()), ["T2"]);
        dequeue(dir.path(), "T2").expect("left");
        assert!(!path(dir.path()).exists());
    }
}
