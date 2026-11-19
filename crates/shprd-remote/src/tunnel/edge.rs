//! Which headers a request is believed about.
//!
//! Every request reaches the service from `127.0.0.1` — the tunnel's agent is a
//! local process — so a header cannot be told from a visitor's by who sent it.
//! M7d §4.3 rule 5 therefore trusts a provider's header only where that
//! provider's agent is the one that sets it *and drops a visitor's copy*:
//! `tailscale serve` for `Tailscale-User-Login`. Nothing else is believed:
//! `X-Forwarded-For` never changes the peer a rate limit is keyed on **except**
//! by [`client_address`], below, which reads the one header a provider's edge
//! writes for the visitor's address, only on a request addressed to the tunnel's
//! own host, and only from the rightmost entry (the one the edge appended, not
//! one a visitor wrote). No `Cf-Access-*` or `X-Forwarded-Proto` is read (the
//! cookie's `Secure` follows the `Host`, which an allowlist already vouches for).

use std::net::{IpAddr, SocketAddr};

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

/// The header a provider's edge writes the visitor's address into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientHeader {
    /// `X-Forwarded-For`: ngrok's edge and `tailscale serve`'s proxy append the
    /// address they saw, so the **rightmost** entry is theirs; everything to its
    /// left is whatever the visitor sent.
    ForwardedFor,
    /// `CF-Connecting-IP`: Cloudflare's edge sets it from the connection and
    /// overwrites a visitor's copy.
    CfConnectingIp,
}

impl Provider {
    /// The header this provider's edge sets for the visitor's address, if any.
    ///
    /// `tunnl` (an `ssh -R` to a hosted service) is not listed: its proxy's
    /// behaviour is not documented to this project, so nothing it adds is
    /// believed and its visitors share the loopback bucket.
    pub fn client_header(self) -> Option<ClientHeader> {
        match self {
            Self::Ngrok | Self::Tailscale => Some(ClientHeader::ForwardedFor),
            Self::CloudflaredQuick | Self::CloudflaredNamed => Some(ClientHeader::CfConnectingIp),
            Self::Tunnl => None,
        }
    }
}

/// Whether a request came in over the active tunnel: it reached the service
/// from the loopback agent and is addressed to the tunnel's own host.
pub fn over_tunnel(edge: Option<&Edge>, peer: IpAddr, host: Option<&str>) -> bool {
    match (edge, host) {
        (Some(edge), Some(host)) => peer.is_loopback() && same_host(host, &edge.host),
        _ => false,
    }
}

/// The address a rate limit should be keyed on.
///
/// Over the tunnel, every request's peer is `127.0.0.1` (the provider's agent),
/// so keying on it puts every visitor, the owner included, in one bucket. This
/// reads the provider's own client-address header instead, but only when the
/// request [`over_tunnel`]: a direct request (a loopback client, or any other
/// peer) keeps its socket address, so a header a local or LAN client writes
/// moves nothing. Whatever is missing or unparsable falls back to the peer,
/// which is the shared bucket: failing closed, never open.
///
/// `forwarded_for` is the whole `X-Forwarded-For` value and `cf_connecting_ip`
/// the `CF-Connecting-IP` value, either of which may be absent.
pub fn client_address(
    edge: Option<&Edge>,
    peer: IpAddr,
    host: Option<&str>,
    forwarded_for: Option<&str>,
    cf_connecting_ip: Option<&str>,
) -> IpAddr {
    if !over_tunnel(edge, peer, host) {
        return peer;
    }
    let Some(edge) = edge else { return peer };
    let found = match edge.provider.client_header() {
        Some(ClientHeader::ForwardedFor) => forwarded_for.and_then(rightmost_address),
        Some(ClientHeader::CfConnectingIp) => cf_connecting_ip.and_then(parse_address),
        None => None,
    };
    found.unwrap_or(peer)
}

/// The last address in an `X-Forwarded-For` list.
fn rightmost_address(value: &str) -> Option<IpAddr> {
    value.rsplit(',').next().and_then(parse_address)
}

/// One address, with or without a port or brackets; nothing else.
fn parse_address(text: &str) -> Option<IpAddr> {
    let text = text.trim();
    text.parse::<IpAddr>()
        .ok()
        .or_else(|| text.parse::<SocketAddr>().ok().map(|socket| socket.ip()))
        .or_else(|| {
            text.strip_prefix('[')
                .and_then(|rest| rest.strip_suffix(']'))
                .and_then(|inner| inner.parse::<IpAddr>().ok())
        })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

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

    const TUNNEL_HOST: &str = "abc.ngrok-free.app";

    fn at(provider: Provider) -> Edge {
        Edge {
            provider,
            host: TUNNEL_HOST.to_owned(),
        }
    }

    fn loopback() -> IpAddr {
        "127.0.0.1".parse().unwrap()
    }

    #[test]
    fn a_tunnel_request_is_keyed_on_the_rightmost_forwarded_address() {
        let ngrok = at(Provider::Ngrok);
        let got = client_address(
            Some(&ngrok),
            loopback(),
            Some(TUNNEL_HOST),
            // A visitor's own claim on the left, the edge's entry on the right.
            Some("6.6.6.6, 203.0.113.9"),
            None,
        );
        assert_eq!(got, "203.0.113.9".parse::<IpAddr>().unwrap());
        let v6 = client_address(
            Some(&ngrok),
            loopback(),
            Some(TUNNEL_HOST),
            Some("2001:db8::7"),
            None,
        );
        assert_eq!(v6, "2001:db8::7".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn cloudflared_is_keyed_on_cf_connecting_ip_not_forwarded_for() {
        let cf = Edge {
            provider: Provider::CloudflaredQuick,
            host: TUNNEL_HOST.to_owned(),
        };
        let got = client_address(
            Some(&cf),
            loopback(),
            Some(TUNNEL_HOST),
            Some("6.6.6.6"),
            Some("198.51.100.4"),
        );
        assert_eq!(got, "198.51.100.4".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn a_direct_request_ignores_a_spoofed_header() {
        let ngrok = at(Provider::Ngrok);
        // Loopback client addressed to a loopback name.
        assert_eq!(
            client_address(
                Some(&ngrok),
                loopback(),
                Some("127.0.0.1:8080"),
                Some("6.6.6.6"),
                Some("6.6.6.6")
            ),
            loopback()
        );
        // No tunnel up at all.
        assert_eq!(
            client_address(None, loopback(), Some(TUNNEL_HOST), Some("6.6.6.6"), None),
            loopback()
        );
        // A non-loopback peer addressing the tunnel's host directly.
        let lan: IpAddr = "192.168.1.20".parse().unwrap();
        assert_eq!(
            client_address(Some(&ngrok), lan, Some(TUNNEL_HOST), Some("6.6.6.6"), None),
            lan
        );
    }

    #[test]
    fn an_unusable_or_untrusted_header_falls_back_to_the_shared_peer() {
        let ngrok = at(Provider::Ngrok);
        for bad in ["", "not an address", "1.2.3.4, nope", "1.2.3.4,"] {
            assert_eq!(
                client_address(Some(&ngrok), loopback(), Some(TUNNEL_HOST), Some(bad), None),
                loopback(),
                "{bad:?}"
            );
        }
        // A provider whose edge is not documented is not believed.
        let tunnl = at(Provider::Tunnl);
        assert_eq!(
            client_address(
                Some(&tunnl),
                loopback(),
                Some(TUNNEL_HOST),
                Some("6.6.6.6"),
                None
            ),
            loopback()
        );
    }
}
