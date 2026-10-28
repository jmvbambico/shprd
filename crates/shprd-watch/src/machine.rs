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
    ///
    /// The attempt has no worker any more, but the program may be perfectly
    /// usable: a fresh process of the same agent is a valid replacement, so
    /// this source hands off without drying anything.
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
    /// The worker keeps dying: only a human can decide what to do next.
    Escalated {
        /// When the escalation was reached, in epoch milliseconds.
        since_ms: u64,
        /// Human-readable explanation, shown to the operator.
        reason: String,
    },
}

/// Something the caller must do as a result of [`Watch::step`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Mark the session dry in persisted state.
    MarkDry {
        /// Why the session went dry, for the operator.
        reason: String,
        /// When the agent is expected back, in epoch milliseconds: the
        /// provider's reset time when it gave one, and otherwise the policy's
        /// backoff end, so no dry mark ever lasts forever.
        reset_at_ms: Option<u64>,
    },
    /// Hand the attempt to a worker.
    HandOff {
        /// Why the attempt is being handed over, for the record.
        reason: String,
        /// True when the hand-off follows a dry verdict, so the outgoing agent
        /// stays on cooldown. False when only the attempt's worker is gone and
        /// the agent itself is still usable.
        dry: bool,
    },
    /// Stop handing off and ask a human: the worker keeps dying.
    Escalate {
        /// What the human is being asked about.
        reason: String,
    },
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
    /// How many of one attempt's workers may exit in a row before the watchdog
    /// stops handing off and asks a human instead.
    ///
    /// A crashed worker is replaced silently, because a fresh process usually
    /// runs: a worker that dies this many times running the same brief is not
    /// going to be fixed by a fourth one.
    pub exit_budget: usize,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            suspect_after_ms: 180_000,
            dry_after_suspect_ms: 120_000,
            transient_backoff_ms: vec![60_000, 120_000, 240_000, 480_000],
            dry_backoff_ms: vec![900_000, 1_800_000, 3_600_000],
            exit_budget: 3,
        }
    }
}

/// How long one continuous run of serve retries is tolerated: the whole
/// [`Policy::transient_backoff_ms`] schedule added up, or the fallback when the
/// policy lists no steps.
fn transient_budget(p: &Policy) -> u64 {
    p.transient_backoff_ms
        .iter()
        .copied()
        .reduce(u64::saturating_add)
        .unwrap_or(FALLBACK_BACKOFF_MS)
}

/// Carries the watchdog's memory from one [`Watch::step`] to the next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watch {
    /// What the watchdog believes right now; also exposed to readers.
    pub health: Health,
    /// When the current run of serve retries was first seen, if it is running.
    ///
    /// `None` means no retry is outstanding: the next retry observation starts
    /// a fresh budget.
    transient_since_ms: Option<u64>,
    /// How many times the agent has gone dry in this run of trouble.
    dry_count: usize,
    /// How many of this attempt's workers have exited in a row.
    ///
    /// A hand-off keeps the attempt, so the count survives it: that is what
    /// makes a worker that dies every time escalate instead of being replaced
    /// for ever.
    exit_count: usize,
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
            transient_since_ms: None,
            dry_count: 0,
            exit_count: 0,
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
        // A dry mark with no end is dry for ever, and a roster of one has no
        // other worker to fall back on: the policy's cooldown stands in for a
        // reset the provider never gave, so the canary can re-admit the agent.
        let backoff_end = now_ms.saturating_add(backoff);
        let retry_at_ms = reset_at_ms.unwrap_or(backoff_end);
        self.transient_since_ms = None;
        self.health = Health::Dry {
            since_ms: now_ms,
            reason: reason.clone(),
            retry_at_ms,
            source,
            canary_sent: false,
        };
        vec![
            Action::MarkDry {
                reason: reason.clone(),
                reset_at_ms: Some(retry_at_ms),
            },
            Action::HandOff { reason, dry: true },
        ]
    }

    /// A worker process is gone: hand the attempt on, dry nothing.
    ///
    /// The program was never at fault, so nothing about it is recorded as
    /// unusable, and the hand-off may well pick the same agent again: a fresh
    /// process is a perfectly good replacement. Only a worker that keeps dying
    /// on the same attempt is a decision for a human.
    fn on_exit(&mut self, now_ms: u64, p: &Policy) -> Vec<Action> {
        self.exit_count = self.exit_count.saturating_add(1);
        self.transient_since_ms = None;
        let reason = String::from("agent process exited");
        if self.exit_count > p.exit_budget.max(1) {
            self.health = Health::Escalated {
                since_ms: now_ms,
                reason: reason.clone(),
            };
            return vec![Action::Escalate {
                reason: format!(
                    "{reason} {} times in a row on this attempt: pick up by hand",
                    self.exit_count
                ),
            }];
        }
        self.health = Health::Working;
        vec![Action::HandOff { reason, dry: false }]
    }

    /// The attempt was handed on: a new worker continues it.
    ///
    /// The fresh process has proved nothing yet, so health starts over, but the
    /// count of exits stays: a worker that dies again on the same attempt is
    /// the next entry in the same streak.
    pub fn after_handoff(&mut self) {
        self.health = Health::Working;
        self.transient_since_ms = None;
    }

    /// Fold one polling instant into the health verdict.
    ///
    /// Rules run in priority order: an exited worker, an unretryable provider
    /// error, serve retries, a dry screen pattern, then silence. An exit hands
    /// the attempt on without drying anything; every other verdict dries the
    /// agent. While already dry only the canary is considered, and while
    /// escalated nothing is.
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

        if matches!(self.health, Health::Escalated { .. }) {
            // Nothing more is decided here: the task is a human's now.
            return Vec::new();
        }

        if i.process_exited {
            return self.on_exit(i.now_ms, p);
        }

        if let Some(err) = &i.record_error
            && is_unretryable(err.kind)
        {
            let reason = err.message.clone();
            return self.go_dry(i.now_ms, Source::Record, reason, err.resets_at_ms, p);
        }

        if let Some(status) = &i.serve {
            if let Status::Retry { message, .. } = status {
                // The run of retries is timed, not counted: one retry that the
                // server keeps re-issuing must not age into a dry verdict, so
                // the clock starts with the first observation and only the
                // whole backoff schedule expiring is fatal.
                let since_ms = *self.transient_since_ms.get_or_insert(i.now_ms);
                if i.now_ms.saturating_sub(since_ms) > transient_budget(p) {
                    let reason = message.clone();
                    return self.go_dry(i.now_ms, Source::Serve, reason, None, p);
                }
            } else {
                // Any other serve status ends the run of retries.
                self.transient_since_ms = None;
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
            self.transient_since_ms = None;
            self.health = Health::Working;
            return Vec::new();
        }

        // A moving screen means the agent is producing again, so the retry
        // budget restarts.
        if screen.liveness.changed {
            self.transient_since_ms = None;
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
            self.transient_since_ms = None;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screen::Liveness;

    /// A fixed epoch time so the timelines read as plain offsets.
    const NOW: u64 = 1_700_000_000_000;

    /// Inputs with every signal absent.
    fn inputs(now_ms: u64) -> Inputs {
        Inputs {
            now_ms,
            record_error: None,
            serve: None,
            screen: None,
            process_exited: false,
        }
    }

    /// A screen signal in `state` whose screen has been still for a while.
    fn still(state: AgentState, unchanged_for_ms: u64) -> ScreenSignal {
        ScreenSignal {
            dry_pattern: None,
            reset_hint: None,
            liveness: Liveness {
                changed: false,
                unchanged_for_ms,
            },
            agent_state: state,
        }
    }

    /// A working pane that has produced nothing for `unchanged_for_ms`.
    fn silence(now_ms: u64, unchanged_for_ms: u64) -> Inputs {
        Inputs {
            screen: Some(still(AgentState::Working, unchanged_for_ms)),
            ..inputs(now_ms)
        }
    }

    #[test]
    fn silence_goes_suspect_then_dry() {
        let p = Policy::default();
        let mut w = Watch::new();

        assert_eq!(w.step(&silence(NOW, 179_999), &p), Vec::new());
        assert_eq!(w.health, Health::Working);

        let suspect_at = NOW + 180_000;
        assert_eq!(w.step(&silence(suspect_at, 180_000), &p), Vec::new());
        assert_eq!(
            w.health,
            Health::Suspect {
                since_ms: suspect_at
            }
        );

        assert_eq!(w.step(&silence(NOW + 270_000, 270_000), &p), Vec::new());
        let acts = w.step(&silence(NOW + 300_000, 300_000), &p);
        assert_eq!(
            acts,
            vec![
                Action::MarkDry {
                    reason: String::from("no output for too long"),
                    reset_at_ms: Some(NOW + 300_000 + 900_000),
                },
                Action::HandOff {
                    reason: String::from("no output for too long"),
                    dry: true,
                },
            ]
        );
        assert_eq!(
            w.health,
            Health::Dry {
                since_ms: NOW + 300_000,
                reason: String::from("no output for too long"),
                retry_at_ms: NOW + 300_000 + 900_000,
                source: Source::Silence,
                canary_sent: false,
            }
        );
    }

    #[test]
    fn quota_record_error_goes_dry_immediately() {
        let p = Policy::default();
        let mut w = Watch::new();
        let resets = NOW + 3_600_000;
        let i = Inputs {
            record_error: Some(RecordError {
                kind: ErrorKind::Quota,
                message: String::from("credit balance is too low"),
                at_ms: Some(NOW),
                resets_at_ms: Some(resets),
            }),
            ..inputs(NOW)
        };

        assert_eq!(
            w.step(&i, &p),
            vec![
                Action::MarkDry {
                    reason: String::from("credit balance is too low"),
                    reset_at_ms: Some(resets),
                },
                Action::HandOff {
                    reason: String::from("credit balance is too low"),
                    dry: true,
                },
            ]
        );
        assert_eq!(
            w.health,
            Health::Dry {
                since_ms: NOW,
                reason: String::from("credit balance is too low"),
                retry_at_ms: resets,
                source: Source::Record,
                canary_sent: false,
            }
        );
    }

    /// One polling instant in which the server reports a self-retry.
    fn retrying(now_ms: u64) -> Inputs {
        Inputs {
            serve: Some(Status::Retry {
                attempt: 1,
                message: String::from("overloaded"),
                next_at_ms: None,
            }),
            ..inputs(now_ms)
        }
    }

    #[test]
    fn serve_retries_are_tolerated_for_the_whole_backoff_schedule() {
        let p = Policy::default();
        let mut w = Watch::new();

        // A retry every 10 s for 10 minutes: one continuous retry well inside
        // the policy's 15 minute budget, so the agent is never declared dry.
        for tick in 0..=60 {
            assert_eq!(w.step(&retrying(NOW + tick * 10_000), &p), Vec::new());
            assert_eq!(w.health, Health::Working);
        }
    }

    #[test]
    fn serve_retries_go_dry_past_the_backoff_sum() {
        let p = Policy::default();
        let mut w = Watch::new();

        let mut dry_acts = None;
        for tick in 0..=96 {
            let acts = w.step(&retrying(NOW + tick * 10_000), &p);
            if !acts.is_empty() {
                dry_acts.get_or_insert(acts);
            }
        }

        let Some(acts) = dry_acts else {
            panic!("the retry budget expiring must be a dry verdict");
        };
        assert!(
            matches!(
                acts.as_slice(),
                [
                    Action::MarkDry {
                        reason,
                        reset_at_ms: Some(_),
                    },
                    Action::HandOff { dry: true, .. },
                ] if reason == "overloaded"
            ),
            "serve retries past the budget still dry the agent: {acts:?}"
        );
        assert!(matches!(
            w.health,
            Health::Dry {
                source: Source::Serve,
                ..
            }
        ));
    }

    /// The budget is a threshold to exceed, not to reach: a retry still being
    /// reported at exactly one budget's worth of elapsed time is inside the
    /// schedule, and only the first instant past it is fatal.
    #[test]
    fn serve_retries_dry_only_past_the_backoff_sum() {
        let p = Policy::default();
        let budget = transient_budget(&p);
        assert_eq!(budget, 900_000, "the default schedule sums to 15 minutes");
        let mut w = Watch::new();

        // The clock starts with the first observation.
        assert_eq!(w.step(&retrying(NOW), &p), Vec::new());
        assert_eq!(w.health, Health::Working);

        // Exactly one budget later the retry spell is not yet fatal.
        assert_eq!(w.step(&retrying(NOW + budget), &p), Vec::new());
        assert_eq!(w.health, Health::Working);

        // One millisecond past it is.
        let dry_at = NOW + budget + 1;
        assert_eq!(
            w.step(&retrying(dry_at), &p),
            vec![
                Action::MarkDry {
                    reason: String::from("overloaded"),
                    reset_at_ms: Some(dry_at + 900_000),
                },
                Action::HandOff {
                    reason: String::from("overloaded"),
                    dry: true,
                },
            ]
        );
        assert_eq!(
            w.health,
            Health::Dry {
                since_ms: dry_at,
                reason: String::from("overloaded"),
                retry_at_ms: dry_at + 900_000,
                source: Source::Serve,
                canary_sent: false,
            }
        );
    }

    #[test]
    fn a_non_retry_status_restarts_the_transient_clock() {
        let p = Policy::default();
        let mut w = Watch::new();

        for tick in 0..=60 {
            assert_eq!(w.step(&retrying(NOW + tick * 10_000), &p), Vec::new());
        }

        // The server reports idle: the retry spell is over.
        let idle = Inputs {
            serve: Some(Status::Idle),
            ..inputs(NOW + 610_000)
        };
        assert_eq!(w.step(&idle, &p), Vec::new());
        assert_eq!(w.health, Health::Working);

        // A further 10 minutes of retries stays inside the budget because the
        // clock restarted with the first retry after the idle report.
        for tick in 1..=60 {
            let acts = w.step(&retrying(NOW + 610_000 + tick * 10_000), &p);
            assert_eq!(acts, Vec::new());
            assert_eq!(w.health, Health::Working);
        }
    }

    #[test]
    fn a_screen_change_clears_suspicion() {
        let p = Policy::default();
        let mut w = Watch::new();
        w.step(&silence(NOW, 180_000), &p);
        assert!(matches!(w.health, Health::Suspect { .. }));

        let moved = Inputs {
            screen: Some(ScreenSignal {
                liveness: Liveness {
                    changed: true,
                    unchanged_for_ms: 0,
                },
                ..still(AgentState::Working, 0)
            }),
            ..inputs(NOW + 200_000)
        };
        assert_eq!(w.step(&moved, &p), Vec::new());
        assert_eq!(w.health, Health::Working);
    }

    #[test]
    fn an_idle_agent_is_never_suspect() {
        let p = Policy::default();
        let mut w = Watch::new();
        let i = Inputs {
            screen: Some(still(AgentState::Idle, 10_000_000)),
            ..inputs(NOW)
        };
        assert_eq!(w.step(&i, &p), Vec::new());
        assert_eq!(w.health, Health::Working);
    }

    /// One polling instant in which the attempt's worker is gone.
    fn exited(now_ms: u64) -> Inputs {
        Inputs {
            process_exited: true,
            ..inputs(now_ms)
        }
    }

    /// A screen whose dry pattern matches: a real "this agent is unusable"
    /// signal, and one the provider gives no reset time for.
    fn dry_screen(now_ms: u64) -> Inputs {
        Inputs {
            screen: Some(ScreenSignal {
                dry_pattern: Some(String::from("usage limit reached")),
                ..still(AgentState::Working, 0)
            }),
            ..inputs(now_ms)
        }
    }

    /// An exited process is a lost worker, not an unusable program: the task is
    /// handed on and nothing is marked dry, so the roster stays usable.
    #[test]
    fn an_exited_process_hands_off_without_drying_the_agent() {
        let p = Policy::default();
        let mut w = Watch::new();

        assert_eq!(
            w.step(&exited(NOW), &p),
            vec![Action::HandOff {
                reason: String::from("agent process exited"),
                dry: false,
            }]
        );
        assert_eq!(w.health, Health::Working);
    }

    /// A worker that keeps dying on one attempt is not a hand-off problem: the
    /// policy's budget runs out and a human is asked, instead of a fourth
    /// replacement that would only die the same way.
    #[test]
    fn exits_past_the_budget_escalate_instead_of_handing_off_again() {
        let p = Policy::default();
        assert_eq!(p.exit_budget, 3);
        let mut w = Watch::new();

        for round in 0..p.exit_budget {
            assert_eq!(
                w.step(&exited(NOW + round as u64 * 1_000), &p),
                vec![Action::HandOff {
                    reason: String::from("agent process exited"),
                    dry: false,
                }],
                "exit {} is still replaced silently",
                round + 1
            );
            // The hand-off starts a fresh worker, which proves nothing yet.
            w.after_handoff();
        }

        let escalated_at = NOW + 3_000;
        let acts = w.step(&exited(escalated_at), &p);
        assert_eq!(acts.len(), 1);
        assert!(
            matches!(
                acts.as_slice(),
                [Action::Escalate { reason }]
                    if reason.contains("4 times in a row") && !reason.is_empty()
            ),
            "the fourth exit escalates: {acts:?}"
        );
        assert!(matches!(
            w.health,
            Health::Escalated {
                since_ms,
                ..
            } if since_ms == escalated_at
        ));

        // One escalation, not one per tick.
        assert_eq!(w.step(&exited(escalated_at + 1_000), &p), Vec::new());
    }

    /// A hand-off to another agent keeps the streak: the same attempt losing
    /// worker after worker escalates no matter which agent ran it.
    #[test]
    fn a_hand_off_to_another_agent_keeps_the_exit_streak() {
        let p = Policy::default();
        let mut w = Watch::new();
        for round in 0..p.exit_budget {
            w.step(&exited(NOW + round as u64 * 1_000), &p);
            w.after_handoff();
        }
        assert!(matches!(
            w.step(&exited(NOW + 3_000), &p).as_slice(),
            [Action::Escalate { .. }]
        ));
    }

    /// Drive `w` into a dry verdict through a screen pattern.
    fn go_dry(w: &mut Watch, p: &Policy) {
        w.step(&dry_screen(NOW), p);
    }

    #[test]
    fn canary_success_marks_available() {
        let p = Policy::default();
        let mut w = Watch::new();
        go_dry(&mut w, &p);

        assert_eq!(w.step(&inputs(NOW + 899_999), &p), Vec::new());
        assert_eq!(w.step(&inputs(NOW + 900_000), &p), vec![Action::Canary]);
        assert_eq!(w.step(&inputs(NOW + 900_001), &p), Vec::new());

        assert_eq!(
            w.canary_result(true, NOW + 900_002, &p),
            vec![Action::MarkAvailable]
        );
        assert_eq!(w.health, Health::Available);

        // Counters were cleared: the next dry spell starts at the first backoff.
        go_dry(&mut w, &p);
        assert!(matches!(
            w.health,
            Health::Dry {
                retry_at_ms,
                ..
            } if retry_at_ms == NOW + 900_000
        ));
    }

    #[test]
    fn canary_failure_schedules_a_later_probe() {
        let p = Policy::default();
        let mut w = Watch::new();
        go_dry(&mut w, &p);
        w.step(&inputs(NOW + 900_000), &p);

        assert_eq!(w.canary_result(false, NOW + 900_001, &p), Vec::new());
        let retry_at = NOW + 900_001 + 1_800_000;
        assert_eq!(
            w.health,
            Health::Dry {
                since_ms: NOW,
                reason: String::from("usage limit reached"),
                retry_at_ms: retry_at,
                source: Source::Screen,
                canary_sent: false,
            }
        );

        assert_eq!(w.step(&inputs(retry_at - 1), &p), Vec::new());
        assert_eq!(w.step(&inputs(retry_at), &p), vec![Action::Canary]);
    }

    /// A dry verdict the provider gave no reset for still ends: the policy's
    /// cooldown stands in for it, so the canary can re-admit the agent and a
    /// roster of one is never stuck for ever.
    #[test]
    fn a_dry_verdict_without_a_provider_reset_ends_at_the_default_cooldown() {
        let p = Policy::default();
        let mut w = Watch::new();

        assert_eq!(
            w.step(&dry_screen(NOW), &p),
            vec![
                Action::MarkDry {
                    reason: String::from("usage limit reached"),
                    reset_at_ms: Some(NOW + 900_000),
                },
                Action::HandOff {
                    reason: String::from("usage limit reached"),
                    dry: true,
                },
            ]
        );

        // The canary is due when the cooldown ends, and re-admits the agent.
        assert_eq!(w.step(&inputs(NOW + 899_999), &p), Vec::new());
        assert_eq!(w.step(&inputs(NOW + 900_000), &p), vec![Action::Canary]);
        assert_eq!(
            w.canary_result(true, NOW + 900_001, &p),
            vec![Action::MarkAvailable]
        );
        assert_eq!(w.health, Health::Available);
    }

    #[test]
    fn a_screen_dry_pattern_goes_dry() {
        let p = Policy::default();
        let mut w = Watch::new();
        let i = Inputs {
            screen: Some(ScreenSignal {
                dry_pattern: Some(String::from("usage limit reached")),
                ..still(AgentState::Working, 0)
            }),
            ..inputs(NOW)
        };
        assert_eq!(
            w.step(&i, &p),
            vec![
                Action::MarkDry {
                    reason: String::from("usage limit reached"),
                    reset_at_ms: Some(NOW + 900_000),
                },
                Action::HandOff {
                    reason: String::from("usage limit reached"),
                    dry: true,
                },
            ]
        );
    }
}
