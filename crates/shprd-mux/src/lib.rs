//! Host adapter trait + capability flags.
//!
//! Everything shprd does to a terminal multiplexer goes through [`Mux`]: open a
//! workspace per repository, a tab per agent, a pane per process, and read back
//! what the agent is doing. `shprd-mux-herdr` implements it for herdr today;
//! `shprd-mux-tuios` will implement the same trait later, and
//! [`fake::FakeMux`] implements it for tests in other crates.
//!
//! The trait is deliberately process-shaped rather than shell-shaped: programs
//! start from an argv, never from a string, so a branch name or a title can
//! never turn into a command.

pub mod fake;
pub mod naming;
pub mod types;

use std::path::Path;
use std::time::Duration;

pub use types::{
    AgentState, Capabilities, Direction, MuxError, MuxEvent, PaneId, Tab, TabId, Workspace,
    WorkspaceId,
};

/// What every [`Mux`] method returns.
pub type Result<T> = std::result::Result<T, MuxError>;

/// The terminal multiplexer shprd drives.
///
/// Implementations must be `Send + Sync`: the orchestrator holds one adapter
/// and calls it from the scheduler task and the event pump at the same time.
/// Every method that takes an id returns [`MuxError::NotFound`] when the host
/// does not know it, so a race with a human closing a tab is an ordinary error
/// and never a panic.
pub trait Mux: Send + Sync {
    /// What this host can do, consulted once per adapter before a plan is
    /// built. Implementations must answer from the host's version, not by
    /// trying the operation.
    fn capabilities(&self) -> Capabilities;

    /// Find a workspace by name. `Ok(None)` means "no such workspace", which
    /// is not an error — the caller will create it.
    fn find_workspace(&self, name: &str) -> Result<Option<Workspace>>;

    /// Create a workspace named `name`, with its first pane started in `cwd`.
    /// Fails with [`MuxError::Command`] if the host refuses (e.g. duplicate
    /// name) rather than silently returning the existing one.
    fn create_workspace(&self, name: &str, cwd: &Path) -> Result<Workspace>;

    /// Bring a workspace to the front for the human watching the screen. Has no
    /// effect on any process; safe to call on every status poll.
    fn focus_workspace(&self, workspace: &WorkspaceId) -> Result<()>;

    /// Every tab in a workspace, in the host's display order.
    fn list_tabs(&self, workspace: &WorkspaceId) -> Result<Vec<Tab>>;

    /// Create a tab in `workspace` titled `title`, whose first pane starts in
    /// `cwd`. The returned [`Tab`] carries the root pane every later split is
    /// made from.
    fn create_tab(&self, workspace: &WorkspaceId, title: &str, cwd: &Path) -> Result<Tab>;

    /// Retitle an existing tab. Idempotent: renaming to the current title
    /// succeeds.
    fn rename_tab(&self, tab: &TabId, title: &str) -> Result<()>;

    /// Select a tab, so its panes are the visible ones.
    fn focus_tab(&self, tab: &TabId) -> Result<()>;

    /// Close a tab and everything in it. Closing an already-closed tab returns
    /// [`MuxError::NotFound`]; callers that race a human should ignore it.
    fn close_tab(&self, tab: &TabId) -> Result<()>;

    /// Close one pane, leaving its tab and its siblings alone.
    ///
    /// Used to retire a pane whose program shprd did not start, once a
    /// replacement is running beside it. A host that cannot close a pane on
    /// its own answers [`MuxError::Unsupported`], which is the trait's "this
    /// host cannot do that" signal; callers must degrade rather than retry.
    /// Such a host reports [`Capabilities::close_pane`] as false, so a caller
    /// that would be left with an unwanted process can tell before planning.
    fn close_pane(&self, _pane: &PaneId) -> Result<()> {
        Err(MuxError::Unsupported("close pane"))
    }

    /// Split `pane`, returning the new pane. `ratio` is the share of the split
    /// axis the *existing* pane keeps, in `0.0..=1.0`; hosts without
    /// [`Capabilities::split_ratio`] ignore it and split evenly. The new pane
    /// starts in `cwd`.
    fn split(&self, pane: &PaneId, direction: Direction, ratio: f32, cwd: &Path) -> Result<PaneId>;

    /// Start `argv` as the pane's program. Never a shell string.
    ///
    /// `env` applies to the new process only: `Some(v)` sets the variable,
    /// `None` removes it from the inherited environment. Order is preserved,
    /// so a later entry for the same key wins.
    fn run(&self, pane: &PaneId, argv: &[String], env: &[(String, Option<String>)]) -> Result<()>;

    /// Type `text` into the pane without submitting it. Used for keystroke-level
    /// interaction; prefer [`Mux::prompt`] when the target is an agent.
    fn send_text(&self, pane: &PaneId, text: &str) -> Result<()>;

    /// Submit `text` to the agent running in `pane` as one turn of input: the
    /// host writes it and presses enter. This is the only way shprd talks to an
    /// agent, so a prompt can never be mistaken for a shell command.
    fn prompt(&self, pane: &PaneId, text: &str) -> Result<()>;

    /// The last `lines` lines of a pane's screen, as the host renders them.
    /// Trailing blank lines are the host's business; callers trim.
    fn read(&self, pane: &PaneId, lines: usize) -> Result<String>;

    /// The agent state the host reports for `pane`. Returns
    /// [`AgentState::Unknown`] — not an error — when the host has no opinion or
    /// does not support state at all (see [`Capabilities::agent_state`]).
    fn agent_state(&self, pane: &PaneId) -> Result<AgentState>;

    /// The process group the host sees in the foreground of `pane`.
    ///
    /// This is the identity shprd uses to tell a process *it* started from one
    /// something else did — a host that restored the pane with a plain
    /// `--resume`, say. A process group survives an `exec`, so a pane whose
    /// shell replaced itself with an agent still reports that agent's group.
    ///
    /// `Ok(None)` means the host cannot tell, which is not an error: callers
    /// keep their conservative behaviour (leave the pane alone) rather than
    /// read "no group" as "not mine". A pane the host does not know is
    /// [`MuxError::NotFound`], never `Ok(None)`: the pane's process is gone,
    /// which is a different answer from a live pane with no group to report,
    /// and the one callers replace.
    fn pane_process_group(&self, _pane: &PaneId) -> Result<Option<u32>> {
        Ok(None)
    }

    /// Wait up to `timeout` for events and return everything queued so far.
    ///
    /// An empty vector means the timeout expired with nothing to report, which
    /// is normal and not an error. With [`Capabilities::resumable_events`],
    /// events produced before the call are still delivered; without it, events
    /// that happened while nobody was reading are lost.
    fn next_events(&self, timeout: Duration) -> Result<Vec<MuxEvent>>;
}
