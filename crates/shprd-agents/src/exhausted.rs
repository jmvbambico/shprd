//! Whether a CLI's own error says its account is spent, not that the harness broke.
//!
//! The live-launch tests start each real CLI once and check the brief came back.
//! A program whose account is out of usage answers that launch with its own
//! allowance message and no reply, which is the account's state rather than
//! anything shprd did: the test says `skipped: <program> out of quota` and
//! returns instead of failing, so a milestone gate can be green on a machine
//! whose accounts are not.
//!
//! Two sources of evidence are read, in order:
//!
//! 1. the program's own [`HarnessSpec::dry_patterns`], which the watchdog uses to
//!    recognise a pane that has run out — the harness is the one place per
//!    program that says how it words the limit;
//! 2. a small table of allowance and rate-limit phrases below, for the wording a
//!    program only ever says in an *error*, never on a screen the watchdog
//!    reads. A screen is mostly the content the agent is reading, so a short
//!    phrase there is unsafe (`dry_patterns` are whole phrases for that reason);
//!    a line the CLI itself wrote to stderr is not.
//!
//! The table mirrors the groups [`shprd_watch::records`] classifies provider
//! errors with, so a message shprd would treat as an exhausted allowance in a
//! worker's log is the same message a live test skips on.
//!
//! [`HarnessSpec::dry_patterns`]: crate::spec::HarnessSpec::dry_patterns

/// Allowance and rate-limit phrases a CLI prints when its account cannot answer.
///
/// These are substrings, matched ASCII case-insensitively, and they are short on
/// purpose: the text they are read from is the program's own error output, not a
/// pane of whatever the agent was reading.
const EXHAUSTED: &[&str] = &[
    "usage limit",
    "rate limit",
    "rate_limit",
    "too many requests",
    "insufficient",
    "insufficient_quota",
    "quota",
    "credit",
    "billing",
    "payment",
    "hit your limit",
    "limit reached",
    "weekly limit",
    "monthly limit",
    "out of allowance",
    "429",
];

/// The phrase saying this output is an exhausted account, if any.
///
/// `patterns` is the program's `dry_patterns`, tried first so a harness whose
/// wording the table misses can still say how it is worded. The answer is the
/// phrase that matched, which a caller prints: naming the evidence keeps
/// "skipped" from reading like "the test did not run".
pub fn exhausted(patterns: &[String], text: &str) -> Option<String> {
    let lowered = text.to_ascii_lowercase();
    let hit = patterns
        .iter()
        .map(String::as_str)
        .chain(EXHAUSTED.iter().copied())
        .find(|pattern| {
            let pattern = pattern.trim();
            !pattern.is_empty() && lowered.contains(&pattern.to_ascii_lowercase())
        });
    hit.map(|pattern| pattern.trim().to_owned())
}

/// When the account may answer again, read out of the CLI's own words, as ms
/// since the Unix epoch in this machine's time zone.
///
/// Codex says `try again at Dec 18th, 2026 6:50 AM`; the day alone, with no
/// time, reads as that day's start. Nothing printed, or a form this does not
/// know, is [`None`]: a caller then uses a bounded cooldown of its own.
pub fn reset_at_ms(text: &str) -> Option<u64> {
    use chrono::{Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};
    let lowered = text.to_ascii_lowercase();
    let at = lowered.rfind("try again at ")?;
    let rest = text.get(at + "try again at ".len()..)?;
    let line = rest.lines().next()?.trim().trim_end_matches('.');
    // Ordinal suffixes (`18th`) and the comma are not part of any format.
    let words: Vec<String> = line
        .replace(',', " ")
        .split_whitespace()
        .map(|word| {
            let digits: String = word.chars().take_while(char::is_ascii_digit).collect();
            let tail = &word[digits.len()..];
            if !digits.is_empty() && matches!(tail, "st" | "nd" | "rd" | "th") {
                digits
            } else {
                word.to_owned()
            }
        })
        .collect();
    let local = |naive: NaiveDateTime| {
        Local
            .from_local_datetime(&naive)
            .earliest()
            .map(|at| at.timestamp_millis())
            .and_then(|ms| u64::try_from(ms).ok())
    };
    for format in ["%b %d %Y %I:%M %p", "%b %d %Y %H:%M", "%B %d %Y %I:%M %p"] {
        // Cut the line to as many words as the format has, so a trailing
        // sentence does not spoil it.
        let count = format.split_whitespace().count();
        let head = words
            .iter()
            .take(count)
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");
        if let Ok(naive) = NaiveDateTime::parse_from_str(&head, format) {
            return local(naive);
        }
    }
    for format in ["%b %d %Y", "%B %d %Y"] {
        let head = words.iter().take(3).cloned().collect::<Vec<_>>().join(" ");
        if let Ok(day) = NaiveDate::parse_from_str(&head, format) {
            return local(day.and_time(NaiveTime::MIN));
        }
    }
    None
}

/// The day a reset time falls on, as a person says it: `Dec 18`.
pub fn day_of(ms: u64) -> Option<String> {
    use chrono::{Local, TimeZone};
    let at = Local
        .timestamp_millis_opt(i64::try_from(ms).ok()?)
        .single()?;
    Some(at.format("%b %-d").to_string())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    /// Codex's own wording, as the gate runs saw it: the phrase the harness's
    /// dry patterns do not spell out, and the reason this table exists.
    const CODEX_LIMIT: &str = "ERROR: You've hit your usage limit. Upgrade to Plus to continue \
                               using Codex, or try again at Dec 4th, 2026 6:50 AM.";

    #[test]
    fn a_spent_accounts_message_is_recognised() {
        let phrase = exhausted(&[], CODEX_LIMIT);
        assert_eq!(
            phrase.as_deref(),
            Some("usage limit"),
            "the usage-limit line is an exhausted account"
        );
    }

    #[test]
    fn a_harnesss_own_dry_pattern_also_counts() {
        let patterns = vec!["you have no tokens left".to_string()];
        assert_eq!(
            exhausted(&patterns, "sorry, you have no tokens left today"),
            Some("you have no tokens left".to_string())
        );
    }

    #[test]
    fn a_rate_limit_is_the_same_skip() {
        for text in [
            "429 Too Many Requests",
            "Error: rate_limit_exceeded",
            "you are rate limited, retry shortly",
        ] {
            assert!(exhausted(&[], text).is_some(), "{text:?} is a rate limit");
        }
    }

    #[test]
    fn an_ordinary_failure_is_not_an_exhausted_account() {
        for text in [
            "error: could not read the brief",
            "thread 'main' panicked at src/main.rs:12",
            "BANANA was not in the reply",
            "",
        ] {
            assert!(
                exhausted(&[], text).is_none(),
                "{text:?} must fail its test, not skip it"
            );
        }
    }

    #[test]
    fn an_empty_pattern_never_matches() {
        let patterns = vec![String::new(), "  ".to_string()];
        assert!(exhausted(&patterns, "anything at all").is_none());
    }

    #[test]
    fn the_match_ignores_case_and_keeps_the_pattern_as_written() {
        assert_eq!(
            exhausted(&["Usage Limit Reached".to_string()], "usage limit reached"),
            Some("Usage Limit Reached".to_string())
        );
    }

    #[test]
    fn codexs_reset_time_is_read_from_its_own_words() {
        let ms = reset_at_ms(CODEX_LIMIT).expect("a time");
        assert_eq!(day_of(ms).as_deref(), Some("Dec 4"));
        let ms = reset_at_ms("ERROR: You've hit your usage limit. try again at Dec 18th, 2026.")
            .expect("a day");
        assert_eq!(day_of(ms).as_deref(), Some("Dec 18"));
        assert_eq!(reset_at_ms("429 Too Many Requests"), None);
    }
}
