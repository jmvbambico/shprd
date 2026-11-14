//! Readers for each CLI's conversation record.
//!
//! Every supported CLI keeps a local record of what a session did: Claude,
//! Codex and pi write JSONL, opencode and kilo write SQLite. A reader scans
//! one record for the signals the watchdog cares about — the newest provider
//! error (rate limit, allowance, auth, overload) and, where the CLI records it,
//! how much of the current usage window is spent.
//!
//! One question needs the whole tail rather than the newest error: whether a
//! turn was *cut off* and is still the last thing the record says. A normal
//! answer, a new turn or a person's message after the error means the agent
//! carried on, so there is nothing to retry. [`Cut`] is that answer, and every
//! reader has a `cut_*` function that produces it.
//!
//! A reader never fails over a single unreadable line: malformed JSONL lines
//! are skipped, so a half-written tail cannot blind the watchdog.
//!
//! The classes and the cut reading are ported from Shepherdr's
//! `provider.py` (`provider_cut`, `provider_error`, commit `a3f327a`, MIT);
//! see `THIRD_PARTY_NOTICES.md`.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Datelike, TimeZone, Utc};

pub mod claude;
pub mod codex;
pub mod locate;
pub mod pi;
pub mod sqlite;

/// What kind of provider trouble an error record describes.
///
/// The five kinds collapse into the four classes the fallback ladder acts on:
/// [`ErrorKind::RateLimit`] and [`ErrorKind::Overloaded`] pass by themselves
/// (waiting is the fix), [`ErrorKind::Quota`] and [`ErrorKind::Auth`] need a
/// different program or the person, and [`ErrorKind::Other`] is neither proven
/// passing nor one of the two reasons a backup exists for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// The provider asked us to slow down (429, rate limit, too many requests).
    RateLimit,
    /// The account is out of allowance: usage limit, quota, credit or billing.
    Quota,
    /// The credentials were rejected (401/403, logged out, invalid API key).
    Auth,
    /// The provider is down or overloaded (529, 5xx, timeouts, dropped streams).
    Overloaded,
    /// Anything else the reader could not classify.
    Other,
}

/// Whether an error passes by itself, as Shepherdr's `provider_error` decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Waiting and trying again is the fix: a rate limit or an overloaded
    /// provider.
    Passing,
    /// Waiting cannot fix it: the allowance, the login or the request itself.
    Lasting,
}

impl ErrorKind {
    /// Whether waiting can fix this kind of trouble.
    pub fn verdict(self) -> Verdict {
        match self {
            Self::RateLimit | Self::Overloaded => Verdict::Passing,
            Self::Quota | Self::Auth | Self::Other => Verdict::Lasting,
        }
    }

    /// The few words a status line uses, as Shepherdr's provider reasons.
    ///
    /// Ported from Shepherdr's `provider_error` (`provider.py:35-55`); the
    /// allowance reason there also names a reset time when the text carries
    /// one, which [`Cut::resets_at_ms`] holds separately here.
    pub fn reason(self) -> &'static str {
        match self {
            Self::RateLimit => "Rate limited",
            Self::Quota => "Out of allowance",
            Self::Auth => "Signed out",
            Self::Overloaded => "Provider overloaded",
            Self::Other => "Provider error",
        }
    }
}

/// A turn the model's provider cut off, as the CLI's own record ends.
///
/// Shepherdr's `provider_cut` (`provider.py:64-102`) makes the same reading:
/// the error is the last thing the record says, and `tries` counts the cut
/// turns since anyone last wrote to the agent. Herdr's own carry-on messages do
/// not reset the count ([`NUDGE_MARK`]), because they are shprd talking to
/// itself; anything else a person writes starts a fresh count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cut {
    /// A stable identity for this cut, so a nudge recorded for it is not sent
    /// twice and a new cut starts the ladder over.
    pub id: String,
    /// The provider trouble it describes.
    pub kind: ErrorKind,
    /// The provider's own words, truncated to 200 characters.
    pub message: String,
    /// When the cut happened, in epoch milliseconds.
    pub at_ms: u64,
    /// When the provider said the limit lifts, in epoch milliseconds.
    pub resets_at_ms: Option<u64>,
    /// How many cut turns there have been since a person last wrote.
    pub tries: u32,
}

impl Cut {
    /// The few words the ladder uses for this cut.
    pub fn reason(&self) -> &'static str {
        self.kind.reason()
    }

    /// Whether waiting can fix it.
    pub fn verdict(&self) -> Verdict {
        self.kind.verdict()
    }
}

/// The words in shprd's own carry-on message, so a cut after one still counts
/// as another try (Shepherdr's `NUDGE_MARK`, `provider.py:59-61`).
pub const NUDGE_MARK: &str = "cut off by the model's provider";

/// The newest provider error found in a record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordError {
    /// The classified kind of the error.
    pub kind: ErrorKind,
    /// The human-readable message, truncated to its first 200 characters.
    pub message: String,
    /// When the error was recorded, in epoch milliseconds, if known.
    pub at_ms: Option<u64>,
    /// When the provider said the limit lifts, in epoch milliseconds.
    pub resets_at_ms: Option<u64>,
}

/// Usage of the current provider window, as recorded by the CLI.
#[derive(Debug, Clone, PartialEq)]
pub struct Usage {
    /// Percentage of the window already spent, `0..=100`.
    pub used_percent: f64,
    /// When the window resets, in epoch milliseconds.
    pub resets_at_ms: Option<u64>,
    /// The length of the window in minutes, when the record says.
    pub window_minutes: Option<u64>,
}

/// What a reader found in one conversation record.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Scan {
    /// The newest provider error at or after the `since_ms` cutoff.
    pub last_error: Option<RecordError>,
    /// The newest usage reading at or after the `since_ms` cutoff.
    pub usage: Option<Usage>,
    /// The newest activity timestamp in the record, in epoch milliseconds.
    pub last_activity_ms: Option<u64>,
}

/// Why a record could not be read at all.
#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    /// The record exists but could not be read from disk.
    #[error("cannot read record {path}: {source}")]
    Io {
        /// The record that could not be read.
        path: PathBuf,
        /// The underlying IO failure.
        #[source]
        source: std::io::Error,
    },
    /// The record is not in the shape its CLI writes.
    #[error("record {path} is unreadable: {reason}")]
    Storage {
        /// The record that could not be read.
        path: PathBuf,
        /// What went wrong.
        reason: String,
    },
}

impl ReadError {
    /// Build a [`ReadError::Storage`] for `path` from any message.
    pub(crate) fn storage(path: &Path, reason: impl std::fmt::Display) -> Self {
        Self::Storage {
            path: path.to_path_buf(),
            reason: reason.to_string(),
        }
    }
}

/// Authentication keywords: the login is gone, which waiting cannot fix.
const AUTH: &[&str] = &[
    "401",
    "403",
    "authentication",
    "invalid api key",
    "authorization",
    "unauthorized",
    "please run /login",
    "login expired",
    "not logged in",
    "logged out",
    "oauth token",
];
/// Rate-limit keywords: checked before the allowance group, because
/// "rate limit reached" contains "limit reached" too.
const RATE_LIMIT: &[&str] = &["rate limit", "rate_limit", "429", "too many requests"];
/// Allowance keywords: the account has nothing left until it resets.
const QUOTA: &[&str] = &[
    "usage limit",
    "weekly limit",
    "monthly limit",
    "limit reached",
    "hit your limit",
    "quota",
    "insufficient",
    "credit",
    "billing",
    "payment",
];
/// Provider-down and timeout keywords: waiting is the fix.
const OVERLOADED: &[&str] = &[
    "overloaded",
    "529",
    "at capacity",
    "timed out",
    "timeout",
    "connection",
    "network",
    "socket",
    "econn",
    "etimedout",
    "eai again",
    "epipe",
    "fetch failed",
    "stream ended",
    "stream closed",
    "stream disconnected",
    "stream error",
    "disconnect",
    "internal server error",
    "bad gateway",
    "service unavailable",
    "upstream",
    "server error",
    "try again later",
    "5xx",
];

/// Classify an error from its code and text.
///
/// Matching is case-insensitive and ignores underscores; the groups are tried
/// in the order auth, allowance, rate limit, provider down, else
/// [`ErrorKind::Other`] — Shepherdr's order in `provider_error`
/// (`provider.py:35-55`), where an exhausted allowance outranks a passing rate
/// limit. The one collision is handled explicitly: "rate limit reached" also
/// contains the allowance words "limit reached", so that phrase only counts
/// when it is not preceded by "rate ".
pub fn classify(text: &str) -> ErrorKind {
    let normalised = text.replace('_', " ").to_lowercase();
    let hit = |needles: &[&str]| needles.iter().any(|n| normalised.contains(n));
    if hit(AUTH) {
        ErrorKind::Auth
    } else if hit_allowance(&normalised) {
        ErrorKind::Quota
    } else if hit(RATE_LIMIT) {
        ErrorKind::RateLimit
    } else if hit(OVERLOADED) {
        ErrorKind::Overloaded
    } else {
        ErrorKind::Other
    }
}

/// Whether `text` names an exhausted allowance rather than a passing rate
/// limit. "limit reached" is an allowance word unless the word before it is
/// "rate", which is what separates a 429 from a spent weekly quota.
fn hit_allowance(text: &str) -> bool {
    QUOTA.iter().any(|needle| {
        let mut from = 0;
        while let Some(offset) = text[from..].find(needle) {
            let at = from + offset;
            let rate_prefixed = *needle == "limit reached" && text[..at].ends_with("rate ");
            if !rate_prefixed {
                return true;
            }
            from = at + needle.len();
        }
        false
    })
}

/// Keep the first 200 characters of `text`, on a character boundary.
pub(crate) fn truncate(text: &str) -> String {
    text.chars().take(200).collect()
}

/// The conversation so far from the record `program` wrote for `cwd`, latest
/// last, as Shepherdr's `chat_tail` builds it (`takeover.py:150-178`).
///
/// The first message is the brief the agent started with, so it is left out:
/// a backup gets its own brief instead of the outgoing agent's. Each line is
/// capped, only the last twelve are kept, and the whole tail is capped again,
/// so a pasted file cannot become the handover.
///
/// `None` when the record cannot be found or its format has no text to read
/// (the SQLite readers keep no message text in the row this module reads).
pub fn chat_tail(program: &str, home: &Path, cwd: &Path) -> Option<String> {
    let record = locate::newest_record(program, home, cwd)?;
    let mut said: Vec<(bool, String)> = Vec::new();
    match program {
        "claude" | "codex" | "pi" => {
            each_json_line(&record, |line| {
                if let Some((person, words)) = conversation_line(program, line)
                    && !words.trim().is_empty()
                {
                    said.push((person, words));
                }
            })
            .ok()?;
        }
        _ => return None,
    }
    // The brief is the first thing a person (or the engine) wrote.
    if let Some(first) = said.iter().position(|(person, _)| *person) {
        said.remove(first);
    }
    let tail: Vec<String> = said
        .iter()
        .map(|(person, words)| {
            format!(
                "{}: {}",
                if *person { "person" } else { "agent" },
                words.chars().take(2000).collect::<String>().trim()
            )
        })
        .collect();
    let start = tail.len().saturating_sub(12);
    let mut text = tail[start..].join("\n\n");
    while text.chars().count() > 12_000 {
        match text.split_once("\n\n") {
            Some((_, rest)) => text = rest.to_string(),
            None => {
                let skip = text.chars().count() - 12_000;
                text = text.chars().skip(skip).collect();
            }
        }
    }
    Some(text)
}

/// Whether a record line is a person speaking, and its words, for the CLI
/// formats whose text lives in JSONL.
fn conversation_line(program: &str, line: &serde_json::Value) -> Option<(bool, String)> {
    match program {
        "claude" => {
            if line.get("isSidechain").and_then(|v| v.as_bool()) == Some(true) {
                return None;
            }
            match line.get("type").and_then(|v| v.as_str()) {
                Some("assistant")
                    if line.get("isApiErrorMessage").and_then(|v| v.as_bool()) != Some(true) =>
                {
                    Some((false, message_words(line.get("message"))))
                }
                Some("user") => {
                    let content = line.get("message").and_then(|m| m.get("content"))?;
                    if content.as_array().is_some_and(|parts| {
                        parts.iter().any(|part| {
                            part.get("type").and_then(|t| t.as_str()) == Some("tool_result")
                        })
                    }) {
                        return None;
                    }
                    Some((true, value_words(content)))
                }
                _ => None,
            }
        }
        "codex" => {
            let payload = line.get("payload")?;
            match (
                line.get("type").and_then(|v| v.as_str()),
                payload.get("type").and_then(|v| v.as_str()),
            ) {
                (Some("response_item"), _)
                    if payload.get("role").and_then(|v| v.as_str()) == Some("assistant") =>
                {
                    Some((false, value_words(payload.get("content")?)))
                }
                (Some("event_msg"), Some("user_message")) => Some((
                    true,
                    payload
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                )),
                _ => None,
            }
        }
        "pi" => {
            let message = line.get("message")?;
            match message.get("role").and_then(|v| v.as_str()) {
                Some("assistant") => Some((false, value_words(message.get("content")?))),
                Some("user") => Some((true, value_words(message.get("content")?))),
                _ => None,
            }
        }
        _ => None,
    }
}

/// The text of a message object's `content`.
fn message_words(message: Option<&serde_json::Value>) -> String {
    message
        .and_then(|message| message.get("content"))
        .map(value_words)
        .unwrap_or_default()
}

/// The words of a content value, whether it is a string or a list of parts.
fn value_words(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part.get("text").and_then(|v| v.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Parse an RFC 3339 timestamp into epoch milliseconds.
///
/// `None` for anything unparseable, so one odd timestamp cannot abort a scan.
pub(crate) fn parse_rfc3339_ms(text: &str) -> Option<u64> {
    let parsed = DateTime::parse_from_rfc3339(text).ok()?;
    u64::try_from(parsed.timestamp_millis()).ok()
}

/// Convert epoch seconds, as some records write them, into milliseconds.
pub(crate) fn secs_to_ms(secs: i64) -> Option<u64> {
    u64::try_from(secs).ok().map(|s| s.saturating_mul(1000))
}

/// Run `f` over every JSON line of `path`, skipping blank and malformed lines.
pub(crate) fn each_json_line(
    path: &Path,
    mut f: impl FnMut(&serde_json::Value),
) -> Result<(), ReadError> {
    let bytes = std::fs::read(path).map_err(|source| ReadError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    for line in bytes.split(|b| *b == b'\n') {
        let Ok(text) = std::str::from_utf8(line) else {
            continue;
        };
        if text.trim().is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(text) {
            f(&value);
        }
    }
    Ok(())
}

/// Resolve a wall-clock time in the IANA zone `tz` to epoch milliseconds.
///
/// An ambiguous local time (the hour a DST change repeats) resolves to its
/// earliest occurrence; a time that never happens resolves to `None`.
pub(crate) fn zoned_ms(
    tz: &str,
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
) -> Option<u64> {
    let zone: chrono_tz::Tz = tz.parse().ok()?;
    let naive = chrono::NaiveDate::from_ymd_opt(year, month, day)?.and_hms_opt(hour, minute, 0)?;
    let local = zone.from_local_datetime(&naive).earliest()?;
    u64::try_from(local.timestamp_millis()).ok()
}

/// The UTC year of an epoch-millisecond timestamp, for records that omit it.
pub(crate) fn utc_year(at_ms: Option<u64>) -> Option<i32> {
    let at = i64::try_from(at_ms?).ok()?;
    Some(DateTime::<Utc>::from_timestamp_millis(at)?.year())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn classifies_rate_limit_words() {
        assert_eq!(classify("rate_limit"), ErrorKind::RateLimit);
        assert_eq!(classify("Error 429: slow down"), ErrorKind::RateLimit);
        assert_eq!(classify("rate limit reached"), ErrorKind::RateLimit);
        assert_eq!(classify("too many requests"), ErrorKind::RateLimit);
    }

    #[test]
    fn an_allowance_is_not_a_rate_limit() {
        assert_eq!(classify("usage_limit_reached"), ErrorKind::Quota);
        assert_eq!(classify("You've hit your weekly limit"), ErrorKind::Quota);
        assert_eq!(classify("insufficient credit"), ErrorKind::Quota);
        assert_eq!(classify("billing problem"), ErrorKind::Quota);
        assert_eq!(classify("credit balance too low"), ErrorKind::Quota);
    }

    #[test]
    fn classifies_auth_overload_and_other() {
        assert_eq!(classify("authentication_failed"), ErrorKind::Auth);
        assert_eq!(classify("401: bad token"), ErrorKind::Auth);
        assert_eq!(classify("you are not logged in"), ErrorKind::Auth);
        assert_eq!(classify("Overloaded"), ErrorKind::Overloaded);
        assert_eq!(classify("Request timed out."), ErrorKind::Overloaded);
        assert_eq!(classify("connection reset by peer"), ErrorKind::Overloaded);
        assert_eq!(classify("invalid_request"), ErrorKind::Other);
    }

    #[test]
    fn a_chat_tail_skips_the_brief_and_keeps_the_conversation() {
        let home = tempfile::tempdir().unwrap();
        let cwd = Path::new("/tmp/worktree");
        let slug = cwd.to_string_lossy().replace(['/', '.'], "-");
        let dir = home.path().join(".claude").join("projects").join(slug);
        std::fs::create_dir_all(&dir).unwrap();
        let lines = [
            r#"{"timestamp":"2026-10-27T06:00:00Z","type":"user","message":{"role":"user","content":[{"type":"text","text":"the brief"}]}}"#,
            r#"{"timestamp":"2026-10-27T06:01:00Z","type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"I am halfway"}]}}"#,
            r#"{"timestamp":"2026-10-27T06:02:00Z","type":"user","message":{"role":"user","content":[{"type":"text","text":"carry on"}]}}"#,
        ]
        .join("\n");
        std::fs::write(dir.join("s.jsonl"), lines).unwrap();
        let tail = chat_tail("claude", home.path(), cwd).expect("a tail");
        assert!(!tail.contains("the brief"), "{tail}");
        assert!(tail.contains("agent: I am halfway"), "{tail}");
        assert!(tail.contains("person: carry on"), "{tail}");
    }

    #[test]
    fn the_verdict_and_reason_of_each_class() {
        for (kind, verdict, reason) in [
            (ErrorKind::RateLimit, Verdict::Passing, "Rate limited"),
            (
                ErrorKind::Overloaded,
                Verdict::Passing,
                "Provider overloaded",
            ),
            (ErrorKind::Quota, Verdict::Lasting, "Out of allowance"),
            (ErrorKind::Auth, Verdict::Lasting, "Signed out"),
            (ErrorKind::Other, Verdict::Lasting, "Provider error"),
        ] {
            assert_eq!(kind.verdict(), verdict, "{kind:?}");
            assert_eq!(kind.reason(), reason, "{kind:?}");
        }
    }

    #[test]
    fn truncates_messages_to_two_hundred_chars() {
        let long = "é".repeat(500);
        assert_eq!(truncate(&long).chars().count(), 200);
        assert_eq!(truncate("short"), "short");
    }
}
