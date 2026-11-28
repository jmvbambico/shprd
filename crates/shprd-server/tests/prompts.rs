//! The prompt routes: `GET /api/prompts/:id` and `PUT /api/prompts/:id`,
//! driven through the router with `tower::ServiceExt`.
//!
//! Every test gets its own state directory, repository, home and prompts
//! directory, so no test reads or writes the developer's own prompt files.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
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
const PORT: u16 = 18_091;

/// One prepared service, with the directories it lives in kept alive.
struct Fixture {
    app: App,
    token: String,
    prompts: TempDir,
    _home: TempDir,
    _repo: TempDir,
    _state: TempDir,
}

impl Fixture {
    /// A service over a temp repository, with its own prompts directory.
    fn new() -> Fixture {
        let state = TempDir::new().expect("state dir");
        let repo = TempDir::new().expect("repo dir");
        let home = TempDir::new().expect("home dir");
        let prompts = TempDir::new().expect("prompts dir");
        fs::create_dir_all(repo.path().join(".shprd")).expect("the repo's .shprd");
        let app = App::prepare(repo.path(), PORT, &Actor::Human, state.path(), None)
            .expect("prepare")
            .with_home(home.path().to_path_buf())
            .with_prompts_dir(prompts.path().to_path_buf());
        let info = app.info();
        let token = fs::read_to_string(&info.token_path)
            .expect("the token file")
            .trim()
            .to_string();
        Fixture {
            app,
            token,
            prompts,
            _home: home,
            _repo: repo,
            _state: state,
        }
    }

    /// Sends one request through the router and returns its status and body.
    async fn send(&self, method: Method, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method(method.clone())
            .uri(path)
            .header(header::HOST, HOST)
            .header(header::AUTHORIZATION, format!("Bearer {}", self.token));
        if method != Method::GET {
            request = request.header("x-shprd", "1");
        }
        let request = match body {
            Some(body) => request
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .expect("the request"),
            None => request.body(Body::empty()).expect("the request"),
        };
        let response = self.app.router().oneshot(request).await.expect("an answer");
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("the body reads")
            .to_bytes();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).expect("the body is JSON")
        };
        (status, body)
    }

    /// `GET /api/prompts/:id`.
    async fn prompt(&self, id: &str) -> (StatusCode, Value) {
        self.send(Method::GET, &format!("/api/prompts/{id}"), None)
            .await
    }

    /// `PUT /api/prompts/:id` with one body.
    async fn put(&self, id: &str, body: Value) -> (StatusCode, Value) {
        self.send(Method::PUT, &format!("/api/prompts/{id}"), Some(body))
            .await
    }

    /// One prompt's file.
    fn file(&self, id: &str) -> PathBuf {
        self.prompts.path().join(format!("{id}.md"))
    }
}

/// The shipped worker prompt, as the build carries it.
fn shipped_worker() -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/templates/worker.md"))
        .expect("the shipped worker prompt")
}

#[tokio::test]
async fn a_read_installs_the_shipped_prompt_on_first_use() {
    let fixture = Fixture::new();
    let (status, body) = fixture.prompt("worker").await;
    assert_eq!(status, StatusCode::OK, "GET: {body}");
    assert_eq!(body["id"], "worker");
    assert_eq!(body["state"], "default");
    assert_eq!(body["text"], shipped_worker());
    assert!(fixture.file("worker").exists(), "first use writes the file");
    let path = body["path"].as_str().expect("a path");
    assert_eq!(
        Path::new(path),
        fixture.file("worker"),
        "the DTO names the file the words are in"
    );
}

#[tokio::test]
async fn a_write_stores_the_persons_words_and_a_read_keeps_them() {
    let fixture = Fixture::new();
    let (status, body) = fixture
        .put("worker", json!({"text": "my own words\n"}))
        .await;
    assert_eq!(status, StatusCode::OK, "PUT: {body}");
    assert_eq!(body["state"], "edited");
    assert_eq!(
        fs::read_to_string(fixture.file("worker")).expect("the file is there"),
        "my own words\n"
    );

    let (status, body) = fixture.prompt("worker").await;
    assert_eq!(status, StatusCode::OK, "GET: {body}");
    assert_eq!(body["state"], "edited", "the edit is not upgraded away");
    assert_eq!(body["text"], "my own words\n");
}

#[tokio::test]
async fn writing_the_shipped_words_back_reads_as_the_default() {
    let fixture = Fixture::new();
    let (status, body) = fixture
        .put("worker", json!({"text": shipped_worker()}))
        .await;
    assert_eq!(status, StatusCode::OK, "PUT: {body}");
    assert_eq!(body["state"], "default");
}

#[tokio::test]
async fn a_prompt_nothing_ships_is_a_404_a_bad_id_a_400() {
    let fixture = Fixture::new();
    let (status, body) = fixture.prompt("nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["code"], "not_found");
    assert!(
        body["message"]
            .as_str()
            .is_some_and(|text| text.contains("no prompt `nope`")),
        "{body}"
    );

    let (status, body) = fixture.prompt("..%2Fworker").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "bad_request");
}

#[tokio::test]
async fn a_body_that_is_not_a_prompt_write_is_a_400() {
    let fixture = Fixture::new();
    let (status, body) = fixture.put("worker", json!({"words": "typo"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["message"]
            .as_str()
            .is_some_and(|text| text.contains("\"text\"")),
        "the refusal says what the body looks like: {body}"
    );
    assert!(
        !fixture.file("worker").exists(),
        "a refused write leaves no file"
    );
}

#[tokio::test]
async fn the_list_names_every_template_without_its_words() {
    let fixture = Fixture::new();
    let (status, body) = fixture.send(Method::GET, "/api/prompts", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let prompts = body["prompts"].as_array().expect("a list of prompts");
    let ids: Vec<&str> = prompts
        .iter()
        .map(|prompt| prompt["id"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(ids, ["assistant", "worker", "reviewer", "backup"]);
    let worker = prompts
        .iter()
        .find(|prompt| prompt["id"] == "worker")
        .expect("the worker's prompt");
    assert_eq!(worker["state"], "default");
    assert_eq!(worker["shipped"], true);
    assert!(worker["about"].is_string(), "the list says what it is for");
    assert!(
        worker.get("text").is_none(),
        "the words are read one at a time: {worker}"
    );
    assert_eq!(
        Path::new(worker["path"].as_str().expect("a path")),
        fixture.file("worker"),
        "each entry names its file"
    );
}

#[tokio::test]
async fn a_reset_puts_the_shipped_words_back_over_an_edit() {
    let fixture = Fixture::new();
    let (status, body) = fixture
        .put("worker", json!({"text": "my own words\n"}))
        .await;
    assert_eq!(status, StatusCode::OK, "PUT: {body}");
    assert_eq!(body["state"], "edited");

    let (status, body) = fixture
        .send(Method::DELETE, "/api/prompts/worker", None)
        .await;
    assert_eq!(status, StatusCode::OK, "DELETE: {body}");
    assert_eq!(body["state"], "default");
    assert_eq!(body["text"], shipped_worker());
    assert_eq!(
        fs::read_to_string(fixture.file("worker")).expect("the file is there"),
        shipped_worker(),
        "reset writes the shipped words"
    );

    // Resetting a prompt that is already the default changes nothing and still
    // answers with the prompt.
    let (status, body) = fixture
        .send(Method::DELETE, "/api/prompts/worker", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "default");
}

#[tokio::test]
async fn a_reset_of_a_prompt_nothing_ships_is_a_404_and_needs_the_write_header() {
    let fixture = Fixture::new();
    let (status, body) = fixture
        .send(Method::DELETE, "/api/prompts/nope", None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    // A DELETE is a write: without `x-shprd: 1` it is refused before the route.
    let request = Request::builder()
        .method(Method::DELETE)
        .uri("/api/prompts/worker")
        .header(header::HOST, HOST)
        .header(header::AUTHORIZATION, format!("Bearer {}", fixture.token))
        .body(Body::empty())
        .expect("the request");
    let response = fixture
        .app
        .router()
        .oneshot(request)
        .await
        .expect("an answer");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(!fixture.file("worker").exists(), "nothing was written");
}
