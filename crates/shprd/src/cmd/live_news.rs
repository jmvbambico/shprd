//! The binary's side of the news: the multiplexer seam to a coordinator's pane.
//!
//! Shepherdr types its coordinator's news into the Assistant's terminal with
//! `hc send`. shprd does the same through its host adapter
//! ([`shprd_mux::Host`]) — one turn of input, never a shell string — into the
//! pane the run recorded when it launched its coordinator.
//!
//! Only a pane shprd started is written to: the run's log holds the
//! coordinator's pane (`OrchestratorLaunched`), and a run without one keeps its
//! news for a later pass rather than typing into a tab it did not open.
//!
//! [`OrchestratorLaunched`]: shprd_core::event::EventKind::OrchestratorLaunched

use shprd_mux::{Host, PaneId};
use shprd_server::Coordinator;

/// The mux seam the news is delivered through.
pub struct LiveNews {
    /// The host the coordinator's pane lives in.
    mux: Box<dyn Host>,
}

impl LiveNews {
    /// A seam over the host adapter the binary drives.
    pub fn new(mux: Box<dyn Host>) -> Self {
        Self { mux }
    }
}

impl Coordinator for LiveNews {
    fn tell(&self, pane: &str, text: &str) -> Result<(), String> {
        self.mux
            .write(&PaneId::new(pane), text, true)
            .map_err(|err| err.to_string())
    }
}
