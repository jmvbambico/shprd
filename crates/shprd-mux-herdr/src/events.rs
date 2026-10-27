//! Events, by polling `herdr api snapshot` and diffing it.
//!
//! herdr 0.9.5 does have an event stream — `events.subscribe` and `events.wait`
//! exist in the socket API — but the CLI exposes no way to call them: `herdr
//! api` offers only `snapshot` and `schema`. So this adapter polls the snapshot
//! on a short interval and reports what changed between two polls: an agent
//! whose status word changed, and a pane that disappeared.
//!
//! The consequence is documented on [`shprd_mux::Capabilities::resumable_events`]
//! (which this adapter answers `false`): a state that changed and changed back
//! between two polls is never seen, and nothing produced before the first call
//! is replayed. The orchestrator treats events as hints and still polls state
//! before it acts, so this is a degradation, not a correctness hole.

use serde_json::Value;
use shprd_mux::{MuxError, MuxEvent, PaneId, Result};
use std::collections::BTreeMap;
use std::sync::MutexGuard;
use std::time::{Duration, Instant};

use crate::HerdrMux;
use crate::panes::map_status;

/// How long to wait between two snapshots while [`crate::Mux::next_events`]
/// waits for something to happen.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// The part of a snapshot this adapter remembers: each pane's reported status.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Snapshot {
    /// Pane id to the status word herdr reported for it.
    pub(crate) panes: BTreeMap<String, String>,
}

impl Snapshot {
    /// Read `panes[]` out of a `session_snapshot` result.
    pub(crate) fn from_result(result: &Value) -> Result<Self> {
        let snapshot = result.get("snapshot").ok_or_else(|| {
            MuxError::command("read snapshot", "herdr answered without a snapshot")
        })?;
        let panes = snapshot
            .get("panes")
            .and_then(Value::as_array)
            .ok_or_else(|| MuxError::command("read snapshot", "herdr answered without panes"))?;
        let mut seen = Self::default();
        for pane in panes {
            let (Some(id), Some(status)) = (
                pane.get("pane_id").and_then(Value::as_str),
                pane.get("agent_status").and_then(Value::as_str),
            ) else {
                continue;
            };
            seen.panes.insert(id.to_string(), status.to_string());
        }
        Ok(seen)
    }

    /// What changed between `previous` and `self`.
    ///
    /// Panes are compared in id order so the event list is stable; a pane that
    /// vanished can only be "its agent is gone", which is what
    /// [`MuxEvent::PaneExited`] means.
    pub(crate) fn changes_from(&self, previous: &Snapshot) -> Vec<MuxEvent> {
        let mut events = Vec::new();
        for (pane, status) in &self.panes {
            match previous.panes.get(pane) {
                Some(before) if before == status => {}
                _ => events.push(MuxEvent::AgentState {
                    pane: PaneId::new(pane.clone()),
                    state: map_status(status),
                }),
            }
        }
        for pane in previous.panes.keys() {
            if !self.panes.contains_key(pane) {
                events.push(MuxEvent::PaneExited {
                    pane: PaneId::new(pane.clone()),
                });
            }
        }
        events
    }
}

impl HerdrMux {
    /// One live snapshot from herdr.
    fn snapshot(&self) -> Result<Snapshot> {
        let result = self.call("read snapshot", &["api", "snapshot"])?;
        Snapshot::from_result(&result)
    }

    /// The remembered snapshot, recovering from a poisoned lock instead of
    /// panicking: a previous caller's panic must not stop the event pump.
    fn seen(&self) -> MutexGuard<'_, Option<Snapshot>> {
        match self.seen.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Poll until something changed or `timeout` expires.
    ///
    /// The first call ever only records a baseline and reports nothing, because
    /// "the state of every pane at startup" is not a change; it still waits out
    /// the timeout so the caller's contract ("wait up to `timeout`") holds.
    pub(crate) fn poll_events(&self, timeout: Duration) -> Result<Vec<MuxEvent>> {
        let started = Instant::now();
        loop {
            let current = self.snapshot()?;
            let events = {
                let mut previous = self.seen();
                let events = previous
                    .as_ref()
                    .map(|previous| current.changes_from(previous))
                    .unwrap_or_default();
                *previous = Some(current);
                events
            };
            if !events.is_empty() {
                return Ok(events);
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Ok(Vec::new());
            }
            std::thread::sleep(POLL_INTERVAL.min(remaining));
        }
    }
}

#[cfg(test)]
mod tests {
    // Tests may panic: a failed assertion is the point.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use shprd_mux::AgentState;

    const SNAPSHOT: &str = include_str!("../tests/fixtures/snapshot.json");

    fn snapshot() -> Snapshot {
        let value: Value = serde_json::from_str(SNAPSHOT).unwrap();
        Snapshot::from_result(&value["result"]).unwrap()
    }

    fn with_status(mut snapshot: Snapshot, pane: &str, status: &str) -> Snapshot {
        snapshot.panes.insert(pane.to_string(), status.to_string());
        snapshot
    }

    #[test]
    fn every_pane_in_the_snapshot_is_remembered_with_its_status() {
        let seen = snapshot();
        assert_eq!(seen.panes.len(), 5);
        assert_eq!(seen.panes.get("w1:p1").map(String::as_str), Some("working"));
        assert_eq!(seen.panes.get("w2:p1").map(String::as_str), Some("unknown"));
    }

    #[test]
    fn an_unchanged_snapshot_reports_nothing() {
        let seen = snapshot();
        assert!(seen.changes_from(&seen).is_empty());
    }

    #[test]
    fn a_changed_status_is_one_agent_state_event() {
        let before = snapshot();
        let after = with_status(snapshot(), "w1:p1", "idle");
        assert_eq!(
            after.changes_from(&before),
            vec![MuxEvent::AgentState {
                pane: PaneId::new("w1:p1"),
                state: AgentState::Idle,
            }]
        );
    }

    #[test]
    fn a_pane_that_disappeared_has_exited() {
        let before = snapshot();
        let mut after = snapshot();
        after.panes.remove("w2:p1");
        assert_eq!(
            after.changes_from(&before),
            vec![MuxEvent::PaneExited {
                pane: PaneId::new("w2:p1"),
            }]
        );
    }

    #[test]
    fn a_new_pane_reports_the_state_it_appeared_in() {
        let before = snapshot();
        let after = with_status(snapshot(), "w1:p9", "working");
        assert_eq!(
            after.changes_from(&before),
            vec![MuxEvent::AgentState {
                pane: PaneId::new("w1:p9"),
                state: AgentState::Working,
            }]
        );
    }

    #[test]
    fn a_snapshot_without_panes_is_an_error() {
        let value: Value = serde_json::from_str(SNAPSHOT).unwrap();
        let error = Snapshot::from_result(&value["id"]).unwrap_err();
        assert!(error.to_string().contains("without a snapshot"));
    }
}
