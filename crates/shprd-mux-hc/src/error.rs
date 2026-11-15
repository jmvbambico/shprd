//! What can go wrong between shprd and herdr-core.
//!
//! Every variant is a decision the caller can act on, so nothing here hides a
//! failure behind a string: a refusal from the core keeps the core's own
//! `kind` (its stable taxonomy, `not_found` / `conflict` / `live` / …) because
//! that — never the message — is what the core promises not to change.

use std::time::Duration;

use thiserror::Error;

/// The oldest herdr-core this crate was built and tested against.
///
/// Taken from the M7ef spike, which recorded the real API — framing, method
/// inventory, event stream — on herdr-core 0.1.21 (`hc 0.1.21`, Herdr.app
/// 0.1.15). Anything older is refused rather than tried: the protocol is
/// documented to only grow within a generation, so newer versions pass.
pub const CORE_MINIMUM: &str = "0.1.21";

/// The herdr-core protocol generation this crate speaks.
///
/// `hello` takes it, and a core of another generation answers an error instead
/// of its own generation, which is why the gate checks this first.
pub const CORE_GENERATION: u32 = 1;

/// What every call in this crate returns.
pub type Result<T> = std::result::Result<T, Error>;

/// Why a herdr-core operation failed.
#[derive(Debug, Error)]
pub enum Error {
    /// The core (or its `hc connect` child) broke the protocol: a frame larger
    /// than the cap, a stream nobody asked for, a line that is not JSON.
    #[error("herdr-core protocol violation: {0}")]
    Protocol(String),

    /// The core is older than [`CORE_MINIMUM`]; the message names both
    /// versions so the person can tell which `hc update` fixes it.
    #[error("herdr-core needs to be {needed} or newer, and this one is {found}: run `hc update`")]
    VersionTooOld {
        /// The version the core reported.
        found: String,
        /// [`CORE_MINIMUM`].
        needed: &'static str,
    },

    /// The core speaks another protocol generation, so no call can be read.
    #[error(
        "herdr-core speaks generation {found} and shprd speaks generation {needed}: update \
         `hc`, or point shprd at a generation {needed} core"
    )]
    Generation {
        /// The generation the core reported.
        found: u32,
        /// [`CORE_GENERATION`].
        needed: u32,
    },

    /// The core answered `hello` but lacks something this crate needs.
    #[error("herdr-core {version} lacks {what}: update `hc` and shprd")]
    Incompatible {
        /// The version the core reported.
        version: String,
        /// What is missing, in words.
        what: String,
    },

    /// There is no connection right now: it never opened, or it died.
    #[error("herdr-core is unreachable: {0}")]
    Unreachable(String),

    /// The core refused the call. `kind` is the core's stable taxonomy.
    #[error("herdr-core refused `{method}` ({kind}): {message}")]
    Refused {
        /// The method that was called.
        method: String,
        /// The core's `error.data.kind`, or `"rpc"` when it named none.
        kind: String,
        /// The core's message, verbatim.
        message: String,
        /// The core's suggestion, when it offered one.
        hint: Option<String>,
    },

    /// The call did not answer in time. The request may still be running in the
    /// core, so a mutating call must not be retried blind.
    #[error("herdr-core did not answer `{method}` within {}s; check the state before retrying", after.as_secs())]
    Timeout {
        /// The method that was called.
        method: String,
        /// How long it waited.
        after: Duration,
    },

    /// Too many calls are waiting on one core (the cap keeps a stuck reader
    /// from growing the pending map without bound).
    #[error("too many calls are waiting on herdr-core ({0} in flight)")]
    Busy(usize),

    /// The output was not what the protocol says it is: a malformed status
    /// report, an unexpected shape in a document.
    #[error("unexpected herdr-core output: {0}")]
    Output(String),
}

impl Error {
    /// The core's own taxonomy for a refusal (`not_found`, `conflict`, …),
    /// when this error is one.
    pub fn refusal_kind(&self) -> Option<&str> {
        match self {
            Self::Refused { kind, .. } => Some(kind),
            _ => None,
        }
    }

    /// Whether the connection went away rather than the call being refused —
    /// the caller's cue to reconnect (the client does so by itself) and to read
    /// the state again instead of trusting what it had.
    pub fn is_unreachable(&self) -> bool {
        matches!(self, Self::Unreachable(_) | Self::Protocol(_))
    }
}
