//! Reader for the Codex CLI conversation record.
//!
//! Codex writes one JSONL rollout per session under
//! `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`. Each line is an envelope
//! with a `type` and a `payload`; `event_msg` payloads carry the rate-limit
//! snapshot (`token_count`) and the provider errors.
//!
//! The cut reader is ported from Shepherdr's `provider_cut`
//! (`mods/tasks/bin/herdr_task/provider.py`, commit `a3f327a`, MIT); see
//! `THIRD_PARTY_NOTICES.md`.

use std::path::Path;

use super::{
    Cut, NUDGE_MARK, ReadError, RecordError, Scan, Usage, classify, each_json_line,
    parse_rfc3339_ms, secs_to_ms, truncate,
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

/// The provider cut a Codex rollout ends on, or `None` when it carried on.
///
/// Codex records a turn's end as `task_complete` or `turn_aborted` with an
/// `error` object; a new turn (`task_started`), a normal assistant item, or a
/// person's message after it means the cut is over. A message shprd itself sent
/// to carry on does not start the tries again (`NUDGE_MARK`).
pub fn cut_codex(path: &Path) -> Result<Option<Cut>, ReadError> {
    let mut cut: Option<Cut> = None;
    let mut tries = 0u32;
    each_json_line(path, |line| {
        let Some(payload) = line.get("payload") else {
            return;
        };
        let kind = payload.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match (line.get("type").and_then(|v| v.as_str()), kind) {
            (Some("event_msg"), "task_started") => cut = None,
            (Some("event_msg"), "task_complete" | "turn_aborted") => {
                let Some(error) = payload.get("error").filter(|e| e.is_object()) else {
                    cut = None;
                    return;
                };
                tries = tries.saturating_add(1);
                let message = error
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("codex reported an error");
                let code = error
                    .get("codex_error_info")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let at = line
                    .get("timestamp")
                    .and_then(|v| v.as_str())
                    .and_then(parse_rfc3339_ms)
                    .unwrap_or_default();
                let id = payload
                    .get("turn_id")
                    .and_then(|v| v.as_str())
                    .map(|turn| format!("codex-{turn}"))
                    .unwrap_or_else(|| format!("codex-{at}"));
                cut = Some(Cut {
                    id,
                    kind: classify(&format!("{code} {message}")),
                    message: truncate(message),
                    at_ms: at,
                    resets_at_ms: None,
                    tries,
                });
            }
            (Some("response_item"), _) => {
                if payload.get("role").and_then(|v| v.as_str()) == Some("assistant") {
                    cut = None;
                }
            }
            (Some("event_msg"), "user_message") => {
                let words = payload
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
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
    use crate::records::{ErrorKind, NUDGE_MARK, Verdict};

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
    fn the_latest_error_wins() {
        let auth = r#"{"timestamp":"2026-10-27T06:00:00Z","type":"event_msg","payload":{"type":"error","message":"401 invalid api key"}}"#;
        let (_dir, path) = fixture("rollout.jsonl", &[LIMIT_ERROR, auth]);
        let scan = scan_codex(&path, 1_792_972_800_000).unwrap();
        let error = scan.last_error.unwrap();
        assert_eq!(error.kind, ErrorKind::Auth);
        assert_eq!(error.at_ms, Some(1_793_080_800_000));
    }

    /// One Codex turn that ended on `message` with code `code`.
    fn cut_line(turn: &str, code: &str, message: &str) -> String {
        format!(
            r#"{{"timestamp":"2026-10-27T06:30:00Z","type":"event_msg","payload":{{"type":"task_complete","turn_id":"{turn}","error":{{"message":"{message}","codex_error_info":"{code}"}}}}}}"#
        )
    }

    const ANSWER: &str = r#"{"timestamp":"2026-10-27T06:40:00Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"carrying on"}]}}"#;
    const NEW_TURN: &str = r#"{"timestamp":"2026-10-27T06:41:00Z","type":"event_msg","payload":{"type":"task_started","turn_id":"t9"}}"#;

    #[test]
    fn a_cut_is_classified_per_error_class() {
        let cases = [
            (
                "t1",
                "rate_limit",
                "rate limit reached",
                ErrorKind::RateLimit,
                Verdict::Passing,
                "Rate limited",
            ),
            (
                "t2",
                "usage_limit_exceeded",
                "usage limit reached",
                ErrorKind::Quota,
                Verdict::Lasting,
                "Out of allowance",
            ),
            (
                "t3",
                "unauthorized",
                "401 Unauthorized",
                ErrorKind::Auth,
                Verdict::Lasting,
                "Signed out",
            ),
            (
                "t4",
                "server_overloaded",
                "server overloaded",
                ErrorKind::Overloaded,
                Verdict::Passing,
                "Provider overloaded",
            ),
            (
                "t5",
                "invalid_request",
                "invalid request",
                ErrorKind::Other,
                Verdict::Lasting,
                "Provider error",
            ),
        ];
        for (turn, code, message, kind, verdict, reason) in cases {
            let line = cut_line(turn, code, message);
            let (_dir, path) = fixture("rollout.jsonl", &[&line]);
            let cut = cut_codex(&path).unwrap().expect("a cut");
            assert_eq!(cut.kind, kind, "{code}");
            assert_eq!(cut.verdict(), verdict, "{code}");
            assert_eq!(cut.reason(), reason, "{code}");
            assert_eq!(cut.id, format!("codex-{turn}"));
        }
    }

    #[test]
    fn a_later_answer_or_a_new_turn_ends_the_cut() {
        let error = cut_line("t1", "rate_limit", "rate limit reached");
        let (_dir, path) = fixture("rollout.jsonl", &[&error, ANSWER]);
        assert!(cut_codex(&path).unwrap().is_none());
        let (_dir, path) = fixture("rollout.jsonl", &[&error, NEW_TURN]);
        assert!(cut_codex(&path).unwrap().is_none());
    }

    #[test]
    fn tries_count_cut_turns_since_a_person_wrote() {
        let first = cut_line("t1", "server_overloaded", "server overloaded");
        let second = cut_line("t2", "server_overloaded", "server overloaded");
        let nudge = format!(
            r#"{{"timestamp":"2026-10-27T06:31:00Z","type":"event_msg","payload":{{"type":"user_message","message":"Carry on: {NUDGE_MARK} (provider overloaded)"}}}}"#
        );
        let (_dir, path) = fixture("rollout.jsonl", &[&first, &nudge, &second]);
        assert_eq!(cut_codex(&path).unwrap().unwrap().tries, 2);

        let person = r#"{"timestamp":"2026-10-27T06:32:00Z","type":"event_msg","payload":{"type":"user_message","message":"try again"}}"#;
        let third = cut_line("t3", "server_overloaded", "server overloaded");
        let (_dir, path) = fixture("rollout.jsonl", &[&first, &nudge, &second, person, &third]);
        assert_eq!(cut_codex(&path).unwrap().unwrap().tries, 1);
    }

    #[test]
    fn classifies_an_error_only_payload() {
        let auth = r#"{"timestamp":"2026-10-27T05:30:00Z","type":"event_msg","payload":{"type":"error","message":"401 Unauthorized"}}"#;
        let (_dir, path) = fixture("rollout.jsonl", &[auth]);
        let scan = scan_codex(&path, 0).unwrap();
        assert_eq!(scan.last_error.unwrap().kind, ErrorKind::Auth);
    }
}
