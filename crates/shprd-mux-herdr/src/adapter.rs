//! The [`shprd_mux::Host`] implementation itself: one thin method per trait
//! method, each delegating to the module that owns that part of herdr.
//!
//! # Capabilities, and why the two hosts disagree
//!
//! herdr 0.9.5 and tuios 0.12/0.13 answer the same CLI (M7g §2.6), so the shape
//! of every verb is shared; what differs is what each one will *allow*. tuios
//! runs the herdr plugin through its compatibility layer and refuses any verb
//! that creates a pane or a space unless the plugin holds the `admin` grant,
//! answering `{"code":"forbidden", …}` where herdr answers `ok` (M7g §2.7).
//!
//! So the capability flags here are the portable subset both hosts promise, and
//! a `forbidden` reply becomes [`shprd_mux::MuxError::Forbidden`] carrying the
//! host's own instructions — "grant the admin permission" is a thing a person
//! can do, unlike "this host is broken". The flags do not try to predict the
//! grant: the adapter answers from the protocol, which is why
//! [`shprd_mux::Capabilities::spawn`] is `true` even on a tuios install whose
//! plugin was installed without `admin`. The refusal is the host's answer, and
//! it is the one a caller has to read.

use std::path::Path;
use std::time::Duration;

use shprd_mux::{
    AgentState, Capabilities, Direction, Host, Key, MuxEvent, Outcome, PaneId, Result, Space,
    SpaceId, Tab, TabId, Waited,
};

use crate::HerdrMux;

impl Host for HerdrMux {
    /// Always the same answer: the verbs herdr 0.9.5 answers are the ones a
    /// terminal host can answer.
    ///
    /// The two `false` flags are measurements, not modesty: herdr cannot grant
    /// extra permissions to a pane (tuios can, through `TUIOS_PANE_GRANTS`) and
    /// cannot ask a human from inside a pane (tuios has `ask-human`). The CLI
    /// exposes no `exec` either, so a one-shot command is the caller's own
    /// process; and no document store, which is a `hc`/Herdr desktop fact
    /// rather than an herdr TUI one.
    fn capabilities(&self) -> Capabilities {
        // A verb this host has already refused is not promised again: tuios
        // refuses the creating verbs without the `admin` grant, and a caller
        // that reads the flags before planning deserves that answer rather than
        // a second refusal.
        let refused = self.refused();
        Capabilities {
            spawn: !refused.spawn,
            split: !refused.split,
            split_ratio: !refused.split,
            close_pane: true,
            pane_grants: false,
            human_ask: false,
            keys: true,
            // `pane send-text` types without pressing enter; a submit goes
            // through `agent prompt` (or send-text plus Enter).
            type_without_submit: true,
            // `pane wait-output` exists on both, and is what `wait` uses.
            wait: true,
            kill: true,
            // `herdr api snapshot` is the only event source the CLI offers, and
            // it cannot replay.
            resumable_events: false,
            doc: false,
            exec: false,
            agent_state: true,
            // herdr 0.9.5 has `pane report-agent` / `pane report-agent-session`
            // (M7g), which is the method a program's plugin calls, so a plugin's
            // report does reach this host.
            agent_reports: true,
            close_space: !refused.close_space,
            list_spaces: true,
            headless_agents: false,
        }
    }

    fn find_space(&self, name: &str) -> Result<Option<Space>> {
        self.lookup_workspace(name)
    }

    fn list_spaces(&self) -> Result<Vec<Space>> {
        Ok(self
            .workspaces()?
            .into_iter()
            .map(|(id, name)| Space {
                id: SpaceId::new(id),
                name,
            })
            .collect())
    }

    fn create_space(&self, name: &str, cwd: &Path) -> Result<Space> {
        self.new_workspace(name, cwd)
    }

    /// Find the space called `name`, or make it with its first pane in `cwd`.
    ///
    /// The trait's default does exactly this, but writing it out says why the
    /// two host families differ here: a tuios plugin without the `admin` grant
    /// gets `forbidden` from `workspace create`, and the answer a person needs
    /// is the host's own ("grant the admin permission"), which is what
    /// [`shprd_mux::MuxError::Forbidden`] carries.
    fn ensure_space(&self, name: &str, cwd: &Path) -> Result<Space> {
        if let Some(space) = self.lookup_workspace(name)? {
            return Ok(space);
        }
        match self.new_workspace(name, cwd) {
            Ok(space) => Ok(space),
            Err(error) => match self.lookup_workspace(name) {
                Ok(Some(space)) => Ok(space),
                _ => Err(error),
            },
        }
    }

    fn close_space(&self, space: &SpaceId) -> Result<()> {
        self.kill_workspace(space)
    }

    fn focus_space(&self, space: &SpaceId) -> Result<()> {
        self.focus_workspace_by_id(space)
    }

    fn list_tabs(&self, space: &SpaceId) -> Result<Vec<Tab>> {
        self.tabs_in(space)
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

    fn spawn(
        &self,
        space: &SpaceId,
        title: &str,
        argv: &[String],
        env: &[(String, Option<String>)],
        cwd: &Path,
    ) -> Result<Tab> {
        self.spawn_in_space(space, title, argv, env, cwd)
    }

    fn split(&self, pane: &PaneId, direction: Direction, ratio: f32, cwd: &Path) -> Result<PaneId> {
        self.split_pane(pane, direction, ratio, cwd)
    }

    fn close_pane(&self, pane: &PaneId) -> Result<()> {
        self.close_pane_of(pane)
    }

    fn write(&self, pane: &PaneId, text: &str, submit: bool) -> Result<()> {
        self.write_to(pane, text, submit)
    }

    fn keys(&self, pane: &PaneId, keys: &[Key]) -> Result<()> {
        self.press_keys(pane, keys)
    }

    fn read(&self, pane: &PaneId, lines: usize) -> Result<String> {
        self.read_pane(pane, lines)
    }

    fn wait(&self, pane: &PaneId, wanted: &Waited, timeout: Duration) -> Result<Outcome> {
        self.wait_in(pane, wanted, timeout)
    }

    fn kill(&self, pane: &PaneId) -> Result<()> {
        self.kill_pane(pane)
    }

    fn agent_state(&self, pane: &PaneId) -> Result<AgentState> {
        self.state_of(pane)
    }

    /// Whether the program in `pane` has ended, from `pane process-info`.
    ///
    /// herdr has no exit code to offer, so the answer is "the host no longer
    /// knows this pane", which is [`shprd_mux::MuxError::NotFound`] exactly as the trait
    /// says. `pane get` would answer the same question, but process-info is the
    /// call a waiting loop makes anyway.
    fn exit_of(&self, pane: &PaneId) -> Result<Option<shprd_mux::Outcome>> {
        match self.process_group_of(pane) {
            Ok(_) => Ok(None),
            Err(error) if error.is_gone() => Err(error),
            Err(error) => Err(error),
        }
    }

    fn pane_process_group(&self, pane: &PaneId) -> Result<Option<u32>> {
        self.process_group_of(pane)
    }

    fn subscribe(&self, timeout: Duration) -> Result<Vec<MuxEvent>> {
        self.poll_events(timeout)
    }
}
