//! Tunnels: how the loopback listener is reached from somewhere else.
//!
//! A [`Tunnel`] is one provider's way of putting `127.0.0.1:<port>` at a public
//! address: Tailscale serve (a private tailnet name), a Cloudflare quick or
//! named tunnel, ngrok, or tunnl.gg over `ssh -R`. They differ in everything
//! that matters for safety — who can reach the address, whether it is on the
//! public internet, what the address looks like — and agree on one rule, which
//! the [`TunnelManager`] enforces for all of them: **a provider on the public
//! internet does not start unless a login password is set.** There is no flag
//! around it (M7d §4.3 rule 3, threat row 1).
//!
//! The researched design is M7d §4.3 (`_research/m7/M7d-web-remote.md`). Its
//! rules, and where they land here:
//!
//! * the person's own binaries from `PATH`, `--version` printed, nothing
//!   downloaded (threat row 17): [`SystemTunnels`] and [`TunnelState`]'s
//!   version;
//! * the `Host` allowlist holds the current endpoint and no other tunnel's:
//!   [`TunnelManager`] adds it on start and removes it on stop;
//! * one tunnel at a time, stopped on `web down`, on exit and after a storm of
//!   failed sign-ins ([`crate::limits::StormSignal`]);
//! * forwarded headers are trusted only where a provider's own agent sets them
//!   ([`edge`]): everything else, `X-Forwarded-For` included, is just text;
//! * the address is a credential and is never logged in full ([`parse::mask_url`]).

mod child;
mod edge;
mod manager;
mod parse;
mod process;
mod providers;
mod state;
mod tailscale;

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;

pub use edge::{Edge, trusted_login};
pub use manager::{StartRequest, TunnelManager};
pub use parse::{authority_of, mask_url, name_of, redact, strip_ansi};
pub use providers::{Options, SystemTunnels, TunnelFactory};
pub use state::{WebFile, qr_svg, qr_unicode};

/// A boxed future, so a [`Tunnel`] can be used as a trait object.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Which provider a tunnel is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Provider {
    /// `tailscale serve`: a private name on the tailnet. Never Funnel.
    Tailscale,
    /// `cloudflared tunnel --url`: a random `trycloudflare.com` name, no account.
    CloudflaredQuick,
    /// `cloudflared tunnel run <name>`: the person's own hostname.
    CloudflaredNamed,
    /// `ngrok http <port>`.
    Ngrok,
    /// `ssh -R 80:… proxy.tunnl.gg`.
    Tunnl,
}

/// The providers, in the order a settings page lists them.
pub const PROVIDERS: [Provider; 5] = [
    Provider::Tailscale,
    Provider::CloudflaredQuick,
    Provider::CloudflaredNamed,
    Provider::Ngrok,
    Provider::Tunnl,
];

impl Provider {
    /// The word `--tunnel` and the `web.tunnel` setting use.
    pub fn word(self) -> &'static str {
        match self {
            Self::Tailscale => "tailscale",
            Self::CloudflaredQuick => "cloudflared",
            Self::CloudflaredNamed => "cloudflared-named",
            Self::Ngrok => "ngrok",
            Self::Tunnl => "tunnl",
        }
    }

    /// The provider a word names.
    pub fn parse(word: &str) -> Option<Provider> {
        PROVIDERS
            .into_iter()
            .find(|provider| provider.word() == word.trim())
    }

    /// Whether the address is reachable from the whole internet.
    ///
    /// It is a gate input, not a warning: every provider but Tailscale serve is
    /// public, and a public one needs a password to start.
    pub fn public_internet(self) -> bool {
        !matches!(self, Self::Tailscale)
    }

    /// Who is believed about the headers a request arrives with.
    pub fn trust(self) -> Trust {
        match self {
            Self::Tailscale => Trust::Tailnet,
            _ => Trust::Edge,
        }
    }

    /// The program this provider runs.
    pub fn program(self) -> &'static str {
        match self {
            Self::Tailscale => "tailscale",
            Self::CloudflaredQuick | Self::CloudflaredNamed => "cloudflared",
            Self::Ngrok => "ngrok",
            Self::Tunnl => "ssh",
        }
    }
}

/// What a provider's headers are believed for.
///
/// Every request reaches the service from `127.0.0.1`, so the peer is the
/// provider's agent whichever one it is, and the headers it adds cannot be told
/// from a visitor's own. Only Tailscale's are used, and only for who is asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trust {
    /// `tailscale serve` sets `Tailscale-User-Login` from the tailnet's own
    /// identity, and drops one a visitor sends.
    Tailnet,
    /// Nothing a tunnel's edge adds is believed: `X-Forwarded-For`,
    /// `Cf-Access-*` and the like are text any visitor can send.
    Edge,
}

/// Where a running tunnel can be reached.
///
/// The address is a credential for a quick tunnel, so `Debug` prints it masked
/// and the field is read through [`PublicEndpoint::url`], which a log line has
/// no reason to call.
#[derive(Clone, PartialEq, Eq)]
pub struct PublicEndpoint {
    url: String,
    /// The `Host` the address is reached under, `host[:port]`: what the
    /// allowlist is given.
    pub host: String,
    /// Whether the address is on the public internet.
    pub public_internet: bool,
    /// How its headers are treated.
    pub trust: Trust,
}

impl PublicEndpoint {
    /// An endpoint at `url`, whose host is the URL's own authority.
    ///
    /// `None` when the URL is not a plain `https://` name: an address that
    /// cannot be an allowlist entry cannot be a tunnel.
    pub fn new(url: &str, provider: Provider) -> Option<Self> {
        let host = authority_of(url)?;
        Some(Self {
            url: url.to_owned(),
            host,
            public_internet: provider.public_internet(),
            trust: provider.trust(),
        })
    }

    /// The full address. A credential: never log it.
    pub fn url(&self) -> &str {
        &self.url
    }
}

impl std::fmt::Debug for PublicEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PublicEndpoint")
            .field("url", &mask_url(&self.url))
            .field("public_internet", &self.public_internet)
            .field("trust", &self.trust)
            .finish_non_exhaustive()
    }
}

/// Where a tunnel is in its life.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelState {
    /// Not running.
    Down,
    /// Started, and the address is not known yet.
    Starting,
    /// Running at an address.
    Up(PublicEndpoint),
    /// It stopped by itself or never came up.
    Failed {
        /// Why, in words that carry no address.
        why: String,
    },
}

/// Why a tunnel did not start, or could not be stopped.
#[derive(Debug, thiserror::Error)]
pub enum TunnelError {
    /// The provider's program is not on `PATH`. Nothing is downloaded.
    #[error(
        "{program} is not installed (not found on PATH); shprd never downloads a tunnel program"
    )]
    NotInstalled {
        /// The program looked for.
        program: &'static str,
    },
    /// A public tunnel was asked for with no password to stand in front of it.
    #[error("{0}")]
    NeedsPassword(String),
    /// A tunnel is already up, or starting.
    #[error("a tunnel is already {0}; stop it first (one tunnel at a time)")]
    Busy(&'static str),
    /// The provider's settings are missing or malformed.
    #[error("{0}")]
    Config(String),
    /// The provider would put the service somewhere it must not be.
    #[error("{0}")]
    Refused(String),
    /// The provider's program ran and did not give an address in time, or died.
    #[error("{0}")]
    Failed(String),
}

/// One provider's way of reaching the service.
///
/// A tunnel is stateful — it owns the child process or the Tailscale mapping it
/// started — and one value is one tunnel. The methods box their futures so a
/// manager can hold any provider (or a test's fake) as `Box<dyn Tunnel>`.
pub trait Tunnel: Send {
    /// Which provider this is.
    fn provider(&self) -> Provider;

    /// Whether the address is on the public internet. A manager refuses to start
    /// a tunnel that says `true` unless a password is set.
    fn public_internet(&self) -> bool {
        self.provider().public_internet()
    }

    /// How the provider's headers are treated.
    fn trust(&self) -> Trust {
        self.provider().trust()
    }

    /// The first line of the provider program's `--version`, for the banner.
    fn version(&self) -> BoxFuture<'_, Option<String>>;

    /// Starts the tunnel to `local` and answers where it can be reached.
    fn start(&mut self, local: SocketAddr) -> BoxFuture<'_, Result<PublicEndpoint, TunnelError>>;

    /// Where the tunnel is now. A child that has exited is `Failed`.
    fn status(&mut self) -> BoxFuture<'_, TunnelState>;

    /// Stops the tunnel and releases what it held. Stopping twice is not an
    /// error.
    fn stop(&mut self) -> BoxFuture<'_, Result<(), TunnelError>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_settings_words_are_the_providers_words() {
        let words: Vec<&str> = PROVIDERS.iter().map(|provider| provider.word()).collect();
        assert_eq!(words, shprd_core::settings::WEB_TUNNEL_WORDS);
        for provider in PROVIDERS {
            assert_eq!(Provider::parse(provider.word()), Some(provider));
        }
        assert_eq!(Provider::parse("nope"), None);
    }

    #[test]
    fn only_tailscale_is_private() {
        for provider in PROVIDERS {
            assert_eq!(provider.public_internet(), provider != Provider::Tailscale);
        }
    }

    #[test]
    fn an_endpoint_prints_masked() {
        let endpoint =
            PublicEndpoint::new("https://quiet-green-river.trycloudflare.com", Provider::Tunnl)
                .expect("a plain name");
        let shown = format!("{endpoint:?}");
        assert!(!shown.contains("quiet-green"), "{shown}");
        assert!(PublicEndpoint::new("http://x.example.com", Provider::Tunnl).is_none());
    }
}
