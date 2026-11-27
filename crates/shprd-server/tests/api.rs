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

/// The `Host` every fixture request is addressed to.
///
/// The gate refuses a request with no `Host` (`421`), so a test that drove the
/// router without one would be testing the refusal, not the route.
const HOST: &str = "127.0.0.1:18080";

/// The port the fixtures claim in their `Origin` and `service.json`.
const PORT: u16 = 18_080;

/// One prepared service, with the temp dirs it lives in kept alive.
struct Fixture {
    app: App,
    token: String,
    id: String,
    state: TempDir,
    repo: TempDir,
}

impl Fixture {
    /// A service over a repository whose run has just started.
    ///
    /// The run start is the log's first event and every rule needs it, so it is
    /// written before the service is prepared, exactly as `shprd up` writes it.
    fn new() -> Self {
        let repo = tempfile::tempdir().expect("repo dir");
        start_run(repo.path());
        Self::prepared(repo, tempfile::tempdir().expect("state dir"))
    }

    /// A service over a repository with no run in its log at all.
    fn without_a_run() -> Self {
        let repo = tempfile::tempdir().expect("repo dir");
        Self::prepared(repo, tempfile::tempdir().expect("state dir"))
    }

    /// A service over `repo` (already holding whatever log the test wrote) with
    /// its discovery files in `state`.
    fn prepared(repo: TempDir, state: TempDir) -> Self {
        let app =
            App::prepare(repo.path(), PORT, &Actor::Human, state.path(), None).expect("prepare");
        let info = app.info();
        // Read the token the way a client does: from the state directory.
        let token = std::fs::read_to_string(&info.token_path).expect("the token file");
        Self {
            token: token.trim().to_string(),
            id: info.id,
            app,
            state,
            repo,
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
            .uri(&self.uri)
            .header(header::HOST, HOST);
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
async fn a_board_with_no_run_says_what_to_do() {
    // Bug 7 of the M9 gate: a service started before `shprd run start` answered
    // a board with no run in it, which looks exactly like a run with no tasks.
    // It says so now, and names the one verb that fixes it.
    let fixture = Fixture::without_a_run();
    let reply = fixture
        .send(Call::get("/api/state").with_token(&fixture.token))
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    let snapshot = reply.json();
    assert!(snapshot["run"].is_null(), "there is no run: {snapshot}");
    assert_eq!(snapshot["hint"], json!(shprd_server::NO_RUN_HINT));

    // A run started after the service was up shows without a restart: the
    // snapshot is folded from the log on every request, never cached.
    start_run(fixture.repo.path());
    let after = fixture
        .send(Call::get("/api/state").with_token(&fixture.token))
        .await;
    let snapshot = after.json();
    assert_eq!(snapshot["run"]["id"], json!("r1"));
    assert!(
        snapshot.get("hint").is_none(),
        "a board that can advance needs no hint: {snapshot}"
    );
}

#[tokio::test]
async fn a_failed_pass_is_on_the_board() {
    // A pass that fails a step goes on with the others and leaves the board as
    // it was, so the failure has to be carried beside it. This is the shape
    // `/api/state` answers with after a pass like that.
    let fixture = Fixture::new();
    let report = shprd_server::TickReport {
        run: Some(String::from("r1")),
        steps: vec![
            shprd_server::StepReport {
                action: String::from("start"),
                task: String::from("T1"),
                ok: false,
                detail: String::from("no workspace \"shprd · repo\" yet"),
                seq: None,
            },
            shprd_server::StepReport {
                action: String::from("start"),
                task: String::from("T2"),
                ok: true,
                detail: String::from("started T2"),
                seq: Some(3),
            },
        ],
        news: Vec::new(),
        interval_secs: None,
    };
    let last = shprd_server::LastPass::of(&report, fixture.repo.path(), 1_700_000_000_000);
    last.write(fixture.state.path())
        .expect("the pass is recorded");

    let reply = fixture
        .send(Call::get("/api/state").with_token(&fixture.token))
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    let snapshot = reply.json();
    assert_eq!(snapshot["last_tick"]["at_ms"], json!(1_700_000_000_000u64));
    assert_eq!(snapshot["last_tick"]["steps"], json!(2));
    assert_eq!(
        snapshot["last_tick"]["failed"],
        json!([{
            "task": "T1",
            "step": "start",
            "error": "no workspace \"shprd · repo\" yet",
        }]),
        "the step's own error reaches the board: {snapshot}"
    );
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
async fn dismissing_a_card_removes_it_and_an_unknown_one_is_refused() {
    let fixture = Fixture::new();
    fixture.add_task("T1").await;
    {
        let path = fixture.repo.path().join(".shprd").join("events.jsonl");
        let mut log = EventLog::open(&path).expect("the log opens");
        log.append(
            Actor::Engine,
            EventKind::Escalated {
                task: Some("T1".to_string()),
                body: "which branch?".to_string(),
                default_action: "use main".to_string(),
            },
            &SystemClock,
        )
        .expect("the card is raised");
    }
    let needs = |reply: &Reply| reply.json()["needs"].as_array().expect("needs").clone();
    let state = fixture
        .send(Call::get("/api/state").with_token(&fixture.token))
        .await;
    let seq = needs(&state)[0]["seq"].as_u64().expect("a seq");

    let unknown = fixture
        .send(
            Call::post("/api/action", json!({"kind": "need.dismiss", "seq": 9999}))
                .with_token(&fixture.token)
                .with_write_header(),
        )
        .await;
    assert_ne!(unknown.status, StatusCode::OK);
    assert!(
        unknown.json()["message"]
            .as_str()
            .is_some_and(|m| m.contains("no card number 9999")),
        "{:?}",
        unknown.json()
    );

    let done = fixture
        .send(
            Call::post("/api/action", json!({"kind": "need.dismiss", "seq": seq}))
                .with_token(&fixture.token)
                .with_write_header(),
        )
        .await;
    assert_eq!(done.status, StatusCode::OK, "{:?}", done.body);
    let after = fixture
        .send(Call::get("/api/state").with_token(&fixture.token))
        .await;
    assert!(needs(&after).is_empty(), "the card is gone");
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
async fn the_graph_route_says_which_probe_found_dagr() {
    use shprd_server::graph::{Located, Origin};

    let repo = tempfile::tempdir().expect("repo dir");
    start_run(repo.path());
    let state = tempfile::tempdir().expect("state dir");
    let app = App::prepare(repo.path(), PORT, &Actor::Human, state.path(), None)
        .expect("prepare")
        .with_dagr(shprd_server::Dagr::Found(Located {
            path: std::path::PathBuf::from("/opt/dagr"),
            origin: Origin::Setting,
        }));
    let token = std::fs::read_to_string(app.info().token_path).expect("the token file");
    let token = token.trim().to_string();
    let response = app
        .router()
        .oneshot(Call::get("/api/graph").with_token(&token).request())
        .await
        .expect("answers");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("the body reads")
        .to_bytes();
    let body: Value = serde_json::from_slice(&bytes).expect("the body is JSON");

    let run_json = repo.path().join(".dagr").join("run.json");
    assert_eq!(body["repo"], json!(repo.path().display().to_string()));
    assert_eq!(body["run_json"], json!(run_json.display().to_string()));
    assert_eq!(body["run"], json!("r1"), "the run the board is showing");
    assert_eq!(body["dagr"]["path"], json!("/opt/dagr"));
    assert_eq!(body["dagr"]["origin"], json!("setting"));
    assert_eq!(
        body["argv"],
        json!(["/opt/dagr", "view", run_json.display().to_string(),]),
        "the block runs dagr's own follow mode on the projected document"
    );
    assert_eq!(body["setting"], json!("graph.dagr"));
    assert_eq!(body["message"], Value::Null);
}

#[tokio::test]
async fn a_graph_with_no_dagr_names_the_setting_to_fix() {
    use shprd_server::graph::Missing;

    let repo = tempfile::tempdir().expect("repo dir");
    let state = tempfile::tempdir().expect("state dir");
    // No run: the graph must still answer, because the block renders before a
    // run exists.
    let app = App::prepare(repo.path(), PORT, &Actor::Human, state.path(), None)
        .expect("prepare")
        .with_dagr(shprd_server::Dagr::Missing(Missing::NotFound));
    let token = std::fs::read_to_string(app.info().token_path).expect("the token file");
    let token = token.trim().to_string();
    let response = app
        .router()
        .oneshot(Call::get("/api/graph").with_token(&token).request())
        .await
        .expect("answers");
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("the body reads")
        .to_bytes();
    let body: Value = serde_json::from_slice(&bytes).expect("the body is JSON");

    assert_eq!(body["dagr"], Value::Null);
    assert_eq!(body["argv"], json!([]), "nothing to run");
    assert_eq!(body["run"], Value::Null, "no run has started");
    let message = body["message"].as_str().expect("the refusal is in words");
    assert!(message.contains("graph.dagr"), "{message}");
    assert!(message.contains("dagr was not found"), "{message}");
}

#[tokio::test]
async fn a_service_write_rewrites_the_graph_document_atomically() {
    let repo = tempfile::tempdir().expect("repo dir");
    start_run(repo.path());
    let state = tempfile::tempdir().expect("state dir");
    // The binary's own after-write step, at the level a route can prove: project
    // and write through the one atomic writer `shprd dagr sync` uses.
    let app = App::prepare(
        repo.path(),
        PORT,
        &Actor::Human,
        state.path(),
        Some(shprd_server::AfterWrite::new(|repo: &std::path::Path| {
            project_and_write(repo);
        })),
    )
    .expect("prepare");
    let token = std::fs::read_to_string(app.info().token_path).expect("the token file");
    let token = token.trim().to_string();
    let run_json = repo.path().join(".dagr").join("run.json");

    let call = Call::post("/api/action", task_add("T1"))
        .with_token(&token)
        .with_write_header();
    let response = app.router().oneshot(call.request()).await.expect("answers");
    assert_eq!(response.status(), StatusCode::OK);
    let first = read_json(&run_json);
    assert_eq!(
        first["tasks"].as_array().map(Vec::len),
        Some(1),
        "the write reached the document"
    );

    // A second write replaces it whole: a reader never sees half a document,
    // and no temporary file is left behind.
    let call = Call::post("/api/action", task_add("T2"))
        .with_token(&token)
        .with_write_header();
    let response = app.router().oneshot(call.request()).await.expect("answers");
    assert_eq!(response.status(), StatusCode::OK);
    let second = read_json(&run_json);
    assert_eq!(second["tasks"].as_array().map(Vec::len), Some(2));
    assert_ne!(first, second, "the document moved with the log");

    let leftovers: Vec<String> = std::fs::read_dir(repo.path().join(".dagr"))
        .expect("the graph folder")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name != "run.json")
        .collect();
    assert!(leftovers.is_empty(), "temporary files left: {leftovers:?}");
}

#[tokio::test]
async fn the_graph_run_route_answers_the_document_the_block_draws() {
    let repo = tempfile::tempdir().expect("repo dir");
    start_run(repo.path());
    let state = tempfile::tempdir().expect("state dir");
    let app = App::prepare(
        repo.path(),
        PORT,
        &Actor::Human,
        state.path(),
        Some(shprd_server::AfterWrite::new(|repo: &std::path::Path| {
            project_and_write(repo);
        })),
    )
    .expect("prepare");
    let token = std::fs::read_to_string(app.info().token_path).expect("the token file");
    let token = token.trim().to_string();

    // One task, with a dependency, so the answer carries an edge as well as a
    // node: the drawing is the page's, but the facts have to be there.
    let mut second = task_add("T2");
    second["deps"] = json!(["T1"]);
    for body in [task_add("T1"), second] {
        let response = app
            .router()
            .oneshot(
                Call::post("/api/action", body)
                    .with_token(&token)
                    .with_write_header()
                    .request(),
            )
            .await
            .expect("answers");
        assert_eq!(response.status(), StatusCode::OK);
    }

    // The gate is in front of it, like every other read.
    let refused = app
        .router()
        .oneshot(Call::get("/api/graph/run").request())
        .await
        .expect("answers");
    assert_eq!(refused.status(), StatusCode::UNAUTHORIZED);

    let response = app
        .router()
        .oneshot(Call::get("/api/graph/run").with_token(&token).request())
        .await
        .expect("answers");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("the body reads")
        .to_bytes();
    let body: Value = serde_json::from_slice(&bytes).expect("the body is JSON");

    let on_disk = read_json(&shprd_server::graph::run_json(repo.path()));
    assert_eq!(body, on_disk, "the answer is the file, byte for byte");
    assert_eq!(body["run"]["id"], json!("r1"));
    let tasks = body["tasks"].as_array().expect("tasks");
    assert_eq!(tasks.len(), 2);
    assert_eq!(
        tasks[1]["deps"],
        json!(["T1"]),
        "the edge is in the document"
    );
}

#[tokio::test]
async fn a_graph_run_with_no_document_is_a_typed_404() {
    let fixture = Fixture::new();
    let reply = fixture
        .send(Call::get("/api/graph/run").with_token(&fixture.token))
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(reply.code(), "not_found");
    let message = reply.json()["message"].as_str().expect("a message");
    assert!(message.contains("run.json"), "{message}");
    assert!(message.contains("no run document"), "{message}");
}

#[tokio::test]
async fn a_file_that_is_not_a_run_document_is_refused_not_drawn() {
    let fixture = Fixture::new();
    let graph = fixture.repo.path().join(".dagr");
    std::fs::create_dir_all(&graph).expect("the graph folder");
    std::fs::write(graph.join("run.json"), "{\"hello\": 1}").expect("the file");

    let reply = fixture
        .send(Call::get("/api/graph/run").with_token(&fixture.token))
        .await;
    assert_eq!(reply.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(reply.code(), "internal");
    let message = reply.json()["message"].as_str().expect("a message");
    assert!(message.contains("not a dagr run document"), "{message}");
}

/// What `shprd dagr sync` does after a write: fold the log, project it and write
/// the document through the one atomic writer.
fn project_and_write(repo: &std::path::Path) {
    let outcome = EventLog::read_all(&repo.join(".shprd").join("events.jsonl"))
        .unwrap_or_else(|err| panic!("the log reads: {err}"));
    let folded = shprd_core::state::RunState::fold(&outcome.events)
        .unwrap_or_else(|err| panic!("the log folds: {err}"));
    let doc = shprd_views::dagr::project(&folded, &outcome.events, 1_763_200_000_000)
        .unwrap_or_else(|err| panic!("the run projects: {err}"));
    shprd_views::dagr::write_atomic(&doc, &shprd_server::graph::run_json(repo), None)
        .unwrap_or_else(|err| panic!("the document writes: {err}"));
}

/// One JSON file, read as a value.
fn read_json(path: &std::path::Path) -> Value {
    let text = std::fs::read_to_string(path).unwrap_or_else(|err| {
        panic!("reading {}: {err}", path.display());
    });
    serde_json::from_str(&text).unwrap_or_else(|err| {
        panic!("{} is not JSON: {err}", path.display());
    })
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
    // A drop with no reason says what is missing, not serde's own text.
    let drop = fixture
        .send(
            Call::post("/api/action", json!({"kind": "task.drop", "task": "T1"}))
                .with_token(&fixture.token)
                .with_write_header(),
        )
        .await;
    let message = drop.json()["message"]
        .as_str()
        .expect("a message")
        .to_string();
    assert!(message.contains("needs a reason"), "{message}");
    assert!(!message.contains("missing field"), "{message}");
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

/// A repository whose task `T7` failed its review twice: the rounds are spent
/// and the card (`review-out-2`) is open.
fn spent_review_repo() -> TempDir {
    use shprd_core::event::{CheckReport, Finding, ReportStatus, ReviewSource, Severity};
    let repo = tempfile::tempdir().expect("repo dir");
    start_run(repo.path());
    let path = repo.path().join(".shprd").join("events.jsonl");
    let mut log = EventLog::open(&path).expect("the log opens");
    put(
        &mut log,
        Actor::Human,
        EventKind::TaskAdded {
            task: "T7".into(),
            title: "t7".into(),
            scope: vec!["tcalc".into()],
            acceptance: vec!["it works".into()],
            tags: Vec::new(),
            skills: Vec::new(),
            deps: Vec::new(),
        },
    );
    for attempt in 1..=2u32 {
        put(
            &mut log,
            Actor::Engine,
            EventKind::Spawned {
                task: "T7".into(),
                attempt,
                agent: "claude".into(),
                model: None,
                worktree: "/wt/T7".into(),
                branch: "shprd/r1/T7".into(),
            },
        );
        put(
            &mut log,
            Actor::Agent {
                id: "claude".into(),
            },
            EventKind::Reported {
                task: "T7".into(),
                attempt,
                status: ReportStatus::Completed,
                summary: "done".into(),
                tests: Vec::new(),
                checks: vec![CheckReport {
                    gate: "tests".into(),
                    command: "cargo test".into(),
                    exit_code: Some(0),
                    output_tail: String::new(),
                    pre_existing: None,
                }],
                changed_paths: Vec::new(),
                commit: Some("abc123".into()),
                skills_used: Vec::new(),
                proofs: Vec::new(),
            },
        );
        let request_seq = put_review_requested(&mut log, attempt);
        put(
            &mut log,
            Actor::Engine,
            EventKind::GateFailed {
                task: "T7".into(),
                attempt,
                gate: "review".into(),
                findings: vec![Finding {
                    severity: Severity::Major,
                    title: "outside scope".into(),
                    location: None,
                    detail: String::new(),
                }],
                source: Some(ReviewSource {
                    reviewer: "codex".into(),
                    findings_sha256:
                        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".into(),
                    request_seq,
                }),
            },
        );
    }
    let ask = shprd_core::gate::rounds_spent_ask(2);
    let body = "T7: its review of attempt 2 failed 2 time(s) and no round is left".to_string();
    log.append(
        Actor::Engine,
        EventKind::AskRaised {
            task: "T7".into(),
            ask: ask.clone(),
            ask_kind: shprd_core::ask::AskKind::Question,
            text: body.clone(),
            options: shprd_core::gate::ROUNDS_SPENT_OPTIONS
                .iter()
                .map(|choice| (*choice).to_string())
                .collect(),
            created_at_ms: 1,
            expires_at_ms: None,
        },
        &SystemClock,
    )
    .expect("raise");
    shprd_core::ask::AskQueue::for_repo(repo.path())
        .raise(
            &shprd_core::ask::Ask::new(ask, "T7", shprd_core::ask::AskKind::Question, body, 1)
                .with_options(
                    shprd_core::gate::ROUNDS_SPENT_OPTIONS
                        .iter()
                        .map(|choice| (*choice).to_string())
                        .collect(),
                ),
        )
        .expect("the ask file");
    repo
}

/// Appends one event to `log`.
fn put(log: &mut EventLog, actor: Actor, kind: EventKind) {
    log.append(actor, kind, &SystemClock).expect("append");
}

/// Records a review request for `attempt` and returns its seq.
fn put_review_requested(log: &mut EventLog, attempt: u32) -> u64 {
    log.append(
        Actor::Engine,
        EventKind::ReviewRequested {
            task: "T7".into(),
            attempt,
            reviewer: "codex".into(),
            degraded: false,
            note: None,
        },
        &SystemClock,
    )
    .expect("request")
    .seq
}

/// Answers the spent-review card and returns the folded run.
async fn answer_spent_card(answer: &str) -> (Fixture, shprd_core::state::RunState) {
    let fixture = Fixture::prepared(spent_review_repo(), tempfile::tempdir().expect("state"));
    let reply = fixture
        .send(
            Call::post(
                "/api/asks/answer",
                json!({"task": "T7", "ask": "review-out-2", "answer": answer}),
            )
            .with_token(&fixture.token)
            .with_write_header(),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
    let events = EventLog::read_all(&fixture.repo.path().join(".shprd").join("events.jsonl"))
        .expect("read the log")
        .events;
    let state = shprd_core::state::RunState::fold(&events).expect("folds");
    (fixture, state)
}

#[tokio::test]
async fn taking_a_spent_review_as_it_is_records_the_override_that_lets_it_land() {
    let (_fixture, state) = answer_spent_card("take it as it is").await;
    let task = state.tasks.get("T7").expect("T7");
    assert!(
        task.overrides
            .iter()
            .any(|record| record.rule == shprd_core::rules::INTEGRATE_AFTER_FAILED_GATE),
        "the answer is the override: {:?}",
        task.overrides
    );
    assert!(
        shprd_core::rules::ready_to_land(task).is_ok(),
        "the task can now land: {:?}",
        shprd_core::rules::ready_to_land(task)
    );
}

#[tokio::test]
async fn dropping_a_spent_review_drops_the_task() {
    let (_fixture, state) = answer_spent_card("drop it").await;
    assert_eq!(
        state.tasks.get("T7").expect("T7").status,
        shprd_core::state::TaskStatus::Dropped
    );
}

#[tokio::test]
async fn retrying_a_spent_review_records_no_override() {
    let (_fixture, state) = answer_spent_card("retry with another round").await;
    let task = state.tasks.get("T7").expect("T7");
    assert!(task.overrides.is_empty());
    assert_ne!(task.status, shprd_core::state::TaskStatus::Dropped);
}

/// A repository whose `T1` (scope `tcalc/ops.py`) holds the open card an older
/// build raised for a refused hand-in: `hand-in-refused-1-1`, as its log wrote it.
fn refused_hand_in_repo() -> TempDir {
    let repo = tempfile::tempdir().expect("repo dir");
    start_run(repo.path());
    let path = repo.path().join(".shprd").join("events.jsonl");
    let mut log = EventLog::open(&path).expect("the log opens");
    put(
        &mut log,
        Actor::Human,
        EventKind::TaskAdded {
            task: "T1".into(),
            title: "t1".into(),
            scope: vec!["tcalc/ops.py".into()],
            acceptance: vec!["add a test in tests/".into()],
            tags: Vec::new(),
            skills: Vec::new(),
            deps: Vec::new(),
        },
    );
    put(
        &mut log,
        Actor::Engine,
        EventKind::Spawned {
            task: "T1".into(),
            attempt: 1,
            agent: "claude".into(),
            model: None,
            worktree: "/wt/T1".into(),
            branch: "shprd/r1/T1".into(),
        },
    );
    let ask = String::from("hand-in-refused-1-1");
    let body = String::from(
        "T1: the engine refused attempt 1's hand-in, so nothing was recorded: path outside scope",
    );
    let options = vec![
        String::from("widen the scope to tests/test_ops.py"),
        String::from("send it back to a new attempt"),
    ];
    put(
        &mut log,
        Actor::Agent {
            id: "claude".into(),
        },
        EventKind::AskRaised {
            task: "T1".into(),
            ask: ask.clone(),
            ask_kind: shprd_core::ask::AskKind::Question,
            text: body.clone(),
            options: options.clone(),
            created_at_ms: 1,
            expires_at_ms: None,
        },
    );
    shprd_core::ask::AskQueue::for_repo(repo.path())
        .raise(
            &shprd_core::ask::Ask::new(ask, "T1", shprd_core::ask::AskKind::Question, body, 1)
                .with_options(options),
        )
        .expect("the ask file");
    repo
}

#[tokio::test]
async fn an_old_open_scope_card_is_never_listed() {
    let fixture = Fixture::prepared(refused_hand_in_repo(), tempfile::tempdir().expect("state"));
    for uri in ["/api/asks", "/api/state"] {
        let reply = fixture
            .send(Call::get(uri).with_token(&fixture.token))
            .await;
        assert_eq!(reply.status, StatusCode::OK, "{uri}: {:?}", reply.body);
        let text = format!("{:?}", reply.body);
        assert!(
            !text.contains("hand-in-refused-1-1"),
            "{uri} lists the moot card: {text}"
        );
    }
}

#[tokio::test]
async fn review_again_on_a_stuck_review_needs_a_host_and_consumes_nothing_without_one() {
    let repo = refused_hand_in_repo();
    let ask = shprd_core::review::stuck_ask(1, 1);
    let options = shprd_core::review::stuck_options();
    let path = repo.path().join(".shprd").join("events.jsonl");
    let mut log = EventLog::open(&path).expect("open");
    put(
        &mut log,
        Actor::Engine,
        EventKind::AskRaised {
            task: "T1".into(),
            ask: ask.clone(),
            ask_kind: shprd_core::ask::AskKind::Question,
            text: "stuck".into(),
            options: options.clone(),
            created_at_ms: 2,
            expires_at_ms: None,
        },
    );
    shprd_core::ask::AskQueue::for_repo(repo.path())
        .raise(
            &shprd_core::ask::Ask::new(
                ask.clone(),
                "T1",
                shprd_core::ask::AskKind::Question,
                "stuck",
                2,
            )
            .with_options(options),
        )
        .expect("the ask file");
    let fixture = Fixture::prepared(repo, tempfile::tempdir().expect("state"));
    let reply = fixture
        .send(
            Call::post(
                "/api/asks/answer",
                json!({"task": "T1", "ask": ask, "answer": "review again"}),
            )
            .with_token(&fixture.token)
            .with_write_header(),
        )
        .await;
    assert_ne!(reply.status, StatusCode::OK, "{:?}", reply.body);
    let events = EventLog::read_all(&path).expect("read").events;
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.kind, EventKind::AskAnswered { .. })),
        "the card is still open"
    );
}

/// Runs `git` in `dir`, panicking on failure (tests only).
fn git_in(dir: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git runs");
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[tokio::test]
async fn the_first_task_in_a_fresh_repo_starts_a_run() {
    // Major 12 of SHPR-54's QA: `+` in a repo with no run was refused with
    // "no run has started". The add now starts one on the checked-out branch.
    let repo = tempfile::tempdir().expect("repo dir");
    git_in(repo.path(), &["init", "-b", "trunk"]);
    git_in(repo.path(), &["commit", "--allow-empty", "-m", "seed"]);
    let fixture = Fixture::prepared(repo, tempfile::tempdir().expect("state dir"));
    let reply = fixture.add_task("T1").await;
    assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);

    let state = fixture
        .send(Call::get("/api/state").with_token(&fixture.token))
        .await;
    let snapshot = state.json();
    assert!(snapshot["run"].is_object(), "a run started: {snapshot}");
    assert_eq!(snapshot["run"]["base_branch"], json!("trunk"), "{snapshot}");
    let integration = snapshot["run"]["integration_branch"]
        .as_str()
        .expect("an integration branch")
        .to_string();
    let branches = git_in(fixture.repo.path(), &["branch", "--list", &integration]);
    assert!(!branches.is_empty(), "the integration branch was made");

    // A second task adds to the same run.
    let again = fixture.add_task("T2").await;
    assert_eq!(again.status, StatusCode::OK, "{:?}", again.body);
}

#[tokio::test]
async fn a_task_in_a_repo_with_no_commit_is_refused_in_plain_words() {
    let repo = tempfile::tempdir().expect("repo dir");
    git_in(repo.path(), &["init", "-b", "trunk"]);
    let fixture = Fixture::prepared(repo, tempfile::tempdir().expect("state dir"));
    let reply = fixture.add_task("T1").await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{:?}", reply.body);
    let message = reply.json()["message"].as_str().unwrap_or("").to_string();
    assert!(
        message.contains("no commit") || message.contains("commit to base"),
        "{message}"
    );
    assert!(
        !message.contains("shprd run start"),
        "names no command: {message}"
    );
}

/// A repository with task `T1` and an open `propose-drop-1` card on it, the
/// shape `shprd agent propose drop T1 --reason stuck` raises.
fn drop_proposal_repo() -> TempDir {
    let repo = tempfile::tempdir().expect("repo dir");
    start_run(repo.path());
    let path = repo.path().join(".shprd").join("events.jsonl");
    let mut log = EventLog::open(&path).expect("the log opens");
    put(
        &mut log,
        Actor::Human,
        EventKind::TaskAdded {
            task: "T1".into(),
            title: "t1".into(),
            scope: vec!["src".into()],
            acceptance: vec!["it works".into()],
            tags: Vec::new(),
            skills: Vec::new(),
            deps: Vec::new(),
        },
    );
    let ask = "propose-drop-1".to_string();
    let body = "T1: run `drop`?\nreason: stuck".to_string();
    let options = vec!["confirm drop".to_string(), "leave it".to_string()];
    put(
        &mut log,
        Actor::Agent {
            id: "claude".into(),
        },
        EventKind::AskRaised {
            task: "T1".into(),
            ask: ask.clone(),
            ask_kind: shprd_core::ask::AskKind::Question,
            text: body.clone(),
            options: options.clone(),
            created_at_ms: 1,
            expires_at_ms: None,
        },
    );
    shprd_core::ask::AskQueue::for_repo(repo.path())
        .raise(
            &shprd_core::ask::Ask::new(ask, "T1", shprd_core::ask::AskKind::Question, body, 1)
                .with_options(options),
        )
        .expect("the ask file");
    repo
}

/// Answers the drop proposal on `fixture` and returns the reply and the events.
async fn answer_drop_proposal(
    fixture: &Fixture,
    answer: &str,
) -> (Reply, Vec<shprd_core::event::Event>) {
    let reply = fixture
        .send(
            Call::post(
                "/api/asks/answer",
                json!({"task": "T1", "ask": "propose-drop-1", "answer": answer}),
            )
            .with_token(&fixture.token)
            .with_write_header(),
        )
        .await;
    let events = EventLog::read_all(&fixture.repo.path().join(".shprd").join("events.jsonl"))
        .expect("read the log")
        .events;
    (reply, events)
}

#[tokio::test]
async fn confirming_a_proposal_card_runs_the_verb_once_as_the_person() {
    let fixture = Fixture::prepared(drop_proposal_repo(), tempfile::tempdir().expect("state"));
    let (reply, events) = answer_drop_proposal(&fixture, "confirm drop").await;
    assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
    let drops: Vec<_> = events
        .iter()
        .filter(|event| matches!(&event.kind, EventKind::TaskDropped { .. }))
        .collect();
    assert_eq!(drops.len(), 1, "the verb ran once");
    assert_eq!(drops[0].actor, Actor::Human);
    match &drops[0].kind {
        EventKind::TaskDropped { task, reason } => {
            assert_eq!(task, "T1");
            assert_eq!(reason, "stuck", "the card's reason is the drop's");
        }
        other => panic!("not a drop: {other:?}"),
    }
    // Answering the same card again is refused and runs nothing more.
    let (again, events) = answer_drop_proposal(&fixture, "confirm drop").await;
    assert_ne!(again.status, StatusCode::OK, "{:?}", again.body);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(&event.kind, EventKind::TaskDropped { .. }))
            .count(),
        1,
        "a second confirmation runs nothing"
    );
}

#[tokio::test]
async fn declining_a_proposal_card_writes_no_verb_event() {
    let fixture = Fixture::prepared(drop_proposal_repo(), tempfile::tempdir().expect("state"));
    let (reply, events) = answer_drop_proposal(&fixture, "leave it").await;
    assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
    assert!(
        !events
            .iter()
            .any(|event| matches!(&event.kind, EventKind::TaskDropped { .. })),
        "declining drops nothing"
    );
    let state = shprd_core::state::RunState::fold(&events).expect("folds");
    assert_ne!(
        state.tasks.get("T1").expect("T1").status,
        shprd_core::state::TaskStatus::Dropped
    );
}

#[tokio::test]
async fn an_agent_answering_a_proposal_card_is_refused_and_runs_nothing() {
    let repo = drop_proposal_repo();
    let state = tempfile::tempdir().expect("state");
    let app = App::prepare(
        repo.path(),
        PORT,
        &Actor::Agent {
            id: "claude".into(),
        },
        state.path(),
        None,
    )
    .expect("prepare");
    let info = app.info();
    let token = std::fs::read_to_string(&info.token_path).expect("the token file");
    let fixture = Fixture {
        token: token.trim().to_string(),
        id: info.id,
        app,
        state,
        repo,
    };
    let (reply, events) = answer_drop_proposal(&fixture, "confirm drop").await;
    assert_ne!(reply.status, StatusCode::OK, "{:?}", reply.body);
    assert!(
        !events.iter().any(|event| matches!(
            &event.kind,
            EventKind::TaskDropped { .. } | EventKind::AskAnswered { .. }
        )),
        "nothing ran and the card is still open"
    );
}

#[tokio::test]
async fn an_answer_that_is_not_a_choice_is_refused_and_the_card_stays_open() {
    let fixture = Fixture::prepared(spent_review_repo(), tempfile::tempdir().expect("state"));
    let reply = fixture
        .send(
            Call::post(
                "/api/asks/answer",
                json!({"task": "T7", "ask": "review-out-2", "answer": "take it"}),
            )
            .with_token(&fixture.token)
            .with_write_header(),
        )
        .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{:?}", reply.body);
    assert!(
        format!("{:?}", reply.body).contains("take it as it is"),
        "the refusal lists the choices: {:?}",
        reply.body
    );
    let events = EventLog::read_all(&fixture.repo.path().join(".shprd").join("events.jsonl"))
        .expect("read the log")
        .events;
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.kind, EventKind::AskAnswered { .. })),
        "nothing was recorded"
    );
}
