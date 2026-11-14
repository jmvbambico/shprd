//! The gate every route behind `/api/health` passes through.
//!
//! It is deliberately the same shape as Shepherdr's local door, minus the
//! tailnet: a bearer token from the state directory, `x-shprd: 1` on writes so a
//! cross-site form cannot post one, and an `Origin` check so a browser page
//! served from anywhere else cannot read this service either.

use axum::extract::{Request, State};
use axum::http::{Method, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::api::AppState;
use crate::error::ApiReject;

/// The header a write must carry.
pub const WRITE_HEADER: &str = "x-shprd";

/// Refuses a request without a valid token, from another origin, or a write
/// without the custom header.
pub(crate) async fn require(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    if let Some(origin) = request
        .headers()
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        && !is_own_origin(&state, origin)
    {
        return ApiReject::cross_origin(origin).into_response();
    }

    match presented_token(&request) {
        Some(token) if secret_eq(&token, &state.token) => {}
        _ => return ApiReject::unauthorized().into_response(),
    }

    if is_write(request.method()) && !carries_write_header(&request) {
        return ApiReject::missing_header().into_response();
    }

    next.run(request).await
}

/// Whether `origin` is this service's own page origin.
///
/// The listener is loopback only, so a page that may call it is one served from
/// the same loopback port. A browser always sends `Origin` on a cross-origin
/// request and on every write, which is what makes this check worth something:
/// no page can drive the service from another site, whatever the token.
pub(crate) fn is_own_origin(state: &AppState, origin: &str) -> bool {
    let port = state.port;
    origin == format!("http://127.0.0.1:{port}") || origin == format!("http://localhost:{port}")
}

/// The token a request presented, from its header or, on a socket, its query.
///
/// A browser cannot set headers on a WebSocket handshake, so `?token=` is
/// accepted there and only there — a loopback listener whose query strings never
/// leave this machine.
fn presented_token(request: &Request) -> Option<String> {
    if let Some(value) = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    {
        let (scheme, token) = value.split_once(' ')?;
        if scheme.eq_ignore_ascii_case("bearer") {
            return Some(token.trim().to_string());
        }
    }
    if request.uri().path() != "/api/ws" {
        return None;
    }
    let query = request.uri().query()?;
    query.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name == "token").then(|| value.to_string())
    })
}

/// Whether the request changes anything.
fn is_write(method: &Method) -> bool {
    !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

/// Whether a write carries `x-shprd: 1`.
fn carries_write_header(request: &Request) -> bool {
    request
        .headers()
        .get(WRITE_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim() == "1")
        .unwrap_or(false)
}

/// Compares two secrets without letting the comparison time say how much of the
/// token was right.
pub(crate) fn secret_eq(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    let mut difference = left.len() ^ right.len();
    for index in 0..left.len().max(right.len()) {
        let a = left.get(index).copied().unwrap_or(0);
        let b = right.get(index).copied().unwrap_or(0);
        difference |= usize::from(a ^ b);
    }
    difference == 0
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
            Some("abc".to_string())
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
        assert_eq!(presented_token(&bearer), Some("header-token".to_string()));

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
    fn a_secret_compares_exactly() {
        assert!(secret_eq("abc", "abc"));
        assert!(!secret_eq("abc", "abd"));
        assert!(!secret_eq("abc", "ab"));
        assert!(!secret_eq("", "a"));
        assert!(secret_eq("", ""));
    }

    #[test]
    fn a_write_is_anything_but_a_read() {
        assert!(is_write(&Method::POST));
        assert!(is_write(&Method::DELETE));
        assert!(!is_write(&Method::GET));
        assert!(!is_write(&Method::HEAD));
        assert!(!is_write(&Method::OPTIONS));
    }
}
