//! Value types shared by every multiplexer adapter.
//!
//! Identifiers are newtypes so a tab id can never be passed where a pane id is
//! wanted, and so each adapter stays free to use whatever string its
//! multiplexer hands back.

use std::fmt;

use thiserror::Error;

/// Identifies a workspace (a herdr/tuios window, roughly a repository) inside
/// a multiplexer.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WorkspaceId(String);

impl WorkspaceId {
    /// Wraps a multiplexer-native identifier.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The multiplexer-native identifier, as handed back by the adapter.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for WorkspaceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Identifies a tab (one agent's terminal) inside a workspace.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TabId(String);

impl TabId {
    /// Wraps a multiplexer-native identifier.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The multiplexer-native identifier, as handed back by the adapter.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TabId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Identifies a pane: the process-bearing rectangle a program runs in.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PaneId(String);

impl PaneId {
    /// Wraps a multiplexer-native identifier.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The multiplexer-native identifier, as handed back by the adapter.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PaneId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a multiplexer can actually do.
///
/// The orchestrator never probes a feature by trying it: it reads this once per
/// adapter and degrades the plan for anything the host cannot offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// `split` honours a ratio; a host without this ignores the ratio and
    /// splits evenly.
    pub split_ratio: bool,
    /// Panes can be handed extra permissions (e.g. pre-approved tool grants)
    /// at creation time.
    pub pane_grants: bool,
    /// A pane can be closed on its own, leaving its tab and its siblings
    /// alone. Without it, retiring one pane costs the whole tab.
    pub close_pane: bool,
    /// A pane can ask a human for input and wait for the answer.
    pub human_ask: bool,
    /// `next_events` replays events that happened while nobody was reading.
    pub resumable_events: bool,
    /// The host reports whether an agent is working, idle, blocked or done.
    pub agent_state: bool,
}

/// Which way a split puts the new pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// The new pane goes to the right; panes sit side by side.
    Right,
    /// The new pane goes below; panes stack.
    Down,
}

/// What an agent in a pane is doing, as the host reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    /// The agent is producing output or running tools.
    Working,
    /// The agent finished its turn and waits for input.
    Idle,
    /// The agent is waiting for a human answer or a permission grant.
    Blocked,
    /// The agent's process ended.
    Done,
    /// The host cannot tell; treat as "no signal", not as failure.
    Unknown,
}

/// A tab, as listed or created by an adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tab {
    /// The host's identifier for the tab.
    pub id: TabId,
    /// The visible title, which shprd owns and keeps reproducible.
    pub title: String,
    /// The pane the tab was created with; splits from here carry the tab.
    pub root_pane: PaneId,
}

/// A workspace, as found or created by an adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    /// The host's identifier for the workspace.
    pub id: WorkspaceId,
    /// The workspace name; see [`crate::naming::workspace_name`].
    pub name: String,
}

/// Something the host observed and queued for the orchestrator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MuxEvent {
    /// An agent's reported state changed.
    AgentState {
        /// The affected pane.
        pane: PaneId,
        /// The state the agent is now in.
        state: AgentState,
    },
    /// A pane's program exited (the pane may or may not still exist).
    PaneExited {
        /// The pane whose program ended.
        pane: PaneId,
    },
    /// A host-specific event this crate does not model. Never load-bearing;
    /// unknown events are surfaced for logging only.
    Other(String),
}

/// Why a multiplexer operation failed.
#[derive(Debug, Error)]
pub enum MuxError {
    /// The host has nothing under that identifier — the object was closed, or
    /// never existed.
    #[error("{what} {id} does not exist")]
    NotFound {
        /// What kind of object was looked up, e.g. `"pane"`.
        what: &'static str,
        /// The identifier as it was given.
        id: String,
    },
    /// The adapter cannot do this; the message names the missing capability so
    /// the caller can degrade instead of retrying.
    #[error("unsupported by this multiplexer: {0}")]
    Unsupported(&'static str),
    /// The host ran a command and it failed.
    #[error("{context}: {detail}")]
    Command {
        /// What shprd was trying to do, e.g. `"create tab"`.
        context: String,
        /// The host's own diagnosis, verbatim.
        detail: String,
    },
}

impl MuxError {
    /// Build a [`MuxError::NotFound`] for a kind of object and its id.
    pub fn not_found(what: &'static str, id: impl fmt::Display) -> Self {
        Self::NotFound {
            what,
            id: id.to_string(),
        }
    }

    /// Build a [`MuxError::Command`] from a context and the host's detail.
    pub fn command(context: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::Command {
            context: context.into(),
            detail: detail.into(),
        }
    }
}
