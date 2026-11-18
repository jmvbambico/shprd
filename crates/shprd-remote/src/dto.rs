//! The wire shapes the login routes answer with, and the TypeScript they
//! generate.
//!
//! They are exported with the service's other DTOs (`shprd-server`'s
//! `bindings`), so the login screen and a Herdr mod read the same messages the
//! service writes.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::session::{SessionRecord, UaClass};

/// The body of `POST /api/login`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "LoginRequest.ts")]
pub struct LoginRequest {
    /// The password the person typed. It is never logged and never echoed.
    pub password: String,
    /// Whether this session should survive a browser restart ("remember this
    /// device": a 30-day idle timeout instead of 12 hours).
    #[serde(default)]
    pub remember: bool,
}

/// What a successful login answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "LoginReply.ts")]
pub struct LoginReply {
    /// True; a refusal answers [`crate::dto::LoginFailure`] instead.
    pub ok: bool,
    /// The session the cookie now names.
    pub session: SessionInfoDto,
    /// True when this login refreshed an existing session (a password entered
    /// again for a destructive action) rather than starting a new one.
    pub refreshed: bool,
}

/// What a refused login answers.
///
/// It is deliberately one message for every reason: a wrong password, no
/// password set at all, and a stale session are all "that did not work", so a
/// stranger learns nothing from trying.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "LoginFailure.ts")]
pub struct LoginFailure {
    /// Always `ok: false`.
    pub ok: bool,
    /// One line a person can read, with no reason in it.
    pub message: String,
    /// How long to wait, in seconds, when the refusal is a rate limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub retry_after: Option<u64>,
}

/// What `POST /api/logout` answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "LogoutReply.ts")]
pub struct LogoutReply {
    /// True when the request was understood. A logout is idempotent: ending a
    /// session that is already gone is not a failure.
    pub ok: bool,
    /// Whether this request ended a session.
    pub revoked: bool,
}

/// Which door a request came through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "ViaDto.ts")]
pub enum ViaDto {
    /// The loopback bearer token: a program on this machine, never a browser.
    Token,
    /// A browser's session cookie.
    Session,
}

/// One session, as a person reads it in a list or a page shows it.
///
/// The id is the stored hash's short form: the raw id lives only in the
/// browser's cookie, so nothing here can be replayed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "SessionInfoDto.ts")]
pub struct SessionInfoDto {
    /// Twelve hex characters naming this session; what `--revoke` takes.
    pub id: String,
    /// When the password was entered, in seconds since the Unix epoch.
    pub created: u64,
    /// The last request this session was used for.
    pub last_seen: u64,
    /// Until when a destructive action needs no password again.
    pub fresh_until: u64,
    /// Whether the password was entered recently enough for a destructive
    /// action.
    pub fresh: bool,
    /// Whether this session survives a browser restart.
    pub remember: bool,
    /// What kind of device it is.
    pub ua_class: UaClass,
    /// A short label: the device class and the browser family.
    pub label: String,
    /// When it stops being usable if it is never used again.
    pub expires_at: u64,
}

impl SessionInfoDto {
    /// The view of `record` at `now` (seconds since the Unix epoch).
    pub fn new(record: &SessionRecord, now: u64) -> Self {
        Self {
            id: record.short_id().to_string(),
            created: record.created,
            last_seen: record.last_seen,
            fresh_until: record.fresh_until,
            fresh: record.fresh(now),
            remember: record.remember,
            ua_class: record.ua_class,
            label: record.label.clone(),
            expires_at: record.expires_at(),
        }
    }
}

/// What `GET /api/sessions` answers: every live session, and which is this one.
///
/// Only the short ids are here, never a cookie value, so the list cannot be
/// replayed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "SessionListDto.ts")]
pub struct SessionListDto {
    /// The live sessions, oldest first.
    pub sessions: Vec<SessionInfoDto>,
    /// The short id of the session that asked, when the door was a cookie.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub current: Option<String>,
}

/// What `GET /api/session` answers: who is asking, and how.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "SessionDto.ts")]
pub struct SessionDto {
    /// True when the request was allowed, which it must have been to be answered.
    pub ok: bool,
    /// Which door it came through.
    pub via: ViaDto,
    /// Whether a destructive action is allowed without the password again.
    pub fresh: bool,
    /// Whether a password is set at all: a web app with none can never sign in,
    /// and says so on the login screen.
    pub password: bool,
    /// The session, when the door was a cookie.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub session: Option<SessionInfoDto>,
}

/// The body of `POST /api/tunnels/start`. Both fields are optional: what is not
/// given comes from the `web.*` settings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "TunnelStartRequest.ts")]
pub struct TunnelStartRequest {
    /// The provider's word (`tailscale`, `cloudflared`, `cloudflared-named`,
    /// `ngrok`, `tunnl`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub provider: Option<String>,
    /// tunnl.gg only: the stable address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub stable: Option<bool>,
}

/// One provider a tunnel can be started with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "TunnelProviderDto.ts")]
pub struct TunnelProviderDto {
    /// The word `--tunnel` takes.
    pub id: String,
    /// Whether its address is on the public internet, which is what makes a
    /// password a condition of starting it.
    pub public_internet: bool,
}

/// What `GET /api/tunnels` and the start and stop routes answer: the tunnel as
/// `web.json` records it, and what the page needs to offer a start.
///
/// It is only ever sent through the gate, so it carries the full link and its
/// QR code — the person who is signed in is the person who may share it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "TunnelStatusDto.ts")]
pub struct TunnelStatusDto {
    /// Whether a tunnel is up.
    pub on: bool,
    /// Whether one is being started.
    pub starting: bool,
    /// The provider's word, while one is up or was last tried.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub provider: Option<String>,
    /// The full link, while a tunnel is up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub link: Option<String>,
    /// The link as an SVG QR code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub qr_svg: Option<String>,
    /// Why there is no tunnel, when the last start or run ended badly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub why: Option<String>,
    /// When the record was written, in epoch seconds.
    pub at: u64,
    /// The provider program's version line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub version: Option<String>,
    /// Whether a login password is set: without one, no public provider starts.
    pub password_set: bool,
    /// The provider `start` uses when none is named.
    pub default_provider: String,
    /// Every provider.
    pub providers: Vec<TunnelProviderDto>,
    /// The tailnet login Tailscale vouches for, only on a request that arrived
    /// through `tailscale serve`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tailnet_user: Option<String>,
}
