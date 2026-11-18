//! Which headers a request is believed about.
//!
//! Every request reaches the service from `127.0.0.1` — the tunnel's agent is a
//! local process — so a header cannot be told from a visitor's by who sent it.
//! M7d §4.3 rule 5 therefore trusts a provider's header only where that
//! provider's agent is the one that sets it *and drops a visitor's copy*:
//! `tailscale serve` for `Tailscale-User-Login`. Nothing else is believed:
//! `X-Forwarded-For` never changes the peer a rate limit is keyed on, and no
//! `Cf-Access-*` or `X-Forwarded-Proto` is read (the cookie's `Secure` follows
//! the `Host`, which an allowlist already vouches for).

use super::{Provider, Trust};
use crate::hosts::same_host;

/// The endpoint a running tunnel is reached at, as the gate's side needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    /// The provider that is up.
    pub provider: Provider,
    /// The `host[:port]` it is reached under.
    pub host: String,
}

/// The login `Tailscale-User-Login` names, when it may be believed.
///
/// It is believed only while the tunnel that is up is Tailscale serve, and only
/// for a request addressed to that tunnel's own host: a request to any other
/// name, or with no tunnel up, carries a header that is just text.
pub fn trusted_login(
    edge: Option<&Edge>,
    host: Option<&str>,
    header: Option<&str>,
) -> Option<String> {
    let edge = edge?;
    if edge.provider.trust() != Trust::Tailnet || !same_host(host?, &edge.host) {
        return None;
    }
    let login = header?.trim();
    if login.is_empty() || login.len() > 256 || login.chars().any(char::is_control) {
        return None;
    }
    Some(login.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge(provider: Provider) -> Edge {
        Edge {
            provider,
            host: "studio.tail1234.ts.net".to_owned(),
        }
    }

    #[test]
    fn login_is_believed_only_from_tailscale_serve_on_its_own_host() {
        let ts = edge(Provider::Tailscale);
        let host = Some("studio.tail1234.ts.net");
        let login = Some("person@example.com");
        assert_eq!(
            trusted_login(Some(&ts), host, login).as_deref(),
            Some("person@example.com")
        );
        // The same header through a public tunnel is a visitor's text.
        assert_eq!(
            trusted_login(Some(&edge(Provider::CloudflaredQuick)), host, login),
            None
        );
        assert_eq!(
            trusted_login(Some(&edge(Provider::Tunnl)), host, login),
            None
        );
        // No tunnel up, or another host, or loopback.
        assert_eq!(trusted_login(None, host, login), None);
        assert_eq!(
            trusted_login(Some(&ts), Some("127.0.0.1:8080"), login),
            None
        );
        assert_eq!(trusted_login(Some(&ts), Some("evil.example"), login), None);
        assert_eq!(trusted_login(Some(&ts), None, login), None);
        // A missing or odd value.
        assert_eq!(trusted_login(Some(&ts), host, None), None);
        assert_eq!(trusted_login(Some(&ts), host, Some("  ")), None);
        assert_eq!(trusted_login(Some(&ts), host, Some("a\nb")), None);
    }
}
