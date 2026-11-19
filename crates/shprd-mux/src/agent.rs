//! Agents without tabs: the vocabulary of [`crate::Host::spawn_agent`].
//!
//! # Design (SHPR-23): agents run the Shepherdr v3.1.34 way
//!
//! Decision of 2026-11-18, replacing `docs/decisions.md` 2026-11-14's "space per
//! task": **no tab and no space per task or agent.** A worker or a reviewer is a
//! *headless, labelled terminal*; the only layout shprd makes is **one space per
//! repository** (`shprd · <repo folder>`, the label shprd already used for the
//! coordinator's space, kept so existing windows are reused) holding the
//! coordinator's own tab. A person reaches an agent through its task's thread,
//! which binds a terminal block to the agent's terminal id.
//!
//! ## Starting one
//!
//! Shepherdr's `launch.py` runs `hc run --bg --keep --parent <coordinator>
//! --outlive-parent --label shprd.task=<id> --label shprd.task.space=<space>
//! --label shprd.agent=<program> --label shprd.coordinator=<tab> [--label
//! shprd.role=reviewer]`, and **never** `herdr.space`/`herdr.tab`, which is what
//! would make the window draw a tab for it. [`AgentLaunch`] carries exactly those
//! facts and the Herdr desktop adapter turns it into one `terminal.run`
//! (`shprd-mux-hc`): the labels are `shprd.task`, `shprd.task.space`,
//! `shprd.agent`, `shprd.role` and, under a coordinator, `shprd.coordinator`
//! with `parent` set and `outlive_parent` on, so a restarted coordinator does not
//! take its workers with it. The spike in the SHPR-23 notes confirmed (private
//! core, no window): a terminal with those labels and no placement labels appears
//! in no layout, can be read, written and killed by address, and after its
//! program exits stays `state: finished` with its output readable until killed.
//! A review starts the same way, as a second agent labelled `role=reviewer` with
//! the worker's task and the coordinator as parent (the worker's helper is the
//! worker's *child* only in Shepherdr's chat-driven flow; in shprd the engine
//! starts the review, so the coordinator is the parent).
//!
//! ## Reading, writing, stopping
//!
//! The handle [`Host::spawn_agent`](crate::Host::spawn_agent) returns is a
//! [`Tab`](crate::Tab) whose `root_pane` is the terminal, so everything that
//! types, reads or asks the state of an agent (`write`, `read`, `agent_state`,
//! `wait`) is the existing pane verbs. The tab id is a *handle*:
//! [`Host::close_tab`](crate::Host::close_tab) on it **stops the agent** (a
//! terminal kill on Herdr desktop; the core keeps the finished record, so the
//! agent stays listed as finished and its output stays readable), which is how landing, dropping, a review's
//! verdict and a run's end already stop agents through their `.tab` records, with
//! no cascade through parentage relied on. [`Host::list_agents`](crate::Host::list_agents)
//! lists them (from the host's own fold, by label) so a caller can find a handle
//! without a layout.
//!
//! ## Hosts that keep tabs
//!
//! herdr TUI and tuios have no terminal without a pane. They report
//! [`Capabilities::headless_agents`](crate::Capabilities) false, and the default
//! [`spawn_agent`](crate::Host::spawn_agent) is a plain tab of the *repository's*
//! space (not a space per task), titled by the launch. Nothing else changes: the
//! handle is the tab, [`list_agents`](crate::Host::list_agents) is empty (their
//! tabs are found by `list_tabs`), and `close_tab` closes it.
//!
//! ## The coordinator
//!
//! A coordinator is an agent whose role is [`AgentRole::Coordinator`]. It is the
//! one agent that **does** keep a tab, in the repository's space, because it is
//! what a person talks to there; `shprd serve` starts it (program from the
//! `coordinator` role's settings) instead of leaving the space's starter shell,
//! and only ever types news into a pane it started and that is still running.

use crate::{AgentState, PaneId, SpaceId, Tab};
use std::path::Path;

/// What an agent is for, as the labels and the sidebar name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentRole {
    /// Works on a task in its worktree.
    Worker,
    /// Reviews a task's attempt.
    Reviewer,
    /// The repository's own agent: it is told the news and takes the person's
    /// orders. The only role that keeps a tab.
    Coordinator,
}

impl AgentRole {
    /// The word on the `shprd.role` label and in `/api/state`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Worker => "worker",
            Self::Reviewer => "reviewer",
            Self::Coordinator => "coordinator",
        }
    }

    /// The role a `shprd.role` label names; `None` for a word shprd does not use.
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "worker" => Some(Self::Worker),
            "reviewer" => Some(Self::Reviewer),
            "coordinator" => Some(Self::Coordinator),
            _ => None,
        }
    }
}

/// One agent to start: its program and everything the host labels it with.
#[derive(Debug, Clone, Copy)]
pub struct AgentLaunch<'a> {
    /// The repository's space: where a host that keeps tabs puts one, and what
    /// `shprd.task.space` says for a host that does not.
    pub space: &'a SpaceId,
    /// What a tab is titled when the host has to make one, and the agent's name.
    pub title: &'a str,
    /// The program and its arguments.
    pub argv: &'a [String],
    /// Environment for the new process only (`None` removes a variable).
    pub env: &'a [(String, Option<String>)],
    /// Where the program starts.
    pub cwd: &'a Path,
    /// What the agent is for.
    pub role: AgentRole,
    /// The task it works on; `None` for a coordinator.
    pub task: Option<&'a str>,
    /// The harness id of the program, e.g. `claude`.
    pub program: &'a str,
    /// The terminal that started it, when there is one: the space's
    /// coordinator, whose tab `shprd.coordinator` names.
    pub parent: Option<&'a PaneId>,
}

/// One agent a host reports, found by its labels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveAgent {
    /// The handle [`crate::Host::spawn_agent`] returned: `root_pane` is the
    /// terminal, and closing the id stops the agent.
    pub tab: Tab,
    /// The task it works on; `None` for a coordinator.
    pub task: Option<String>,
    /// What it is for.
    pub role: AgentRole,
    /// The harness id of its program.
    pub program: String,
    /// The repository space it belongs to (`shprd.task.space`).
    pub space: SpaceId,
    /// What the host says it is doing.
    pub state: AgentState,
    /// Whether its program is still running.
    pub live: bool,
    /// When it started, in epoch milliseconds, when the host says.
    pub started_ms: Option<u64>,
    /// When its program ended, in epoch milliseconds, when it has.
    pub ended_ms: Option<u64>,
}
