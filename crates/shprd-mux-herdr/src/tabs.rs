//! Tabs: one agent's terminal inside a workspace.
//!
//! A herdr tab is created with `--label`, which is the title shprd owns, and
//! carries one root pane that every later split hangs off. `tab list` does not
//! report that pane, so listing tabs reads `pane list` as well and pairs each
//! pane with its tab; a tab with no pane at all is skipped, because a
//! [`shprd_mux::Tab`] without a root pane cannot be split.

use serde_json::Value;
use shprd_mux::{MuxError, PaneId, Result, Tab, TabId, WorkspaceId};
use std::collections::HashMap;
use std::path::Path;

use crate::HerdrMux;

impl HerdrMux {
    /// Every tab of `workspace`, in herdr's display order, each with the
    /// lowest-numbered pane of that tab as its root.
    pub(crate) fn tabs_in(&self, workspace: &WorkspaceId) -> Result<Vec<Tab>> {
        let id = workspace.as_str();
        let tabs = self.call_as(
            "workspace",
            id,
            "list tabs",
            &["tab", "list", "--workspace", id],
        )?;
        let panes = self.call("list panes", &["pane", "list", "--workspace", id])?;
        match tabs {
            Some(tabs) => Self::tabs_from(&tabs, &panes),
            None => Err(MuxError::command(
                "list tabs",
                "herdr answered without a tab list",
            )),
        }
    }

    /// Pair a `tab_list` result with a `pane_list` result.
    pub(crate) fn tabs_from(tabs: &Value, panes: &Value) -> Result<Vec<Tab>> {
        let entries = tabs
            .get("tabs")
            .and_then(Value::as_array)
            .ok_or_else(|| MuxError::command("list tabs", "herdr answered without a tab list"))?;
        let roots = Self::root_panes_from(panes)?;
        Ok(entries
            .iter()
            .filter_map(|entry| {
                let tab_id = entry.get("tab_id")?.as_str()?;
                Some(Tab {
                    id: TabId::new(tab_id),
                    title: entry.get("label")?.as_str()?.to_string(),
                    root_pane: PaneId::new(roots.get(tab_id)?),
                })
            })
            .collect())
    }

    /// The first pane herdr lists for each tab.
    pub(crate) fn root_panes_from(panes: &Value) -> Result<HashMap<String, String>> {
        let entries = panes
            .get("panes")
            .and_then(Value::as_array)
            .ok_or_else(|| MuxError::command("list panes", "herdr answered without a pane list"))?;
        let mut roots = HashMap::new();
        for entry in entries {
            let (Some(tab), Some(pane)) = (
                entry.get("tab_id").and_then(Value::as_str),
                entry.get("pane_id").and_then(Value::as_str),
            ) else {
                continue;
            };
            roots
                .entry(tab.to_string())
                .or_insert_with(|| pane.to_string());
        }
        Ok(roots)
    }
}

impl HerdrMux {
    /// Create a tab labelled `title` in `workspace`, root pane in `cwd`.
    pub(crate) fn new_tab(&self, workspace: &WorkspaceId, title: &str, cwd: &Path) -> Result<Tab> {
        let cwd = cwd.to_string_lossy().into_owned();
        let result = self.call(
            "create tab",
            &[
                "tab",
                "create",
                "--workspace",
                workspace.as_str(),
                "--cwd",
                &cwd,
                "--label",
                title,
                "--no-focus",
            ],
        )?;
        let tab = result
            .get("tab")
            .ok_or_else(|| MuxError::command("create tab", "herdr answered without a tab"))?;
        let id = tab
            .get("tab_id")
            .and_then(Value::as_str)
            .ok_or_else(|| MuxError::command("create tab", "herdr answered without a tab id"))?;
        let root = result
            .get("root_pane")
            .and_then(|pane| pane.get("pane_id"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                MuxError::command("create tab", "herdr answered without the tab's root pane")
            })?;
        Ok(Tab {
            id: TabId::new(id),
            title: tab
                .get("label")
                .and_then(Value::as_str)
                .unwrap_or(title)
                .to_string(),
            root_pane: PaneId::new(root),
        })
    }

    /// Retitle a tab; renaming to the current title is a no-op for herdr and
    /// still succeeds.
    pub(crate) fn retitle_tab(&self, tab: &TabId, title: &str) -> Result<()> {
        self.call_as(
            "tab",
            tab.as_str(),
            "rename tab",
            &["tab", "rename", tab.as_str(), title],
        )?;
        Ok(())
    }

    /// Select a tab so its panes are the visible ones.
    pub(crate) fn select_tab(&self, tab: &TabId) -> Result<()> {
        self.call_as(
            "tab",
            tab.as_str(),
            "focus tab",
            &["tab", "focus", tab.as_str()],
        )?;
        Ok(())
    }

    /// Close a tab and its panes.
    pub(crate) fn kill_tab(&self, tab: &TabId) -> Result<()> {
        self.call_as(
            "tab",
            tab.as_str(),
            "close tab",
            &["tab", "close", tab.as_str()],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const TABS: &str = include_str!("../tests/fixtures/tab_list.json");
    const PANES: &str = include_str!("../tests/fixtures/pane_list.json");
    const CREATED: &str = include_str!("../tests/fixtures/tab_create.json");

    fn result(text: &str) -> Value {
        serde_json::from_str::<Value>(text).unwrap()["result"].clone()
    }

    #[test]
    fn every_tab_gets_the_first_pane_of_that_tab_as_its_root() {
        let tabs = HerdrMux::tabs_from(&result(TABS), &result(PANES)).unwrap();
        let pairs: Vec<(&str, &str, &str)> = tabs
            .iter()
            .map(|tab| (tab.id.as_str(), tab.title.as_str(), tab.root_pane.as_str()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("w1:t1", "1", "w1:p1"),
                ("w1:t2", "dev renamed", "w1:p2"),
                ("w1:t3", "dev", "w1:p4"),
            ]
        );
    }

    #[test]
    fn a_tab_without_a_listed_pane_is_skipped_not_invented() {
        let tabs = result(TABS);
        let tabs = json!({"tabs": [tabs["tabs"][0].clone()]});
        let panes = json!({"panes": []});
        assert!(HerdrMux::tabs_from(&tabs, &panes).unwrap().is_empty());
    }

    #[test]
    fn the_root_pane_of_a_tab_is_its_lowest_listed_pane() {
        let panes = json!({"panes": [
            {"tab_id": "w1:t1", "pane_id": "w1:p1"},
            {"tab_id": "w1:t1", "pane_id": "w1:p5"}
        ]});
        let roots = HerdrMux::root_panes_from(&panes).unwrap();
        assert_eq!(roots.get("w1:t1").map(String::as_str), Some("w1:p1"));
    }

    #[test]
    fn a_created_tab_carries_its_label_and_root_pane() {
        let created = result(CREATED);
        assert_eq!(created["tab"]["tab_id"], "w1:t3");
        assert_eq!(created["tab"]["label"], "dev");
        assert_eq!(created["root_pane"]["pane_id"], "w1:p4");
    }

    #[test]
    fn a_pane_list_that_is_not_a_list_is_an_error() {
        assert!(HerdrMux::root_panes_from(&json!({})).is_err());
        assert!(HerdrMux::tabs_from(&json!({}), &result(PANES)).is_err());
    }
}
