//! A scriptable [`Mux`] for tests.
//!
//! [`FakeMux`] is compiled into the library, not just this crate's tests, so
//! every crate that needs to drive a scheduler, planner or tui against a
//! multiplexer can use the same double. It does no I/O at all: ids are
//! deterministic, every call is recorded in order, and screen text, agent state
//! and events are whatever the test scripted.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use crate::types::{
    AgentState, Capabilities, Direction, MuxError, MuxEvent, PaneId, Tab, TabId, Workspace,
    WorkspaceId,
};
use crate::{Mux, Result};

/// One call the fake recorded, in the order it was made.
///
/// Argument order matches the trait method, except that calls carrying no
/// interesting data keep only the fields a test would assert on.
#[derive(Debug, Clone, PartialEq)]
pub enum Call {
    /// `capabilities()`.
    Capabilities,
    /// `find_workspace(name)`.
    FindWorkspace(String),
    /// `create_workspace(name, cwd)`.
    CreateWorkspace(String, PathBuf),
    /// `focus_workspace(workspace)`.
    FocusWorkspace(WorkspaceId),
    /// `list_tabs(workspace)`.
    ListTabs(WorkspaceId),
    /// `create_tab(workspace, title, cwd)`.
    CreateTab(WorkspaceId, String, PathBuf),
    /// `rename_tab(tab, title)`.
    RenameTab(TabId, String),
    /// `focus_tab(tab)`.
    FocusTab(TabId),
    /// `close_tab(tab)`.
    CloseTab(TabId),
    /// `split(pane, direction, ratio, cwd)`.
    Split(PaneId, Direction, f32, PathBuf),
    /// `run(pane, argv, env)`.
    Run(PaneId, Vec<String>, Vec<(String, Option<String>)>),
    /// `send_text(pane, text)`.
    SendText(PaneId, String),
    /// `prompt(pane, text)`.
    Prompt(PaneId, String),
    /// `read(pane, lines)`.
    Read(PaneId, usize),
    /// `agent_state(pane)`.
    AgentState(PaneId),
    /// `next_events(timeout)`.
    NextEvents(Duration),
}

/// The record-keeping state behind the mutex.
#[derive(Debug)]
struct Inner {
    calls: Vec<Call>,
    workspaces: Vec<Workspace>,
    /// Tabs with the workspace that owns each, so listing stays scoped.
    tabs: Vec<(WorkspaceId, Tab)>,
    panes: BTreeSet<PaneId>,
    screens: HashMap<PaneId, String>,
    states: HashMap<PaneId, AgentState>,
    events: VecDeque<MuxEvent>,
    next_workspace: usize,
    next_tab: usize,
    next_pane: usize,
}

impl Inner {
    fn fresh() -> Self {
        Self {
            calls: Vec::new(),
            workspaces: Vec::new(),
            tabs: Vec::new(),
            panes: BTreeSet::new(),
            screens: HashMap::new(),
            states: HashMap::new(),
            events: VecDeque::new(),
            next_workspace: 0,
            next_tab: 0,
            next_pane: 0,
        }
    }

    /// Allocate the next deterministic pane id and remember it exists.
    fn new_pane(&mut self) -> PaneId {
        self.next_pane += 1;
        let pane = PaneId::new(format!("pane-{}", self.next_pane));
        self.panes.insert(pane.clone());
        pane
    }

    /// Fail the way a real host does when the id is gone.
    fn require_pane(&self, pane: &PaneId) -> Result<()> {
        if self.panes.contains(pane) {
            Ok(())
        } else {
            Err(MuxError::not_found("pane", pane))
        }
    }

    /// The workspace with this id, if it exists.
    fn workspace(&self, id: &WorkspaceId) -> Option<&Workspace> {
        self.workspaces.iter().find(|ws| &ws.id == id)
    }

    /// The tab with this id, if it exists.
    fn tab(&self, id: &TabId) -> Option<&Tab> {
        self.tabs
            .iter()
            .map(|(_, tab)| tab)
            .find(|tab| &tab.id == id)
    }
}

/// A [`Mux`] that pretends, and remembers everything it was asked to do.
#[derive(Debug)]
pub struct FakeMux {
    caps: Capabilities,
    inner: Mutex<Inner>,
}

impl Default for FakeMux {
    /// A fake claiming every capability, for tests that do not care which
    /// features the host has.
    fn default() -> Self {
        Self::new(Capabilities {
            split_ratio: true,
            pane_grants: true,
            human_ask: true,
            resumable_events: true,
            agent_state: true,
        })
    }
}

impl FakeMux {
    /// A fake reporting exactly `caps` from [`Mux::capabilities`]; this is how a
    /// test exercises a caller's degraded path.
    pub fn new(caps: Capabilities) -> Self {
        Self {
            caps,
            inner: Mutex::new(Inner::fresh()),
        }
    }

    /// Every call made so far, oldest first.
    pub fn calls(&self) -> Vec<Call> {
        self.lock()
            .map(|inner| inner.calls.clone())
            .unwrap_or_default()
    }

    /// Take the recorded calls, leaving the log empty.
    pub fn take_calls(&self) -> Vec<Call> {
        self.lock()
            .map(|mut inner| std::mem::take(&mut inner.calls))
            .unwrap_or_default()
    }

    /// Script what [`Mux::read`] returns for `pane`, registering the pane as
    /// existing if it was not already.
    pub fn set_screen(&self, pane: &PaneId, text: impl Into<String>) {
        if let Ok(mut inner) = self.lock() {
            inner.panes.insert(pane.clone());
            inner.screens.insert(pane.clone(), text.into());
        }
    }

    /// Script what [`Mux::agent_state`] reports for `pane`, registering the pane
    /// as existing if it was not already.
    pub fn set_state(&self, pane: &PaneId, state: AgentState) {
        if let Ok(mut inner) = self.lock() {
            inner.panes.insert(pane.clone());
            inner.states.insert(pane.clone(), state);
        }
    }

    /// Queue an event for the next [`Mux::next_events`] call.
    pub fn push_event(&self, event: MuxEvent) {
        if let Ok(mut inner) = self.lock() {
            inner.events.push_back(event);
        }
    }

    /// Acquire the state, turning a poisoned lock into an ordinary error.
    fn lock(&self) -> Result<MutexGuard<'_, Inner>> {
        self.inner
            .lock()
            .map_err(|_| MuxError::command("fake mux", "state mutex is poisoned"))
    }
}

impl Mux for FakeMux {
    fn capabilities(&self) -> Capabilities {
        if let Ok(mut inner) = self.lock() {
            inner.calls.push(Call::Capabilities);
        }
        self.caps
    }

    fn find_workspace(&self, name: &str) -> Result<Option<Workspace>> {
        let mut inner = self.lock()?;
        inner.calls.push(Call::FindWorkspace(name.to_string()));
        Ok(inner.workspaces.iter().find(|ws| ws.name == name).cloned())
    }

    fn create_workspace(&self, name: &str, cwd: &Path) -> Result<Workspace> {
        let mut inner = self.lock()?;
        inner
            .calls
            .push(Call::CreateWorkspace(name.to_string(), cwd.into()));
        inner.next_workspace += 1;
        let workspace = Workspace {
            id: WorkspaceId::new(format!("ws-{}", inner.next_workspace)),
            name: name.to_string(),
        };
        inner.workspaces.push(workspace.clone());
        Ok(workspace)
    }

    fn focus_workspace(&self, workspace: &WorkspaceId) -> Result<()> {
        let mut inner = self.lock()?;
        inner.calls.push(Call::FocusWorkspace(workspace.clone()));
        if inner.workspace(workspace).is_none() {
            return Err(MuxError::not_found("workspace", workspace));
        }
        Ok(())
    }

    fn list_tabs(&self, workspace: &WorkspaceId) -> Result<Vec<Tab>> {
        let mut inner = self.lock()?;
        inner.calls.push(Call::ListTabs(workspace.clone()));
        if inner.workspace(workspace).is_none() {
            return Err(MuxError::not_found("workspace", workspace));
        }
        Ok(inner
            .tabs
            .iter()
            .filter(|(owner, _)| owner == workspace)
            .map(|(_, tab)| tab.clone())
            .collect())
    }

    fn create_tab(&self, workspace: &WorkspaceId, title: &str, cwd: &Path) -> Result<Tab> {
        let mut inner = self.lock()?;
        inner.calls.push(Call::CreateTab(
            workspace.clone(),
            title.to_string(),
            cwd.into(),
        ));
        if inner.workspace(workspace).is_none() {
            return Err(MuxError::not_found("workspace", workspace));
        }
        let root_pane = inner.new_pane();
        inner.next_tab += 1;
        let tab = Tab {
            id: TabId::new(format!("tab-{}", inner.next_tab)),
            title: title.to_string(),
            root_pane,
        };
        inner.tabs.push((workspace.clone(), tab.clone()));
        Ok(tab)
    }

    fn rename_tab(&self, tab: &TabId, title: &str) -> Result<()> {
        let mut inner = self.lock()?;
        inner
            .calls
            .push(Call::RenameTab(tab.clone(), title.to_string()));
        if inner.tab(tab).is_none() {
            return Err(MuxError::not_found("tab", tab));
        }
        for (_, existing) in inner.tabs.iter_mut() {
            if &existing.id == tab {
                existing.title = title.to_string();
            }
        }
        Ok(())
    }

    fn focus_tab(&self, tab: &TabId) -> Result<()> {
        let mut inner = self.lock()?;
        inner.calls.push(Call::FocusTab(tab.clone()));
        if inner.tab(tab).is_none() {
            return Err(MuxError::not_found("tab", tab));
        }
        Ok(())
    }

    fn close_tab(&self, tab: &TabId) -> Result<()> {
        let mut inner = self.lock()?;
        inner.calls.push(Call::CloseTab(tab.clone()));
        if inner.tab(tab).is_none() {
            return Err(MuxError::not_found("tab", tab));
        }
        inner.tabs.retain(|(_, existing)| &existing.id != tab);
        Ok(())
    }

    fn split(&self, pane: &PaneId, direction: Direction, ratio: f32, cwd: &Path) -> Result<PaneId> {
        let mut inner = self.lock()?;
        inner
            .calls
            .push(Call::Split(pane.clone(), direction, ratio, cwd.into()));
        inner.require_pane(pane)?;
        Ok(inner.new_pane())
    }

    fn run(&self, pane: &PaneId, argv: &[String], env: &[(String, Option<String>)]) -> Result<()> {
        let mut inner = self.lock()?;
        inner
            .calls
            .push(Call::Run(pane.clone(), argv.to_vec(), env.to_vec()));
        inner.require_pane(pane)
    }

    fn send_text(&self, pane: &PaneId, text: &str) -> Result<()> {
        let mut inner = self.lock()?;
        inner
            .calls
            .push(Call::SendText(pane.clone(), text.to_string()));
        inner.require_pane(pane)
    }

    fn prompt(&self, pane: &PaneId, text: &str) -> Result<()> {
        let mut inner = self.lock()?;
        inner
            .calls
            .push(Call::Prompt(pane.clone(), text.to_string()));
        inner.require_pane(pane)
    }

    fn read(&self, pane: &PaneId, lines: usize) -> Result<String> {
        let mut inner = self.lock()?;
        inner.calls.push(Call::Read(pane.clone(), lines));
        inner.require_pane(pane)?;
        Ok(inner.screens.get(pane).cloned().unwrap_or_default())
    }

    fn agent_state(&self, pane: &PaneId) -> Result<AgentState> {
        let mut inner = self.lock()?;
        inner.calls.push(Call::AgentState(pane.clone()));
        inner.require_pane(pane)?;
        Ok(inner
            .states
            .get(pane)
            .copied()
            .unwrap_or(AgentState::Unknown))
    }

    /// Never blocks: everything queued is returned at once, and an empty queue
    /// returns immediately regardless of `timeout`.
    fn next_events(&self, timeout: Duration) -> Result<Vec<MuxEvent>> {
        let mut inner = self.lock()?;
        inner.calls.push(Call::NextEvents(timeout));
        Ok(inner.events.drain(..).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::{Call, FakeMux};
    use crate::Mux;
    use crate::types::{AgentState, Direction, MuxError, MuxEvent, PaneId, TabId, WorkspaceId};
    use std::fmt::Display;
    use std::path::Path;
    use std::time::Duration;

    /// Unwrap like a test wants to, without tripping the `unwrap_used` lint.
    fn ok<T, E: Display>(result: Result<T, E>) -> T {
        match result {
            Ok(value) => value,
            Err(error) => panic!("expected Ok, got: {error}"),
        }
    }

    fn fixture() -> (FakeMux, WorkspaceId, crate::types::Tab) {
        let mux = FakeMux::default();
        let ws = ok(mux.create_workspace("shprd · repo", Path::new("/tmp/repo")));
        let tab = ok(mux.create_tab(&ws.id, "● claude·w1", Path::new("/tmp/repo")));
        (mux, ws.id, tab)
    }

    #[test]
    fn records_every_call_in_order() {
        let (mux, ws, tab) = fixture();
        ok(mux.focus_tab(&tab.id));
        ok(mux.split(&tab.root_pane, Direction::Right, 0.5, Path::new("/tmp")));
        let calls = mux.calls();
        assert_eq!(
            calls,
            vec![
                Call::CreateWorkspace("shprd · repo".to_string(), "/tmp/repo".into()),
                Call::CreateTab(ws, "● claude·w1".to_string(), "/tmp/repo".into()),
                Call::FocusTab(tab.id.clone()),
                Call::Split(tab.root_pane.clone(), Direction::Right, 0.5, "/tmp".into()),
            ]
        );
        assert!(mux.take_calls().len() == 4);
        assert!(mux.calls().is_empty());
    }

    #[test]
    fn unknown_ids_are_not_found() {
        let mux = FakeMux::default();
        let pane = PaneId::new("pane-404");
        assert!(matches!(
            mux.read(&pane, 10),
            Err(MuxError::NotFound { what: "pane", .. })
        ));
        assert!(matches!(
            mux.prompt(&pane, "hi"),
            Err(MuxError::NotFound { .. })
        ));
        assert!(matches!(
            mux.agent_state(&pane),
            Err(MuxError::NotFound { .. })
        ));
        assert!(matches!(
            mux.close_tab(&TabId::new("tab-404")),
            Err(MuxError::NotFound { what: "tab", .. })
        ));
        assert!(matches!(
            mux.list_tabs(&WorkspaceId::new("ws-404")),
            Err(MuxError::NotFound {
                what: "workspace",
                ..
            })
        ));
    }

    #[test]
    fn scripted_screen_state_and_events_round_trip() {
        let (mux, _, tab) = fixture();
        let pane = tab.root_pane.clone();
        mux.set_screen(&pane, "working…\n> branch m4.1");
        assert_eq!(ok(mux.read(&pane, 5)), "working…\n> branch m4.1");
        assert_eq!(ok(mux.agent_state(&pane)), AgentState::Unknown);
        mux.set_state(&pane, AgentState::Blocked);
        assert_eq!(ok(mux.agent_state(&pane)), AgentState::Blocked);
        mux.push_event(MuxEvent::AgentState {
            pane: pane.clone(),
            state: AgentState::Done,
        });
        mux.push_event(MuxEvent::PaneExited { pane: pane.clone() });
        let events = ok(mux.next_events(Duration::from_millis(50)));
        assert_eq!(
            events,
            vec![
                MuxEvent::AgentState {
                    pane: pane.clone(),
                    state: AgentState::Done,
                },
                MuxEvent::PaneExited { pane },
            ]
        );
        assert!(ok(mux.next_events(Duration::ZERO)).is_empty());
    }

    #[test]
    fn env_and_argv_are_recorded_verbatim() {
        let (mux, _, tab) = fixture();
        let argv = vec!["claude".to_string(), "--flag=x y".to_string()];
        let env = vec![
            ("SOME_TOKEN".to_string(), Some("s3cret".to_string())),
            ("NO_COLOR".to_string(), None),
        ];
        ok(mux.run(&tab.root_pane, &argv, &env));
        assert_eq!(
            mux.calls().last(),
            Some(&Call::Run(tab.root_pane.clone(), argv, env))
        );
    }
}
