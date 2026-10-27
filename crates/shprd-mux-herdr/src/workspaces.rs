//! Workspaces: the herdr window shprd keeps per repository.
//!
//! herdr names a workspace with a free-text `label`, so [`shprd_mux::Mux`]'s
//! "workspace name" is the label and the workspace id is herdr's `wN`. Finding
//! a workspace by name therefore lists and compares rather than asking herdr
//! for a name, and creating one that already exists fails on herdr's side,
//! which is what the trait asks for.

use serde_json::Value;
use shprd_mux::{MuxError, Result, Workspace, WorkspaceId};
use std::path::Path;

use crate::HerdrMux;

impl HerdrMux {
    /// Every workspace herdr knows, as `(id, label)`.
    pub(crate) fn workspaces(&self) -> Result<Vec<(String, String)>> {
        Self::workspaces_from(&self.call("list workspaces", &["workspace", "list"])?)
    }

    /// Read `workspaces[]` out of a `workspace_list` result.
    pub(crate) fn workspaces_from(result: &Value) -> Result<Vec<(String, String)>> {
        let entries = result
            .get("workspaces")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                MuxError::command("list workspaces", "herdr answered without a workspace list")
            })?;
        Ok(entries
            .iter()
            .filter_map(|entry| {
                Some((
                    entry.get("workspace_id")?.as_str()?.to_string(),
                    entry.get("label")?.as_str()?.to_string(),
                ))
            })
            .collect())
    }

    /// Find the single workspace whose label is exactly `name`.
    pub(crate) fn lookup_workspace(&self, name: &str) -> Result<Option<Workspace>> {
        Ok(self
            .workspaces()?
            .into_iter()
            .find(|(_, label)| label == name)
            .map(|(id, name)| Workspace {
                id: WorkspaceId::new(id),
                name,
            }))
    }

    /// Create a workspace and let herdr start its first pane in `cwd`.
    ///
    /// `--no-focus` keeps the human's screen where it is; shprd only moves
    /// focus when [`shprd_mux::Mux::focus_workspace`] is called.
    pub(crate) fn new_workspace(&self, name: &str, cwd: &Path) -> Result<Workspace> {
        let cwd = cwd.to_string_lossy().into_owned();
        let result = self.call(
            "create workspace",
            &[
                "workspace",
                "create",
                "--label",
                name,
                "--cwd",
                &cwd,
                "--no-focus",
            ],
        )?;
        let workspace = result.get("workspace").ok_or_else(|| {
            MuxError::command("create workspace", "herdr answered without a workspace")
        })?;
        let id = workspace
            .get("workspace_id")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                MuxError::command("create workspace", "herdr answered without a workspace id")
            })?;
        let label = workspace
            .get("label")
            .and_then(Value::as_str)
            .unwrap_or(name);
        Ok(Workspace {
            id: WorkspaceId::new(id),
            name: label.to_string(),
        })
    }

    /// Bring a workspace to the front.
    pub(crate) fn focus_workspace_by_id(&self, workspace: &WorkspaceId) -> Result<()> {
        self.call_as(
            "workspace",
            workspace.as_str(),
            "focus workspace",
            &["workspace", "focus", workspace.as_str()],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    // Tests may panic: a failed assertion is the point.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use serde_json::from_str;

    const LIST: &str = include_str!("../tests/fixtures/workspace_list.json");
    const CREATED: &str = include_str!("../tests/fixtures/workspace_create.json");

    fn result(text: &str) -> Value {
        from_str::<Value>(text).unwrap()["result"].clone()
    }

    #[test]
    fn reads_id_and_label_of_every_workspace() {
        let found = HerdrMux::workspaces_from(&result(LIST)).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, "w1");
        assert_eq!(found[0].1, "probe ws");
    }

    #[test]
    fn a_snapshot_without_workspaces_is_not_a_list() {
        let error =
            HerdrMux::workspaces_from(&serde_json::json!({"type": "workspace_list"})).unwrap_err();
        assert!(error.to_string().contains("without a workspace list"));
    }

    #[test]
    fn reads_the_created_workspace_and_its_label_verbatim() {
        let workspace = result(CREATED)["workspace"].clone();
        assert_eq!(workspace["workspace_id"], "w2");
        // The label is shprd's own name, middot and all.
        assert_eq!(workspace["label"], "shprd · scratch");
    }

    #[test]
    fn finds_no_workspace_when_the_label_differs() {
        let found = HerdrMux::workspaces_from(&result(LIST)).unwrap();
        assert!(found.iter().all(|(_, label)| label != "shprd · other"));
    }
}
