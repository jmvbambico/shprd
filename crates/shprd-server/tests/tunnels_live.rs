//! Live tunnel checks: a real provider, a real public address, one test each.
//!
//! **They are `#[ignore]`d and never run in CI.** Each starts the provider the
//! person has installed against a throwaway service on a free loopback port —
//! never the person's own service, state directory or settings — fetches the
//! public address, checks that the gate answers `401` to a request with no
//! session, and stops the tunnel in the same test, whatever happened in between.
//!
//! Run them by hand:
//!
//! ```text
//! cargo test -p shprd-server --test tunnels_live -- --ignored --test-threads=1
//! ```
//!
//! A provider that is not installed here, or that needs an account this machine
//! does not have, prints why and passes: a skipped provider proves nothing, and
//! the hand-in says which ones ran.
//!
//! `tailscale` has no test here: it is not installed on the machine this was
//! written on, and its start and stop are covered by the fake-binary tests in
//! `shprd-remote`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use shprd_remote::PasswordStore;
use shprd_server::{Config, Service};

/// A throwaway service with a password, and the pieces a test needs to talk to
/// it.
struct Live {
    port: u16,
    token: String,
    state: tempfile::TempDir,
    _repo: tempfile::TempDir,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Live {
    async fn start() -> Self {
        let repo = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        PasswordStore::new(state.path())
            .set_with_cost("a long enough password", 8, 1, 1)
            .unwrap();
        let mut config = Config::new(repo.path(), state.path());
        // The person's own programs, found on the `PATH` this test was started
        // with — the same value `shprd serve` hands the service.
        config.path_env = std::env::var("PATH").ok();
        let service = Service::start(config).await.unwrap();
        let port = service.info().port;
        let token = std::fs::read_to_string(&service.info().token_path)
            .unwrap()
            .trim()
            .to_owned();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            let _ = service
                .run(async {
                    let _ = stopped.await;
                })
                .await;
        });
        Self {
            port,
            token,
            state,
            _repo: repo,
            stop: Some(stop),
        }
    }

    /// One request to the local service, with the loopback door's headers.
    fn local(&self, path: &str, body: Option<Value>) -> (u16, Value) {
        let origin = format!("http://127.0.0.1:{}", self.port);
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(120)))
            .http_status_as_error(false)
            .build()
            .into();
        let url = format!("{origin}{path}");
        let mut response = match body {
            None => agent
                .get(&url)
                .header("Authorization", format!("Bearer {}", self.token))
                .call(),
            Some(body) => agent
                .post(&url)
                .header("Authorization", format!("Bearer {}", self.token))
                .header("Origin", &origin)
                .header("x-shprd", "1")
                .header("Content-Type", "application/json")
                .send(body.to_string()),
        }
        .unwrap();
        let status = response.status().as_u16();
        let text = response.body_mut().read_to_string().unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    async fn stop_service(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

/// What a public request answered: a status, or why there was none.
fn public_status(url: &str, skip_header: Option<(&str, &str)>) -> Result<u16, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .http_status_as_error(false)
        .build()
        .into();
    let mut request = agent.get(url);
    if let Some((name, value)) = skip_header {
        request = request.header(name, value);
    }
    request
        .call()
        .map(|response| response.status().as_u16())
        .map_err(|err| err.to_string())
}

/// Whether `program` is on the `PATH`.
fn installed(program: &str) -> bool {
    std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .any(|dir| std::path::Path::new(dir).join(program).is_file())
}

/// Starts `provider`, proves the gate over the public address, and stops it.
async fn prove(provider: &str, skip_header: Option<(&str, &str)>) {
    let live = Live::start().await;
    let started = {
        let body = json!({"provider": provider});
        let this = &live;
        tokio::task::block_in_place(|| this.local("/api/tunnels/start", Some(body)))
    };
    let (status, reply) = started;
    assert_eq!(status, 200, "{provider} did not start: {reply}");
    let link = reply["link"].as_str().unwrap().to_owned();
    println!(
        "{provider}: up, program: {}",
        reply["version"].as_str().unwrap_or("unknown")
    );

    // The assertions run inside this block so the tunnel is stopped after them
    // whatever they find.
    let outcome = std::panic::AssertUnwindSafe(async {
        let base = link.trim_end_matches('/').to_owned();
        // The name takes a moment to resolve and the edge a moment to route:
        // retry the unauthenticated read until the gate itself answers.
        let deadline = Instant::now() + Duration::from_secs(90);
        loop {
            let url = format!("{base}/api/state");
            let seen = tokio::task::block_in_place(|| public_status(&url, skip_header));
            let last = match seen {
                Ok(401) => break,
                Ok(other) => format!("status {other}"),
                Err(why) => why,
            };
            assert!(
                Instant::now() < deadline,
                "{provider}: never saw the gate's 401 over the public address ({last})"
            );
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        // Reachable and public: the unauthenticated handshake answers 200, and
        // a state read without a session does not.
        let health = tokio::task::block_in_place(|| {
            public_status(&format!("{base}/api/health"), skip_header)
        });
        assert_eq!(health, Ok(200), "{provider}: /api/health over the tunnel");
    });
    let result = futures_util::FutureExt::catch_unwind(outcome).await;

    let (status, stopped) =
        tokio::task::block_in_place(|| live.local("/api/tunnels/stop", Some(json!({}))));
    assert_eq!(status, 200, "{provider} did not stop: {stopped}");
    assert_eq!(stopped["on"], json!(false));
    let file = shprd_remote::tunnel::WebFile::read(live.state.path());
    assert!(!file.on, "{provider}: web.json still says on");
    live.stop_service().await;
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "starts a real cloudflared quick tunnel (public internet)"]
async fn cloudflared_quick_serves_the_gate_and_stops() {
    if !installed("cloudflared") {
        println!("skipped: cloudflared is not installed");
        return;
    }
    prove("cloudflared", None).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "starts a real tunnl.gg tunnel over ssh (public internet)"]
async fn tunnl_serves_the_gate_and_stops() {
    if !installed("ssh") {
        println!("skipped: ssh is not installed");
        return;
    }
    prove("tunnl", Some(("tunnl-skip-browser-warning", "1"))).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "starts a real ngrok tunnel (public internet)"]
async fn ngrok_serves_the_gate_and_stops() {
    if !installed("ngrok") {
        println!("skipped: ngrok is not installed");
        return;
    }
    let configured = std::process::Command::new("ngrok")
        .args(["config", "check"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    if !configured {
        println!("skipped: ngrok has no usable configuration (no authtoken)");
        return;
    }
    prove("ngrok", Some(("ngrok-skip-browser-warning", "1"))).await;
}
