//! The providers that are one child process: cloudflared (quick and named),
//! ngrok and tunnl.gg.
//!
//! They differ only in what they run and how the address is found, so one type
//! holds all four: [`Plan`] says the program, its arguments and the
//! [`Discovery`], and [`ChildTunnel`] runs it, waits for the address, and kills
//! the child on stop or drop.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use super::parse::{self, redact, strip_ansi};
use super::process::{self, Running};
use super::{BoxFuture, Provider, PublicEndpoint, Tunnel, TunnelError, TunnelState};

/// How long a provider has to give an address.
pub(crate) const READY_TIMEOUT: Duration = Duration::from_secs(45);

/// How the address of a child's tunnel is found.
#[derive(Debug, Clone)]
pub(crate) enum Discovery {
    /// A line of the child's output carries it; the function finds it.
    Line(fn(&str) -> Option<String>),
    /// The address is known up front (a named tunnel's hostname); a line says
    /// the child is connected.
    Ready {
        /// The address, already built from the person's settings.
        url: String,
        /// Whether a line means the connection is up.
        ready: fn(&str) -> bool,
    },
    /// The ngrok agent's local API names it.
    NgrokApi {
        /// The API's base, e.g. `http://127.0.0.1:4040`.
        base: String,
    },
}

/// What to run for one provider.
#[derive(Debug, Clone)]
pub(crate) struct Plan {
    /// Which provider this is.
    pub provider: Provider,
    /// The program, resolved from the person's `PATH`.
    pub program: PathBuf,
    /// Arguments that print the program's version.
    pub version_args: &'static [&'static str],
    /// The arguments to start a tunnel, given the loopback port.
    pub args: fn(&Plan, u16) -> Vec<String>,
    /// How the address is found.
    pub discovery: Discovery,
    /// Whether tunnl's `stable@` form was asked for.
    pub stable: bool,
    /// The named tunnel's name, for cloudflared-named.
    pub name: String,
    /// How long to wait for an address.
    pub timeout: Duration,
}

/// A tunnel that is a child process.
pub(crate) struct ChildTunnel {
    plan: Plan,
    child: Option<tokio::process::Child>,
    state: TunnelState,
}

impl ChildTunnel {
    /// A tunnel that will run `plan`.
    pub(crate) fn new(plan: Plan) -> Self {
        Self {
            plan,
            child: None,
            state: TunnelState::Down,
        }
    }

    /// Waits for the address, reading the child's lines.
    async fn discover(
        &self,
        running: &mut Running,
        local: SocketAddr,
    ) -> Result<String, TunnelError> {
        let deadline = tokio::time::Instant::now() + self.plan.timeout;
        let mut last = String::new();
        // The API is polled between lines, so a quiet ngrok is still asked.
        let mut tick = tokio::time::interval(Duration::from_millis(500));
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => {
                    return Err(TunnelError::Failed(format!(
                        "{} gave no address within {}s{}",
                        self.plan.provider.word(),
                        self.plan.timeout.as_secs(),
                        tail(&last),
                    )));
                }
                line = running.lines.recv() => {
                    let Some(line) = line else {
                        // Both streams ended: the program is gone.
                        let code = running.child.try_wait().ok().flatten();
                        return Err(TunnelError::Failed(format!(
                            "{} exited before it gave an address{}{}",
                            self.plan.provider.word(),
                            code.map(|status| format!(" ({status})")).unwrap_or_default(),
                            tail(&last),
                        )));
                    };
                    match &self.plan.discovery {
                        Discovery::Line(find) => {
                            if let Some(url) = find(&line) {
                                return Ok(url);
                            }
                        }
                        Discovery::Ready { url, ready } => {
                            if ready(&line) {
                                return Ok(url.clone());
                            }
                        }
                        Discovery::NgrokApi { .. } => {}
                    }
                    let clean = strip_ansi(&line);
                    if !clean.trim().is_empty() {
                        last = clean.trim().to_owned();
                    }
                }
                _ = tick.tick() => {
                    if let Discovery::NgrokApi { base } = &self.plan.discovery
                        && let Some(url) = ngrok_lookup(base, local.port()).await
                    {
                        return Ok(url);
                    }
                    if let Ok(Some(status)) = running.child.try_wait() {
                        return Err(TunnelError::Failed(format!(
                            "{} exited before it gave an address ({status}){}",
                            self.plan.provider.word(),
                            tail(&last),
                        )));
                    }
                }
            }
        }
    }
}

/// The last thing a child said, as a suffix for an error, with addresses masked.
fn tail(last: &str) -> String {
    if last.is_empty() {
        String::new()
    } else {
        let said = redact(last);
        let said: String = said.chars().take(160).collect();
        format!(": {said}")
    }
}

/// Asks the ngrok agent for the tunnel to `port`, off the async threads.
async fn ngrok_lookup(base: &str, port: u16) -> Option<String> {
    let url = format!("{}/api/tunnels", base.trim_end_matches('/'));
    tokio::task::spawn_blocking(move || {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(2)))
            .build()
            .into();
        let mut response = agent.get(&url).call().ok()?;
        let body = response.body_mut().read_to_string().ok()?;
        parse::ngrok(&body, port)
    })
    .await
    .ok()
    .flatten()
}

impl Tunnel for ChildTunnel {
    fn provider(&self) -> Provider {
        self.plan.provider
    }

    fn version(&self) -> BoxFuture<'_, Option<String>> {
        Box::pin(
            async move { process::version_line(&self.plan.program, self.plan.version_args).await },
        )
    }

    fn start(&mut self, local: SocketAddr) -> BoxFuture<'_, Result<PublicEndpoint, TunnelError>> {
        Box::pin(async move {
            if self.child.is_some() {
                return Err(TunnelError::Busy("running"));
            }
            self.state = TunnelState::Starting;
            let args = (self.plan.args)(&self.plan, local.port());
            let mut running = process::spawn(&self.plan.program, &args).map_err(|err| {
                self.state = TunnelState::Failed {
                    why: err.to_string(),
                };
                TunnelError::Failed(format!(
                    "could not run {}: {err}",
                    self.plan.provider.program()
                ))
            })?;
            let found = self.discover(&mut running, local).await;
            let url = match found {
                Ok(url) => url,
                Err(err) => {
                    process::kill(&mut running.child).await;
                    self.state = TunnelState::Failed {
                        why: err.to_string(),
                    };
                    return Err(err);
                }
            };
            let Some(endpoint) = PublicEndpoint::new(&url, self.plan.provider) else {
                process::kill(&mut running.child).await;
                let err = TunnelError::Failed(format!(
                    "{} gave an address that is not a plain https name",
                    self.plan.provider.word()
                ));
                self.state = TunnelState::Failed {
                    why: err.to_string(),
                };
                return Err(err);
            };
            process::drain(running.lines);
            self.child = Some(running.child);
            self.state = TunnelState::Up(endpoint.clone());
            Ok(endpoint)
        })
    }

    fn status(&mut self) -> BoxFuture<'_, TunnelState> {
        Box::pin(async move {
            if let Some(child) = self.child.as_mut()
                && let Ok(Some(status)) = child.try_wait()
            {
                self.child = None;
                self.state = TunnelState::Failed {
                    why: format!("the {} process ended ({status})", self.plan.provider.word()),
                };
            }
            self.state.clone()
        })
    }

    fn stop(&mut self) -> BoxFuture<'_, Result<(), TunnelError>> {
        Box::pin(async move {
            if let Some(mut child) = self.child.take() {
                process::kill(&mut child).await;
            }
            self.state = TunnelState::Down;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn local() -> SocketAddr {
        "127.0.0.1:18080".parse().unwrap()
    }

    fn cloudflared_plan(program: PathBuf, timeout: Duration) -> Plan {
        Plan {
            provider: Provider::CloudflaredQuick,
            program,
            version_args: &["--version"],
            args: |_, port| vec![format!("http://127.0.0.1:{port}")],
            discovery: Discovery::Line(parse::cloudflared_quick),
            stable: false,
            name: String::new(),
            timeout,
        }
    }

    /// Whether a process with this pid is alive, asked of the kernel.
    fn alive(pid: u32) -> bool {
        std::process::Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    #[tokio::test]
    async fn a_fake_program_gives_an_address_and_stop_ends_it() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("pid");
        let fake = script(
            dir.path(),
            "cloudflared",
            &format!(
                "echo $$ > {}\n\
                 echo 'INF noise https://api.trycloudflare.com/tunnel' >&2\n\
                 echo 'INF |  https://fake-name-here.trycloudflare.com  |' >&2\n\
                 exec sleep 60",
                pidfile.display()
            ),
        );
        let mut tunnel = ChildTunnel::new(cloudflared_plan(fake, Duration::from_secs(10)));
        let endpoint = tunnel.start(local()).await.unwrap();
        assert_eq!(endpoint.url(), "https://fake-name-here.trycloudflare.com");
        assert_eq!(endpoint.host, "fake-name-here.trycloudflare.com");
        assert!(endpoint.public_internet);
        assert!(matches!(tunnel.status().await, TunnelState::Up(_)));

        let pid: u32 = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(alive(pid), "the child is running");
        tunnel.stop().await.unwrap();
        assert_eq!(tunnel.status().await, TunnelState::Down);
        assert!(!alive(pid), "stop left no child behind");
        // A second stop is not an error.
        tunnel.stop().await.unwrap();
    }

    #[tokio::test]
    async fn dropping_the_tunnel_kills_the_child() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("pid");
        let fake = script(
            dir.path(),
            "cloudflared",
            &format!(
                "echo $$ > {}\necho 'https://drop-me-now.trycloudflare.com' >&2\nexec sleep 60",
                pidfile.display()
            ),
        );
        let mut tunnel = ChildTunnel::new(cloudflared_plan(fake, Duration::from_secs(10)));
        tunnel.start(local()).await.unwrap();
        let pid: u32 = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        drop(tunnel);
        for _ in 0..50 {
            if !alive(pid) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("the child outlived its tunnel");
    }

    #[tokio::test]
    async fn a_program_that_dies_early_is_a_redacted_failure() {
        let dir = tempfile::tempdir().unwrap();
        let fake = script(
            dir.path(),
            "cloudflared",
            "echo 'ERR cannot reach https://secret-words-here.example.org/x' >&2\nexit 3",
        );
        let mut tunnel = ChildTunnel::new(cloudflared_plan(fake, Duration::from_secs(10)));
        let err = tunnel.start(local()).await.unwrap_err().to_string();
        assert!(err.contains("exited before it gave an address"), "{err}");
        assert!(!err.contains("secret-words"), "{err}");
        assert!(matches!(tunnel.status().await, TunnelState::Failed { .. }));
    }

    #[tokio::test]
    async fn a_silent_program_times_out_and_is_killed() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("pid");
        let fake = script(
            dir.path(),
            "cloudflared",
            &format!("echo $$ > {}\nexec sleep 60", pidfile.display()),
        );
        let mut tunnel = ChildTunnel::new(cloudflared_plan(fake, Duration::from_millis(2500)));
        let err = tunnel.start(local()).await.unwrap_err().to_string();
        assert!(err.contains("no address within"), "{err}");
        let pid: u32 = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(!alive(pid));
    }

    #[tokio::test]
    async fn a_child_that_exits_later_is_reported_failed() {
        let dir = tempfile::tempdir().unwrap();
        let fake = script(
            dir.path(),
            "cloudflared",
            "echo 'https://short-lived-one.trycloudflare.com' >&2\nsleep 1",
        );
        let mut tunnel = ChildTunnel::new(cloudflared_plan(fake, Duration::from_secs(10)));
        tunnel.start(local()).await.unwrap();
        for _ in 0..50 {
            if matches!(tunnel.status().await, TunnelState::Failed { .. }) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("the exit was not noticed");
    }

    #[tokio::test]
    async fn named_tunnel_is_up_when_the_edge_connection_registers() {
        let dir = tempfile::tempdir().unwrap();
        let fake = script(
            dir.path(),
            "cloudflared",
            "echo 'INF starting' >&2\necho 'INF Registered tunnel connection connIndex=0' >&2\nexec sleep 60",
        );
        let mut plan = cloudflared_plan(fake, Duration::from_secs(10));
        plan.provider = Provider::CloudflaredNamed;
        plan.discovery = Discovery::Ready {
            url: "https://shprd.example.org".to_owned(),
            ready: parse::cloudflared_registered,
        };
        let mut tunnel = ChildTunnel::new(plan);
        let endpoint = tunnel.start(local()).await.unwrap();
        assert_eq!(endpoint.host, "shprd.example.org");
        tunnel.stop().await.unwrap();
    }

    #[tokio::test]
    async fn ngrok_address_comes_from_the_local_api() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let body = include_str!("../../tests/fixtures/ngrok-api.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut buffer = [0u8; 1024];
                let _ = socket.read(&mut buffer).await;
                let reply = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(reply.as_bytes()).await;
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let fake = script(dir.path(), "ngrok", "echo started\nexec sleep 60");
        let mut plan = cloudflared_plan(fake, Duration::from_secs(10));
        plan.provider = Provider::Ngrok;
        plan.discovery = Discovery::NgrokApi { base };
        let mut tunnel = ChildTunnel::new(plan);
        // The fixture's tunnel forwards to 18765.
        let endpoint = tunnel
            .start("127.0.0.1:18765".parse().unwrap())
            .await
            .unwrap();
        assert_eq!(endpoint.host, "sample-name-here.ngrok-free.dev");
        tunnel.stop().await.unwrap();
    }
}
