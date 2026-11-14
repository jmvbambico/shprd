//! Reader for the pi CLI conversation record.
//!
//! pi writes one JSONL file per session under
//! `~/.pi/agent/sessions/<slug>/<timestamp>_<id>.jsonl`. Messages record a
//! `stopReason`; when it is `error`, `errorMessage` holds the provider text.
//! pi records no reset time and no usage window.
//!
//! The cut reader is ported from Shepherdr's `provider_cut`
//! (`mods/tasks/bin/herdr_task/provider.py`, commit `a3f327a`, MIT); see
//! `THIRD_PARTY_NOTICES.md`.

use std::path::Path;

use super::{
    Cut, NUDGE_MARK, ReadError, RecordError, Scan, classify, each_json_line, parse_rfc3339_ms,
    truncate,
};

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

/// The provider cut a pi session ends on, or `None` when it carried on.
///
/// pi records `stopReason: "error"` plus `errorMessage` on the assistant
/// message that failed; a later answer, or a person's message that is not
/// shprd's carry-on, means the turn is over.
pub fn cut_pi(path: &Path) -> Result<Option<Cut>, ReadError> {
    let mut cut: Option<Cut> = None;
    let mut tries = 0u32;
    each_json_line(path, |line| {
        let Some(message) = line.get("message") else {
            return;
        };
        let at = message
            .get("timestamp")
            .and_then(|v| v.as_i64())
            .and_then(|ms| u64::try_from(ms).ok())
            .or_else(|| {
                line.get("timestamp")
                    .and_then(|v| v.as_str())
                    .and_then(parse_rfc3339_ms)
            })
            .unwrap_or_default();
        match message.get("role").and_then(|v| v.as_str()) {
            Some("assistant") => {
                if message.get("stopReason").and_then(|v| v.as_str()) == Some("error") {
                    tries = tries.saturating_add(1);
                    let text = message
                        .get("errorMessage")
                        .and_then(|v| v.as_str())
                        .unwrap_or("pi reported an error");
                    let id = message
                        .get("id")
                        .and_then(|v| v.as_str())
                        .or_else(|| line.get("id").and_then(|v| v.as_str()))
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("pi-{at}"));
                    cut = Some(Cut {
                        id,
                        kind: classify(text),
                        message: truncate(text),
                        at_ms: at,
                        resets_at_ms: None,
                        tries,
                    });
                } else {
                    cut = None;
                }
            }
            Some("user") => {
                let words = content_text(message.get("content"));
                cut = None;
                if !words.contains(NUDGE_MARK) {
                    tries = 0;
                }
            }
            _ => {}
        }
    })?;
    if let Some(cut) = &mut cut {
        cut.tries = cut.tries.max(1);
    }
    Ok(cut)
}

/// The words of a pi message's `content`, whether it is a string or parts.
pub(crate) fn content_text(content: Option<&serde_json::Value>) -> String {
    match content {
        Some(serde_json::Value::String(text)) => text.clone(),
        Some(serde_json::Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| part.get("text").and_then(|v| v.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::fs;
    use std::path::PathBuf;

    use super::*;
    use crate::records::{ErrorKind, NUDGE_MARK, Verdict};

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

    /// One pi assistant message with `stop` and `words`.
    fn message(id: &str, ms: u64, stop: &str, words: &str) -> String {
        format!(
            r#"{{"type":"message","timestamp":"2026-10-27T06:30:00Z","message":{{"role":"assistant","id":"{id}","stopReason":"{stop}","errorMessage":"{words}","timestamp":{ms}}}}}"#
        )
    }

    #[test]
    fn a_cut_is_classified_per_error_class() {
        let cases = [
            (
                "rate limit reached",
                ErrorKind::RateLimit,
                Verdict::Passing,
                "Rate limited",
            ),
            (
                "usage limit reached",
                ErrorKind::Quota,
                Verdict::Lasting,
                "Out of allowance",
            ),
            (
                "401 Unauthorized: invalid api key",
                ErrorKind::Auth,
                Verdict::Lasting,
                "Signed out",
            ),
            (
                "Request timed out.",
                ErrorKind::Overloaded,
                Verdict::Passing,
                "Provider overloaded",
            ),
            (
                "unsupported request",
                ErrorKind::Other,
                Verdict::Lasting,
                "Provider error",
            ),
        ];
        for (words, kind, verdict, reason) in cases {
            let line = message("m1", 1_793_073_600_000, "error", words);
            let (_dir, path) = fixture("s.jsonl", &[&line]);
            let cut = cut_pi(&path).unwrap().expect("a cut");
            assert_eq!(cut.kind, kind, "{words}");
            assert_eq!(cut.verdict(), verdict, "{words}");
            assert_eq!(cut.reason(), reason, "{words}");
        }
    }

    #[test]
    fn a_later_answer_ends_the_cut() {
        let error = message("m1", 1_793_073_600_000, "error", "rate limit reached");
        let answer = message("m2", 1_793_074_000_000, "end_turn", "");
        let (_dir, path) = fixture("s.jsonl", &[&error, &answer]);
        assert!(cut_pi(&path).unwrap().is_none());
    }

    #[test]
    fn tries_count_cut_turns_since_a_person_wrote() {
        let first = message("m1", 1_793_073_600_000, "error", "Request timed out.");
        let second = message("m2", 1_793_074_000_000, "error", "Request timed out.");
        let nudge = format!(
            r#"{{"type":"message","timestamp":"2026-10-27T06:31:00Z","message":{{"role":"user","content":[{{"type":"text","text":"Carry on: {NUDGE_MARK} (provider overloaded)"}}]}}}}"#
        );
        let (_dir, path) = fixture("s.jsonl", &[&first, &nudge, &second]);
        assert_eq!(cut_pi(&path).unwrap().unwrap().tries, 2);

        let person = r#"{"type":"message","timestamp":"2026-10-27T06:32:00Z","message":{"role":"user","content":[{"type":"text","text":"try again"}]}}"#;
        let third = message("m3", 1_793_075_000_000, "error", "Request timed out.");
        let (_dir, path) = fixture("s.jsonl", &[&first, &nudge, &second, person, &third]);
        assert_eq!(cut_pi(&path).unwrap().unwrap().tries, 1);
    }

    #[test]
    fn reports_a_missing_record_as_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.jsonl");
        assert!(matches!(scan_pi(&missing, 0), Err(ReadError::Io { .. })));
    }
}
