//! Readers for each CLI's conversation record.
//!
//! Every supported CLI keeps a local record of what a session did: Claude,
//! Codex and pi write JSONL, opencode and kilo write SQLite. A reader scans
//! one record for the signals the watchdog cares about — the newest provider
//! error (rate limit, quota, auth, overload) and, where the CLI records it,
//! how much of the current usage window is spent.
//!
//! A reader never fails over a single unreadable line: malformed JSONL lines
//! are skipped, so a half-written tail cannot blind the watchdog.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Datelike, TimeZone, Utc};

pub mod claude;
pub mod codex;

/// What kind of provider trouble an error record describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// The provider asked us to slow down (429, rate or usage limit).
    RateLimit,
    /// The account is out of credit, quota or billing headroom.
    Quota,
    /// The credentials were rejected (401, invalid API key).
    Auth,
    /// The provider is overloaded or the request timed out (529).
    Overloaded,
    /// Anything else the reader could not classify.
    Other,
}

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

/// Rate-limit keywords; checked before every other group.
const RATE_LIMIT: &[&str] = &[
    "rate limit",
    "429",
    "usage limit",
    "weekly limit",
    "monthly limit",
    "limit reached",
];
/// Quota keywords.
const QUOTA: &[&str] = &["quota", "insufficient", "credit", "billing"];
/// Authentication keywords.
const AUTH: &[&str] = &[
    "401",
    "authentication",
    "invalid api key",
    "authorization",
    "unauthorized",
];
/// Overload and timeout keywords.
const OVERLOADED: &[&str] = &["overloaded", "529", "timed out"];

/// Classify an error from its code and text.
///
/// Matching is case-insensitive and ignores underscores; the groups are tried
/// in the order rate limit, quota, auth, overloaded, else [`ErrorKind::Other`].
pub fn classify(text: &str) -> ErrorKind {
    let normalised = text.replace('_', " ").to_lowercase();
    let hit = |needles: &[&str]| needles.iter().any(|n| normalised.contains(n));
    if hit(RATE_LIMIT) {
        ErrorKind::RateLimit
    } else if hit(QUOTA) {
        ErrorKind::Quota
    } else if hit(AUTH) {
        ErrorKind::Auth
    } else if hit(OVERLOADED) {
        ErrorKind::Overloaded
    } else {
        ErrorKind::Other
    }
}

/// Keep the first 200 characters of `text`, on a character boundary.
pub(crate) fn truncate(text: &str) -> String {
    text.chars().take(200).collect()
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
    use super::*;

    #[test]
    fn classifies_rate_limit_words() {
        assert_eq!(classify("rate_limit"), ErrorKind::RateLimit);
        assert_eq!(classify("Error 429: slow down"), ErrorKind::RateLimit);
        assert_eq!(classify("usage_limit_reached"), ErrorKind::RateLimit);
        assert_eq!(
            classify("You've hit your weekly limit"),
            ErrorKind::RateLimit
        );
    }

    #[test]
    fn classifies_quota_auth_overload_and_other() {
        assert_eq!(classify("insufficient credit"), ErrorKind::Quota);
        assert_eq!(classify("billing problem"), ErrorKind::Quota);
        assert_eq!(classify("authentication_failed"), ErrorKind::Auth);
        assert_eq!(classify("401: bad token"), ErrorKind::Auth);
        assert_eq!(classify("Overloaded"), ErrorKind::Overloaded);
        assert_eq!(classify("Request timed out."), ErrorKind::Overloaded);
        assert_eq!(classify("invalid_request"), ErrorKind::Other);
    }

    #[test]
    fn truncates_messages_to_two_hundred_chars() {
        let long = "é".repeat(500);
        assert_eq!(truncate(&long).chars().count(), 200);
        assert_eq!(truncate("short"), "short");
    }
}
