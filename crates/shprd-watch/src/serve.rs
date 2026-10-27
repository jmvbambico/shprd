//! Client for `opencode serve` / `kilo serve`.
//!
//! Both servers expose the same two endpoints, which is where the live retry
//! state of a rate-limited turn comes from:
//!
//! - `GET /session/status` — one snapshot of every session's state;
//! - `GET /event` — Server-Sent Events carrying the same states as they change.
//!
//! Only what the watchdog needs is modelled. The client is read-only: it never
//! writes to the server.

use std::collections::BTreeMap;

use serde_json::Value;

/// Anything above this is an epoch-milliseconds timestamp, not a delay.
///
/// `next` is documented as neither; 10^12 ms is 2001, so no plausible relative
/// delay (that would be 31 years) crosses the line.
const EPOCH_MS_FLOOR: u64 = 1_000_000_000_000;

/// How much of a retry message is kept.
const MESSAGE_LIMIT: usize = 200;

/// The latest known state of one session on a serve instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// The session is not working.
    Idle,
    /// A turn is running.
    Busy,
    /// A turn is rate-limited and the server will retry by itself.
    Retry {
        /// How many attempts have failed; 0 when the server omits it.
        attempt: u32,
        /// The server's reason, truncated to [`MESSAGE_LIMIT`] characters.
        message: String,
        /// When the next attempt is due, in epoch milliseconds.
        ///
        /// `None` when the server gave no usable `next` value.
        next_at_ms: Option<u64>,
    },
    /// The server reported a state this build does not know.
    Unknown,
}

impl Status {
    /// Parse one `SessionStatus` JSON object.
    ///
    /// `now_ms` only matters for a relative `next`, which becomes an absolute
    /// deadline. An unrecognized or missing `type` yields [`Status::Unknown`].
    #[must_use]
    pub fn from_json(value: &Value, now_ms: u64) -> Self {
        match value.get("type").and_then(Value::as_str) {
            Some("idle") => Self::Idle,
            Some("busy") => Self::Busy,
            Some("retry") => Self::Retry {
                attempt: value
                    .get("attempt")
                    .and_then(Value::as_u64)
                    .and_then(|n| u32::try_from(n).ok())
                    .unwrap_or(0),
                message: value
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .chars()
                    .take(MESSAGE_LIMIT)
                    .collect(),
                next_at_ms: value
                    .get("next")
                    .and_then(Value::as_u64)
                    .map(|next| resolve_next(next, now_ms)),
            },
            _ => Self::Unknown,
        }
    }
}

/// Turn the server's ambiguous `next` into an absolute deadline.
///
/// A value above [`EPOCH_MS_FLOOR`] is already a timestamp; anything else is a
/// delay counted from `now_ms`.
#[must_use]
pub fn resolve_next(next: u64, now_ms: u64) -> u64 {
    if next > EPOCH_MS_FLOOR {
        next
    } else {
        now_ms.saturating_add(next)
    }
}

/// One event read off the `/event` stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServeEvent {
    /// A session's state changed.
    Status {
        /// Session the state belongs to.
        session: String,
        /// The new state.
        status: Status,
    },
    /// A session failed a turn.
    Error {
        /// Session the error belongs to, when the server names one.
        session: Option<String>,
        /// Error name, for example `UnknownError`.
        name: String,
        /// Human-readable message.
        message: String,
    },
    /// An event this build does not care about, or could not read.
    Other,
}

/// Why a serve request could not answer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ServeError {
    /// The base URL is not a loopback address; the client refuses to leave the
    /// machine.
    #[error("not a loopback base URL")]
    NotLocal,
    /// The request never produced a response (connect, read, timeout).
    #[error("network error")]
    Network,
    /// The server answered with a status other than 200.
    #[error("HTTP {0}")]
    Http(u16),
    /// The body was not the JSON shape this client understands.
    #[error("unrecognized response shape")]
    Shape,
}

/// Parse a `GET /session/status` body into one entry per session.
///
/// # Errors
///
/// [`ServeError::Shape`] when the body is not a JSON object.
pub fn parse_statuses(body: &str, now_ms: u64) -> Result<BTreeMap<String, Status>, ServeError> {
    let value: Value = serde_json::from_str(body).map_err(|_| ServeError::Shape)?;
    let object = value.as_object().ok_or(ServeError::Shape)?;
    Ok(object
        .iter()
        .map(|(session, status)| (session.clone(), Status::from_json(status, now_ms)))
        .collect())
}
