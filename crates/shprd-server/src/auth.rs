//! The gate every route but health and login passes through, and the two checks
//! that come before it.
//!
//! Three middlewares, outermost first:
//!
//! | | what it does |
//! |---|---|
//! | [`peer`] | records the client address the request came from (the socket's, or over the tunnel the one its edge reported), which is what a rate limit is keyed on |
//! | [`host`] | refuses a request addressed to a name this service does not answer to, with `421` |
//! | [`require`] | the gate itself: the token door or a session, `Origin` and `x-shprd` on a write, the limits |
//!
//! The decision is [`shprd_remote::Gateway::decide`], a plain function over a
//! [`shprd_remote::GateRequest`], so the table is a unit test in that crate and
//! this file only fills it in and turns the answer into an HTTP response.
//!
//! `peer` and `host` run for health and login too. Health is the handshake and
//! stays unauthenticated, but it is still answered only under a name this
//! service knows — an unknown `Host` is the DNS-rebinding attempt, whichever
//! route it is aimed at.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, HeaderName, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use shprd_remote::GateRequest;

use crate::api::AppState;
use crate::error::ApiReject;

/// The header a write must carry.
pub const WRITE_HEADER: &str = "x-shprd";

/// The peer address, inserted by [`peer`] for everything behind it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Peer(pub IpAddr);

/// Records the address the request came from.
///
/// The listener is served with `into_make_service_with_connect_info`, so the
/// socket address is the kernel's, not a header's. A request that came over the
/// active tunnel (from the loopback agent, addressed to the tunnel's own host)
/// is keyed on the address its provider's edge reported instead, since its
/// socket address is the same for every visitor; any other request keeps its
/// socket address and its headers are just text
/// ([`shprd_remote::tunnel::client_address`]). A router driven without a socket
/// — every test that uses `tower::ServiceExt` — has no peer, and is treated as
/// loopback, which is what it is.
pub(crate) async fn peer(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let socket = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0.ip())
        .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
    let headers = request.headers();
    let address = shprd_remote::tunnel::client_address(
        state.tunnels.edge().as_ref(),
        socket,
        host_of(headers),
        header_str(headers, HeaderName::from_static("x-forwarded-for")),
        header_str(headers, HeaderName::from_static("cf-connecting-ip")),
    );
    request.extensions_mut().insert(Peer(address));
    next.run(request).await
}

/// Whether the request is a local caller: addressed to a loopback name, with
/// none of the headers a tunnel's edge adds. A tunnel's visitors are addressed
/// to the tunnel's host, and their requests carry forwarding headers, so
/// neither is loopback; the check is for what only a local client should see
/// (the service's id on `GET /api/health`).
pub(crate) fn is_local_caller(headers: &HeaderMap) -> bool {
    shprd_remote::hosts::is_loopback_request(host_of(headers))
        && [
            "x-forwarded-for",
            "x-forwarded-host",
            "forwarded",
            "cf-connecting-ip",
            "cf-ray",
        ]
        .iter()
        .all(|name| !headers.contains_key(HeaderName::from_static(name)))
}

/// Refuses a request addressed to a name this service does not answer to.
///
/// The loopback names are always allowed and a tunnel adds its own through
/// [`crate::App::allowed_hosts`]. A `421` and not a redirect: telling an unknown
/// caller which name would work is exactly what a rebinding attack needs.
pub(crate) async fn host(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let host = host_of(request.headers());
    if !state.remote.host_allowed(host) {
        return ApiReject::wrong_host(host).into_response();
    }
    next.run(request).await
}

/// The gate: allow the request through one door, or refuse it.
pub(crate) async fn require(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let gate = gate_request(&request);
    let decision = state.remote.decide(&gate);
    match decision {
        Ok(door) => {
            let mut request = request;
            request.extensions_mut().insert(door);
            next.run(request).await
        }
        Err(err) => crate::remote::reject(err).into_response(),
    }
}

/// What one request looks like to the gate.
fn gate_request(request: &Request) -> GateRequest<'_> {
    GateRequest {
        host: host_of(request.headers()),
        method: request.method().as_str(),
        origin: header_str(request.headers(), header::ORIGIN),
        bearer: presented_token(request),
        cookies: header_str(request.headers(), header::COOKIE),
        write_header: carries_write_header(request),
        upgrade: is_socket_upgrade(request.headers()),
        peer: request
            .extensions()
            .get::<Peer>()
            .map(|peer| peer.0)
            .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST)),
    }
}

/// One header as a string, when it is one.
pub(crate) fn header_str(headers: &HeaderMap, name: header::HeaderName) -> Option<&str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

/// The `Host` the request was addressed to.
///
/// HTTP/1.1 requires one, so a request without it is refused by the gate rather
/// than guessed at.
pub(crate) fn host_of(headers: &HeaderMap) -> Option<&str> {
    header_str(headers, header::HOST)
}

/// Whether the request asks to become a WebSocket.
///
/// A socket is a write: the page types into a terminal through one, so the same
/// `Origin` and `x-shprd` rules apply (threat row 12).
pub(crate) fn is_socket_upgrade(headers: &HeaderMap) -> bool {
    let connection = headers
        .get(header::CONNECTION)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_default();
    let upgrade = headers
        .get(header::UPGRADE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_default();
    connection.split(',').any(|part| part.trim() == "upgrade") && upgrade == "websocket"
}

/// The token a request presented, from its header or, on a socket, its query.
///
/// A browser cannot set headers on a WebSocket handshake, so `?token=` is
/// accepted there and only there — a loopback listener whose query strings never
/// leave this machine. A browser page never sees the token at all.
fn presented_token(request: &Request) -> Option<&str> {
    if let Some(value) = header_str(request.headers(), header::AUTHORIZATION) {
        let (scheme, token) = value.split_once(' ')?;
        if scheme.eq_ignore_ascii_case("bearer") {
            return Some(token.trim());
        }
    }
    if request.uri().path() != "/api/ws" {
        return None;
    }
    let query = request.uri().query()?;
    query.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name == "token").then_some(value)
    })
}

/// Whether a write carries `x-shprd: 1`.
fn carries_write_header(request: &Request) -> bool {
    header_str(
        request.headers(),
        header::HeaderName::from_static(WRITE_HEADER),
    )
    .map(|value| value.trim() == "1")
    .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use axum::body::Body;

    use super::*;

    /// A request with the given URI and no headers.
    fn request(uri: &str) -> Request {
        Request::builder().uri(uri).body(Body::empty()).unwrap()
    }

    #[test]
    fn a_token_in_the_query_is_only_for_the_socket() {
        assert_eq!(presented_token(&request("/api/state?token=abc")), None);
        assert_eq!(
            presented_token(&request("/api/ws?token=abc")),
            Some("abc"),
            "a browser cannot set headers on a handshake"
        );
        assert_eq!(presented_token(&request("/api/ws?since=1")), None);
    }

    #[test]
    fn the_authorization_header_wins_and_any_scheme_case_is_fine() {
        let bearer = Request::builder()
            .uri("/api/ws?token=query")
            .header(header::AUTHORIZATION, "bearer header-token")
            .body(Body::empty())
            .unwrap();
        assert_eq!(presented_token(&bearer), Some("header-token"));

        let basic = Request::builder()
            .uri("/api/state")
            .header(header::AUTHORIZATION, "Basic dXNlcjpwYXNz")
            .body(Body::empty())
            .unwrap();
        assert_eq!(presented_token(&basic), None, "only a bearer token counts");
    }

    #[test]
    fn only_the_exact_write_header_counts() {
        for (value, expected) in [("1", true), ("0", false), ("yes", false), ("", false)] {
            let request = Request::builder()
                .uri("/api/action")
                .header(WRITE_HEADER, value)
                .body(Body::empty())
                .unwrap();
            assert_eq!(carries_write_header(&request), expected, "{value:?}");
        }
    }

    #[test]
    fn a_socket_upgrade_is_told_apart_from_a_plain_get() {
        let upgrade = Request::builder()
            .uri("/api/ws")
            .header(header::CONNECTION, "keep-alive, Upgrade")
            .header(header::UPGRADE, "websocket")
            .body(Body::empty())
            .unwrap();
        assert!(is_socket_upgrade(upgrade.headers()));

        let plain = Request::builder()
            .uri("/api/state")
            .header(header::CONNECTION, "keep-alive")
            .body(Body::empty())
            .unwrap();
        assert!(!is_socket_upgrade(plain.headers()));

        let header_only = Request::builder()
            .uri("/api/state")
            .header(header::UPGRADE, "websocket")
            .body(Body::empty())
            .unwrap();
        assert!(
            !is_socket_upgrade(header_only.headers()),
            "both headers count"
        );
    }

    #[test]
    fn the_gate_sees_a_host_only_when_the_request_has_one() {
        let with = Request::builder()
            .uri("/api/state")
            .header(header::HOST, "127.0.0.1:18080")
            .body(Body::empty())
            .unwrap();
        assert_eq!(host_of(with.headers()), Some("127.0.0.1:18080"));
        assert_eq!(host_of(request("/api/state").headers()), None);
    }
}
