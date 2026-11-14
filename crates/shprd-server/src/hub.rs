//! Change notifications: one counter and one wake-up.
//!
//! Every writer that appends through the service publishes the new sequence
//! here, and every long poll and socket waits on it. The log stays the source
//! of truth: a waiter that wakes reads the log, so an event appended by another
//! process is still delivered, only one tick later.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::Notify;

/// The service's change counter and its wake-up.
#[derive(Debug, Clone, Default)]
pub struct Hub {
    last: Arc<AtomicU64>,
    notify: Arc<Notify>,
}

impl Hub {
    /// A hub whose counter starts at zero.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records that the log now reaches `seq` and wakes every waiter.
    pub fn publish(&self, seq: u64) {
        self.last.fetch_max(seq, Ordering::SeqCst);
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
}
