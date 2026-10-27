//! Per-agent quota probes, ported from `og`.
//!
//! Ported from og installer/og_quota.py.
//!
//! Each probe reads one agent's credential store or one vendor's usage
//! endpoint and produces a [`QuotaReport`]. A probe never panics and never
//! carries a token into a detail string: a failure is a report with
//! [`QuotaState::Unknown`] and a one-line, non-secret reason.
//!
//! Privacy contract (carried over from og): read-only, tokens are never
//! printed, logged or persisted, and OAuth tokens are never refreshed (a
//! refresh rotates the user's real session).

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub mod cache;
/// Worst-case wall clock for one network probe (og `TIMEOUT_S`).
pub const TIMEOUT: Duration = Duration::from_secs(8);

/// Capacity state of one agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaState {
    /// The agent has capacity for another dispatch.
    Ok,
    /// The agent is exhausted (a window is at 100 % or a balance is zero).
    Dry,
    /// No answer: missing credentials, bad response or network failure.
    Unknown,
}

/// How much the reported numbers can be trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// Read from the vendor's own usage number.
    Measured,
    /// Inferred, e.g. a free tier that does not report a remaining amount.
    Assumed,
    /// Nothing known.
    Unknown,
}

/// One rate-limit window (a five-hour block, a weekly cap, a primary window).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Window {
    /// Window name as the vendor reports it (`five_hour`, `primary`, …).
    pub name: String,
    /// Consumed percentage, normalized to 0-100.
    pub used_percent: f64,
    /// Reset instant, epoch milliseconds, when the vendor gave one.
    pub reset_at_ms: Option<u64>,
}

/// One agent's quota verdict.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaReport {
    /// Agent id this report is about (`claude`, `codex`, `kilo`).
    pub agent: String,
    /// Capacity verdict.
    pub state: QuotaState,
    /// Confidence in the numbers.
    pub tier: Tier,
    /// Every window the vendor reported, binding window first.
    pub windows: Vec<Window>,
    /// Reset instant of the binding window, epoch milliseconds.
    pub reset_at_ms: Option<u64>,
    /// Where the numbers came from (`anthropic-oauth`, `codex-wham`, …).
    pub source: String,
    /// One-line human explanation; never contains a credential.
    pub detail: String,
    /// When the probe ran, epoch milliseconds.
    pub checked_at_ms: u64,
}

impl QuotaReport {
    /// An "nothing known" report with a non-secret reason.
    pub fn unknown(agent: &str, source: &str, detail: &str, now_ms: u64) -> Self {
        Self {
            agent: agent.to_owned(),
            state: QuotaState::Unknown,
            tier: Tier::Unknown,
            windows: Vec::new(),
            reset_at_ms: None,
            source: source.to_owned(),
            detail: detail.to_owned(),
            checked_at_ms: now_ms,
        }
    }

    /// Age of this report in milliseconds, saturating at zero for a clock
    /// that went backwards.
    pub fn age_ms(&self, now_ms: u64) -> u64 {
        now_ms.saturating_sub(self.checked_at_ms)
    }

    /// Mark a reused report so a reader can tell it apart from fresh data.
    pub fn mark_cached(mut self, why: &str) -> Self {
        self.detail = format!("{}; {why}, reusing cached record", self.detail);
        self
    }
}

/// Why a probe could not answer.
///
/// The messages are fixed strings: no variant carries a response body, a
/// header or a credential, so an error can be displayed as-is.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum QuotaError {
    /// The vendor asked us to slow down (`HTTP 429`).
    #[error("rate-limited (HTTP 429)")]
    RateLimited,
    /// The request never produced a response (DNS, TLS, timeout).
    #[error("network error")]
    Network,
    /// The vendor answered with a status other than 200.
    #[error("HTTP {0}")]
    Http(u16),
    /// The response was not the JSON shape we understand.
    #[error("unrecognized response shape")]
    Shape,
    /// A required local credential is missing or unusable.
    #[error("no usable credential: {0}")]
    Credential(&'static str),
}

/// Minimal HTTP seam, so probes are testable without a network.
pub trait Http {
    /// `GET` `url` with `headers`; returns `(status, body)`.
    fn get(&self, url: &str, headers: &[(&str, String)]) -> Result<(u16, String), QuotaError>;
}

/// The real [`Http`], backed by `ureq` with a hard 8 s ceiling.
#[derive(Debug, Clone, Default)]
pub struct UreqHttp;

impl Http for UreqHttp {
    fn get(&self, url: &str, headers: &[(&str, String)]) -> Result<(u16, String), QuotaError> {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            .build();
        let agent: ureq::Agent = config.into();
        let mut req = agent.get(url);
        for (name, value) in headers {
            req = req.header(*name, value);
        }
        match req.call() {
            Ok(mut resp) => {
                let status = resp.status().as_u16();
                let body = resp
                    .body_mut()
                    .read_to_string()
                    .map_err(|_| QuotaError::Network)?;
                Ok((status, body))
            }
            // ureq turns 4xx/5xx into errors; 429 keeps its own variant so
            // every probe maps it to the cached-record fallback.
            Err(ureq::Error::StatusCode(429)) => Err(QuotaError::RateLimited),
            Err(ureq::Error::StatusCode(code)) => Err(QuotaError::Http(code)),
            Err(_) => Err(QuotaError::Network),
        }
    }
}

/// Everything a probe needs from the outside world, injected so tests touch
/// neither the network, the Keychain nor the real home directory.
pub struct ProbeContext {
    /// Home directory the credential and cache paths resolve against.
    pub home: PathBuf,
    /// HTTP seam.
    pub http: Box<dyn Http>,
    /// Wall clock, epoch milliseconds.
    pub now_ms: u64,
    /// Whether the macOS Keychain may be consulted for Claude credentials.
    pub keychain: bool,
    /// `kilo` binary to run; defaults to `kilo` on `PATH`.
    pub kilo_bin: PathBuf,
    /// True when the pinned Kilo model routes inside the free tier.
    pub free_pin: bool,
}

impl ProbeContext {
    /// A context with the real HTTP client, the real Keychain and the current
    /// wall clock.
    #[must_use]
    pub fn new(home: PathBuf) -> Self {
        Self {
            home,
            http: Box::new(UreqHttp),
            now_ms: cache::now_ms(),
            keychain: cfg!(target_os = "macos"),
            kilo_bin: PathBuf::from("kilo"),
            free_pin: false,
        }
    }

    /// Replace the HTTP seam (tests).
    #[must_use]
    pub fn with_http(mut self, http: Box<dyn Http>) -> Self {
        self.http = http;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_report_carries_no_secret() {
        let r = QuotaReport::unknown("claude", "none", "no OAuth token", 7);
        assert_eq!(r.state, QuotaState::Unknown);
        assert_eq!(r.tier, Tier::Unknown);
        assert_eq!(r.source, "none");
        assert_eq!(r.checked_at_ms, 7);
        assert_eq!(r.age_ms(9), 2);
        assert_eq!(r.age_ms(1), 0);
        assert!(r.mark_cached("HTTP 429").detail.contains("cached"));
    }
}
