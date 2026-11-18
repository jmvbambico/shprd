//! Building a tunnel for a provider, from the person's own programs.
//!
//! The factory is the only place that looks for a program, and it looks on the
//! `PATH` string it was constructed with: this crate reads no environment
//! variable, and the binary hands in the `PATH` it started with (the same value
//! the service already carries for the roster). Nothing is downloaded and no
//! program is installed (M7d §4.3, threat row 17); the version of the one that
//! will run is printed when a tunnel starts.

use super::child::{ChildTunnel, Discovery, Plan, READY_TIMEOUT};
use super::parse;
use super::process::find_program;
use super::tailscale::TailscaleTunnel;
use super::{Provider, Tunnel, TunnelError};

/// The settings a tunnel is built from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Options {
    /// tunnl.gg only: ask for the stable address (`stable@`), tied to the
    /// person's SSH key, instead of a new random one.
    pub stable: bool,
    /// cloudflared-named only: the tunnel's name (`cloudflared tunnel create`).
    pub cloudflared_name: String,
    /// cloudflared-named only: the hostname routed to it
    /// (`cloudflared tunnel route dns`).
    pub cloudflared_host: String,
}

/// Something that can make a tunnel. The service uses [`SystemTunnels`]; a test
/// hands in a fake.
pub trait TunnelFactory: Send + Sync {
    /// A tunnel for `provider`, or why one cannot be made.
    fn create(&self, provider: Provider, options: &Options)
    -> Result<Box<dyn Tunnel>, TunnelError>;
}

/// The real providers, run from the `PATH` this was given.
#[derive(Debug, Clone)]
pub struct SystemTunnels {
    path: String,
    ngrok_api: String,
}

impl SystemTunnels {
    /// Providers whose programs are looked up in `path`, a `PATH`-style string.
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            ngrok_api: "http://127.0.0.1:4040".to_owned(),
        }
    }

    /// The same, asking an ngrok agent at `base` instead of `127.0.0.1:4040`:
    /// for a test with a stand-in agent.
    pub fn with_ngrok_api(mut self, base: impl Into<String>) -> Self {
        self.ngrok_api = base.into();
        self
    }
}

impl TunnelFactory for SystemTunnels {
    fn create(
        &self,
        provider: Provider,
        options: &Options,
    ) -> Result<Box<dyn Tunnel>, TunnelError> {
        let program =
            find_program(provider.program(), &self.path).ok_or(TunnelError::NotInstalled {
                program: provider.program(),
            })?;
        let plan = |args: fn(&Plan, u16) -> Vec<String>,
                    version_args: &'static [&'static str],
                    discovery: Discovery| Plan {
            provider,
            program: program.clone(),
            version_args,
            args,
            discovery,
            stable: options.stable,
            name: options.cloudflared_name.clone(),
            timeout: READY_TIMEOUT,
        };
        Ok(match provider {
            Provider::Tailscale => Box::new(TailscaleTunnel::new(program.clone())),
            Provider::CloudflaredQuick => Box::new(ChildTunnel::new(plan(
                |_, port| {
                    vec![
                        "tunnel".into(),
                        "--no-autoupdate".into(),
                        "--url".into(),
                        format!("http://127.0.0.1:{port}"),
                    ]
                },
                &["--version"],
                Discovery::Line(parse::cloudflared_quick),
            ))),
            Provider::CloudflaredNamed => {
                let name = valid_name(&options.cloudflared_name)?;
                let host = valid_host(&options.cloudflared_host)?;
                let mut plan = plan(
                    |plan, port| {
                        vec![
                            "tunnel".into(),
                            "--no-autoupdate".into(),
                            "run".into(),
                            "--url".into(),
                            format!("http://127.0.0.1:{port}"),
                            plan.name.clone(),
                        ]
                    },
                    &["--version"],
                    Discovery::Ready {
                        url: format!("https://{host}"),
                        ready: parse::cloudflared_registered,
                    },
                );
                plan.name = name;
                Box::new(ChildTunnel::new(plan))
            }
            Provider::Ngrok => Box::new(ChildTunnel::new(plan(
                |_, port| {
                    vec![
                        "http".into(),
                        format!("127.0.0.1:{port}"),
                        "--log=stdout".into(),
                    ]
                },
                &["version"],
                Discovery::NgrokApi {
                    base: self.ngrok_api.clone(),
                },
            ))),
            Provider::Tunnl => Box::new(ChildTunnel::new(plan(
                |plan, port| {
                    let destination = if plan.stable {
                        "stable@proxy.tunnl.gg"
                    } else {
                        "proxy.tunnl.gg"
                    };
                    // `-tt`: the service wants a terminal and there is none, so
                    // it is forced. `BatchMode` so a missing key is a failure
                    // and not a prompt nobody can answer; the first connection
                    // records the host key rather than asking.
                    vec![
                        "-tt".into(),
                        "-o".into(),
                        "BatchMode=yes".into(),
                        "-o".into(),
                        "StrictHostKeyChecking=accept-new".into(),
                        "-o".into(),
                        "ServerAliveInterval=30".into(),
                        "-o".into(),
                        "ExitOnForwardFailure=yes".into(),
                        "-R".into(),
                        format!("80:127.0.0.1:{port}"),
                        destination.into(),
                        "output=json".into(),
                    ]
                },
                &["-V"],
                Discovery::Line(parse::tunnl),
            ))),
        })
    }
}

/// A tunnel name `cloudflared` will take: letters, digits, `.`, `_` and `-`, not
/// starting with `-` (it would be read as a flag).
fn valid_name(name: &str) -> Result<String, TunnelError> {
    let name = name.trim();
    let ok = !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('-')
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'));
    if ok {
        Ok(name.to_owned())
    } else {
        Err(TunnelError::Config(
            "cloudflared-named needs the tunnel's name: `shprd settings set \
             web.cloudflared_name <name>`"
                .to_owned(),
        ))
    }
}

/// A hostname the named tunnel is routed to: a plain DNS name with a dot, and no
/// port, path or credentials.
fn valid_host(host: &str) -> Result<String, TunnelError> {
    let host = host.trim().to_ascii_lowercase();
    let ok = host.contains('.')
        && !host.contains(':')
        && parse::authority_of(&format!("https://{host}")).as_deref() == Some(host.as_str());
    if ok {
        Ok(host)
    } else {
        Err(TunnelError::Config(
            "cloudflared-named needs the hostname routed to the tunnel: `shprd settings set \
             web.cloudflared_host <host>`"
                .to_owned(),
        ))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn bin_dir(names: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for name in names {
            let path = dir.path().join(name);
            std::fs::write(&path, "#!/bin/sh\ntrue\n").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        dir
    }

    fn make(
        path: &str,
        provider: Provider,
        options: &Options,
    ) -> Result<Box<dyn Tunnel>, TunnelError> {
        SystemTunnels::new(path).create(provider, options)
    }

    #[test]
    fn a_missing_program_is_not_installed_and_nothing_is_fetched() {
        let empty = bin_dir(&[]);
        for provider in crate::tunnel::PROVIDERS {
            let err = make(
                &empty.path().display().to_string(),
                provider,
                &Options {
                    cloudflared_name: "n".into(),
                    cloudflared_host: "h.example.org".into(),
                    ..Options::default()
                },
            )
            .err()
            .unwrap();
            assert!(matches!(err, TunnelError::NotInstalled { .. }), "{err}");
            assert!(err.to_string().contains("never downloads"));
        }
    }

    #[test]
    fn each_provider_is_built_from_its_own_program() {
        let dir = bin_dir(&["tailscale", "cloudflared", "ngrok", "ssh"]);
        let path = dir.path().display().to_string();
        let named = Options {
            cloudflared_name: "shprd".into(),
            cloudflared_host: "shprd.example.org".into(),
            ..Options::default()
        };
        for provider in crate::tunnel::PROVIDERS {
            let tunnel = make(&path, provider, &named).unwrap();
            assert_eq!(tunnel.provider(), provider);
            assert_eq!(tunnel.public_internet(), provider != Provider::Tailscale);
        }
    }

    #[test]
    fn a_named_tunnel_needs_a_name_and_a_plain_host() {
        let dir = bin_dir(&["cloudflared"]);
        let path = dir.path().display().to_string();
        let build = |name: &str, host: &str| {
            make(
                &path,
                Provider::CloudflaredNamed,
                &Options {
                    cloudflared_name: name.into(),
                    cloudflared_host: host.into(),
                    ..Options::default()
                },
            )
            .err()
            .map(|err| err.to_string())
        };
        assert_eq!(build("shprd", "shprd.example.org"), None);
        for (name, host) in [
            ("", "shprd.example.org"),
            ("-flag", "shprd.example.org"),
            ("has space", "shprd.example.org"),
            ("shprd", ""),
            ("shprd", "nodot"),
            ("shprd", "host.example.org:8080"),
            ("shprd", "https://host.example.org"),
            ("shprd", "user@host.example.org"),
        ] {
            let said = build(name, host).unwrap_or_default();
            assert!(
                said.contains("web.cloudflared_"),
                "{name:?} {host:?}: {said}"
            );
        }
    }
}
