//! The tunnel routes, driven through the router with a fake provider.
//!
//! A fake provider stands in for cloudflared and friends, so what is proven here
//! is the wiring: the fail-closed rule reaches the route, the `Host` allowlist
//! follows the tunnel through the real gate, a stale session cannot start one,
//! and `Tailscale-User-Login` is believed from Tailscale serve and nothing else.
//! Nothing starts a real tunnel and nothing reads a real state directory.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use shprd_core::event::Actor;
use shprd_core::log::Clock;
use shprd_remote::PasswordStore;
use shprd_remote::tunnel::{
    BoxFuture, Options, Provider, PublicEndpoint, Tunnel, TunnelError, TunnelFactory, TunnelState,
};
use shprd_server::App;
use tempfile::TempDir;
use tower::ServiceExt;

const PORT: u16 = 18_081;
const HOST: &str = "127.0.0.1:18081";
const ORIGIN: &str = "http://127.0.0.1:18081";
const PASSWORD: &str = "a long enough password";
const QUICK: &str = "fake-words-here.trycloudflare.com";
const TAILNET: &str = "studio.tail1234.ts.net";

#[derive(Debug, Default)]
struct FakeClock(AtomicU64);

impl Clock for FakeClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// A provider that comes up at a fixed name and goes down when told.
#[derive(Debug)]
struct Fakes;

struct Fake(Provider);

impl Tunnel for Fake {
    fn provider(&self) -> Provider {
        self.0
    }
    fn version(&self) -> BoxFuture<'_, Option<String>> {
        Box::pin(async { Some("fake 1".to_owned()) })
    }
    fn start(&mut self, _: SocketAddr) -> BoxFuture<'_, Result<PublicEndpoint, TunnelError>> {
        let url = match self.0 {
            Provider::Tailscale => format!("https://{TAILNET}/shprd/"),
            _ => format!("https://{QUICK}"),
        };
        let provider = self.0;
        Box::pin(async move { Ok(PublicEndpoint::new(&url, provider).unwrap()) })
    }
    fn status(&mut self) -> BoxFuture<'_, TunnelState> {
        Box::pin(async { TunnelState::Starting })
    }
    fn stop(&mut self) -> BoxFuture<'_, Result<(), TunnelError>> {
        Box::pin(async { Ok(()) })
    }
}

impl TunnelFactory for Fakes {
    fn create(&self, provider: Provider, _: &Options) -> Result<Box<dyn Tunnel>, TunnelError> {
        Ok(Box::new(Fake(provider)))
    }
}

struct Fixture {
    app: App,
    token: String,
    state: TempDir,
    _repo: TempDir,
    clock: Arc<FakeClock>,
}

impl Fixture {
    fn new() -> Self {
        let repo = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let clock = Arc::new(FakeClock(AtomicU64::new(1_700_000_000_000)));
        let app = App::prepare(repo.path(), PORT, &Actor::Human, state.path(), None)
            .unwrap()
            .with_clock(clock.clone())
            .with_tunnel_factory(Arc::new(Fakes));
        let token = std::fs::read_to_string(app.info().token_path)
            .unwrap()
            .trim()
            .to_string();
        Self {
            app,
            token,
            state,
            _repo: repo,
            clock,
        }
    }

    fn with_password(self) -> Self {
        PasswordStore::new(self.state.path())
            .set_with_cost(PASSWORD, 8, 1, 1)
            .unwrap();
        self
    }

    async fn send(&self, request: Request<Body>) -> (StatusCode, Value, Option<String>) {
        let response = self.app.router().oneshot(request).await.unwrap();
        let status = response.status();
        let cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, body, cookie)
    }

    fn token_get(&self, uri: &str, host: &str) -> Request<Body> {
        Request::builder()
            .uri(uri)
            .header(header::HOST, host)
            .header(header::AUTHORIZATION, format!("Bearer {}", self.token))
            .body(Body::empty())
            .unwrap()
    }

    fn token_post(&self, uri: &str, body: Value) -> Request<Body> {
        Request::builder()
            .method(Method::POST)
            .uri(uri)
            .header(header::HOST, HOST)
            .header(header::ORIGIN, ORIGIN)
            .header("x-shprd", "1")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::AUTHORIZATION, format!("Bearer {}", self.token))
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    fn cookie_post(&self, uri: &str, cookie: &str, body: Value) -> Request<Body> {
        Request::builder()
            .method(Method::POST)
            .uri(uri)
            .header(header::HOST, HOST)
            .header(header::ORIGIN, ORIGIN)
            .header("x-shprd", "1")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::COOKIE, cookie)
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    async fn login(&self) -> String {
        let request = Request::builder()
            .method(Method::POST)
            .uri("/api/login")
            .header(header::HOST, HOST)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(json!({"password": PASSWORD}).to_string()))
            .unwrap();
        let (status, _, cookie) = self.send(request).await;
        assert_eq!(status, StatusCode::OK);
        let id = cookie
            .unwrap()
            .split(';')
            .next()
            .and_then(|pair| pair.split_once('='))
            .map(|(_, value)| value.to_owned())
            .unwrap();
        format!("shprd={id}")
    }

    /// What a stranger at `host` with no credential is told.
    async fn stranger(&self, host: &str) -> StatusCode {
        let request = Request::builder()
            .uri("/api/state")
            .header(header::HOST, host)
            .body(Body::empty())
            .unwrap();
        self.send(request).await.0
    }
}

#[tokio::test]
async fn the_routes_are_behind_the_gate() {
    let fixture = Fixture::new().with_password();
    for (method, uri) in [
        (Method::GET, "/api/tunnels"),
        (Method::POST, "/api/tunnels/start"),
        (Method::POST, "/api/tunnels/stop"),
    ] {
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::HOST, HOST)
            .body(Body::empty())
            .unwrap();
        let (status, _, _) = fixture.send(request).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}");
    }
}

#[tokio::test]
async fn the_status_says_what_could_be_started_and_that_nothing_is() {
    let fixture = Fixture::new();
    let (status, body, _) = fixture.send(fixture.token_get("/api/tunnels", HOST)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["on"], json!(false));
    assert_eq!(body["password_set"], json!(false));
    assert_eq!(body["default_provider"], json!("tailscale"));
    let providers: Vec<(&str, bool)> = body["providers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["id"].as_str().unwrap(),
                p["public_internet"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        providers,
        [
            ("tailscale", false),
            ("cloudflared", true),
            ("cloudflared-named", true),
            ("ngrok", true),
            ("tunnl", true)
        ]
    );
}

#[tokio::test]
async fn a_public_tunnel_is_refused_without_a_password_and_nothing_is_allowed() {
    let fixture = Fixture::new();
    let (status, body, _) = fixture
        .send(fixture.token_post("/api/tunnels/start", json!({"provider": "cloudflared"})))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], json!("refused"));
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("no login password"),
        "{body}"
    );
    assert_eq!(
        fixture.stranger(QUICK).await,
        StatusCode::MISDIRECTED_REQUEST
    );
    let (_, status, _) = fixture.send(fixture.token_get("/api/tunnels", HOST)).await;
    assert_eq!(status["on"], json!(false));
    assert!(status["why"].as_str().unwrap().contains("password"));
}

#[tokio::test]
async fn start_allows_the_tunnels_host_and_stop_closes_it_again() {
    let fixture = Fixture::new().with_password();
    // Before: an unknown Host is a 421, not a 401.
    assert_eq!(
        fixture.stranger(QUICK).await,
        StatusCode::MISDIRECTED_REQUEST
    );

    let (status, body, _) = fixture
        .send(fixture.token_post("/api/tunnels/start", json!({"provider": "cloudflared"})))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["on"], json!(true));
    assert_eq!(body["link"], json!(format!("https://{QUICK}")));
    assert!(body["qr_svg"].as_str().unwrap().contains("<svg"));
    assert_eq!(body["version"], json!("fake 1"));

    // During: the host reaches the gate and is told to sign in.
    assert_eq!(fixture.stranger(QUICK).await, StatusCode::UNAUTHORIZED);
    assert_eq!(
        fixture.stranger("other.trycloudflare.com").await,
        StatusCode::MISDIRECTED_REQUEST
    );
    // web.json is on and private.
    let file = shprd_remote::tunnel::WebFile::read(fixture.state.path());
    assert!(file.on);

    let (status, body, _) = fixture
        .send(fixture.token_post("/api/tunnels/stop", json!({})))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["on"], json!(false));
    // After: closed again.
    assert_eq!(
        fixture.stranger(QUICK).await,
        StatusCode::MISDIRECTED_REQUEST
    );
    assert!(!shprd_remote::tunnel::WebFile::read(fixture.state.path()).on);
}

#[tokio::test]
async fn a_second_start_is_refused_while_one_is_up() {
    let fixture = Fixture::new().with_password();
    let start = || fixture.token_post("/api/tunnels/start", json!({"provider": "cloudflared"}));
    assert_eq!(fixture.send(start()).await.0, StatusCode::OK);
    let (status, body, _) = fixture.send(start()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body["message"].as_str().unwrap().contains("already"));
    fixture
        .send(fixture.token_post("/api/tunnels/stop", json!({})))
        .await;
}

#[tokio::test]
async fn an_unknown_provider_is_a_bad_request() {
    let fixture = Fixture::new().with_password();
    let (status, body, _) = fixture
        .send(fixture.token_post("/api/tunnels/start", json!({"provider": "carrier-pigeon"})))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn a_stale_session_cannot_start_a_tunnel_but_a_fresh_one_can() {
    let fixture = Fixture::new().with_password();
    let cookie = fixture.login().await;
    fixture
        .clock
        .0
        .fetch_add(5 * 60 * 1000 + 1000, Ordering::SeqCst);
    let (status, body, _) = fixture
        .send(fixture.cookie_post(
            "/api/tunnels/start",
            &cookie,
            json!({"provider": "cloudflared"}),
        ))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["code"], json!("reauth"));
    assert_eq!(
        fixture.stranger(QUICK).await,
        StatusCode::MISDIRECTED_REQUEST
    );

    // The password again refreshes the session, and then it can.
    let again = Request::builder()
        .method(Method::POST)
        .uri("/api/login")
        .header(header::HOST, HOST)
        .header(header::COOKIE, &cookie)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"password": PASSWORD}).to_string()))
        .unwrap();
    assert_eq!(fixture.send(again).await.0, StatusCode::OK);
    let (status, body, _) = fixture
        .send(fixture.cookie_post(
            "/api/tunnels/start",
            &cookie,
            json!({"provider": "cloudflared"}),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    fixture
        .send(fixture.token_post("/api/tunnels/stop", json!({})))
        .await;
}

#[tokio::test]
async fn tailscale_runs_without_a_password_and_its_login_header_is_believed_only_there() {
    let fixture = Fixture::new();
    let (status, body, _) = fixture
        .send(fixture.token_post("/api/tunnels/start", json!({"provider": "tailscale"})))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["on"], json!(true));

    let with_login = |host: &str| {
        Request::builder()
            .uri("/api/tunnels")
            .header(header::HOST, host)
            .header(header::AUTHORIZATION, format!("Bearer {}", fixture.token))
            .header("tailscale-user-login", "person@example.com")
            .body(Body::empty())
            .unwrap()
    };
    // Through tailscale serve's own host: believed.
    let (status, body, _) = fixture.send(with_login(TAILNET)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["tailnet_user"], json!("person@example.com"));
    // From loopback, the same header is just text.
    let (_, body, _) = fixture.send(with_login(HOST)).await;
    assert!(body.get("tailnet_user").is_none(), "{body}");
    fixture
        .send(fixture.token_post("/api/tunnels/stop", json!({})))
        .await;

    // With a public tunnel up instead, it is never believed.
    PasswordStore::new(fixture.state.path())
        .set_with_cost(PASSWORD, 8, 1, 1)
        .unwrap();
    fixture
        .send(fixture.token_post("/api/tunnels/start", json!({"provider": "cloudflared"})))
        .await;
    let (_, body, _) = fixture.send(with_login(QUICK)).await;
    assert!(body.get("tailnet_user").is_none(), "{body}");
    fixture
        .send(fixture.token_post("/api/tunnels/stop", json!({})))
        .await;
}

#[tokio::test]
async fn x_forwarded_for_never_changes_who_the_peer_is() {
    // Ten wrong passwords with ten different forwarded addresses still count as
    // one peer: the sixth is the per-address block.
    let fixture = Fixture::new().with_password();
    let mut statuses = Vec::new();
    for n in 0..8 {
        let request = Request::builder()
            .method(Method::POST)
            .uri("/api/login")
            .header(header::HOST, HOST)
            .header("x-forwarded-for", format!("203.0.113.{n}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({"password": "wrong wrong wrong"}).to_string(),
            ))
            .unwrap();
        statuses.push(fixture.send(request).await.0);
    }
    assert_eq!(statuses[0], StatusCode::UNAUTHORIZED);
    assert_eq!(
        statuses.last(),
        Some(&StatusCode::TOO_MANY_REQUESTS),
        "{statuses:?}"
    );
}

#[tokio::test]
async fn a_login_through_the_tunnel_gets_the_secure_cookie_and_one_on_loopback_does_not() {
    let fixture = Fixture::new().with_password();
    fixture
        .send(fixture.token_post("/api/tunnels/start", json!({"provider": "cloudflared"})))
        .await;
    let login = |host: &str| {
        Request::builder()
            .method(Method::POST)
            .uri("/api/login")
            .header(header::HOST, host)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(json!({"password": PASSWORD}).to_string()))
            .unwrap()
    };
    // Every tunnel serves https at its edge whatever the loopback leg is, so a
    // request under the tunnel's host gets the `__Host-` and `Secure` form.
    let (status, _, cookie) = fixture.send(login(QUICK)).await;
    assert_eq!(status, StatusCode::OK);
    let cookie = cookie.unwrap();
    assert!(cookie.starts_with("__Host-shprd="), "{cookie}");
    assert!(cookie.contains("; Secure"), "{cookie}");
    assert!(
        cookie.contains("HttpOnly") && cookie.contains("SameSite=Lax"),
        "{cookie}"
    );
    // On loopback http a browser would drop a Secure cookie, so it is plain.
    let (_, _, cookie) = fixture.send(login(HOST)).await;
    let cookie = cookie.unwrap();
    assert!(cookie.starts_with("shprd="), "{cookie}");
    assert!(!cookie.contains("Secure"), "{cookie}");
    fixture
        .send(fixture.token_post("/api/tunnels/stop", json!({})))
        .await;
}
