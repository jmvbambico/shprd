//! The login routes: `POST /api/login`, `POST /api/logout`, `GET /api/session`.
//!
//! They are thin: the decision is [`shprd_remote::Gateway`], and this file turns
//! its answers into statuses, cookies and the DTOs the web app reads.
//!
//! Two things are deliberate and worth keeping:
//!
//! * **one failure message.** A wrong password, no password set at all and a
//!   session that is no longer valid answer with the same words, so a stranger
//!   learns nothing from trying;
//! * **no secrets in the answer.** The cookie carries the session id; nothing
//!   else in the response ever does, and no path here logs a password, a session
//!   id, a token or a tunnel URL.

use axum::Extension;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use shprd_remote::dto::{
    LoginFailure, LoginReply, LoginRequest, LogoutReply, SessionDto, SessionInfoDto, SessionListDto,
};
use shprd_remote::session::Revoked;
use shprd_remote::{Door, GateError, cookie};

use crate::api::AppState;
use crate::auth;
use crate::error::ApiReject;

/// The one line every failed login says.
///
/// It is the same for a wrong password, an absent password and an expired
/// session: there is one account, so the message leaks nothing, and the logs say
/// no more than the client.
pub const FAILURE: &str = "that password did not work";

/// The gate's refusal as an HTTP answer.
///
/// It is the one place a [`GateError`] turns into a status, so the decision
/// table stays a function and this stays a table.
pub(crate) fn reject(err: GateError) -> ApiReject {
    match err {
        GateError::WrongHost { host } => ApiReject::wrong_host(host.as_deref()),
        GateError::WrongOrigin { origin } => {
            ApiReject::cross_origin(origin.as_deref().unwrap_or(""))
        }
        GateError::MissingHeader => ApiReject::missing_header(),
        GateError::Unauthorized => ApiReject::unauthorized(),
        GateError::Reauth => ApiReject::reauth(),
        GateError::TooManyRequests { after_ms } => {
            ApiReject::too_many_requests(after_ms.div_ceil(1000).max(1))
        }
        GateError::Failed(reason) => ApiReject::internal(reason),
    }
}

/// `POST /api/login`: the password for a session, or a fresh one for an existing
/// session.
///
/// It is outside the gate — a login is how a session comes to exist — but the
/// `Host` check and the rate limits still apply, and it answers `no-store` so no
/// cache keeps the reply.
pub(crate) async fn login(
    State(state): State<AppState>,
    Extension(auth::Peer(peer)): Extension<auth::Peer>,
    headers: HeaderMap,
    Json(body): Json<LoginRequest>,
) -> Response {
    let host = auth::host_of(&headers);
    let user_agent = auth::header_str(&headers, header::USER_AGENT);
    let current = cookie::session_id(auth::header_str(&headers, header::COOKIE));
    match state
        .remote
        .login(peer, &body.password, body.remember, user_agent, current)
        .await
    {
        Ok(outcome) => {
            let now = state.remote.now();
            let reply = LoginReply {
                ok: true,
                session: shprd_remote::dto::SessionInfoDto::new(&outcome.record, now),
                refreshed: outcome.refreshed,
            };
            with_cookie(Json(reply).into_response(), cookie::set(&outcome.id, host))
        }
        Err(GateError::Unauthorized) => failure(StatusCode::UNAUTHORIZED, None),
        Err(GateError::TooManyRequests { after_ms }) => failure(
            StatusCode::TOO_MANY_REQUESTS,
            Some(after_ms.div_ceil(1000).max(1)),
        ),
        Err(err) => reject(err).into_response(),
    }
}

/// `POST /api/logout`: ends the session the cookie names, and clears it.
pub(crate) async fn logout(
    State(state): State<AppState>,
    Extension(door): Extension<Door>,
    headers: HeaderMap,
) -> Response {
    let host = auth::host_of(&headers);
    let current = cookie::session_id(auth::header_str(&headers, header::COOKIE));
    // A gateway that cannot rewrite its session file is a logout that did not
    // happen, and pretending it did would leave a live session in a browser that
    // believes it is signed out.
    let revoked = match state.remote.logout(current) {
        Ok(revoked) => revoked,
        Err(err) => return reject(GateError::Failed(err.to_string())).into_response(),
    };
    let _ = door;
    with_cookie(
        Json(LogoutReply { ok: true, revoked }).into_response(),
        cookie::clear(host),
    )
}

/// `GET /api/session`: who is asking, through which door, and how fresh.
pub(crate) async fn session(
    State(state): State<AppState>,
    Extension(door): Extension<Door>,
) -> Response {
    // A password file that cannot be read is not "no password": the view says
    // one is set, and the login route is where the failure is shown, because
    // this answer must not tell a stranger anything about the file.
    let password_set = state.remote.password().is_set().unwrap_or(true);
    let view: SessionDto = state.remote.view(&door, password_set);
    ([(header::CACHE_CONTROL, "no-store")], Json(view)).into_response()
}

/// `GET /api/sessions`: every live session, with the one that asked marked.
///
/// Only short ids travel: the cookie value is never in an answer, so the list
/// cannot be replayed into a session.
pub(crate) async fn sessions(
    State(state): State<AppState>,
    Extension(door): Extension<Door>,
) -> Result<Response, ApiReject> {
    let now = state.remote.now();
    let records = state
        .remote
        .sessions()
        .list()
        .map_err(|err| reject(GateError::Failed(err.to_string())))?;
    let current = match &door {
        Door::Session(record) => Some(record.short_id().to_string()),
        Door::Token => None,
    };
    let list = SessionListDto {
        sessions: records
            .iter()
            .map(|record| SessionInfoDto::new(record, now))
            .collect(),
        current,
    };
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(list)).into_response())
}

/// `DELETE /api/sessions/{id}`: signs one device out, by its short id.
///
/// It needs a fresh password through the session door, as every destructive
/// action does: a stolen cookie must not be able to sign the owner out of their
/// own devices. An id that names nothing is a `404`, and one that names more
/// than one session removes none of them.
pub(crate) async fn revoke(
    State(state): State<AppState>,
    Extension(door): Extension<Door>,
    Path(id): Path<String>,
) -> Result<Json<LogoutReply>, ApiReject> {
    state.remote.require_fresh(&door).map_err(reject)?;
    match state.remote.sessions().revoke_by_id(&id) {
        Ok(Revoked::Removed(_)) => Ok(Json(LogoutReply {
            ok: true,
            revoked: true,
        })),
        Ok(Revoked::Nothing) => Err(ApiReject::new(
            StatusCode::NOT_FOUND,
            crate::dto::ErrorCode::NotFound,
            "no session has that id",
        )),
        Ok(Revoked::Ambiguous(_)) => Err(ApiReject::bad_request(
            "that id names more than one session; give more of it",
        )),
        Err(err) => Err(reject(GateError::Failed(err.to_string()))),
    }
}

/// A refused login: one message, and a `Retry-After` when it was a rate limit
/// (whose message says the limit was hit instead).
fn failure(status: StatusCode, retry_after: Option<u64>) -> Response {
    // A rate limit says so and says when to come back: it is true of the
    // request whatever the password was, so it leaks nothing, and "that password
    // did not work" would send an owner who is locked out to retype a right one.
    let message = match retry_after {
        Some(after) => format!("too many sign-in attempts; try again in {after} seconds"),
        None => FAILURE.to_string(),
    };
    let mut response = (
        status,
        Json(LoginFailure {
            ok: false,
            message,
            retry_after,
        }),
    )
        .into_response();
    if let Some(after) = retry_after
        && let Ok(value) = HeaderValue::from_str(&after.to_string())
    {
        response.headers_mut().insert(header::RETRY_AFTER, value);
    }
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// Adds the session cookie to `response`, and makes it uncacheable.
fn with_cookie(mut response: Response, cookie: String) -> Response {
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
