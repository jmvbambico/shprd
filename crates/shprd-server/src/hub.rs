//! Change notifications: one counter, one wake-up and one file.
//!
//! Every writer that appends through the service publishes the new sequence
//! here, and every long poll and socket waits on it. The log stays the source
//! of truth: a waiter that wakes reads the log, so an event appended by another
//! process is still delivered, only one tick later.
//!
//! A Herdr desktop mod cannot subscribe at all: its `fetch` is buffered and it has
//! no `WebSocket` or `EventSource` (M7ef §2.11 measured it on a real window). So a
//! publish also rewrites the change file ([`crate::changes`]) that a mod follows
//! with `watchFolder`. That file is a hint for a watcher, the wake-up is what the
//! long poll uses, and neither of them is ever the record.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::Notify;

/// The service's change counter, its wake-up, and the file it touches.
#[derive(Debug, Clone, Default)]
pub struct Hub {
    last: Arc<AtomicU64>,
    notify: Arc<Notify>,
    /// The state directory whose change file a publish rewrites, when the service
    /// was given one. A hub a test makes by hand touches no file.
    state_dir: Option<Arc<PathBuf>>,
}

impl Hub {
    /// A hub whose counter starts at zero and that touches no file.
    pub fn new() -> Self {
        Self::default()
    }

    /// A hub that also keeps `<state_dir>/changes` up to date for a watching mod.
    ///
    /// The service builds its hub this way: the file is what makes a Herdr mod's
    /// `watchFolder` arm work at all, since that runtime has no stream to read.
    pub fn touching(state_dir: impl Into<PathBuf>) -> Self {
        Self {
            last: Arc::new(AtomicU64::new(0)),
            notify: Arc::new(Notify::new()),
            state_dir: Some(Arc::new(state_dir.into())),
        }
    }

    /// The state directory this hub touches, when it touches one.
    pub fn change_dir(&self) -> Option<&Path> {
        self.state_dir.as_deref().map(PathBuf::as_path)
    }

    /// Records that the log now reaches `seq` and wakes every waiter.
    ///
    /// It also rewrites the change file, so a mod watching the state directory
    /// reads the same change the long poll was woken for. A file that cannot be
    /// written is a warning and nothing else: the wake-up already happened, and no
    /// client's answer depends on the file.
    pub fn publish(&self, seq: u64) {
        self.last.fetch_max(seq, Ordering::SeqCst);
        if let Some(dir) = self.change_dir()
            && let Err(err) = crate::changes::touch(dir, seq)
        {
            tracing::warn!(
                %err,
                dir = %dir.display(),
                "could not write the change file a watching mod reads"
            );
        }
        self.notify.notify_waiters();
    }

    /// The last sequence anyone published through this service.
    pub fn last(&self) -> u64 {
        self.last.load(Ordering::SeqCst)
    }

    /// Resolves on the next publish.
    ///
    /// A publish that lands before a waiter starts waiting is not stored, which
    /// is why every waiter also has a tick: the log is re-checked regardless, so
    /// a missed wake-up costs latency, never an event.
    pub async fn wait(&self) {
        self.notify.notified().await;
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[tokio::test]
    async fn publishing_wakes_a_waiter() {
        let hub = Hub::new();
        let waiter = hub.clone();
        let handle = tokio::spawn(async move {
            waiter.wait().await;
            waiter.last()
        });
        // Give the waiter a moment to start waiting before publishing.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        hub.publish(7);
        let seen = handle.await.expect("the waiter finished");
        assert_eq!(seen, 7);
    }

    #[test]
    fn the_counter_never_goes_backwards() {
        let hub = Hub::new();
        hub.publish(5);
        hub.publish(3);
        assert_eq!(hub.last(), 5);
    }

    #[test]
    fn a_hub_with_a_state_directory_touches_the_change_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let hub = Hub::touching(dir.path());
        hub.publish(12);
        let touch = crate::changes::Touch::read(dir.path()).expect("the change file");
        assert_eq!(touch.seq, 12);
    }

    #[test]
    fn a_hub_without_a_state_directory_writes_no_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let state = dir.path().join("shprd");
        let hub = Hub::new();
        hub.publish(12);
        assert!(!state.exists());
        assert!(crate::changes::Touch::read(&state).is_none());
    }
}
