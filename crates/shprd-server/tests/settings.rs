//! The settings routes: `GET /api/settings` and `PUT /api/settings`, driven
//! through the router with `tower::ServiceExt`.
//!
//! Every test gets its own state directory, repository and home, and the
//! service is told where the shipped harness table is, so no test reads or
//! writes the developer's own settings and the role checks are the real ones.

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
const PORT: u16 = 18_090;

/// One prepared service, with the directories it lives in kept alive.
struct Fixture {
    app: App,
    token: String,
    home: TempDir,
    repo: TempDir,
    _state: TempDir,
}

impl Fixture {
    /// A service over a temp repository, with a temp home and the shipped table.
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
            home,
            repo,
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

    /// `GET /api/settings`.
    async fn settings(&self) -> Value {
        let (status, body) = self.send(Method::GET, "/api/settings", None).await;
        assert_eq!(status, StatusCode::OK, "GET: {body}");
        body
    }

    /// `PUT /api/settings` with one body.
    async fn put(&self, body: Value) -> (StatusCode, Value) {
        self.send(Method::PUT, "/api/settings", Some(body)).await
    }

    /// The person's settings file.
    fn settings_file(&self) -> PathBuf {
        self.home.path().join(".config/shprd/settings.toml")
    }

    /// The person's settings file's text, empty when it does not exist.
    fn settings_text(&self) -> String {
        fs::read_to_string(self.settings_file()).unwrap_or_default()
    }
}

/// The shipped `data/` directory: the real harness and family tables.
fn data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../data")
        .canonicalize()
        .expect("the shipped data directory")
}

/// One key's row from a settings document.
fn row<'a>(document: &'a Value, key: &str) -> &'a Value {
    document["values"]
        .as_array()
        .expect("values is a list")
        .iter()
        .find(|row| row["key"] == key)
        .unwrap_or_else(|| panic!("no row `{key}` in {document}"))
}

#[tokio::test]
async fn get_answers_every_key_with_the_layer_it_came_from() {
    let fixture = Fixture::new();
    let document = fixture.settings().await;
    assert!(
        document["repo"].is_string(),
        "the repo is named: {document}"
    );
    assert_eq!(row(&document, "tasks.merge")["value"], "ask");
    assert_eq!(row(&document, "tasks.merge")["source"], "built_in");
    assert_eq!(row(&document, "roles.worker.agent")["value"], Value::Null);
    assert_eq!(document["problems"], json!([]));
    assert!(
        document["values"].as_array().expect("values").len() > 30,
        "every key is answered"
    );
}

#[tokio::test]
async fn put_stores_a_value_and_get_reads_it_back() {
    let fixture = Fixture::new();
    let (status, reply) = fixture
        .put(json!({"key": "tasks.merge", "value": "checks"}))
        .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["outcome"], "stored");
    assert_eq!(row(&reply["settings"], "tasks.merge")["value"], "checks");
    assert_eq!(row(&reply["settings"], "tasks.merge")["source"], "you");
    assert!(
        fixture.settings_text().contains("merge = \"checks\""),
        "{}",
        fixture.settings_text()
    );

    let document = fixture.settings().await;
    assert_eq!(row(&document, "tasks.merge")["value"], "checks");
}

#[tokio::test]
async fn a_value_equal_to_the_inherited_one_is_not_stored() {
    let fixture = Fixture::new();
    let (status, reply) = fixture
        .put(json!({"key": "tasks.merge", "value": "ask"}))
        .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["outcome"], "equal_to_inherited");
    assert_eq!(row(&reply["settings"], "tasks.merge")["source"], "built_in");
    assert!(
        fixture.settings_text().is_empty() || !fixture.settings_text().contains("merge"),
        "the equal value is not written: {}",
        fixture.settings_text()
    );
}

#[tokio::test]
async fn put_without_a_value_removes_the_key() {
    let fixture = Fixture::new();
    fixture
        .put(json!({"key": "tasks.max_running", "value": 2}))
        .await;
    let (status, reply) = fixture.put(json!({"key": "tasks.max_running"})).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["outcome"], "removed");
    assert_eq!(row(&reply["settings"], "tasks.max_running")["value"], 4);
    assert!(!fixture.settings_text().contains("max_running"));

    let (status, reply) = fixture.put(json!({"key": "tasks.max_running"})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reply["outcome"], "was_unset");
}

#[tokio::test]
async fn put_for_repo_writes_that_repositorys_entry() {
    let fixture = Fixture::new();
    let name = fixture
        .repo
        .path()
        .file_name()
        .expect("the repo has a name")
        .to_string_lossy()
        .into_owned();
    let (status, reply) = fixture
        .put(json!({"key": "tasks.merge", "value": "auto", "for_repo": true}))
        .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["outcome"], "stored");
    assert_eq!(
        row(&reply["settings"], "tasks.merge")["source"],
        "repo_entry"
    );
    assert!(
        fixture
            .settings_text()
            .contains(&format!("[repos.\"{name}\".tasks]")),
        "{}",
        fixture.settings_text()
    );
}

#[tokio::test]
async fn a_same_vendor_review_is_refused_and_nothing_is_written() {
    let fixture = Fixture::new();
    let (status, reply) = fixture
        .put(json!({"key": "roles.worker.agent", "value": "claude"}))
        .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    let before = fixture.settings_text();

    let (status, reply) = fixture
        .put(json!({"key": "roles.reviewer.agent", "value": "claude"}))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{reply}");
    let message = reply["message"].as_str().unwrap_or_default();
    assert!(message.contains("are both anthropic"), "{reply}");
    assert_eq!(
        fixture.settings_text(),
        before,
        "a refused write leaves the file alone"
    );

    let (status, reply) = fixture
        .put(json!({"key": "roles.reviewer.agent", "value": "codex"}))
        .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(
        reply["settings"]["problems"],
        json!([]),
        "the pair now checks out"
    );
}

#[tokio::test]
async fn a_key_the_schema_does_not_have_is_a_bad_request() {
    let fixture = Fixture::new();
    let (status, reply) = fixture
        .put(json!({"key": "tasks.nope", "value": "x"}))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{reply}");
    let message = reply["message"].as_str().unwrap_or_default();
    assert!(message.contains("unknown settings key"), "{reply}");
    assert!(message.contains("roles.order"), "{reply}");
}

#[tokio::test]
async fn a_value_the_key_does_not_take_is_a_bad_request() {
    let fixture = Fixture::new();
    let (status, reply) = fixture
        .put(json!({"key": "tasks.max_running", "value": 99}))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{reply}");
    assert!(
        reply["message"]
            .as_str()
            .unwrap_or_default()
            .contains("takes 1 to 10"),
        "{reply}"
    );

    let (status, reply) = fixture
        .put(json!({"key": "tasks.merge", "value": {}}))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{reply}");
}

#[tokio::test]
async fn a_write_needs_the_write_header() {
    let fixture = Fixture::new();
    let request = Request::builder()
        .method(Method::PUT)
        .uri("/api/settings")
        .header(header::HOST, HOST)
        .header(header::AUTHORIZATION, format!("Bearer {}", fixture.token))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({"key": "tasks.merge", "value": "checks"}).to_string(),
        ))
        .expect("the request");
    let response = fixture
        .app
        .router()
        .oneshot(request)
        .await
        .expect("an answer");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(fixture.settings_text().is_empty(), "nothing was written");
}

#[tokio::test]
async fn a_broken_settings_file_is_reported_not_hidden() {
    let fixture = Fixture::new();
    let path = fixture.settings_file();
    fs::create_dir_all(path.parent().expect("the config dir")).expect("the dir");
    fs::write(&path, "[tasks]\nmerge = \"sometimes\"\n").expect("the file");
    let (status, reply) = fixture.send(Method::GET, "/api/settings", None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{reply}");
    let message = reply["message"].as_str().unwrap_or_default();
    assert!(message.contains("tasks.merge"), "{reply}");
    assert!(message.contains("ask, checks, auto"), "{reply}");
}

#[tokio::test]
async fn a_role_is_never_stored_unchecked() {
    let fixture = Fixture::with_table(false);
    let (status, reply) = fixture
        .put(json!({"key": "roles.worker.agent", "value": "claude"}))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{reply}");
    let message = reply["message"].as_str().unwrap_or_default();
    assert!(message.contains("cannot be stored"), "{reply}");
    assert!(message.contains("SHPRD_DATA_DIR"), "{reply}");
    assert!(fixture.settings_text().is_empty(), "nothing was written");

    // A workflow key needs no harness table.
    let (status, reply) = fixture
        .put(json!({"key": "tasks.max_running", "value": 2}))
        .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["outcome"], "stored");
}

#[tokio::test]
async fn a_read_says_when_the_roles_could_not_be_checked() {
    let fixture = Fixture::with_table(false);
    let path = fixture.settings_file();
    fs::create_dir_all(path.parent().expect("the config dir")).expect("the dir");
    fs::write(
        &path,
        "[roles.worker]\nagent = \"claude\"\n\n[roles.reviewer]\nagent = \"claude\"\n",
    )
    .expect("the file");

    let document = fixture.settings().await;
    assert_eq!(document["problems"], json!([]), "{document}");
    let reason = document["not_checked"].as_str().unwrap_or_default();
    assert!(reason.contains("SHPRD_DATA_DIR"), "{document}");

    // With a table, the same file is checked and the pair is named.
    let checked = Fixture::with_table(true);
    let path = checked.settings_file();
    fs::create_dir_all(path.parent().expect("the config dir")).expect("the dir");
    fs::write(
        &path,
        "[roles.worker]\nagent = \"claude\"\n\n[roles.reviewer]\nagent = \"claude\"\n",
    )
    .expect("the file");
    let document = checked.settings().await;
    assert!(
        document["problems"][0]
            .as_str()
            .unwrap_or_default()
            .contains("both anthropic"),
        "{document}"
    );
    assert!(document["not_checked"].is_null(), "{document}");
}

#[tokio::test]
async fn a_broken_pair_someone_left_does_not_block_other_keys() {
    let fixture = Fixture::new();
    let path = fixture.settings_file();
    fs::create_dir_all(path.parent().expect("the config dir")).expect("the dir");
    fs::write(
        &path,
        "[roles.worker]\nagent = \"claude\"\n\n[roles.reviewer]\nagent = \"claude\"\n",
    )
    .expect("the file");

    let (status, reply) = fixture
        .put(json!({"key": "tasks.review_rounds", "value": 3}))
        .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["outcome"], "stored");

    let (status, reply) = fixture
        .put(json!({"key": "roles.docs.agent", "value": "gemini"}))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{reply}");
    assert!(
        reply["message"]
            .as_str()
            .unwrap_or_default()
            .contains("no harness defines"),
        "{reply}"
    );
}

/// An opener that keeps the names the coordinator's tab was given and the
/// restarts it was asked for.
#[derive(Debug, Default)]
struct Tabs {
    named: std::sync::Mutex<Vec<String>>,
    restarts: std::sync::Mutex<u32>,
}

impl shprd_server::AgentOpener for Tabs {
    fn open(&self, _: &str, _: Option<&str>) -> Result<shprd_server::OpenedTab, String> {
        Err("not used".to_owned())
    }

    fn name_coordinator(&self, title: &str) -> Result<(), String> {
        self.named.lock().expect("lock").push(title.to_owned());
        Ok(())
    }

    fn restart_coordinator(&self) -> Result<(), String> {
        *self.restarts.lock().expect("lock") += 1;
        Ok(())
    }
}

#[tokio::test]
async fn naming_the_coordinator_renames_its_tab_and_the_state_carries_the_names() {
    let tabs = std::sync::Arc::new(Tabs::default());
    let mut fixture = Fixture::new();
    fixture.app = fixture.app.with_agent_opener(tabs.clone());
    // A name is trimmed, and shows in the state for every client.
    let (status, reply) = fixture
        .put(json!({"key": "roles.coordinator.title", "value": "  Orchestrator "}))
        .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(
        row(&reply["settings"], "roles.coordinator.title")["value"],
        "Orchestrator"
    );
    assert_eq!(*tabs.named.lock().expect("lock"), ["Orchestrator"]);
    fixture
        .put(json!({"key": "roles.worker.title", "value": "Dev"}))
        .await;
    let (_, state) = fixture.send(Method::GET, "/api/state", None).await;
    assert_eq!(state["role_titles"]["coordinator"], "Orchestrator");
    assert_eq!(state["role_titles"]["worker"], "Dev");
    assert_eq!(state["role_titles"]["reviewer"], "Reviewer");
    // A worker's name does not touch the coordinator's tab.
    assert_eq!(tabs.named.lock().expect("lock").len(), 1);
    // A blank name is no name: the built-in title comes back, and the tab follows.
    let (status, _) = fixture
        .put(json!({"key": "roles.coordinator.title", "value": "   "}))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        tabs.named.lock().expect("lock").last().map(String::as_str),
        Some("Assistant")
    );
    let (_, state) = fixture.send(Method::GET, "/api/state", None).await;
    assert_eq!(state["role_titles"]["coordinator"], "Assistant");
    // A name is cut to its cap.
    fixture
        .put(json!({"key": "roles.reviewer.title", "value": "x".repeat(60)}))
        .await;
    let (_, state) = fixture.send(Method::GET, "/api/state", None).await;
    assert_eq!(state["role_titles"]["reviewer"].as_str().unwrap().len(), 24);
}

#[tokio::test]
async fn restarting_the_coordinator_asks_the_host_once() {
    let tabs = std::sync::Arc::new(Tabs::default());
    let mut fixture = Fixture::new();
    fixture.app = fixture.app.with_agent_opener(tabs.clone());
    let (status, _) = fixture
        .send(Method::POST, "/api/coordinator/restart", None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(*tabs.restarts.lock().expect("lock"), 1);
}

#[tokio::test]
async fn a_service_with_no_host_refuses_to_restart_the_coordinator() {
    let fixture = Fixture::new();
    let (status, _) = fixture
        .send(Method::POST, "/api/coordinator/restart", None)
        .await;
    assert_ne!(status, StatusCode::NO_CONTENT);
}

/// The model the dialog would choose for a program: the roster's first model, or —
/// for `pi`, whose models this hand-built service cannot list — two of the
/// provider-qualified ids its own `--list-models` prints.
fn model_for(roster: &Value, program: &str, other_vendor: bool) -> Option<String> {
    if program == "pi" {
        return Some(match other_vendor {
            false => "Command Code/deepseek/deepseek-v4.1-flash".to_owned(),
            true => "Command Code/z-ai/glm-5.3-flash".to_owned(),
        });
    }
    roster["programs"]
        .as_array()?
        .iter()
        .find(|entry| entry["id"] == program)?["models"]
        .as_array()?
        .first()?["id"]
        .as_str()
        .map(str::to_owned)
}

/// The writes the dialog makes to put `program` (with `model`) in the first line of
/// `role`: the old program's model, level and mode out, then the program, then its model.
fn first_line_writes(role: &str, program: &str, model: Option<&str>) -> Vec<Value> {
    let key = |field: &str| format!("roles.{role}.{field}");
    let mut writes = vec![
        json!({"key": key("model"), "value": null}),
        json!({"key": key("effort"), "value": null}),
        json!({"key": key("perm"), "value": null}),
        json!({"key": key("agent"), "value": program}),
    ];
    if let Some(model) = model {
        writes.push(json!({"key": key("model"), "value": model}));
    }
    writes
}

/// Every roster program, as the first and as the second line of Worker (s) and of
/// Reviewer (s), through the calls the dialog makes: each write is accepted, or
/// refused only with the cross-vendor sentence.
#[tokio::test]
async fn every_harness_is_accepted_in_every_slot() {
    let mut accepted = 0;
    let mut cross_vendor = 0;
    let probe = Fixture::new();
    let (_, roster) = probe.send(Method::GET, "/api/roster", None).await;
    let programs: Vec<String> = roster["programs"]
        .as_array()
        .expect("programs")
        .iter()
        .map(|entry| entry["id"].as_str().expect("id").to_owned())
        .collect();
    assert_eq!(programs, ["claude", "codex", "kilo", "opencode", "pi"]);
    for role in ["worker", "reviewer"] {
        for position in [0usize, 1] {
            for program in &programs {
                let fixture = Fixture::new();
                // Both roles start on other programs, with a model and a level set.
                for (key, value) in [
                    ("roles.worker.agent", json!("claude")),
                    ("roles.worker.model", json!("opus")),
                    ("roles.reviewer.agent", json!("codex")),
                    ("roles.reviewer.model", json!("gpt-6-luna")),
                ] {
                    let (status, reply) = fixture.put(json!({"key": key, "value": value})).await;
                    assert_eq!(status, StatusCode::OK, "{key}: {reply}");
                }
                // The other role's vendor decides which Pi model is a different vendor.
                let other_vendor = role == "reviewer";
                let model = model_for(&roster, program, other_vendor);
                let writes = if position == 0 {
                    first_line_writes(role, program, model.as_deref())
                } else {
                    let line = format!("{program}|{}|||", model.as_deref().unwrap_or(""));
                    vec![json!({"key": format!("roles.{role}.agents"), "value": [line]})]
                };
                for write in writes {
                    let (status, reply) = fixture.put(write.clone()).await;
                    match status {
                        StatusCode::OK => accepted += 1,
                        _ => {
                            let message = reply["message"].as_str().unwrap_or_default();
                            assert!(
                                message.contains("not cross-vendor"),
                                "{program} as {role} line {}: {write} was refused: {message}",
                                position + 1
                            );
                            cross_vendor += 1;
                        }
                    }
                }
                let document = fixture.settings().await;
                assert_eq!(
                    document["problems"],
                    json!([]),
                    "{program} {role} {position}"
                );
            }
        }
    }
    println!(
        "harness x role x position: {accepted} writes accepted, {cross_vendor} refused as cross-vendor"
    );
    assert!(accepted > 40, "{accepted}");
}
