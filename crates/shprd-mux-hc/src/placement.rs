//! Where a terminal belongs: shprd's `herdr.space` / `herdr.tab` labels.
//!
//! Herdr desktop shows a terminal in the space and tab named by its labels —
//! a terminal started by the window carries them, and one started outside with
//! `hc run --label herdr.space=<id> --label herdr.tab=<label>` appears in that
//! space and tab too (M7ef §2.8, verified live). That is the whole placement
//! contract: shprd labels a terminal, and the window puts it where the task is.
//!
//! Herdr's own space ids are opaque, so shprd cannot make one up and label a
//! terminal with it: the space is *found* by the label it already has, and made
//! through the window (`space.new`) only when the layout side asks for that
//! (M10.3). What this module owns is the reading half — which space a layout
//! entry is, which tab a label names — so the two sides cannot disagree about
//! the spelling.
//!
//! Ids are compared bare: a remote machine's terminal address is
//! `machine/t7` and a space id is local, so `machine/t7` and `t7` are the same
//! terminal (M7ef §2.8, the caveat Shepherdr keeps in `service/main.ts:248`).

use serde_json::Value;

use crate::model::{Terminal, bare};

/// The label that puts a terminal in a space.
pub const SPACE_LABEL: &str = "herdr.space";

/// The label that puts a terminal in a tab of its space.
pub const TAB_LABEL: &str = "herdr.tab";

/// The label that names the shprd task (`shprd.*`, not `herdr.task`, which
/// Shepherdr reads as one of its own tasks) a terminal belongs to (`<slug>#<id>`).
pub const TASK_LABEL: &str = "shprd.task";

/// Where a task's terminals belong: the space and the tab.
///
/// This is shprd's own choice of placement, spelled as the two labels Herdr
/// desktop reads. Making a space is a matter of starting its first terminal with
/// these labels: the window then makes (or picks) the space and the tab for it
/// (M7ef §2.8). That is why nothing here invents a space id — the value of the
/// space label is simply the space a task works in, and Herdr adopts it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    /// The value of [`SPACE_LABEL`], bare.
    pub space: String,
    /// The value of [`TAB_LABEL`], bare.
    pub tab: String,
}

impl Placement {
    /// A placement in `space`, tab `tab`. Both are stored bare, because a
    /// label is never machine-qualified.
    pub fn new(space: impl AsRef<str>, tab: impl AsRef<str>) -> Self {
        Self {
            space: bare(space.as_ref()).to_string(),
            tab: bare(tab.as_ref()).to_string(),
        }
    }

    /// The labels a terminal must carry to land here.
    pub fn labels(&self) -> std::collections::BTreeMap<String, String> {
        let mut labels = std::collections::BTreeMap::new();
        labels.insert(SPACE_LABEL.to_string(), self.space.clone());
        labels.insert(TAB_LABEL.to_string(), self.tab.clone());
        labels
    }

    /// The same placement with the [`TASK_LABEL`] added.
    pub fn with_task(&self, task: impl AsRef<str>) -> std::collections::BTreeMap<String, String> {
        let mut labels = self.labels();
        labels.insert(TASK_LABEL.to_string(), task.as_ref().to_string());
        labels
    }
}

/// What a layout entry is: the space a task works in, or one of its tabs.
///
/// The window's own model (`herdr-app`) keeps spaces → tabs → a tree of
/// `row`/`column` nodes and `block` leaves whose content is a terminal or a mod
/// (M7ef §2.7). Only the parts shprd has to agree on are read here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Space {
    /// The window's space id.
    pub id: String,
    /// The space's name, which a person sees; empty when the window has none.
    pub name: String,
    /// The folder the space works in, when the window stored one.
    pub folder: Option<String>,
    /// The space's tabs, in the window's order.
    pub tabs: Vec<Tab>,
}

/// One tab of a space, and the terminals its panes hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tab {
    /// The window's tab id.
    pub id: String,
    /// The tab's name; empty when the window names it by its number.
    pub name: String,
    /// The terminal of every pane in the tab, in tree order.
    pub terminals: Vec<String>,
}

/// Read the `herdr-app` document's value into spaces.
///
/// Tolerant by design: unknown keys are ignored, a space or tab without an id
/// is skipped, and the pane tree is walked without assuming its depth. A
/// document this build cannot read yields no spaces rather than an error,
/// because the layout is the window's and only ever grows.
pub fn spaces_from_document(value: &Value) -> Vec<Space> {
    let Some(spaces) = value.get("spaces").and_then(Value::as_array) else {
        return Vec::new();
    };
    spaces.iter().filter_map(space_from_value).collect()
}

/// Read one space object.
fn space_from_value(value: &Value) -> Option<Space> {
    let id = value.get("id").and_then(Value::as_str)?.to_string();
    let tabs = value
        .get("tabs")
        .and_then(Value::as_array)
        .map(|tabs| tabs.iter().filter_map(tab_from_value).collect())
        .unwrap_or_default();
    Some(Space {
        id,
        name: value
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        folder: value
            .get("folder")
            .and_then(Value::as_str)
            .map(str::to_string),
        tabs,
    })
}

/// Read one tab object, collecting the terminals of every pane under its root.
fn tab_from_value(value: &Value) -> Option<Tab> {
    let id = value.get("id").and_then(Value::as_str)?.to_string();
    let mut terminals = Vec::new();
    if let Some(root) = value.get("root") {
        collect_terminals(root, &mut terminals);
    }
    Some(Tab {
        id,
        name: value
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        terminals,
    })
}

/// The desktop's chat mod: the pane the Chat │ Terminal switch puts in an agent
/// pane's place, with the parked agent's terminal in its `props.terminal`.
pub const CHAT_MOD: &str = "shprd-chat";

/// The terminal a chat pane shows, when `content` is one.
///
/// The switch parks the agent's terminal (it keeps running, without a pane) and
/// opens the chat mod in the slot, so the window's layout no longer lists the
/// terminal as a pane. It is still the tab's agent, though: a chat line, a
/// prompt and the janitor must find it where it was, so the chat's block counts
/// as the terminal it shows.
fn parked_terminal(content: &Value) -> Option<&str> {
    if content.get("mod").and_then(Value::as_str) != Some(CHAT_MOD) {
        return None;
    }
    content
        .get("props")
        .and_then(|props| props.get("terminal"))
        .and_then(Value::as_str)
        .filter(|terminal| !terminal.is_empty())
}

/// Walk a pane tree, appending the terminal of every block that holds one, or
/// that shows one parked behind a chat pane.
fn collect_terminals(node: &Value, out: &mut Vec<String>) {
    if let Some(terminal) = node.get("content").and_then(|content| {
        content
            .get("terminal")
            .and_then(Value::as_str)
            .or_else(|| parked_terminal(content))
    }) {
        out.push(terminal.to_string());
        return;
    }
    if let Some(children) = node.get("children").and_then(Value::as_array) {
        for child in children {
            collect_terminals(child, out);
        }
    }
}

/// Find the space whose id matches `id`, comparing bare ids.
pub fn find_space<'a>(spaces: &'a [Space], id: &str) -> Option<&'a Space> {
    let wanted = bare(id);
    spaces.iter().find(|space| bare(&space.id) == wanted)
}

/// Find the tab of `space` whose label names it.
///
/// A tab label is whatever shprd wrote — a role name (`worker`, `reviewer`) or
/// a task id — and it matches the window's tab by id first (shprd may have been
/// given one) and by name second, because the window names a tab from what was
/// typed for it. Both comparisons are bare and case-insensitive, since the
/// window's own naming is a person's.
pub fn find_tab<'a>(space: &'a Space, label: &str) -> Option<&'a Tab> {
    let wanted = bare(label);
    space
        .tabs
        .iter()
        .find(|tab| bare(&tab.id) == wanted)
        .or_else(|| {
            space
                .tabs
                .iter()
                .find(|tab| !tab.name.is_empty() && tab.name.eq_ignore_ascii_case(wanted))
        })
}

/// Which terminals carry `label` as their [`TAB_LABEL`].
pub fn terminals_in_tab<'a>(terminals: &'a [Terminal], label: &str) -> Vec<&'a Terminal> {
    let wanted = bare(label);
    terminals
        .iter()
        .filter(|terminal| terminal.tab_label().map(bare) == Some(wanted))
        .collect()
}

/// Which terminals carry `space` as their [`SPACE_LABEL`].
pub fn terminals_in_space<'a>(terminals: &'a [Terminal], space: &str) -> Vec<&'a Terminal> {
    let wanted = bare(space);
    terminals
        .iter()
        .filter(|terminal| terminal.space_label().map(bare) == Some(wanted))
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use serde_json::json;

    use super::*;

    /// The probe space's document value, as M7ef §2.7 recorded it.
    fn document() -> Value {
        json!({
            "spaces": [{
                "id": "spc3f2a",
                "name": "shprd probe",
                "folder": "/tmp/shs/tmp",
                "tabs": [
                    {
                        "id": "tbroot",
                        "name": "",
                        "root": {"type": "row", "id": "tbroot", "children": [
                            {"type": "block", "id": "p1", "content": {"terminal": "t1y"},
                             "size": {"weight": 1.0}}
                        ]}
                    },
                    {
                        "id": "tbprobe",
                        "name": "probe",
                        "root": {"type": "row", "id": "tbprobe", "children": [
                            {"type": "column", "id": "s1", "size": {"weight": 0.5}, "children": [
                                {"type": "block", "id": "p2", "content": {"mod": "shprd-probe"}},
                                {"type": "block", "id": "p3", "content": {"mod": "workspace-status", "props": {"compact": true}}}
                            ]},
                            {"type": "block", "id": "p4", "content": {"terminal": "t20"}, "size": {"weight": 0.5}}
                        ]}
                    }
                ]
            }]
        })
    }

    #[test]
    fn the_recorded_document_reads_into_spaces_and_tabs() {
        let spaces = spaces_from_document(&document());
        assert_eq!(spaces.len(), 1);
        let space = &spaces[0];
        assert_eq!(space.id, "spc3f2a");
        assert_eq!(space.name, "shprd probe");
        assert_eq!(space.folder.as_deref(), Some("/tmp/shs/tmp"));
        assert_eq!(space.tabs.len(), 2);
        assert_eq!(space.tabs[0].terminals, vec!["t1y"]);
        // The mod panes carry no terminal, so only the terminal block counts.
        assert_eq!(space.tabs[1].terminals, vec!["t20"]);
        assert_eq!(space.tabs[1].name, "probe");
    }

    #[test]
    fn an_agent_parked_behind_a_chat_pane_is_still_the_tabs_terminal() {
        // The Chat │ Terminal switch parks the agent and opens the chat mod in its
        // slot; the tab's root terminal must stay the agent, or a chat line, a
        // prompt and the janitor would find a tab with nothing in it.
        let document = json!({"spaces": [{
            "id": "spc1", "name": "task", "tabs": [{
                "id": "tb1", "name": "worker",
                "root": {"type": "row", "id": "tb1", "children": [
                    {"type": "block", "id": "p1", "content": {"mod": "shprd-chat",
                        "props": {"space": "T1", "terminal": "t9"}}},
                    {"type": "block", "id": "p2", "content": {"terminal": "t10"}}
                ]}
            }]
        }]});
        let spaces = spaces_from_document(&document);
        assert_eq!(spaces[0].tabs[0].terminals, vec!["t9", "t10"]);
        // Another mod, or a chat with no terminal named, is not a terminal.
        let others = json!({"spaces": [{"id": "s", "tabs": [{"id": "t", "root": {"type": "row", "children": [
            {"type": "block", "content": {"mod": "shprd-tasks", "props": {"terminal": "t1"}}},
            {"type": "block", "content": {"mod": "shprd-chat", "props": {"space": "coordinator"}}},
            {"type": "block", "content": {"mod": "shprd-chat", "props": {"terminal": ""}}}
        ]}}]}]});
        assert!(
            spaces_from_document(&others)[0].tabs[0]
                .terminals
                .is_empty()
        );
    }

    #[test]
    fn a_space_is_found_by_a_bare_id() {
        let spaces = spaces_from_document(&document());
        assert!(find_space(&spaces, "spc3f2a").is_some());
        assert!(find_space(&spaces, "box/spc3f2a").is_some());
        assert!(find_space(&spaces, "spcother").is_none());
    }

    #[test]
    fn a_tab_is_found_by_its_label() {
        let spaces = spaces_from_document(&document());
        let space = &spaces[0];
        // A label shprd wrote matches the window's tab id first …
        assert_eq!(find_tab(space, "tbprobe").unwrap().id, "tbprobe");
        // … and the tab's own name second, case-insensitively.
        assert_eq!(find_tab(space, "Probe").unwrap().id, "tbprobe");
        assert!(find_tab(space, "reviewer").is_none());
    }

    #[test]
    fn an_empty_document_has_no_spaces() {
        assert!(spaces_from_document(&json!({"spaces": []})).is_empty());
        assert!(spaces_from_document(&json!(null)).is_empty());
        assert!(spaces_from_document(&json!({"something": "else"})).is_empty());
    }

    #[test]
    fn terminals_are_grouped_by_their_labels() {
        let terminals: Vec<Terminal> = serde_json::from_value(json!([
            {"address": "t1", "labels": {"herdr.space": "sp1", "herdr.tab": "worker"}},
            {"address": "t2", "labels": {"herdr.space": "sp1", "herdr.tab": "reviewer"}},
            {"address": "box/t3", "labels": {"herdr.space": "sp1", "herdr.tab": "worker"}},
            {"address": "t4", "labels": {}}
        ]))
        .unwrap();
        let workers = terminals_in_tab(&terminals, "worker");
        assert_eq!(workers.len(), 2);
        assert_eq!(workers[1].address, "box/t3");
        assert_eq!(terminals_in_tab(&terminals, "Worker").len(), 0);
        assert_eq!(terminals_in_space(&terminals, "sp1").len(), 3);
        assert_eq!(terminals_in_space(&terminals, "sp2").len(), 0);
    }
}
