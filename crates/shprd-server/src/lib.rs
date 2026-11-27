//! `shprd-server`: the service every shprd UI calls.
//!
//! One loopback listener, one token, these routes:
//!
//! | Route | What it is |
//! |---|---|
//! | `GET /api/health` | the unauthenticated handshake: `{ok, id, version}` |
//! | `GET /api/state` | the snapshot: the fold, projected for a board |
//! | `GET /api/changes` | long-poll for events after a sequence, `204` on timeout |
//! | `POST /api/action` | one tab-typed envelope, run against the same log the CLI writes |
//! | `GET /api/graph` | where the graph's program is, and the command that draws the run |
//! | `GET /api/graph/run` | the run document dagr draws, verbatim, for a client that draws it itself |
//! | `POST /api/tasks` | add one task, as `shprd agent task add` does |
//! | `GET /api/tasks/:id/changes` | the task's branch against its base, with line counts |
//! | `GET /api/settings` | every settings key, its value and the layer it came from |
//! | `PUT /api/settings` | one settings key, written and checked like the CLI writes it |
//! | `GET /api/roster` | the programs a role may run, with their models and levels |
//! | `POST /api/agents` | start a program in a new tab, as the tab bar's "+" menu asks |
//! | `GET /api/agents/:id/output` | one agent's recent screen, read-only (finished agents too) |
//! | `POST /api/agents/:id/attach` | the terminal id and socket a client binds a real terminal to |
//! | `GET /api/prompts` | the templates this machine has, without their words |
//! | `GET /api/prompts/:id` | one role prompt: its words, its file and its state |
//! | `PUT /api/prompts/:id` | the person's words, stored over that prompt's file |
//! | `DELETE /api/prompts/:id` | the shipped words put back over an edit |
//! | `GET /api/chat` | a space's conversation, read from its agent's own record, after a cursor |
//! | `POST /api/chat` | one turn of the person's input, typed into that space's agent |
//! | `GET /api/terminal` | a socket attached to one pane: its screen pushed, keys typed back |
//! | `GET /api/ws` | the same change notices, pushed (for the web app) |
//! | `POST /api/login` | the login password, exchanged for a session cookie |
//! | `POST /api/logout` | ends the session the cookie names |
//! | `GET /api/session` | who is asking, through which door, and how fresh |
//! | anything else | the built web app: hashed assets, and `index.html` for an SPA route |
//!
//! Every `/api` route but health and login passes the gate (`crate::auth`): a
//! request is allowed by **either** `Authorization: Bearer <token>` from the
//! state directory — the door the CLI and the Herdr mods use — **or** a valid
//! session cookie from `POST /api/login`. A write also needs `x-shprd: 1`, and
//! its `Origin`, when it sends one, must be this service's own page origin; a
//! socket upgrade needs that `Origin` and no header (a browser cannot set one on
//! a handshake). A `Host` that is neither loopback nor a hostname a tunnel added
//! is a `421`, and the login is rate limited with `429` and `Retry-After`.
//!
//! A destructive action — dropping a task, and later landing or merging one —
//! needs a session whose password was entered in the last five minutes; the token
//! door is exempt, because the token is already local.
//!
//! The web app itself is public, because the page a signed-out person gets *is*
//! the login screen; everything it calls is behind the gate.
//!
//! A Herdr mod can only `fetch` (no streaming, no WebSocket), which is why the
//! change channel is a long poll first and a socket second.
//!
//! Every socket is bounded and a request that is not an upgrade is refused with
//! a typed body, so no client — a page, a probe, or a connection nobody closed —
//! holds one open for ever; see [`WsLimits`].
//!
//! Starting one is [`serve`]; a test that wants no socket builds
//! an [`App`] and drives its router.

pub mod agents;
pub mod api;
pub mod attachments;
pub mod bindings;
pub mod changes;
pub mod chat;
pub mod dto;
pub mod engine;
pub mod error;
pub mod graph;
pub mod pass;
pub mod repos;
pub mod self_update;
pub mod service;
pub mod snapshot;
pub mod tasks;
pub mod terminal;
pub mod tick;
mod tunnels;

pub(crate) mod actions;
pub(crate) mod auth;
pub mod hub;
pub(crate) mod ledger;
pub mod live;
pub mod news;
pub(crate) mod prompts;
pub mod remote;
pub(crate) mod roster;
pub(crate) mod settings;
pub(crate) mod web;

pub use actions::default_run;
pub use agents::{AgentOpener, Agents, OpenedTab};
pub use api::{App, BODY_LIMIT, WsLimits};
pub use changes::Touch;
pub use chat::{AnswerStep, COORDINATOR, ChatHost, ChatWriter, Space as ChatSpace, answer_steps};
pub use engine::{Engine, Resender, RunVerb};
pub use error::{ApiReject, ServerError};
pub use graph::{Dagr, Located, Missing, Origin};
pub use live::{HostSpace, LiveAgentRow, LiveAgents, LiveSource};
pub use news::{Coordinator, NewsDesk, NewsSent};
pub use pass::{FailedStep, LastPass};
pub use repos::{Prepare, REPO_HEADER, Registry, RepoFactory, RepoInfo, RepoParts};
pub use service::{
    AfterWrite, Config, PassNotice, Service, ServiceFiles, ServiceInfo, ServiceRecord, TickConfig,
    VERSION, repo_dir_in, repo_state_dir, serve, shutdown_signal, state_dir, state_home,
};
pub use shprd_remote::AllowedHosts;
pub use snapshot::NO_RUN_HINT;
pub use terminal::{TermKey, TerminalHost, TerminalPort};
pub use tick::{
    FactsSource, FallbackSource, FallbackSourceArc, IdleFn, LiveFacts, LiveFn, ReviewCloser,
    ReviewSwitcher, Reviewer, SharedForge, Starter, StepReport, TickReport, Worker, WorkerCloser,
};
