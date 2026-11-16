//! The `Host` allowlist: which names this service may be reached under.
//!
//! A tunnel adds a hostname, and an attacker can point a `CNAME` at one — so the
//! `Host` header decides whether a request is even considered, before any
//! credential is looked at (M7d §4.3 rule 2, threat row 6). Two kinds of name are
//! allowed:
//!
//! * **loopback**, always: `127.0.0.1`, `localhost` and `[::1]`, on any port.
//!   These are the mods' and the CLI's door and never depend on a tunnel.
//! * **the current endpoints**, added by whoever starts a tunnel. A quick
//!   Cloudflare tunnel gets a new name on every run, so the set is mutable and
//!   the tunnel task removes what it added when it stops.
//!
//! Anything else is unknown, which is a `421` and not a redirect: a wrong `Host`
//! is what DNS rebinding looks like from inside the process.

use std::collections::BTreeSet;
use std::sync::{Arc, RwLock};

use crate::error::RemoteError;

/// The names a loopback request may arrive under.
pub const LOOPBACK_NAMES: &[&str] = &["127.0.0.1", "localhost", "[::1]", "::1"];

/// The set of `Host` values this service answers to.
///
/// A clone shares the set, so the tunnel task can be handed one while the
/// service keeps its own: `add` on the clone is visible to the gate at once.
#[derive(Debug, Clone, Default)]
pub struct AllowedHosts {
    inner: Arc<RwLock<BTreeSet<String>>>,
}

impl AllowedHosts {
    /// The loopback names and nothing else — a service with no tunnel.
    pub fn loopback_only() -> Self {
        Self::default()
    }

    /// The names a tunnel added, plus whatever loopback resolves to.
    ///
    /// It is a constructor for a caller that knows its endpoints up front (a
    /// test, or a start that read a previous tunnel's record); a running tunnel
    /// uses [`AllowedHosts::add`].
    pub fn with(hosts: impl IntoIterator<Item = impl AsRef<str>>) -> Result<Self, RemoteError> {
        let set = Self::loopback_only();
        for host in hosts {
            set.add(host.as_ref())?;
        }
        Ok(set)
    }

    /// Allows `host` — a hostname, or `hostname:port`, which is one endpoint.
    ///
    /// It is refused when it is empty, carries a scheme or a path, or is one of
    /// the loopback names (those are always allowed and are not the tunnel
    /// task's to remove). A malformed entry is an error rather than a silently
    /// narrower allowlist, because a tunnel that thinks it is allowed and is not
    /// would look like the app is broken.
    pub fn add(&self, host: &str) -> Result<(), RemoteError> {
        let host = normalize(host);
        if host.is_empty()
            || host.contains("://")
            || host.contains('/')
            || host.contains(char::is_whitespace)
        {
            return Err(RemoteError::Refused(format!("{host:?} is not a host name")));
        }
        if is_loopback(&host) {
            return Ok(());
        }
        self.write().insert(host);
        Ok(())
    }

    /// Stops allowing `host`.
    ///
    /// Removing a name that was never there is not an error: a tunnel stops once,
    /// and the second stop must not fail.
    pub fn remove(&self, host: &str) {
        let host = normalize(host);
        self.write().remove(&host);
    }

    /// The tunnel names currently allowed, sorted, without the loopback ones.
    pub fn list(&self) -> Vec<String> {
        self.inner
            .read()
            .map(|set| set.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Whether a request's `Host` may be answered at all.
    ///
    /// A missing `Host` is not allowed: HTTP/1.1 requires one, so a request
    /// without it came from something that is not a browser and not this
    /// machine's client, and guessing which host it meant is exactly the guess a
    /// rebinding attack needs.
    pub fn allows(&self, host: Option<&str>) -> bool {
        let Some(host) = host else {
            return false;
        };
        let host = normalize(host);
        if host.is_empty() {
            return false;
        }
        if is_loopback(&host) {
            return true;
        }
        self.inner
            .read()
            .map(|set| set.contains(&host))
            .unwrap_or(false)
    }

    /// Whether `origin` names a host this service answers to.
    ///
    /// The `Origin` of a write must be the page's own origin, and the page is
    /// served from one of the allowed hosts, so this is the same test applied to
    /// the `scheme://host[:port]` form. `Origin: null` (a sandboxed frame, a
    /// `file://` page) never matches.
    pub fn allows_origin(&self, origin: Option<&str>) -> bool {
        self.allows(origin_host(origin))
    }

    /// The lock, with a poisoned one treated as empty rather than panicking.
    ///
    /// A panic while adding a host is not worth taking the service down for, and
    /// an empty set can only refuse — the failure mode that fails closed.
    fn write(&self) -> std::sync::RwLockWriteGuard<'_, BTreeSet<String>> {
        match self.inner.write() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

/// The `host[:port]` an `Origin` names, if it names one at all.
///
/// `Some("abc.trycloudflare.com")` for `https://abc.trycloudflare.com`, and
/// `None` for `null`, an empty string, or anything without a scheme this
/// service could have served.
pub fn origin_host(origin: Option<&str>) -> Option<&str> {
    let origin = origin?;
    let rest = origin
        .strip_prefix("https://")
        .or_else(|| origin.strip_prefix("http://"))?;
    let authority = rest.split('/').next()?;
    if authority.is_empty() {
        return None;
    }
    Some(authority)
}

/// Whether two `host[:port]` values name the same endpoint.
///
/// The comparison is the normalized form — trimmed, lowercase, no trailing dot —
/// so `Origin: https://Localhost:18080` matches `Host: localhost:18080`.
pub fn same_host(left: &str, right: &str) -> bool {
    normalize(left) == normalize(right)
}

/// Whether `host` (`host[:port]`) is a loopback name.
pub fn is_loopback(host: &str) -> bool {
    let name = host_name(host);
    LOOPBACK_NAMES.contains(&name.as_str())
}

/// Whether the request `host` is a plain loopback address.
///
/// It decides two things that are only safe on loopback: the cookie drops
/// `Secure` and the `__Host-` prefix (a browser will not store either over
/// plain http), and the bearer token's `Origin` is held to loopback.
pub fn is_loopback_request(host: Option<&str>) -> bool {
    host.map(is_loopback).unwrap_or(false)
}

/// The name part of `host[:port]`, lowercase, without a trailing dot.
///
/// `[::1]:8080` keeps its brackets, which is how it appears in a `Host` header
/// and in an `Origin`.
fn host_name(host: &str) -> String {
    let trimmed = normalize(host);
    if let Some(rest) = trimmed.strip_prefix('[') {
        // An IPv6 literal: the port, if any, is after the closing bracket.
        if let Some((inside, _)) = rest.split_once(']') {
            return format!("[{inside}]");
        }
        return trimmed;
    }
    trimmed
        .rsplit_once(':')
        .map(|(name, _)| name.to_string())
        .unwrap_or(trimmed)
}

/// A host as this crate compares it: trimmed, lowercase, without a trailing dot.
fn normalize(host: &str) -> String {
    host.trim().trim_end_matches('.').to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn loopback_names_are_always_allowed_on_any_port() {
        let hosts = AllowedHosts::loopback_only();
        for host in [
            "127.0.0.1",
            "127.0.0.1:8080",
            "localhost",
            "LOCALHOST:18080",
            "[::1]",
            "[::1]:8080",
            "localhost.",
        ] {
            assert!(hosts.allows(Some(host)), "{host}");
        }
    }

    #[test]
    fn a_tunnel_name_is_allowed_while_it_is_added_and_not_after() {
        let hosts = AllowedHosts::loopback_only();
        assert!(!hosts.allows(Some("abc.trycloudflare.com")));
        hosts.add("abc.trycloudflare.com").expect("a host");
        assert!(hosts.allows(Some("abc.trycloudflare.com")));
        assert!(
            hosts.allows(Some("ABC.TryCloudflare.com")),
            "case does not matter"
        );
        assert!(
            !hosts.allows(Some("abc.trycloudflare.com:8443")),
            "a different port is a different endpoint"
        );
        assert_eq!(hosts.list(), vec!["abc.trycloudflare.com".to_string()]);
        hosts.remove("abc.trycloudflare.com");
        assert!(!hosts.allows(Some("abc.trycloudflare.com")));
        hosts.remove("abc.trycloudflare.com");
    }

    #[test]
    fn a_clone_shares_the_set_so_a_running_service_sees_the_change() {
        let hosts = AllowedHosts::loopback_only();
        let handle = hosts.clone();
        handle.add("shprd.example.com").expect("a host");
        assert!(hosts.allows(Some("shprd.example.com")));
        hosts.remove("shprd.example.com");
        assert!(!handle.allows(Some("shprd.example.com")));
    }

    #[test]
    fn a_missing_or_empty_host_is_unknown() {
        let hosts = AllowedHosts::loopback_only();
        assert!(!hosts.allows(None));
        assert!(!hosts.allows(Some("")));
        assert!(!hosts.allows(Some("   ")));
    }

    #[test]
    fn a_host_that_is_not_a_name_is_refused_rather_than_stored() {
        let hosts = AllowedHosts::loopback_only();
        for host in ["", "https://abc.example.com", "abc.example.com/path", "a b"] {
            assert!(hosts.add(host).is_err(), "{host:?}");
        }
    }

    #[test]
    fn an_origin_must_be_a_scheme_and_a_host_that_is_allowed() {
        let hosts = AllowedHosts::loopback_only();
        assert!(hosts.allows_origin(Some("http://127.0.0.1:18080")));
        assert!(hosts.allows_origin(Some("http://localhost:18080")));
        assert!(!hosts.allows_origin(Some("https://evil.example")));
        assert!(!hosts.allows_origin(Some("null")));
        assert!(!hosts.allows_origin(None));
        assert!(!hosts.allows_origin(Some("http://")));
        assert!(
            !hosts.allows_origin(Some("ftp://127.0.0.1")),
            "only a scheme a page could be served with"
        );

        hosts.add("abc.trycloudflare.com").expect("a host");
        assert!(hosts.allows_origin(Some("https://abc.trycloudflare.com")));
    }

    #[test]
    fn an_origin_with_a_path_is_not_an_origin_a_page_could_have() {
        assert_eq!(
            origin_host(Some("https://abc.example.com/shepherdr")),
            Some("abc.example.com"),
            "the path is not part of the authority, and the host is what is compared"
        );
    }

    #[test]
    fn the_name_part_survives_a_port_and_an_ipv6_literal() {
        assert_eq!(host_name("127.0.0.1:8080"), "127.0.0.1");
        assert_eq!(host_name("[::1]:8080"), "[::1]");
        assert!(is_loopback("[::1]:8080"));
        assert!(!is_loopback("127.0.0.2"));
    }
}
