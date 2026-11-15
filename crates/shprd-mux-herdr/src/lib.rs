//! Herdr adapter: [`HerdrMux`] implements [`shprd_mux::Host`] by shelling out to
//! the `herdr` binary.
//!
//! Every call runs one `herdr` process and reads the single JSON object the CLI
//! prints (`{"id":…,"result":…}` or `{"id":…,"error":{"code":…,"message":…}}`),
//! so no state is kept in this crate and a restarted herdr server is invisible
//! to the adapter. The binary, the socket path and `HOME` are configuration: a
//! test or a sandbox points them at a throwaway directory and the adapter can
//! never reach the human's own herdr session.
//!
//! # No guard against the human's own session
//!
//! The adapter does not refuse a cwd outside some "safe" directory, nor a call
//! against the default socket: there is no clean way to tell the human's own
//! session from a private one — a person may legitimately run shprd against the
//! session they are working in, and a temp cwd may legitimately be a task's
//! worktree (a sandboxed agent builds one there). A rule keyed on
//! `std::env::temp_dir()` or on "the socket looks default" would be real
//! behaviour that only fires on guesses, and it would refuse the person's own
//! work before it ever helped. Isolation is therefore the caller's rule, stated
//! once in `tests/support/herdr_env.rs` (`isolate`): every test that spawns the
//! binary starts from a socket that does not exist.
//!
//! What the adapter does do is refuse to carry the caller's pane into a server it
//! was told to use: an adapter built with [`HerdrMux::with_environment`] removes
//! `HERDR_PANE_ID` and the other pane variables from every command, because
//! `herdr` treats them as the default target and a command aimed at a private
//! server must never land in the pane that started shprd.
//!
//! Programs are process-shaped, not shell-shaped: [`shprd_mux::Host::spawn`]
//! takes an argv and this adapter quotes it element by element before handing it
//! to the pane, so a branch name or a title is never re-parsed as shell syntax.
//!
//! Events come from [`shprd_mux::Host::subscribe`], which polls
//! `herdr api snapshot` and diffs it: herdr 0.9.5 exposes `events.subscribe` and
//! `events.wait` on the socket but nothing on the CLI, so events produced while
//! nobody was polling are lost and [`shprd_mux::Capabilities::resumable_events`]
//! is `false`.
//!
//! # Spaces, and tuios
//!
//! A herdr workspace is what the trait calls a space, and a tuios session is the
//! same thing under the shim. shprd gives each task one, labelled from the task
//! (`shprd · <repo> · <run> · <task>`), and keeps the repository's own
//! (`shprd · <repo>`) for the coordinator's tabs; `workspace close` is what
//! removes a landed task's space in one call.
//!
//! tuios runs this same CLI through its compatibility layer and refuses anything
//! that creates a pane or a space without the `admin` grant, answering
//! `{"code":"forbidden", …}` where herdr answers `ok` (M7g §2.7). That refusal is
//! mapped to [`shprd_mux::MuxError::Forbidden`], which carries the host's own
//! instructions so a person can grant what is missing; every other host error
//! stays a [`shprd_mux::MuxError::Command`].

mod adapter;
mod cli;
mod events;
mod panes;
mod shell;
mod tabs;
mod workspaces;

use std::path::{Path, PathBuf};

/// Drives one herdr server through the `herdr` command line.
///
/// It holds no connection, so every method is one independent process; the only
/// state is the last snapshot [`shprd_mux::Host::subscribe`] saw, behind a mutex
/// so the adapter is `Send + Sync` and can be shared by the scheduler and the
/// event pump at once.
#[derive(Debug)]
pub struct HerdrMux {
    bin: PathBuf,
    socket: Option<PathBuf>,
    home: Option<PathBuf>,
    /// Last snapshot this adapter saw, for [`shprd_mux::Host::subscribe`].
    seen: std::sync::Mutex<Option<events::Snapshot>>,
    /// Verbs this host has refused for this caller (tuios answers `forbidden`
    /// where herdr answers `ok`). A refusal is remembered so the capability
    /// flags stop promising something the host will not do.
    refused: std::sync::Mutex<Refused>,
}

/// Which verbs a host has refused, so the capability flags can say so.
///
/// It is measured, not guessed: the first refusal is what sets a flag, and
/// [`shprd_mux::Host::capabilities`] reads it back. A host that has never
/// refused anything reports everything it always did, which is why an adapter
/// built for herdr keeps its full set of flags.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Refused {
    /// `spawn` / `workspace create` / `tab create` was refused.
    pub(crate) spawn: bool,
    /// `pane split` was refused.
    pub(crate) split: bool,
    /// `workspace close` was refused.
    pub(crate) close_space: bool,
}

impl Refused {
    /// Remember a refusal of `verb`, if it is one the flags model.
    pub(crate) fn record(&mut self, verb: &str) {
        match verb {
            "spawn" => self.spawn = true,
            "split" => self.split = true,
            "close space" => self.close_space = true,
            _ => {}
        }
    }
}

impl HerdrMux {
    /// An adapter for the `herdr` on `PATH`, talking to whatever server the
    /// current environment points at.
    pub fn new() -> Self {
        Self {
            bin: PathBuf::from("herdr"),
            socket: None,
            home: None,
            seen: std::sync::Mutex::new(None),
            refused: std::sync::Mutex::new(Refused::default()),
        }
    }
}

impl Default for HerdrMux {
    fn default() -> Self {
        Self::new()
    }
}

impl HerdrMux {
    /// An adapter for a specific `herdr` binary.
    pub fn with_bin(bin: impl Into<PathBuf>) -> Self {
        Self {
            bin: bin.into(),
            ..Self::new()
        }
    }

    /// Point this adapter at one server: `socket` becomes `HERDR_SOCKET_PATH`
    /// and `home` becomes `HOME` for every command it runs.
    ///
    /// Setting `home` is what keeps a test away from the human's own herdr
    /// session: herdr keeps per-session state under `HOME`, so an isolated
    /// `HOME` plus an isolated socket is a private multiplexer.
    pub fn with_environment(
        mut self,
        socket: impl Into<PathBuf>,
        home: impl Into<PathBuf>,
    ) -> Self {
        self.socket = Some(socket.into());
        self.home = Some(home.into());
        self
    }

    /// The herdr binary this adapter runs.
    pub fn bin(&self) -> &Path {
        &self.bin
    }

    /// Record that the host refused `verb`, recovering from a poisoned lock
    /// instead of panicking.
    pub(crate) fn note_refusal(&self, verb: &str) {
        match self.refused.lock() {
            Ok(mut refused) => refused.record(verb),
            Err(poisoned) => poisoned.into_inner().record(verb),
        }
    }

    /// The verbs this host has refused so far.
    pub(crate) fn refused(&self) -> Refused {
        match self.refused.lock() {
            Ok(refused) => *refused,
            Err(poisoned) => *poisoned.into_inner(),
        }
    }
}
