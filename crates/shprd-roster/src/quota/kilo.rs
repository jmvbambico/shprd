//! Kilo credit probe, driven by the local `kilo profile --json` command.
//!
//! Ported from og installer/og_quota.py.
//!
//! `balance` is Kilo's *paid* credit pool. A worker pinned to the free router
//! (`kilo/kilo-auto/free`) never draws on it, so the balance stays zero and a
//! measured `dry` would blacklist a healthy worker. A free pin therefore
//! reports `ok` at tier `assumed`, with no number (og's rule).

use std::process::Command;

use serde_json::Value;

use crate::quota::{ProbeContext, QuotaError, QuotaReport, QuotaState, Tier, first};

/// `source` field of a report produced here.
pub const SOURCE: &str = "kilo-profile";

/// Run `kilo profile --json` and read the paid balance.
///
/// The command is local and quick; `std::process` has no timeout, so a hung
/// binary is the user's problem (og bounded it at 8 s, which would need a
/// watchdog thread here).
pub fn probe(ctx: &ProbeContext) -> Result<QuotaReport, QuotaError> {
    let output = Command::new(&ctx.kilo_bin)
        .args(["profile", "--json"])
        .output()
        .map_err(|_| QuotaError::Command("kilo binary not found"))?;
    if !output.status.success() {
        return Err(QuotaError::Command("kilo profile failed"));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    balance(&text, ctx.free_pin, ctx.now_ms)
}

/// Parse `kilo profile --json` output into a report.
pub fn balance(body: &str, free_pin: bool, now_ms: u64) -> Result<QuotaReport, QuotaError> {
    let doc: Value = serde_json::from_str(body).map_err(|_| QuotaError::Shape)?;
    let Some(amount) = first(&doc, &["balance", "remaining_balance", "credits"]) else {
        return Err(QuotaError::Shape);
    };
    let credits = amount.as_f64().ok_or(QuotaError::Shape)?;
    if free_pin {
        // The free tier exposes no queryable remaining, so the paid balance is
        // reported as not binding instead of as a meaningless measured zero.
        return Ok(QuotaReport {
            agent: "kilo".to_owned(),
            state: QuotaState::Ok,
            tier: Tier::Assumed,
            windows: Vec::new(),
            reset_at_ms: None,
            source: SOURCE.to_owned(),
            detail: format!("kilo profile --json; free pin, paid balance {credits} does not bind"),
            checked_at_ms: now_ms,
        });
    }
    Ok(QuotaReport {
        agent: "kilo".to_owned(),
        state: if credits > 0.0 {
            QuotaState::Ok
        } else {
            QuotaState::Dry
        },
        tier: Tier::Measured,
        windows: Vec::new(),
        reset_at_ms: None,
        source: SOURCE.to_owned(),
        detail: format!("kilo profile --json; balance {credits}"),
        checked_at_ms: now_ms,
    })
}

/// True when a pinned Kilo model routes inside the free tier.
///
/// Derived from the id's shape, never from a list of tier names, so a future
/// `kilo/<router>/free` maps without a code change (og's `_kilo_free_pin`).
/// A concrete free model is `kilo/<vendor>/<model>:free`, whose `:free` suffix
/// counts too.
#[must_use]
pub fn free_pin(model_id: Option<&str>) -> bool {
    let Some(id) = model_id else {
        return false;
    };
    let last = id
        .trim()
        .to_ascii_lowercase()
        .rsplit('/')
        .find(|seg| !seg.is_empty())
        .unwrap_or_default()
        .to_owned();
    last == "free" || last.ends_with(":free")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quota::fake::{FakeHttp, ctx, temp_dir};

    const NOW: u64 = 1_700_000_000_000;

    fn parsed(body: &str, free: bool) -> QuotaReport {
        match balance(body, free, NOW) {
            Ok(report) => report,
            Err(error) => panic!("should parse kilo profile output: {error}"),
        }
    }

    #[test]
    fn a_positive_balance_is_ok_and_a_zero_balance_is_dry() {
        let paid = parsed(r#"{"balance": 4.5}"#, false);
        assert_eq!(paid.state, QuotaState::Ok);
        assert_eq!(paid.tier, Tier::Measured);
        assert_eq!(paid.source, SOURCE);
        assert_eq!(paid.agent, "kilo");
        assert!(paid.windows.is_empty());
        assert_eq!(paid.reset_at_ms, None);

        assert_eq!(parsed(r#"{"balance": 0}"#, false).state, QuotaState::Dry);
        assert_eq!(parsed(r#"{"balance": 0.0}"#, false).state, QuotaState::Dry);
    }

    #[test]
    fn every_balance_field_name_is_accepted() {
        assert_eq!(parsed(r#"{"balance": 2}"#, false).state, QuotaState::Ok);
        assert_eq!(
            parsed(r#"{"remaining_balance": 1}"#, false).state,
            QuotaState::Ok
        );
        assert_eq!(parsed(r#"{"credits": 3}"#, false).state, QuotaState::Ok);
        assert_eq!(parsed(r#"{"credits": 0}"#, false).state, QuotaState::Dry);
    }

    #[test]
    fn a_free_pin_is_ok_at_tier_assumed_even_with_no_balance() {
        let report = parsed(r#"{"balance": 0}"#, true);
        assert_eq!(report.state, QuotaState::Ok);
        assert_eq!(report.tier, Tier::Assumed);
        assert!(report.detail.contains("free pin"), "{}", report.detail);
        assert!(report.detail.contains("does not bind"), "{}", report.detail);
    }

    #[test]
    fn an_unusable_payload_is_a_shape_error() {
        assert_eq!(balance("{}", false, NOW), Err(QuotaError::Shape));
        assert_eq!(balance("not json", false, NOW), Err(QuotaError::Shape));
        assert_eq!(
            balance(r#"{"balance": "lots"}"#, false, NOW),
            Err(QuotaError::Shape)
        );
    }

    #[test]
    fn free_pins_are_recognized_by_shape() {
        assert!(free_pin(Some("kilo/kilo-auto/free")));
        assert!(free_pin(Some("kilo/poolside/laguna-s-2.1:free")));
        assert!(free_pin(Some("  FREE  ")));
        assert!(!free_pin(Some("kilo/openai/gpt-5")));
        assert!(!free_pin(Some("kilo/openai/free-lunch")));
        assert!(!free_pin(Some("")));
        assert!(!free_pin(None));
    }

    #[cfg(unix)]
    fn fake_kilo(dir: &std::path::Path, stdout: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("kilo");
        let script = format!("#!/bin/sh\necho '{stdout}'\n");
        if let Err(error) = std::fs::write(&path, script) {
            panic!("should write fake kilo: {error}");
        }
        if let Err(error) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        {
            panic!("should make fake kilo executable: {error}");
        }
        path
    }

    #[cfg(unix)]
    #[test]
    fn probe_runs_the_injected_binary() {
        let dir = temp_dir();
        let bin = fake_kilo(dir.path(), r#"{"balance": 7}"#);
        let mut probe_ctx = ctx(dir.path(), FakeHttp::new(200, "{}"), NOW);
        probe_ctx.kilo_bin = bin;
        let report = match probe(&probe_ctx) {
            Ok(report) => report,
            Err(error) => panic!("probe should succeed: {error}"),
        };
        assert_eq!(report.state, QuotaState::Ok);
        assert_eq!(report.tier, Tier::Measured);
    }

    #[test]
    fn a_missing_binary_reports_a_command_error() {
        // `ctx` points at a binary name that does not exist on any PATH.
        let dir = temp_dir();
        let error = match probe(&ctx(dir.path(), FakeHttp::new(200, "{}"), NOW)) {
            Ok(report) => panic!("should not report, got {report:?}"),
            Err(error) => error,
        };
        assert_eq!(error, QuotaError::Command("kilo binary not found"));
    }
}
