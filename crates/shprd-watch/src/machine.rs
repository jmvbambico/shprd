//! Pure watchdog state machine: signals in, actions out.
//!
//! Nothing here reads a clock or touches the filesystem; callers pass
//! [`Inputs`] with `now_ms` already filled in, which keeps the decision logic
//! deterministic and testable with plain numbers.

use crate::records::{ErrorKind, RecordError};
use crate::screen::ScreenSignal;
use crate::serve::Status;
use shprd_mux::AgentState;

/// Backoff used when a [`Policy`] supplies an empty backoff list.
const FALLBACK_BACKOFF_MS: u64 = 900_000;

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

    /// The next dry backoff, consuming one entry of the schedule.
    fn next_dry_backoff(&mut self, p: &Policy) -> u64 {
        let last = p.dry_backoff_ms.len().saturating_sub(1);
        let step = p
            .dry_backoff_ms
            .get(self.dry_count.min(last))
            .copied()
            .unwrap_or(FALLBACK_BACKOFF_MS);
        self.dry_count = self.dry_count.saturating_add(1);
        step
    }

    /// Move into [`Health::Dry`] and report the actions that implies.
    fn go_dry(
        &mut self,
        now_ms: u64,
        source: Source,
        reason: String,
        reset_at_ms: Option<u64>,
        p: &Policy,
    ) -> Vec<Action> {
        let backoff = self.next_dry_backoff(p);
        let retry_at_ms = reset_at_ms.unwrap_or_else(|| now_ms.saturating_add(backoff));
        self.transient_attempts = 0;
        self.health = Health::Dry {
            since_ms: now_ms,
            reason: reason.clone(),
            retry_at_ms,
            source,
            canary_sent: false,
        };
        vec![
            Action::MarkDry {
                reason,
                reset_at_ms,
            },
            Action::HandOff,
        ]
    }

    /// Fold one polling instant into the health verdict.
    ///
    /// Rules run in priority order: exit, unretryable provider error, serve
    /// retries, a dry screen pattern, then silence. While already dry only the
    /// canary is considered.
    pub fn step(&mut self, i: &Inputs, p: &Policy) -> Vec<Action> {
        if let Health::Dry {
            retry_at_ms,
            canary_sent,
            ..
        } = &mut self.health
        {
            if *canary_sent || i.now_ms < *retry_at_ms {
                return Vec::new();
            }
            *canary_sent = true;
            return vec![Action::Canary];
        }

        if i.process_exited {
            let reason = String::from("agent process exited");
            return self.go_dry(i.now_ms, Source::Exit, reason, None, p);
        }

        if let Some(err) = &i.record_error
            && is_unretryable(err.kind)
        {
            let reason = err.message.clone();
            return self.go_dry(i.now_ms, Source::Record, reason, err.resets_at_ms, p);
        }

        if let Some(Status::Retry { message, .. }) = &i.serve {
            self.transient_attempts = self.transient_attempts.saturating_add(1);
            if self.transient_attempts > p.transient_backoff_ms.len() {
                let reason = message.clone();
                return self.go_dry(i.now_ms, Source::Serve, reason, None, p);
            }
        }

        let Some(screen) = &i.screen else {
            return Vec::new();
        };

        if let Some(pattern) = &screen.dry_pattern {
            let reason = pattern.clone();
            return self.go_dry(i.now_ms, Source::Screen, reason, None, p);
        }

        if matches!(screen.agent_state, AgentState::Idle | AgentState::Done) {
            self.health = Health::Working;
            return Vec::new();
        }

        let suspect_since = match &self.health {
            Health::Suspect { since_ms } => Some(*since_ms),
            _ => None,
        };

        if let Some(since_ms) = suspect_since {
            if screen.liveness.changed {
                self.health = Health::Working;
            } else if i.now_ms.saturating_sub(since_ms) >= p.dry_after_suspect_ms {
                let reason = String::from("no output for too long");
                return self.go_dry(i.now_ms, Source::Silence, reason, None, p);
            }
        } else if screen.liveness.unchanged_for_ms >= p.suspect_after_ms {
            self.health = Health::Suspect { since_ms: i.now_ms };
        }

        Vec::new()
    }

    /// Record the outcome of a canary probe.
    ///
    /// Success clears the dry mark and every counter; failure keeps the agent
    /// dry and schedules a later probe with the next backoff.
    pub fn canary_result(&mut self, ok: bool, now_ms: u64, p: &Policy) -> Vec<Action> {
        if ok {
            self.health = Health::Available;
            self.transient_attempts = 0;
            self.dry_count = 0;
            return vec![Action::MarkAvailable];
        }
        let backoff = self.next_dry_backoff(p);
        if let Health::Dry {
            retry_at_ms,
            canary_sent,
            ..
        } = &mut self.health
        {
            *retry_at_ms = now_ms.saturating_add(backoff);
            *canary_sent = false;
        }
        Vec::new()
    }
}

/// Whether an error is one the agent cannot retry past by itself.
fn is_unretryable(kind: ErrorKind) -> bool {
    matches!(
        kind,
        ErrorKind::RateLimit | ErrorKind::Quota | ErrorKind::Auth
    )
}
