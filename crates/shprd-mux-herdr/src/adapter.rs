//! The [`shprd_mux::Mux`] implementation itself: one thin method per trait
//! method, each delegating to the module that owns that part of herdr.

use std::path::Path;
use std::time::Duration;

use shprd_mux::{
    AgentState, Capabilities, Direction, Mux, MuxEvent, PaneId, Result, Tab, TabId, Workspace,
    WorkspaceId,
};

use crate::HerdrMux;

impl Mux for HerdrMux {
    /// Always the same answer: herdr 0.9.5 splits with a ratio, cannot grant
    /// extra permissions to a pane, cannot wait for a human answer in a pane,
    /// reports agent state, and does not replay lost events.
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            split_ratio: true,
            pane_grants: false,
            human_ask: false,
            resumable_events: false,
            agent_state: true,
        }
    }

    fn find_workspace(&self, name: &str) -> Result<Option<Workspace>> {
        self.lookup_workspace(name)
    }

    fn create_workspace(&self, name: &str, cwd: &Path) -> Result<Workspace> {
        self.new_workspace(name, cwd)
    }

    fn focus_workspace(&self, workspace: &WorkspaceId) -> Result<()> {
        self.focus_workspace_by_id(workspace)
    }

    fn list_tabs(&self, workspace: &WorkspaceId) -> Result<Vec<Tab>> {
        self.tabs_in(workspace)
    }

    fn create_tab(&self, workspace: &WorkspaceId, title: &str, cwd: &Path) -> Result<Tab> {
        self.new_tab(workspace, title, cwd)
    }

    fn rename_tab(&self, tab: &TabId, title: &str) -> Result<()> {
        self.retitle_tab(tab, title)
    }

    fn focus_tab(&self, tab: &TabId) -> Result<()> {
        self.select_tab(tab)
    }

    fn close_tab(&self, tab: &TabId) -> Result<()> {
        self.kill_tab(tab)
    }

    fn close_pane(&self, pane: &PaneId) -> Result<()> {
        self.close_pane_of(pane)
    }

    fn split(&self, pane: &PaneId, direction: Direction, ratio: f32, cwd: &Path) -> Result<PaneId> {
        self.split_pane(pane, direction, ratio, cwd)
    }

    fn run(&self, pane: &PaneId, argv: &[String], env: &[(String, Option<String>)]) -> Result<()> {
        self.run_argv(pane, argv, env)
    }

    fn send_text(&self, pane: &PaneId, text: &str) -> Result<()> {
        self.type_text(pane, text)
    }

    fn prompt(&self, pane: &PaneId, text: &str) -> Result<()> {
        self.prompt_agent(pane, text)
    }

    fn read(&self, pane: &PaneId, lines: usize) -> Result<String> {
        self.read_pane(pane, lines)
    }

    fn agent_state(&self, pane: &PaneId) -> Result<AgentState> {
        self.state_of(pane)
    }

    fn pane_process_group(&self, pane: &PaneId) -> Result<Option<u32>> {
        self.process_group_of(pane)
    }

    fn next_events(&self, timeout: Duration) -> Result<Vec<MuxEvent>> {
        self.poll_events(timeout)
    }
}
