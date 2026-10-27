//! Reader for the pi CLI conversation record.
//!
//! pi writes one JSONL file per session under
//! `~/.pi/agent/sessions/<slug>/<timestamp>_<id>.jsonl`. Messages record a
//! `stopReason`; when it is `error`, `errorMessage` holds the provider text.
//! pi records no reset time and no usage window.

use std::path::Path;

use super::{ReadError, RecordError, Scan, classify, each_json_line, parse_rfc3339_ms, truncate};

/// Scan one pi session record for errors.
///
/// Only entries at or after `since_ms` are considered; the newest such error
/// wins. Malformed lines are skipped rather than failing the scan.
pub fn scan_pi(path: &Path, since_ms: u64) -> Result<Scan, ReadError> {
    let mut scan = Scan::default();
    each_json_line(path, |line| {
        let message = line.get("message");
        let at = message
            .and_then(|m| m.get("timestamp"))
            .and_then(|v| v.as_i64())
            .and_then(|ms| u64::try_from(ms).ok())
            .or_else(|| {
                line.get("timestamp")
                    .and_then(|v| v.as_str())
                    .and_then(parse_rfc3339_ms)
            });
        let Some(at) = at else {
            return;
        };
        if at < since_ms {
            return;
        }
        scan.last_activity_ms = Some(scan.last_activity_ms.unwrap_or(at).max(at));
        let Some(message) = message else {
            return;
        };
        if message.get("stopReason").and_then(|v| v.as_str()) != Some("error") {
            return;
        }
        let text = message
            .get("errorMessage")
            .and_then(|v| v.as_str())
            .unwrap_or("pi reported an error");
        scan.last_error = Some(RecordError {
            kind: classify(text),
            message: truncate(text),
            at_ms: Some(at),
            resets_at_ms: None,
        });
    })?;
    Ok(scan)
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

    const OLD_ERROR: &str = r#"{"type":"message","timestamp":"2026-09-01T00:00:00Z","message":{"role":"assistant","provider":"anthropic","model":"m","stopReason":"error","errorMessage":"Request timed out.","timestamp":1788220800000}}"#;
    const AUTH_ERROR: &str = r#"{"type":"message","timestamp":"2026-10-27T04:00:00Z","message":{"role":"assistant","provider":"anthropic","model":"m","stopReason":"error","errorMessage":"401: {\"message\":\"Invalid 'Authorization' header or token.\"}","timestamp":1793073600000}}"#;
    const TIMEOUT: &str = r#"{"type":"message","timestamp":"2026-10-27T05:00:00Z","message":{"role":"assistant","provider":"openai","model":"m","stopReason":"error","errorMessage":"Request timed out.","timestamp":1793077200000}}"#;
    const OK: &str = r#"{"type":"message","timestamp":"2026-10-27T06:00:00Z","message":{"role":"assistant","provider":"openai","model":"m","stopReason":"end_turn","timestamp":1793080800000}}"#;

    #[test]
    fn keeps_the_newest_error_and_skips_the_rest() {
        let (_dir, path) = fixture("s.jsonl", &[OLD_ERROR, r#"{"nope"#, AUTH_ERROR, OK]);
        let scan = scan_pi(&path, 1_792_972_800_000).unwrap();
        let error = scan.last_error.unwrap();
        assert_eq!(error.kind, ErrorKind::Auth);
        assert_eq!(error.at_ms, Some(1_793_073_600_000));
        assert_eq!(scan.last_activity_ms, Some(1_793_080_800_000));
    }

    #[test]
    fn ignores_entries_before_the_cutoff() {
        let (_dir, path) = fixture("s.jsonl", &[OLD_ERROR, TIMEOUT]);
        let scan = scan_pi(&path, 1_792_972_800_000).unwrap();
        let error = scan.last_error.unwrap();
        assert_eq!(error.kind, ErrorKind::Overloaded);
        assert_eq!(error.at_ms, Some(1_793_077_200_000));

        let scan = scan_pi(&path, 1_800_000_000_000).unwrap();
        assert!(scan.last_error.is_none());
        assert!(scan.last_activity_ms.is_none());
    }

    #[test]
    fn reports_a_missing_record_as_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.jsonl");
        assert!(matches!(scan_pi(&missing, 0), Err(ReadError::Io { .. })));
    }
}
