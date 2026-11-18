//! The agents a service can see: `spaces` and `live_agents` on `GET /api/state`,
//! `GET /api/agents/{id}/output` and `POST /api/agents/{id}/attach`.
//!
//! Workers and reviewers have no tab and no space of their own (SHPR-23): the
//! host lists them by label and the sidebar's Agents pane draws them from
//! `live_agents`. The service itself reads no host. The binary passes a
//! [`LiveSource`] holding the host it drives, so a test passes a fake and a
//! service started without one answers an empty list and refuses the two
//! per-agent routes rather than pretending an agent is there.
//!
//! Every call into a source is blocking (a host call is a process or a socket
//! round trip), so the handlers run them off the runtime.
//!
//! An agent is addressed by the id the source gave it, resolved against the
//! source's own list: a client never names a terminal address the service did
//! not just list.

use std::path::Path;
use std::sync::Arc;

use crate::dto::{AgentAttachDto, AgentOutputDto, LiveAgentDto, SnapshotDto, SpaceDto};
use crate::error::ApiReject;

/// What a [`LiveSource`] knows about one agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveAgentRow {
    /// The id a client addresses it by: the host's own id for its terminal.
    pub id: String,
    /// The task it works on; `None` for the coordinator.
    pub task: Option<String>,
    /// `worker`, `reviewer` or `coordinator`.
    pub role: String,
    /// The harness id of its program.
    pub program: String,
    /// `working`, `needs_you`, `idle` or `done`.
    pub state: String,
    /// The host's terminal id, which a window binds a terminal block to and the
    /// web app opens a socket on.
    pub terminal: String,
    /// When it started, ms since the Unix epoch.
    pub started_ms: Option<u64>,
    /// When its program ended, when it has.
    pub ended_ms: Option<u64>,
    /// When it entered its current state.
    pub since_ms: Option<u64>,
}

/// The binary's view of the agents the host has.
pub trait LiveSource: Send + Sync {
    /// Every agent worth showing now: the live ones, a finished agent for 30
    /// minutes after it ended, and the coordinator for as long as its space.
    fn agents(&self) -> Vec<LiveAgentRow>;

    /// The last `lines` lines of the agent's screen, when it is listed.
    fn output(&self, id: &str, lines: usize) -> Result<String, String>;
}

/// The source a service holds, with a `Debug` that never prints a host.
#[derive(Clone, Default)]
pub struct LiveAgents(Option<Arc<dyn LiveSource>>);

impl LiveAgents {
    /// A service with no source: no agents are listed and the routes refuse.
    pub fn new() -> Self {
        Self::default()
    }

    /// The service that lists agents through `source`.
    pub fn with(source: Arc<dyn LiveSource>) -> Self {
        Self(Some(source))
    }

    /// The source, when one is wired.
    pub fn get(&self) -> Option<&Arc<dyn LiveSource>> {
        self.0.as_ref()
    }

    /// The agents as the state answer lists them, blocking.
    pub fn rows(&self) -> Vec<LiveAgentRow> {
        self.0
            .as_ref()
            .map(|source| source.agents())
            .unwrap_or_default()
    }

    /// One agent's recent output, blocking.
    pub fn output(&self, id: &str, lines: usize) -> Result<AgentOutputDto, ApiReject> {
        let source = self.source()?;
        let row = Self::find(source.as_ref(), id)?;
        let text = source
            .output(&row.id, lines.clamp(1, MAX_LINES))
            .map_err(ApiReject::refused)?;
        Ok(AgentOutputDto {
            id: row.id,
            live: row.state != "done",
            text,
        })
    }

    /// What a client binds a real terminal to, blocking: the terminal id and the
    /// socket route that types into it.
    pub fn attach(&self, id: &str) -> Result<AgentAttachDto, ApiReject> {
        let source = self.source()?;
        let row = Self::find(source.as_ref(), id)?;
        if row.state == "done" {
            return Err(ApiReject::refused(format!(
                "the agent {id} has finished: its output can be read, but there is nothing to type into"
            )));
        }
        Ok(AgentAttachDto {
            socket: format!("/api/terminal?space=agent:{}", row.id),
            id: row.id,
            terminal: row.terminal,
        })
    }

    /// The wired source, or the refusal a service without one earns.
    fn source(&self) -> Result<&Arc<dyn LiveSource>, ApiReject> {
        self.0.as_ref().ok_or_else(|| {
            ApiReject::refused("this service was started without a host, so it lists no agents")
        })
    }

    /// The listed agent called `id`.
    fn find(source: &dyn LiveSource, id: &str) -> Result<LiveAgentRow, ApiReject> {
        source
            .agents()
            .into_iter()
            .find(|row| row.id == id)
            .ok_or_else(|| ApiReject::not_found(format!("no agent {id} is listed")))
    }
}

impl std::fmt::Debug for LiveAgents {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveAgents")
            .field("source", &self.0.is_some())
            .finish()
    }
}

/// The most lines one output read returns.
const MAX_LINES: usize = 400;

/// The id of the one space a service serves: its repository's folder.
pub(crate) fn space_id(repo: &Path) -> String {
    let name = repo
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "repo".to_string());
    format!("sp-{name}")
}

/// Adds what only the service knows to a snapshot: the space it serves, which
/// space each task belongs to, and the agents the host lists.
pub(crate) fn decorate(snapshot: &mut SnapshotDto, repo: &Path, rows: Vec<LiveAgentRow>) {
    let id = space_id(repo);
    let name = repo
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "repo".to_string());
    snapshot.spaces = vec![SpaceDto {
        id: id.clone(),
        name,
        folder: repo.display().to_string(),
        branch: snapshot
            .run
            .as_ref()
            .map(|run| run.integration_branch.clone())
            .unwrap_or_default(),
    }];
    for task in &mut snapshot.tasks {
        task.space = Some(id.clone());
    }
    snapshot.live_agents = rows
        .into_iter()
        .map(|row| LiveAgentDto {
            id: row.id,
            space: id.clone(),
            task: row.task,
            role: row.role,
            program: row.program,
            state: row.state,
            terminal: row.terminal,
            started_ms: row.started_ms,
            ended_ms: row.ended_ms,
            since_ms: row.since_ms,
        })
        .collect();
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    struct One(&'static str);

    impl LiveSource for One {
        fn agents(&self) -> Vec<LiveAgentRow> {
            vec![LiveAgentRow {
                id: "t5".into(),
                task: Some("T1".into()),
                role: "worker".into(),
                program: "claude".into(),
                state: self.0.into(),
                terminal: "t5".into(),
                started_ms: Some(1),
                ended_ms: None,
                since_ms: Some(1),
            }]
        }

        fn output(&self, id: &str, _lines: usize) -> Result<String, String> {
            Ok(format!("screen of {id}"))
        }
    }

    #[test]
    fn a_listed_agent_can_be_read_and_attached() {
        let live = LiveAgents::with(Arc::new(One("working")));
        assert_eq!(live.output("t5", 20).unwrap().text, "screen of t5");
        let attach = live.attach("t5").unwrap();
        assert_eq!(attach.terminal, "t5");
        assert_eq!(attach.socket, "/api/terminal?space=agent:t5");
    }

    #[test]
    fn an_unlisted_agent_and_a_finished_one_are_refused() {
        let live = LiveAgents::with(Arc::new(One("done")));
        assert!(
            live.output("t9", 20).is_err(),
            "never a terminal nobody listed"
        );
        assert!(
            live.output("t5", 20).is_ok(),
            "a finished agent's output is readable"
        );
        assert!(
            live.attach("t5").is_err(),
            "nothing to type into once it ended"
        );
        assert!(LiveAgents::new().output("t5", 20).is_err());
        assert!(LiveAgents::new().rows().is_empty());
    }
}
