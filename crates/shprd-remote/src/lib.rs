//! `shprd-remote`: the login, the sessions and the gate that stand between a
//! public tunnel and this machine's shell.
//!
//! The service's loopback door — a bearer token in the state directory — is for
//! programs on the same machine and stays exactly as it is. A tunnel puts the
//! same listener on the internet, where a token cannot go, so a browser presents
//! something else: a password the person set at their own terminal
//! ([`PasswordStore`]), exchanged once for a server-side session
//! ([`Sessions`]) named by an `HttpOnly` cookie. [`Gateway`] is the decision
//! that runs in front of every request: either door, the `Host` allowlist, the
//! `Origin` and `x-shprd` rules on a write, the rate limits, and the fresh
//! password a destructive action needs.
//!
//! The researched design is M7d §4.4 (`_research/m7/M7d-web-remote.md`): the
//! argon2id parameters, the cookie's attributes, the timeouts and the rate-limit
//! table are all taken from it, and its threat rows 1–7, 12 and 15–18 are what
//! this crate is for.
//!
//! Nothing here is a UI: the crate answers [`Gateway::decide`] and
//! [`Gateway::login`], and `shprd-server` turns the answers into routes. It
//! reads no environment variable — every path and clock is handed in — so a
//! test can point it at a temporary state directory and a fake clock.

pub mod cookie;
pub mod dto;
pub mod gate;
pub mod hosts;
pub mod limits;
pub mod password;
pub mod session;
pub mod tunnel;

mod error;
mod fsutil;

pub use error::{PasswordRefusal, RemoteError};
pub use fsutil::secret_eq;
pub use gate::{Door, GateError, GateRequest, Gateway, LoginOutcome};
pub use hosts::AllowedHosts;
pub use limits::{RateLimiter, Retry, StormSignal};
pub use password::{MIN_LENGTH, PasswordStore};
pub use session::{Revoked, SessionRecord, Sessions, UaClass};
