//! Value types shared by every multiplexer adapter.
//!
//! Identifiers are newtypes so a tab id can never be passed where a pane id is
//! wanted, and so each adapter stays free to use whatever string its
//! multiplexer hands back.
//!
//! The vocabulary is the one Herdr desktop, herdr TUI and tuios all use: a
//! **space** (a herdr workspace, a tuios session) holds **tabs**, a tab holds
//! **panes**, and a pane runs a program. shprd's own word for it is "space", and
//! [`Workspace`] is the same thing under the name the hosts use.

use std::fmt;

use thiserror::Error;

/// Identifies a workspace — a **space** — inside a multiplexer: the container a
/// task's tabs live in.
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

/// Identifies a tab (one agent's terminal) inside a space.
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

/// shprd's word for a [`Workspace`]: the host space one task's tabs live in.
pub type Space = Workspace;

/// The identifier of a [`Space`].
pub type SpaceId = WorkspaceId;

/// What a multiplexer can actually do.
///
/// The orchestrator never probes a feature by trying it: it reads this once per
/// adapter and degrades the plan for anything the host cannot offer. A host that
/// advertises nothing is still usable — a whole run works with nothing but
/// [`Capabilities::spawn`], [`crate::Host::read`] and
/// [`Capabilities::agent_state`].
///
/// Every set field has a matching trait verb that answers
/// [`MuxError::Unsupported`] when the field is false, so "the host cannot do
/// this" is answered the same way whether it is asked before or after the
/// attempt. `false` is what a host that is merely old, or running behind a
/// grant it does not hold, reports: `shprd-mux-herdr` maps tuios's `forbidden`
/// replies here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// A program can be started as a tab of a space: a new tab, its pane
    /// running `argv`. Without it only a workspace's own starter pane can hold
    /// a process.
    pub spawn: bool,
    /// An existing pane can be split into a second one.
    pub split: bool,
    /// `split` honours a ratio; a host without this ignores the ratio and
    /// splits evenly.
    pub split_ratio: bool,
    /// A pane can be closed on its own, leaving its tab and its siblings
    /// alone. Without it, retiring one pane costs the whole tab.
    pub close_pane: bool,
    /// Panes can be handed extra permissions (e.g. pre-approved tool grants)
    /// at creation time.
    pub pane_grants: bool,
    /// A pane can ask a human for input and wait for the answer.
    pub human_ask: bool,
    /// `keys` can press named keys. Without it a prompt is only ever typed
    /// text plus the host's own submit ([`crate::Host::spawn`]'s `--submit` as it
    /// were), so a flow needing escape or arrow keys degrades to a refusal.
    pub keys: bool,
    /// `write` can leave the text at the cursor (`write(…, submit = false)`).
    ///
    /// Without it the only way in is paste-then-submit, so keystroke-level work
    /// that must not press enter — typing half a line, filling a field before
    /// the point is confirmed — is refused rather than submitted
    /// ([`crate::Host::write`]).
    pub type_without_submit: bool,
    /// `wait` is answered by the host rather than by polling [`crate::Host::read`].
    pub wait: bool,
    /// `kill` ends a pane's program. Without it a pane can only be closed,
    /// which takes its siblings with it.
    pub kill: bool,
    /// `subscribe` replays events that happened while nobody was reading.
    pub resumable_events: bool,
    /// The host has documents (Herdr desktop's `herdr-app` layout document):
    /// a block or a file can be opened beside a task's terminals.
    pub doc: bool,
    /// A one-shot command can be run without a pane, and its output and exit
    /// code read back.
    pub exec: bool,
    /// The host reports whether an agent is working, idle, blocked or done.
    pub agent_state: bool,
    /// The host carries a program's *own* report of its state — herdr-core's
    /// `pane.report_agent`, which is what a program's plugin or hook calls.
    ///
    /// This is deliberately separate from [`Self::agent_state`]. A program may
    /// report its status itself (OSC 7501 on its terminal, `pi`'s own reporter)
    /// or through a plugin the host installs (`opencode`, `kilo`, `codex`, and
    /// `claude`'s hooks), and the two paths are not the same method. herdr-core
    /// 0.1.21 has `terminal.status` and OSC 7501 but no `pane.report_agent`, so
    /// its `agent_state` is true while a plugin's report goes nowhere: the
    /// plugin silently no-ops and the program never reports. A program whose
    /// only status source is such a plugin therefore has no status at all on
    /// this host, and its readiness has to be read another way — which is what
    /// [`shprd_agents::spec::ReadySpec`] is for.
    pub agent_reports: bool,
    /// A space can be closed whole, taking every tab in it. Without it a task's
    /// space has to be emptied tab by tab and stays on the host's bar.
    pub close_space: bool,
    /// Spaces can be listed, so a caller can say which labels the host has when
    /// the one it wanted is missing.
    pub list_spaces: bool,
    /// A worker or reviewer can run as a labelled terminal with no tab and no
    /// space ([`crate::Host::spawn_agent`]). Without it every agent is a tab of
    /// the repository's space.
    pub headless_agents: bool,
}

impl Capabilities {
    /// Everything on: the fake host's default, for tests that do not care which
    /// features the host has.
    pub const fn all() -> Self {
        Self {
            spawn: true,
            split: true,
            split_ratio: true,
            close_pane: true,
            pane_grants: true,
            human_ask: true,
            keys: true,
            type_without_submit: true,
            wait: true,
            kill: true,
            resumable_events: true,
            doc: true,
            exec: true,
            agent_state: true,
            agent_reports: true,
            close_space: true,
            list_spaces: true,
            headless_agents: true,
        }
    }

    /// Only the verbs no host can do without: start a process in a tab, read a
    /// screen, ask what an agent is doing.
    pub const fn minimal() -> Self {
        Self {
            spawn: true,
            split: false,
            split_ratio: false,
            close_pane: false,
            pane_grants: false,
            human_ask: false,
            keys: false,
            type_without_submit: false,
            wait: false,
            kill: false,
            resumable_events: false,
            doc: false,
            exec: false,
            agent_state: true,
            agent_reports: false,
            close_space: false,
            list_spaces: false,
            headless_agents: false,
        }
    }
}

/// Which way a split puts the new pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// The new pane goes to the right; panes sit side by side.
    Right,
    /// The new pane goes below; panes stack.
    Down,
}

/// A key a [`crate::Host::keys`] call may press, named the way every host in
/// M7g's socket table names it.
///
/// The list is deliberately the portable subset: herdr's `pane send-keys` and
/// tuios's herdr-compatible shim both accept these words, and herdr-core's
/// `terminal.keys` names them the same way (M7ef §2.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    /// Return / Enter.
    Enter,
    /// Escape.
    Esc,
    /// Tab.
    Tab,
    /// Up arrow.
    Up,
    /// Down arrow.
    Down,
    /// Left arrow.
    Left,
    /// Right arrow.
    Right,
    /// Page up.
    PageUp,
    /// Page down.
    PageDown,
    /// Backspace.
    Backspace,
    /// Delete.
    Delete,
    /// Home.
    Home,
    /// End.
    End,
    /// Ctrl-C, which interrupts the program in front.
    CtrlC,
    /// A printable character, as one key press.
    Char(char),
}

impl Key {
    /// The key's word, as the hosts accept it.
    pub fn word(&self) -> String {
        match self {
            Self::Enter => "enter".to_string(),
            Self::Esc => "esc".to_string(),
            Self::Tab => "tab".to_string(),
            Self::Up => "up".to_string(),
            Self::Down => "down".to_string(),
            Self::Left => "left".to_string(),
            Self::Right => "right".to_string(),
            Self::PageUp => "pageup".to_string(),
            Self::PageDown => "pagedown".to_string(),
            Self::Backspace => "backspace".to_string(),
            Self::Delete => "delete".to_string(),
            Self::Home => "home".to_string(),
            Self::End => "end".to_string(),
            Self::CtrlC => "ctrl+c".to_string(),
            Self::Char(letter) => letter.to_string(),
        }
    }
}

/// What a program did, as the host saw it: the answer to [`crate::Host::wait`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Outcome {
    /// The program ended (exited or was killed). `false` means the wait timed
    /// out while it kept running.
    pub exited: bool,
    /// The exit code, when the host knows it.
    pub exit_code: Option<i32>,
    /// The screen text that matched, when the wait was for output.
    pub output: Option<String>,
}

impl Outcome {
    /// A wait that timed out with the program still running.
    pub fn running() -> Self {
        Self::default()
    }

    /// A program that ended with `code`.
    pub fn exited(code: Option<i32>) -> Self {
        Self {
            exited: true,
            exit_code: code,
            output: None,
        }
    }

    /// A wait that saw the output it looked for.
    pub fn matched(text: impl Into<String>) -> Self {
        Self {
            exited: false,
            exit_code: None,
            output: Some(text.into()),
        }
    }
}

/// What a [`crate::Host::wait`] is waiting for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Waited {
    /// The pane's program ends, however it ends.
    Exit,
    /// The pane's screen shows this text.
    Output(String),
    /// Either of the two above, whichever happens first.
    Either(String),
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

/// A space (a herdr workspace, a tuios session), as found or created by an
/// adapter.
///
/// shprd makes one space per repository, labelled from its folder, for the
/// coordinator's tab; tasks and agents get none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    /// The host's identifier for the space.
    pub id: WorkspaceId,
    /// The space's name, which the caller picks: shprd names a repository's
    /// after its folder.
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
    /// A space appeared or went away, when the host can say.
    Space {
        /// The space that changed.
        space: WorkspaceId,
        /// True when it was created, false when it was closed.
        created: bool,
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
    ///
    /// The message is the *capability's* name, exactly as a caller reads it off
    /// [`Capabilities`], e.g. `"closing a space"`.
    #[error("unsupported by this multiplexer: {0}")]
    Unsupported(&'static str),
    /// The host refused: it knows the verb, and this caller is not allowed it.
    ///
    /// tuios answers `forbidden` where herdr answers `ok`, because creating a
    /// pane or a space needs the `admin` grant a plugin may not hold (M7g §2.7).
    /// That is not a broken host and not a missing verb: it is a permission the
    /// person has to grant, so it carries a line telling them how.
    #[error("this host refused the {verb}: {reason}")]
    Forbidden {
        /// The verb that was refused, e.g. `"spawn"`.
        verb: &'static str,
        /// What the host said, and what a person can do about it.
        reason: String,
    },
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

    /// Build a [`MuxError::Unsupported`] naming the capability `what` the host
    /// does not advertise.
    pub fn unsupported(what: &'static str) -> Self {
        Self::Unsupported(what)
    }

    /// Build a [`MuxError::Forbidden`] from the verb the host refused and what a
    /// person can do about it.
    pub fn forbidden(verb: &'static str, reason: impl Into<String>) -> Self {
        Self::Forbidden {
            verb,
            reason: reason.into(),
        }
    }

    /// True when the host no longer knows the object a caller was tidying up,
    /// which for every close in this workspace means "already done".
    pub fn is_gone(&self) -> bool {
        matches!(self, Self::NotFound { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::{Capabilities, Direction, Key, MuxError, Outcome, Waited};

    #[test]
    fn the_key_words_are_the_ones_every_host_accepts() {
        let words: Vec<String> = [
            Key::Enter,
            Key::Esc,
            Key::CtrlC,
            Key::PageUp,
            Key::Char('a'),
        ]
        .iter()
        .map(Key::word)
        .collect();
        assert_eq!(words, ["enter", "esc", "ctrl+c", "pageup", "a"]);
    }

    #[test]
    fn a_capability_error_names_the_capability() {
        let error = MuxError::unsupported("closing a space");
        assert!(matches!(error, MuxError::Unsupported("closing a space")));
        assert_eq!(
            error.to_string(),
            "unsupported by this multiplexer: closing a space"
        );
    }

    #[test]
    fn a_forbidden_error_says_what_the_person_can_do() {
        let error = MuxError::forbidden("spawn", "grant the admin permission");
        assert!(error.to_string().contains("spawn"));
        assert!(error.to_string().contains("grant the admin permission"));
        assert!(!error.is_gone());
    }

    #[test]
    fn a_vanished_object_is_gone_but_a_refusal_is_not() {
        assert!(MuxError::not_found("tab", "w1:t1").is_gone());
        assert!(!MuxError::command("close tab", "no").is_gone());
    }

    #[test]
    fn the_minimal_host_still_spawns_and_reads_state() {
        let caps = Capabilities::minimal();
        assert!(caps.spawn);
        assert!(caps.agent_state);
        assert!(!caps.split);
        assert!(!caps.keys);
        assert!(!caps.type_without_submit);
        assert!(!caps.close_space);
        assert!(!caps.doc);
        assert!(!caps.exec);
    }

    #[test]
    fn every_capability_is_on_in_the_full_set() {
        let caps = Capabilities::all();
        assert!(caps.type_without_submit);
        assert!(caps.split_ratio);
        assert!(caps.pane_grants);
        assert!(caps.human_ask);
        assert!(caps.wait);
        assert!(caps.kill);
        assert!(caps.resumable_events);
        assert!(caps.list_spaces);
        assert!(caps.exec);
        assert!(caps.agent_state);
        assert!(caps.agent_reports);
    }

    #[test]
    fn an_outcome_says_which_way_the_wait_ended() {
        assert!(!Outcome::running().exited);
        assert!(Outcome::exited(Some(2)).exited);
        assert_eq!(Outcome::exited(Some(2)).exit_code, Some(2));
        assert_eq!(Outcome::matched("done").output.as_deref(), Some("done"));
        assert_eq!(
            Waited::Either("ok".to_string()),
            Waited::Either(String::from("ok"))
        );
    }

    #[test]
    fn a_direction_is_either_way_a_split_can_go() {
        assert_ne!(Direction::Right, Direction::Down);
    }
}
