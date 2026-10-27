//! Claude Code (Anthropic OAuth) usage probe.
//!
//! Ported from og installer/og_quota.py.
//!
//! The credential comes from the macOS Keychain (`security
//! find-generic-password`) or, failing that, `<home>/.claude/.credentials.json`.
//! Only the OAuth access token is read: it is never refreshed (a refresh would
//! rotate the user's real session) and never appears in a report or an error.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use crate::quota::{
    ProbeContext, QuotaError, QuotaReport, Tier, Window, first, pct, state_from_windows,
    timestamp_ms,
};

/// Anthropic's OAuth usage endpoint.
pub const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";

/// Keychain service name Claude Code writes.
pub const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";

/// `source` field of a report produced here.
pub const SOURCE: &str = "anthropic-oauth";

/// Where the Claude Code OAuth token lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialSource {
    /// Consult the macOS Keychain before the fallback file.
    pub keychain: bool,
    /// Home directory holding `.claude/.credentials.json`.
    pub home: PathBuf,
}

impl CredentialSource {
    /// Credential source for a probe context.
    #[must_use]
    pub fn from_ctx(ctx: &ProbeContext) -> Self {
        Self {
            keychain: ctx.keychain,
            home: ctx.home.clone(),
        }
    }

    /// The OAuth access token, or why there is none.
    ///
    /// `security` has no timeout in `std::process`; og bounded the call at 5 s.
    /// The keychain helper is local and returns promptly, so the probe accepts
    /// the unbounded wait rather than spawning a watchdog thread.
    pub fn token(&self, now_ms: u64) -> Result<String, QuotaError> {
        if self.keychain
            && let Some(token) = self.keychain_token(now_ms)
        {
            return token;
        }
        self.file_token(now_ms)
    }

    /// Keychain path. A failure here is not fatal: the file is tried next.
    fn keychain_token(&self, now_ms: u64) -> Option<Result<String, QuotaError>> {
        let output = Command::new("security")
            .args(["find-generic-password", "-s", KEYCHAIN_SERVICE, "-w"])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let blob: Value = serde_json::from_slice(&output.stdout).ok()?;
        Some(oauth_token(&blob, now_ms))
    }

    /// Fallback credential file. A missing file is a plain "no token".
    fn file_token(&self, now_ms: u64) -> Result<String, QuotaError> {
        let path = self.home.join(".claude").join(".credentials.json");
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(_) => return Err(QuotaError::Credential("no OAuth token")),
        };
        match serde_json::from_str::<Value>(&text) {
            Ok(blob) => oauth_token(&blob, now_ms),
            // The parse error can quote the file contents; never propagate it.
            Err(_) => Err(QuotaError::Credential("unreadable credentials file")),
        }
    }
}

/// Extract `claudeAiOauth.accessToken`, rejecting an API key and an expired
/// token, exactly as og does.
fn oauth_token(blob: &Value, now_ms: u64) -> Result<String, QuotaError> {
    let oauth = blob.get("claudeAiOauth").unwrap_or(&Value::Null);
    let token = match oauth.get("accessToken").and_then(Value::as_str) {
        Some(token) if !token.is_empty() => token,
        _ => return Err(QuotaError::Credential("no OAuth token")),
    };
    // Only `sk-ant-api*` is a long-lived API key; real Claude Code OAuth
    // access tokens are `sk-ant-oat01-*`, so the whole prefix must not be
    // rejected (og's rule).
    if token.starts_with("sk-ant-api") {
        return Err(QuotaError::Credential("API key is not an OAuth token"));
    }
    let expired = oauth
        .get("expiresAt")
        .and_then(Value::as_f64)
        .is_some_and(|ms| ms <= now_ms as f64);
    if expired {
        return Err(QuotaError::Credential("OAuth token expired"));
    }
    Ok(token.to_owned())
}

/// Path of the fallback credential file under `home`, for diagnostics.
#[must_use]
pub fn credentials_path(home: &Path) -> PathBuf {
    home.join(".claude").join(".credentials.json")
}

/// Read the local OAuth token and ask the usage endpoint for this account.
pub fn probe(ctx: &ProbeContext) -> Result<QuotaReport, QuotaError> {
    let token = CredentialSource::from_ctx(ctx).token(ctx.now_ms)?;
    // Same request og sends; the User-Agent must look like Claude Code or the
    // endpoint rejects the OAuth token.
    let headers = [
        ("Authorization", format!("Bearer {token}")),
        ("anthropic-beta", "oauth-2025-04-20".to_owned()),
        ("User-Agent", "claude-code/2.0.0".to_owned()),
    ];
    let (status, body) = ctx.http.get(USAGE_URL, &headers)?;
    if status == 429 {
        return Err(QuotaError::RateLimited);
    }
    if status != 200 {
        return Err(QuotaError::Http(status));
    }
    parse(&body, ctx.now_ms)
}

/// Parse a usage payload into a report.
///
/// `utilization` is a 0-1 fraction and `used_percentage` is already 0-100
/// (see [`crate::quota::pct`]). The live endpoint puts the windows at the top
/// level, the documented one wraps them in `rate_limits`, so both are read.
pub fn parse(body: &str, now_ms: u64) -> Result<QuotaReport, QuotaError> {
    let doc: Value = serde_json::from_str(body).map_err(|_| QuotaError::Shape)?;
    let limits = match doc.get("rate_limits").filter(|v| !v.is_null()) {
        Some(nested) => nested,
        None => &doc,
    };
    let mut windows = Vec::new();
    for name in ["five_hour", "seven_day"] {
        let Some(window) = limits.get(name).filter(|v| !v.is_null()) else {
            continue;
        };
        let Some(used) = first(window, &["utilization", "used_percentage"]).and_then(pct) else {
            continue;
        };
        windows.push(Window {
            name: name.to_owned(),
            used_percent: used,
            reset_at_ms: first(window, &["resets_at"]).and_then(timestamp_ms),
        });
    }
    let Some(binding) = windows.first() else {
        return Err(QuotaError::Shape);
    };
    let (source_reset, binding_name) = (binding.reset_at_ms, binding.name.clone());
    Ok(QuotaReport {
        agent: "claude".to_owned(),
        state: state_from_windows(&windows),
        tier: Tier::Measured,
        reset_at_ms: source_reset,
        detail: format!("{} window(s); {binding_name} binding", windows.len()),
        windows,
        source: SOURCE.to_owned(),
        checked_at_ms: now_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quota::QuotaState;
    use crate::quota::fake::{FakeHttp, ctx, temp_dir};

    const NOW: u64 = 1_700_000_000_000;
    /// A fake OAuth token: never a real one, and never asserted on.
    const FAKE: &str = "sk-ant-oat01-fake-token-for-tests";

    fn write_credentials(home: &std::path::Path, body: &str) {
        let path = credentials_path(home);
        if let Some(dir) = path.parent()
            && let Err(error) = std::fs::create_dir_all(dir)
        {
            panic!("should create credentials dir: {error}");
        }
        if let Err(error) = std::fs::write(&path, body) {
            panic!("should write credentials file: {error}");
        }
    }

    fn parsed(body: &str) -> QuotaReport {
        match parse(body, NOW) {
            Ok(report) => report,
            Err(error) => panic!("should parse usage payload: {error}"),
        }
    }

    fn credential_file(token: &str, expires_at: u64) -> String {
        format!(r#"{{"claudeAiOauth":{{"accessToken":"{token}","expiresAt":{expires_at}}}}}"#)
    }

    #[test]
    fn rejects_an_api_key_in_the_credentials_file() {
        let dir = temp_dir();
        write_credentials(
            dir.path(),
            &credential_file("sk-ant-api03-not-oauth", NOW + 60_000),
        );
        let http = FakeHttp::new(200, "{}");
        let error = match probe(&ctx(dir.path(), http, NOW)) {
            Ok(report) => panic!("API key should be rejected, got {report:?}"),
            Err(error) => error,
        };
        assert_eq!(
            error,
            QuotaError::Credential("API key is not an OAuth token")
        );
        // The reason is safe to display: no part of the key is in it.
        assert!(!error.to_string().contains("sk-ant-api"));
    }

    #[test]
    fn rejects_an_expired_token() {
        let dir = temp_dir();
        write_credentials(dir.path(), &credential_file(FAKE, NOW - 1));
        let http = FakeHttp::new(200, "{}");
        assert_eq!(
            probe(&ctx(dir.path(), http, NOW)),
            Err(QuotaError::Credential("OAuth token expired"))
        );
    }

    #[test]
    fn missing_credentials_is_a_non_secret_unknown() {
        let dir = temp_dir();
        let error = match probe(&ctx(dir.path(), FakeHttp::new(200, "{}"), NOW)) {
            Ok(report) => panic!("no credentials should not report, got {report:?}"),
            Err(error) => error,
        };
        assert_eq!(error, QuotaError::Credential("no OAuth token"));
        assert!(!error.to_string().contains("sk-"));
    }

    #[test]
    fn nested_rate_limits_use_the_fraction_form() {
        // Documented shape: windows under rate_limits, `utilization` as 0-1.
        let body = r#"{
            "rate_limits": {
                "five_hour": {"utilization": 0.42, "resets_at": "2023-11-14T22:13:20Z"},
                "seven_day": {"utilization": 0.1, "resets_at": "2023-11-21T22:13:20Z"}
            }
        }"#;
        let report = parsed(body);
        assert_eq!(report.state, QuotaState::Ok);
        assert_eq!(report.tier, Tier::Measured);
        assert_eq!(report.source, SOURCE);
        assert_eq!(report.windows.len(), 2);
        assert_eq!(report.windows[0].name, "five_hour");
        assert_eq!(report.windows[0].used_percent, 42.0);
        assert_eq!(report.windows[1].used_percent, 10.0);
        assert_eq!(report.reset_at_ms, Some(1_700_000_000_000));
        assert!(report.detail.contains("five_hour binding"));
    }

    #[test]
    fn top_level_windows_use_the_percent_form_and_five_hour_binds() {
        // Live shape (2026-09-21): windows at the top level, `used_percentage`.
        let body = r#"{
            "five_hour": {"used_percentage": 98, "resets_at": "2023-11-14T23:00:00Z"},
            "seven_day": {"used_percentage": 12, "resets_at": "2023-11-21T23:00:00Z"}
        }"#;
        let report = parsed(body);
        assert_eq!(report.windows[0].used_percent, 98.0);
        assert_eq!(report.windows[1].used_percent, 12.0);
        // five_hour binds even though seven_day has more headroom.
        assert_eq!(report.reset_at_ms, Some(1_700_002_800_000));
        assert_eq!(report.state, QuotaState::Ok);
    }

    #[test]
    fn an_exhausted_five_hour_window_is_dry() {
        let body = r#"{
            "five_hour": {"used_percentage": 100, "resets_at": 1700003400},
            "seven_day": {"used_percentage": 3}
        }"#;
        let report = parsed(body);
        assert_eq!(report.state, QuotaState::Dry);
        // Epoch seconds are accepted for resets_at and normalized to ms.
        assert_eq!(report.reset_at_ms, Some(1_700_003_400_000));
    }

    #[test]
    fn an_unrecognized_shape_is_an_error_not_a_guess() {
        assert_eq!(parse("{}", NOW), Err(QuotaError::Shape));
        assert_eq!(parse(r#"{"other": 1}"#, NOW), Err(QuotaError::Shape));
        assert_eq!(parse("not json", NOW), Err(QuotaError::Shape));
    }

    #[test]
    fn probe_asks_the_oauth_endpoint_with_the_right_header_names() {
        let dir = temp_dir();
        write_credentials(dir.path(), &credential_file(FAKE, NOW + 3_600_000));
        let fake = std::rc::Rc::new(FakeHttp::new(
            200,
            r#"{"five_hour": {"utilization": 0.5}, "seven_day": {"utilization": 0.2}}"#,
        ));
        let report = match probe(&ctx(dir.path(), std::rc::Rc::clone(&fake), NOW)) {
            Ok(report) => report,
            Err(error) => panic!("probe should succeed: {error}"),
        };
        assert_eq!(report.agent, "claude");
        assert_eq!(report.source, SOURCE);
        assert_eq!(fake.url().as_deref(), Some(USAGE_URL));
        let mut names = fake.header_names();
        names.sort();
        assert_eq!(
            names,
            ["Authorization", "User-Agent", "anthropic-beta"]
                .map(str::to_owned)
                .to_vec()
        );
        // Values stay unasserted beyond the scheme prefix: the token itself is
        // never compared, printed or stored.
        let auth = fake.header("Authorization").unwrap_or_default();
        assert!(auth.starts_with("Bearer "));
        assert_eq!(fake.count(), 1);
    }

    #[test]
    fn probe_maps_http_failures_without_echoing_the_body() {
        let dir = temp_dir();
        write_credentials(dir.path(), &credential_file(FAKE, NOW + 3_600_000));
        let limited = probe(&ctx(
            dir.path(),
            FakeHttp::new(429, r#"{"error":"secret-ish detail"}"#),
            NOW,
        ));
        assert_eq!(limited, Err(QuotaError::RateLimited));
        let unauthorized = probe(&ctx(dir.path(), FakeHttp::new(401, ""), NOW));
        assert_eq!(unauthorized, Err(QuotaError::Http(401)));
        let offline = probe(&ctx(
            dir.path(),
            FakeHttp::failing(QuotaError::Network),
            NOW,
        ));
        assert_eq!(offline, Err(QuotaError::Network));
    }

    #[test]
    fn credentials_path_points_into_the_home_dir() {
        assert!(
            credentials_path(std::path::Path::new("/home/x"))
                .ends_with(".claude/.credentials.json")
        );
    }

    /// Live probe against this machine's real token and the real endpoint.
    /// `#[ignore]`: never run by CI or by a worker, only by hand.
    #[test]
    #[ignore = "live network + real Claude Code credentials"]
    fn live_claude_usage_probe() {
        let Some(home) = std::env::var_os("HOME") else {
            panic!("HOME should be set");
        };
        let ctx = ProbeContext::new(std::path::PathBuf::from(home));
        match probe(&ctx) {
            Ok(report) => println!(
                "state={:?} tier={:?} detail={}",
                report.state, report.tier, report.detail
            ),
            Err(error) => println!("probe answered: {error}"),
        }
    }
}
