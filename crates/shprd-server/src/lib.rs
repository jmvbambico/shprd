//! `shprd-server`: the service every shprd UI calls.
//!
//! One loopback listener, one token, five routes:
//!
//! | Route | What it is |
//! |---|---|
//! | `GET /api/health` | the unauthenticated handshake: `{ok, id, version}` |
//! | `GET /api/state` | the snapshot: the fold, projected for a board |
//! | `GET /api/changes` | long-poll for events after a sequence, `204` on timeout |
//! | `POST /api/action` | one tab-typed envelope, run against the same log the CLI writes |
//! | `GET /api/ws` | the same change notices, pushed (for the web app) |
//!
//! Everything but health needs `Authorization: Bearer <token>` from the state
//! directory; writes also need `x-shprd: 1`; a request whose `Origin` is not
//! this service's own loopback origin is refused. A Herdr mod can only `fetch`
//! (no streaming, no WebSocket), which is why the change channel is a long poll
//! first and a socket second.
//!
//! Starting one is [`serve`](service::serve); a test that wants no socket builds
//! an [`App`](api::App) and drives its router.

pub mod api;
pub mod bindings;
pub mod dto;
pub mod error;
pub mod service;
pub mod snapshot;

pub(crate) mod actions;
pub(crate) mod auth;
pub(crate) mod hub;
pub(crate) mod ledger;

pub use api::App;
pub use error::{ApiReject, ServerError};
pub use service::{
    Config, Service, ServiceFiles, ServiceInfo, ServiceRecord, VERSION, serve, shutdown_signal,
    state_dir,
};
