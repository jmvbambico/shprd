//! Pure watchdog state machine: signals in, actions out.
//!
//! Nothing here reads a clock or touches the filesystem; callers pass
//! [`Inputs`] with `now_ms` already filled in, which keeps the decision logic
//! deterministic and testable with plain numbers.

use crate::records::RecordError;
use crate::screen::ScreenSignal;
use crate::serve::Status;

/// Where a dry verdict came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// A provider error record in the session's transcript.
    Record,
    /// The serve instance keeps retrying a failed turn.
    Serve,
    /// The pane's screen matched a configured dry pattern.
    Screen,
    /// The agent stopped producing output for too long.
    Silence,
    /// The agent's process is gone.
    Exit,
}

/// What the watchdog currently believes about one agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Health {
    /// Output is moving; nothing to do.
    Working,
    /// The screen has not moved for a while, but not long enough to act.
    Suspect {
        /// When the suspicion started, in epoch milliseconds.
        since_ms: u64,
    },
    /// The agent is not usable and a hand-off is warranted.
    Dry {
        /// When the dry verdict was reached, in epoch milliseconds.
        since_ms: u64,
        /// Human-readable explanation, shown to the operator.
        reason: String,
        /// When the next canary probe is due, in epoch milliseconds.
        retry_at_ms: u64,
        /// Which signal produced this verdict.
        source: Source,
        /// Whether the one probe for this attempt was already sent.
        canary_sent: bool,
    },
    /// A canary succeeded: the agent is believed usable again.
    Available,
}

/// Something the caller must do as a result of [`Watch::step`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Mark the session dry in persisted state.
    MarkDry {
        /// Why the session went dry, for the operator.
        reason: String,
        /// Provider-stated reset time, in epoch milliseconds, if it gave one.
        reset_at_ms: Option<u64>,
    },
    /// Hand the work to another agent.
    HandOff,
    /// Probe the dry agent with a cheap request to see whether it recovered.
    Canary,
    /// Clear the dry mark: the agent is usable again.
    MarkAvailable,
}

/// Every signal the watchdog sees at one polling instant.
#[derive(Debug, Clone, PartialEq)]
pub struct Inputs {
    /// The current time, in epoch milliseconds.
    pub now_ms: u64,
    /// The newest provider error in the record, if any.
    pub record_error: Option<RecordError>,
    /// The serve instance's view of the session, if it reports one.
    pub serve: Option<Status>,
    /// The pane's screen signal, if the pane could be read.
    pub screen: Option<ScreenSignal>,
    /// Whether the agent's process has ended.
    pub process_exited: bool,
}

/// Thresholds and backoff schedules for the watchdog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    /// How long the screen may sit unchanged before the agent is suspect.
    pub suspect_after_ms: u64,
    /// How long an agent may stay suspect before it is declared dry.
    pub dry_after_suspect_ms: u64,
    /// Backoff for each tolerated serve retry, before going dry.
    pub transient_backoff_ms: Vec<u64>,
    /// Backoff for successive dry attempts, indexed by dry count.
    pub dry_backoff_ms: Vec<u64>,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            suspect_after_ms: 180_000,
            dry_after_suspect_ms: 120_000,
            transient_backoff_ms: vec![60_000, 120_000, 240_000, 480_000],
            dry_backoff_ms: vec![900_000, 1_800_000, 3_600_000],
        }
    }
}

/// Carries the watchdog's memory from one [`Watch::step`] to the next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watch {
    /// What the watchdog believes right now; also exposed to readers.
    pub health: Health,
    /// Serve retries tolerated since the last healthy observation.
    transient_attempts: usize,
    /// How many times the agent has gone dry in this run of trouble.
    dry_count: usize,
}

impl Default for Watch {
    fn default() -> Self {
        Self::new()
    }
}

impl Watch {
    /// A watchdog that has seen nothing yet.
    pub fn new() -> Self {
        Self {
            health: Health::Working,
            transient_attempts: 0,
            dry_count: 0,
        }
    }
}
