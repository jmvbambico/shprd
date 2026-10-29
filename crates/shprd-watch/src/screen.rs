//! Screen signals: which dry pattern a pane shows, what reset hint it carries,
//! and whether the screen is still changing.
//!
//! Everything here reads through [`shprd_mux::Mux`], so the same code runs
//! against herdr, tuios and [`shprd_mux::fake::FakeMux`].

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use shprd_mux::{AgentState, Mux, MuxError, PaneId};

/// How many trailing screen lines a signal looks at. Anything further up has
/// scrolled out of the agent's current turn.
pub const WINDOW_LINES: usize = 40;

/// How many trailing non-blank lines a dry pattern may match.
///
/// Far fewer than [`WINDOW_LINES`], because the two answer different
/// questions. A reset hint is shown to a human and reads best from the whole
/// window; a dry verdict hands work to another agent, so it must not be
/// reached from a screen the agent is merely reading. A CLI that hits its
/// limit prints its own error and status line at the bottom of the pane, so
/// only that bottom is a verdict. Code, diffs and tool output higher up are
/// content, and content quotes words like "quota" all the time.
pub const VERDICT_LINES: usize = 6;

/// How much of the text after a reset marker is worth showing a human.
const HINT_CHARS: usize = 60;

/// Markers that introduce the wait time in a rate-limit or quota message.
const RESET_MARKERS: [&str; 3] = ["resets ", "reset at ", "try again in "];

/// The last `count` lines of `screen`, oldest first.
fn tail_lines(screen: &str, count: usize) -> Vec<&str> {
    let lines: Vec<&str> = screen.lines().collect();
    lines[lines.len().saturating_sub(count)..].to_vec()
}

/// The last `count` non-blank lines of `screen`, oldest first.
///
/// Blank lines are dropped before counting, so a pane padded with empty rows
/// still reads as the few lines of text it actually shows.
fn tail_non_blank(screen: &str, count: usize) -> Vec<&str> {
    let lines: Vec<&str> = screen
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
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
/// a plain substring of one of the last [`VERDICT_LINES`] non-blank lines.
///
/// The window is the bottom of the pane and nothing above it: a CLI that runs
/// out of quota or hits a rate limit stops there, so its own error line is
/// among the last the pane shows. Scrollback above it is what the agent read
/// last, and a file, a diff or a tool output quoting "quota" or "429" is
/// content, not a verdict — a strict screen pattern must not decide one.
///
/// Echo lines (`$ ` and `> `) are skipped, so a prompt shprd typed that
/// happens to quote "rate limit" does not look like a real limit message.
/// Returns the pattern as configured, so the caller can name the rule that
/// fired.
pub fn match_dry(screen: &str, patterns: &[String]) -> Option<String> {
    for line in tail_non_blank(screen, VERDICT_LINES) {
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

/// The screen with trailing whitespace trimmed on each line and trailing blank
/// lines dropped, so a blinking cursor or a resize's padding is not a change.
fn normalized(screen: &str) -> String {
    let mut lines: Vec<&str> = screen.lines().map(str::trim_end).collect();
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

/// A hash of [`normalized`]`(screen)`: equal screens hash equal.
fn screen_hash(screen: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    normalized(screen).hash(&mut hasher);
    hasher.finish()
}

/// Whether a pane's screen is still moving, and how long it has not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Liveness {
    /// The screen differs from the last observation. The first observation
    /// counts as a change: it is what establishes the baseline.
    pub changed: bool,
    /// Milliseconds the screen has been byte-identical (after [`normalized`])
    /// to the last observation; zero when it just changed.
    pub unchanged_for_ms: u64,
}

/// Remembers one pane's last screen so a caller can tell "still working" from
/// "stuck on the same output".
///
/// Only a hash is kept, so holding a sampler costs nothing however large the
/// screen is, and a screen that goes back to an earlier state still counts as
/// a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Sampler {
    /// Hash of the last normalized screen, if any.
    last_hash: Option<u64>,
    /// When the current screen first appeared.
    unchanged_since_ms: Option<u64>,
}

impl Sampler {
    /// A sampler that has seen nothing yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold one observation of `screen` taken at `now_ms` into the sampler and
    /// report what changed.
    pub fn observe(&mut self, screen: &str, now_ms: u64) -> Liveness {
        let hash = screen_hash(screen);
        match self.last_hash {
            Some(previous) if previous == hash => {
                let since = *self.unchanged_since_ms.get_or_insert(now_ms);
                Liveness {
                    changed: false,
                    unchanged_for_ms: now_ms.saturating_sub(since),
                }
            }
            _ => {
                self.last_hash = Some(hash);
                self.unchanged_since_ms = Some(now_ms);
                Liveness {
                    changed: true,
                    unchanged_for_ms: 0,
                }
            }
        }
    }
}

/// Everything the watchdog knows about one pane at one moment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenSignal {
    /// The configured dry pattern the screen matched, if any.
    pub dry_pattern: Option<String>,
    /// The text after a reset marker, for display only.
    pub reset_hint: Option<String>,
    /// Whether the screen is still changing.
    pub liveness: Liveness,
    /// What the host reports the agent is doing.
    pub agent_state: AgentState,
}

/// Read `pane` through `mux` and turn its screen into one [`ScreenSignal`].
///
/// Reads the last [`WINDOW_LINES`] lines and the host's agent state, then folds
/// the screen into `sampler`. The caller owns the sampler so the liveness
/// history survives between polls of the same pane.
pub fn sample(
    mux: &dyn Mux,
    pane: &PaneId,
    patterns: &[String],
    sampler: &mut Sampler,
    now_ms: u64,
) -> Result<ScreenSignal, MuxError> {
    let screen = mux.read(pane, WINDOW_LINES)?;
    let agent_state = mux.agent_state(pane)?;
    Ok(ScreenSignal {
        dry_pattern: match_dry(&screen, patterns),
        reset_hint: reset_hint(&screen),
        liveness: sampler.observe(&screen, now_ms),
        agent_state,
    })
}

#[cfg(test)]
mod tests {
    use super::{Sampler, VERDICT_LINES, match_dry, reset_hint, sample};
    use shprd_mux::fake::FakeMux;
    use shprd_mux::{AgentState, PaneId};
    use std::fmt::Display;

    /// Unwrap like a test wants to, without tripping the `unwrap_used` lint.
    fn ok<T, E: Display>(result: Result<T, E>) -> T {
        match result {
            Ok(value) => value,
            Err(error) => panic!("expected Ok, got: {error}"),
        }
    }

    /// The patterns a roster spec would carry for a Claude Code harness.
    fn patterns() -> Vec<String> {
        vec!["rate limit".to_string(), "out of quota".to_string()]
    }

    #[test]
    fn patterns_match_case_insensitively() {
        assert_eq!(
            match_dry("Claude\nAPI error: RATE LIMIT reached", &patterns()),
            Some("rate limit".to_string())
        );
        assert_eq!(
            match_dry("out of QUOTA", &patterns()),
            Some("out of quota".to_string())
        );
        assert_eq!(match_dry("still working", &patterns()), None);
    }

    /// The patterns `data/harness/pi.toml` ships, so the cases below are the
    /// ones the roster really configures.
    fn pi_patterns() -> Vec<String> {
        [
            "429 Too Many Requests",
            "rate limit exceeded",
            "rate limit reached",
            "insufficient credits",
            "credit balance is too low",
            "usage limit reached",
        ]
        .iter()
        .map(|pattern| (*pattern).to_owned())
        .collect()
    }

    #[test]
    fn only_the_bottom_of_the_pane_is_a_verdict() {
        let mut escaped = vec!["rate limit hit".to_string()];
        escaped.extend((0..VERDICT_LINES).map(|_| "working".to_string()));
        assert_eq!(match_dry(&escaped.join("\n"), &patterns()), None);

        let mut inside = vec!["rate limit hit".to_string()];
        inside.extend((0..VERDICT_LINES - 1).map(|_| "working".to_string()));
        assert_eq!(
            match_dry(&inside.join("\n"), &patterns()),
            Some("rate limit".to_string())
        );
    }

    #[test]
    fn blank_padding_does_not_push_the_verdict_out_of_the_window() {
        let mut lines = vec![
            "pi".to_string(),
            "API error: usage limit reached".to_string(),
        ];
        lines.extend((0..VERDICT_LINES).map(|_| String::new()));
        assert_eq!(
            match_dry(&lines.join("\n"), &pi_patterns()),
            Some("usage limit reached".to_string())
        );
    }

    /// The false positive that made the window strict: a working agent showing
    /// its own source, a diff or a tool's output. shprd's source holds every
    /// one of these words, and none of them is a verdict.
    #[test]
    fn a_working_agent_reading_content_is_not_dry() {
        let screen = [
            "-let quota = load()?;",
            "-assert_eq!(response.status(), 429);",
            "-// a rate limit here is retried by the caller",
            "+fn dry_patterns() -> Vec<String> {",
        ];
        assert_eq!(match_dry(&screen.join("\n"), &pi_patterns()), None);
    }

    #[test]
    fn a_pi_quota_error_on_the_last_line_is_a_verdict() {
        let screen = "pi\nworking on the task\nError: credit balance is too low";
        assert_eq!(
            match_dry(screen, &pi_patterns()),
            Some("credit balance is too low".to_string())
        );
    }

    #[test]
    fn a_quota_phrase_scrolled_out_of_the_window_is_not_a_verdict() {
        let mut lines = vec!["Error: 429 Too Many Requests".to_string()];
        lines.extend((0..VERDICT_LINES).map(|_| "reading src/main.rs".to_string()));
        assert_eq!(match_dry(&lines.join("\n"), &pi_patterns()), None);
    }

    #[test]
    fn echoed_input_lines_are_ignored() {
        let screen = "$ grep -i rate limit notes.md\n> rate limit\nstill working";
        assert_eq!(match_dry(screen, &patterns()), None);
        assert_eq!(reset_hint("$ resets 3pm\n> try again in 4 hours"), None);
    }

    #[test]
    fn reset_hints_keep_the_text_after_the_marker() {
        assert_eq!(
            reset_hint("Limit reached · resets 3:00pm (America/Los_Angeles)"),
            Some("3:00pm (America/Los_Angeles)".to_string())
        );
        assert_eq!(
            reset_hint("quota spent, try again in 4 hours"),
            Some("4 hours".to_string())
        );
        assert_eq!(
            reset_hint("reset at midnight"),
            Some("midnight".to_string())
        );
        assert_eq!(reset_hint("all clear"), None);
        let long = format!("resets {}", "9".repeat(80));
        assert_eq!(reset_hint(&long).map(|hint| hint.len()), Some(60));
    }

    #[test]
    fn unchanged_screen_accumulates_and_padding_does_not_count() {
        let mut sampler = Sampler::new();
        let first = sampler.observe("working", 1_000);
        assert!(first.changed);
        assert_eq!(first.unchanged_for_ms, 0);

        // Trailing spaces and trailing blank lines are resize and cursor noise.
        let padded = sampler.observe("working   \n\n", 3_000);
        assert!(!padded.changed);
        assert_eq!(padded.unchanged_for_ms, 2_000);

        let same = sampler.observe("working", 7_500);
        assert!(!same.changed);
        assert_eq!(same.unchanged_for_ms, 6_500);

        let moved = sampler.observe("done", 8_000);
        assert!(moved.changed);
        assert_eq!(moved.unchanged_for_ms, 0);
    }

    #[test]
    fn fake_mux_pane_yields_the_expected_signal() {
        let mux = FakeMux::default();
        let pane = PaneId::new("pane-1");
        mux.set_screen(
            &pane,
            "Claude\nAPI error: rate limit reached · resets 5pm\n",
        );
        mux.set_state(&pane, AgentState::Blocked);

        let mut sampler = Sampler::new();
        let signal = ok(sample(&mux, &pane, &patterns(), &mut sampler, 1_000));
        assert_eq!(signal.dry_pattern, Some("rate limit".to_string()));
        assert_eq!(signal.reset_hint, Some("5pm".to_string()));
        assert!(signal.liveness.changed);
        assert_eq!(signal.agent_state, AgentState::Blocked);

        let again = ok(sample(&mux, &pane, &patterns(), &mut sampler, 4_500));
        assert_eq!(again.dry_pattern, Some("rate limit".to_string()));
        assert!(!again.liveness.changed);
        assert_eq!(again.liveness.unchanged_for_ms, 3_500);
    }
}
