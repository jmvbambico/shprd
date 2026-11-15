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
//! # One space per task
//!
//! The layout is [`Space`]s (a herdr workspace, a tuios session), each holding
//! [`Tab`]s, each holding [`PaneId`]s. shprd gives **a task its own space**,
//! labelled from the task, and keeps one repository-level space
//! (`shprd · <folder>`) for the coordinator. [`Host::ensure_space`] finds or
//! makes one, [`Host::close_space`] removes a task's when it lands.
//!
//! # Shape of the trait
//!
//! The trait is deliberately process-shaped rather than shell-shaped: a program
//! starts from an argv, never from a string, so a branch name or a title can
//! never turn into a command. Text that is meant as a shell line is typed
//! through [`Host::write`], which the caller chose to do.

pub mod fake;
pub mod types;

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

    /// Type `text` into `pane`, submitting it (pressing enter) when `submit`.
    ///
    /// With `submit`, this is one turn of input for an agent and one line for a
    /// shell — which is why it takes a flag rather than a thinner verb: a host
    /// presses its own enter, so shprd never has to send a newline byte as text.
    /// Without it the text lands at the cursor unsent, for keystroke-level work.
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

/// The space shprd gives one task: its name, derived from the task and the run.
///
/// One task, one space: a person reading the host's bar sees which task each
/// group of terminals belongs to, and closing a task's space closes exactly its
/// tabs. The repository's own space ([`repo_space_name`]) stays, for the
/// coordinator.
pub fn task_space_name(repo_name: &str, run: &str, task: &str) -> String {
    format!("shprd · {repo_name} · {run} · {task}")
}

/// The space the coordinator's tabs live in, named after the repository folder.
///
/// This is the one space a run does not own, so it outlives every task and is
/// only ever emptied, never closed.
pub fn repo_space_name(repo_name: &str) -> String {
    format!("shprd · {repo_name}")
}
