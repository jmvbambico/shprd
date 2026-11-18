//! The tunnel routes: `GET /api/tunnels`, `POST /api/tunnels/start` and
//! `POST /api/tunnels/stop`.
//!
//! They are thin over [`shprd_remote::tunnel::TunnelManager`], which holds every
//! rule that matters — the password a public tunnel needs, one tunnel at a time,
//! the allowlist that follows it — so the CLI and a page get the same answers
//! whichever door they came by. What is added here is the part that is about
//! the request:
//!
//! * **start needs a fresh password** through the session door, like every
//!   destructive action: a stolen cookie must not be able to put this machine
//!   on a new public address (the loopback token is exempt, as everywhere);
//! * **nothing in a reply or a log is an address except the reply to the person
//!   who is signed in**, and the log never is.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

use axum::Extension;
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use shprd_remote::Door;
use shprd_remote::dto::{TunnelProviderDto, TunnelStartRequest, TunnelStatusDto};
use shprd_remote::tunnel::{
    PROVIDERS, Provider, StartRequest, TunnelError, WebFile, trusted_login,
};

use crate::api::AppState;
use crate::auth;
use crate::dto::ErrorCode;
use crate::error::ApiReject;
use crate::remote::reject;

/// The tunnel as it stands, for a client that is signed in.
pub(crate) fn status(state: &AppState, headers: &HeaderMap) -> TunnelStatusDto {
    let file = state.tunnels.file();
    let tailnet_user = trusted_login(
        state.tunnels.edge().as_ref(),
        auth::host_of(headers),
        auth::header_str(
            headers,
            axum::http::HeaderName::from_static("tailscale-user-login"),
        ),
    );
    dto(state, file, state.tunnels.starting(), tailnet_user)
}

/// The wire shape of a record plus what the page needs around it.
fn dto(
    state: &AppState,
    file: WebFile,
    starting: bool,
    tailnet_user: Option<String>,
) -> TunnelStatusDto {
    // A password file that cannot be read is "not set" here: the page must not
    // offer a public tunnel the manager would refuse.
    let password_set = state.remote.password().is_set().unwrap_or(false);
    let (default_provider, _) = crate::settings::tunnel_defaults(state);
    TunnelStatusDto {
        on: file.on,
        starting,
        provider: file.provider,
        link: file.link,
        qr_svg: file.qr_svg,
        why: file.why,
        at: file.at,
        version: file.version,
        password_set,
        default_provider: default_provider.word().to_owned(),
        providers: PROVIDERS
            .into_iter()
            .map(|provider| TunnelProviderDto {
                id: provider.word().to_owned(),
                public_internet: provider.public_internet(),
            })
            .collect(),
        tailnet_user,
    }
}

/// `GET /api/tunnels`: whether a tunnel is up, where, and what could be started.
pub(crate) async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Json<TunnelStatusDto> {
    Json(status(&state, &headers))
}

/// `POST /api/tunnels/start`: start one tunnel, or say why not.
///
/// The provider and its options are the body's, or the `web.*` settings' where
/// the body says nothing. It answers once the tunnel is up or has failed, which
/// can take most of a minute for a provider that has to reach its edge.
pub(crate) async fn start(
    State(state): State<AppState>,
    Extension(door): Extension<Door>,
    body: Option<Json<TunnelStartRequest>>,
) -> Result<Json<TunnelStatusDto>, ApiReject> {
    state.remote.require_fresh(&door).map_err(reject)?;
    let body = body.map(|Json(body)| body).unwrap_or_default();
    let (default_provider, mut options) = crate::settings::tunnel_defaults(&state);
    let provider = match body.provider.as_deref() {
        Some(word) => Provider::parse(word).ok_or_else(|| {
            ApiReject::bad_request(format!(
                "{word:?} is not a tunnel provider; use one of {}",
                PROVIDERS.map(Provider::word).join(", ")
            ))
        })?,
        None => default_provider,
    };
    if let Some(stable) = body.stable {
        options.stable = stable;
    }
    let local = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, state.port));
    let file = state
        .tunnels
        .start(StartRequest { provider, options }, local)
        .await
        .map_err(tunnel_reject)?;
    let tailnet_user = None;
    Ok(Json(dto(&state, file, false, tailnet_user)))
}

/// `POST /api/tunnels/stop`: stop the tunnel. Stopping when none is up is fine.
pub(crate) async fn stop(
    State(state): State<AppState>,
) -> Result<Json<TunnelStatusDto>, ApiReject> {
    let file = state
        .tunnels
        .stop(Some("stopped from the web app"))
        .await
        .map_err(tunnel_reject)?;
    Ok(Json(dto(&state, file, false, None)))
}

/// A tunnel refusal as an HTTP answer.
///
/// A rule or a setup problem is `409` with the words the CLI prints; a provider
/// that ran and failed is `502`, because the service is fine and the thing it
/// reached for is not.
pub(crate) fn tunnel_reject(err: TunnelError) -> ApiReject {
    match err {
        TunnelError::Failed(message) => {
            ApiReject::new(StatusCode::BAD_GATEWAY, ErrorCode::Internal, message)
        }
        other => ApiReject::refused(other.to_string()),
    }
}
