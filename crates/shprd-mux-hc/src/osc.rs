//! The OSC 7501 status report: how a program tells its terminal what it is doing.
//!
//! herdr-core never guesses a status from the screen. A program prints an
//! escape sequence, the core parses it and the status becomes a fact other
//! processes can read. The grammar, byte for byte (herdr-core
//! `crates/herdr-core/src/status.rs:45-57`, recorded live in M7ef §2.5):
//!
//! ```text
//! ESC ] 7 5 0 1 ; state=<word>[:id=<id>][:kind=<kind>][:progress=<0-100>]
//!                  [:app=<app>][:title=<base64 utf8>][:msg=<base64 utf8>]
//! ST          (ST = ESC \ ; BEL also ends it)
//! ```
//!
//! Four details are load-bearing, and each has a fixture below:
//!
//! - Fields are separated by `:`, not `;`. `state=blocked;kind=question` is
//!   silently ignored by the core — it never becomes a status.
//! - `state=clear` removes the record named by `id` (or the root record) and
//!   that record's `id/` children; it is not a state.
//! - `id=` makes a child record, which reports on one thing a program is doing
//!   and does not move the turn counter.
//! - `title` and `msg` are base64 of UTF-8, and invalid UTF-8 inside them drops
//!   the whole report.
//!
//! shprd reads reports here (a wrapper, a test fixture, and later a shprd-owned
//! reporter may emit them), so the crate carries one implementation of the
//! grammar rather than a second, subtly different one.

use crate::model::{AgentState, AskKind};

/// The escape sequence that starts a report.
pub const PREFIX: &[u8] = b"\x1b]7501;";

/// One parsed report: either what a program says it is doing, or a clear.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// The status word, or a request to remove a record.
    pub word: ReportWord,
    /// Which child record, when the program reports on a part of its work.
    pub id: Option<String>,
    /// What a `blocked` program waits for.
    pub kind: Option<AskKind>,
    /// How far along a `working` program is, 0–100.
    pub progress: Option<u8>,
    /// The program's own name.
    pub app: Option<String>,
    /// A one-line title, decoded from base64.
    pub title: Option<String>,
    /// A longer message, decoded from base64.
    pub msg: Option<String>,
}

/// What a report asks for.
///
/// `clear` is a word programs send and a state no terminal is ever in — it
/// removes the record named by `id`, or the root record and its `id/` children
/// — so it gets its own arm instead of a second status word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportWord {
    /// A state to report.
    State(AgentState),
    /// Remove the record.
    Clear,
}

impl Report {
    /// Whether this report clears a record rather than setting one.
    pub fn is_clear(&self) -> bool {
        matches!(self.word, ReportWord::Clear)
    }

    /// The state this report sets, when it sets one.
    pub fn state(&self) -> Option<&AgentState> {
        match &self.word {
            ReportWord::State(state) => Some(state),
            ReportWord::Clear => None,
        }
    }

    /// Parse the body of a report — what follows `ESC ] 7501;` and precedes the
    /// terminator.
    ///
    /// `None` means the core would ignore this report: an unknown or missing
    /// `state`, a field it does not know, a `progress` outside 0–100, or a
    /// title/msg that is not base64 UTF-8. All of those drop the *whole* report,
    /// because a half-applied status is worse than the previous one.
    pub fn parse(body: &str) -> Option<Self> {
        let mut fields = body.split(':');
        let first = fields.next()?;
        let word = field(first, "state")?;
        // `clear` is a status word on the wire, though not a state. Any other
        // word the core does not know drops the report, which is what makes
        // `state=blocked;kind=question` vanish: it is one word, and not one of
        // the five.
        let word = if word == "clear" {
            ReportWord::Clear
        } else {
            ReportWord::State(known_word(word)?)
        };
        let mut report = Self {
            word,
            id: None,
            kind: None,
            progress: None,
            app: None,
            title: None,
            msg: None,
        };
        for piece in fields {
            let (key, value) = piece.split_once('=')?;
            match key {
                "state" => return None,
                "id" => report.id = Some(value.to_string()),
                "kind" => report.kind = Some(AskKind::parse(value)?),
                "progress" => report.progress = Some(value.parse().ok().filter(|p| *p <= 100)?),
                "app" => report.app = Some(value.to_string()),
                "title" => report.title = Some(decode(value)?),
                "msg" => report.msg = Some(decode(value)?),
                _ => return None,
            }
        }
        Some(report)
    }
}

/// The state `word` names, when the core accepts it.
///
/// Stricter than [`AgentState::parse`], which tolerates a word this build does
/// not know so a newer core's status still folds. A report is a request, and a
/// request with a word the core does not know is one it ignores.
fn known_word(word: &str) -> Option<AgentState> {
    match AgentState::parse(word)? {
        AgentState::Other(_) => None,
        state => Some(state),
    }
}

/// The value of `key=value`, when `key` is the one wanted.
fn field<'a>(piece: &'a str, key: &str) -> Option<&'a str> {
    let (found, value) = piece.split_once('=')?;
    (found == key).then_some(value)
}

/// base64 of UTF-8, as the core decodes it.
fn decode(value: &str) -> Option<String> {
    let bytes = crate::osc::base64_decode(value)?;
    String::from_utf8(bytes).ok()
}

/// Find every complete report in `bytes`, and where their raw spans were.
///
/// The scanner is deliberately separate from [`Report::parse`]: a terminal's
/// output arrives in arbitrary chunks, so a report may be split across two
/// reads, and a program that prints something else may print the prefix. A
/// span with no terminator is reported as incomplete so the caller can wait for
/// more bytes instead of dropping it.
pub fn scan(bytes: &[u8]) -> Vec<Found<'_>> {
    let mut found = Vec::new();
    let mut at = 0;
    loop {
        let Some(start) = find(bytes, at, PREFIX) else {
            // A chunk may end in the middle of the prefix itself, so a tail
            // that could still become one is carried, not dropped.
            if let Some(tail) = partial_prefix(&bytes[at..]) {
                let rest = &bytes[bytes.len() - tail..];
                found.push(Found {
                    body: None,
                    start: bytes.len() - tail,
                    rest,
                });
            }
            return found;
        };
        let body_at = start + PREFIX.len();
        let Some((end, terminator_len)) = terminator(bytes, body_at) else {
            found.push(Found {
                body: None,
                start,
                rest: &bytes[start..],
            });
            return found;
        };
        let body = std::str::from_utf8(&bytes[body_at..end]).ok();
        found.push(Found {
            body,
            start,
            rest: &[],
        });
        at = end + terminator_len;
    }
}

/// How many bytes at the end of `bytes` are a proper prefix of [`PREFIX`].
fn partial_prefix(bytes: &[u8]) -> Option<usize> {
    (1..PREFIX.len())
        .rev()
        .find(|length| bytes.len() >= *length && bytes[bytes.len() - length..] == PREFIX[..*length])
}

/// One report found in a byte stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found<'a> {
    /// The report's body, when the sequence was terminated and is UTF-8.
    pub body: Option<&'a str>,
    /// Where `ESC ] 7501;` started.
    pub start: usize,
    /// An unterminated report from its prefix onwards, so a caller can carry it
    /// into the next read; empty once the report is complete.
    pub rest: &'a [u8],
}

impl Found<'_> {
    /// Whether the terminator arrived. A caller that gets `false` should keep
    /// `rest` and prepend it to the next chunk.
    pub fn is_complete(&self) -> bool {
        self.body.is_some()
    }
}

/// Find `needle` in `haystack` at or after `from`.
fn find(haystack: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if from >= haystack.len() || needle.is_empty() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|at| at + from)
}

/// Where a report body ends: `ESC \` (ST) or `BEL`, and how long that was.
fn terminator(bytes: &[u8], from: usize) -> Option<(usize, usize)> {
    let mut at = from;
    while at < bytes.len() {
        match bytes[at] {
            0x07 => return Some((at, 1)),
            0x1b if bytes.get(at + 1) == Some(&b'\\') => return Some((at, 2)),
            _ => at += 1,
        }
    }
    None
}

/// base64, standard alphabet with padding, as the core reads it.
///
/// Kept here rather than adding a crate: the only base64 this crate meets is a
/// status title or message, and the grammar is fixed.
pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    // A group of one character can never be a whole byte, padded or not.
    if bytes.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3 + 2);
    for chunk in bytes.chunks(4) {
        let mut group = [0u8; 4];
        let mut padding = 0;
        for (at, byte) in chunk.iter().enumerate() {
            group[at] = match byte {
                b'A'..=b'Z' => byte - b'A',
                b'a'..=b'z' => byte - b'a' + 26,
                b'0'..=b'9' => byte - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                b'=' if at >= 2 => {
                    padding += 1;
                    0
                }
                _ => return None,
            };
        }
        let bits = (u32::from(group[0]) << 18)
            | (u32::from(group[1]) << 12)
            | (u32::from(group[2]) << 6)
            | u32::from(group[3]);
        out.push((bits >> 16) as u8);
        if padding < 2 && chunk.len() > 2 {
            out.push((bits >> 8) as u8);
        }
        if padding < 1 && chunk.len() > 3 {
            out.push(bits as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// The fake agent M7ef kept as a fixture: three reports, ST-terminated.
    const SCRIPT: &str = "\x1b]7501;state=working:app=shprd-probe:progress=10\x1b\\\
                          \x1b]7501;state=blocked:kind=permission:msg=QXBwcm92ZT8=\x1b\\\
                          \x1b]7501;state=done:msg=ZmluaXNoZWQ=\x1b\\";

    #[test]
    fn the_recorded_working_report_parses() {
        let report = Report::parse("state=working:app=shprd-probe:progress=10").unwrap();
        assert_eq!(report.state(), Some(&AgentState::Working));
        assert_eq!(report.app.as_deref(), Some("shprd-probe"));
        assert_eq!(report.progress, Some(10));
        assert!(!report.is_clear());
    }

    #[test]
    fn the_recorded_blocked_report_decodes_its_message() {
        // msg=QXBwcm92ZT8= is "Approve?".
        let report = Report::parse("state=blocked:kind=permission:msg=QXBwcm92ZT8=").unwrap();
        assert_eq!(report.state(), Some(&AgentState::Blocked));
        assert!(report.state().unwrap().needs_a_person());
        assert_eq!(report.kind, Some(AskKind::Permission));
        assert_eq!(report.msg.as_deref(), Some("Approve?"));
    }

    #[test]
    fn the_recorded_done_report_decodes_its_message() {
        let report = Report::parse("state=done:msg=ZmluaXNoZWQ=").unwrap();
        assert_eq!(report.state(), Some(&AgentState::Done));
        assert_eq!(report.msg.as_deref(), Some("finished"));
    }

    #[test]
    fn a_semicolon_separated_field_is_ignored_as_the_core_ignores_it() {
        // Recorded live: `state=blocked;kind=question` never becomes a status.
        assert_eq!(Report::parse("state=blocked;kind=question"), None);
        // The same fields with the right separator do parse.
        assert_eq!(
            Report::parse("state=blocked:kind=question").unwrap().kind,
            Some(AskKind::Question)
        );
    }

    #[test]
    fn clear_is_a_report_that_removes_a_record() {
        let report = Report::parse("state=clear").unwrap();
        assert!(report.is_clear());
        let child = Report::parse("state=clear:id=sub-1").unwrap();
        assert!(child.is_clear());
        assert_eq!(child.id.as_deref(), Some("sub-1"));
    }

    #[test]
    fn a_child_record_keeps_its_id() {
        let report = Report::parse("state=working:id=tests:progress=40").unwrap();
        assert_eq!(report.id.as_deref(), Some("tests"));
        assert_eq!(report.progress, Some(40));
    }

    #[test]
    fn a_word_the_core_does_not_know_drops_the_report() {
        assert_eq!(Report::parse("state=thinking"), None);
        assert_eq!(Report::parse("app=x"), None);
        assert_eq!(Report::parse("state="), None);
        assert_eq!(Report::parse(""), None);
    }

    #[test]
    fn the_five_words_the_core_knows_all_parse() {
        for (word, expected) in [
            ("idle", AgentState::Idle),
            ("working", AgentState::Working),
            ("done", AgentState::Done),
            ("blocked", AgentState::Blocked),
            ("error", AgentState::Error),
        ] {
            let report = Report::parse(&format!("state={word}")).unwrap();
            assert_eq!(report.state(), Some(&expected), "{word}");
        }
    }

    #[test]
    fn a_field_the_grammar_does_not_have_drops_the_whole_report() {
        assert_eq!(Report::parse("state=working:colour=red"), None);
        assert_eq!(Report::parse("state=working:noequals"), None);
    }

    #[test]
    fn a_progress_outside_the_range_drops_the_report() {
        assert_eq!(Report::parse("state=working:progress=101"), None);
        assert_eq!(Report::parse("state=working:progress=-1"), None);
        assert_eq!(Report::parse("state=working:progress=half"), None);
        assert_eq!(
            Report::parse("state=working:progress=100")
                .unwrap()
                .progress,
            Some(100)
        );
        assert_eq!(
            Report::parse("state=working:progress=0").unwrap().progress,
            Some(0)
        );
    }

    #[test]
    fn invalid_utf8_inside_a_message_drops_the_whole_report() {
        // /w== is the single byte 0xFF, which is not UTF-8.
        assert_eq!(Report::parse("state=done:msg=/w=="), None);
        // And a message that is not base64 at all.
        assert_eq!(Report::parse("state=done:msg=not base64!"), None);
    }

    #[test]
    fn base64_matches_the_recorded_examples() {
        assert_eq!(base64_decode("QXBwcm92ZT8=").unwrap(), b"Approve?");
        assert_eq!(base64_decode("ZmluaXNoZWQ=").unwrap(), b"finished");
        assert_eq!(base64_decode("").unwrap(), b"");
        assert_eq!(base64_decode("aGk=").unwrap(), b"hi");
        assert_eq!(base64_decode("aGk").unwrap(), b"hi");
        assert_eq!(base64_decode("aGk=").unwrap(), b"hi");
        assert!(base64_decode("a").is_none());
        assert!(base64_decode("aGk=").is_some());
        assert_eq!(base64_decode("aGk=").unwrap(), b"hi");
        assert!(base64_decode("!!!!").is_none());
    }

    #[test]
    fn the_recorded_script_yields_three_term_stop_reports() {
        let found = scan(SCRIPT.as_bytes());
        assert_eq!(found.len(), 3, "{found:?}");
        let reports: Vec<Report> = found
            .iter()
            .map(|one| Report::parse(one.body.unwrap()).unwrap())
            .collect();
        assert_eq!(reports[0].state(), Some(&AgentState::Working));
        assert_eq!(reports[1].kind, Some(AskKind::Permission));
        assert_eq!(reports[2].msg.as_deref(), Some("finished"));
    }

    #[test]
    fn a_bel_terminator_counts_too() {
        let script = "\x1b]7501;state=idle\x07";
        let found = scan(script.as_bytes());
        assert_eq!(found.len(), 1);
        assert_eq!(
            Report::parse(found[0].body.unwrap()).unwrap().state(),
            Some(&AgentState::Idle)
        );
    }

    #[test]
    fn a_report_split_across_reads_waits_for_the_rest() {
        let whole = b"\x1b]7501;state=working\x1b\\";
        for split in 1..whole.len() {
            let first = scan(&whole[..split]);
            assert_eq!(first.len(), 1, "split at {split}");
            assert!(!first[0].is_complete(), "split at {split}");
            let mut rejoined = first[0].rest.to_vec();
            rejoined.extend_from_slice(&whole[split..]);
            let second = scan(&rejoined);
            assert_eq!(second.len(), 1, "split at {split}");
            assert!(second[0].is_complete(), "split at {split}");
            assert_eq!(second[0].body, Some("state=working"));
        }
    }

    #[test]
    fn text_around_a_report_is_not_part_of_it() {
        let script = "starting\x1b]7501;state=working\x1b\\and then more output\n";
        let found = scan(script.as_bytes());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].start, 8);
        assert_eq!(found[0].body, Some("state=working"));
    }

    #[test]
    fn bytes_with_no_report_yield_nothing() {
        assert!(scan(b"plain terminal output\r\n").is_empty());
        assert!(scan(b"").is_empty());
    }
}
