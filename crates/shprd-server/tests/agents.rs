//! The tab bar's "+" menu, served: `POST /api/agents` starts a program in a new
//! tab, and `GET /api/roster` says which programs this machine has.
//!
//! Nothing here opens a terminal or starts a CLI. The opener is a recorder, so
//! the route is proved — the body it accepts, the tab it answers, the refusal a
//! service with no host gives — without a person's window being touched. The
//! installed flag is a `PATH` lookup, and the "installed" one is a file this test
//! wrote in a temp directory: a real `claude` on the machine running the tests
//! must not decide what the assertion sees.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use shprd_core::event::Actor;
use shprd_server::{AgentOpener, App, OpenedTab};
use tempfile::TempDir;
use tower::ServiceExt;

/// The port the fixture claims; no `Origin` is sent, so it is only a label.
const PORT: u16 = 18_093;

/// An opener that keeps every program it was asked for, and answers a tab.
#[derive(Debug, Default)]
struct Recorder {
    /// The programs asked for, in order.
    asked: Mutex<Vec<String>>,
    /// When set, every open fails with these words.
    refuse: Option<String>,
}

impl AgentOpener for Recorder {
    fn open(&self, program: &str) -> Result<OpenedTab, String> {
        if let Some(reason) = &self.refuse {
            return Err(reason.clone());
        }
        self.asked
            .lock()
            .expect("the lock")
            .push(program.to_string());
        Ok(OpenedTab {
            program: program.to_string(),
            name: "Claude Code".to_string(),
            tab: "t7".to_string(),
        })
    }
}

/// One prepared service, with the directories it lives in kept alive.
struct Fixture {
    app: App,
    token: String,
    _home: TempDir,
    _repo: TempDir,
    _state: TempDir,
}

impl Fixture {
    /// A service whose opener is `recorder`, or none when `None`.
    fn new(recorder: Option<Arc<Recorder>>) -> Fixture {
        Fixture::with(recorder, None)
    }

    /// [`Fixture::new`], with the `PATH` a program's presence is read from.
    fn with(recorder: Option<Arc<Recorder>>, path: Option<&Path>) -> Fixture {
        let state = TempDir::new().expect("state dir");
        let repo = TempDir::new().expect("repo dir");
        let home = TempDir::new().expect("home dir");
        fs::create_dir_all(repo.path().join(".shprd")).expect("the repo's .shprd");
        let mut app = App::prepare(repo.path(), PORT, &Actor::Human, state.path(), None)
            .expect("prepare")
            .with_home(home.path().to_path_buf())
            .with_data(data_dir());
        if let Some(recorder) = recorder {
            let opener: Arc<dyn AgentOpener> = recorder;
            app = app.with_agent_opener(opener);
        }
        if let Some(path) = path {
            app = app.with_path_env(Some(path.display().to_string()));
        }
        let token = fs::read_to_string(&app.info().token_path)
            .expect("the token file")
            .trim()
            .to_string();
        Fixture {
            app,
            token,
            _home: home,
            _repo: repo,
            _state: state,
        }
    }

    /// `POST /api/agents`, through the router.
    async fn open(&self, body: Value) -> (StatusCode, Value) {
        let request = Request::builder()
            .method(Method::POST)
            .uri("/api/agents")
            .header(header::AUTHORIZATION, format!("Bearer {}", self.token))
            .header("x-shprd", "1")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .expect("the request");
        let response = self.app.router().oneshot(request).await.expect("an answer");
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("the body reads")
            .to_bytes();
        let body: Value = serde_json::from_slice(&bytes).expect("the body is JSON");
        (status, body)
    }

    /// `GET /api/roster`, through the router.
    async fn roster(&self) -> Value {
        let request = Request::builder()
            .method(Method::GET)
            .uri("/api/roster")
            .header(header::AUTHORIZATION, format!("Bearer {}", self.token))
            .body(Body::empty())
            .expect("the request");
        let response = self.app.router().oneshot(request).await.expect("an answer");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("the body reads")
            .to_bytes();
        serde_json::from_slice(&bytes).expect("the body is JSON")
    }
}

/// The shipped `data/` directory: the real harness and family tables.
fn data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../data")
        .canonicalize()
        .expect("the shipped data directory")
}

#[tokio::test]
async fn opening_an_agent_asks_the_host_for_the_program_and_answers_the_tab() {
    let recorder = Arc::new(Recorder::default());
    let fixture = Fixture::new(Some(recorder.clone()));
    let (status, body) = fixture.open(json!({ "program": "claude" })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body,
        json!({ "program": "claude", "name": "Claude Code", "tab": "t7" })
    );
    assert_eq!(
        recorder.asked.lock().expect("the lock").as_slice(),
        ["claude".to_string()]
    );
}

#[tokio::test]
async fn a_service_without_a_host_refuses_and_says_why() {
    let fixture = Fixture::new(None);
    let (status, body) = fixture.open(json!({ "program": "claude" })).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let message = body["message"].as_str().expect("a message");
    assert!(message.contains("without a host"), "{message}");
}

#[tokio::test]
async fn a_refused_open_is_the_hosts_words() {
    let fixture = Fixture::new(Some(Arc::new(Recorder {
        asked: Mutex::default(),
        refuse: Some("no space called \"shop\" on the host".to_string()),
    })));
    let (status, body) = fixture.open(json!({ "program": "pi" })).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["message"], "no space called \"shop\" on the host");
}

#[tokio::test]
async fn a_malformed_body_is_refused_without_reaching_the_host() {
    let recorder = Arc::new(Recorder::default());
    let fixture = Fixture::new(Some(recorder.clone()));
    let (status, body) = fixture.open(json!({ "prog": "claude" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, body) = fixture.open(json!({ "program": "   " })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap_or_default()
            .contains("harness id")
    );
    assert!(
        recorder.asked.lock().expect("the lock").is_empty(),
        "the host was asked about a request the route refused"
    );
}

#[tokio::test]
async fn the_gate_refuses_a_write_without_the_custom_header() {
    let fixture = Fixture::new(Some(Arc::new(Recorder::default())));
    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/agents")
        .header(header::AUTHORIZATION, format!("Bearer {}", fixture.token))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "program": "claude" }).to_string()))
        .expect("the request");
    let response = fixture
        .app
        .router()
        .oneshot(request)
        .await
        .expect("an answer");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn installed_is_what_the_told_path_says_and_a_hand_built_service_claims_nothing() {
    // A `PATH` holding one executable called `claude`, and nothing else.
    let path = TempDir::new().expect("path dir");
    let binary = path.path().join("claude");
    fs::write(&binary, "#!/bin/sh\nexit 0\n").expect("the fixture binary");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).expect("the mode");
    }
    let fixture = Fixture::with(None, Some(path.path()));
    let roster = fixture.roster().await;
    let programs = roster["programs"].as_array().expect("programs");
    let installed: Vec<(&str, bool)> = programs
        .iter()
        .map(|program| {
            (
                program["id"].as_str().unwrap_or_default(),
                program["installed"].as_bool().unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(
        installed,
        [
            ("claude", true),
            ("codex", false),
            ("kilo", false),
            ("opencode", false),
            ("pi", false),
        ]
    );

    // A service nobody told a `PATH`: no program is claimed to be installed.
    let fixture = Fixture::with(None, None);
    let roster = fixture.roster().await;
    for program in roster["programs"].as_array().expect("programs") {
        assert_eq!(program["installed"], json!(false), "{program}");
    }
}
