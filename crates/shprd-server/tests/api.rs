//! The API's acceptance tests: one prepared service per test, driven through the
//! router with `tower::ServiceExt`, plus one real socket for the WebSocket.
//!
//! Nothing here needs a network, a model or a multiplexer. Each test makes its
//! own repository (a temp dir with an empty run) and its own state directory, so
//! the tests run in parallel without sharing a token.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use shprd_core::event::{Actor, EventKind};
use shprd_core::log::{EventLog, SystemClock};
use shprd_server::{App, WsLimits};
use tempfile::TempDir;
use tower::ServiceExt;

/// The port the fixtures claim in their `Origin` and `service.json`.
const PORT: u16 = 18_080;

/// One prepared service, with the temp dirs it lives in kept alive.
struct Fixture {
    app: App,
    token: String,
    id: String,
    _state: TempDir,
    _repo: TempDir,
}

impl Fixture {
    /// A service over a repository whose run has just started.
    ///
    /// The run start is the log's first event and every rule needs it, so it is
    /// written before the service is prepared, exactly as `shprd up` writes it.
    fn new() -> Self {
        let state = tempfile::tempdir().expect("state dir");
        let repo = tempfile::tempdir().expect("repo dir");
        start_run(repo.path());
        let app =
            App::prepare(repo.path(), PORT, &Actor::Human, state.path(), None).expect("prepare");
        let info = app.info();
        // Read the token the way a client does: from the state directory.
        let token = std::fs::read_to_string(&info.token_path).expect("the token file");
        Self {
            token: token.trim().to_string(),
            id: info.id,
            app,
            _state: state,
            _repo: repo,
        }
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
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("the body reads")
            .to_bytes();
        let body = if bytes.is_empty() {
            None
        } else {
            Some(serde_json::from_slice(&bytes).expect("the body is JSON"))
        };
        Reply { status, body }
    }

    /// Adds one task and returns the reply.
    async fn add_task(&self, id: &str) -> Reply {
        self.send(
            Call::post("/api/action", task_add(id))
                .with_token(&self.token)
                .with_write_header(),
        )
        .await
    }
}

/// One request under construction.
struct Call {
    method: Method,
    uri: String,
    token: Option<String>,
    write_header: bool,
    origin: Option<String>,
    body: Option<Value>,
}

impl Call {
    fn get(uri: &str) -> Self {
        Self {
            method: Method::GET,
            uri: uri.to_string(),
            token: None,
            write_header: false,
            origin: None,
            body: None,
        }
    }

    fn post(uri: &str, body: Value) -> Self {
        Self {
            method: Method::POST,
            uri: uri.to_string(),
            token: None,
            write_header: false,
            origin: None,
            body: Some(body),
        }
    }

    fn with_token(mut self, token: &str) -> Self {
        self.token = Some(token.to_string());
        self
    }

    fn with_write_header(mut self) -> Self {
        self.write_header = true;
        self
    }

    fn with_origin(mut self, origin: &str) -> Self {
        self.origin = Some(origin.to_string());
        self
    }

    fn request(&self) -> Request<Body> {
        let mut builder = Request::builder()
            .method(self.method.clone())
            .uri(&self.uri);
        if let Some(token) = &self.token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
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
}

impl Reply {
    fn json(&self) -> &Value {
        self.body.as_ref().expect("the reply carries a body")
    }

    fn code(&self) -> &str {
        self.json()["code"]
            .as_str()
            .expect("the reply names a code")
    }
}

/// Appends a run start to `repo`'s log, the way `shprd up` does.
fn start_run(repo: &std::path::Path) {
    let path = repo.join(".shprd").join("events.jsonl");
    let mut log = EventLog::open(&path).expect("the log opens");
    log.append(
        Actor::Human,
        EventKind::RunStarted {
            run_id: "r1".to_string(),
            repo: repo.display().to_string(),
            base_branch: "main".to_string(),
            integration_branch: "integration/m8".to_string(),
        },
        &SystemClock,
    )
    .expect("the run starts");
}

/// The body of a `task.add` action.
fn task_add(id: &str) -> Value {
    json!({
        "kind": "task.add",
        "task": id,
        "title": format!("task {id}"),
        "scope": ["crates/shprd-core"],
        "acceptance": ["it works"],
    })
}

#[tokio::test]
async fn health_answers_the_handshake_without_a_token() {
    let fixture = Fixture::new();
    let reply = fixture.send(Call::get("/api/health")).await;
    assert_eq!(reply.status, StatusCode::OK);
    let body = reply.json();
    assert_eq!(body["ok"], json!(true));
    assert_eq!(body["id"], json!(fixture.id), "the id is the handshake");
    assert_eq!(body["version"], json!(shprd_server::VERSION));
}

#[tokio::test]
async fn every_other_route_needs_the_token() {
    let fixture = Fixture::new();
    for uri in ["/api/state", "/api/changes?since=0", "/api/ws"] {
        let reply = fixture.send(Call::get(uri)).await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{uri}");
        assert_eq!(reply.code(), "unauthorized", "{uri}");
    }

    let wrong = fixture
        .send(Call::get("/api/state").with_token("not-the-token"))
        .await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);

    let right = fixture
        .send(Call::get("/api/state").with_token(&fixture.token))
        .await;
    assert_eq!(right.status, StatusCode::OK);
}

#[tokio::test]
async fn an_action_write_needs_the_custom_header() {
    let fixture = Fixture::new();
    let reply = fixture
        .send(Call::post("/api/action", task_add("T1")).with_token(&fixture.token))
        .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert_eq!(reply.code(), "missing_header");

    let added = fixture.add_task("T1").await;
    assert_eq!(added.status, StatusCode::OK);
}

#[tokio::test]
async fn a_request_from_another_origin_is_refused() {
    let fixture = Fixture::new();
    let foreign = fixture
        .send(
            Call::get("/api/state")
                .with_token(&fixture.token)
                .with_origin("https://evil.example"),
        )
        .await;
    assert_eq!(foreign.status, StatusCode::FORBIDDEN);
    assert_eq!(foreign.code(), "cross_origin");

    let own = fixture
        .send(
            Call::get("/api/state")
                .with_token(&fixture.token)
                .with_origin(&format!("http://127.0.0.1:{PORT}")),
        )
        .await;
    assert_eq!(own.status, StatusCode::OK);
}

#[tokio::test]
async fn the_snapshot_is_the_fold() {
    let fixture = Fixture::new();
    let empty = fixture
        .send(Call::get("/api/state").with_token(&fixture.token))
        .await;
    assert_eq!(empty.status, StatusCode::OK);
    assert_eq!(empty.json()["tasks"], json!([]));
    assert_eq!(
        empty.json()["seq"],
        json!(1),
        "the run start is the first event"
    );
    assert_eq!(empty.json()["run"]["id"], json!("r1"));

    fixture.add_task("T1").await;
    let after = fixture
        .send(Call::get("/api/state").with_token(&fixture.token))
        .await;
    let snapshot = after.json();
    assert_eq!(snapshot["seq"], json!(2));
    let tasks = snapshot["tasks"].as_array().expect("tasks is an array");
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0]["id"], json!("T1"));
    assert_eq!(tasks[0]["status"], json!("queued"));
    assert_eq!(tasks[0]["acceptance"], json!(["it works"]));
}

#[tokio::test]
async fn a_long_poll_wakes_up_on_a_new_event() {
    let fixture = Fixture::new();
    let started = Instant::now();
    let token = fixture.token.clone();
    let poll = tokio::spawn({
        let router = fixture.app.router();
        async move {
            let request = Call::get("/api/changes?since=1&timeout=5")
                .with_token(&token)
                .request();
            let response = router.oneshot(request).await.expect("the poll answers");
            let status = response.status();
            let bytes = response
                .into_body()
                .collect()
                .await
                .expect("the body reads")
                .to_bytes();
            (status, bytes)
        }
    });

    // Let the poll start waiting before the event lands.
    tokio::time::sleep(Duration::from_millis(150)).await;
    fixture.add_task("T1").await;

    let (status, bytes) = tokio::time::timeout(Duration::from_secs(4), poll)
        .await
        .expect("the poll woke up")
        .expect("the poll task finished");
    assert_eq!(status, StatusCode::OK);
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "a long poll answers as soon as the event lands"
    );
    let change: Value = serde_json::from_slice(&bytes).expect("the change is JSON");
    assert_eq!(change["seq"], json!(2));
    let events = change["events"].as_array().expect("events is an array");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["kind"], json!("task_added"));
    assert_eq!(events[0]["task"], json!("T1"));
}

#[tokio::test]
async fn a_long_poll_times_out_with_no_content() {
    let fixture = Fixture::new();
    let reply = fixture
        .send(Call::get("/api/changes?since=1&timeout=0").with_token(&fixture.token))
        .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    assert!(reply.body.is_none());
}

#[tokio::test]
async fn an_action_round_trips() {
    let fixture = Fixture::new();
    let added = fixture.add_task("T1").await;
    assert_eq!(added.status, StatusCode::OK);
    let body = added.json();
    assert_eq!(body["ok"], json!(true));
    assert_eq!(body["seq"], json!(2));
    assert_eq!(body["task"]["id"], json!("T1"));
    assert_eq!(body["task"]["status"], json!("queued"));

    // `task.status` is the CLI's write verb: it appends a progress event, and
    // the reply carries the line it recorded.
    let status = fixture
        .send(
            Call::post(
                "/api/action",
                json!({"kind": "task.status", "task": "T1", "msg": "halfway there", "progress": 50}),
            )
            .with_token(&fixture.token)
            .with_write_header(),
        )
        .await;
    assert_eq!(status.status, StatusCode::OK);
    assert_eq!(status.json()["task"]["id"], json!("T1"));
    assert_eq!(
        status.json()["task"]["status"],
        json!("queued"),
        "a progress line moves no status"
    );
    assert_eq!(
        status.json()["seq"],
        json!(3),
        "the write appended a second event"
    );
    assert_eq!(
        status.json()["task"]["last_status"]["message"],
        json!("halfway there")
    );
    assert_eq!(status.json()["task"]["last_status"]["progress"], json!(50));
    assert_eq!(status.json()["task"]["last_status"]["seq"], json!(3));

    // The snake_case spelling is the same action, and progress is optional.
    let snake = fixture
        .send(
            Call::post(
                "/api/action",
                json!({"kind": "task_status", "task": "T1", "msg": "another line"}),
            )
            .with_token(&fixture.token)
            .with_write_header(),
        )
        .await;
    assert_eq!(snake.status, StatusCode::OK);
    assert_eq!(snake.json()["task"]["id"], json!("T1"));
    assert_eq!(
        snake.json()["task"]["last_status"]["message"],
        json!("another line")
    );
    assert_eq!(snake.json()["task"]["last_status"]["progress"], Value::Null);
    assert_eq!(snake.json()["seq"], json!(4));
}

#[tokio::test]
async fn a_progress_line_is_capped_and_reaches_the_snapshot_and_the_poll() {
    let fixture = Fixture::new();
    fixture.add_task("T1").await;
    let long = "a pasted agent message that goes on and on and on for far too long";
    let written = fixture
        .send(
            Call::post(
                "/api/action",
                json!({"kind": "task.status", "task": "T1", "msg": long, "progress": 12}),
            )
            .with_token(&fixture.token)
            .with_write_header(),
        )
        .await;
    assert_eq!(written.status, StatusCode::OK);
    let message = written.json()["task"]["last_status"]["message"]
        .as_str()
        .expect("a message")
        .to_string();
    assert!(
        message.chars().count() <= shprd_core::event::TASK_STATUS_MAX_CHARS,
        "the line is capped, not stored whole: {message:?}"
    );
    assert!(message.ends_with('\u{2026}'), "{message:?}");

    let snapshot = fixture
        .send(Call::get("/api/state").with_token(&fixture.token))
        .await;
    let task = &snapshot.json()["tasks"][0];
    assert_eq!(task["last_status"]["progress"], json!(12));
    assert_eq!(task["last_status"]["message"], Value::from(message.clone()));

    let change = fixture
        .send(Call::get("/api/changes?since=2&timeout=0").with_token(&fixture.token))
        .await;
    assert_eq!(change.status, StatusCode::OK);
    assert_eq!(change.json()["events"][0]["kind"], json!("task_status"));
    assert_eq!(change.json()["events"][0]["task"], json!("T1"));
}

#[tokio::test]
async fn a_status_line_with_no_msg_or_a_bad_percentage_is_refused() {
    let fixture = Fixture::new();
    fixture.add_task("T1").await;
    for body in [
        json!({"kind": "task.status", "task": "T1"}),
        json!({"kind": "task.status", "task": "T1", "msg": "   "}),
        json!({"kind": "task.status", "task": "T1", "msg": "ok", "progress": 101}),
    ] {
        let reply = fixture
            .send(
                Call::post("/api/action", body.clone())
                    .with_token(&fixture.token)
                    .with_write_header(),
            )
            .await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(reply.code(), "bad_request", "{body}");
    }
    let snapshot = fixture
        .send(Call::get("/api/state").with_token(&fixture.token))
        .await;
    assert_eq!(snapshot.json()["seq"], json!(2), "nothing was appended");
}

#[tokio::test]
async fn an_action_runs_the_binarys_after_write_step() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let state = tempfile::tempdir().expect("state dir");
    let repo = tempfile::tempdir().expect("repo dir");
    start_run(repo.path());
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let app = App::prepare(
        repo.path(),
        PORT,
        &Actor::Human,
        state.path(),
        Some(shprd_server::AfterWrite::new(move |_repo| {
            counted.fetch_add(1, Ordering::SeqCst);
        })),
    )
    .expect("prepare");
    let token = std::fs::read_to_string(app.info().token_path).expect("the token file");
    let token = token.trim().to_string();

    let call = Call::post(
        "/api/action",
        json!({
            "kind": "task.add", "task": "T1", "title": "t",
            "scope": ["crates/a"], "acceptance": ["works"],
        }),
    )
    .with_token(&token)
    .with_write_header();
    let response = app.router().oneshot(call.request()).await.expect("answers");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the step ran once, after the write"
    );

    // A refused write does not run it.
    let call = Call::post(
        "/api/action",
        json!({
            "kind": "task.add", "task": "T1", "title": "t",
            "scope": ["crates/a"], "acceptance": ["works"],
        }),
    )
    .with_token(&token)
    .with_write_header();
    let response = app.router().oneshot(call.request()).await.expect("answers");
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn an_unknown_action_kind_is_a_typed_refusal() {
    let fixture = Fixture::new();
    let reply = fixture
        .send(
            Call::post("/api/action", json!({"kind": "task.delete", "task": "T1"}))
                .with_token(&fixture.token)
                .with_write_header(),
        )
        .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert_eq!(reply.code(), "unknown_action");
    let message = reply.json()["message"].as_str().expect("a message");
    assert!(message.contains("task_add"), "{message}");
    assert!(message.contains("task_status"), "{message}");
}

#[tokio::test]
async fn a_malformed_action_is_a_typed_refusal() {
    let fixture = Fixture::new();
    for body in [
        json!({"task": "T1"}),
        json!({"kind": "task.add"}),
        json!({"kind": "task_add", "task": "T1", "title": "t", "scope": "not-a-list"}),
    ] {
        let reply = fixture
            .send(
                Call::post("/api/action", body.clone())
                    .with_token(&fixture.token)
                    .with_write_header(),
            )
            .await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(reply.code(), "bad_request", "{body}");
    }
}

#[tokio::test]
async fn a_refused_action_writes_nothing() {
    let fixture = Fixture::new();
    fixture.add_task("T1").await;
    let duplicate = fixture.add_task("T1").await;
    assert_eq!(duplicate.status, StatusCode::CONFLICT);
    assert_eq!(duplicate.code(), "refused");

    let snapshot = fixture
        .send(Call::get("/api/state").with_token(&fixture.token))
        .await;
    assert_eq!(snapshot.json()["seq"], json!(2), "nothing was appended");
    assert_eq!(
        snapshot.json()["tasks"].as_array().expect("an array").len(),
        1
    );
}

#[tokio::test]
async fn recording_status_for_an_unknown_task_is_a_typed_refusal() {
    let fixture = Fixture::new();
    let reply = fixture
        .send(
            Call::post(
                "/api/action",
                json!({"kind": "task.status", "task": "nope", "msg": "here"}),
            )
            .with_token(&fixture.token)
            .with_write_header(),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CONFLICT);
    assert_eq!(reply.code(), "refused");
    assert!(
        reply.json()["message"]
            .as_str()
            .expect("a message")
            .contains("nope"),
        "the refusal names the task"
    );
}

#[tokio::test]
async fn an_unknown_route_answers_a_typed_404() {
    let fixture = Fixture::new();
    let reply = fixture
        .send(Call::get("/api/nope").with_token(&fixture.token))
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(reply.code(), "not_found");
}

/// A service on a real loopback socket, with its router and its token.
struct Live {
    app: App,
    token: String,
    port: u16,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    server: tokio::task::JoinHandle<()>,
    _state: TempDir,
    _repo: TempDir,
}

impl Live {
    /// Starts a service on a free port, over a repository whose run has started.
    ///
    /// The listener is bound here, before the server task starts, so a client
    /// that connects at once finds a socket rather than a race.
    fn start(limits: Option<WsLimits>) -> Self {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("a free port");
        listener
            .set_nonblocking(true)
            .expect("the listener goes non-blocking");
        let port = listener.local_addr().expect("the bound address").port();
        let state = tempfile::tempdir().expect("state dir");
        let repo = tempfile::tempdir().expect("repo dir");
        start_run(repo.path());
        let mut app =
            App::prepare(repo.path(), port, &Actor::Human, state.path(), None).expect("prepare");
        if let Some(limits) = limits {
            app = app.with_ws_limits(limits);
        }
        let token = std::fs::read_to_string(app.info().token_path).expect("the token file");
        let token = token.trim().to_string();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let server_router = app.router();
        let server = tokio::spawn(async move {
            let listener =
                tokio::net::TcpListener::from_std(listener).expect("the bound listener is usable");
            axum::serve(listener, server_router)
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await
                .expect("the test server runs");
        });
        Self {
            app,
            token,
            port,
            stop: Some(stop),
            server,
            _state: state,
            _repo: repo,
        }
    }

    /// The socket URL, with the token a browser cannot send as a header.
    fn url(&self) -> String {
        format!("ws://127.0.0.1:{}/api/ws?token={}", self.port, self.token)
    }

    /// Connects and reads the hello.
    async fn connect(
        &self,
    ) -> (
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        Value,
    ) {
        let (mut socket, _) = tokio_tungstenite::connect_async(self.url())
            .await
            .expect("the socket connects");
        let hello = next_json(&mut socket).await;
        (socket, hello)
    }

    /// Adds a task through the router, so the socket has a change to push.
    async fn add_task(&self, id: &str) {
        let call = Call::post("/api/action", task_add(id))
            .with_token(&self.token)
            .with_write_header();
        let response = self
            .app
            .router()
            .oneshot(call.request())
            .await
            .expect("answers");
        assert_eq!(response.status(), StatusCode::OK);
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        self.server.abort();
    }
}

#[tokio::test]
async fn the_socket_says_hello_and_pushes_a_change() {
    use tokio_tungstenite::tungstenite::Message;

    let live = Live::start(None);
    let (mut socket, hello) = live.connect().await;
    assert_eq!(hello["type"], json!("hello"));
    assert_eq!(
        hello["seq"],
        json!(1),
        "the hello names the log's last event"
    );
    assert_eq!(hello["id"], json!(live.app.info().id));

    live.add_task("T1").await;
    let change = next_json(&mut socket).await;
    assert_eq!(change["type"], json!("change"));
    assert_eq!(change["change"]["seq"], json!(2));
    assert_eq!(change["change"]["events"][0]["kind"], json!("task_added"));

    // A ping is answered, which is how a page know a quiet socket is alive.
    socket
        .send(Message::Text(
            json!({"type": "ping", "at": 7}).to_string().into(),
        ))
        .await
        .expect("the ping goes out");
    let pong = next_json(&mut socket).await;
    assert_eq!(pong["type"], json!("pong"));
    assert_eq!(pong["at"], json!(7));

    let _ = socket.close(None).await;
}

#[tokio::test]
async fn a_socket_a_client_abandoned_is_closed_rather_than_held_open() {
    let live = Live::start(Some(WsLimits {
        ping: Duration::from_millis(50),
        idle: Duration::from_millis(150),
        session: Duration::from_secs(30),
    }));
    let (mut socket, _) = live.connect().await;

    // The server pings on its own, then gives up on the silence with a clean
    // `1001 going away`, so a probe or a page that vanished cannot hold a
    // connection open for ever.
    let started = Instant::now();
    let closed = loop {
        let frame = tokio::time::timeout(Duration::from_secs(10), socket.next())
            .await
            .expect("the server closes the socket")
            .expect("the socket is open")
            .expect("the frame is a message");
        match frame {
            tokio_tungstenite::tungstenite::Message::Ping(_) => continue,
            tokio_tungstenite::tungstenite::Message::Close(frame) => break frame,
            other => panic!("unexpected frame {other:?}"),
        }
    };
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the idle bound, not the session bound, is what closed it"
    );
    let frame = closed.expect("the close carries a code");
    assert_eq!(
        u16::from(frame.code),
        1001,
        "going away, so the client reconnects"
    );
    assert!(frame.reason.contains("idle"), "{}", frame.reason);
}

#[tokio::test]
async fn a_client_that_keeps_talking_is_closed_at_the_session_limit() {
    let live = Live::start(Some(WsLimits {
        ping: Duration::from_millis(20),
        idle: Duration::from_secs(30),
        session: Duration::from_millis(250),
    }));
    let (mut socket, _) = live.connect().await;

    // Answering every ping is not enough to keep the socket: the session bound
    // is a backstop, not a silence timer.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let frame = tokio::time::timeout_at(deadline, socket.next())
            .await
            .expect("the server closes the socket")
            .expect("the socket is open")
            .expect("the frame is a message");
        match frame {
            tokio_tungstenite::tungstenite::Message::Ping(payload) => {
                socket
                    .send(tokio_tungstenite::tungstenite::Message::Pong(payload))
                    .await
                    .expect("the pong goes out");
            }
            tokio_tungstenite::tungstenite::Message::Close(frame) => {
                let frame = frame.expect("the close carries a code");
                assert_eq!(u16::from(frame.code), 1001);
                assert!(frame.reason.contains("session"), "{}", frame.reason);
                return;
            }
            other => panic!("unexpected frame {other:?}"),
        }
    }
}

#[test]
fn a_second_service_for_one_state_directory_is_refused() {
    let state = tempfile::tempdir().expect("state dir");
    let repo = tempfile::tempdir().expect("repo dir");
    let first = App::prepare(repo.path(), PORT, &Actor::Human, state.path(), None)
        .expect("the first service takes the directory");
    let record = std::fs::read_to_string(first.info().service_json).expect("the record");

    match App::prepare(repo.path(), PORT, &Actor::Human, state.path(), None) {
        Err(shprd_server::ServerError::AlreadyRunning { port, .. }) => {
            assert_eq!(port, Some(PORT), "the refusal names the running service");
        }
        Err(other) => panic!("expected AlreadyRunning, got {other}"),
        Ok(_) => panic!("a second service must not share a state directory"),
    }
    assert_eq!(
        std::fs::read_to_string(first.info().service_json).expect("the record"),
        record,
        "the running service's record is untouched"
    );

    // Stopping the first frees the directory for the next start.
    first.remove_record().expect("remove");
    drop(first);
    App::prepare(repo.path(), PORT, &Actor::Human, state.path(), None)
        .expect("a stopped service leaves the directory free");
}

#[test]
fn stopping_a_service_keeps_a_record_another_one_owns() {
    let state = tempfile::tempdir().expect("state dir");
    let repo = tempfile::tempdir().expect("repo dir");
    let app = App::prepare(repo.path(), PORT, &Actor::Human, state.path(), None).expect("prepare");
    let path = app.info().service_json;
    let foreign = serde_json::json!({
        "port": 1, "id": "another-service", "pid": 1, "version": shprd_server::VERSION,
    });
    std::fs::write(&path, foreign.to_string()).expect("overwrite the record");
    app.remove_record().expect("a clean stop");
    assert!(
        path.exists(),
        "a record another service owns must survive this one's stop"
    );
}

#[tokio::test]
async fn a_request_that_is_not_an_upgrade_is_a_typed_refusal() {
    let fixture = Fixture::new();
    let reply = fixture
        .send(Call::get("/api/ws").with_token(&fixture.token))
        .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert_eq!(reply.code(), "bad_request");
    let message = reply.json()["message"].as_str().expect("a message");
    assert!(
        message.contains("/api/changes"),
        "the refusal points at the poll: {message}"
    );
    assert!(
        message.contains("upgrade") || message.contains("Upgrade"),
        "the refusal says what was missing: {message}"
    );
}

/// Reads the next text frame as JSON, failing after three seconds.
async fn next_json<S>(socket: &mut S) -> Value
where
    S: Sink<tokio_tungstenite::tungstenite::Message>
        + Stream<
            Item = Result<
                tokio_tungstenite::tungstenite::Message,
                tokio_tungstenite::tungstenite::Error,
            >,
        > + Unpin,
{
    let message = tokio::time::timeout(Duration::from_secs(3), socket.next())
        .await
        .expect("a message arrives")
        .expect("the socket is open")
        .expect("the frame is a message");
    let text = message.into_text().expect("a text frame");
    serde_json::from_str(text.as_str()).expect("the frame is JSON")
}
