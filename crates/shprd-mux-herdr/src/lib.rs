//! Herdr adapter: [`HerdrMux`] implements [`shprd_mux::Mux`] by shelling out to
//! the `herdr` binary.
//!
//! Every call runs one `herdr` process and reads the single JSON object the CLI
//! prints (`{"id":…,"result":…}` or `{"id":…,"error":{"code":…,"message":…}}`),
//! so no state is kept in this crate and a restarted herdr server is invisible
//! to the adapter. The binary, the socket path and `HOME` are configuration: a
//! test or a sandbox points them at a throwaway directory and the adapter can
//! never reach the human's own herdr session.
//!
//! Panes are process-shaped, not shell-shaped: [`shprd_mux::Mux::run`] takes an
//! argv and this adapter quotes it element by element before handing it to the
//! pane, so a branch name or a title is never re-parsed as shell syntax.
//!
//! Events come from [`shprd_mux::Mux::next_events`], which polls
//! `herdr api snapshot` and diffs it: herdr 0.9.5 exposes `events.subscribe` and
//! `events.wait` on the socket but nothing on the CLI, so events produced while
//! nobody was polling are lost and [`shprd_mux::Capabilities::resumable_events`]
//! is `false`.

mod adapter;
mod cli;
mod events;
pub mod keys;
mod panes;
mod shell;
mod tabs;
mod workspaces;

use std::path::{Path, PathBuf};

/// Drives one herdr server through the `herdr` command line.
///
/// It holds no connection, so every method is one independent process; the only
/// state is the last snapshot [`Mux::next_events`] saw, behind a mutex so the
/// adapter is `Send + Sync` and can be shared by the scheduler and the event
/// pump at once.
#[derive(Debug)]
pub struct HerdrMux {
    bin: PathBuf,
    socket: Option<PathBuf>,
    home: Option<PathBuf>,
    /// Last snapshot this adapter saw, for [`Mux::next_events`].
    seen: std::sync::Mutex<Option<events::Snapshot>>,
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
}
