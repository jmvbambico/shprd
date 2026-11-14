//! The HTTP surface: five routes, one of them a socket.
//!
//! `GET /api/health` is the only unauthenticated one — it is the handshake, not
//! a secret. Everything else is behind [`crate::auth`]. Reads answer from the
//! fold (a snapshot or a change notice); the only write is `POST /api/action`,
//! which hands one [`Action`] to [`crate::actions`] and adds nothing of its own.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, middleware};
use serde::Deserialize;
use serde_json::Value;
use shprd_core::event::Actor;
use tower_http::trace::TraceLayer;

use crate::actions;
use crate::auth;
use crate::dto::{
    Action, ActionReply, ChangeDto, ClientMessage, ErrorCode, HealthDto, SnapshotDto, WsMessage,
};
use crate::error::{ApiReject, ServerError};
use crate::hub::Hub;
use crate::ledger::{self, Stamp};
use crate::service::{AfterWrite, ServiceFiles, ServiceInfo, VERSION};

/// How long a change request waits when it does not say.
const DEFAULT_WAIT: Duration = Duration::from_secs(10);

/// The longest a change request may wait, so a client cannot pin a task for
/// ever.
const MAX_WAIT: Duration = Duration::from_secs(30);

/// How often a waiter re-checks the log on its own, in case a wake-up was
/// missed or another process appended.
const TICK: Duration = Duration::from_millis(200);

/// What every handler sees.
#[derive(Debug, Clone)]
pub(crate) struct AppState {
    /// The repository whose run log this service serves.
    pub(crate) repo: PathBuf,
    /// The port the listener bound, which is what "same origin" means.
    pub(crate) port: u16,
    /// The bearer token every route behind the gate accepts.
    pub(crate) token: String,
    /// This start's identity.
    pub(crate) id: String,
    /// The service's version.
    pub(crate) version: String,
    /// Who the actions act as.
    pub(crate) actor: Actor,
    /// The change counter and its wake-up.
    pub(crate) hub: Hub,
    /// The binary's own step after a write, if it supplied one.
    pub(crate) after_write: Option<AfterWrite>,
}

/// One prepared service: its state, its router and its discovery files.
///
/// It is the seam a test drives without a socket, and what [`crate::service`]
/// serves. Preparing it writes `service.json` and the token, because a client
/// that cannot find the port cannot ask anything.
#[derive(Debug)]
pub struct App {
    state: AppState,
    router: Router,
    files: ServiceFiles,
}

impl App {
    /// Writes the discovery files for `port` and builds the router.
    ///
    /// `after_write` is the binary's own step after a write (its dagr
    /// projection); a test passes `None`.
    pub fn prepare(
        repo: &Path,
        port: u16,
        actor: &Actor,
        state_dir: &Path,
        after_write: Option<AfterWrite>,
    ) -> Result<Self, ServerError> {
        let files = ServiceFiles::prepare(state_dir)?;
        files.write_record(port)?;
        let state = AppState {
            repo: repo.to_path_buf(),
            port,
            token: files.token().to_string(),
            id: files.id().to_string(),
            version: VERSION.to_string(),
            actor: actor.clone(),
            hub: Hub::new(),
            after_write,
        };
        let router = router(state.clone());
        Ok(Self {
            state,
            router,
            files,
        })
    }

    /// The router, ready to be served or driven by `tower::ServiceExt`.
    pub fn router(&self) -> Router {
        self.router.clone()
    }

    /// What the service tells a client about itself.
    pub fn info(&self) -> ServiceInfo {
        ServiceInfo {
            port: self.state.port,
            id: self.state.id.clone(),
            version: self.state.version.clone(),
            pid: std::process::id(),
            token_path: self.files.token_path(),
            service_json: self.files.service_json_path(),
        }
    }

    /// Removes `service.json`, on a clean shutdown.
    pub fn remove_record(&self) -> Result<(), ServerError> {
        self.files.remove_record()
    }
}

/// The whole API, with the gate in front of everything but health.
pub(crate) fn router(state: AppState) -> Router {
    let guarded = Router::new()
        .route("/api/state", get(snapshot_handler))
        .route("/api/changes", get(changes))
        .route("/api/action", post(action))
        .route("/api/ws", get(socket))
        .layer(middleware::from_fn_with_state(state.clone(), auth::require));
    Router::new()
        .route("/api/health", get(health))
        .merge(guarded)
        .fallback(not_found)
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// `GET /api/health`: who this is, for the handshake.
async fn health(State(state): State<AppState>) -> Json<HealthDto> {
    Json(HealthDto {
        ok: true,
        id: state.id.clone(),
        version: state.version.clone(),
    })
}

/// `GET /api/state`: the whole board, folded now.
async fn snapshot_handler(State(state): State<AppState>) -> Result<Json<SnapshotDto>, ApiReject> {
    let repo = state.repo.clone();
    let loaded = blocking(move || ledger::load(&repo)).await?;
    let (folded, _) = loaded.map_err(|err| ApiReject::internal(err.to_string()))?;
    Ok(Json(SnapshotDto::from_state(&folded)))
}

/// The query `GET /api/changes` takes.
#[derive(Debug, Deserialize)]
struct ChangesQuery {
    /// The last sequence the client already has.
    since: Option<u64>,
    /// How long to wait for more, in seconds.
    timeout: Option<u64>,
}

/// `GET /api/changes`: long-poll for events after `since`.
///
/// It answers the events themselves (a snapshot's worth of information in one
/// call, which is what a Herdr mod can afford), or `204` when the wait runs out.
async fn changes(
    State(state): State<AppState>,
    Query(query): Query<ChangesQuery>,
) -> Result<Response, ApiReject> {
    let since = query.since.unwrap_or(0);
    let wait = Duration::from_secs(query.timeout.unwrap_or(DEFAULT_WAIT.as_secs()));
    let wait = wait.min(MAX_WAIT);
    let deadline = Instant::now() + wait;
    let mut seen: Option<Option<Stamp>> = None;

    loop {
        let current = {
            let repo = state.repo.clone();
            let stamp = blocking(move || ledger::stamp(&repo)).await?;
            stamp.map_err(|err| {
                ApiReject::internal(format!("could not look at the run log: {err}"))
            })?
        };
        let changed = match &seen {
            None => true,
            Some(previous) => previous != &current,
        };
        if changed {
            let repo = state.repo.clone();
            let events = blocking(move || ledger::read_since(&repo, since)).await?;
            let events = events.map_err(|err| ApiReject::internal(err.to_string()))?;
            seen = Some(current);
            // An empty answer means the log is behind the client, or has no
            // event past `since`: wait for one rather than answer nothing.
            if !events.is_empty() {
                let seq = events.last().map(|event| event.seq).unwrap_or(since);
                let notices = ledger::notices(&events).map_err(|err| {
                    ApiReject::internal(format!("could not project events: {err}"))
                })?;
                return Ok(Json(ChangeDto {
                    seq,
                    events: notices,
                })
                .into_response());
            }
        }

        let now = Instant::now();
        if now >= deadline {
            return Ok(StatusCode::NO_CONTENT.into_response());
        }
        let remaining = (deadline - now).min(TICK);
        tokio::select! {
            _ = state.hub.wait() => {}
            _ = tokio::time::sleep(remaining) => {}
        }
    }
}

/// `POST /api/action`: one envelope, run against the same log the CLI writes.
async fn action(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Json<ActionReply>, ApiReject> {
    let value: Value = serde_json::from_slice(&body)
        .map_err(|err| ApiReject::bad_request(format!("the body is not JSON: {err}")))?;
    let kind = value
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ApiReject::bad_request(format!(
                "the body needs a \"kind\": one of {}",
                Action::KINDS.join(", ")
            ))
        })?
        .to_string();
    if !Action::KINDS.contains(&kind.as_str()) {
        return Err(ApiReject::unknown_action(&kind));
    }
    let action: Action = Action::from_value(value)
        .map_err(|err| ApiReject::bad_request(format!("a {kind:?} action is malformed: {err}")))?;

    let repo = state.repo.clone();
    let actor = state.actor.clone();
    let hub = state.hub.clone();
    let after_write = state.after_write.clone();
    let reply =
        blocking(move || actions::run(&repo, &actor, &hub, after_write.as_ref(), action)).await?;
    Ok(Json(reply?))
}

/// `GET /api/ws`: the same change notices, pushed.
async fn socket(State(state): State<AppState>, upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(move |socket| pump(socket, state))
}

/// Pushes a hello, then one change per batch of new events, until the client
/// goes away.
///
/// The loop's tick is what makes a missed wake-up harmless: the log is the truth
/// and it is re-read regardless.
async fn pump(mut socket: WebSocket, state: AppState) {
    let seq = {
        let repo = state.repo.clone();
        match blocking(move || ledger::last_seq(&repo)).await {
            Ok(Ok(seq)) => seq,
            // A log that will not read yet is an empty one: hello says so and
            // the tick below will catch up.
            Ok(Err(_)) | Err(_) => 0,
        }
    };
    let hello = WsMessage::Hello {
        id: state.id.clone(),
        version: state.version.clone(),
        seq,
    };
    if send(&mut socket, &hello).await.is_err() {
        return;
    }

    // Anything published between reading the log and the hello above is still
    // delivered: the socket starts from the furthest of the two watermarks.
    let mut since = seq.max(state.hub.last());
    loop {
        let tick = tokio::time::sleep(TICK);
        tokio::select! {
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    if let Ok(ClientMessage::Ping { at }) = serde_json::from_str(&text)
                        && send(&mut socket, &WsMessage::Pong { at }).await.is_err()
                    {
                        return;
                    }
                }
                Some(Ok(Message::Close(_)) | Err(_)) | None => return,
                Some(Ok(_)) => {}
            },
            _ = state.hub.wait() => {}
            _ = tick => {}
        }

        let repo = state.repo.clone();
        let events = match blocking(move || ledger::read_since(&repo, since)).await {
            Ok(Ok(events)) => events,
            // A log that will not read is not a reason to drop the socket; the
            // next tick tries again.
            Ok(Err(_)) | Err(_) => continue,
        };
        if events.is_empty() {
            continue;
        }
        let seq = events.last().map(|event| event.seq).unwrap_or(since);
        let Ok(notices) = ledger::notices(&events) else {
            continue;
        };
        let message = WsMessage::Change {
            change: ChangeDto {
                seq,
                events: notices,
            },
        };
        if send(&mut socket, &message).await.is_err() {
            return;
        }
        since = seq;
    }
}

/// Sends one message as JSON text.
async fn send(socket: &mut WebSocket, message: &WsMessage) -> Result<(), ()> {
    let text = serde_json::to_string(message).map_err(|_| ())?;
    socket
        .send(Message::Text(text.into()))
        .await
        .map_err(|_| ())
}

/// Always a typed body, so a client never has to parse HTML to learn why a path
/// it guessed did not exist.
async fn not_found() -> impl IntoResponse {
    ApiReject::new(StatusCode::NOT_FOUND, ErrorCode::NotFound, "no such route")
}

/// Runs a blocking ledger operation off the async runtime.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, ApiReject> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|err| ApiReject::internal(format!("the ledger task did not finish: {err}")))
}
