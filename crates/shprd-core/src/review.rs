//! Where a review's packet lives, and the one file read back out of it.
//!
//! A reviewer runs outside the repository: it reads a packet folder and writes
//! `findings.json`, and nothing else. The engine is what turns that file into a
//! gate result, and it may be running as `shprd watch`, as `shprd serve` or as a
//! person's `shprd agent review-verdict`. They all have to look for the file in
//! the same place and trust it at the same age, so the path and the settle
//! window live here rather than in whichever caller was written first.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::event::{Event, Finding};
use crate::state::RunState;

/// How long a findings file must sit untouched before a reader trusts it.
///
/// A reviewer writes its file in one go, but a reader can fall between the open
/// and the last byte: reading a file younger than this would decide a review
/// from half of it.
pub const FINDINGS_SETTLE: Duration = Duration::from_secs(2);

/// How long a review may be out without a verdict before a person is asked.
///
/// A reviewer that died, hung (a provider retrying for hours) or keeps failing
/// writes nothing, and nothing else says so: the task would read `In review`
/// for ever. Thirty minutes is longer than any review the engine has seen take
/// and short enough that nobody finds the board stale at the end of the day.
pub const STUCK_AFTER_MS: u64 = 30 * 60 * 1000;

/// What every ask id of a stuck review's card begins with.
pub const STUCK_ASK_PREFIX: &str = "review-stuck-";

/// The choice that closes the reviewer and starts a new review.
pub const REVIEW_AGAIN_CHOICE: &str = "review again";

/// The choice that drops the task.
pub const STUCK_DROP_CHOICE: &str = "drop it";

/// What a refused review begins with when every reviewer is out of quota.
pub const NO_REVIEWER_LEFT: &str = "no reviewer is left to take this review";

/// What one pass over the running reviewers did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewSwitch {
    /// A reviewer that ran out of quota was replaced by the next one.
    Switched {
        /// The task under review.
        task: String,
        /// The plain sentence for the log and the card.
        line: String,
    },
    /// A reviewer ran out of quota and no other can take the review.
    NoneLeft {
        /// The task under review.
        task: String,
        /// The attempt being reviewed.
        attempt: u32,
        /// The reviewers that are out of quota, in plain words.
        why: String,
    },
}

/// A review note as a card's one short line: the dates and the "instead" go,
/// `codex is out of quota; GLM (pi) reviews`.
pub fn short_note(note: &str) -> String {
    note.split("; ")
        .map(|part| {
            let part = part.strip_suffix(" instead").unwrap_or(part);
            match part.find(" until ") {
                Some(at) => &part[..at],
                None => part,
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// What the person's card says when no reviewer is left.
pub fn none_left_body(task: &str, attempt: u32, why: &str) -> String {
    format!(
        "{task}: no reviewer can review attempt {attempt} right now: {why}. Add another \
         reviewer in Settings, or wait until one is back, then choose review again; or drop \
         the task"
    )
}

/// The id of the `n`th card a review of `attempt` has put in front of a person.
pub fn stuck_ask(attempt: u32, n: usize) -> String {
    format!("{STUCK_ASK_PREFIX}{attempt}-{n}")
}

/// The choices a stuck review's card offers.
pub fn stuck_options() -> Vec<String> {
    vec![
        REVIEW_AGAIN_CHOICE.to_string(),
        STUCK_DROP_CHOICE.to_string(),
    ]
}

/// What a stuck review's card says.
pub fn stuck_body(task: &str, attempt: u32, reviewer: &str, minutes: u64) -> String {
    format!(
        "{task}: the review of {} by {reviewer} has not finished after {minutes} \
         minutes; the reviewer may have died, hung or be failing",
        crate::event::attempt_label(task, attempt)
    )
}

/// A review that has been out too long.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StuckReview {
    /// The task under review.
    pub task: String,
    /// The attempt being reviewed.
    pub attempt: u32,
    /// The reviewer the request named.
    pub reviewer: String,
    /// The id of the card to raise for it.
    pub ask: String,
    /// How long the request has been out, in whole minutes.
    pub minutes: u64,
}

/// The reviews that were requested more than `after_ms` before `now_ms` and
/// have neither a verdict nor a card since that request.
///
/// A card raised after the request is the decision to wait for, so the same
/// request is never put in front of a person twice; a new request (`review
/// again`) starts the clock over.
pub fn stuck_reviews(
    state: &RunState,
    events: &[Event],
    now_ms: u64,
    after_ms: u64,
) -> Vec<StuckReview> {
    let mut stuck = Vec::new();
    for task in state.tasks.values() {
        let Some(attempt) = crate::tick::awaiting_verdict(task) else {
            continue;
        };
        let Some(request) = task
            .latest_attempt()
            .and_then(|current| current.reviews.last())
        else {
            continue;
        };
        let Some(asked_at) = events
            .iter()
            .find(|event| event.seq == request.seq)
            .map(|event| event.at_ms)
        else {
            continue;
        };
        let age = now_ms.saturating_sub(asked_at);
        if age < after_ms {
            continue;
        }
        let prefix = format!("{STUCK_ASK_PREFIX}{attempt}-");
        let own: Vec<_> = state
            .asks
            .values()
            .filter(|ask| ask.task == task.id && ask.id.starts_with(&prefix))
            .collect();
        if own.iter().any(|ask| ask.raised_seq > request.seq) {
            continue;
        }
        stuck.push(StuckReview {
            task: task.id.clone(),
            attempt,
            reviewer: request.reviewer.clone(),
            ask: stuck_ask(attempt, own.len() + 1),
            minutes: age / 60_000,
        });
    }
    stuck
}

/// Where a review packet lives: `<reviews>/<run>/<task>-a<attempt>/`.
pub fn packet_dir(reviews: &Path, run: &str, task: &str, attempt: u32) -> PathBuf {
    reviews.join(run).join(format!("{task}-a{attempt}"))
}

/// The file a reviewer writes its findings to, inside its packet folder.
///
/// It is the one file read back out of a packet: the engine watches it, so a
/// finished review is picked up without the reviewer running any command.
pub fn findings_path(reviews: &Path, run: &str, task: &str, attempt: u32) -> PathBuf {
    packet_dir(reviews, run, task, attempt).join("findings.json")
}

/// What a human is asked to do about a reviewer's unreadable findings file.
pub const INVALID_FINDINGS_ACTION: &str = "read the packet and decide the review by hand";

/// What every escalation about an unreadable findings file begins with.
///
/// It is the marker a later pass compares, so one bad file is one escalation
/// rather than one per pass.
pub fn invalid_findings_prefix(task: &str) -> String {
    format!("{task}: the reviewer's findings could not be read: ")
}

/// The mtime of `path`, when the file is old enough to be read.
///
/// A file that is missing, unreadable or still being written yields `None`: all
/// three mean "nothing to decide yet", and none of them is a failed review. A
/// restart asks the same question, which is why a caller that must not delete
/// findings mid-write (`review --again`) reads the answer from here too.
pub fn settled_mtime(path: &Path) -> Option<SystemTime> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let age = SystemTime::now().duration_since(modified).ok()?;
    (age >= FINDINGS_SETTLE).then_some(modified)
}

/// Reads the JSON array of findings a reviewer wrote.
///
/// Unreadable or invalid findings are the reviewer's mistake, never the engine's:
/// the error names the file so the caller can escalate it and a person can look
/// at the packet.
pub fn read_findings(path: &Path) -> Result<Vec<Finding>, FindingsError> {
    let bytes = std::fs::read(path).map_err(|source| FindingsError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    parse_findings(&bytes, path)
}

/// Parses the bytes of a reviewer's findings file.
///
/// It is separate from [`read_findings`] because a caller that also records the
/// file's hash needs the exact bytes it parsed, and reading the file twice could
/// read two different files.
///
/// Both shapes of the file are read: the bare array of findings, and the object
/// `{findings, decisions}` ([`parse_report`]). Only the findings are returned,
/// so a caller that decides a verdict never sees a decision to confirm as a
/// defect.
pub fn parse_findings(bytes: &[u8], path: &Path) -> Result<Vec<Finding>, FindingsError> {
    parse_report(bytes, path).map(|report| report.findings)
}

/// A choice the worker disclosed in its summary, which the reviewer lists for
/// the person to confirm.
///
/// It is not a defect: it never counts toward a verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decision {
    /// One-line title: what the worker chose.
    pub title: String,
    /// Why it matters and what confirming it means.
    #[serde(default)]
    pub detail: String,
}

/// What a reviewer's `findings.json` holds: the defects, and the decisions to
/// confirm that are kept apart from them.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewReport {
    /// The defects, which decide the verdict.
    #[serde(default)]
    pub findings: Vec<Finding>,
    /// The worker's disclosed choices, for the person to confirm.
    #[serde(default)]
    pub decisions: Vec<Decision>,
}

/// Parses a reviewer's file, either the bare array of findings (no decisions)
/// or the object `{findings, decisions}`.
pub fn parse_report(bytes: &[u8], path: &Path) -> Result<ReviewReport, FindingsError> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Shape {
        Bare(Vec<Finding>),
        Report(ReviewReport),
    }
    let shape: Shape = serde_json::from_slice(bytes).map_err(|source| FindingsError::Parse {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(match shape {
        Shape::Bare(findings) => ReviewReport {
            findings,
            decisions: Vec::new(),
        },
        Shape::Report(report) => report,
    })
}

/// What the person may answer about one finding or decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemarkAnswer {
    /// Seen, and it is what the person wants.
    AcceptedAsIs,
    /// The agent should change it.
    FixIt,
    /// Not for this task: a new task takes it.
    FollowUp,
}

impl RemarkAnswer {
    /// Every answer, in the order a card offers them.
    pub const ALL: [Self; 3] = [Self::AcceptedAsIs, Self::FixIt, Self::FollowUp];

    /// The words recorded in the log and shown on the card.
    pub fn text(self) -> &'static str {
        match self {
            Self::AcceptedAsIs => "accepted as is",
            Self::FixIt => "fix it",
            Self::FollowUp => "follow-up task",
        }
    }

    /// The answer a recorded text names.
    pub fn from_text(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|answer| answer.text() == text)
    }
}

/// Which remark of a review an answer is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "of", content = "index", rename_all = "snake_case")]
pub enum RemarkRef {
    /// The `n`th finding, counted from 1.
    Finding(u32),
    /// The `n`th decision to confirm, counted from 1.
    Decision(u32),
}

/// What every ask id of an answerable remark begins with.
pub const REMARK_ASK_PREFIX: &str = "remark-";

/// The id of the ask that records the answer to `remark` of `attempt`.
///
/// An answer is an `AskRaised` and an `AskAnswered` under this id, so it is an
/// event and needs no kind of its own: the first answer to a remark is the one
/// the log keeps (a second is refused as already answered).
pub fn remark_ask(attempt: u32, remark: RemarkRef) -> String {
    match remark {
        RemarkRef::Finding(n) => format!("{REMARK_ASK_PREFIX}{attempt}-f{n}"),
        RemarkRef::Decision(n) => format!("{REMARK_ASK_PREFIX}{attempt}-d{n}"),
    }
}

/// The remark an ask id names, with its attempt, when it is one.
pub fn parse_remark_ask(ask: &str) -> Option<(u32, RemarkRef)> {
    let rest = ask.strip_prefix(REMARK_ASK_PREFIX)?;
    let (attempt, remark) = rest.split_once('-')?;
    let attempt = attempt.parse().ok()?;
    let (kind, n) = remark.split_at_checked(1)?;
    let n: u32 = n.parse().ok().filter(|n| *n >= 1)?;
    match kind {
        "f" => Some((attempt, RemarkRef::Finding(n))),
        "d" => Some((attempt, RemarkRef::Decision(n))),
        _ => None,
    }
}

/// The question an answerable remark's card asks, naming its title.
pub fn remark_text(task: &str, remark: RemarkRef, title: &str) -> String {
    match remark {
        RemarkRef::Finding(_) => format!("{task}: the reviewer found: {title}"),
        RemarkRef::Decision(_) => {
            format!("{task}: the agent chose this and said so; confirm it: {title}")
        }
    }
}

/// Why an answer to a remark was refused before anything was recorded.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RemarkError {
    /// The review has no such finding or decision.
    #[error("the review of attempt {attempt} has no {what} number {index}")]
    Unknown {
        /// The attempt whose review was read.
        attempt: u32,
        /// `finding` or `decision`.
        what: &'static str,
        /// The number asked about.
        index: u32,
    },
}

/// The title of the remark an answer names, or why there is none.
///
/// Pure: the service reads the attempt's report, asks this before it raises
/// and answers the remark's ask ([`remark_ask`]), so an answer to a finding
/// the review never made is refused and not recorded.
pub fn remark_title(
    report: &ReviewReport,
    attempt: u32,
    remark: RemarkRef,
) -> Result<String, RemarkError> {
    let (what, index, title) = match remark {
        RemarkRef::Finding(n) => (
            "finding",
            n,
            usize::try_from(n)
                .ok()
                .and_then(|n| n.checked_sub(1))
                .and_then(|at| report.findings.get(at))
                .map(|f| f.title.clone()),
        ),
        RemarkRef::Decision(n) => (
            "decision",
            n,
            usize::try_from(n)
                .ok()
                .and_then(|n| n.checked_sub(1))
                .and_then(|at| report.decisions.get(at))
                .map(|d| d.title.clone()),
        ),
    };
    title.ok_or(RemarkError::Unknown {
        attempt,
        what,
        index,
    })
}

/// The JSON Schema a reviewer's `findings.json` must match: the array of
/// findings, or the object `{findings, decisions}` ([`parse_report`]).
pub fn report_schema() -> serde_json::Value {
    let finding = serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["severity", "title", "detail"],
        "properties": {
            "severity": {
                "type": "string",
                "enum": ["critical", "major", "minor", "trivial"]
            },
            "title": { "type": "string", "minLength": 1 },
            "location": { "type": "string" },
            "detail": { "type": "string", "minLength": 1 }
        }
    });
    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "shprd review findings",
        "oneOf": [
            { "type": "array", "items": finding },
            {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "findings": { "type": "array", "items": finding },
                    "decisions": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["title"],
                            "properties": {
                                "title": { "type": "string", "minLength": 1 },
                                "detail": { "type": "string" }
                            }
                        }
                    }
                }
            }
        ]
    })
}

/// The choices an answerable remark's card offers.
pub fn remark_options() -> Vec<String> {
    RemarkAnswer::ALL
        .iter()
        .map(|answer| answer.text().to_string())
        .collect()
}

/// One decision on the "ready to land" card: what the worker chose and what
/// the person has said about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionToConfirm {
    /// The decision's place in the review, from 1.
    pub index: u32,
    /// What the worker chose.
    pub title: String,
    /// Why it matters.
    pub detail: String,
    /// The person's answer, when there is one.
    pub answer: Option<RemarkAnswer>,
}

/// The decisions a review of `attempt` listed, with the answers recorded so far.
///
/// It reads the answered asks of the task, so the card data follows the log.
pub fn decisions_to_confirm(
    state: &RunState,
    task: &str,
    attempt: u32,
    report: &ReviewReport,
) -> Vec<DecisionToConfirm> {
    report
        .decisions
        .iter()
        .enumerate()
        .map(|(at, decision)| {
            let index = u32::try_from(at + 1).unwrap_or(u32::MAX);
            let id = remark_ask(attempt, RemarkRef::Decision(index));
            let answer = state
                .asks
                .values()
                .find(|ask| ask.task == task && ask.id == id)
                .and_then(|ask| ask.answer.as_ref())
                .and_then(|answer| RemarkAnswer::from_text(&answer.text));
            DecisionToConfirm {
                index,
                title: decision.title.clone(),
                detail: decision.detail.clone(),
                answer,
            }
        })
        .collect()
}

/// Everything that can go wrong reading a reviewer's findings.
#[derive(Debug, Error)]
pub enum FindingsError {
    /// The file could not be read.
    #[error("reading findings from {path}: {source}")]
    Read {
        /// The file that failed.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// The file is not the JSON array of findings the packet asks for.
    #[error("parsing findings in {path}: {source}")]
    Parse {
        /// The file that failed.
        path: PathBuf,
        /// The underlying JSON error.
        source: serde_json::Error,
    },
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::io::Write;
    use std::path::Path;
    use std::time::Duration;

    use super::{
        FINDINGS_SETTLE, RemarkAnswer, RemarkRef, findings_path, packet_dir, parse_remark_ask,
        parse_report, read_findings, remark_ask, remark_title, report_schema, settled_mtime,
    };

    #[test]
    fn a_report_carries_decisions_apart_from_findings() {
        let path = std::path::Path::new("findings.json");
        let report = parse_report(
            br#"{"findings":[{"severity":"minor","title":"t","detail":"d"}],
                "decisions":[{"title":"Titan card stays dark","detail":"disclosed"}]}"#,
            path,
        )
        .expect("the object is valid");
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.decisions[0].title, "Titan card stays dark");
        let only = parse_report(br#"{"decisions":[{"title":"x"}]}"#, path).expect("valid");
        assert!(only.findings.is_empty(), "decisions alone are no defect");
        let bare = parse_report(b"[]", path).expect("the bare array still reads");
        assert!(bare.findings.is_empty() && bare.decisions.is_empty());
        assert!(
            super::parse_findings(br#"{"findings":[],"decisions":[{"title":"x"}]}"#, path)
                .expect("valid")
                .is_empty(),
            "a decision never reaches the verdict"
        );
        assert!(parse_report(br#"{"findings":[],"other":1}"#, path).is_err());
    }

    #[test]
    fn an_answer_names_a_real_remark_and_the_schema_allows_decisions() {
        let report = parse_report(
            br#"{"findings":[{"severity":"minor","title":"f1","detail":"d"}],"decisions":[{"title":"d1"}]}"#,
            Path::new("findings.json"),
        )
        .unwrap();
        assert_eq!(
            remark_title(&report, 1, RemarkRef::Finding(1)).unwrap(),
            "f1"
        );
        assert_eq!(
            remark_title(&report, 1, RemarkRef::Decision(1)).unwrap(),
            "d1"
        );
        assert!(remark_title(&report, 1, RemarkRef::Decision(2)).is_err());
        assert!(remark_title(&report, 1, RemarkRef::Finding(0)).is_err());
        let schema = report_schema();
        let object = &schema["oneOf"][1]["properties"];
        assert_eq!(object["decisions"]["items"]["required"][0], "title");
        assert!(object["findings"]["items"]["properties"]["severity"].is_object());
    }

    #[test]
    fn a_remark_answer_has_one_ask_id_and_one_text() {
        for remark in [RemarkRef::Finding(3), RemarkRef::Decision(1)] {
            assert_eq!(parse_remark_ask(&remark_ask(2, remark)), Some((2, remark)));
        }
        assert_eq!(remark_ask(2, RemarkRef::Decision(1)), "remark-2-d1");
        assert_eq!(parse_remark_ask("remark-2-d0"), None);
        assert_eq!(parse_remark_ask("review-stuck-2-1"), None);
        for answer in RemarkAnswer::ALL {
            assert_eq!(RemarkAnswer::from_text(answer.text()), Some(answer));
        }
        assert_eq!(RemarkAnswer::from_text("maybe"), None);
    }

    /// A fresh file whose mtime is `age` before now.
    fn file_at(dir: &std::path::Path, name: &str, body: &str, age: Duration) -> std::path::PathBuf {
        let path = dir.join(name);
        let mut file = std::fs::File::create(&path).expect("the file is created");
        file.write_all(body.as_bytes())
            .expect("the file is written");
        file.set_modified(std::time::SystemTime::now() - age)
            .expect("the mtime is set");
        path
    }

    #[test]
    fn the_packet_path_names_the_run_the_task_and_the_attempt() {
        let reviews = std::path::Path::new("/home/.shprd/reviews");
        assert_eq!(
            packet_dir(reviews, "r1", "T1", 2),
            std::path::Path::new("/home/.shprd/reviews/r1/T1-a2")
        );
        assert_eq!(
            findings_path(reviews, "r1", "T1", 2),
            std::path::Path::new("/home/.shprd/reviews/r1/T1-a2/findings.json")
        );
    }

    #[test]
    fn a_findings_file_is_read_only_once_it_has_settled() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let fresh = file_at(dir.path(), "fresh.json", "[]", Duration::from_millis(0));
        assert!(
            settled_mtime(&fresh).is_none(),
            "a file just written may still be being written"
        );
        let settled = file_at(
            dir.path(),
            "settled.json",
            "[]",
            FINDINGS_SETTLE + Duration::from_millis(10),
        );
        assert!(settled_mtime(&settled).is_some());
        assert!(
            settled_mtime(&dir.path().join("missing.json")).is_none(),
            "a missing file says nothing about the review"
        );
    }

    #[test]
    fn a_switch_note_is_one_short_card_line() {
        assert_eq!(
            crate::review::short_note(
                "codex is out of quota until Dec 18; GLM (pi) reviews instead"
            ),
            "codex is out of quota; GLM (pi) reviews"
        );
        assert_eq!(
            crate::review::short_note("nothing to cut"),
            "nothing to cut"
        );
    }

    #[test]
    fn findings_parse_into_the_list_the_rules_read() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = file_at(
            dir.path(),
            "findings.json",
            r#"[{"severity":"major","title":"a finding","detail":"fix it"}]"#,
            FINDINGS_SETTLE,
        );
        let findings = read_findings(&path).expect("the file is valid");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].title, "a finding");

        let broken = file_at(dir.path(), "broken.json", "not json", FINDINGS_SETTLE);
        assert!(
            read_findings(&broken).is_err(),
            "invalid findings are an error"
        );
    }
}
