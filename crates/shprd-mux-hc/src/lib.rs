//! Herdr desktop through herdr-core: `hc connect`, its framed JSON-RPC, its
//! event stream and the CLI-side truths around it.
//!
//! Herdr desktop is a window over **herdr-core** (`hc`), and herdr-core is where
//! spaces, tabs, terminals and their status actually live. This crate is the one
//! place in shprd that speaks to it:
//!
//! - [`wire`] — the bytes: plain JSON lines until `hello`, then
//!   `channel | kind | length | payload` frames, with the mode switch inside the
//!   parser (the trap the M7ef spike recorded);
//! - [`client`] — the connection: a spawned `hc connect`, request ids and
//!   per-call timeouts, the version gate, and a keeper that reconnects with
//!   backoff and fails in-flight calls instead of hanging;
//! - [`events`] — the fold: terminals from the twelve event types, the
//!   `missed`/reset discipline that re-reads documents rather than trusting the
//!   stream, and the window's spaces and tabs from its `herdr-app` document;
//! - [`placement`] — `herdr.space` / `herdr.tab`: how a terminal lands in the
//!   space and tab a task works in;
//! - [`osc`] — the OSC 7501 status grammar a program uses to say what it is
//!   doing, byte for byte;
//! - [`host`] — [`HcHost`], the [`shprd_mux::Host`] every shprd verb drives on
//!   Herdr desktop, built from the client and the fold above and from the
//!   window's own `herdr-app` command line for the layout.
//!
//! Everything here was shaped by the live spikes in
//! `_research/m7/M7ef-herdr-desktop-spikes.md` (herdr-core 0.1.21, Herdr.app
//! 0.1.15), which recorded the real API, framing, event types, error kinds and
//! the fact that a `missed` notice carries a fresh snapshot but never a
//! document. Its layout, methods, event fold and error taxonomy are ported from
//! Shepherdr's `service/hc/{client,protocol}.ts` and `service/hub.ts`
//! (MIT, Copyright (c) 2026 Elias Stravik, tag `v3.1.34`, commit `a3f327a`); the
//! notices and the fold are shprd's own code.
//!
//! The layout, methods, event fold and error taxonomy are ported from
//! Shepherdr's `service/hc/{client,protocol}.ts` and `service/hub.ts`
//! (MIT, Copyright (c) 2026 Elias Stravik, tag `v3.1.34`, commit `a3f327a`); the
//! notices and the fold are shprd's own code. The host adapter's split — `hc`
//! for terminals, `herdr-app` for the window's spaces and tabs — is Shepherdr's
//! too (`service/spaces.ts`, `service/hub.ts`).

pub mod client;
pub mod error;
pub mod events;
pub mod host;
pub mod model;
pub mod osc;
pub mod placement;
pub mod wire;

pub use client::{
    DEFAULT_TIMEOUT, Endpoint, HcClient, Hello, MAX_PENDING, RECONNECT_CEILING, RECONNECT_DELAYS,
    REQUIRED_METHODS, version_at_least,
};
pub use error::{CORE_GENERATION, CORE_MINIMUM, Error, Result};
pub use events::{Fold, LAYOUT_DOCUMENT, State, Update};
pub use host::{HcHost, Window};
pub use model::{
    AgentState, AgentStateWord, AskKind, Change, ChangeKind, Foreground, Notice, Status, Terminal,
    notice_from_message, terminals_from,
};
pub use osc::{Report, ReportWord};
pub use placement::{Placement, SPACE_LABEL, Space, TAB_LABEL, TASK_LABEL, Tab};
pub use wire::{FrameKind, Incoming, MAX_FRAME, Parser};
