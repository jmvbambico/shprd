//! Millisecond-to-UTC clock formatting.
//!
//! The views render timestamps from the log without pulling in a date crate:
//! every deadline is an epoch millisecond count, and the only rendering the
//! views need is `HH:MM` in UTC.

/// Seconds in one day.
const SECS_PER_DAY: u64 = 86_400;

/// Renders `ms` since the Unix epoch as `HH:MM` in UTC.
pub fn hhmm(ms: u64) -> String {
    let secs_of_day = (ms / 1000) % SECS_PER_DAY;
    let hours = secs_of_day / 3600;
    let minutes = (secs_of_day % 3600) / 60;
    format!("{hours:02}:{minutes:02}")
}

/// Renders a cooldown deadline as `HH:MM`, or `unknown` when there is none.
pub fn until(ms: Option<u64>) -> String {
    match ms {
        Some(ms) => hhmm(ms),
        None => "unknown".to_string(),
    }
}

/// Shortens `text` to at most `max` characters, marking the cut with `…`.
pub fn clip(text: &str, max: usize) -> String {
    let mut out: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        out.push('…');
    }
    out
}
