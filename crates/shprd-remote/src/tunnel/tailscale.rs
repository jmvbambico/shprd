//! Tailscale serve: the tailnet's private name for the service.
//!
//! This is the one provider that is not a child process: `tailscale serve --bg`
//! hands the mapping to `tailscaled`, which keeps it after the command exits, so
//! "stop" is a second command (`serve … off`) and it has to run on the service's
//! way out too. It is also the one that is never public: Funnel, which would put
//! the same mapping on the internet, is refused.
//!
//! The behaviour is ported from Shepherdr's `service/tailscale.ts`
//! (eliasstravik/shepherdr, commit a3f327a, MIT; see `THIRD_PARTY_NOTICES.md`):
//! the Funnel refusal, never replacing a mount that points elsewhere, and a
//! taken HTTPS port falling to another instead of failing the start.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::parse::{self, redact};
use super::process::{self, SHORT};
use super::{BoxFuture, Provider, PublicEndpoint, Tunnel, TunnelError, TunnelState};

/// Where the web app is mounted on the tailnet name.
pub(crate) const MOUNT: &str = "/shprd";

/// How often a running share is looked at again.
const RECHECK: Duration = Duration::from_secs(15);

/// Tailscale serve, for one run of the service.
pub(crate) struct TailscaleTunnel {
    program: PathBuf,
    state: TunnelState,
    /// What this tunnel mapped, once it has: the tailnet name, the HTTPS port
    /// and the loopback target. Present exactly while a mapping is ours to undo.
    mapping: Option<Mapping>,
    checked: Option<Instant>,
}

/// One `serve` mapping this tunnel made.
struct Mapping {
    dns: String,
    port: u16,
    target: String,
}

impl TailscaleTunnel {
    /// A tunnel that runs `program` (the person's `tailscale`).
    pub(crate) fn new(program: PathBuf) -> Self {
        Self {
            program,
            state: TunnelState::Down,
            mapping: None,
            checked: None,
        }
    }

    /// `serve status --json`, or an empty object when it cannot be read.
    async fn serve_status(&self) -> String {
        let got = process::capture(&self.program, &["serve", "status", "--json"], SHORT).await;
        if got.ok && !got.out.trim().is_empty() {
            got.out
        } else {
            "{}".to_owned()
        }
    }

    /// Removes the mapping this tunnel made, if it is still ours.
    async fn unmap(&mut self) -> Result<(), TunnelError> {
        let Some(mapping) = self.mapping.take() else {
            return Ok(());
        };
        let status = self.serve_status().await;
        let served = parse::tailscale_served(&status, &mapping.dns, MOUNT);
        // Only undo a mapping that still leads here: another program's share of
        // the same mount is not this service's to remove.
        if served.proxy.as_deref() != Some(mapping.target.as_str()) {
            return Ok(());
        }
        let https = format!("--https={}", mapping.port);
        let got = process::capture(
            &self.program,
            &["serve", &https, "--set-path", MOUNT, "off"],
            SHORT,
        )
        .await;
        if got.ok {
            Ok(())
        } else {
            Err(TunnelError::Failed(format!(
                "tailscale serve off failed: {}",
                redact(got.err.trim())
            )))
        }
    }
}

impl Tunnel for TailscaleTunnel {
    fn provider(&self) -> Provider {
        Provider::Tailscale
    }

    fn version(&self) -> BoxFuture<'_, Option<String>> {
        Box::pin(async move { process::version_line(&self.program, &["version"]).await })
    }

    fn start(&mut self, local: SocketAddr) -> BoxFuture<'_, Result<PublicEndpoint, TunnelError>> {
        Box::pin(async move {
            if self.mapping.is_some() {
                return Err(TunnelError::Busy("running"));
            }
            self.state = TunnelState::Starting;
            let fail = |this: &mut Self, err: TunnelError| {
                this.state = TunnelState::Failed {
                    why: err.to_string(),
                };
                err
            };

            let status = process::capture(&self.program, &["status", "--json"], SHORT).await;
            let Some(me) = status
                .ok
                .then(|| parse::tailscale_me(&status.out))
                .flatten()
            else {
                let said = redact(status.err.trim());
                return Err(fail(
                    self,
                    TunnelError::Failed(format!(
                        "tailscale is not running or is not signed in{}",
                        if said.is_empty() {
                            String::new()
                        } else {
                            format!(": {said}")
                        }
                    )),
                ));
            };

            let serve = self.serve_status().await;
            let target = format!("http://127.0.0.1:{}", local.port());
            let served = parse::tailscale_served(&serve, &me.dns, MOUNT);
            if let Some(proxy) = served.proxy.as_deref()
                && proxy != target
            {
                return Err(fail(
                    self,
                    TunnelError::Refused(format!(
                        "{MOUNT} on this machine's tailnet name is already shared to {proxy}; \
                         shprd will not replace it"
                    )),
                ));
            }
            let port = match served.port.filter(|_| served.proxy.is_some()) {
                Some(port) => port,
                None => match parse::tailscale_pick_port(&serve) {
                    Some(port) => port,
                    None => {
                        return Err(fail(
                            self,
                            TunnelError::Failed(
                                "every HTTPS port tailscale serve can use (443, 8443, 10000) \
                                 is taken on this machine's tailnet name"
                                    .to_owned(),
                            ),
                        ));
                    }
                },
            };
            let host_port = format!("{}:{port}", me.dns);
            if served.funnel || funnel_on(&serve, &host_port) {
                return Err(fail(
                    self,
                    TunnelError::Refused(
                        "Funnel is on for this machine's HTTPS port: shprd will not serve \
                         there (Funnel is the public internet)"
                            .to_owned(),
                    ),
                ));
            }

            if served.proxy.is_none() {
                let https = format!("--https={port}");
                let got = process::capture(
                    &self.program,
                    &["serve", "--bg", &https, "--set-path", MOUNT, &target],
                    SHORT,
                )
                .await;
                if !got.ok {
                    let said = redact(got.err.trim());
                    return Err(fail(
                        self,
                        TunnelError::Failed(format!("tailscale serve failed: {said}")),
                    ));
                }
            }
            self.mapping = Some(Mapping {
                dns: me.dns.clone(),
                port,
                target,
            });

            // Funnel may have been on for this port all along and only visible
            // now that the mapping exists; check again and undo rather than
            // leave a public mapping.
            let after = self.serve_status().await;
            if funnel_on(&after, &host_port) {
                let _ = self.unmap().await;
                return Err(fail(
                    self,
                    TunnelError::Refused(
                        "Funnel is on for this machine's HTTPS port: shprd will not serve \
                         there (Funnel is the public internet)"
                            .to_owned(),
                    ),
                ));
            }

            let link = if port == 443 {
                format!("https://{}{MOUNT}/", me.dns)
            } else {
                format!("https://{}:{port}{MOUNT}/", me.dns)
            };
            let Some(endpoint) = PublicEndpoint::new(&link, Provider::Tailscale) else {
                let _ = self.unmap().await;
                return Err(fail(
                    self,
                    TunnelError::Failed("tailscale gave a name that is not a plain host".into()),
                ));
            };
            self.checked = Some(Instant::now());
            self.state = TunnelState::Up(endpoint.clone());
            Ok(endpoint)
        })
    }

    fn status(&mut self) -> BoxFuture<'_, TunnelState> {
        Box::pin(async move {
            let due = self.checked.is_none_or(|at| at.elapsed() >= RECHECK);
            if matches!(self.state, TunnelState::Up(_)) && due {
                self.checked = Some(Instant::now());
                if let Some(mapping) = &self.mapping {
                    let serve = self.serve_status().await;
                    let served = parse::tailscale_served(&serve, &mapping.dns, MOUNT);
                    let host_port = format!("{}:{}", mapping.dns, mapping.port);
                    if served.proxy.as_deref() != Some(mapping.target.as_str()) {
                        self.mapping = None;
                        self.state = TunnelState::Failed {
                            why: "the tailscale share is gone".to_owned(),
                        };
                    } else if funnel_on(&serve, &host_port) {
                        // Someone turned Funnel on under a running share.
                        let _ = self.unmap().await;
                        self.state = TunnelState::Failed {
                            why: "Funnel was turned on for the share; it was removed".to_owned(),
                        };
                    }
                }
            }
            self.state.clone()
        })
    }

    fn stop(&mut self) -> BoxFuture<'_, Result<(), TunnelError>> {
        Box::pin(async move {
            let result = self.unmap().await;
            self.state = TunnelState::Down;
            result
        })
    }
}

/// Whether `serve status` says Funnel is on for `host_port`.
fn funnel_on(serve_json: &str, host_port: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(serve_json)
        .ok()
        .and_then(|value| value.pointer(&format!("/AllowFunnel/{host_port}")).cloned())
        .and_then(|on| on.as_bool())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    const STATUS: &str = include_str!("../../tests/fixtures/tailscale-status.json");

    /// A `tailscale` that answers from files and logs what it was asked.
    ///
    /// `serve status --json` answers `serve.json`; `serve … off` and `serve
    /// --bg …` are appended to `calls`, and `--bg` rewrites `serve.json` to
    /// what a real daemon would then report.
    fn fake(dir: &Path, serve_after_bg: &str) -> PathBuf {
        std::fs::write(dir.join("status.json"), STATUS).unwrap();
        std::fs::write(dir.join("serve.json"), "{}").unwrap();
        std::fs::write(dir.join("after.json"), serve_after_bg).unwrap();
        let path = dir.join("tailscale");
        let body = format!(
            "#!/bin/sh\nD={d}\n\
             case \"$1 $2\" in\n\
             'status --json') cat $D/status.json;;\n\
             'serve status') cat $D/serve.json;;\n\
             'serve --bg') echo \"$@\" >> $D/calls; cp $D/after.json $D/serve.json;;\n\
             *) echo \"$@\" >> $D/calls; echo '{{}}' > $D/serve.json;;\n\
             esac\n",
            d = dir.display()
        );
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn after(port: u16, funnel: bool) -> String {
        format!(
            r#"{{"TCP":{{"443":{{"HTTPS":true}}}},"Web":{{"studio.tail1234.ts.net:443":{{"Handlers":{{"/shprd":{{"Proxy":"http://127.0.0.1:{port}"}}}}}}}},"AllowFunnel":{{"studio.tail1234.ts.net:443":{funnel}}}}}"#
        )
    }

    fn local() -> SocketAddr {
        "127.0.0.1:18080".parse().unwrap()
    }

    #[tokio::test]
    async fn serve_is_started_without_funnel_and_stopped_again() {
        let dir = tempfile::tempdir().unwrap();
        let program = fake(dir.path(), &after(18080, false));
        let mut tunnel = TailscaleTunnel::new(program);
        let endpoint = tunnel.start(local()).await.unwrap();
        assert_eq!(endpoint.url(), "https://studio.tail1234.ts.net/shprd/");
        assert_eq!(endpoint.host, "studio.tail1234.ts.net");
        assert!(!endpoint.public_internet);
        assert_eq!(endpoint.trust, crate::tunnel::Trust::Tailnet);
        let calls = std::fs::read_to_string(dir.path().join("calls")).unwrap();
        assert!(
            calls.contains("serve --bg --https=443 --set-path /shprd http://127.0.0.1:18080"),
            "{calls}"
        );

        tunnel.stop().await.unwrap();
        let calls = std::fs::read_to_string(dir.path().join("calls")).unwrap();
        assert!(
            calls.contains("serve --https=443 --set-path /shprd off"),
            "{calls}"
        );
        assert_eq!(tunnel.status().await, TunnelState::Down);
        // Stopping again runs nothing.
        tunnel.stop().await.unwrap();
        let again = std::fs::read_to_string(dir.path().join("calls")).unwrap();
        assert_eq!(again, calls);
    }

    #[tokio::test]
    async fn funnel_is_refused_and_the_mapping_is_undone() {
        let dir = tempfile::tempdir().unwrap();
        let program = fake(dir.path(), &after(18080, true));
        let mut tunnel = TailscaleTunnel::new(program);
        let err = tunnel.start(local()).await.unwrap_err();
        assert!(matches!(err, TunnelError::Refused(_)), "{err}");
        assert!(err.to_string().contains("Funnel"));
        let calls = std::fs::read_to_string(dir.path().join("calls")).unwrap();
        assert!(calls.contains("off"), "the mapping was undone: {calls}");
    }

    #[tokio::test]
    async fn a_mount_that_points_elsewhere_is_not_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let program = fake(dir.path(), &after(18080, false));
        std::fs::write(dir.path().join("serve.json"), after(9999, false)).unwrap();
        // The fake's `serve status` reads serve.json, which `fake` reset; put
        // the other program's mapping back.
        std::fs::write(dir.path().join("serve.json"), after(9999, false)).unwrap();
        let mut tunnel = TailscaleTunnel::new(program);
        let err = tunnel.start(local()).await.unwrap_err();
        assert!(matches!(err, TunnelError::Refused(_)), "{err}");
        assert!(!dir.path().join("calls").exists(), "nothing was changed");
    }

    #[tokio::test]
    async fn a_machine_without_tailscale_signed_in_is_a_failure() {
        let dir = tempfile::tempdir().unwrap();
        let program = fake(dir.path(), &after(18080, false));
        std::fs::write(
            dir.path().join("status.json"),
            r#"{"BackendState":"NeedsLogin"}"#,
        )
        .unwrap();
        let mut tunnel = TailscaleTunnel::new(program);
        let err = tunnel.start(local()).await.unwrap_err().to_string();
        assert!(err.contains("not running or is not signed in"), "{err}");
    }
}
