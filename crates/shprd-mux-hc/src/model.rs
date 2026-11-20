//! What herdr-core says about terminals: the [`Terminal`] record, its status,
//! and the notifications that carry changes to it.
//!
//! The shapes come from the M7ef spike's live capture (§2.3 and §2.4): the
//! `Terminal` object as 0.1.21 answers it, the twelve event types with the
//! `data` each carries, and the one notification envelope shared by `event`,
//! `heartbeat` and `missed`.
//!
//! Every struct here tolerates keys it does not know and fields that are
//! missing: herdr-core only ever adds within a generation, and a field a new
//! core drops must not fail a whole fold.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// What a program told its terminal it is doing — the OSC 7501 status word.
///
/// `state=clear` is not a state a terminal is in: it removes a record, so it
/// only ever appears in [`crate::osc::Report`], never in a folded [`Terminal`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentState {
    /// Nothing to do; the program waits for input.
    Idle,
    /// The program is producing output or running tools.
    Working,
    /// The program finished its turn.
    Done,
    /// The program waits for a person: a permission, a question, a login.
    Blocked,
    /// The program failed.
    Error,
    /// A word this build does not know, kept verbatim.
    Other(String),
}

impl AgentState {
    /// The word herdr-core uses, so it can round-trip.
    pub fn word(&self) -> &str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Done => "done",
            Self::Blocked => "blocked",
            Self::Error => "error",
            Self::Other(word) => word,
        }
    }

    /// The state `word` names, or `None` when it is not a status word at all.
    ///
    /// A word this build does not know is [`AgentState::Other`], so a newer
    /// core's status still folds; only `clear`, which is not a state, is
    /// refused here.
    pub fn parse(word: &str) -> Option<Self> {
        Some(match word {
            "idle" => Self::Idle,
            "working" => Self::Working,
            "done" => Self::Done,
            "blocked" => Self::Blocked,
            "error" => Self::Error,
            "clear" => return None,
            other => Self::Other(other.to_string()),
        })
    }

    /// Whether this state means a person is being waited on.
    pub fn needs_a_person(&self) -> bool {
        matches!(self, Self::Blocked)
    }
}

impl std::fmt::Display for AgentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.word())
    }
}

impl Serialize for AgentState {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.word())
    }
}

impl<'de> Deserialize<'de> for AgentState {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let word = String::deserialize(deserializer)?;
        Self::parse(&word)
            .ok_or_else(|| serde::de::Error::custom(format!("{word:?} is not a status word")))
    }
}

/// What kind of thing a blocked program waits for.
///
/// herdr-core's set is `permission`, `question` and `auth`; the M7b layout turns
/// each into a different needs-you card, so an unknown one is kept rather than
/// dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AskKind {
    /// A tool wants to run and asks first.
    Permission,
    /// The agent asks the person a question.
    Question,
    /// A login, a token or a credential is missing.
    Auth,
    /// A word this build does not know.
    Other(String),
}

impl AskKind {
    /// The word herdr-core uses.
    pub fn word(&self) -> &str {
        match self {
            Self::Permission => "permission",
            Self::Question => "question",
            Self::Auth => "auth",
            Self::Other(word) => word,
        }
    }

    /// The kind `word` names, or `None` for an empty one.
    pub fn parse(word: &str) -> Option<Self> {
        Some(match word {
            "" => return None,
            "permission" => Self::Permission,
            "question" => Self::Question,
            "auth" => Self::Auth,
            other => Self::Other(other.to_string()),
        })
    }
}

impl Serialize for AskKind {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.word())
    }
}

impl<'de> Deserialize<'de> for AskKind {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let word = String::deserialize(deserializer)?;
        Self::parse(&word)
            .ok_or_else(|| serde::de::Error::custom(format!("{word:?} is not an ask kind")))
    }
}

/// One program's report: the status word and what goes with it.
///
/// `title` and `msg` are base64 on the wire (UTF-8 inside), so they are already
/// decoded here — an undecodable one makes the whole report be dropped, exactly
/// as herdr-core does (`status.rs:45-57` in its source).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    /// The status word.
    pub state: AgentStateWord,
    /// What a `blocked` program waits for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<AskKind>,
    /// How far along a `working` program is, 0–100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<u8>,
    /// The program's own name, as it reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// A one-line title, already decoded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// A longer message, already decoded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msg: Option<String>,
}

impl Status {
    /// Read a status out of a `status` notification's `data`.
    ///
    /// The core has sent both shapes: the report flat in `data` (`{state,
    /// app, progress, reported, records}`, which is what the M7ef capture
    /// shows) and wrapped under `status` (`{status:{…}, reported, records}`,
    /// which is what its schema says). Both are read; anything else is `None`,
    /// so a shape this build does not know leaves the last status alone rather
    /// than clearing it.
    pub fn from_event_data(data: &Value) -> Option<Self> {
        let report = data.get("status").unwrap_or(data);
        if report.is_null() {
            return None;
        }
        serde_json::from_value(report.clone()).ok()
    }
}

/// A status word that must be one: [`Status::state`] cannot be `clear`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AgentStateWord(AgentState);

impl AgentStateWord {
    /// Wrap a state, refusing nothing: `clear` cannot get here through serde,
    /// but a caller building a status by hand may still make one.
    pub fn new(state: AgentState) -> Self {
        Self(state)
    }

    /// The state itself.
    pub fn get(&self) -> &AgentState {
        &self.0
    }
}

impl From<AgentStateWord> for String {
    fn from(word: AgentStateWord) -> Self {
        word.0.word().to_string()
    }
}

impl TryFrom<String> for AgentStateWord {
    type Error = String;

    fn try_from(word: String) -> std::result::Result<Self, Self::Error> {
        AgentState::parse(&word)
            .map(Self)
            .ok_or_else(|| format!("{word:?} is not a status word"))
    }
}

impl std::fmt::Display for AgentStateWord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0.word())
    }
}

/// The program in front of a terminal: what a shell's command, or a started
/// agent, currently is.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Foreground {
    /// The argv the program was started with.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub argv: Vec<String>,
    /// The program's working directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// The name herdr-core gives it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Its process id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
}

/// One terminal as herdr-core describes it (M7ef §2.3).
///
/// Only the fields this crate reads are typed; everything else the core sends is
/// ignored, and every field but `address` may be absent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Terminal {
    /// The core's address for it: `t7` here, `machine/t7` for another machine.
    pub address: String,
    /// The terminal's name, unique among live ones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The terminal that started this one, when any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Every label on it; `herdr.space` / `herdr.tab` are the ones shprd owns.
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    /// The argv it runs.
    #[serde(default)]
    pub argv: Vec<String>,
    /// The template the core started it from, when any (Shepherdr's Assistant
    /// is `herdr-assistant-…`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    /// Where it starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// The program in front, when the core knows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreground: Option<Foreground>,
    /// `"live"` or `"finished"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// The root status record, when a program reported one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<Status>,
    /// The child status records, by their `id=`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub records: BTreeMap<String, Status>,
    /// Whether anything ever reported a status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reported: Option<bool>,
    /// How many times the root word left `working`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<u64>,
    /// Its process id, until it is lost.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// Why it finished: `killed`, `exited`, …
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Its exit code, when it exited by itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    /// The signal that ended it, when one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<i64>,
    /// Whether the core keeps it as a recovery shell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep: Option<bool>,
    /// When the core created it, in epoch milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_ms: Option<u64>,
    /// When it finished, in epoch milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_ms: Option<u64>,
    /// When it last reported a status, in epoch milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_ms: Option<u64>,
    /// Its width in columns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cols: Option<u32>,
    /// Its height in rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<u32>,
    /// The terminal's title, as it set it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

impl Terminal {
    /// The address without a machine prefix (`machine/t7` → `t7`).
    ///
    /// Labels and space ids are compared bare, because a remote machine's
    /// address carries its name and a space id never does (M7ef §2.8).
    pub fn bare_address(&self) -> &str {
        bare(&self.address)
    }

    /// The `herdr.space` label, when this terminal carries one.
    pub fn space_label(&self) -> Option<&str> {
        self.labels
            .get(crate::placement::SPACE_LABEL)
            .map(String::as_str)
    }

    /// The `herdr.tab` label, when this terminal carries one.
    pub fn tab_label(&self) -> Option<&str> {
        self.labels
            .get(crate::placement::TAB_LABEL)
            .map(String::as_str)
    }

    /// Whether the terminal's process is still running.
    pub fn is_live(&self) -> bool {
        self.state.as_deref() != Some("finished")
    }

    /// The state a person should see: the reported status word when there is
    /// one, else `done` for a finished terminal and `idle` for a live one that
    /// never reported.
    pub fn agent_state(&self) -> AgentState {
        match &self.status {
            Some(status) => status.state.get().clone(),
            None if !self.is_live() => AgentState::Done,
            None => AgentState::Idle,
        }
    }
}

/// The last path segment of an address or a space id.
pub fn bare(id: &str) -> &str {
    id.rsplit('/').next().unwrap_or(id)
}

/// One notification the core pushed, or one that its stream implies.
///
/// herdr-core sends `event`, `heartbeat` and `missed` on the event channel
/// (M7ef §2.4); [`Notice::Synced`] is this crate's own, published when a fresh
/// subscription replaced the terminal map.
#[derive(Debug, Clone, PartialEq)]
pub enum Notice {
    /// Something changed.
    Event(Change),
    /// The core is alive; nothing changed.
    Heartbeat {
        /// The combined sequence number.
        seq: u64,
    },
    /// The subscriber fell behind: `terminals` is the whole current set, and
    /// every event with a lower `seq` is to be ignored.
    Missed {
        /// The combined sequence number of the snapshot.
        seq: u64,
        /// Every terminal the core has.
        terminals: Vec<Terminal>,
    },
    /// A fresh subscription: the whole terminal set, and the cue to read the
    /// documents again (they are never part of a snapshot).
    Synced {
        /// The combined sequence number.
        seq: u64,
        /// Every terminal the core has.
        terminals: Vec<Terminal>,
    },
}

impl Notice {
    /// The sequence number, when the notice carries one.
    pub fn seq(&self) -> u64 {
        match self {
            Self::Event(change) => change.seq,
            Self::Heartbeat { seq } | Self::Missed { seq, .. } | Self::Synced { seq, .. } => *seq,
        }
    }
}

/// One `event` notification: which terminal, of what kind, with what data.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    /// The combined sequence number.
    pub seq: u64,
    /// The terminal it is about; `None` only for [[Change::kind]] `Doc`.
    pub terminal: Option<String>,
    /// The machine, for an event about another machine's terminal.
    pub machine: Option<String>,
    /// What kind of change it is.
    pub kind: ChangeKind,
    /// The notification's `data`, kept as it arrived so a caller can read a
    /// field this build does not type yet.
    pub data: Value,
}

/// The twelve event types, plus a case for a newer core's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeKind {
    /// A terminal was created; `data` is the whole [`Terminal`].
    Created,
    /// A kept terminal was restarted; `data` is the whole [`Terminal`].
    Restarted,
    /// A terminal was removed.
    Removed,
    /// Labels changed: `data` carries `labels`, or a whole `record`.
    Labels,
    /// The process finished: exit code, reason, signal.
    Finished,
    /// The core lost the terminal: its pid is gone.
    Lost,
    /// A status record changed.
    Status,
    /// The turn counter moved.
    Turn,
    /// The terminal title changed.
    Title,
    /// The terminal was resized.
    Size,
    /// The keep flag changed.
    Keep,
    /// The program in front changed.
    Foreground,
    /// A herdr-core document changed; its name is in `data`, its value is not.
    Doc,
    /// A kind this build does not know, kept verbatim.
    Other(String),
}

impl ChangeKind {
    /// The event type `word` names.
    pub fn parse(word: &str) -> Self {
        match word {
            "created" => Self::Created,
            "restarted" => Self::Restarted,
            "removed" => Self::Removed,
            "labels" => Self::Labels,
            "finished" => Self::Finished,
            "lost" => Self::Lost,
            "status" => Self::Status,
            "turn" => Self::Turn,
            "title" => Self::Title,
            "size" => Self::Size,
            "keep" => Self::Keep,
            "foreground" => Self::Foreground,
            "doc" => Self::Doc,
            other => Self::Other(other.to_string()),
        }
    }

    /// The word herdr-core uses.
    pub fn word(&self) -> &str {
        match self {
            Self::Created => "created",
            Self::Restarted => "restarted",
            Self::Removed => "removed",
            Self::Labels => "labels",
            Self::Finished => "finished",
            Self::Lost => "lost",
            Self::Status => "status",
            Self::Turn => "turn",
            Self::Title => "title",
            Self::Size => "size",
            Self::Keep => "keep",
            Self::Foreground => "foreground",
            Self::Doc => "doc",
            Self::Other(word) => word,
        }
    }
}

/// Read one JSON-RPC notification off the event channel.
///
/// Returns `None` for a message this crate has nothing to say about (an unknown
/// method), so a newer core never breaks the fold.
pub fn notice_from_message(message: &Value) -> Option<Notice> {
    let method = message.get("method")?.as_str()?;
    let params = message.get("params")?;
    let seq = params
        .get("seq")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    match method {
        "heartbeat" => Some(Notice::Heartbeat { seq }),
        "missed" => Some(Notice::Missed {
            seq,
            terminals: terminals_from(params.get("terminals")),
        }),
        "event" => Some(Notice::Event(Change {
            seq,
            terminal: params
                .get("terminal")
                .and_then(Value::as_str)
                .map(str::to_string),
            machine: params
                .get("machine")
                .and_then(Value::as_str)
                .map(str::to_string),
            kind: ChangeKind::parse(
                params
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            ),
            data: params.get("data").cloned().unwrap_or(Value::Null),
        })),
        _ => None,
    }
}

/// Read a `terminals` array, skipping an entry this build cannot read.
pub fn terminals_from(value: Option<&Value>) -> Vec<Terminal> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| serde_json::from_value(item.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use serde_json::json;

    use super::*;

    /// One `status` notification, verbatim from M7ef §2.4.
    const STATUS: &str = r#"{"method":"event","params":{"data":{"app":"shprd-probe","progress":10,"state":"working","reported":true,"records":{}},"seq":315,"terminal":"t23","type":"status"}}"#;
    /// One `turn` notification, verbatim.
    const TURN: &str = r#"{"method":"event","params":{"data":{"state":"blocked","turn":1},"seq":324,"terminal":"t23","type":"turn"}}"#;
    /// One `doc` notification, verbatim.
    const DOC: &str = r#"{"method":"event","params":{"data":{"name":"herdr-app","version":16},"seq":319,"type":"doc"}}"#;
    /// One `finished` notification, verbatim.
    const FINISHED: &str = r#"{"method":"event","params":{"data":{"exit_code":null,"finished_ms":1794618609228,"reason":"killed","signal":1},"seq":334,"terminal":"t23","type":"finished"}}"#;

    fn parse(line: &str) -> Notice {
        notice_from_message(&serde_json::from_str::<Value>(line).unwrap()).unwrap()
    }

    #[test]
    fn the_recorded_status_notification_parses() {
        let Notice::Event(change) = parse(STATUS) else {
            panic!("not an event");
        };
        assert_eq!(change.seq, 315);
        assert_eq!(change.terminal.as_deref(), Some("t23"));
        assert_eq!(change.kind, ChangeKind::Status);
        let status = Status::from_event_data(&change.data).unwrap();
        assert_eq!(status.state.get(), &AgentState::Working);
        assert_eq!(status.app.as_deref(), Some("shprd-probe"));
        assert_eq!(status.progress, Some(10));
    }

    #[test]
    fn a_status_wrapped_under_status_is_read_too() {
        let data = json!({
            "status": {"state": "blocked", "kind": "question", "msg": "What now?"},
            "reported": true,
            "records": {}
        });
        let status = Status::from_event_data(&data).unwrap();
        assert_eq!(status.state.get(), &AgentState::Blocked);
        assert_eq!(status.kind, Some(AskKind::Question));
        assert_eq!(status.msg.as_deref(), Some("What now?"));
    }

    #[test]
    fn a_cleared_status_is_no_status() {
        assert_eq!(Status::from_event_data(&json!({"status": null})), None);
    }

    #[test]
    fn the_recorded_turn_and_finished_notifications_parse() {
        let Notice::Event(turn) = parse(TURN) else {
            panic!("not an event");
        };
        assert_eq!(turn.kind, ChangeKind::Turn);
        assert_eq!(turn.data["turn"], 1);

        let Notice::Event(finished) = parse(FINISHED) else {
            panic!("not an event");
        };
        assert_eq!(finished.kind, ChangeKind::Finished);
        assert_eq!(finished.data["reason"], "killed");
    }

    #[test]
    fn the_recorded_doc_notification_parses() {
        let Notice::Event(doc) = parse(DOC) else {
            panic!("not an event");
        };
        assert_eq!(doc.kind, ChangeKind::Doc);
        assert_eq!(doc.terminal, None);
        assert_eq!(doc.data["name"], "herdr-app");
        assert_eq!(doc.data["version"], 16);
    }

    #[test]
    fn a_missed_notice_carries_the_whole_set() {
        let line = json!({
            "jsonrpc": "2.0",
            "method": "missed",
            "params": {"seq": 400, "terminals": [{"address": "t1", "labels": {}, "argv": [], "state": "live"}]}
        });
        let Notice::Missed { seq, terminals } = notice_from_message(&line).unwrap() else {
            panic!("not a missed notice");
        };
        assert_eq!(seq, 400);
        assert_eq!(terminals.len(), 1);
        assert_eq!(terminals[0].address, "t1");
        assert!(terminals[0].is_live());
    }

    #[test]
    fn a_heartbeat_carries_only_a_seq() {
        let line = json!({"method": "heartbeat", "params": {"seq": 7}});
        assert_eq!(
            notice_from_message(&line),
            Some(Notice::Heartbeat { seq: 7 })
        );
    }

    #[test]
    fn an_unknown_method_is_ignored() {
        let line = json!({"method": "something.new", "params": {"seq": 1}});
        assert_eq!(notice_from_message(&line), None);
    }

    #[test]
    fn an_unknown_event_type_is_kept_verbatim() {
        let line = json!({
            "method": "event",
            "params": {"seq": 2, "terminal": "t1", "type": "shinier", "data": {}}
        });
        let Notice::Event(change) = notice_from_message(&line).unwrap() else {
            panic!("not an event");
        };
        assert_eq!(change.kind, ChangeKind::Other("shinier".into()));
    }

    #[test]
    fn a_terminal_reads_the_fields_boundaries_care_about() {
        let terminal: Terminal = serde_json::from_value(json!({
            "address": "box/t7",
            "labels": {"herdr.space": "sp1", "herdr.tab": "worker"},
            "argv": ["claude"],
            "state": "live",
            "status": {"state": "blocked", "kind": "permission", "msg": "Approve?"},
            "records": {"sub-1": {"state": "working"}},
            "turn": 3,
            "something_new": {"ignored": true}
        }))
        .unwrap();
        assert_eq!(terminal.bare_address(), "t7");
        assert_eq!(terminal.space_label(), Some("sp1"));
        assert_eq!(terminal.tab_label(), Some("worker"));
        assert_eq!(terminal.agent_state(), AgentState::Blocked);
        assert!(terminal.is_live());
        assert_eq!(terminal.records["sub-1"].state.get(), &AgentState::Working);
        assert_eq!(terminal.turn, Some(3));
    }

    #[test]
    fn a_finished_terminal_with_no_status_is_done() {
        let terminal: Terminal =
            serde_json::from_value(json!({"address": "t9", "state": "finished"})).unwrap();
        assert!(!terminal.is_live());
        assert_eq!(terminal.agent_state(), AgentState::Done);
    }

    #[test]
    fn an_unknown_status_word_round_trips() {
        let status: Status = serde_json::from_value(json!({"state": "thinking"})).unwrap();
        assert_eq!(status.state.get(), &AgentState::Other("thinking".into()));
        let back = serde_json::to_value(&status).unwrap();
        assert_eq!(back["state"], "thinking");
    }

    #[test]
    fn clear_is_not_a_status_word() {
        assert!(serde_json::from_value::<Status>(json!({"state": "clear"})).is_err());
    }
}
