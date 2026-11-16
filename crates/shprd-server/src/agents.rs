//! Starting a program in a tab: the tab bar's "+" menu's "Start an agent".
//!
//! shprd's own agents are a task's attempts, and an attempt is not something a
//! window asks for — it writes a brief, makes a worktree and records an event.
//! What Shepherdr's "+" menu offers under *Start an agent* is smaller: a plain
//! CLI in a new tab, in the space's folder, with nothing to work on yet (a
//! person's own conversation with it). That is a host verb — it opens a terminal
//! and starts a program — so the mod never does it itself: it asks this route,
//! and the binary, which owns the harness table and the host adapter, does it.
//!
//! A service prepared without one — every test that only drives the router —
//! refuses the route rather than pretending a tab exists: an agent that was
//! never started is a lie a person would find later, in a tab that is not there.
//!
//! Nothing here reaches a host by itself: a test wires an [`AgentOpener`] that
//! records what it was asked for.

use std::sync::Arc;

use crate::dto::{AgentOpenDto, AgentTabDto};
use crate::error::ApiReject;

/// The tab a host opened for a program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenedTab {
    /// The harness id that was started, e.g. `claude`.
    pub program: String,
    /// The words the tab is named with, from the harness table.
    pub name: String,
    /// The host's own tab id.
    pub tab: String,
}

impl From<OpenedTab> for AgentTabDto {
    fn from(opened: OpenedTab) -> Self {
        Self {
            program: opened.program,
            name: opened.name,
            tab: opened.tab,
        }
    }
}

/// The binary's way of opening a program in a new tab.
///
/// It holds the host and the harness table, which the service has neither of: a
/// harness id becomes the program's launch line, and the tab opens in the space
/// the run lives in, at the repository root.
pub trait AgentOpener: Send + Sync {
    /// Start `program` in a new tab and answer the tab.
    ///
    /// `Err` is the program the harness table does not name, or a host that
    /// could not open the tab; the words are what a person reads.
    fn open(&self, program: &str) -> Result<OpenedTab, String>;
}

/// The opener a service holds, with a `Debug` that never prints a host.
#[derive(Clone, Default)]
pub struct Agents(Option<Arc<dyn AgentOpener>>);

impl Agents {
    /// A service with no opener: opening an agent is refused, not pretended.
    pub fn new() -> Self {
        Self::default()
    }

    /// The service that opens tabs through `opener`.
    pub fn with(opener: Arc<dyn AgentOpener>) -> Self {
        Self(Some(opener))
    }

    /// The opener, when one is wired.
    pub fn get(&self) -> Option<&Arc<dyn AgentOpener>> {
        self.0.as_ref()
    }

    /// Opens `program` in a new tab.
    ///
    /// The body is parsed here, so the route and a test refuse a malformed
    /// request the same way. An empty program is the caller's mistake and says
    /// so; a program this machine does not have, or a host that fails, is the
    /// opener's words, refused rather than retried (a second tab of the same
    /// program would be worse than the sentence explaining the first one did not
    /// open).
    pub fn open(&self, body: &[u8]) -> Result<OpenedTab, ApiReject> {
        let request: AgentOpenDto = serde_json::from_slice(body).map_err(|err| {
            ApiReject::bad_request(format!(
                "the body is not an agent to start: {err}; send {{\"program\": \"claude\"}}"
            ))
        })?;
        let program = request.program.trim();
        if program.is_empty() {
            return Err(ApiReject::bad_request(
                "a \"program\" is required: the harness id of the CLI to start, e.g. \"claude\"",
            ));
        }
        let opener = self.0.as_ref().ok_or_else(|| {
            ApiReject::refused(
                "this service was started without a host, so it cannot start an agent",
            )
        })?;
        opener.open(program).map_err(ApiReject::refused)
    }
}

impl std::fmt::Debug for Agents {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Agents")
            .field("opener", &self.0.is_some())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::sync::{Arc, Mutex};

    use super::*;

    /// An opener that records every program and answers one fixed tab.
    #[derive(Debug)]
    struct Recorder {
        asked: Mutex<Vec<String>>,
    }

    impl AgentOpener for Recorder {
        fn open(&self, program: &str) -> Result<OpenedTab, String> {
            self.asked
                .lock()
                .expect("the lock")
                .push(program.to_string());
            Ok(OpenedTab {
                program: program.to_string(),
                name: "Claude Code".to_string(),
                tab: "t1".to_string(),
            })
        }
    }

    #[test]
    fn a_service_without_a_host_refuses_and_says_why() {
        let refused = Agents::new()
            .open(br#"{"program":"claude"}"#)
            .expect_err("a refusal");
        assert_eq!(refused.status, axum::http::StatusCode::CONFLICT);
        assert!(
            refused.body.message.contains("without a host"),
            "{}",
            refused.body.message
        );
    }

    #[test]
    fn the_opener_is_asked_for_the_program_the_body_names() {
        let recorder = Arc::new(Recorder {
            asked: Mutex::default(),
        });
        let opened = Agents::with(recorder.clone())
            .open(br#"{"program":"  pi "}"#)
            .expect("an opened tab");
        assert_eq!(opened.tab, "t1");
        assert_eq!(opened.program, "pi");
        assert_eq!(
            recorder.asked.lock().expect("the lock").as_slice(),
            ["pi".to_string()]
        );
    }

    #[test]
    fn a_body_without_a_program_is_the_callers_mistake() {
        let refused = Agents::with(Arc::new(Recorder {
            asked: Mutex::default(),
        }))
        .open(br#"{"program":""}"#)
        .expect_err("a refusal");
        assert_eq!(refused.status, axum::http::StatusCode::BAD_REQUEST);
        assert!(
            refused.body.message.contains("harness id"),
            "{}",
            refused.body.message
        );
        let malformed = Agents::new()
            .open(br#"{"program":1}"#)
            .expect_err("a refusal");
        assert_eq!(malformed.status, axum::http::StatusCode::BAD_REQUEST);
    }
}
