//! The login, the sessions and the gate, driven through the router.
//!
//! Each test makes its own repository and state directory, sets its own password
//! and moves a clock it owns, so nothing here reads the developer's state
//! directory, their XDG directories or a real socket.
//!
//! The gate's decision table itself is a unit test in `shprd-remote`; these tests
//! prove the wiring — that the cookie a browser gets really does open the door,
//! that the token door still works, that an unknown `Host` is a `421` everywhere
//! and that a destructive action asks for the password again.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use shprd_core::event::{Actor, EventKind};
use shprd_core::log::{Clock, EventLog, SystemClock};
use shprd_remote::PasswordStore;
use shprd_server::App;
use tempfile::TempDir;
use tower::ServiceExt;

/// The port every fixture claims in its `Host`, `Origin` and `service.json`.
const PORT: u16 = 18_080;

/// The `Host` a loopback client addresses.
const HOST: &str = "127.0.0.1:18080";

/// The loopback page's own origin.
const ORIGIN: &str = "http://127.0.0.1:18080";

/// A tunnel hostname the fixture added, as M11.2 will.
const TUNNEL: &str = "abc.trycloudflare.com";

/// The password every test that needs one uses. Not in the common list.
const PASSWORD: &str = "a long enough password";

/// A clock a test moves by hand.
#[derive(Debug, Default)]
struct FakeClock(AtomicU64);

impl FakeClock {
    fn at(ms: u64) -> Arc<Self> {
        Arc::new(Self(AtomicU64::new(ms)))
    }

    fn advance(&self, secs: u64) {
        self.0.fetch_add(secs * 1000, Ordering::SeqCst);
    }
}

impl Clock for FakeClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// One prepared service, with the temp dirs it lives in kept alive.
struct Fixture {
    app: App,
    token: String,
    state: TempDir,
    _repo: TempDir,
    clock: Arc<FakeClock>,
}

impl Fixture {
    /// A service over a repository whose run has just started, with a clock the
    /// test owns and a watermark well past 2020 so a real timestamp is obvious.
    fn new() -> Self {
        let repo = tempfile::tempdir().expect("repo dir");
        start_run(repo.path());
        let state = tempfile::tempdir().expect("state dir");
        let app =
            App::prepare(repo.path(), PORT, &Actor::Human, state.path(), None).expect("prepare");
        let clock = FakeClock::at(1_700_000_000_000);
        let app = app.with_clock(clock.clone());
        let token = std::fs::read_to_string(app.info().token_path)
            .expect("the token file")
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

    /// Sets the login password the way `shprd web password set` does, in the same
    /// state directory the service reads.
    fn with_password(self) -> Self {
        PasswordStore::new(self.state.path())
            // Argon2 at 64 MiB is the right cost for a password and a pointless
            // one for a test: the parameters come back from the file, so a cheap
            // hash still exercises every path a real one does.
            .set_with_cost(PASSWORD, 8, 1, 1)
            .expect("the password sets");
        self
    }

    /// Sends one request through the router.
    async fn send(&self, call: Call) -> Reply {
        let response = self
            .app
            .router()
            .oneshot(call.request())
            .await
            .expect("the router answers");
        let status = response.status();
        let cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let retry_after = response
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("the body reads")
            .to_bytes();
        // A refusal from the body limit is plain text, not JSON.
        let body = if bytes.is_empty() {
            None
        } else {
            Some(
                serde_json::from_slice(&bytes)
                    .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).to_string())),
            )
        };
        Reply {
            status,
            body,
            cookie,
            retry_after,
        }
    }

    /// Logs in and returns the session id its cookie carries.
    async fn login(&self, password: &str) -> String {
        let reply = self.send(Call::login(json!({"password": password}))).await;
        assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
        cookie_id(reply.cookie.as_deref().expect("a Set-Cookie"))
    }

    /// The session id a `Cookie` header would carry, for the loopback name.
    fn cookie(id: &str) -> String {
        format!("shprd={id}")
    }
}

/// One request under construction.
struct Call {
    method: Method,
    uri: String,
    host: String,
    token: Option<String>,
    cookie: Option<String>,
    origin: Option<String>,
    write_header: bool,
    body: Option<Value>,
}

impl Call {
    fn get(uri: &str) -> Self {
        Self {
            method: Method::GET,
            uri: uri.to_string(),
            host: HOST.to_string(),
            token: None,
            cookie: None,
            origin: None,
            write_header: false,
            body: None,
        }
    }

    fn post(uri: &str, body: Value) -> Self {
        Self {
            method: Method::POST,
            uri: uri.to_string(),
            body: Some(body),
            ..Self::get(uri)
        }
    }

    /// `POST /api/login`, as the login page sends it.
    fn login(body: Value) -> Self {
        Self::post("/api/login", body)
    }

    fn with_host(mut self, host: &str) -> Self {
        self.host = host.to_string();
        self
    }

    fn with_token(mut self, token: &str) -> Self {
        self.token = Some(token.to_string());
        self
    }

    fn with_cookie(mut self, cookie: &str) -> Self {
        self.cookie = Some(cookie.to_string());
        self
    }

    fn with_origin(mut self, origin: &str) -> Self {
        self.origin = Some(origin.to_string());
        self
    }

    fn with_write_header(mut self) -> Self {
        self.write_header = true;
        self
    }

    fn request(&self) -> Request<Body> {
        let mut builder = Request::builder()
            .method(self.method.clone())
            .uri(&self.uri)
            .header(header::HOST, &self.host);
        if let Some(token) = &self.token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        if let Some(cookie) = &self.cookie {
            builder = builder.header(header::COOKIE, cookie);
        }
        if self.write_header {
            builder = builder.header("x-shprd", "1");
        }
        if let Some(origin) = &self.origin {
            builder = builder.header(header::ORIGIN, origin);
        }
        let body = match &self.body {
            Some(value) => {
                builder = builder.header(header::CONTENT_TYPE, "application/json");
                Body::from(serde_json::to_vec(value).expect("the body serializes"))
            }
            None => Body::empty(),
        };
        builder.body(body).expect("the request is well formed")
    }
}

/// What a request answered.
struct Reply {
    status: StatusCode,
    body: Option<Value>,
    cookie: Option<String>,
    retry_after: Option<String>,
}

impl Reply {
    fn json(&self) -> &Value {
        self.body.as_ref().expect("the reply carries a body")
    }
}

/// The session id out of a `Set-Cookie` value.
fn cookie_id(cookie: &str) -> String {
    cookie
        .split(';')
        .next()
        .and_then(|pair| pair.split_once('='))
        .map(|(_, value)| value.to_string())
        .expect("a name=value cookie")
}

/// Appends a run start to `repo`'s log, the way `shprd run start` does.
fn start_run(repo: &std::path::Path) {
    let path = repo.join(".shprd").join("events.jsonl");
    let mut log = EventLog::open(&path).expect("the log opens");
    log.append(
        Actor::Human,
        EventKind::RunStarted {
            run_id: "r1".to_string(),
            repo: repo.display().to_string(),
            base_branch: "main".to_string(),
            integration_branch: "integration/m11".to_string(),
        },
        &SystemClock,
    )
    .expect("the run starts");
}

#[tokio::test]
async fn a_login_with_the_password_sets_a_cookie_and_opens_the_door() {
    let fixture = Fixture::new().with_password();
    let reply = fixture
        .send(Call::login(json!({"password": PASSWORD})))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
    assert_eq!(reply.json()["ok"], json!(true));
    assert_eq!(reply.json()["refreshed"], json!(false));

    let cookie = reply.cookie.expect("a Set-Cookie");
    assert!(cookie.starts_with("shprd="), "loopback http: {cookie}");
    assert!(cookie.contains("HttpOnly"), "{cookie}");
    assert!(cookie.contains("SameSite=Lax"), "{cookie}");
    assert!(cookie.contains("Path=/"), "{cookie}");
    assert!(!cookie.contains("Secure"), "a browser drops that on http");

    let id = cookie_id(&cookie);
    assert_eq!(id.len(), 64, "32 bytes of hex");

    // The cookie alone opens a read.
    let session = fixture
        .send(Call::get("/api/session").with_cookie(&Fixture::cookie(&id)))
        .await;
    assert_eq!(session.status, StatusCode::OK, "{:?}", session.body);
    assert_eq!(session.json()["via"], json!("session"));
    assert_eq!(session.json()["password"], json!(true));
    assert_eq!(session.json()["fresh"], json!(true));
    assert_eq!(
        session.json()["session"]["id"].as_str().map(str::len),
        Some(12)
    );

    let state = fixture
        .send(Call::get("/api/state").with_cookie(&Fixture::cookie(&id)))
        .await;
    assert_eq!(state.status, StatusCode::OK, "the cookie is a door");
}

#[tokio::test]
async fn an_existing_session_is_refreshed_by_a_second_login_not_replaced() {
    let fixture = Fixture::new().with_password();
    let id = fixture.login(PASSWORD).await;
    let again = fixture
        .send(
            Call::login(json!({"password": PASSWORD, "remember": true}))
                .with_cookie(&Fixture::cookie(&id)),
        )
        .await;
    assert_eq!(again.status, StatusCode::OK, "{:?}", again.body);
    assert_eq!(again.json()["refreshed"], json!(true));
    assert_eq!(
        cookie_id(again.cookie.as_deref().expect("a Set-Cookie")),
        id,
        "the same session, refreshed"
    );
}

#[tokio::test]
async fn a_wrong_password_and_no_password_at_all_get_one_generic_refusal() {
    let fixture = Fixture::new();

    // No password set: nothing is accepted.
    let nothing = fixture
        .send(Call::login(json!({"password": PASSWORD})))
        .await;
    assert_eq!(nothing.status, StatusCode::UNAUTHORIZED);
    assert_eq!(nothing.json()["ok"], json!(false));
    assert_eq!(
        nothing.json()["message"],
        json!(shprd_server::remote::FAILURE)
    );

    PasswordStore::new(fixture.state.path())
        .set_with_cost(PASSWORD, 8, 1, 1)
        .expect("the password sets");
    let wrong = fixture
        .send(Call::login(json!({"password": "a different long one"})))
        .await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        wrong.json()["message"],
        json!(shprd_server::remote::FAILURE),
        "the same words, so a stranger learns nothing"
    );
    assert!(wrong.cookie.is_none(), "a failure sets no cookie");
}

#[tokio::test]
async fn every_route_but_nothing_at_an_unknown_host() {
    let fixture = Fixture::new().with_password();
    for uri in ["/api/health", "/api/login", "/api/state", "/api/session"] {
        let call = if uri == "/api/login" {
            Call::post(uri, json!({"password": PASSWORD}))
        } else {
            Call::get(uri)
        }
        .with_host("evil.example");
        let reply = fixture.send(call).await;
        assert_eq!(reply.status, StatusCode::MISDIRECTED_REQUEST, "{uri}");
        assert_eq!(reply.json()["code"], json!("wrong_host"), "{uri}");
    }

    // The health handshake is unauthenticated but still only under a known name.
    let health = fixture.send(Call::get("/api/health")).await;
    assert_eq!(health.status, StatusCode::OK);
}

#[tokio::test]
async fn a_tunnel_host_gets_the_secure_host_prefixed_cookie() {
    let fixture = Fixture::new().with_password();
    fixture.app.allowed_hosts().add(TUNNEL).expect("a host");
    let reply = fixture
        .send(Call::login(json!({"password": PASSWORD})).with_host(TUNNEL))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
    let cookie = reply.cookie.expect("a Set-Cookie");
    assert!(cookie.starts_with("__Host-shprd="), "{cookie}");
    assert!(cookie.contains("; Secure"), "{cookie}");

    // And that host was allowed only because the tunnel added it.
    let unknown = fixture
        .send(Call::get("/api/state").with_host("other.example"))
        .await;
    assert_eq!(unknown.status, StatusCode::MISDIRECTED_REQUEST);
}

#[tokio::test]
async fn a_write_through_a_session_needs_the_origin_and_the_header() {
    let fixture = Fixture::new().with_password();
    let id = fixture.login(PASSWORD).await;
    let cookie = Fixture::cookie(&id);
    let body = json!({
        "kind": "task.add",
        "task": "T1",
        "title": "t",
        "scope": ["crates/shprd-core"],
        "acceptance": ["it works"],
    });

    let no_origin = fixture
        .send(
            Call::post("/api/action", body.clone())
                .with_cookie(&cookie)
                .with_write_header(),
        )
        .await;
    assert_eq!(
        no_origin.status,
        StatusCode::FORBIDDEN,
        "{:?}",
        no_origin.body
    );
    assert_eq!(no_origin.json()["code"], json!("cross_origin"));

    let foreign = fixture
        .send(
            Call::post("/api/action", body.clone())
                .with_cookie(&cookie)
                .with_origin("https://evil.example")
                .with_write_header(),
        )
        .await;
    assert_eq!(foreign.status, StatusCode::FORBIDDEN);

    let no_header = fixture
        .send(
            Call::post("/api/action", body.clone())
                .with_cookie(&cookie)
                .with_origin(ORIGIN),
        )
        .await;
    assert_eq!(no_header.status, StatusCode::FORBIDDEN);
    assert_eq!(no_header.json()["code"], json!("missing_header"));

    let added = fixture
        .send(
            Call::post("/api/action", body)
                .with_cookie(&cookie)
                .with_origin(ORIGIN)
                .with_write_header(),
        )
        .await;
    assert_eq!(added.status, StatusCode::OK, "{:?}", added.body);
}

#[tokio::test]
async fn the_loopback_token_still_opens_every_route_and_needs_no_session() {
    let fixture = Fixture::new();
    let state = fixture
        .send(Call::get("/api/state").with_token(&fixture.token))
        .await;
    assert_eq!(state.status, StatusCode::OK);

    // The token door is exempt from the fresh password a destructive action
    // needs, because the token is already local.
    let dropped = fixture
        .send(
            Call::post(
                "/api/action",
                json!({"kind": "task.drop", "task": "T1", "reason": "no"}),
            )
            .with_token(&fixture.token)
            .with_origin(ORIGIN)
            .with_write_header(),
        )
        .await;
    assert_ne!(
        dropped.status,
        StatusCode::UNAUTHORIZED,
        "the token door never asks for a fresh password: {:?}",
        dropped.body
    );

    let session = fixture
        .send(Call::get("/api/session").with_token(&fixture.token))
        .await;
    assert_eq!(session.json()["via"], json!("token"));
    assert_eq!(session.json()["fresh"], json!(true));
}

#[tokio::test]
async fn a_destructive_action_needs_the_password_again_after_five_minutes() {
    let fixture = Fixture::new().with_password();
    let id = fixture.login(PASSWORD).await;
    let cookie = Fixture::cookie(&id);
    let drop = || {
        Call::post(
            "/api/action",
            json!({"kind": "task.drop", "task": "T1", "reason": "no"}),
        )
        .with_cookie(&cookie)
        .with_origin(ORIGIN)
        .with_write_header()
    };

    // Fresh: the request gets as far as the action's own rules.
    let fresh = fixture.send(drop()).await;
    assert_ne!(fresh.status, StatusCode::UNAUTHORIZED, "{:?}", fresh.body);

    fixture.clock.advance(5 * 60 + 1);
    let stale = fixture.send(drop()).await;
    assert_eq!(stale.status, StatusCode::UNAUTHORIZED, "{:?}", stale.body);
    assert_eq!(stale.json()["code"], json!("reauth"), "{:?}", stale.body);

    // Entering the password again refreshes this session, and the action works.
    let refreshed = fixture
        .send(Call::login(json!({"password": PASSWORD})).with_cookie(&Fixture::cookie(&id)))
        .await;
    assert_eq!(refreshed.json()["refreshed"], json!(true));
    let after = fixture.send(drop()).await;
    assert_ne!(after.status, StatusCode::UNAUTHORIZED, "{:?}", after.body);
}

#[tokio::test]
async fn a_logout_ends_the_session_it_names_and_clears_the_cookie() {
    let fixture = Fixture::new().with_password();
    let id = fixture.login(PASSWORD).await;
    let cookie = Fixture::cookie(&id);

    let out = fixture
        .send(
            Call::post("/api/logout", json!({}))
                .with_cookie(&cookie)
                .with_origin(ORIGIN)
                .with_write_header(),
        )
        .await;
    assert_eq!(out.status, StatusCode::OK, "{:?}", out.body);
    assert_eq!(out.json()["revoked"], json!(true));
    let cleared = out.cookie.expect("a Set-Cookie");
    assert!(cleared.starts_with("shprd=;"), "{cleared}");
    assert!(cleared.contains("Max-Age=0"), "{cleared}");

    let after = fixture
        .send(Call::get("/api/session").with_cookie(&cookie))
        .await;
    assert_eq!(
        after.status,
        StatusCode::UNAUTHORIZED,
        "revoked means revoked"
    );
}

#[tokio::test]
async fn the_session_list_marks_this_one_and_a_revoke_signs_the_other_out() {
    let fixture = Fixture::new().with_password();
    let mine = fixture.login(PASSWORD).await;
    let other = fixture.login(PASSWORD).await;
    let my_cookie = Fixture::cookie(&mine);

    let list = fixture
        .send(Call::get("/api/sessions").with_cookie(&my_cookie))
        .await;
    assert_eq!(list.status, StatusCode::OK, "{:?}", list.body);
    let sessions = list.json()["sessions"].as_array().expect("a list").clone();
    assert_eq!(sessions.len(), 2);
    let current = list.json()["current"].as_str().expect("a current id");
    assert!(
        sessions.iter().any(|s| s["id"] == json!(current)),
        "the current id is one of the sessions"
    );
    let text = list.json().to_string();
    assert!(!text.contains(&mine) && !text.contains(&other), "no raw id");

    let theirs = sessions
        .iter()
        .find(|s| s["id"] != json!(current))
        .and_then(|s| s["id"].as_str())
        .expect("the other session")
        .to_string();
    let revoke = |id: &str| {
        Call {
            method: Method::DELETE,
            ..Call::get(&format!("/api/sessions/{id}"))
        }
        .with_cookie(&my_cookie)
        .with_origin(ORIGIN)
        .with_write_header()
    };
    let gone = fixture.send(revoke(&theirs)).await;
    assert_eq!(gone.status, StatusCode::OK, "{:?}", gone.body);
    let after = fixture
        .send(Call::get("/api/session").with_cookie(&Fixture::cookie(&other)))
        .await;
    assert_eq!(after.status, StatusCode::UNAUTHORIZED);

    let missing = fixture.send(revoke("ffffffffffff")).await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND, "{:?}", missing.body);

    // Stale: the password is asked for again, and nothing is revoked.
    fixture.clock.advance(5 * 60 + 1);
    let stale = fixture.send(revoke(&current.to_string())).await;
    assert_eq!(stale.status, StatusCode::UNAUTHORIZED, "{:?}", stale.body);
    assert_eq!(stale.json()["code"], json!("reauth"));
}

#[tokio::test]
async fn a_password_change_revokes_every_session() {
    let fixture = Fixture::new().with_password();
    let first = fixture.login(PASSWORD).await;
    let second = fixture.login(PASSWORD).await;
    assert_ne!(first, second);

    // What `shprd web password set` does next, in its own process: one store
    // writes the hash, another — the same file — ends every session.
    PasswordStore::new(fixture.state.path())
        .set_with_cost("another long password", 8, 1, 1)
        .expect("the new password");
    let sessions = shprd_remote::Sessions::new(fixture.state.path(), fixture.clock.clone());
    assert_eq!(sessions.revoke_all().expect("a revoke"), 2);

    for id in [first, second] {
        let reply = fixture
            .send(Call::get("/api/session").with_cookie(&Fixture::cookie(&id)))
            .await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    }
}

#[tokio::test]
async fn six_logins_from_one_address_are_a_429_with_a_retry_after() {
    let fixture = Fixture::new().with_password();
    for _ in 0..5 {
        let reply = fixture
            .send(Call::login(json!({"password": "a wrong long password"})))
            .await;
        assert_ne!(
            reply.status,
            StatusCode::TOO_MANY_REQUESTS,
            "{:?}",
            reply.body
        );
    }
    let refused = fixture
        .send(Call::login(json!({"password": PASSWORD})))
        .await;
    assert_eq!(
        refused.status,
        StatusCode::TOO_MANY_REQUESTS,
        "{:?}",
        refused.body
    );
    assert!(
        refused.retry_after.is_some(),
        "a 429 says when to try again"
    );
}

#[tokio::test]
async fn a_json_body_over_a_megabyte_is_refused() {
    let fixture = Fixture::new().with_password();
    let huge = json!({"password": "x".repeat(1024 * 1024 + 16)});
    let reply = fixture.send(Call::login(huge)).await;
    assert_eq!(reply.status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn a_session_is_not_a_token_and_a_token_is_not_a_session() {
    let fixture = Fixture::new().with_password();
    // A made-up session id opens nothing.
    let invented = fixture
        .send(Call::get("/api/state").with_cookie(&Fixture::cookie(&"ab".repeat(32))))
        .await;
    assert_eq!(invented.status, StatusCode::UNAUTHORIZED);

    // A wrong bearer token opens nothing either, and is not quietly a session.
    let wrong = fixture
        .send(Call::get("/api/state").with_token("not-the-token"))
        .await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
}
