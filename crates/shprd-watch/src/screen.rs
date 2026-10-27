//! Screen signals: which dry pattern a pane shows, what reset hint it carries,
//! and whether the screen is still changing.
//!
//! Everything here reads through [`shprd_mux::Mux`], so the same code runs
//! against herdr, tuios and [`shprd_mux::fake::FakeMux`].

/// How many trailing screen lines a signal looks at. Anything further up has
/// scrolled out of the agent's current turn.
pub const WINDOW_LINES: usize = 40;

/// How much of the text after a reset marker is worth showing a human.
const HINT_CHARS: usize = 60;

/// Markers that introduce the wait time in a rate-limit or quota message.
const RESET_MARKERS: [&str; 3] = ["resets ", "reset at ", "try again in "];

/// The last `count` lines of `screen`, oldest first.
fn tail_lines(screen: &str, count: usize) -> Vec<&str> {
    let lines: Vec<&str> = screen.lines().collect();
    lines[lines.len().saturating_sub(count)..].to_vec()
}

/// Whether a line echoes input shprd or the user typed, which must never trip
/// a dry pattern or a reset hint.
fn is_echo(line: &str) -> bool {
    line.starts_with("$ ") || line.starts_with("> ")
}

/// The first `max` characters of `text`, cut on a character boundary.
fn first_chars(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// The entry of `patterns` that the screen shows, ASCII case-insensitively as
/// a plain substring of one of the last [`WINDOW_LINES`] lines.
///
/// Echo lines (`$ ` and `> `) are skipped, so a prompt shprd typed that
/// happens to quote "rate limit" does not look like a real limit message.
/// Returns the pattern as configured, so the caller can name the rule that
/// fired.
pub fn match_dry(screen: &str, patterns: &[String]) -> Option<String> {
    for line in tail_lines(screen, WINDOW_LINES) {
        if is_echo(line) {
            continue;
        }
        let lowered = line.to_ascii_lowercase();
        for pattern in patterns {
            if !pattern.is_empty() && lowered.contains(&pattern.to_ascii_lowercase()) {
                return Some(pattern.clone());
            }
        }
    }
    None
}

/// What the screen says about when the agent will be allowed again: the text
/// after a "resets ", "reset at " or "try again in " marker inside the last
/// [`WINDOW_LINES`] lines, trimmed and cut to [`HINT_CHARS`] characters.
///
/// Display only. It is host prose, never a time we parse or schedule against.
pub fn reset_hint(screen: &str) -> Option<String> {
    for line in tail_lines(screen, WINDOW_LINES) {
        if is_echo(line) {
            continue;
        }
        let lowered = line.to_ascii_lowercase();
        for marker in RESET_MARKERS {
            let Some(at) = lowered.find(marker) else {
                continue;
            };
            let rest = line.get(at + marker.len()..).unwrap_or_default();
            let hint = first_chars(rest.trim(), HINT_CHARS);
            if !hint.is_empty() {
                return Some(hint);
            }
        }
    }
    None
}
