//! Kilo credit probe, driven by the local `kilo profile --json` command.
//!
//! Ported from og installer/og_quota.py.
//!
//! `balance` is Kilo's *paid* credit pool. A worker pinned to the free router
//! (`kilo/kilo-auto/free`) never draws on it, so the balance stays zero and a
//! measured `dry` would blacklist a healthy worker. A free pin therefore
//! reports `ok` at tier `assumed`, with no number (og's rule).

use std::path::Path;
use std::time::Duration;

use serde_json::Value;

use crate::detect::{DetectError, run_with_timeout};
use crate::quota::{ProbeContext, QuotaError, QuotaReport, QuotaState, Tier, first};

/// `source` field of a report produced here.
pub const SOURCE: &str = "kilo-profile";

/// How long `kilo profile --json` may run (og's 8 s).
///
/// A `kilo` that hangs must not freeze `shprd doctor`.
pub const PROFILE_TIMEOUT: Duration = Duration::from_secs(8);

/// Why the probe gave up. Fixed text: no command output.
const PROFILE_TIMED_OUT: &str = "kilo profile timed out";

/// Run `kilo profile --json` and read the paid balance.
pub fn probe(ctx: &ProbeContext) -> Result<QuotaReport, QuotaError> {
    probe_with_timeout(ctx, PROFILE_TIMEOUT)
}

/// [`probe`] with an explicit timeout, so a test can prove a hung `kilo` is
/// killed instead of waiting out the real 8 s.
fn probe_with_timeout(ctx: &ProbeContext, timeout: Duration) -> Result<QuotaReport, QuotaError> {
    let args = ["profile".to_owned(), "--json".to_owned()];
    let output =
        run_with_timeout(&ctx.kilo_bin, &args, &[], timeout).map_err(|error| match error {
            DetectError::Timeout { .. } => QuotaError::Command(PROFILE_TIMED_OUT),
            _ => QuotaError::Command("kilo binary not found"),
        })?;
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

/// The model Kilo itself is configured to use, read from its own config.
///
/// Only the top-level `model` string of `<home>/.config/kilo/kilo.jsonc` is
/// read: no other key is looked at and nothing is returned for a missing or
/// unreadable file, so a Kilo user who pinned the free router in their own
/// client is recognized without shprd naming that model.
#[must_use]
pub fn default_model(home: &Path) -> Option<String> {
    let path = home.join(".config").join("kilo").join("kilo.jsonc");
    top_level_model(&std::fs::read_to_string(path).ok()?)
}

/// The top-level `"model"` string of JSON-with-comments text, if it has one.
fn top_level_model(text: &str) -> Option<String> {
    let chars: Vec<char> = uncommented(text).chars().collect();
    let mut depth = 0usize;
    let mut index = 0usize;
    while index < chars.len() {
        match chars[index] {
            '{' | '[' => {
                depth += 1;
                index += 1;
            }
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                index += 1;
            }
            '"' => {
                let (key, after) = string_at(&chars, index)?;
                index = after;
                if depth == 1 && key == "model" {
                    let value_at = skip_ws(&chars, after);
                    if chars.get(value_at) != Some(&':') {
                        continue;
                    }
                    let quote = skip_ws(&chars, value_at + 1);
                    if chars.get(quote) != Some(&'"') {
                        continue;
                    }
                    return string_at(&chars, quote).map(|(value, _)| value);
                }
            }
            _ => index += 1,
        }
    }
    None
}

/// `index` moved past any whitespace.
fn skip_ws(chars: &[char], index: usize) -> usize {
    let mut at = index;
    while matches!(chars.get(at), Some(found) if found.is_whitespace()) {
        at += 1;
    }
    at
}

/// The string literal opening at `start`, unescaped, and the index past it.
fn string_at(chars: &[char], start: usize) -> Option<(String, usize)> {
    let mut text = String::new();
    let mut index = start + 1;
    while let Some(&found) = chars.get(index) {
        match found {
            '\\' => {
                index += 1;
                if let Some(&escaped) = chars.get(index) {
                    text.push(escaped);
                }
            }
            '"' => return Some((text, index + 1)),
            other => text.push(other),
        }
        index += 1;
    }
    None
}

/// `text` with `//` line comments removed, never inside a string literal.
fn uncommented(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut quoted = false;
    while let Some(found) = chars.next() {
        match found {
            '"' => {
                quoted = !quoted;
                out.push(found);
            }
            '\\' if quoted => {
                out.push(found);
                if let Some(next) = chars.next() {
                    out.push(next);
                }
            }
            '/' if !quoted && chars.peek() == Some(&'/') => {
                for next in chars.by_ref() {
                    if next == '\n' {
                        out.push(next);
                        break;
                    }
                }
            }
            other => out.push(other),
        }
    }
    out
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

    #[test]
    fn the_default_model_comes_from_kilos_own_config() {
        let home = temp_dir();
        let dir = home.path().join(".config").join("kilo");
        assert!(std::fs::create_dir_all(&dir).is_ok());
        let body = "{\n  // the router this user picked\n  \"model\": \"ohmyproxy/kilo-auto/free\", // pinned\n  \"other\": {\"model\": \"kilo/openai/gpt-5\"},\n}\n";
        assert!(std::fs::write(dir.join("kilo.jsonc"), body).is_ok());

        let model = default_model(home.path());
        assert_eq!(model.as_deref(), Some("ohmyproxy/kilo-auto/free"));
        assert!(free_pin(model.as_deref()));
    }

    #[test]
    fn no_config_or_no_model_key_is_no_default_model() {
        let home = temp_dir();
        assert_eq!(default_model(home.path()), None);

        let dir = home.path().join(".config").join("kilo");
        assert!(std::fs::create_dir_all(&dir).is_ok());
        let only_nested = "{\"provider\": {\"model\": \"kilo/openai/gpt-5\"}}";
        assert!(std::fs::write(dir.join("kilo.jsonc"), only_nested).is_ok());
        assert_eq!(default_model(home.path()), None);
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

    /// A hanging shell script standing in for a wedged `kilo` binary.
    fn write_hanging_script(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        if let Err(error) = std::fs::write(&path, "#!/bin/sh\nsleep 30\n") {
            panic!("writing {name} failed: {error}");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::Permissions::from_mode(0o755);
            if let Err(error) = std::fs::set_permissions(&path, mode) {
                panic!("chmod {name} failed: {error}");
            }
        }
        path
    }

    #[test]
    fn a_hanging_kilo_profile_times_out() {
        let dir = temp_dir();
        let mut probe_ctx = ctx(dir.path(), FakeHttp::new(200, "{}"), NOW);
        // A fake `kilo`, so the real binary is never run.
        probe_ctx.kilo_bin = write_hanging_script(dir.path(), "kilo");
        let started = std::time::Instant::now();
        let error = match probe_with_timeout(&probe_ctx, std::time::Duration::from_millis(300)) {
            Ok(report) => panic!("a hanging kilo should not report, got {report:?}"),
            Err(error) => error,
        };
        assert_eq!(error, QuotaError::Command("kilo profile timed out"));
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(3),
            "the kilo probe should give up promptly, took {elapsed:?}"
        );
    }
}
