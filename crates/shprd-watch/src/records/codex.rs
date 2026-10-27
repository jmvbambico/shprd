//! Reader for the Codex CLI conversation record.
//!
//! Codex writes one JSONL rollout per session under
//! `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`. Each line is an envelope
//! with a `type` and a `payload`; `event_msg` payloads carry the rate-limit
//! snapshot (`token_count`) and the provider errors.

use std::path::Path;

use super::{
    ReadError, RecordError, Scan, Usage, classify, each_json_line, parse_rfc3339_ms, secs_to_ms,
    truncate,
};

/// Scan one Codex rollout for errors and usage.
///
/// Only entries at or after `since_ms` are considered; the newest such
/// reading wins. Malformed lines are skipped rather than failing the scan.
pub fn scan_codex(path: &Path, since_ms: u64) -> Result<Scan, ReadError> {
    let mut scan = Scan::default();
    each_json_line(path, |line| {
        let Some(at) = line
            .get("timestamp")
            .and_then(|v| v.as_str())
            .and_then(parse_rfc3339_ms)
        else {
            return;
        };
        if at < since_ms {
            return;
        }
        scan.last_activity_ms = Some(scan.last_activity_ms.unwrap_or(at).max(at));
        let Some(payload) = line.get("payload") else {
            return;
        };
        let kind = payload.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if kind == "token_count" {
            if let Some(usage) = usage_of(payload) {
                scan.usage = Some(usage);
            }
            return;
        }
        if !kind.to_lowercase().contains("error") {
            return;
        }
        let message = payload
            .get("message")
            .or_else(|| payload.get("error").and_then(|e| e.get("message")))
            .and_then(|v| v.as_str())
            .unwrap_or(kind);
        scan.last_error = Some(RecordError {
            kind: classify(&format!("{kind} {message}")),
            message: truncate(message),
            at_ms: Some(at),
            resets_at_ms: None,
        });
    })?;
    Ok(scan)
}

/// The usage of the busiest rate-limit window in a `token_count` payload.
///
/// Codex reports a `primary` window and sometimes a `secondary` one; the one
/// with the higher `used_percent` is the binding limit.
fn usage_of(payload: &serde_json::Value) -> Option<Usage> {
    let limits = payload.get("rate_limits")?;
    let windows = ["primary", "secondary"]
        .iter()
        .filter_map(|key| limits.get(*key))
        .filter_map(usage_of_window);
    windows.max_by(|a, b| {
        a.used_percent
            .partial_cmp(&b.used_percent)
            .unwrap_or(std::cmp::Ordering::Equal)
    })
}

/// Turn one rate-limit window object into [`Usage`].
fn usage_of_window(window: &serde_json::Value) -> Option<Usage> {
    let used_percent = window.get("used_percent")?.as_f64()?;
    Some(Usage {
        used_percent,
        resets_at_ms: window
            .get("resets_at")
            .and_then(|v| v.as_i64())
            .and_then(secs_to_ms),
        window_minutes: window.get("window_minutes").and_then(|v| v.as_u64()),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::fs;
    use std::path::PathBuf;

    use super::*;
    use crate::records::ErrorKind;

    /// Write `lines` to `name` inside a fresh temp dir and return its path.
    fn fixture(name: &str, lines: &[&str]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        fs::write(&path, lines.join("\n")).unwrap();
        (dir, path)
    }

    const OLD_USAGE: &str = r#"{"timestamp":"2026-09-01T00:00:00Z","type":"event_msg","payload":{"type":"token_count","rate_limits":{"limit_id":"primary","primary":{"used_percent":10.0,"window_minutes":300,"resets_at":1788220800},"secondary":null}}}"#;
    const USAGE: &str = r#"{"timestamp":"2026-10-27T04:00:00Z","type":"event_msg","payload":{"type":"token_count","rate_limits":{"limit_id":"primary","primary":{"used_percent":40.5,"window_minutes":300,"resets_at":1793077200},"secondary":{"used_percent":88.0,"window_minutes":10080,"resets_at":1793682000},"credits":null,"plan_type":"pro"}}}"#;
    const LATER_USAGE: &str = r#"{"timestamp":"2026-10-27T05:00:00Z","type":"event_msg","payload":{"type":"token_count","rate_limits":{"primary":{"used_percent":96.0,"window_minutes":300,"resets_at":1793077200}}}}"#;
    const LIMIT_ERROR: &str = r#"{"timestamp":"2026-10-27T05:30:00Z","type":"event_msg","payload":{"type":"error","message":"stream error: rate limit reached"}}"#;

    #[test]
    fn keeps_the_latest_usage_and_the_newest_error() {
        let (_dir, path) = fixture(
            "rollout.jsonl",
            &[OLD_USAGE, r#"{"oops"#, USAGE, LATER_USAGE, LIMIT_ERROR],
        );
        let scan = scan_codex(&path, 1_792_972_800_000).unwrap();
        let usage = scan.usage.unwrap();
        assert!((usage.used_percent - 96.0).abs() < f64::EPSILON);
        assert_eq!(usage.resets_at_ms, Some(1_793_077_200_000));
        assert_eq!(usage.window_minutes, Some(300));
        let error = scan.last_error.unwrap();
        assert_eq!(error.kind, ErrorKind::RateLimit);
        assert_eq!(error.at_ms, Some(1_793_079_000_000));
        assert_eq!(scan.last_activity_ms, Some(1_793_079_000_000));
    }

    #[test]
    fn picks_the_binding_window_and_ignores_entries_before_the_cutoff() {
        let (_dir, path) = fixture("rollout.jsonl", &[OLD_USAGE, USAGE]);
        let scan = scan_codex(&path, 1_792_972_800_000).unwrap();
        assert!((scan.usage.unwrap().used_percent - 88.0).abs() < f64::EPSILON);
        assert!(scan.last_error.is_none());

        let scan = scan_codex(&path, 1_800_000_000_000).unwrap();
        assert!(scan.usage.is_none());
        assert!(scan.last_activity_ms.is_none());
    }

    #[test]
    fn classifies_an_error_only_payload() {
        let auth = r#"{"timestamp":"2026-10-27T05:30:00Z","type":"event_msg","payload":{"type":"error","message":"401 Unauthorized"}}"#;
        let (_dir, path) = fixture("rollout.jsonl", &[auth]);
        let scan = scan_codex(&path, 0).unwrap();
        assert_eq!(scan.last_error.unwrap().kind, ErrorKind::Auth);
    }
}
