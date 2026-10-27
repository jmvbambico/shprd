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
use serde_json::Value;

pub mod cache;
pub mod claude;
pub mod codex;
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

/// Normalize a utilization to 0-100.
///
/// Vendors disagree: `utilization` is a 0-1 fraction, `used_percentage` is
/// already 0-100. Anything above 1.0 is treated as a percentage (og `_pct`).
pub(crate) fn pct(value: &Value) -> Option<f64> {
    let v = match value {
        Value::Number(n) => n.as_f64()?,
        Value::String(s) => s.parse::<f64>().ok()?,
        _ => return None,
    };
    if !v.is_finite() {
        return None;
    }
    Some(if v > 1.0 { v } else { v * 100.0 })
}

/// First present, non-null field among `names` (og `_first`).
pub(crate) fn first<'a>(obj: &'a Value, names: &[&str]) -> Option<&'a Value> {
    names
        .iter()
        .find_map(|n| obj.get(*n).filter(|v| !v.is_null()))
}

/// Interpret a vendor timestamp as epoch milliseconds.
///
/// Accepts epoch seconds (numbers below 1e12), epoch milliseconds, and ISO
/// 8601 strings with a `Z` or `±HH:MM` offset (what the Anthropic usage
/// endpoint sends).
pub(crate) fn timestamp_ms(value: &Value) -> Option<u64> {
    match value {
        Value::Number(n) => {
            let v = n.as_f64()?;
            if !v.is_finite() || v < 0.0 {
                return None;
            }
            let ms = if v < 1.0e12 { v * 1000.0 } else { v };
            Some(ms as u64)
        }
        Value::String(s) => iso_to_ms(s),
        _ => None,
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`), so no date crate is needed.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = i64::from((m + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Parse `YYYY-MM-DDTHH:MM:SS[.fff][Z|±HH:MM]` into epoch milliseconds.
pub(crate) fn iso_to_ms(s: &str) -> Option<u64> {
    let s = s.trim();
    let (date, rest) = s.split_once(['T', ' '])?;
    let mut d = date.split('-');
    let y: i64 = d.next()?.parse().ok()?;
    let mo: u32 = d.next()?.parse().ok()?;
    let da: u32 = d.next()?.parse().ok()?;
    let idx = rest
        .char_indices()
        .skip(1)
        .find(|(_, c)| matches!(c, 'Z' | '+' | '-'))
        .map(|(i, _)| i);
    let (clock, offset_min) = match idx {
        Some(i) => {
            let off = &rest[i..];
            let sign = if off.starts_with('-') { -1 } else { 1 };
            let mut it = off.trim_start_matches(&['Z', '+', '-'][..]).split(':');
            let oh: i64 = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            let om: i64 = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            (&rest[..i], sign * (oh * 60 + om))
        }
        None => (rest, 0),
    };
    let mut t = clock.split(':');
    let h: u64 = t.next()?.parse().ok()?;
    let mi: u64 = t.next()?.parse().ok()?;
    let sec: f64 = t.next().unwrap_or("0").parse().ok()?;
    if !(sec.is_finite() && sec >= 0.0) {
        return None;
    }
    let millis = h * 3_600_000 + mi * 60_000 + (sec * 1000.0).round() as u64;
    let days = days_from_civil(y, mo, da);
    let total = days * 86_400_000 + i64::try_from(millis).ok()? - offset_min * 60_000;
    if total < 0 {
        return None;
    }
    u64::try_from(total).ok()
}

/// Exhaustion rule, in one place so every probe reports `dry` alike: a window
/// at 100 % or more means the agent has no capacity.
pub(crate) fn state_from_windows(windows: &[Window]) -> QuotaState {
    if windows.iter().any(|w| w.used_percent >= 100.0) {
        QuotaState::Dry
    } else {
        QuotaState::Ok
    }
}

/// Probe one agent's quota, using and updating the on-disk cache.
///
/// A report younger than [`cache::CACHE_TTL_MS`] is served as-is; a fresh
/// success is stored. When the probe is rate-limited or the network fails, the
/// last good report stands in if it is younger than [`cache::STALE_TTL_MS`],
/// marked so a reader knows it is stale. A missing credential is not transient
/// and is reported as-is rather than masked by a cached record.
#[must_use]
pub fn probe(agent: &str, ctx: &ProbeContext) -> QuotaReport {
    let store = cache::Cache::new(&ctx.home);
    if let Some(fresh) = store.fresh(agent, ctx.now_ms) {
        return fresh;
    }
    let result = match agent.trim().to_ascii_lowercase().as_str() {
        "claude" | "claude-code" => claude::probe(ctx),
        "codex" => codex::probe(ctx),
        _ => return QuotaReport::unknown(agent, "none", "no quota source", ctx.now_ms),
    };
    match result {
        Ok(report) => {
            // The cache is an optimization; a failed write must not lose the
            // measurement we just took.
            let _ = store.store(&report);
            report
        }
        Err(error @ (QuotaError::RateLimited | QuotaError::Network)) => {
            let why = error.to_string();
            match store.last_good(agent, ctx.now_ms) {
                Some(report) => report.mark_cached(&why),
                None => QuotaReport::unknown(agent, "none", &why, ctx.now_ms),
            }
        }
        Err(error) => QuotaReport::unknown(agent, "none", &error.to_string(), ctx.now_ms),
    }
}

/// Test doubles shared by the probe modules.
#[cfg(test)]
pub(crate) mod fake {
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    use super::{Http, ProbeContext, QuotaError};

    /// One recorded request: URL and header name/value pairs.
    pub type Recorded = Vec<(String, Vec<(String, String)>)>;

    /// A scripted [`Http`]: replays one answer and records every request.
    #[derive(Debug, Default)]
    pub struct FakeHttp {
        reply: RefCell<Option<Result<(u16, String), QuotaError>>>,
        requests: RefCell<Recorded>,
    }

    impl FakeHttp {
        /// Always answer `status` with `body`.
        pub fn new(status: u16, body: &str) -> Self {
            Self {
                reply: RefCell::new(Some(Ok((status, body.to_owned())))),
                requests: RefCell::new(Vec::new()),
            }
        }

        /// Always fail with `error`.
        pub fn failing(error: QuotaError) -> Self {
            Self {
                reply: RefCell::new(Some(Err(error))),
                requests: RefCell::new(Vec::new()),
            }
        }

        /// URL of the sole request, if any.
        pub fn url(&self) -> Option<String> {
            self.requests.borrow().first().map(|(url, _)| url.clone())
        }

        /// Header names of the sole request; values stay unasserted on purpose.
        pub fn header_names(&self) -> Vec<String> {
            self.requests
                .borrow()
                .first()
                .map(|(_, headers)| headers.iter().map(|(n, _)| n.clone()).collect())
                .unwrap_or_default()
        }

        /// A header value by name from the sole request.
        pub fn header(&self, name: &str) -> Option<String> {
            self.requests.borrow().first().and_then(|(_, headers)| {
                headers
                    .iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, v)| v.clone())
            })
        }

        /// Number of requests seen.
        pub fn count(&self) -> usize {
            self.requests.borrow().len()
        }
    }

    impl Http for FakeHttp {
        fn get(&self, url: &str, headers: &[(&str, String)]) -> Result<(u16, String), QuotaError> {
            self.requests.borrow_mut().push((
                url.to_owned(),
                headers
                    .iter()
                    .map(|(n, v)| ((*n).to_owned(), v.clone()))
                    .collect(),
            ));
            match self.reply.borrow().clone() {
                Some(reply) => reply,
                None => Err(QuotaError::Network),
            }
        }
    }

    /// Let a test keep a handle on the fake after handing it to a context.
    impl<T: Http + ?Sized> Http for std::rc::Rc<T> {
        fn get(&self, url: &str, headers: &[(&str, String)]) -> Result<(u16, String), QuotaError> {
            (**self).get(url, headers)
        }
    }

    /// A probe context wired to `http`, rooted at `home`, with the Keychain
    /// off so no test can reach the real one.
    pub fn ctx<H: Http + 'static>(home: &Path, http: H, now_ms: u64) -> ProbeContext {
        ProbeContext {
            home: home.to_path_buf(),
            http: Box::new(http),
            now_ms,
            keychain: false,
            kilo_bin: PathBuf::from("kilo-not-installed"),
            free_pin: false,
        }
    }

    /// Create a temp dir, panicking with a message on failure.
    pub fn temp_dir() -> tempfile::TempDir {
        match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(error) => panic!("should create a temp dir: {error}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pct_normalizes_fraction_and_percentage() {
        assert_eq!(pct(&json!(0.5)), Some(50.0));
        assert_eq!(pct(&json!(1.0)), Some(100.0));
        assert_eq!(pct(&json!(1.5)), Some(1.5));
        assert_eq!(pct(&json!(87.0)), Some(87.0));
        assert_eq!(pct(&json!("42")), Some(42.0));
        assert_eq!(pct(&json!(null)), None);
        assert_eq!(pct(&json!("nope")), None);
    }

    #[test]
    fn first_skips_missing_and_null() {
        let v = json!({"a": null, "b": 7});
        assert_eq!(first(&v, &["a", "b"]), Some(&json!(7)));
        assert_eq!(first(&v, &["a", "z"]), None);
    }

    #[test]
    fn timestamps_accept_seconds_millis_and_iso() {
        assert_eq!(timestamp_ms(&json!(1_700_000_000)), Some(1_700_000_000_000));
        assert_eq!(
            timestamp_ms(&json!(1_700_000_000_000u64)),
            Some(1_700_000_000_000)
        );
        assert_eq!(
            timestamp_ms(&json!("2023-11-14T22:13:20Z")),
            Some(1_700_000_000_000)
        );
        assert_eq!(
            timestamp_ms(&json!("2023-11-14T22:13:20.500Z")),
            Some(1_700_000_000_500)
        );
        // +02:00 is two hours ahead of UTC: the same instant, earlier epoch.
        assert_eq!(
            timestamp_ms(&json!("2023-11-15T00:13:20+02:00")),
            Some(1_700_000_000_000)
        );
        assert_eq!(timestamp_ms(&json!("not a time")), None);
        assert_eq!(timestamp_ms(&json!(null)), None);
    }

    #[test]
    fn state_is_dry_when_any_window_is_exhausted() {
        let w = |p: f64| Window {
            name: "five_hour".into(),
            used_percent: p,
            reset_at_ms: None,
        };
        assert_eq!(state_from_windows(&[w(10.0), w(99.9)]), QuotaState::Ok);
        assert_eq!(state_from_windows(&[w(10.0), w(100.0)]), QuotaState::Dry);
        assert_eq!(state_from_windows(&[]), QuotaState::Ok);
    }

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

    use crate::quota::fake::{FakeHttp, ctx, temp_dir};

    /// Write a Claude credential file so a probe gets past the token step.
    fn write_claude_credentials(home: &std::path::Path) {
        let path = claude::credentials_path(home);
        if let Some(dir) = path.parent()
            && let Err(error) = std::fs::create_dir_all(dir)
        {
            panic!("should create credentials dir: {error}");
        }
        let body = format!(
            r#"{{"claudeAiOauth":{{"accessToken":"sk-ant-oat01-fake","expiresAt":{}}}}}"#,
            u64::MAX / 2
        );
        if let Err(error) = std::fs::write(&path, body) {
            panic!("should write credentials file: {error}");
        }
    }

    const USAGE: &str = r#"{"five_hour": {"used_percentage": 30}}"#;

    #[test]
    fn an_unknown_agent_reports_no_quota_source() {
        let dir = temp_dir();
        let report = probe("aider", &ctx(dir.path(), FakeHttp::new(200, "{}"), 1_000));
        assert_eq!(report.state, QuotaState::Unknown);
        assert_eq!(report.tier, Tier::Unknown);
        assert_eq!(report.source, "none");
        assert_eq!(report.detail, "no quota source");
        assert!(report.windows.is_empty());
    }

    #[test]
    fn a_successful_probe_is_cached_and_served_for_two_minutes() {
        let dir = temp_dir();
        write_claude_credentials(dir.path());
        let first = probe("claude", &ctx(dir.path(), FakeHttp::new(200, USAGE), 1_000));
        assert_eq!(first.state, QuotaState::Ok);
        let stored = cache::Cache::new(dir.path()).load();
        assert!(stored.contains_key("claude"));

        // Inside the TTL the cache answers and the (failing) network is not hit.
        let cached = probe(
            "claude",
            &ctx(
                dir.path(),
                FakeHttp::new(500, ""),
                1_000 + cache::CACHE_TTL_MS - 1,
            ),
        );
        assert_eq!(cached, first);
    }

    #[test]
    fn a_rate_limited_probe_reuses_a_young_cached_record() {
        let dir = temp_dir();
        write_claude_credentials(dir.path());
        let good = probe("claude", &ctx(dir.path(), FakeHttp::new(200, USAGE), 1_000));
        // Cache TTL expired, stale window still open: the 429 falls back.
        let later = 1_000 + cache::CACHE_TTL_MS + 1;
        let reused = probe("claude", &ctx(dir.path(), FakeHttp::new(429, ""), later));
        assert_eq!(reused.state, good.state);
        assert_eq!(reused.windows, good.windows);
        assert_eq!(reused.checked_at_ms, good.checked_at_ms);
        assert!(reused.detail.contains("cached"), "{}", reused.detail);
        assert!(reused.detail.contains("429"), "{}", reused.detail);
    }

    #[test]
    fn a_network_failure_reuses_only_a_record_younger_than_fifteen_minutes() {
        let dir = temp_dir();
        write_claude_credentials(dir.path());
        let good = probe("claude", &ctx(dir.path(), FakeHttp::new(200, USAGE), 1_000));

        let young = 1_000 + cache::STALE_TTL_MS - 1;
        let reused = probe(
            "claude",
            &ctx(dir.path(), FakeHttp::failing(QuotaError::Network), young),
        );
        assert_eq!(reused.windows, good.windows);
        assert!(reused.detail.contains("cached"));

        let expired = 1_000 + cache::STALE_TTL_MS;
        let unknown = probe(
            "claude",
            &ctx(dir.path(), FakeHttp::failing(QuotaError::Network), expired),
        );
        assert_eq!(unknown.state, QuotaState::Unknown);
        assert!(unknown.windows.is_empty());
        assert!(!unknown.detail.contains("cached"));
    }

    #[test]
    fn a_missing_credential_is_not_masked_by_a_cached_record() {
        let dir = temp_dir();
        write_claude_credentials(dir.path());
        let _good = probe("claude", &ctx(dir.path(), FakeHttp::new(200, USAGE), 1_000));
        if let Err(error) = std::fs::remove_file(claude::credentials_path(dir.path())) {
            panic!("should remove credentials: {error}");
        }
        // Past the cache TTL, so the probe runs again and hits the missing file.
        let later = 1_000 + cache::CACHE_TTL_MS + 1;
        let report = probe("claude", &ctx(dir.path(), FakeHttp::new(200, USAGE), later));
        assert_eq!(report.state, QuotaState::Unknown);
        assert!(report.detail.contains("OAuth token"), "{}", report.detail);
        assert!(!report.detail.contains("sk-"));
    }
}
