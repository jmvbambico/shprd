//! A scriptable [`Host`] for tests.
//!
//! [`FakeMux`] is compiled into the library, not just this crate's tests, so
//! every crate that needs to drive a scheduler, planner or tick against a
//! multiplexer can use the same double. It does no I/O at all: ids are
//! deterministic, every call is recorded in order, and screen text, agent state
//! and events are whatever the test scripted.
//!
//! It is also where a host's *lacks* are written down: [`Capabilities::all`]
//! minus whatever a builder took away, with every removed capability's verb
//! answering [`MuxError::Unsupported`] naming it. A caller's degraded path is
//! therefore testable without a host that really is old.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use crate::types::{
    AgentState, Capabilities, Direction, Key, MuxError, MuxEvent, Outcome, PaneId, Tab, TabId,
    Waited, Workspace, WorkspaceId,
};
use crate::{Host, Result};

/// One call the fake recorded, in the order it was made.
///
/// Argument order matches the trait method, except that calls carrying no
/// interesting data keep only the fields a test would assert on.
#[derive(Debug, Clone, PartialEq)]
pub enum Call {
    /// `capabilities()`.
    Capabilities,
    /// `find_space(name)`.
    FindSpace(String),
    /// `list_spaces()`.
    ListSpaces,
    /// `ensure_space(name, cwd)`.
    EnsureSpace(String, PathBuf),
    /// `create_space(name, cwd)`.
    CreateSpace(String, PathBuf),
    /// `close_space(space)`.
    CloseSpace(WorkspaceId),
    /// `focus_space(space)`.
    FocusSpace(WorkspaceId),
    /// `list_tabs(space)`.
    ListTabs(WorkspaceId),
    /// `rename_tab(tab, title)`.
    RenameTab(TabId, String),
    /// `focus_tab(tab)`.
    FocusTab(TabId),
    /// `close_tab(tab)`.
    CloseTab(TabId),
    /// `spawn(space, title, argv, env, cwd)`.
    Spawn(
        WorkspaceId,
        String,
        Vec<String>,
        Vec<(String, Option<String>)>,
        PathBuf,
    ),
    /// `split(pane, direction, ratio, cwd)`.
    Split(PaneId, Direction, f32, PathBuf),
    /// `close_pane(pane)`.
    ClosePane(PaneId),
    /// `write(pane, text, submit)`.
    Write(PaneId, String, bool),
    /// `keys(pane, keys)`.
    Keys(PaneId, Vec<Key>),
    /// `read(pane, lines)`.
    Read(PaneId, usize),
    /// `wait(pane, waited, timeout)`.
    Wait(PaneId, Waited, Duration),
    /// `kill(pane)`.
    Kill(PaneId),
    /// `agent_state(pane)`.
    AgentState(PaneId),
    /// `pane_process_group(pane)`.
    PaneProcessGroup(PaneId),
    /// `subscribe(timeout)`.
    Subscribe(Duration),
    /// `doc(space, name, body)`.
    Doc(WorkspaceId, String, String),
    /// `exec(argv, cwd)`.
    Exec(Vec<String>, PathBuf),
}

/// How a test scripted [`Host::close_tab`] for everything except a keeper tab.
///
/// It is the shape two callers need: a hand-off closes the outgoing agent's tab
/// and then the newcomer's, and the tests care about the one close that fails.
#[derive(Debug, Clone)]
enum TabClosePolicy {
    /// Every close but the keeper's fails with this reason.
    Refused(String),
    /// Every close but the keeper's answers "no such tab".
    Forgotten,
}

/// The record-keeping state behind the mutex.
#[derive(Debug)]
struct Inner {
    calls: Vec<Call>,
    spaces: Vec<Workspace>,
    /// Tabs with the space that owns each, so listing stays scoped.
    tabs: Vec<(WorkspaceId, Tab)>,
    panes: BTreeSet<PaneId>,
    /// What each pane's screen holds, and what a wait for output matches on.
    screens: HashMap<PaneId, String>,
    states: HashMap<PaneId, AgentState>,
    /// A scripted "not detected yet, then ready": how many more polls answer
    /// [`AgentState::Unknown`], and the state that follows.
    pending: HashMap<PaneId, (usize, AgentState)>,
    /// Scripted foreground process group per pane; absent means "no group".
    process_groups: HashMap<PaneId, Option<u32>>,
    /// Panes a test scripted `close_pane` to refuse, with the reason.
    close_failures: HashMap<PaneId, String>,
    /// Panes a test scripted `read` to fail on, with the reason: a pane the
    /// host still knows, whose screen it will not hand over.
    read_failures: HashMap<PaneId, String>,
    /// Programs a test scripted to end, and with what code.
    exits: HashMap<PaneId, Option<i32>>,
    /// Spaces a test scripted `close_space` to refuse, with the reason.
    space_close_failures: HashMap<WorkspaceId, String>,
    /// The answer a scripted `exec` gives.
    exec_answer: Option<Outcome>,
    /// The answer a scripted `doc` gives.
    doc_answer: Option<String>,
    events: VecDeque<MuxEvent>,
    next_space: usize,
    next_tab: usize,
    next_pane: usize,
}

impl Inner {
    fn fresh() -> Self {
        Self {
            calls: Vec::new(),
            spaces: Vec::new(),
            tabs: Vec::new(),
            panes: BTreeSet::new(),
            screens: HashMap::new(),
            states: HashMap::new(),
            pending: HashMap::new(),
            process_groups: HashMap::new(),
            close_failures: HashMap::new(),
            read_failures: HashMap::new(),
            exits: HashMap::new(),
            space_close_failures: HashMap::new(),
            exec_answer: None,
            doc_answer: None,
            events: VecDeque::new(),
            next_space: 0,
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

    /// Allocate the id and root pane of a new tab in `space`.
    fn new_tab(&mut self, space: &WorkspaceId, title: &str) -> Tab {
        let root_pane = self.new_pane();
        self.next_tab += 1;
        let tab = Tab {
            id: TabId::new(format!("tab-{}", self.next_tab)),
            title: title.to_string(),
            root_pane,
        };
        self.tabs.push((space.clone(), tab.clone()));
        tab
    }

    /// Fail the way a real host does when the id is gone.
    fn require_pane(&self, pane: &PaneId) -> Result<()> {
        if self.panes.contains(pane) {
            Ok(())
        } else {
            Err(MuxError::not_found("pane", pane))
        }
    }

    /// The space with this id, if it exists.
    fn space(&self, id: &WorkspaceId) -> Option<&Workspace> {
        self.spaces.iter().find(|space| &space.id == id)
    }

    /// The tab with this id, if it exists.
    fn tab(&self, id: &TabId) -> Option<&Tab> {
        self.tabs
            .iter()
            .map(|(_, tab)| tab)
            .find(|tab| &tab.id == id)
    }
}

/// A [`Host`] that pretends, and remembers everything it was asked to do.
#[derive(Debug)]
pub struct FakeMux {
    caps: Capabilities,
    /// Whether a new space also gets the starter tab a real host makes.
    starter_tab: bool,
    /// Whether a pane with no scripted state reports a detected agent.
    ready: bool,
    /// The reason `spawn` fails, when a test wants a host that cannot start a
    /// program.
    spawn_failure: Option<String>,
    /// The reason `spawn` is refused, when a test wants tuios without `admin`.
    spawn_forbidden: Option<String>,
    /// The group every pane reports until a test scripts its own.
    default_group: Option<u32>,
    /// Whether `split` answers as a host that cannot split.
    split_refused: bool,
    /// Whether `close_pane` answers as a host that cannot close one.
    close_pane_refused: bool,
    /// The reason `close_pane` fails with a host error, when a test wants a
    /// host whose capability and runtime disagree.
    close_pane_failure: Option<String>,
    /// How this host answers `close_tab` for every tab but its keeper, when a
    /// test wanted a host that holds one tab open (or has forgotten it).
    tab_close: Option<TabClosePolicy>,
    /// The one tab `tab_close` leaves alone.
    tab_keep: Option<TabId>,
    inner: Mutex<Inner>,
}

impl Default for FakeMux {
    /// A fake claiming every capability, for tests that do not care which
    /// features the host has.
    fn default() -> Self {
        Self::new(Capabilities::all())
    }
}

impl FakeMux {
    /// A fake reporting exactly `caps` from `Host::capabilities`; this is how a
    /// test exercises a caller's degraded path.
    pub fn new(caps: Capabilities) -> Self {
        Self {
            caps,
            starter_tab: false,
            ready: false,
            spawn_failure: None,
            spawn_forbidden: None,
            default_group: None,
            split_refused: false,
            close_pane_refused: false,
            close_pane_failure: None,
            tab_close: None,
            tab_keep: None,
            inner: Mutex::new(Inner::fresh()),
        }
    }

    /// The capabilities this fake reports, without recording a call.
    pub fn caps(&self) -> Capabilities {
        self.caps
    }

    /// Make `create_space` also create the starter tab a real host makes,
    /// titled the way herdr titles it. Tests that lay out a space use this to
    /// see the tab a host would have left behind.
    pub fn with_starter_tab(mut self) -> Self {
        self.starter_tab = true;
        self
    }

    /// Report every unscripted pane as [`AgentState::Idle`], i.e. as an agent
    /// the host has already detected, so a caller waiting for readiness does
    /// not have to script a state for ids it cannot predict.
    pub fn with_ready_agent(mut self) -> Self {
        self.ready = true;
        self
    }

    /// Make `spawn` fail with `reason` after recording the call, which is how a
    /// host that cannot start the CLI looks to a caller.
    pub fn failing_spawn(mut self, reason: impl Into<String>) -> Self {
        self.spawn_failure = Some(reason.into());
        self
    }

    /// Make `spawn` answer [`MuxError::Forbidden`] with `reason`, which is how
    /// tuios refuses `new-window` without the `admin` grant.
    pub fn forbidden_spawn(mut self, reason: impl Into<String>) -> Self {
        self.spawn_forbidden = Some(reason.into());
        self.caps.spawn = false;
        self
    }

    /// Report `group` as the foreground process group of every pane that has
    /// no scripted group of its own, which is how a test gives panes an
    /// identity before their ids exist.
    pub fn with_process_group(mut self, group: u32) -> Self {
        self.default_group = Some(group);
        self
    }

    /// Script what `pane_process_group` reports for `pane`: `Some` is a host
    /// that can tell, `None` a host that cannot. Registers the pane as existing
    /// if it was not already.
    pub fn set_process_group(&self, pane: &PaneId, group: Option<u32>) {
        if let Ok(mut inner) = self.lock() {
            inner.panes.insert(pane.clone());
            inner.process_groups.insert(pane.clone(), group);
        }
    }

    /// Make `close_pane` refuse `pane` alone, with `reason`.
    ///
    /// Everything else about the host keeps working, which is a host that still
    /// holds one pane it will not retire while the next close succeeds.
    pub fn failing_close_of(&self, pane: &PaneId, reason: impl Into<String>) {
        if let Ok(mut inner) = self.lock() {
            inner.close_failures.insert(pane.clone(), reason.into());
        }
    }

    /// Make `close_tab` fail for every tab but `keep`, with `reason`.
    ///
    /// `None` means every close fails, which is a host that holds the lot. The
    /// call is still recorded, so a test can see shprd tried.
    pub fn failing_tab_closes(
        &mut self,
        keep: Option<&TabId>,
        reason: impl Into<String>,
    ) -> &mut Self {
        self.tab_close = Some(TabClosePolicy::Refused(reason.into()));
        self.tab_keep = keep.cloned();
        self
    }

    /// Make `close_tab` answer [`MuxError::NotFound`] for every tab but `keep`:
    /// a host that has already forgotten the tab shprd is tidying up.
    pub fn forgetting_tab_closes(&mut self, keep: Option<&TabId>) -> &mut Self {
        self.tab_close = Some(TabClosePolicy::Forgotten);
        self.tab_keep = keep.cloned();
        self
    }

    /// Make `close_space` refuse `space` alone, with `reason`: a space the host
    /// keeps while every other close succeeds.
    pub fn failing_space_close_of(&self, space: &WorkspaceId, reason: impl Into<String>) {
        if let Ok(mut inner) = self.lock() {
            inner
                .space_close_failures
                .insert(space.clone(), reason.into());
        }
    }

    /// Make `read` fail for `pane` alone with `reason`, registering the pane as
    /// existing if it was not already: a pane the host knows and will still type
    /// into, whose screen it refuses to show.
    pub fn failing_read_of(&self, pane: &PaneId, reason: impl Into<String>) {
        if let Ok(mut inner) = self.lock() {
            inner.panes.insert(pane.clone());
            inner.read_failures.insert(pane.clone(), reason.into());
        }
    }

    /// Make `split` answer [`MuxError::Unsupported`], which is how a host
    /// without splits looks to a caller that checks the capability.
    pub fn without_split(mut self) -> Self {
        self.split_refused = true;
        self.caps.split = false;
        self
    }

    /// Make the fake a host without single-pane closes: the capability says so
    /// and `close_pane` answers [`MuxError::Unsupported`], the two agreeing the
    /// way a real adapter's do.
    pub fn without_close_pane(mut self) -> Self {
        self.caps.close_pane = false;
        self.close_pane_refused = true;
        self
    }

    /// Make the fake a host without whole-space closes: the capability says so
    /// and `close_space` answers [`MuxError::Unsupported`], so a caller's
    /// tab-by-tab fallback is the only way its space can go away.
    pub fn without_close_space(mut self) -> Self {
        self.caps.close_space = false;
        self
    }

    /// Keep [`Capabilities::close_pane`] true but make `close_pane` fail with
    /// `reason` anyway, which is how a host that lost the pane its capability
    /// promised looks to a caller.
    pub fn failing_close_pane(mut self, reason: impl Into<String>) -> Self {
        self.close_pane_failure = Some(reason.into());
        self
    }

    /// Script the program in `pane` to have ended with `code`, which is what
    /// `wait(.., Waited::Exit, ..)` and `exit_of` see.
    pub fn set_exited(&self, pane: &PaneId, code: Option<i32>) {
        if let Ok(mut inner) = self.lock() {
            inner.panes.insert(pane.clone());
            inner.exits.insert(pane.clone(), code);
            inner.states.insert(pane.clone(), AgentState::Done);
        }
    }

    /// Script a space to hold the pane it was created with, so a test can list a
    /// space's tabs before it spawns anything into it.
    pub fn with_pane(&self, space: &WorkspaceId, title: &str) -> Tab {
        match self.lock() {
            Ok(mut inner) => inner.new_tab(space, title),
            Err(_) => Tab {
                id: TabId::new("tab-unavailable"),
                title: title.to_string(),
                root_pane: PaneId::new("pane-unavailable"),
            },
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

    /// Script what `read` returns for `pane`, registering the pane as existing
    /// if it was not already.
    pub fn set_screen(&self, pane: &PaneId, text: impl Into<String>) {
        if let Ok(mut inner) = self.lock() {
            inner.panes.insert(pane.clone());
            inner.screens.insert(pane.clone(), text.into());
        }
    }

    /// Script what `agent_state` reports for `pane`, registering the pane as
    /// existing if it was not already.
    pub fn set_state(&self, pane: &PaneId, state: AgentState) {
        if let Ok(mut inner) = self.lock() {
            inner.panes.insert(pane.clone());
            inner.states.insert(pane.clone(), state);
        }
    }

    /// Script a pane that is not an agent yet: `polls` consecutive
    /// `agent_state` calls answer [`AgentState::Unknown`], and every call after
    /// them answers `state`. Later polls keep answering `state`, so a waiter
    /// that sleeps between polls converges.
    pub fn set_state_after(&self, pane: &PaneId, polls: usize, state: AgentState) {
        if let Ok(mut inner) = self.lock() {
            inner.panes.insert(pane.clone());
            inner.pending.insert(pane.clone(), (polls, state));
        }
    }

    /// Script what `exec` answers: the exit code, and the output a caller reads
    /// from the [`Outcome`].
    pub fn set_exec_answer(&self, code: Option<i32>, output: impl Into<String>) {
        if let Ok(mut inner) = self.lock() {
            inner.exec_answer = Some(Outcome {
                exited: true,
                exit_code: code,
                output: Some(output.into()),
            });
        }
    }

    /// Script what `doc` answers: the handle the host would give back.
    pub fn set_doc_answer(&self, handle: impl Into<String>) {
        if let Ok(mut inner) = self.lock() {
            inner.doc_answer = Some(handle.into());
        }
    }

    /// Queue an event for the next `subscribe` call.
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

impl Host for FakeMux {
    fn capabilities(&self) -> Capabilities {
        if let Ok(mut inner) = self.lock() {
            inner.calls.push(Call::Capabilities);
        }
        self.caps
    }

    fn find_space(&self, name: &str) -> Result<Option<Workspace>> {
        let mut inner = self.lock()?;
        inner.calls.push(Call::FindSpace(name.to_string()));
        Ok(inner
            .spaces
            .iter()
            .find(|space| space.name == name)
            .cloned())
    }

    fn list_spaces(&self) -> Result<Vec<Workspace>> {
        let mut inner = self.lock()?;
        inner.calls.push(Call::ListSpaces);
        if !self.caps.list_spaces {
            return Err(MuxError::Unsupported("listing spaces"));
        }
        Ok(inner.spaces.clone())
    }

    /// Records the lookup before it answers it, so a caller's plan is visible
    /// even when the space is already there.
    fn ensure_space(&self, name: &str, cwd: &Path) -> Result<Workspace> {
        {
            let mut inner = self.lock()?;
            inner
                .calls
                .push(Call::EnsureSpace(name.to_string(), cwd.into()));
        }
        if let Some(space) = self.find_space(name)? {
            return Ok(space);
        }
        match self.create_space(name, cwd) {
            Ok(space) => Ok(space),
            Err(err) => match self.find_space(name) {
                Ok(Some(space)) => Ok(space),
                _ => Err(err),
            },
        }
    }

    fn create_space(&self, name: &str, cwd: &Path) -> Result<Workspace> {
        let mut inner = self.lock()?;
        inner
            .calls
            .push(Call::CreateSpace(name.to_string(), cwd.into()));
        inner.next_space += 1;
        let space = Workspace {
            id: WorkspaceId::new(format!("space-{}", inner.next_space)),
            name: name.to_string(),
        };
        inner.spaces.push(space.clone());
        if self.starter_tab {
            inner.new_tab(&space.id, "1");
        }
        Ok(space)
    }

    fn close_space(&self, space: &WorkspaceId) -> Result<()> {
        let mut inner = self.lock()?;
        inner.calls.push(Call::CloseSpace(space.clone()));
        if !self.caps.close_space {
            return Err(MuxError::Unsupported("closing a space"));
        }
        if let Some(reason) = inner.space_close_failures.get(space) {
            return Err(MuxError::command("close space", reason.clone()));
        }
        if inner.space(space).is_none() {
            return Err(MuxError::not_found("space", space));
        }
        // Closing a space takes its tabs and their panes, as a real host does.
        let gone: Vec<PaneId> = inner
            .tabs
            .iter()
            .filter(|(owner, _)| owner == space)
            .map(|(_, tab)| tab.root_pane.clone())
            .collect();
        for pane in gone {
            inner.panes.remove(&pane);
            inner.screens.remove(&pane);
            inner.states.remove(&pane);
            inner.read_failures.remove(&pane);
            inner.exits.remove(&pane);
        }
        inner.tabs.retain(|(owner, _)| owner != space);
        inner.spaces.retain(|existing| &existing.id != space);
        Ok(())
    }

    fn focus_space(&self, space: &WorkspaceId) -> Result<()> {
        let mut inner = self.lock()?;
        inner.calls.push(Call::FocusSpace(space.clone()));
        if inner.space(space).is_none() {
            return Err(MuxError::not_found("space", space));
        }
        Ok(())
    }

    fn list_tabs(&self, space: &WorkspaceId) -> Result<Vec<Tab>> {
        let mut inner = self.lock()?;
        inner.calls.push(Call::ListTabs(space.clone()));
        if inner.space(space).is_none() {
            return Err(MuxError::not_found("space", space));
        }
        Ok(inner
            .tabs
            .iter()
            .filter(|(owner, _)| owner == space)
            .map(|(_, tab)| tab.clone())
            .collect())
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
        let kept = self.tab_keep.as_ref() == Some(tab);
        if !kept {
            match &self.tab_close {
                Some(TabClosePolicy::Refused(reason)) => {
                    return Err(MuxError::command("close tab", reason.clone()));
                }
                Some(TabClosePolicy::Forgotten) => {
                    return Err(MuxError::not_found("tab", tab));
                }
                None => {}
            }
        }
        let Some(found) = inner.tab(tab).cloned() else {
            return Err(MuxError::not_found("tab", tab));
        };
        inner.tabs.retain(|(_, existing)| &existing.id != tab);
        inner.panes.remove(&found.root_pane);
        inner.screens.remove(&found.root_pane);
        inner.states.remove(&found.root_pane);
        inner.read_failures.remove(&found.root_pane);
        inner.exits.remove(&found.root_pane);
        Ok(())
    }

    fn spawn(
        &self,
        space: &WorkspaceId,
        title: &str,
        argv: &[String],
        env: &[(String, Option<String>)],
        cwd: &Path,
    ) -> Result<Tab> {
        let mut inner = self.lock()?;
        inner.calls.push(Call::Spawn(
            space.clone(),
            title.to_string(),
            argv.to_vec(),
            env.to_vec(),
            cwd.into(),
        ));
        if let Some(reason) = &self.spawn_forbidden {
            return Err(MuxError::forbidden("spawn", reason.clone()));
        }
        if !self.caps.spawn {
            return Err(MuxError::Unsupported("spawning a program"));
        }
        if let Some(reason) = &self.spawn_failure {
            return Err(MuxError::command("spawn", reason.clone()));
        }
        if inner.space(space).is_none() {
            return Err(MuxError::not_found("space", space));
        }
        Ok(inner.new_tab(space, title))
    }

    fn split(&self, pane: &PaneId, direction: Direction, ratio: f32, cwd: &Path) -> Result<PaneId> {
        let mut inner = self.lock()?;
        inner
            .calls
            .push(Call::Split(pane.clone(), direction, ratio, cwd.into()));
        if self.split_refused || !self.caps.split {
            return Err(MuxError::Unsupported("splitting a pane"));
        }
        inner.require_pane(pane)?;
        Ok(inner.new_pane())
    }

    fn close_pane(&self, pane: &PaneId) -> Result<()> {
        let mut inner = self.lock()?;
        inner.calls.push(Call::ClosePane(pane.clone()));
        if self.close_pane_refused || !self.caps.close_pane {
            return Err(MuxError::Unsupported("closing a pane"));
        }
        if let Some(reason) = &self.close_pane_failure {
            return Err(MuxError::command("the fake host", reason.clone()));
        }
        if let Some(reason) = inner.close_failures.get(pane) {
            return Err(MuxError::command("the fake host", reason.clone()));
        }
        inner.require_pane(pane)?;
        inner.panes.remove(pane);
        inner.screens.remove(pane);
        inner.states.remove(pane);
        inner.read_failures.remove(pane);
        inner.exits.remove(pane);
        Ok(())
    }

    fn write(&self, pane: &PaneId, text: &str, submit: bool) -> Result<()> {
        let mut inner = self.lock()?;
        inner
            .calls
            .push(Call::Write(pane.clone(), text.to_string(), submit));
        inner.require_pane(pane)
    }

    fn keys(&self, pane: &PaneId, keys: &[Key]) -> Result<()> {
        let mut inner = self.lock()?;
        inner.calls.push(Call::Keys(pane.clone(), keys.to_vec()));
        if !self.caps.keys {
            return Err(MuxError::Unsupported("pressing keys"));
        }
        inner.require_pane(pane)
    }

    fn read(&self, pane: &PaneId, lines: usize) -> Result<String> {
        let mut inner = self.lock()?;
        inner.calls.push(Call::Read(pane.clone(), lines));
        inner.require_pane(pane)?;
        if let Some(reason) = inner.read_failures.get(pane) {
            return Err(MuxError::command("read", reason.clone()));
        }
        Ok(inner.screens.get(pane).cloned().unwrap_or_default())
    }

    /// The fake answers in one call, as a host with [`Capabilities::wait`]
    /// does: it reads the scripted answer rather than sleeping. A scripted exit
    /// always wins, so a test that only cares about the exit need not script a
    /// screen.
    fn wait(&self, pane: &PaneId, wanted: &Waited, timeout: Duration) -> Result<Outcome> {
        let mut inner = self.lock()?;
        inner
            .calls
            .push(Call::Wait(pane.clone(), wanted.clone(), timeout));
        inner.require_pane(pane)?;
        if matches!(wanted, Waited::Exit | Waited::Either(_))
            && let Some(code) = inner.exits.get(pane)
        {
            return Ok(Outcome::exited(*code));
        }
        if let Waited::Output(text) | Waited::Either(text) = wanted {
            let screen = inner.screens.get(pane).cloned().unwrap_or_default();
            if screen.contains(text.as_str()) {
                return Ok(Outcome::matched(screen));
            }
        }
        Ok(Outcome::running())
    }

    fn kill(&self, pane: &PaneId) -> Result<()> {
        let mut inner = self.lock()?;
        inner.calls.push(Call::Kill(pane.clone()));
        if !self.caps.kill {
            return Err(MuxError::Unsupported("killing a pane's program"));
        }
        inner.require_pane(pane)?;
        inner.exits.insert(pane.clone(), None);
        inner.states.insert(pane.clone(), AgentState::Done);
        Ok(())
    }

    fn exit_of(&self, pane: &PaneId) -> Result<Option<Outcome>> {
        let inner = self.lock()?;
        inner.require_pane(pane)?;
        Ok(inner.exits.get(pane).map(|code| Outcome::exited(*code)))
    }

    fn pane_process_group(&self, pane: &PaneId) -> Result<Option<u32>> {
        let mut inner = self.lock()?;
        inner.calls.push(Call::PaneProcessGroup(pane.clone()));
        inner.require_pane(pane)?;
        if let Some(group) = inner.process_groups.get(pane) {
            return Ok(*group);
        }
        Ok(self.default_group)
    }

    fn agent_state(&self, pane: &PaneId) -> Result<AgentState> {
        let mut inner = self.lock()?;
        inner.calls.push(Call::AgentState(pane.clone()));
        inner.require_pane(pane)?;
        if let Some((polls, state)) = inner.pending.get_mut(pane) {
            if *polls == 0 {
                return Ok(*state);
            }
            *polls -= 1;
            return Ok(AgentState::Unknown);
        }
        if let Some(state) = inner.states.get(pane) {
            return Ok(*state);
        }
        Ok(if self.ready {
            AgentState::Idle
        } else {
            AgentState::Unknown
        })
    }

    /// Never blocks: everything queued is returned at once, and an empty queue
    /// returns immediately regardless of `timeout`.
    fn subscribe(&self, timeout: Duration) -> Result<Vec<MuxEvent>> {
        let mut inner = self.lock()?;
        inner.calls.push(Call::Subscribe(timeout));
        Ok(inner.events.drain(..).collect())
    }

    fn doc(&self, space: &WorkspaceId, name: &str, body: &str) -> Result<String> {
        let mut inner = self.lock()?;
        inner
            .calls
            .push(Call::Doc(space.clone(), name.to_string(), body.to_string()));
        if !self.caps.doc {
            return Err(MuxError::Unsupported("opening a document"));
        }
        inner.require_space(space)?;
        Ok(inner
            .doc_answer
            .clone()
            .unwrap_or_else(|| format!("doc:{name}")))
    }

    fn exec(&self, argv: &[String], cwd: &Path) -> Result<Outcome> {
        let mut inner = self.lock()?;
        inner.calls.push(Call::Exec(argv.to_vec(), cwd.into()));
        if !self.caps.exec {
            return Err(MuxError::Unsupported("running a one-shot command"));
        }
        Ok(inner.exec_answer.clone().unwrap_or_default())
    }
}

impl Inner {
    /// Fail the way a real host does when a space id is gone.
    fn require_space(&self, space: &WorkspaceId) -> Result<()> {
        if self.space(space).is_some() {
            Ok(())
        } else {
            Err(MuxError::not_found("space", space))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Call, FakeMux};
    use crate::Host;
    use crate::types::{
        AgentState, Capabilities, Direction, Key, MuxError, MuxEvent, Outcome, PaneId, TabId,
        Waited, WorkspaceId,
    };
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
        let space = ok(mux.create_space("shprd · repo · r1 · T1", Path::new("/tmp/repo")));
        let tab = ok(mux.spawn(
            &space.id,
            "claude·T1",
            &["claude".to_string()],
            &[],
            Path::new("/tmp/repo"),
        ));
        (mux, space.id, tab)
    }

    #[test]
    fn records_every_call_in_order() {
        let (mux, space, tab) = fixture();
        ok(mux.focus_tab(&tab.id));
        ok(mux.split(&tab.root_pane, Direction::Right, 0.5, Path::new("/tmp")));
        let calls = mux.calls();
        assert_eq!(
            calls,
            vec![
                Call::CreateSpace("shprd · repo · r1 · T1".to_string(), "/tmp/repo".into()),
                Call::Spawn(
                    space,
                    "claude·T1".to_string(),
                    vec!["claude".to_string()],
                    vec![],
                    "/tmp/repo".into()
                ),
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
            mux.write(&pane, "hi", true),
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
            mux.list_tabs(&WorkspaceId::new("space-404")),
            Err(MuxError::NotFound { what: "space", .. })
        ));
        assert!(matches!(
            mux.close_space(&WorkspaceId::new("space-404")),
            Err(MuxError::NotFound { what: "space", .. })
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
        let events = ok(mux.subscribe(Duration::from_millis(50)));
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
        assert!(ok(mux.subscribe(Duration::ZERO)).is_empty());
    }

    #[test]
    fn argv_and_env_are_recorded_verbatim() {
        let (mux, space, tab) = fixture();
        let argv = vec!["claude".to_string(), "--flag=x y".to_string()];
        let env = vec![
            ("SOME_TOKEN".to_string(), Some("s3cret".to_string())),
            ("NO_COLOR".to_string(), None),
        ];
        ok(mux.spawn(&space, "again", &argv, &env, Path::new("/tmp")));
        assert_eq!(
            mux.calls().last(),
            Some(&Call::Spawn(
                space,
                "again".to_string(),
                argv,
                env,
                "/tmp".into()
            ))
        );
        assert!(
            mux.calls()
                .iter()
                .any(|call| matches!(call, Call::Spawn(..)))
        );
        let _ = tab;
    }

    // ---- one test per verb, since the verbs are what callers build on ----

    #[test]
    fn spawn_puts_a_program_in_a_tab_of_the_space() {
        let (mux, space, tab) = fixture();
        assert_eq!(tab.title, "claude·T1");
        let tabs = ok(mux.list_tabs(&space));
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0].root_pane, tab.root_pane);
        assert!(ok(mux.agent_state(&tab.root_pane)) == AgentState::Unknown);
    }

    #[test]
    fn write_with_and_without_submit_are_told_apart() {
        let (mux, _, tab) = fixture();
        ok(mux.write(&tab.root_pane, "hello", true));
        ok(mux.write(&tab.root_pane, "typing", false));
        let writes: Vec<(String, bool)> = mux
            .calls()
            .into_iter()
            .filter_map(|call| match call {
                Call::Write(_, text, submit) => Some((text, submit)),
                _ => None,
            })
            .collect();
        assert_eq!(
            writes,
            vec![("hello".to_string(), true), ("typing".to_string(), false)]
        );
    }

    #[test]
    fn keys_are_pressed_in_order() {
        let (mux, _, tab) = fixture();
        ok(mux.keys(&tab.root_pane, &[Key::Enter, Key::Esc, Key::Char('y')]));
        assert_eq!(
            mux.calls().last(),
            Some(&Call::Keys(
                tab.root_pane.clone(),
                vec![Key::Enter, Key::Esc, Key::Char('y')]
            ))
        );
    }

    #[test]
    fn wait_answers_the_exit_a_test_scripted() {
        let (mux, _, tab) = fixture();
        mux.set_exited(&tab.root_pane, Some(3));
        let outcome = ok(mux.wait(&tab.root_pane, &Waited::Exit, Duration::from_secs(1)));
        assert_eq!(outcome, Outcome::exited(Some(3)));
        assert!(outcome.exited);
    }

    #[test]
    fn wait_for_output_matches_the_scripted_screen() {
        let (mux, _, tab) = fixture();
        mux.set_screen(&tab.root_pane, "building\ndone\n");
        let outcome = ok(mux.wait(
            &tab.root_pane,
            &Waited::Output("done".to_string()),
            Duration::from_millis(10),
        ));
        assert_eq!(outcome.output.as_deref(), Some("building\ndone\n"));
        assert!(!outcome.exited);
    }

    #[test]
    fn a_wait_that_matches_nothing_times_out_running() {
        let (mux, _, tab) = fixture();
        let outcome = ok(mux.wait(
            &tab.root_pane,
            &Waited::Output("never".to_string()),
            Duration::ZERO,
        ));
        assert_eq!(outcome, Outcome::running());
    }

    #[test]
    fn kill_ends_the_program_and_leaves_the_pane() {
        let (mux, _, tab) = fixture();
        ok(mux.kill(&tab.root_pane));
        assert!(
            ok(mux.wait(&tab.root_pane, &Waited::Exit, Duration::ZERO)).exited,
            "a killed program is gone"
        );
        // The pane still exists: a kill is not a close.
        assert!(ok(mux.agent_state(&tab.root_pane)) == AgentState::Done);
        assert_eq!(
            mux.take_calls()
                .iter()
                .filter(|call| matches!(call, Call::Kill(_)))
                .count(),
            1
        );
    }

    #[test]
    fn a_document_answers_the_handle_the_host_gave() {
        let (mux, space, _) = fixture();
        mux.set_doc_answer("block-7");
        assert_eq!(ok(mux.doc(&space, "graph", "{}")), "block-7");
    }

    #[test]
    fn exec_answers_the_scripted_output_and_code() {
        let (mux, _, _) = fixture();
        mux.set_exec_answer(Some(2), "boom\n");
        let outcome = ok(mux.exec(&["false".to_string()], Path::new("/tmp")));
        assert_eq!(outcome.exit_code, Some(2));
        assert_eq!(outcome.output.as_deref(), Some("boom\n"));
    }

    #[test]
    fn closing_a_space_takes_its_tabs_and_leaves_others_alone() {
        let mux = FakeMux::default();
        let one = ok(mux.create_space("one", Path::new("/tmp")));
        let two = ok(mux.create_space("two", Path::new("/tmp")));
        let tab_one = ok(mux.spawn(&one.id, "a", &[], &[], Path::new("/tmp")));
        let tab_two = ok(mux.spawn(&two.id, "b", &[], &[], Path::new("/tmp")));
        ok(mux.close_space(&one.id));
        assert!(ok(mux.find_space("one")).is_none());
        assert!(ok(mux.find_space("two")).is_some());
        assert!(mux.list_tabs(&one.id).is_err());
        // The other space's tab kept its pane.
        assert_eq!(ok(mux.list_tabs(&two.id)), vec![tab_two]);
        assert!(mux.read(&tab_one.root_pane, 1).is_err());
    }

    #[test]
    fn ensure_space_reuses_the_label_it_finds() {
        let mux = FakeMux::default();
        let first = ok(mux.ensure_space("shprd · repo", Path::new("/tmp")));
        let again = ok(mux.ensure_space("shprd · repo", Path::new("/tmp")));
        assert_eq!(first, again);
        assert_eq!(
            mux.calls()
                .iter()
                .filter(|call| matches!(call, Call::CreateSpace(..)))
                .count(),
            1
        );
    }

    // ---- capability degradation, one test per missing verb ----

    #[test]
    fn a_host_without_spawn_refuses_it_and_says_so() {
        let mut caps = Capabilities::all();
        caps.spawn = false;
        let mux = FakeMux::new(caps);
        let space = ok(mux.create_space("s", Path::new("/tmp")));
        let err = mux
            .spawn(&space.id, "t", &[], &[], Path::new("/tmp"))
            .err()
            .unwrap_or_else(|| panic!("a host without spawn must refuse"));
        assert!(matches!(err, MuxError::Unsupported("spawning a program")));
        assert_eq!(
            err.to_string(),
            "unsupported by this multiplexer: spawning a program"
        );
    }

    #[test]
    fn a_host_without_keys_says_what_it_lacks() {
        let mut caps = Capabilities::all();
        caps.keys = false;
        let mux = FakeMux::new(caps).with_starter_tab();
        let space = ok(mux.create_space("s", Path::new("/tmp")));
        let tab = ok(mux.list_tabs(&space.id)).remove(0);
        let err = mux
            .keys(&tab.root_pane, &[Key::Enter])
            .err()
            .unwrap_or_else(|| panic!("a host without keys must refuse"));
        assert!(matches!(err, MuxError::Unsupported("pressing keys")));
    }

    #[test]
    fn a_host_without_kill_says_what_it_lacks() {
        let mut caps = Capabilities::all();
        caps.kill = false;
        let mux = FakeMux::new(caps).with_starter_tab();
        let space = ok(mux.create_space("s", Path::new("/tmp")));
        let tab = ok(mux.list_tabs(&space.id)).remove(0);
        assert!(matches!(
            mux.kill(&tab.root_pane),
            Err(MuxError::Unsupported("killing a pane's program"))
        ));
    }

    #[test]
    fn a_host_without_doc_says_what_it_lacks() {
        let mut caps = Capabilities::all();
        caps.doc = false;
        let mux = FakeMux::new(caps);
        let space = ok(mux.create_space("s", Path::new("/tmp")));
        assert!(matches!(
            mux.doc(&space.id, "graph", "{}"),
            Err(MuxError::Unsupported("opening a document"))
        ));
    }

    #[test]
    fn a_host_without_exec_says_what_it_lacks() {
        let mut caps = Capabilities::all();
        caps.exec = false;
        let mux = FakeMux::new(caps);
        assert!(matches!(
            mux.exec(&["true".to_string()], Path::new("/tmp")),
            Err(MuxError::Unsupported("running a one-shot command"))
        ));
    }

    #[test]
    fn a_host_without_close_space_says_what_it_lacks() {
        let mux = FakeMux::default().without_close_space();
        let space = ok(mux.create_space("s", Path::new("/tmp")));
        assert!(!mux.caps().close_space);
        assert!(matches!(
            mux.close_space(&space.id),
            Err(MuxError::Unsupported("closing a space"))
        ));
        // And the space is still there, for the caller's tab-by-tab fallback.
        assert!(ok(mux.find_space("s")).is_some());
    }

    #[test]
    fn a_host_without_close_pane_says_what_it_lacks() {
        let mux = FakeMux::default().without_close_pane().with_starter_tab();
        let space = ok(mux.create_space("s", Path::new("/tmp")));
        let tab = ok(mux.list_tabs(&space.id)).remove(0);
        assert!(matches!(
            mux.close_pane(&tab.root_pane),
            Err(MuxError::Unsupported("closing a pane"))
        ));
    }

    #[test]
    fn a_host_without_splits_says_what_it_lacks() {
        let mux = FakeMux::default().without_split().with_starter_tab();
        let space = ok(mux.create_space("s", Path::new("/tmp")));
        let tab = ok(mux.list_tabs(&space.id)).remove(0);
        assert!(matches!(
            mux.split(&tab.root_pane, Direction::Down, 0.5, Path::new("/tmp")),
            Err(MuxError::Unsupported("splitting a pane"))
        ));
    }

    #[test]
    fn a_host_without_a_space_list_says_what_it_lacks() {
        let mut caps = Capabilities::all();
        caps.list_spaces = false;
        let mux = FakeMux::new(caps);
        assert!(matches!(
            mux.list_spaces(),
            Err(MuxError::Unsupported("listing spaces"))
        ));
    }

    #[test]
    fn a_forbidden_spawn_is_a_permission_the_person_can_grant() {
        let mux = FakeMux::default().forbidden_spawn("new-window needs the admin grant");
        let space = ok(mux.create_space("s", Path::new("/tmp")));
        let err = mux
            .spawn(&space.id, "t", &[], &[], Path::new("/tmp"))
            .err()
            .unwrap_or_else(|| panic!("a forbidden spawn must be refused"));
        assert!(matches!(err, MuxError::Forbidden { verb: "spawn", .. }));
        assert!(err.to_string().contains("admin grant"));
    }

    #[test]
    fn the_default_wait_polls_a_host_that_has_none_of_its_own() {
        // `wait` is answered by the fake directly, but the *trait's* fallback is
        // what a host without the capability uses: this exercises it through a
        // fake whose scripted screen already holds the answer.
        let (mux, _, tab) = fixture();
        mux.set_screen(&tab.root_pane, "ready\n");
        let outcome = ok(crate::wait_by_polling(
            |lines| mux.read(&tab.root_pane, lines),
            || mux.exit_of(&tab.root_pane),
            &Waited::Output("ready".to_string()),
            Duration::from_millis(50),
        ));
        assert!(outcome.output.is_some());
    }
}
