//! The session cookie: its name, its attributes, and reading it back.
//!
//! The cookie the researched design names is
//! `__Host-shprd=<id>; Secure; HttpOnly; SameSite=Lax; Path=/`. The `__Host-`
//! prefix is what stops a sibling subdomain from setting it — a browser refuses
//! a `__Host-` cookie that has a `Domain`, is not `Secure`, or whose path is not
//! `/` — and `SameSite=Lax` still lets the PWA install flow and a top-level GET
//! work (M7d §4.4, threat rows 4 and 5).
//!
//! **Both the prefix and `Secure` are dropped on plain loopback http**, and only
//! there: a browser will not store a `Secure` cookie that arrived over
//! `http://127.0.0.1`, so a loopback page would sign in and then be forgotten on
//! every request. The decision is a property of the request's host that reached
//! this service — a request through a tunnel is https at the edge even though it
//! is http on the loopback leg, so it gets the secure form.

use crate::hosts::is_loopback_request;

/// The cookie's name when it is set over https.
pub const SECURE_NAME: &str = "__Host-shprd";

/// The cookie's name on plain loopback http, where `__Host-` cannot be stored.
pub const PLAIN_NAME: &str = "shprd";

/// The cookie name for a request that arrived on `host`.
pub fn name_for(host: Option<&str>) -> &'static str {
    if is_loopback_request(host) {
        PLAIN_NAME
    } else {
        SECURE_NAME
    }
}

/// The `Set-Cookie` value that starts a session.
///
/// `host` is the request's `Host` header: a loopback host gets the plain form,
/// and any other host — the tunnel's — gets `Secure` and the `__Host-` prefix.
pub fn set(id: &str, host: Option<&str>) -> String {
    let secure = !is_loopback_request(host);
    let mut cookie = format!("{}={id}; Path=/; HttpOnly; SameSite=Lax", name_for(host));
    if secure {
        cookie.push_str("; Secure");
    }
    cookie
}

/// The `Set-Cookie` value that ends a session in the browser.
pub fn clear(host: Option<&str>) -> String {
    let secure = !is_loopback_request(host);
    let mut cookie = format!(
        "{}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0",
        name_for(host)
    );
    if secure {
        cookie.push_str("; Secure");
    }
    cookie
}

/// The session id a `Cookie` header carries, from either name.
///
/// Both names are read, and the secure one first: a browser may still hold the
/// plain cookie from a loopback session while the secure one is the current
/// login, and the cookie that is tried first is the one that decides.
pub fn session_id(header: Option<&str>) -> Option<&str> {
    let header = header?;
    find(header, SECURE_NAME).or_else(|| find(header, PLAIN_NAME))
}

/// One cookie's value out of a `Cookie` header.
///
/// The header is `name=value; name=value`, so a value is compared by name and
/// never by "contains": a cookie called `shprd-other` must not be read as
/// `shprd`.
pub fn find<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    for pair in header.split(';') {
        let pair = pair.trim();
        let (key, value) = match pair.split_once('=') {
            Some((key, value)) => (key.trim(), value.trim()),
            None => continue,
        };
        if key == name && !value.is_empty() {
            return Some(value);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn a_tunnel_request_gets_the_secure_host_prefixed_cookie() {
        let cookie = set("abc123", Some("shprd.example.com"));
        assert!(cookie.starts_with("__Host-shprd=abc123;"), "{cookie}");
        assert!(cookie.contains("; Secure"), "{cookie}");
        assert!(cookie.contains("HttpOnly"), "{cookie}");
        assert!(cookie.contains("SameSite=Lax"), "{cookie}");
        assert!(cookie.contains("Path=/"), "{cookie}");
        assert!(!cookie.contains("Domain"), "a __Host- cookie has no Domain");
    }

    #[test]
    fn a_loopback_request_gets_a_plain_cookie_a_browser_will_store() {
        for host in ["127.0.0.1:18080", "localhost:18080", "[::1]:18080"] {
            let cookie = set("abc123", Some(host));
            assert!(cookie.starts_with("shprd=abc123;"), "{host}: {cookie}");
            assert!(!cookie.contains("Secure"), "{host}: {cookie}");
            assert!(cookie.contains("HttpOnly"), "{host}: {cookie}");
        }
    }

    #[test]
    fn clearing_ends_both_forms_and_keeps_their_attributes() {
        let secure = clear(Some("shprd.example.com"));
        assert!(secure.starts_with("__Host-shprd=;"), "{secure}");
        assert!(secure.contains("Max-Age=0"), "{secure}");
        assert!(secure.contains("; Secure"), "{secure}");

        let plain = clear(Some("127.0.0.1:18080"));
        assert!(plain.starts_with("shprd=;"), "{plain}");
        assert!(plain.contains("Max-Age=0"), "{plain}");
        assert!(!plain.contains("Secure"), "{plain}");
    }

    #[test]
    fn the_cookie_is_read_by_name_and_not_by_containment() {
        let header = "theme=dark; shprd-other=nope; __Host-shprd=the-session; shprd=plain";
        assert_eq!(session_id(Some(header)), Some("the-session"));
        assert_eq!(find(header, "theme"), Some("dark"));
        assert_eq!(find(header, "shprd-other"), Some("nope"));
        assert_eq!(find(header, "__Host-shprd"), Some("the-session"));
        assert_eq!(find(header, "shprd"), Some("plain"));
        assert_eq!(find(header, "shprd-oth"), None);
        assert_eq!(find(header, "__Host-shpr"), None);
    }

    #[test]
    fn a_plain_cookie_is_read_after_the_secure_one() {
        assert_eq!(session_id(Some("shprd=plain")), Some("plain"));
        assert_eq!(session_id(Some("shprd=; __Host-shprd=")), None);
        assert_eq!(session_id(None), None);
        assert_eq!(session_id(Some("")), None);
    }
}
