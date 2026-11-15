//! The fold: what shprd believes about a herdr-core from the stream it sent.
//!
//! Three things are folded here, and only the first is in the stream:
//!
//! - **Terminals** — the twelve event types of M7ef §2.4, applied to a map by
//!   address, exactly as Shepherdr's `service/hub.ts:65-91` applies them.
//! - **Spaces and tabs** — these are *not* in the event stream at all. The
//!   window's layout lives in the `herdr-app` document, and a `doc` event says
//!   only which document changed and at what version, never its value (M7ef
//!   §2.4, §2.7). So the fold records "read `herdr-app` again" and takes the
//!   value when the caller has fetched it.
//! - **A sequence watermark** — one `seq` covers terminals and documents, which
//!   is what makes `missed` usable: after a snapshot, every event at or below
//!   it is stale and is dropped.
//!
//! The fold is pure: it never reads anything. On a `missed` notice, a fresh
//! subscription or a reconnect it says [`Update::reset`] and names the documents
//! to read, and the caller does the reading. Trusting the stream after a
//! `missed` is the one bug this shape makes impossible.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::model::{Change, ChangeKind, Notice, Status, Terminal};
use crate::placement::{self, Space, Tab};

/// The document the window keeps its whole layout in; read it, never write it.
pub const LAYOUT_DOCUMENT: &str = "herdr-app";

/// What the fold knows.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct State {
    /// The combined sequence number of the last thing applied.
    pub seq: u64,
    /// Every terminal the core has, by address (`t7`, or `machine/t7`).
    pub terminals: BTreeMap<String, Terminal>,
    /// The window's spaces, from the `herdr-app` document.
    pub spaces: Vec<Space>,
    /// The version last seen of each document that changed, by name.
    pub doc_versions: BTreeMap<String, u64>,
    /// The documents still to be read: a `doc` event names one, and a reset
    /// names all of them, and neither carries a value.
    pub pending: BTreeSet<String>,
}

impl State {
    /// Whether something is outstanding: the layout is being re-read, so a
    /// caller should not present it as settled yet.
    pub fn is_stale(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Every terminal, in address order.
    pub fn terminals(&self) -> impl Iterator<Item = &Terminal> {
        self.terminals.values()
    }

    /// One terminal by address, comparing bare addresses.
    pub fn terminal(&self, address: &str) -> Option<&Terminal> {
        let wanted = crate::model::bare(address);
        self.terminals
            .values()
            .find(|terminal| terminal.bare_address() == wanted)
    }

    /// The space with `id`, comparing bare ids.
    pub fn space(&self, id: &str) -> Option<&Space> {
        placement::find_space(&self.spaces, id)
    }

    /// The tab of `space` whose label names it.
    pub fn tab(&self, space: &str, label: &str) -> Option<&Tab> {
        self.space(space)
            .and_then(|space| placement::find_tab(space, label))
    }

    /// The terminals carrying `space` and `tab` as labels.
    pub fn placed_in<'a>(&'a self, space: &str, tab: &str) -> Vec<&'a Terminal> {
        let space = crate::model::bare(space);
        let tab = crate::model::bare(tab);
        self.terminals
            .values()
            .filter(|terminal| {
                terminal.space_label().map(crate::model::bare) == Some(space)
                    && terminal.tab_label().map(crate::model::bare) == Some(tab)
            })
            .collect()
    }
}

/// What the caller should do after a notice.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Update {
    /// The model was replaced wholesale: read every document again before
    /// believing the stream, because documents are never in a snapshot.
    pub reset: bool,
    /// The documents to read again, by name. A `doc` event carries only the
    /// name and version; a reset names every document the fold has seen.
    pub documents: Vec<String>,
    /// Terminals whose record changed, by address.
    pub changed: Vec<String>,
}

impl Update {
    /// Whether anything at all happened.
    pub fn is_empty(&self) -> bool {
        !self.reset && self.documents.is_empty() && self.changed.is_empty()
    }
}

/// The model of one core, built only from what the core sent.
#[derive(Debug, Clone, Default)]
pub struct Fold {
    state: State,
}

impl Fold {
    /// An empty fold, as before the first subscription.
    pub fn new() -> Self {
        Self::default()
    }

    /// What the fold currently believes.
    pub fn state(&self) -> &State {
        &self.state
    }

    /// Apply one notice.
    pub fn apply(&mut self, notice: Notice) -> Update {
        match notice {
            Notice::Heartbeat { seq } => {
                // Liveness only: a heartbeat proves the connection, and moves
                // the watermark so a later snapshot can be compared against it.
                self.state.seq = self.state.seq.max(seq);
                Update::default()
            }
            Notice::Missed { seq, terminals } => self.replace(seq, terminals, true),
            Notice::Synced { seq, terminals } => self.replace(seq, terminals, true),
            Notice::Event(change) => self.change(change),
        }
    }

    /// Replace the terminal set with a snapshot and ask for the documents.
    fn replace(&mut self, seq: u64, terminals: Vec<Terminal>, reset: bool) -> Update {
        let before = std::mem::take(&mut self.state.terminals);
        self.state.terminals = terminals
            .into_iter()
            .map(|terminal| (terminal.address.clone(), terminal))
            .collect();
        self.state.seq = self.state.seq.max(seq);
        if reset {
            self.state.spaces.clear();
            self.state.pending = self.documents().into_iter().collect();
        }
        let mut changed: Vec<String> = self
            .state
            .terminals
            .iter()
            .filter(|(address, terminal)| before.get(*address) != Some(terminal))
            .map(|(address, _)| address.clone())
            .collect();
        changed.extend(
            before
                .keys()
                .filter(|address| !self.state.terminals.contains_key(*address))
                .cloned(),
        );
        Update {
            reset,
            documents: if reset {
                self.state.pending.iter().cloned().collect()
            } else {
                Vec::new()
            },
            changed,
        }
    }

    /// Every document this fold has read or been told about.
    fn documents(&self) -> Vec<String> {
        let mut names: Vec<String> = self.state.doc_versions.keys().cloned().collect();
        if !names.iter().any(|name| name == LAYOUT_DOCUMENT) {
            names.push(LAYOUT_DOCUMENT.to_string());
        }
        names
    }

    /// Apply one `event` notification.
    fn change(&mut self, change: Change) -> Update {
        if change.seq <= self.state.seq && self.state.seq != 0 {
            // Older than the snapshot this fold already replaced itself with.
            return Update::default();
        }
        self.state.seq = self.state.seq.max(change.seq);
        let mut update = Update::default();

        if change.kind == ChangeKind::Doc {
            let name = change
                .data
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let version = change.data.get("version").and_then(Value::as_u64);
            if !name.is_empty() {
                if let Some(version) = version {
                    let previous = self.state.doc_versions.insert(name.clone(), version);
                    if previous == Some(version) {
                        // Same version twice: nothing to re-read.
                        self.state.pending.remove(&name);
                        return update;
                    }
                }
                self.state.pending.insert(name.clone());
                update.documents.push(name);
            }
            return update;
        }

        let Some(address) = change.terminal.clone() else {
            return update;
        };
        match change.kind {
            ChangeKind::Created | ChangeKind::Restarted => {
                if let Ok(terminal) = serde_json::from_value::<Terminal>(change.data.clone()) {
                    self.state
                        .terminals
                        .insert(terminal.address.clone(), terminal);
                    update.changed.push(address.clone());
                }
            }
            ChangeKind::Removed => {
                self.state.terminals.remove(&address);
                update.changed.push(address.clone());
            }
            ChangeKind::Labels => {
                let Some(terminal) = self.state.terminals.get_mut(&address) else {
                    return update;
                };
                if let Some(record) = change.data.get("record")
                    && let Ok(fresh) = serde_json::from_value::<Terminal>(record.clone())
                {
                    *terminal = fresh;
                    update.changed.push(address.clone());
                    return update;
                }
                if let Some(labels) = change.data.get("labels").and_then(Value::as_object) {
                    for (key, value) in labels {
                        match value.as_str() {
                            Some(value) => {
                                terminal.labels.insert(key.clone(), value.to_string());
                            }
                            None => {
                                terminal.labels.remove(key);
                            }
                        }
                    }
                    update.changed.push(address);
                }
            }
            ChangeKind::Status => {
                let Some(terminal) = self.state.terminals.get_mut(&address) else {
                    return update;
                };
                let status = Status::from_event_data(&change.data);
                // A shape this build does not know leaves the status alone.
                if status.is_some() || change.data.get("status").is_some_and(Value::is_null) {
                    terminal.status = status;
                    update.changed.push(address.clone());
                }
                let pushed_status = !update.changed.is_empty();
                if let Some(records) = change.data.get("records").and_then(Value::as_object) {
                    terminal.records = records
                        .iter()
                        .filter_map(|(id, value)| {
                            serde_json::from_value::<Status>(value.clone())
                                .ok()
                                .map(|status| (id.clone(), status))
                        })
                        .collect();
                    if !pushed_status {
                        update.changed.push(address.clone());
                    }
                }
                if let Some(reported) = change.data.get("reported").and_then(Value::as_bool) {
                    terminal.reported = Some(reported);
                }
            }
            ChangeKind::Turn => {
                let Some(terminal) = self.state.terminals.get_mut(&address) else {
                    return update;
                };
                if let Some(turn) = change.data.get("turn").and_then(Value::as_u64) {
                    terminal.turn = Some(turn);
                }
                // A turn event carries the root word it landed on, when it does.
                if let Some(state) = change.data.get("state").and_then(Value::as_str)
                    && let Some(existing) = terminal.status.as_mut()
                    && let Some(word) = crate::model::AgentState::parse(state)
                {
                    existing.state = crate::model::AgentStateWord::new(word);
                }
                update.changed.push(address);
            }
            ChangeKind::Finished => {
                let Some(terminal) = self.state.terminals.get_mut(&address) else {
                    return update;
                };
                terminal.state = Some("finished".to_string());
                terminal.reason = change
                    .data
                    .get("reason")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                terminal.exit_code = change.data.get("exit_code").and_then(Value::as_i64);
                terminal.signal = change.data.get("signal").and_then(Value::as_i64);
                terminal.finished_ms = change.data.get("finished_ms").and_then(Value::as_u64);
                update.changed.push(address);
            }
            ChangeKind::Lost => {
                let Some(terminal) = self.state.terminals.get_mut(&address) else {
                    return update;
                };
                terminal.pid = None;
                update.changed.push(address);
            }
            ChangeKind::Size => {
                let Some(terminal) = self.state.terminals.get_mut(&address) else {
                    return update;
                };
                terminal.cols = change
                    .data
                    .get("cols")
                    .and_then(Value::as_u64)
                    .and_then(|cols| u32::try_from(cols).ok());
                terminal.rows = change
                    .data
                    .get("rows")
                    .and_then(Value::as_u64)
                    .and_then(|rows| u32::try_from(rows).ok());
                update.changed.push(address);
            }
            ChangeKind::Title => {
                let Some(terminal) = self.state.terminals.get_mut(&address) else {
                    return update;
                };
                terminal.title = change
                    .data
                    .get("title")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                update.changed.push(address);
            }
            ChangeKind::Keep => {
                let Some(terminal) = self.state.terminals.get_mut(&address) else {
                    return update;
                };
                terminal.keep = change.data.get("keep").and_then(Value::as_bool);
                update.changed.push(address);
            }
            ChangeKind::Foreground => {
                let Some(terminal) = self.state.terminals.get_mut(&address) else {
                    return update;
                };
                terminal.foreground = if change.data.is_null() {
                    None
                } else {
                    serde_json::from_value(change.data.clone()).ok()
                };
                update.changed.push(address);
            }
            ChangeKind::Doc | ChangeKind::Other(_) => {}
        }
        update
    }

    /// Take a document the caller has read.
    ///
    /// The layout document becomes the fold's spaces; any other is only
    /// remembered by version, because what a document means is its owner's
    /// business.
    pub fn set_document(&mut self, name: &str, value: &Value, version: u64) {
        self.state.doc_versions.insert(name.to_string(), version);
        self.state.pending.remove(name);
        if name == LAYOUT_DOCUMENT {
            self.state.spaces = placement::spaces_from_document(value);
        }
    }

    /// Forget everything: the connection is gone, so nothing it said is trusted
    /// until a fresh subscription answers again.
    pub fn reset(&mut self) -> Update {
        self.state.terminals.clear();
        self.state.spaces.clear();
        self.state.seq = 0;
        self.state.pending = self.documents().into_iter().collect();
        Update {
            reset: true,
            documents: self.state.pending.iter().cloned().collect(),
            changed: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use serde_json::json;

    use super::*;

    fn terminal(address: &str) -> Terminal {
        serde_json::from_value(json!({
            "address": address,
            "labels": {"herdr.space": "sp1", "herdr.tab": "worker"},
            "argv": ["claude"],
            "state": "live"
        }))
        .unwrap()
    }

    fn snapshot(seq: u64, addresses: &[&str]) -> Notice {
        Notice::Synced {
            seq,
            terminals: addresses.iter().map(|address| terminal(address)).collect(),
        }
    }

    /// The four `event` lines M7ef recorded, as notices.
    fn recorded() -> Vec<Notice> {
        [
            r#"{"method":"event","params":{"data":{"app":"shprd-probe","progress":10,"state":"working","reported":true,"records":{}},"seq":315,"terminal":"t23","type":"status"}}"#,
            r#"{"method":"event","params":{"data":{"state":"blocked","turn":1},"seq":324,"terminal":"t23","type":"turn"}}"#,
            r#"{"method":"event","params":{"data":{"name":"herdr-app","version":16},"seq":319,"type":"doc"}}"#,
            r#"{"method":"event","params":{"data":{"exit_code":null,"finished_ms":1794618609228,"reason":"killed","signal":1},"seq":334,"terminal":"t23","type":"finished"}}"#,
        ]
        .iter()
        .map(|line| {
            let value: Value = serde_json::from_str(line).unwrap();
            crate::model::notice_from_message(&value).unwrap()
        })
        .collect::<Vec<Notice>>()
    }

    #[test]
    fn a_snapshot_replaces_the_set_and_asks_for_the_documents() {
        let mut fold = Fold::new();
        let update = fold.apply(snapshot(10, &["t1", "t2"]));
        assert!(update.reset);
        assert!(update.documents.iter().any(|name| name == LAYOUT_DOCUMENT));
        assert_eq!(fold.state().terminals.len(), 2);
        assert_eq!(fold.state().seq, 10);
        assert!(fold.state().is_stale());
    }

    #[test]
    fn the_recorded_events_fold_into_one_terminal() {
        let mut fold = Fold::new();
        fold.apply(Notice::Synced {
            seq: 300,
            terminals: vec![terminal("t23")],
        });
        let mut documents = Vec::new();
        // The stream delivers them in seq order; M7ef quoted them grouped by
        // terminal, so sort before folding.
        let mut notices = recorded();
        notices.sort_by_key(Notice::seq);
        for notice in notices {
            documents.extend(fold.apply(notice).documents);
        }
        let terminal = fold.state().terminal("t23").unwrap();
        assert_eq!(
            terminal.status.as_ref().unwrap().state.get(),
            &crate::model::AgentState::Blocked
        );
        assert_eq!(terminal.turn, Some(1));
        assert!(!terminal.is_live());
        assert_eq!(terminal.reason.as_deref(), Some("killed"));
        assert_eq!(terminal.finished_ms, Some(1794618609228));
        // Only the `doc` event asks for a document.
        assert_eq!(documents, vec![LAYOUT_DOCUMENT.to_string()]);
        assert_eq!(fold.state().seq, 334);
    }

    #[test]
    fn a_missed_notice_replaces_the_set_instead_of_trusting_the_stream() {
        let mut fold = Fold::new();
        fold.apply(snapshot(10, &["t1", "t2"]));
        fold.set_document(LAYOUT_DOCUMENT, &json!({"spaces": []}), 3);

        let update = fold.apply(Notice::Missed {
            seq: 40,
            terminals: vec![terminal("t2"), terminal("t3")],
        });
        assert!(update.reset);
        assert_eq!(update.documents, vec![LAYOUT_DOCUMENT.to_string()]);
        assert_eq!(fold.state().seq, 40);
        let addresses: Vec<&str> = fold
            .state()
            .terminals()
            .map(|terminal| terminal.address.as_str())
            .collect();
        assert_eq!(addresses, vec!["t2", "t3"]);
        // t1 vanished and t3 is new; t2 is unchanged.
        assert!(update.changed.contains(&"t1".to_string()));
        assert!(update.changed.contains(&"t3".to_string()));
        assert!(!update.changed.contains(&"t2".to_string()));
    }

    #[test]
    fn an_event_older_than_the_snapshot_is_dropped() {
        let mut fold = Fold::new();
        fold.apply(Notice::Synced {
            seq: 100,
            terminals: vec![terminal("t1")],
        });
        let stale = Change {
            seq: 99,
            terminal: Some("t1".to_string()),
            machine: None,
            kind: ChangeKind::Removed,
            data: json!({}),
        };
        let update = fold.apply(Notice::Event(stale));
        assert!(update.is_empty());
        assert!(fold.state().terminal("t1").is_some());
    }

    #[test]
    fn a_doc_event_records_its_version_and_asks_for_the_document() {
        let mut fold = Fold::new();
        fold.apply(snapshot(1, &[]));
        let change = Change {
            seq: 5,
            terminal: None,
            machine: None,
            kind: ChangeKind::Doc,
            data: json!({"name": "herdr-app", "version": 16}),
        };
        let update = fold.apply(Notice::Event(change.clone()));
        assert_eq!(update.documents, vec![LAYOUT_DOCUMENT.to_string()]);
        assert_eq!(fold.state().doc_versions[LAYOUT_DOCUMENT], 16);
        // The same version again is not worth a second read.
        assert!(fold.apply(Notice::Event(change)).documents.is_empty());
    }

    #[test]
    fn reading_the_layout_document_fills_in_spaces_tabs_and_panes() {
        let mut fold = Fold::new();
        fold.apply(snapshot(1, &[]));
        fold.set_document(
            LAYOUT_DOCUMENT,
            &json!({
                "spaces": [{
                    "id": "sp1",
                    "name": "shprd",
                    "tabs": [
                        {"id": "tb1", "name": "worker", "root": {"type": "block", "content": {"terminal": "t1"}}},
                        {"id": "tb2", "name": "reviewer", "root": {"type": "block", "content": {"terminal": "t2"}}}
                    ]
                }]
            }),
            4,
        );
        assert!(!fold.state().is_stale());
        assert_eq!(fold.state().spaces.len(), 1);
        assert_eq!(fold.state().tab("sp1", "worker").unwrap().id, "tb1");
        assert_eq!(fold.state().tab("sp1", "reviewer").unwrap().id, "tb2");
        assert!(fold.state().tab("sp1", "nobody").is_none());
    }

    #[test]
    fn labels_events_move_a_terminal_between_tabs() {
        let mut fold = Fold::new();
        fold.apply(Notice::Synced {
            seq: 1,
            terminals: vec![terminal("t1")],
        });
        let change = Change {
            seq: 2,
            terminal: Some("t1".to_string()),
            machine: None,
            kind: ChangeKind::Labels,
            data: json!({"labels": {"herdr.tab": "reviewer"}}),
        };
        fold.apply(Notice::Event(change));
        let terminal = fold.state().terminal("t1").unwrap();
        assert_eq!(terminal.tab_label(), Some("reviewer"));
        assert_eq!(terminal.space_label(), Some("sp1"));
    }

    #[test]
    fn a_labels_event_with_a_whole_record_replaces_it() {
        let mut fold = Fold::new();
        fold.apply(Notice::Synced {
            seq: 1,
            terminals: vec![terminal("t1")],
        });
        let change = Change {
            seq: 2,
            terminal: Some("t1".to_string()),
            machine: None,
            kind: ChangeKind::Labels,
            data: json!({"record": {"address": "t1", "labels": {"herdr.space": "sp9"}, "state": "live"}}),
        };
        fold.apply(Notice::Event(change));
        assert_eq!(
            fold.state().terminal("t1").unwrap().space_label(),
            Some("sp9")
        );
    }

    #[test]
    fn a_terminal_is_found_by_a_bare_address() {
        let mut fold = Fold::new();
        fold.apply(Notice::Synced {
            seq: 1,
            terminals: vec![terminal("box/t7")],
        });
        assert!(fold.state().terminal("t7").is_some());
        assert!(fold.state().terminal("box/t7").is_some());
    }

    #[test]
    fn a_reset_forgets_everything_and_asks_for_the_documents() {
        let mut fold = Fold::new();
        fold.apply(snapshot(10, &["t1"]));
        let update = fold.reset();
        assert!(update.reset);
        assert_eq!(update.documents, vec![LAYOUT_DOCUMENT.to_string()]);
        assert!(fold.state().terminals.is_empty());
        assert!(fold.state().spaces.is_empty());
        assert!(fold.state().is_stale());
    }

    #[test]
    fn an_unknown_event_type_changes_nothing() {
        let mut fold = Fold::new();
        fold.apply(snapshot(1, &["t1"]));
        let change = Change {
            seq: 2,
            terminal: Some("t1".to_string()),
            machine: None,
            kind: ChangeKind::Other("shinier".into()),
            data: json!({"brightness": 11}),
        };
        assert!(fold.apply(Notice::Event(change)).is_empty());
    }
}
