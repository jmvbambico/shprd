//! The change file a Herdr mod watches.
//!
//! A Herdr desktop mod cannot read a stream: its `fetch` is buffered, and
//! `WebSocket`, `EventSource` and `AbortController` are all undefined (M7ef §2.11
//! measured it on a real window). Its live arm is the long poll
//! (`GET /api/changes`) plus `watchFolder`, so the service keeps one small file
//! under the state directory that it rewrites after **every** ledger write —
//! the API's own, the tick's, and a write made by another process that the
//! service's log watcher notices.
//!
//! The file is `<state>/shprd/changes` and holds one JSON object:
//!
//! ```json
//! {"seq": 42, "at_ms": 1763200000000}
//! ```
//!
//! `seq` is the log's last sequence at the write, `at_ms` is when the file was
//! written. A mod watches the folder, reads the file when a batch names it, and
//! re-reads `/api/state` only when the two numbers say something happened — which
//! is what keeps a panel from polling a fold it already has.
//!
//! The file is a hint, never the record: a write that fails is a warning, and no
//! decision reads it. It is deliberately not `service.json` (which a mod must not
//! confuse with the discovery record) and not the log itself (which is the
//! repository's, possibly large, and not in the state directory).

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use shprd_core::log::{Clock, SystemClock};

/// The change file's name in the state directory.
pub const FILE: &str = "changes";

/// Where the change file is, under a state directory.
pub fn path(state_dir: &Path) -> PathBuf {
    state_dir.join(FILE)
}

/// What the change file holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Touch {
    /// The log's last sequence when the file was written.
    pub seq: u64,
    /// When it was written, ms since the Unix epoch.
    pub at_ms: u64,
}

impl Touch {
    /// A touch of `seq` at `now_ms`.
    pub fn new(seq: u64, now_ms: u64) -> Self {
        Self { seq, at_ms: now_ms }
    }

    /// The touch as it is written to the file.
    pub fn to_json(self) -> String {
        serde_json::to_string(&self).unwrap_or_else(|_| String::from("{}"))
    }

    /// Reads the file, or `None` when it is absent or unreadable.
    ///
    /// Nothing depends on it: a missing file only means a mod that watches the
    /// folder has nothing to compare yet.
    pub fn read(state_dir: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(path(state_dir)).ok()?;
        serde_json::from_str(&text).ok()
    }
}

/// Writes the change file for `seq` in `state_dir`, replacing the old one.
///
/// The state directory is made when it is not there yet (`shprd tick` runs with
/// no service beside it), and the file is replaced by a rename, so a mod never
/// reads half of one. A failure is returned rather than ignored so the caller can
/// say it once at `warn`; the write itself never fails a request.
pub fn touch(state_dir: &Path, seq: u64) -> io::Result<Touch> {
    let now_ms = SystemClock.now_ms();
    touch_at(state_dir, seq, now_ms)
}

/// [`touch`] at a stated time, for a caller that has a clock of its own (a test).
pub fn touch_at(state_dir: &Path, seq: u64, now_ms: u64) -> io::Result<Touch> {
    let record = Touch::new(seq, now_ms);
    std::fs::create_dir_all(state_dir)?;
    let target = path(state_dir);
    // Unique per write: the API, the tick and a chat watcher can write at once.
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temporary = state_dir.join(format!("{FILE}.{}.{n}.tmp", std::process::id()));
    std::fs::write(&temporary, record.to_json())?;
    std::fs::rename(&temporary, &target)?;
    Ok(record)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn a_touch_is_written_and_read_back() {
        let dir = tempfile::tempdir().expect("temp dir");
        let state = dir.path().join("shprd");
        let written = touch_at(&state, 42, 1_763_200_000_000).expect("write");
        assert_eq!(written, Touch::new(42, 1_763_200_000_000));
        assert_eq!(Touch::read(&state), Some(written));
        assert_eq!(
            std::fs::read_to_string(path(&state)).expect("read"),
            r#"{"seq":42,"at_ms":1763200000000}"#
        );
    }

    #[test]
    fn a_missing_change_file_is_no_failure() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert_eq!(Touch::read(dir.path()), None);
    }

    #[test]
    fn a_second_touch_replaces_the_first() {
        let dir = tempfile::tempdir().expect("temp dir");
        touch_at(dir.path(), 1, 10).expect("first");
        touch_at(dir.path(), 2, 20).expect("second");
        assert_eq!(Touch::read(dir.path()), Some(Touch::new(2, 20)));
    }

    #[test]
    fn the_file_sits_in_the_state_directory() {
        assert_eq!(
            path(Path::new("/state/shprd")),
            PathBuf::from("/state/shprd/changes")
        );
    }
}
