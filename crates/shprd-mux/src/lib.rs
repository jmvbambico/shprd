//! The host adapter trait: the nine verbs every multiplexer shprd drives can
//! answer, plus the capability flags that say which ones a given host cannot.
//!
//! Everything shprd does to a terminal host goes through [`Host`]. The verbs are
//! the ones Herdr desktop (through `hc`), herdr TUI and tuios (through tuios's
//! herdr-compatible shim) all understand, which is why they are named for what
//! a caller wants rather than for any one host's CLI:
//!
//! | verb | what it is for |
//! |---|---|
//! | [`Host::spawn`] | start a program in a tab of a task's space |
//! | [`Host::write`] | type text into a pane, submittable or not |
//! | [`Host::keys`] | press named keys (enter, esc, arrows) |
//! | [`Host::read`] | the screen or scrollback tail of a pane |
//! | [`Host::wait`] | block until a program exits or its screen shows something |
//! | [`Host::kill`] | end a pane's program |
//! | [`Host::subscribe`] | status and lifecycle events |
//! | [`Host::doc`] | open a document or file block, where the host has one |
//! | [`Host::exec`] | one-shot command, output and exit code |
//!
//! A host advertises what it lacks in [`Host::capabilities`], and every verb it
//! does not have answers [`MuxError::Unsupported`] naming the capability. The
//! two answers are the same fact said twice so that a caller may check first
//! (the plan) and still degrade if it forgot (the act).
//!
//! # One space per repository, no tab per agent
//!
//! The layout is [`Space`]s (a herdr workspace, a tuios session), each holding
//! [`Tab`]s, each holding [`PaneId`]s. shprd keeps **one space per repository**
//! (`shprd · <folder>`) for its coordinator. Workers and reviewers get no space
//! and no tab: [`Host::spawn_agent`] starts them as labelled headless terminals
//! where the host can ([`Capabilities::headless_agents`]) and as a tab of that
//! space where it cannot. See [`agent`] for the design.
//!
//! # Shape of the trait
//!
//! The trait is deliberately process-shaped rather than shell-shaped: a program
//! starts from an argv, never from a string, so a branch name or a title can
//! never turn into a command. Text that is meant as a shell line is typed
//! through [`Host::write`], which the caller chose to do.

pub mod agent;
pub mod fake;
pub mod types;

pub use agent::{AgentLaunch, AgentRole, LiveAgent};

use std::path::Path;
use std::time::Duration;

pub use types::{
    AgentState, Capabilities, Direction, Key, MuxError, MuxEvent, Outcome, PaneId, Space, SpaceId,
    Tab, TabId, Waited, Workspace, WorkspaceId,
};

/// What every [`Host`] method returns.
pub type Result<T> = std::result::Result<T, MuxError>;

/// The terminal multiplexer shprd drives.
///
/// Implementations must be `Send + Sync`: the orchestrator holds one adapter
/// and calls it from the scheduler task and the event pump at the same time.
/// Every method that takes an id returns [`MuxError::NotFound`] when the host
/// does not know it, so a race with a human closing a tab is an ordinary error
/// and never a panic.
pub trait Host: Send + Sync {
    /// What this host can do, consulted once per adapter before a plan is
    /// built.
    ///
    /// Implementations answer from the host's version, from what a plugin was
    /// granted (tuios) and from a refusal the host has already given — never by
    /// trying an operation for the purpose of finding out. `shprd-mux-herdr` is
    /// the example of the last one: a `forbidden` reply marks that verb, and the
    /// flag stops promising it from then on.
    fn capabilities(&self) -> Capabilities;

    /// The space shprd calls `name`, or `Ok(None)` when the host has none.
    ///
    /// `None` is not an error: the caller will make it. A host that cannot list
    /// its spaces still answers `None` or `Some` for the label it was asked
    /// about; it just cannot say what else it has, which
    /// [`Capabilities::list_spaces`] is about.
    fn find_space(&self, name: &str) -> Result<Option<Space>>;

    /// The space shprd marked as `mark`, wherever it is called now.
    ///
    /// shprd's own label on a space, kept apart from the display name so a
    /// person can rename the space and shprd still finds it. `Ok(None)` when
    /// nothing carries the mark (or the host cannot keep one). A mark whose
    /// space is gone is `None` too.
    fn marked_space(&self, _mark: &str) -> Result<Option<Space>> {
        Ok(None)
    }

    /// Put shprd's `mark` on `space`, replacing the space the mark named before.
    ///
    /// The mark lives on the host's own record of the space, not on anything
    /// inside it, so a space with no terminals keeps it. A host that cannot keep
    /// a mark answers [`MuxError::Unsupported`], and the caller falls back to
    /// the name.
    fn mark_space(&self, _space: &SpaceId, _mark: &str) -> Result<()> {
        Err(MuxError::Unsupported("marking a space"))
    }

    /// Give `space` the display name `name`.
    ///
    /// The id and the mark stay. A host that cannot rename answers
    /// [`MuxError::Unsupported`], and the caller leaves the name as it is.
    fn rename_space(&self, _space: &SpaceId, _name: &str) -> Result<()> {
        Err(MuxError::Unsupported("renaming a space"))
    }

    /// The spaces whose folder is `folder`, in the host's display order.
    ///
    /// A host that does not record a space's folder answers none: the caller
    /// then falls back to names.
    fn spaces_in_folder(&self, _folder: &Path) -> Result<Vec<Space>> {
        Ok(Vec::new())
    }

    /// Every coordinator agent the host has, running or ended, whichever space
    /// it was started for. [`Host::list_agents`] lists the headless agents only
    /// (a coordinator keeps a tab), so this is the one place a coordinator
    /// shprd started earlier can be found again.
    fn list_coordinators(&self) -> Result<Vec<LiveAgent>> {
        Ok(Vec::new())
    }

    /// Whether `pane` runs another product's agent (Shepherdr's per-space
    /// Assistant), which shprd never adopts, types into or counts. A host that
    /// cannot tell answers `false`.
    fn is_foreign_agent(&self, _pane: &PaneId) -> bool {
        false
    }

    /// Every space the host has, in the host's display order.
    ///
    /// Only where a name cannot be guessed: a refusal is worth reading when it
    /// says which label shprd expected *and* which ones the host has, and there
    /// is no other way to learn the latter. A host that cannot list answers
    /// [`MuxError::Unsupported`], which is the trait's "cannot do that" signal;
    /// callers then say only what they expected, rather than failing.
    fn list_spaces(&self) -> Result<Vec<Space>> {
        Err(MuxError::Unsupported("listing spaces"))
    }

    /// Find the space called `name`, or make it with its first pane in `cwd`.
    ///
    /// Only that label is reused and only that label is created: anything else
    /// on the host is left alone, so two services starting at once cannot give a
    /// task a second space, and a space a human made with an almost-right label
    /// is never adopted as shprd's own.
    ///
    /// The default is [`Host::find_space`] then [`Host::create_space`], with the
    /// loser of a creation race reading back what the winner made.
    fn ensure_space(&self, name: &str, cwd: &Path) -> Result<Space> {
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

    /// Create a space named `name`, with its first pane started in `cwd`.
    /// Fails with [`MuxError::Command`] if the host refuses (e.g. duplicate
    /// name) rather than silently returning the existing one, and with
    /// [`MuxError::Forbidden`] when the host knows the verb and will not allow
    /// this caller it (tuios without the `admin` grant).
    fn create_space(&self, name: &str, cwd: &Path) -> Result<Space>;

    /// Close a space and everything in it.
    ///
    /// This is how a landed task's space goes away, in one call rather than one
    /// per tab. A space the host has already forgotten is [`MuxError::NotFound`],
    /// which a caller tidying up reads as "already done". A host without
    /// [`Capabilities::close_space`] answers [`MuxError::Unsupported`] and the
    /// caller empties the space tab by tab instead.
    fn close_space(&self, _space: &SpaceId) -> Result<()> {
        Err(MuxError::Unsupported("closing a space"))
    }

    /// Bring a space to the front for the human watching the screen. Has no
    /// effect on any process; safe to call on every status poll.
    fn focus_space(&self, space: &SpaceId) -> Result<()>;

    /// Every tab in a space, in the host's display order.
    fn list_tabs(&self, space: &SpaceId) -> Result<Vec<Tab>>;

    /// Retitle an existing tab. Idempotent: renaming to the current title
    /// succeeds.
    fn rename_tab(&self, tab: &TabId, title: &str) -> Result<()>;

    /// Select a tab, so its panes are the visible ones.
    fn focus_tab(&self, tab: &TabId) -> Result<()>;

    /// Close a tab and everything in it. Closing an already-closed tab returns
    /// [`MuxError::NotFound`]; callers that race a human should ignore it.
    fn close_tab(&self, tab: &TabId) -> Result<()>;

    /// Start `argv` as a new tab's program in `space`, with the tab titled
    /// `title` and its pane starting in `cwd`.
    ///
    /// This is the verb a spawn uses: one call makes the tab shprd will name in
    /// its records and starts the agent in it, so a half-made tab is not a state
    /// a caller has to undo. `env` applies to the new process only: `Some(v)`
    /// sets the variable, `None` removes it from the inherited environment.
    /// Order is preserved, so a later entry for the same key wins.
    ///
    /// A host that cannot start a program in a tab answers
    /// [`MuxError::Unsupported`] when [`Capabilities::spawn`] is false.
    fn spawn(
        &self,
        space: &SpaceId,
        title: &str,
        argv: &[String],
        env: &[(String, Option<String>)],
        cwd: &Path,
    ) -> Result<Tab>;

    /// Start one agent: a worker, a reviewer or the coordinator.
    ///
    /// Where [`Capabilities::headless_agents`] is true (Herdr desktop) a worker
    /// or reviewer is a labelled terminal with **no tab and no space**, found by
    /// [`Host::list_agents`], read and typed into through its `root_pane`, and
    /// stopped by [`Host::close_tab`] on the returned handle. The coordinator
    /// keeps a tab in `launch.space`. Everywhere else — the default — it is
    /// [`Host::spawn`]: a tab of `launch.space` titled `launch.title`.
    fn spawn_agent(&self, launch: &AgentLaunch<'_>) -> Result<Tab> {
        self.spawn(
            launch.space,
            launch.title,
            launch.argv,
            launch.env,
            launch.cwd,
        )
    }

    /// Every agent [`Host::spawn_agent`] started that the host still has, live
    /// or finished and not yet stopped, found by label rather than by layout.
    ///
    /// A host that keeps tabs answers an empty list: its agents are tabs and
    /// [`Host::list_tabs`] finds them.
    fn list_agents(&self) -> Result<Vec<LiveAgent>> {
        Ok(Vec::new())
    }

    /// Split `pane` into a second pane, returning the new one.
    ///
    /// `ratio` is the share of the split axis the *existing* pane keeps, in
    /// `0.0..=1.0`; hosts without [`Capabilities::split_ratio`] ignore it and
    /// split evenly. The new pane starts in `cwd`. A host without
    /// [`Capabilities::split`] answers [`MuxError::Unsupported`].
    fn split(&self, pane: &PaneId, direction: Direction, ratio: f32, cwd: &Path) -> Result<PaneId>;

    /// Close one pane, leaving its tab and its siblings alone.
    ///
    /// Used to retire a pane whose program shprd did not start, once a
    /// replacement is running beside it. A host that cannot close a pane on
    /// its own answers [`MuxError::Unsupported`], which is the trait's "this
    /// host cannot do that" signal; callers must degrade rather than retry.
    /// Such a host reports [`Capabilities::close_pane`] as false, so a caller
    /// that would be left with an unwanted process can tell before planning.
    fn close_pane(&self, _pane: &PaneId) -> Result<()> {
        Err(MuxError::Unsupported("closing a pane"))
    }

    /// The name of every tab in `space`, mod-only tabs included, with its id.
    ///
    /// [`Host::list_tabs`] lists only tabs a program runs in. This is for the
    /// question "is that name taken?", which another product's tab answers too:
    /// the default is the listed tabs' titles.
    fn tab_names(&self, space: &SpaceId) -> Result<Vec<(TabId, String)>> {
        Ok(self
            .list_tabs(space)?
            .into_iter()
            .map(|tab| (tab.id, tab.title))
            .collect())
    }

    /// Show the agent in `pane` as a chat over `chat_space`'s conversation, in
    /// the pane's own place: the chat takes the pane's spot and the terminal
    /// keeps running behind it, one switch away (Shepherdr's Chat · Terminal).
    ///
    /// A host with no window answers [`MuxError::Unsupported`], and the agent
    /// stays a plain terminal tab.
    fn show_chat(&self, _pane: &PaneId, _chat_space: &str) -> Result<()> {
        Err(MuxError::Unsupported("showing a chat"))
    }

    /// Type `text` into `pane`, submitting it (pressing enter) when `submit`.
    ///
    /// With `submit`, this is one turn of input for an agent and one line for a
    /// shell — which is why it takes a flag rather than a thinner verb: a host
    /// presses its own enter, so shprd never has to send a newline byte as text.
    /// Without it the text lands at the cursor unsent, for keystroke-level work;
    /// a host that cannot do that reports
    /// [`Capabilities::type_without_submit`] false and answers
    /// [`MuxError::Unsupported`] here.
    fn write(&self, pane: &PaneId, text: &str, submit: bool) -> Result<()>;

    /// Press `keys` in `pane`, in order.
    ///
    /// A host without [`Capabilities::keys`] answers
    /// [`MuxError::Unsupported`]: pressing escape or an arrow key is not
    /// something a caller can pretend it did.
    fn keys(&self, _pane: &PaneId, _keys: &[Key]) -> Result<()> {
        Err(MuxError::Unsupported("pressing keys"))
    }

    /// The last `lines` lines of a pane's screen — its scrollback tail as the
    /// host renders it. Trailing blank lines are the host's business; callers
    /// trim.
    fn read(&self, pane: &PaneId, lines: usize) -> Result<String>;

    /// Wait up to `timeout` for `wanted` in `pane`.
    ///
    /// Returns what happened: the program exited, the screen showed the output
    /// that was waited for, or (an [`Outcome`] with `exited == false` and no
    /// `output`) the timeout expired with the program still running. A timeout
    /// is normal, not an error. A host with [`Capabilities::wait`] answers in
    /// one call; the default implementation polls [`Host::read`] (and
    /// [`Host::exit_of`]) instead.
    fn wait(&self, pane: &PaneId, wanted: &Waited, timeout: Duration) -> Result<Outcome> {
        wait_by_polling(
            |lines| self.read(pane, lines),
            || self.exit_of(pane),
            wanted,
            timeout,
        )
    }

    /// End the program running in `pane`, leaving the pane itself.
    ///
    /// A host without [`Capabilities::kill`] answers
    /// [`MuxError::Unsupported`], and a caller closes the pane instead. A pane
    /// the host no longer knows is [`MuxError::NotFound`], the same answer every
    /// other verb gives for a vanished object.
    fn kill(&self, _pane: &PaneId) -> Result<()> {
        Err(MuxError::Unsupported("killing a pane's program"))
    }

    /// The agent state the host reports for `pane`. Returns
    /// [`AgentState::Unknown`] — not an error — when the host has no opinion or
    /// does not support state at all (see [`Capabilities::agent_state`]).
    fn agent_state(&self, pane: &PaneId) -> Result<AgentState>;

    /// Whether the program in `pane` has ended, as far as the host can tell.
    ///
    /// `Ok(None)` means the host cannot say, which is not an error and not an
    /// exit: callers keep their conservative reading. A pane the host no longer
    /// knows is [`MuxError::NotFound`], never `Ok(None)`: a vanished pane is a
    /// process that stopped, and a caller that reads it as "cannot tell" never
    /// replaces it. The default implementation derives the answer from
    /// [`Host::agent_state`].
    fn exit_of(&self, pane: &PaneId) -> Result<Option<Outcome>> {
        Ok(match self.agent_state(pane)? {
            AgentState::Done => Some(Outcome::exited(None)),
            _ => None,
        })
    }

    /// The process group the host sees in the foreground of `pane`.
    ///
    /// This is the identity shprd uses to tell a process *it* started from one
    /// something else did — a host that restored the pane with a plain
    /// `--resume`, say. A process group survives an `exec`, so a pane whose
    /// shell replaced itself with an agent still reports that agent's group.
    ///
    /// `Ok(None)` means the host cannot tell, which is not an error: callers
    /// keep their conservative behaviour (leave the pane alone) rather than
    /// read "no group" as "not mine".
    fn pane_process_group(&self, _pane: &PaneId) -> Result<Option<u32>> {
        Ok(None)
    }

    /// Wait up to `timeout` for events and return everything queued so far.
    ///
    /// An empty vector means the timeout expired with nothing to report, which
    /// is normal and not an error. With [`Capabilities::resumable_events`],
    /// events produced before the call are still delivered; without it, events
    /// that happened while nobody was reading are lost.
    fn subscribe(&self, timeout: Duration) -> Result<Vec<MuxEvent>>;

    /// Open `document` where the host has documents: a layout entry, a mod, a
    /// file block beside a task's terminals.
    ///
    /// `body` is what to show, when the host renders content rather than a file
    /// the document names. Returns whatever id or handle the host gave, which
    /// is opaque to shprd and only ever handed back to the host. A host without
    /// [`Capabilities::doc`] answers [`MuxError::Unsupported`], and the caller
    /// puts the same text in a pane instead.
    fn doc(&self, _space: &SpaceId, _name: &str, _body: &str) -> Result<String> {
        Err(MuxError::Unsupported("opening a document"))
    }

    /// Run `argv` once, without a pane, and answer its exit code and streams.
    ///
    /// This is for work that must not depend on a shell or a terminal: a
    /// one-shot command whose output is read rather than watched. A host
    /// without [`Capabilities::exec`] answers [`MuxError::Unsupported`], and the
    /// caller runs the same argv itself with `std::process`.
    fn exec(&self, _argv: &[String], _cwd: &Path) -> Result<Outcome> {
        Err(MuxError::Unsupported("running a one-shot command"))
    }
}

/// A boxed or shared host is a host: every verb goes to the host it wraps.
///
/// The wiring edge picks one adapter at run time — Herdr desktop's `hc`, the
/// herdr TUI, tuios — and hands it down as `Box<dyn Host>` or `Arc<dyn Host>` so
/// a service can keep one host for its whole life. Without these, every caller
/// that holds the choice would have to reach through it with `&*`, and the verbs
/// would read `mux.as_ref().spawn(…)` instead of `mux.spawn(…)`.
///
/// Every verb is forwarded verbatim, defaults included: boxing a host never
/// changes what it answers.
macro_rules! forward_a_wrapper {
    ($($wrapper:ty),* $(,)?) => {
        $(
            impl<T: Host + ?Sized> Host for $wrapper {
                fn capabilities(&self) -> Capabilities {
                    (**self).capabilities()
                }

                fn find_space(&self, name: &str) -> Result<Option<Space>> {
                    (**self).find_space(name)
                }

                fn list_spaces(&self) -> Result<Vec<Space>> {
                    (**self).list_spaces()
                }

                fn ensure_space(&self, name: &str, cwd: &Path) -> Result<Space> {
                    (**self).ensure_space(name, cwd)
                }

                fn create_space(&self, name: &str, cwd: &Path) -> Result<Space> {
                    (**self).create_space(name, cwd)
                }

                fn close_space(&self, space: &SpaceId) -> Result<()> {
                    (**self).close_space(space)
                }

                fn focus_space(&self, space: &SpaceId) -> Result<()> {
                    (**self).focus_space(space)
                }

                fn list_tabs(&self, space: &SpaceId) -> Result<Vec<Tab>> {
                    (**self).list_tabs(space)
                }

                fn rename_tab(&self, tab: &TabId, title: &str) -> Result<()> {
                    (**self).rename_tab(tab, title)
                }

                fn focus_tab(&self, tab: &TabId) -> Result<()> {
                    (**self).focus_tab(tab)
                }

                fn close_tab(&self, tab: &TabId) -> Result<()> {
                    (**self).close_tab(tab)
                }

                fn spawn(
                    &self,
                    space: &SpaceId,
                    title: &str,
                    argv: &[String],
                    env: &[(String, Option<String>)],
                    cwd: &Path,
                ) -> Result<Tab> {
                    (**self).spawn(space, title, argv, env, cwd)
                }

                fn spawn_agent(&self, launch: &AgentLaunch<'_>) -> Result<Tab> {
                    (**self).spawn_agent(launch)
                }

                fn list_agents(&self) -> Result<Vec<LiveAgent>> {
                    (**self).list_agents()
                }

                fn split(
                    &self,
                    pane: &PaneId,
                    direction: Direction,
                    ratio: f32,
                    cwd: &Path,
                ) -> Result<PaneId> {
                    (**self).split(pane, direction, ratio, cwd)
                }

                fn close_pane(&self, pane: &PaneId) -> Result<()> {
                    (**self).close_pane(pane)
                }

                fn tab_names(&self, space: &SpaceId) -> Result<Vec<(TabId, String)>> {
                    (**self).tab_names(space)
                }

                fn show_chat(&self, pane: &PaneId, chat_space: &str) -> Result<()> {
                    (**self).show_chat(pane, chat_space)
                }

                fn write(&self, pane: &PaneId, text: &str, submit: bool) -> Result<()> {
                    (**self).write(pane, text, submit)
                }

                fn keys(&self, pane: &PaneId, keys: &[Key]) -> Result<()> {
                    (**self).keys(pane, keys)
                }

                fn read(&self, pane: &PaneId, lines: usize) -> Result<String> {
                    (**self).read(pane, lines)
                }

                fn wait(&self, pane: &PaneId, wanted: &Waited, timeout: Duration) -> Result<Outcome> {
                    (**self).wait(pane, wanted, timeout)
                }

                fn kill(&self, pane: &PaneId) -> Result<()> {
                    (**self).kill(pane)
                }

                fn agent_state(&self, pane: &PaneId) -> Result<AgentState> {
                    (**self).agent_state(pane)
                }

                fn exit_of(&self, pane: &PaneId) -> Result<Option<Outcome>> {
                    (**self).exit_of(pane)
                }

                fn pane_process_group(&self, pane: &PaneId) -> Result<Option<u32>> {
                    (**self).pane_process_group(pane)
                }

                fn subscribe(&self, timeout: Duration) -> Result<Vec<MuxEvent>> {
                    (**self).subscribe(timeout)
                }

                fn marked_space(&self, mark: &str) -> Result<Option<Space>> {
                    (**self).marked_space(mark)
                }

                fn mark_space(&self, space: &SpaceId, mark: &str) -> Result<()> {
                    (**self).mark_space(space, mark)
                }

                fn rename_space(&self, space: &SpaceId, name: &str) -> Result<()> {
                    (**self).rename_space(space, name)
                }

                fn spaces_in_folder(&self, folder: &Path) -> Result<Vec<Space>> {
                    (**self).spaces_in_folder(folder)
                }

                fn list_coordinators(&self) -> Result<Vec<LiveAgent>> {
                    (**self).list_coordinators()
                }

                fn is_foreign_agent(&self, pane: &PaneId) -> bool {
                    (**self).is_foreign_agent(pane)
                }

                fn doc(&self, space: &SpaceId, name: &str, body: &str) -> Result<String> {
                    (**self).doc(space, name, body)
                }

                fn exec(&self, argv: &[String], cwd: &Path) -> Result<Outcome> {
                    (**self).exec(argv, cwd)
                }
            }
        )*
    };
}

forward_a_wrapper!(Box<T>, std::sync::Arc<T>);

/// [`Host::wait`]'s default: read the screen until it matches, or the program
/// ends, or the timeout runs out.
///
/// The poll interval is short enough that a caller cannot tell the difference
/// for a screen wait, and the first read happens before any sleep so an answer
/// that is already on screen costs no wait at all.
pub(crate) fn wait_by_polling(
    read: impl Fn(usize) -> Result<String>,
    exit: impl Fn() -> Result<Option<Outcome>>,
    wanted: &Waited,
    timeout: Duration,
) -> Result<Outcome> {
    /// How long between two reads when the host has no `wait` of its own.
    const POLL: Duration = Duration::from_millis(100);
    let started = std::time::Instant::now();
    loop {
        if let Waited::Output(text) | Waited::Either(text) = wanted {
            // A screen too long to hold the needle is read in a window big
            // enough for a whole terminal, which is all a prompt or a result
            // line ever needs.
            let screen = read(SCREEN_LINES)?;
            if screen.contains(text.as_str()) {
                return Ok(Outcome::matched(screen));
            }
        }
        if let Some(outcome) = exit()?
            && matches!(wanted, Waited::Exit | Waited::Either(_))
        {
            return Ok(outcome);
        }
        let waited = started.elapsed();
        if waited >= timeout {
            return Ok(Outcome::running());
        }
        std::thread::sleep(POLL.min(timeout - waited));
    }
}

/// How many lines a polling wait reads: a full screen, so a prompt or a result
/// line is never cut off.
const SCREEN_LINES: usize = 200;

/// The space the coordinator's tabs live in, named after the repository folder.
///
/// This is the one space shprd makes: it outlives every task and is only ever
/// emptied, never closed.
pub fn repo_space_name(repo_name: &str) -> String {
    format!("shprd · {repo_name}")
}

/// The space shprd keeps under `name`, found by shprd's own mark first.
///
/// A person may rename the space, so the display name is not the identity: the
/// mark is. Order: the space carrying the mark; else (back-compat, a space made
/// before marks existed) the space with that exact name; else a space already
/// open on `cwd` (one repository is one space, so a space an older shprd or a
/// person made for the same folder is adopted, never shown beside a second);
/// else a new space made in `cwd`. The space found or made is marked, and
/// given `name` when the host shows it by its raw id, by no name at all or by
/// its folder's name, because a person reads the label on the sidebar. A name
/// a person chose is left alone. Returns the space and whether this call made
/// it. A host that cannot keep marks or rename still works by name.
pub fn ensure_marked_space(host: &dyn Host, name: &str, cwd: &Path) -> Result<(Space, bool)> {
    ensure_marked_space_from(host, name, cwd, &[])
}

/// [`ensure_marked_space`], also adopting a plain space open on one of `also`.
///
/// A person who adds a folder *inside* a repository gets a space on that
/// subfolder; the repository's root is where shprd works, so the space on the
/// folder the person added is the one adopted, not a second one made beside it.
pub fn ensure_marked_space_from(
    host: &dyn Host,
    name: &str,
    cwd: &Path,
    also: &[&Path],
) -> Result<(Space, bool)> {
    ensure_marked_space_detail(host, name, cwd, also).map(|(space, created, _)| (space, created))
}

/// [`ensure_marked_space_from`], also saying whether the space was *adopted
/// fresh*: one the window made a moment ago (shown by its raw id, no name or its
/// folder's name) and shprd had never marked. The window's own add-folder flow
/// makes such a space with a default shell tab; the caller may tidy that tab
/// once, because after this call the space carries shprd's mark and name and is
/// never "fresh" again.
pub fn ensure_marked_space_detail(
    host: &dyn Host,
    name: &str,
    cwd: &Path,
    also: &[&Path],
) -> Result<(Space, bool, bool)> {
    let marked_before = matches!(host.marked_space(name), Ok(Some(_)));
    let known = match host.marked_space(name)? {
        Some(space) => Some(space),
        None => match host.find_space(name)? {
            Some(space) => Some(space),
            None => same_folder_space(host, name, cwd).or_else(|| {
                also.iter()
                    .find_map(|folder| same_folder_space(host, name, folder))
            }),
        },
    };
    let (mut space, created) = match known {
        Some(space) => (space, false),
        None => match host.create_space(name, cwd) {
            Ok(space) => (space, true),
            // The loser of a creation race reads back what the winner made.
            Err(err) => match host.find_space(name) {
                Ok(Some(space)) => (space, false),
                _ => return Err(err),
            },
        },
    };
    let auto = shows_as_auto(&space, name, cwd);
    if auto && host.rename_space(&space.id, name).is_ok() {
        space.name = name.to_string();
    }
    match host.mark_space(&space.id, name) {
        Ok(()) | Err(MuxError::Unsupported(_)) => {}
        Err(err) => return Err(err),
    }
    Ok((space, created, !created && auto && !marked_before))
}

/// The space already open on `cwd` that is not another repository's label: the
/// one a repository adopts instead of making a second.
fn same_folder_space(host: &dyn Host, label: &str, cwd: &Path) -> Option<Space> {
    let spaces = host.spaces_in_folder(cwd).ok()?;
    spaces
        .into_iter()
        .find(|space| space.name == label || !space.name.starts_with(SPACE_PREFIX))
}

/// What every label starts with.
const SPACE_PREFIX: &str = "shprd · ";

/// Whether a host shows `space` by something the window made up rather than by
/// a name a person chose: its raw id, nothing, or its folder's name.
fn shows_as_auto(space: &Space, label: &str, cwd: &Path) -> bool {
    if space.name == label {
        return false;
    }
    let name = space.name.trim();
    let folder = cwd
        .file_name()
        .map(|folder| folder.to_string_lossy().to_string())
        .unwrap_or_default();
    name.is_empty()
        || name == space.id.as_str()
        || (!folder.is_empty() && name.eq_ignore_ascii_case(&folder))
}

/// The space shprd keeps under `name` if the host has it: by mark first, then
/// (a space nobody has marked yet) by name. Never creates or marks anything.
pub fn find_marked_space(host: &dyn Host, name: &str) -> Result<Option<Space>> {
    match host.marked_space(name)? {
        Some(space) => Ok(Some(space)),
        None => host.find_space(name),
    }
}
