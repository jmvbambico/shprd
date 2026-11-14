//! Reader for the Claude CLI conversation record.
//!
//! Claude writes one JSONL file per session under
//! `~/.claude/projects/<cwd-slug>/<session>.jsonl`. Provider failures are
//! marked with `isApiErrorMessage`, carry an `error` code (and sometimes an
//! `apiError`), and phrase the limit and its reset time in
//! `message.content[].text`.
//!
//! The cut reader is ported from Shepherdr's `provider_cut`
//! (`mods/tasks/bin/herdr_task/provider.py`, commit `a3f327a`, MIT); see
//! `THIRD_PARTY_NOTICES.md`.

use std::path::Path;

use super::{
    Cut, NUDGE_MARK, ReadError, RecordError, Scan, classify, each_json_line, parse_rfc3339_ms,
    truncate, utc_year, zoned_ms,
};

/// Scan one Claude session record for errors and usage.
///
/// Only entries at or after `since_ms` are considered; the newest such error
/// wins. Malformed lines are skipped rather than failing the scan.
pub fn scan_claude(path: &Path, since_ms: u64) -> Result<Scan, ReadError> {
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
        if line.get("isApiErrorMessage").and_then(|v| v.as_bool()) != Some(true) {
            return;
        }
        let error = text_of(line.get("error"));
        let api_error = text_of(line.get("apiError"));
        let text = message_text(line);
        let found = RecordError {
            kind: classify(&format!("{error} {api_error} {text}")),
            message: truncated_message(&text, &error, &api_error),
            at_ms: Some(at),
            resets_at_ms: parse_reset(&text, at),
        };
        let newer = match &scan.last_error {
            Some(current) => found.at_ms.unwrap_or(0) >= current.at_ms.unwrap_or(0),
            None => true,
        };
        if newer {
            scan.last_error = Some(found);
        }
    })?;
    Ok(scan)
}

/// The provider cut a Claude session ends on, or `None` when it carried on.
///
/// A line with `isApiErrorMessage` is a cut turn; a normal assistant answer
/// clears the cut but keeps the count, and a person's own message (anything but
/// shprd's carry-on) starts the count again. Sidechain lines are the agent's
/// own helpers, not its conversation, so they are ignored.
pub fn cut_claude(path: &Path) -> Result<Option<Cut>, ReadError> {
    let mut cut: Option<Cut> = None;
    let mut tries = 0u32;
    each_json_line(path, |line| {
        if line.get("isSidechain").and_then(|v| v.as_bool()) == Some(true) {
            return;
        }
        match line.get("type").and_then(|v| v.as_str()) {
            Some("assistant") => {
                if line.get("isApiErrorMessage").and_then(|v| v.as_bool()) == Some(true) {
                    tries = tries.saturating_add(1);
                    let error = text_of(line.get("error"));
                    let api_error = text_of(line.get("apiError"));
                    let text = message_text(line);
                    let at = line
                        .get("timestamp")
                        .and_then(|v| v.as_str())
                        .and_then(parse_rfc3339_ms)
                        .unwrap_or_default();
                    let id = line
                        .get("uuid")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                        .or_else(|| {
                            line.get("timestamp")
                                .and_then(|v| v.as_str())
                                .map(|at| format!("claude-{at}"))
                        })
                        .unwrap_or_else(|| format!("claude-{at}"));
                    cut = Some(Cut {
                        id,
                        kind: classify(&format!("{error} {api_error} {text}")),
                        message: truncated_message(&text, &error, &api_error),
                        at_ms: at,
                        resets_at_ms: parse_reset(&text, at),
                        tries,
                    });
                } else {
                    cut = None;
                }
            }
            Some("user") => {
                let Some(words) = user_text(line) else {
                    return;
                };
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

/// What a person wrote in a `user` line, or `None` when it is a tool result.
fn user_text(line: &serde_json::Value) -> Option<String> {
    let content = line.get("message").and_then(|m| m.get("content"))?;
    if let Some(text) = content.as_str() {
        return Some(text.to_string());
    }
    let parts = content.as_array()?;
    if parts
        .iter()
        .any(|part| part.get("type").and_then(|t| t.as_str()) == Some("tool_result"))
    {
        return None;
    }
    Some(
        parts
            .iter()
            .filter_map(|part| part.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// The string form of a JSON value, or empty when it is absent.
fn text_of(value: Option<&serde_json::Value>) -> String {
    match value {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

/// The first human-readable text in `message.content[]`, or empty.
fn message_text(line: &serde_json::Value) -> String {
    let content = line
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array());
    let Some(content) = content else {
        return String::new();
    };
    content
        .iter()
        .filter_map(|part| part.get("text").and_then(|t| t.as_str()))
        .find(|t| !t.trim().is_empty())
        .unwrap_or_default()
        .to_string()
}

/// The recorded message, falling back to the error codes when Claude wrote
/// no human text.
fn truncated_message(text: &str, error: &str, api_error: &str) -> String {
    if !text.trim().is_empty() {
        return truncate(text);
    }
    let codes = format!("{error} {api_error}");
    truncate(codes.trim())
}

/// Parse `resets <Mon> <D> at <H>[:MM](am|pm) (<IANA tz>)` into epoch ms.
///
/// The year is the UTC year of the record's own timestamp, since the text
/// never carries one. Anything unparseable yields `None`.
fn parse_reset(text: &str, at_ms: u64) -> Option<u64> {
    let start = text.to_lowercase().find("resets ")? + "resets ".len();
    let tokens: Vec<&str> = text[start..].split_whitespace().collect();
    let month = month_number(tokens.first()?)?;
    let day: u32 = tokens.get(1)?.trim_end_matches(',').parse().ok()?;
    let (hour, minute) = tokens.iter().find_map(|t| parse_clock(t))?;
    let zone = tokens.iter().find_map(|t| {
        t.strip_prefix('(')?
            .strip_suffix(')')
            .or_else(|| Some(t.trim_start_matches('(')))
    })?;
    let year = utc_year(Some(at_ms))?;
    zoned_ms(zone, year, month, day, hour, minute)
}

/// The 1-based month number of an English three-letter abbreviation.
fn month_number(token: &str) -> Option<u32> {
    let name = token.trim_end_matches(',').to_lowercase();
    let months = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let index = months.iter().position(|m| name.starts_with(m))?;
    u32::try_from(index).ok()?.checked_add(1)
}

/// Parse `12pm` or `12:30am` into a 24-hour hour and a minute.
fn parse_clock(token: &str) -> Option<(u32, u32)> {
    let lower = token.to_lowercase();
    let (body, pm) = match (lower.strip_suffix("am"), lower.strip_suffix("pm")) {
        (Some(body), _) => (body, false),
        (_, Some(body)) => (body, true),
        _ => return None,
    };
    let (hour, minute) = match body.split_once(':') {
        Some((h, m)) => (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?),
        None => (body.parse::<u32>().ok()?, 0),
    };
    if !(1..=12).contains(&hour) || minute > 59 {
        return None;
    }
    let hour = match (hour, pm) {
        (12, false) => 0,
        (12, true) => 12,
        (h, true) => h + 12,
        (h, false) => h,
    };
    Some((hour, minute))
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

    const WEEKLY_LIMIT: &str = r#"{"timestamp":"2026-10-27T06:30:00Z","isApiErrorMessage":true,"error":"rate_limit","message":{"role":"assistant","content":[{"type":"text","text":"You've hit your weekly limit · resets Oct 27 at 12pm (Asia/Saigon)"}]}}"#;
    const AUTH_FAILED: &str = r#"{"timestamp":"2026-10-27T04:00:00Z","isApiErrorMessage":true,"error":"authentication_failed","message":{"role":"assistant","content":[{"type":"text","text":"Invalid API key"}]}}"#;
    const OLD_LIMIT: &str = r#"{"timestamp":"2026-10-01T00:00:00Z","isApiErrorMessage":true,"error":"rate_limit","message":{"role":"assistant","content":[{"type":"text","text":"resets Oct 1 at 1pm (UTC)"}]}}"#;
    const NORMAL: &str = r#"{"timestamp":"2026-10-27T07:00:00Z","type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"hi"}]}}"#;

    #[test]
    fn keeps_the_latest_error_and_parses_the_reset() {
        let (_dir, path) = fixture(
            "s.jsonl",
            &[OLD_LIMIT, r#"{"broken"#, AUTH_FAILED, WEEKLY_LIMIT, NORMAL],
        );
        let scan = scan_claude(&path, 1_792_972_800_000).unwrap();
        let error = scan.last_error.unwrap();
        assert_eq!(error.kind, ErrorKind::Quota);
        assert!(error.message.contains("weekly limit"));
        assert_eq!(error.at_ms, Some(1_793_082_600_000));
        assert_eq!(error.resets_at_ms, Some(1_793_077_200_000));
        assert_eq!(scan.last_activity_ms, Some(1_793_084_400_000));
        assert!(scan.usage.is_none());
    }

    /// One Claude error line for each class the ladder acts on.
    fn error_line(code: &str, text: &str) -> String {
        format!(
            r#"{{"timestamp":"2026-10-27T06:30:00Z","type":"assistant","isApiErrorMessage":true,"error":"{code}","message":{{"role":"assistant","content":[{{"type":"text","text":"{text}"}}]}}}}"#
        )
    }

    #[test]
    fn a_cut_is_classified_per_error_class() {
        let cases = [
            (
                "rate_limit",
                "Rate limit reached",
                ErrorKind::RateLimit,
                Verdict::Passing,
                "Rate limited",
            ),
            (
                "usage_limit_exceeded",
                "You've hit your weekly limit",
                ErrorKind::Quota,
                Verdict::Lasting,
                "Out of allowance",
            ),
            (
                "authentication_failed",
                "Invalid API key",
                ErrorKind::Auth,
                Verdict::Lasting,
                "Signed out",
            ),
            (
                "overloaded_error",
                "Provider overloaded",
                ErrorKind::Overloaded,
                Verdict::Passing,
                "Provider overloaded",
            ),
            (
                "invalid_request",
                "The request was not supported",
                ErrorKind::Other,
                Verdict::Lasting,
                "Provider error",
            ),
        ];
        for (code, text, kind, verdict, reason) in cases {
            let line = error_line(code, text);
            let (_dir, path) = fixture("s.jsonl", &[&line]);
            let cut = cut_claude(&path).unwrap().expect("a cut");
            assert_eq!(cut.kind, kind, "{code}");
            assert_eq!(cut.verdict(), verdict, "{code}");
            assert_eq!(cut.reason(), reason, "{code}");
            assert_eq!(cut.tries, 1, "{code}");
        }
    }

    #[test]
    fn a_later_answer_ends_the_cut() {
        let error = error_line("rate_limit", "Rate limit reached");
        let (_dir, path) = fixture("s.jsonl", &[&error, NORMAL]);
        assert!(cut_claude(&path).unwrap().is_none());
    }

    #[test]
    fn tries_count_cut_turns_since_a_person_wrote() {
        let error = error_line("overloaded_error", "Provider overloaded");
        let nudge = format!(
            r#"{{"timestamp":"2026-10-27T06:31:00Z","type":"user","message":{{"role":"user","content":[{{"type":"text","text":"Please carry on: {NUDGE_MARK} (provider overloaded)"}}]}}}}"#
        );
        let (_dir, path) = fixture("s.jsonl", &[&error, &nudge, &error]);
        assert_eq!(cut_claude(&path).unwrap().unwrap().tries, 2);

        let person = r#"{"timestamp":"2026-10-27T06:32:00Z","type":"user","message":{"role":"user","content":[{"type":"text","text":"try again"}]}}"#;
        let (_dir, path) = fixture("s.jsonl", &[&error, &nudge, &error, person, &error]);
        assert_eq!(cut_claude(&path).unwrap().unwrap().tries, 1);
    }

    #[test]
    fn a_tool_result_does_not_start_the_tries_again() {
        let error = error_line("overloaded_error", "Provider overloaded");
        let tool = r#"{"timestamp":"2026-10-27T06:31:00Z","type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"ok"}]}}"#;
        let (_dir, path) = fixture("s.jsonl", &[&error, tool, &error]);
        assert_eq!(cut_claude(&path).unwrap().unwrap().tries, 2);
    }

    #[test]
    fn falls_back_to_the_api_error_code_when_no_text_is_recorded() {
        let line = r#"{"timestamp":"2026-10-27T05:00:00Z","isApiErrorMessage":true,"error":"rate_limit","apiError":"usage_limit_reached","message":{"role":"assistant","content":[]}}"#;
        let (_dir, path) = fixture("s.jsonl", &[line]);
        let scan = scan_claude(&path, 0).unwrap();
        let error = scan.last_error.unwrap();
        // An exhausted allowance outranks the passing rate-limit code.
        assert_eq!(error.kind, ErrorKind::Quota);
        assert_eq!(error.message, "rate_limit usage_limit_reached");
        assert!(error.resets_at_ms.is_none());
    }

    #[test]
    fn ignores_everything_before_the_cutoff() {
        let (_dir, path) = fixture("s.jsonl", &[OLD_LIMIT, AUTH_FAILED]);
        let scan = scan_claude(&path, 1_792_972_800_000).unwrap();
        assert_eq!(scan.last_error.unwrap().kind, ErrorKind::Auth);
    }

    #[test]
    fn reports_a_missing_record_as_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.jsonl");
        match scan_claude(&missing, 0) {
            Err(ReadError::Io { path, .. }) => assert_eq!(path, missing),
            other => panic!("expected an io error, got {other:?}"),
        }
    }

    #[test]
    fn parses_the_reset_text_of_a_record() {
        let at = 1_793_082_600_000;
        let text = "You've hit your weekly limit · resets Oct 27 at 12pm (Asia/Saigon)";
        assert_eq!(parse_reset(text, at), Some(1_793_077_200_000));
        assert_eq!(
            parse_reset("resets Oct 20 at 6pm (Asia/Saigon)", at),
            Some(1_792_494_000_000)
        );
        assert_eq!(parse_reset("resets someday at 9:15am (UTC)", at), None);
    }
}
