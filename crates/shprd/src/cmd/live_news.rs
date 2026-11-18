//! The binary's side of the news: the multiplexer seam to a coordinator's pane.
//!
//! Shepherdr types its coordinator's news into the Assistant's terminal with
//! `hc send`. shprd does the same through its host adapter
//! ([`shprd_mux::Host`]) — one turn of input, never a shell string — into the
//! pane that belongs to the run's coordinator.
//!
//! Two panes can be that one, and both are found rather than guessed: the pane
//! the run recorded when it launched its own coordinator
//! (`OrchestratorLaunched`), or — in the M10 flow, where the person starts the
//! coordinator themselves — the repository space's coordinator tab, found by
//! the space's label through the host and checked against the pane `shprd
//! serve` recorded. A run with neither keeps its news for a later pass rather
//! than typing into a tab shprd cannot name.
//!
//! [`OrchestratorLaunched`]: shprd_core::event::EventKind::OrchestratorLaunched

use std::path::{Path, PathBuf};

use shprd_core::state::RunState;
use shprd_mux::{Host, PaneId};
use shprd_server::Coordinator;

/// The mux seam the news is delivered through.
pub struct LiveNews {
    /// The host the coordinator's pane lives in.
    mux: Box<dyn Host>,
    /// The home directory the service's own pane record lives under.
    home: PathBuf,
    /// The repository whose space label names the coordinator's tab.
    repo: PathBuf,
}

impl LiveNews {
    /// A seam over the host adapter the binary drives.
    pub fn new(mux: Box<dyn Host>, home: &Path, repo: &Path) -> Self {
        Self {
            mux,
            home: home.to_path_buf(),
            repo: repo.to_path_buf(),
        }
    }
}

impl Coordinator for LiveNews {
    fn pane(&self, state: &RunState) -> Option<String> {
        let repo_name = match crate::cmd::spawn::repo_name(&self.repo) {
            Ok(name) => name,
            Err(err) => {
                tracing::warn!(%err, "the coordinator's space label could not be derived");
                return None;
            }
        };
        let space = crate::cmd::spawn::repo_space_name(&repo_name);
        match crate::cmd::spawn::coordinator_pane_for(self.mux.as_ref(), &self.home, state, &space)
        {
            Ok(pane) => pane.map(|pane| pane.as_str().to_string()),
            Err(err) => {
                tracing::warn!(%err, "the coordinator's pane could not be found");
                None
            }
        }
    }

    fn tell(&self, pane: &str, text: &str) -> Result<(), String> {
        self.mux
            .write(&PaneId::new(pane), text, true)
            .map_err(|err| err.to_string())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use shprd_core::state::RunState;
    use shprd_mux::fake::FakeMux;
    use tempfile::TempDir;

    /// The pane the service recorded for the repository's space is the pane the
    /// news uses when the person started the coordinator themselves.
    #[test]
    fn a_coordinator_the_host_finds_is_the_pane_news_uses() {
        let home = TempDir::new().expect("a home");
        let repo = TempDir::new().expect("a repository");
        let mux = FakeMux::default().with_starter_tab();
        let name = crate::cmd::spawn::repo_space_name(
            &crate::cmd::spawn::repo_name(repo.path()).expect("the repository's name"),
        );
        let space = mux.create_space(&name, repo.path()).expect("the space");
        let recorded = mux
            .list_tabs(&space.id)
            .expect("the tabs")
            .first()
            .map(|tab| tab.root_pane.clone())
            .expect("the space's first tab");
        crate::cmd::spawn::record_coordinator_pane(home.path(), &recorded, "sh")
            .expect("recording the pane");

        let news = LiveNews::new(Box::new(mux), home.path(), repo.path());
        assert_eq!(
            news.pane(&RunState::default()).as_deref(),
            Some(recorded.as_str()),
            "the recorded tab is the coordinator's pane"
        );
    }

    /// A space with no recorded tab has nobody to tell, so the news waits.
    #[test]
    fn a_space_without_a_recorded_tab_has_no_pane() {
        let home = TempDir::new().expect("a home");
        let repo = TempDir::new().expect("a repository");
        let mux = FakeMux::default().with_starter_tab();
        let name = crate::cmd::spawn::repo_space_name(
            &crate::cmd::spawn::repo_name(repo.path()).expect("the repository's name"),
        );
        let _ = mux.create_space(&name, repo.path()).expect("the space");

        let news = LiveNews::new(Box::new(mux), home.path(), repo.path());
        assert_eq!(
            news.pane(&RunState::default()),
            None,
            "nothing recorded, so nothing may be typed into"
        );
    }
}
