//! Codex (ChatGPT OAuth) usage probe.
//!
//! Ported from og installer/og_quota.py.
//!
//! The credential is the ChatGPT OAuth token in `<home>/.codex/auth.json`
//! (`tokens.access_token`, plus `tokens.account_id` for the account header).
//! The token is never refreshed and never leaves the request header.

use std::path::Path;

use serde_json::Value;

use crate::quota::{
    ProbeContext, QuotaError, QuotaReport, Tier, Window, first, pct, state_from_windows,
    timestamp_ms,
};

/// ChatGPT's Codex usage endpoint.
pub const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";

/// `source` field of a report produced here.
pub const SOURCE: &str = "codex-wham";

/// Path of Codex's auth file under `home`.
#[must_use]
pub fn auth_path(home: &Path) -> std::path::PathBuf {
    home.join(".codex").join("auth.json")
}

/// `(access_token, account_id)` from `auth.json`, or why there is none.
pub fn token(home: &Path) -> Result<(String, Option<String>), QuotaError> {
    let text = match std::fs::read_to_string(auth_path(home)) {
        Ok(text) => text,
        Err(_) => return Err(QuotaError::Credential("no codex/openai OAuth token")),
    };
    // The parse error can quote the file; never propagate it.
    let doc: Value = match serde_json::from_str(&text) {
        Ok(doc) => doc,
        Err(_) => return Err(QuotaError::Credential("unreadable auth.json")),
    };
    let tokens = doc.get("tokens").unwrap_or(&Value::Null);
    let access = match tokens.get("access_token").and_then(Value::as_str) {
        Some(token) if !token.is_empty() => token,
        _ => return Err(QuotaError::Credential("no codex/openai OAuth token")),
    };
    let account = tokens
        .get("account_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned);
    Ok((access.to_owned(), account))
}

/// Reset instant for one wham window.
///
/// `reset_at` is an absolute epoch (seconds or millis); `reset_after_seconds`
/// is relative to the probe, and is only used when there is no absolute value.
fn reset_ms(window: &Value, now_ms: u64) -> Option<u64> {
    if let Some(absolute) = first(window, &["reset_at"]).and_then(timestamp_ms) {
        return Some(absolute);
    }
    let after = first(window, &["reset_after_seconds"]).and_then(Value::as_f64)?;
    if !(after.is_finite() && after >= 0.0) {
        return None;
    }
    now_ms.checked_add((after * 1000.0).round() as u64)
}

/// Read the local token and ask the wham endpoint for this account.
pub fn probe(ctx: &ProbeContext) -> Result<QuotaReport, QuotaError> {
    let (access, account) = token(&ctx.home)?;
    let mut headers = vec![("Authorization", format!("Bearer {access}"))];
    if let Some(account) = account {
        headers.push(("ChatGPT-Account-Id", account));
    }
    let (status, body) = ctx.http.get(USAGE_URL, &headers)?;
    if status == 429 {
        return Err(QuotaError::RateLimited);
    }
    if status != 200 {
        return Err(QuotaError::Http(status));
    }
    parse(&body, ctx.now_ms)
}

/// Parse a wham payload into a report.
pub fn parse(body: &str, now_ms: u64) -> Result<QuotaReport, QuotaError> {
    let doc: Value = serde_json::from_str(body).map_err(|_| QuotaError::Shape)?;
    let limits = doc.get("rate_limit").unwrap_or(&Value::Null);
    let mut windows = Vec::new();
    for name in ["primary_window", "secondary_window"] {
        let Some(window) = limits.get(name).filter(|v| !v.is_null()) else {
            continue;
        };
        let Some(used) =
            first(window, &["used_percent", "used_percentage", "percent_used"]).and_then(pct)
        else {
            continue;
        };
        windows.push(Window {
            // "primary_window" reads as "primary" in a report.
            name: name.trim_end_matches("_window").to_owned(),
            used_percent: used,
            reset_at_ms: reset_ms(window, now_ms),
        });
    }
    let Some(binding) = windows.first() else {
        return Err(QuotaError::Shape);
    };
    let (binding_reset, binding_name) = (binding.reset_at_ms, binding.name.clone());
    let mut detail = format!("{} window(s); {binding_name} binding", windows.len());
    if let Some(credits) = doc
        .get("credits")
        .filter(|v| v.as_object().is_some_and(|o| !o.is_empty()))
    {
        let unlimited = credits
            .get("unlimited")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if unlimited {
            detail.push_str("; credits unlimited");
        } else if let Some(balance) = credits.get("balance").filter(|v| !v.is_null()) {
            detail.push_str(&format!("; credits {balance}"));
        }
    }
    Ok(QuotaReport {
        agent: "codex".to_owned(),
        state: state_from_windows(&windows),
        tier: Tier::Measured,
        reset_at_ms: binding_reset,
        detail,
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
    /// A fake token: never a real one.
    const FAKE: &str = "fake-chatgpt-access-token";

    fn write_auth(home: &Path, body: &str) {
        let path = auth_path(home);
        if let Some(dir) = path.parent()
            && let Err(error) = std::fs::create_dir_all(dir)
        {
            panic!("should create auth dir: {error}");
        }
        if let Err(error) = std::fs::write(&path, body) {
            panic!("should write auth.json: {error}");
        }
    }

    fn auth_file(token: &str, account: Option<&str>) -> String {
        match account {
            Some(id) => {
                format!(r#"{{"tokens":{{"access_token":"{token}","account_id":"{id}"}}}}"#)
            }
            None => format!(r#"{{"tokens":{{"access_token":"{token}"}}}}"#),
        }
    }

    fn parsed(body: &str) -> QuotaReport {
        match parse(body, NOW) {
            Ok(report) => report,
            Err(error) => panic!("should parse wham payload: {error}"),
        }
    }

    #[test]
    fn percent_fields_from_both_nesting_shapes_normalize() {
        // Documented shape: used_percent under rate_limit.*_window.
        let report = parsed(
            r#"{
                "rate_limit": {
                    "primary_window": {"used_percent": 35, "reset_at": 1700003600},
                    "secondary_window": {"used_percent": 5, "reset_at": 1700600000}
                },
                "credits": {"balance": 12.5}
            }"#,
        );
        assert_eq!(report.agent, "codex");
        assert_eq!(report.state, QuotaState::Ok);
        assert_eq!(report.windows.len(), 2);
        assert_eq!(report.windows[0].name, "primary");
        assert_eq!(report.windows[0].used_percent, 35.0);
        assert_eq!(report.windows[1].used_percent, 5.0);
        // primary binds even though the secondary has more headroom.
        assert_eq!(report.reset_at_ms, Some(1_700_003_600_000));
        assert!(report.detail.contains("primary binding"));
        assert!(report.detail.contains("credits 12.5"));
    }

    #[test]
    fn fraction_and_millisecond_shapes_are_accepted() {
        let report = parsed(
            r#"{"rate_limit": {"primary_window": {"used_percentage": 0.5, "reset_after_seconds": 90}},
                "credits": {"unlimited": true}}"#,
        );
        assert_eq!(report.windows[0].used_percent, 50.0);
        assert_eq!(report.reset_at_ms, Some(NOW + 90_000));
        assert!(report.detail.contains("credits unlimited"));
    }

    #[test]
    fn a_fully_used_primary_window_is_dry() {
        let report = parsed(r#"{"rate_limit": {"primary_window": {"percent_used": 100}}}"#);
        assert_eq!(report.state, QuotaState::Dry);
        assert_eq!(report.reset_at_ms, None);
    }

    #[test]
    fn missing_token_is_a_non_secret_unknown() {
        let dir = temp_dir();
        let error = match token(dir.path()) {
            Ok((token, _)) => panic!("expected no token, got {} chars", token.len()),
            Err(error) => error,
        };
        assert_eq!(error, QuotaError::Credential("no codex/openai OAuth token"));
        assert!(!error.to_string().contains(FAKE));

        write_auth(dir.path(), r#"{"tokens":{}}"#);
        assert_eq!(
            token(dir.path()),
            Err(QuotaError::Credential("no codex/openai OAuth token"))
        );
        write_auth(dir.path(), "{ not json");
        assert_eq!(
            token(dir.path()),
            Err(QuotaError::Credential("unreadable auth.json"))
        );
    }

    #[test]
    fn probe_sends_the_account_header_names_only() {
        let dir = temp_dir();
        write_auth(dir.path(), &auth_file(FAKE, Some("acct-123")));
        let fake = std::rc::Rc::new(FakeHttp::new(
            200,
            r#"{"rate_limit": {"primary_window": {"used_percent": 10}}}"#,
        ));
        let report = match probe(&ctx(dir.path(), std::rc::Rc::clone(&fake), NOW)) {
            Ok(report) => report,
            Err(error) => panic!("probe should succeed: {error}"),
        };
        assert_eq!(report.source, SOURCE);
        assert_eq!(fake.url().as_deref(), Some(USAGE_URL));
        let mut names = fake.header_names();
        names.sort();
        assert_eq!(
            names,
            ["Authorization", "ChatGPT-Account-Id"]
                .map(str::to_owned)
                .to_vec()
        );
        assert!(
            fake.header("Authorization")
                .unwrap_or_default()
                .starts_with("Bearer ")
        );
    }

    #[test]
    fn probe_without_account_id_sends_one_header_and_maps_429() {
        let dir = temp_dir();
        write_auth(dir.path(), &auth_file(FAKE, None));
        let fake = std::rc::Rc::new(FakeHttp::new(429, "{}"));
        assert_eq!(
            probe(&ctx(dir.path(), std::rc::Rc::clone(&fake), NOW)),
            Err(QuotaError::RateLimited)
        );
        assert_eq!(fake.header_names(), ["Authorization".to_owned()].to_vec());
        assert_eq!(
            probe(&ctx(dir.path(), FakeHttp::new(500, ""), NOW)),
            Err(QuotaError::Http(500))
        );
    }
}
