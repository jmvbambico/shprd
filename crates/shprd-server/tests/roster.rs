//! The roster route: `GET /api/roster`, the programs a settings page offers.
//!
//! Every test gets its own state directory and repository, and the service is
//! told where the shipped harness table is. No test passes a `PATH`, so no
//! program's own model listing runs on this machine: the static catalog in
//! `data/models/` is what a picker sees here, exactly as a service built by hand
//! sees it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::Value;
use shprd_core::event::Actor;
use shprd_server::App;
use tempfile::TempDir;
use tower::ServiceExt;

/// The `Host` every fixture request is addressed to.
///
/// The gate refuses a request with no `Host` (`421`), so a test that drove the
/// router without one would be testing the refusal, not the route.
const HOST: &str = "127.0.0.1:18080";

/// The port the fixture claims; no `Origin` is sent, so it is only a label.
const PORT: u16 = 18_092;

/// One prepared service, with the directories it lives in kept alive.
struct Fixture {
    app: App,
    token: String,
    _home: TempDir,
    _repo: TempDir,
    _state: TempDir,
}

impl Fixture {
    /// A service with the shipped harness table, and no `PATH` to list models.
    fn new() -> Fixture {
        Fixture::with_table(true)
    }

    /// A service that has (or has not) been given a harness table.
    fn with_table(table: bool) -> Fixture {
        let state = TempDir::new().expect("state dir");
        let repo = TempDir::new().expect("repo dir");
        let home = TempDir::new().expect("home dir");
        fs::create_dir_all(repo.path().join(".shprd")).expect("the repo's .shprd");
        let app = App::prepare(repo.path(), PORT, &Actor::Human, state.path(), None)
            .expect("prepare")
            .with_home(home.path().to_path_buf());
        let app = match table {
            true => app.with_data(data_dir()),
            false => app.without_data(),
        };
        let info = app.info();
        let token = fs::read_to_string(&info.token_path)
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

    /// `GET /api/roster`, through the router.
    async fn roster(&self) -> Value {
        let request = Request::builder()
            .method(Method::GET)
            .uri("/api/roster")
            .header(header::HOST, HOST)
            .header(header::AUTHORIZATION, format!("Bearer {}", self.token))
            .body(Body::empty())
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
        assert_eq!(status, StatusCode::OK, "GET /api/roster: {body}");
        body
    }
}

/// The shipped `data/` directory: the real harness and family tables.
fn data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../data")
        .canonicalize()
        .expect("the shipped data directory")
}

/// One program's entry from a roster document.
fn program<'a>(roster: &'a Value, id: &str) -> &'a Value {
    roster["programs"]
        .as_array()
        .expect("programs is a list")
        .iter()
        .find(|program| program["id"] == id)
        .unwrap_or_else(|| panic!("no program `{id}` in {roster}"))
}

#[tokio::test]
async fn every_program_the_harness_table_defines_is_offered() {
    let fixture = Fixture::new();
    let roster = fixture.roster().await;
    let ids: Vec<&str> = roster["programs"]
        .as_array()
        .expect("programs")
        .iter()
        .map(|program| program["id"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(ids, ["claude", "codex", "kilo", "opencode", "pi"]);
    assert!(roster["note"].is_null(), "{roster}");
}

#[tokio::test]
async fn a_static_catalog_supplies_the_models_and_their_levels() {
    let fixture = Fixture::new();
    let roster = fixture.roster().await;
    let claude = program(&roster, "claude");
    assert_eq!(claude["name"], "Claude Code");
    assert_eq!(claude["vendor"], "anthropic");
    assert_eq!(claude["models_source"], "static");
    let models = claude["models"].as_array().expect("models");
    assert_eq!(models.len(), 3, "{claude}");
    assert_eq!(models[1]["id"], "opus");
    assert_eq!(models[1]["name"], "Opus");
    assert_eq!(
        models[1]["efforts"],
        serde_json::json!(["low", "medium", "high", "xhigh", "max"])
    );
    assert_eq!(models[1]["default_effort"], "medium");
    assert_eq!(claude["default_permission"], "manual");
    assert_eq!(claude["fast"], true);
    assert!(
        !claude["permissions"].as_array().expect("modes").is_empty(),
        "the modes a role may pick are offered: {claude}"
    );
    assert!(claude["models_note"].is_null(), "{claude}");
}

#[tokio::test]
async fn a_program_that_lists_its_own_models_is_not_run_by_a_hand_built_service() {
    let fixture = Fixture::new();
    let roster = fixture.roster().await;
    let pi = program(&roster, "pi");
    assert_eq!(pi["models_source"], "command");
    assert_eq!(pi["models"], serde_json::json!([]), "{pi}");
    assert_eq!(pi["needs_provider"], true);
    let note = pi["models_note"].as_str().unwrap_or_default();
    assert!(note.contains("--list-models"), "{pi}");
}

#[tokio::test]
async fn a_service_with_no_data_directory_offers_no_programs_and_says_why() {
    let fixture = Fixture::with_table(false);
    let roster = fixture.roster().await;
    assert_eq!(roster["programs"], serde_json::json!([]), "{roster}");
    let note = roster["note"].as_str().unwrap_or_default();
    assert!(note.contains("SHPRD_DATA_DIR"), "{roster}");
}

#[tokio::test]
async fn the_roster_needs_a_token_like_every_other_read() {
    let fixture = Fixture::new();
    let request = Request::builder()
        .method(Method::GET)
        .uri("/api/roster")
        .header(header::HOST, HOST)
        .body(Body::empty())
        .expect("the request");
    let response = fixture
        .app
        .router()
        .oneshot(request)
        .await
        .expect("an answer");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
