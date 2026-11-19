//! The agents `shprd serve` lists: the host's headless workers and reviewers,
//! the coordinator, and a finished agent for a while after it ended.
//!
//! The host is the source of truth for who is running ([`Host::list_agents`]
//! for workers and reviewers, the recorded pane for the coordinator); this
//! module adds what a host cannot say, the clock: when an agent entered its
//! state, and that an agent which has gone from the host's list (stopped by a
//! landing or a drop) is *finished*, not forgotten, and stays visible for
//! [`FINISHED_FOR`]. The coordinator stays as long as its space has it.
//!
//! The clock is injected, so the retention is tested without waiting.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use shprd_mux::{AgentRole, AgentState, Host, PaneId};
use shprd_server::{HostSpace, LiveAgentRow, LiveSource};

/// How long a finished agent stays listed: 30 minutes, the person's rule (Q4).
pub(crate) const FINISHED_FOR_MS: u64 = 30 * 60 * 1000;

/// What the cache remembers about one agent between two lists.
#[derive(Debug, Clone)]
struct Seen {
    row: LiveAgentRow,
}

/// The binary's [`LiveSource`]: the host it drives, and the clock it reads.
pub(crate) struct HostLive {
    /// The repository whose run and coordinator are listed.
    repo: PathBuf,
    /// The home directory the coordinator's record lives under.
    home: PathBuf,
    /// The host this service drives.
    host: Arc<dyn Host>,
    /// Now, in ms since the epoch.
    now: Arc<dyn Fn() -> u64 + Send + Sync>,
    /// Every agent seen so far, by id.
    seen: Mutex<BTreeMap<String, Seen>>,
    /// The host's space this service made or reused for the repository.
    space: Option<HostSpace>,
}

impl HostLive {
    /// A source over `host` for `repo`, reading the system clock.
    pub(crate) fn new(repo: PathBuf, home: PathBuf, host: Arc<dyn Host>) -> Self {
        use shprd_core::log::{Clock, SystemClock};
        Self::with_clock(repo, home, host, Arc::new(|| SystemClock.now_ms()))
    }

    /// [`HostLive::new`] with a clock of the caller's.
    pub(crate) fn with_clock(
        repo: PathBuf,
        home: PathBuf,
        host: Arc<dyn Host>,
        now: Arc<dyn Fn() -> u64 + Send + Sync>,
    ) -> Self {
        Self {
            repo,
            home,
            host,
            now,
            seen: Mutex::new(BTreeMap::new()),
            space: None,
        }
    }

    /// Names the host's own space for the repository, which `spaces` reports.
    pub(crate) fn with_space(mut self, space: Option<HostSpace>) -> Self {
        self.space = space;
        self
    }

    /// The terminal an agent runs in, when it is listed: what a terminal socket
    /// for `agent:<id>` types into.
    pub(crate) fn terminal_of(&self, id: &str) -> Option<String> {
        self.agents()
            .into_iter()
            .find(|row| row.id == id)
            .map(|row| row.terminal)
    }

    /// What the host lists right now, as rows without a clock; `None` when the
    /// host could not be asked, which is not the same as an empty list.
    fn current(&self) -> Option<(Vec<LiveAgentRow>, bool)> {
        let now = (self.now)();
        let mut rows: Vec<LiveAgentRow> = self
            .host
            .list_agents()
            .ok()?
            .into_iter()
            // The core keeps a finished terminal's record; the pane lists it for
            // as long as the rest do, then lets it go.
            .filter(|agent| {
                agent.live
                    || agent
                        .ended_ms
                        .is_none_or(|ended| now.saturating_sub(ended) < FINISHED_FOR_MS)
            })
            .map(|agent| {
                let state = if agent.live {
                    state_word(agent.state)
                } else {
                    "done"
                };
                LiveAgentRow {
                    id: agent.tab.root_pane.as_str().to_string(),
                    terminal: agent.tab.root_pane.as_str().to_string(),
                    task: agent.task,
                    role: agent.role.as_str().to_string(),
                    program: agent.program,
                    state: state.to_string(),
                    started_ms: agent.started_ms,
                    ended_ms: agent.ended_ms,
                    since_ms: None,
                }
            })
            .collect();
        let coordinator = self.coordinator();
        let unknown = coordinator.is_none() && self.coordinator_unreadable();
        rows.extend(coordinator);
        Some((rows, unknown))
    }

    /// Whether the host could not say where the coordinator is (a failed lookup),
    /// as against it not being there: the last row is kept for the first.
    fn coordinator_unreadable(&self) -> bool {
        let Ok(name) = crate::cmd::spawn::repo_name(&self.repo)
            .map(|name| crate::cmd::spawn::repo_space_name(&name))
        else {
            return false;
        };
        match self.host.find_space(&name) {
            Err(_) => true,
            Ok(None) => false,
            Ok(Some(space)) => self.host.list_tabs(&space.id).is_err(),
        }
    }

    /// The coordinator shprd started in the repository's space, when it is there.
    fn coordinator(&self) -> Option<LiveAgentRow> {
        let name = crate::cmd::spawn::repo_name(&self.repo)
            .map(|name| crate::cmd::spawn::repo_space_name(&name))
            .ok()?;
        let space = self.host.find_space(&name).ok()??;
        let record = crate::cmd::spawn::coordinator_record(
            &self.home,
            &self.repo,
            self.host.as_ref(),
            &space,
        )?;
        let pane = record.pane;
        // Only a pane that is still a tab of the space is the coordinator: a
        // closed tab is no longer its record's.
        let tab = self
            .host
            .list_tabs(&space.id)
            .ok()?
            .into_iter()
            .find(|tab| tab.root_pane == pane)?;
        let state = match self.host.agent_state(&tab.root_pane) {
            Ok(state) => state_word(state),
            Err(_) => "done",
        };
        Some(LiveAgentRow {
            id: pane.as_str().to_string(),
            terminal: pane.as_str().to_string(),
            task: None,
            role: AgentRole::Coordinator.as_str().to_string(),
            program: record.program.unwrap_or_default(),
            state: state.to_string(),
            started_ms: None,
            ended_ms: None,
            since_ms: None,
        })
    }
}

/// The word the sidebar shows for a host's agent state.
fn state_word(state: AgentState) -> &'static str {
    match state {
        AgentState::Working => "working",
        AgentState::Blocked => "needs_you",
        AgentState::Idle | AgentState::Unknown => "idle",
        AgentState::Done => "done",
    }
}

impl LiveSource for HostLive {
    fn space(&self) -> Option<HostSpace> {
        self.space.clone()
    }

    fn agents(&self) -> Vec<LiveAgentRow> {
        let now = (self.now)();
        let Ok(mut seen) = self.seen.lock() else {
            return self.current().map(|(rows, _)| rows).unwrap_or_default();
        };
        // A host that cannot be asked right now (a transient error) says nothing
        // about who is running: what was last seen is shown as it was, and no
        // agent is marked finished on a failed listing.
        let Some((current, coordinator_unknown)) = self.current() else {
            return seen.values().map(|entry| entry.row.clone()).collect();
        };
        let mut listed: Vec<String> = Vec::new();
        for mut row in current {
            listed.push(row.id.clone());
            let before = seen.get(&row.id).map(|seen| &seen.row);
            row.since_ms = match before {
                Some(before) if before.state == row.state => before.since_ms,
                _ => Some(now),
            };
            if row.state == "done" && row.ended_ms.is_none() {
                row.ended_ms = before.and_then(|before| before.ended_ms).or(Some(now));
            }
            seen.insert(row.id.clone(), Seen { row });
        }
        // An agent the host no longer lists was stopped (a landing, a drop, a
        // verdict): it finished, and stays visible for a while. A coordinator
        // that is gone has no space any more, so it is dropped.
        for (id, entry) in seen.iter_mut() {
            if listed.contains(id) || entry.row.role == "coordinator" {
                continue;
            }
            if entry.row.state != "done" {
                entry.row.state = "done".to_string();
                entry.row.ended_ms = Some(now);
                entry.row.since_ms = Some(now);
            }
        }
        seen.retain(|id, entry| {
            if listed.contains(id) {
                return true;
            }
            (entry.row.role == "coordinator" && coordinator_unknown)
                || entry.row.role != "coordinator"
                    && entry
                        .row
                        .ended_ms
                        .is_some_and(|ended| now.saturating_sub(ended) < FINISHED_FOR_MS)
        });
        let mut rows: Vec<LiveAgentRow> = seen.values().map(|entry| entry.row.clone()).collect();
        rows.sort_by(|a, b| {
            (a.role != "coordinator", a.started_ms, &a.id).cmp(&(
                b.role != "coordinator",
                b.started_ms,
                &b.id,
            ))
        });
        rows
    }

    fn output(&self, id: &str, lines: usize) -> Result<String, String> {
        let row = self
            .agents()
            .into_iter()
            .find(|row| row.id == id)
            .ok_or_else(|| format!("no agent {id} is listed"))?;
        self.host
            .read(&PaneId::new(row.terminal), lines)
            .map_err(|err| format!("its terminal could not be read: {err}"))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::sync::atomic::{AtomicU64, Ordering};

    use shprd_mux::fake::FakeMux;
    use shprd_mux::{AgentLaunch, Host};
    use tempfile::TempDir;

    use super::*;

    /// A source over a fake host with a clock the test moves.
    fn source() -> (HostLive, Arc<FakeMux>, Arc<AtomicU64>, TempDir) {
        let home = TempDir::new().expect("home");
        let mux = Arc::new(FakeMux::default().with_ready_agent());
        let clock = Arc::new(AtomicU64::new(1_000));
        let at = Arc::clone(&clock);
        let live = HostLive::with_clock(
            PathBuf::from("/work/repo"),
            home.path().to_path_buf(),
            mux.clone(),
            Arc::new(move || at.load(Ordering::SeqCst)),
        );
        (live, mux, clock, home)
    }

    fn start(mux: &FakeMux, role: AgentRole, task: Option<&str>) -> shprd_mux::Tab {
        let space = mux
            .ensure_space("shprd · repo", std::path::Path::new("/work/repo"))
            .expect("space");
        mux.spawn_agent(&AgentLaunch {
            space: &space.id,
            title: "agent",
            argv: &[],
            env: &[],
            cwd: std::path::Path::new("/work/repo"),
            role,
            task,
            program: "claude",
            parent: None,
        })
        .expect("agent")
    }

    #[test]
    fn a_worker_and_a_reviewer_are_listed_with_no_tab_and_stay_after_they_stop() {
        let (live, mux, clock, _home) = source();
        let worker = start(&mux, AgentRole::Worker, Some("T1"));
        let reviewer = start(&mux, AgentRole::Reviewer, Some("T1"));

        let rows = live.agents();
        assert_eq!(rows.len(), 2);
        assert!(
            rows.iter()
                .any(|row| row.role == "worker" && row.state == "idle")
        );
        assert!(rows.iter().any(|row| row.role == "reviewer"));
        let space = mux.find_space("shprd · repo").unwrap().unwrap();
        assert!(
            mux.list_tabs(&space.id).unwrap().is_empty(),
            "no tab for either"
        );

        // A landing stops them: they are finished, and still listed.
        clock.store(5_000, Ordering::SeqCst);
        mux.close_tab(&worker.id).unwrap();
        mux.close_tab(&reviewer.id).unwrap();
        let rows = live.agents();
        assert_eq!(rows.len(), 2, "finished agents stay: {rows:?}");
        assert!(
            rows.iter()
                .all(|row| row.state == "done" && row.ended_ms == Some(5_000))
        );

        // Thirty minutes later they are gone.
        clock.store(5_000 + FINISHED_FOR_MS, Ordering::SeqCst);
        assert!(live.agents().is_empty());
    }

    #[test]
    fn a_state_change_restarts_its_clock() {
        let (live, mux, clock, _home) = source();
        let worker = start(&mux, AgentRole::Worker, Some("T1"));
        assert_eq!(live.agents()[0].since_ms, Some(1_000));
        clock.store(2_000, Ordering::SeqCst);
        assert_eq!(
            live.agents()[0].since_ms,
            Some(1_000),
            "same state, same clock"
        );
        mux.set_state(&worker.root_pane, AgentState::Working);
        clock.store(3_000, Ordering::SeqCst);
        let row = live.agents().remove(0);
        assert_eq!((row.state.as_str(), row.since_ms), ("working", Some(3_000)));
    }

    #[test]
    fn only_a_listed_agent_has_a_terminal_or_output() {
        let (live, mux, _clock, _home) = source();
        let worker = start(&mux, AgentRole::Worker, Some("T1"));
        let id = worker.root_pane.as_str().to_string();
        mux.set_screen(&worker.root_pane, "hello from the agent");
        assert_eq!(live.terminal_of(&id), Some(id.clone()));
        assert!(live.output(&id, 10).unwrap().contains("hello"));
        assert_eq!(live.terminal_of("pane-404"), None);
        assert!(live.output("pane-404", 10).is_err());
    }
}
