//! The task routes: adding one, reading what its branch changed, and the
//! person's own verbs on it (`start`, `retry`, `stop`, `drop`, `accept`).
//!
//! One prepared service per test, driven through the router with
//! `tower::ServiceExt`. Nothing here needs a network, an agent or the Herdr
//! desktop: a start and a stop are the [`shprd_server::Engine`] closures, and a
//! test wires closures that record what they were asked for. The repository in
//! each test is a real git one, so the changes endpoint reads git exactly as it
//! does in a run, and git's own view is what the assertions compare against.
//!
//! Every child process gets its own HOME and no global git config, so a
//! developer's `~/.gitconfig` (signing, hooks) can never decide a test.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use shprd_core::event::{Actor, EventKind};
use shprd_core::log::{EventLog, SystemClock};
use shprd_server::{App, Engine};
use tempfile::TempDir;
use tower::ServiceExt;

/// The port the fixtures claim; no `Origin` is sent, so it is only a label.
const PORT: u16 = 18_120;

/// What an engine closure was asked for, in order.
type Calls = Arc<Mutex<Vec<(String, String)>>>;

/// One prepared service, with the directories it lives in kept alive.
struct Fixture {
    app: App,
    token: String,
    home: TempDir,
    repo: TempDir,
    _state: TempDir,
}

impl Fixture {
    /// A service over a new repository whose run has started.
    fn new() -> Fixture {
        let repo = TempDir::new().expect("repo dir");
        start_run(repo.path());
        Fixture::prepared(repo)
    }

    /// A service over `repo`, which the test has already written a log into.
    fn prepared(repo: TempDir) -> Fixture {
        let state = TempDir::new().expect("state dir");
        let home = TempDir::new().expect("home dir");
        fs::create_dir_all(repo.path().join(".shprd")).expect("the repo's .shprd");
        let app = App::prepare(repo.path(), PORT, &Actor::Human, state.path(), None)
            .expect("prepare")
            .with_home(home.path().to_path_buf());
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

    /// The same service, with the person's settings file written.
    fn settings(self, text: &str) -> Fixture {
        let dir = self.home.path().join(".config").join("shprd");
        fs::create_dir_all(&dir).expect("the settings dir");
        fs::write(dir.join("settings.toml"), text).expect("the settings file");
        self
    }

    /// Sends one request through the router, with the token and a body.
    async fn send(&self, method: Method, path: &str, body: Option<Value>) -> Reply {
        let mut request = Request::builder()
            .method(method.clone())
            .uri(path)
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
            None
        } else {
            Some(serde_json::from_slice(&bytes).expect("the body is JSON"))
        };
        Reply { status, body }
    }

    /// A write without `x-shprd: 1`.
    async fn send_without_the_header(&self, method: Method, path: &str, body: Value) -> Reply {
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header(header::AUTHORIZATION, format!("Bearer {}", self.token))
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
        Reply {
            status,
            body: Some(serde_json::from_slice(&bytes).expect("the body is JSON")),
        }
    }

    /// `POST /api/action`, as a client sends it.
    async fn act(&self, body: Value) -> Reply {
        self.send(Method::POST, "/api/action", Some(body)).await
    }

    /// `POST /api/tasks`, as a client sends it.
    async fn add(&self, body: Value) -> Reply {
        self.send(Method::POST, "/api/tasks", Some(body)).await
    }

    /// Every event in the log, oldest first.
    fn events(&self) -> Vec<shprd_core::event::Event> {
        let path = self.repo.path().join(".shprd").join("events.jsonl");
        EventLog::read_all(&path).expect("read the log").events
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

    fn message(&self) -> &str {
        self.json()["message"]
            .as_str()
            .expect("the reply carries a message")
    }
}

/// Appends a run start to `repo`'s log, the way `shprd run start` does.
fn start_run(repo: &Path) {
    let path = repo.join(".shprd").join("events.jsonl");
    let mut log = EventLog::open(&path).expect("the log opens");
    log.append(
        Actor::Human,
        EventKind::RunStarted {
            run_id: "r1".to_string(),
            repo: repo.display().to_string(),
            base_branch: "main".to_string(),
            integration_branch: "integration/m10".to_string(),
        },
        &SystemClock,
    )
    .expect("the run starts");
}

/// Appends one event to a fixture's log, as a verb would.
fn append(fixture: &Fixture, kind: EventKind) {
    let path = fixture.repo.path().join(".shprd").join("events.jsonl");
    let mut log = EventLog::open(&path).expect("the log opens");
    log.append(Actor::Human, kind, &SystemClock)
        .expect("the event appends");
}

/// Runs one git command in `dir`, with a HOME and a config of its own.
fn git(dir: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "shprd tests")
        .env("GIT_AUTHOR_EMAIL", "tests@example.invalid")
        .env("GIT_COMMITTER_NAME", "shprd tests")
        .env("GIT_COMMITTER_EMAIL", "tests@example.invalid")
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?} in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A repository with `main`, an `integration/m10` cut from it, and
/// `shprd/r1/T1` cut from the integration branch, plus the run and task the
/// service reads.
///
/// It is the shape the changes endpoint is about: the branch a task branched
/// from (the run's integration branch, not its base branch) and work on its own
/// branch, with one file added and one changed. Another task lands ahead of T1
/// while its card waits, so a diff against the wrong base is visible as files
/// that are not T1's.
fn with_a_task_branch() -> Fixture {
    let repo = TempDir::new().expect("repo dir");
    let root = repo.path().to_path_buf();
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["config", "user.name", "shprd tests"]);
    git(&root, &["config", "user.email", "tests@example.invalid"]);
    git(&root, &["config", "commit.gpgsign", "false"]);
    fs::write(root.join("kept.txt"), "one\ntwo\nthree\n").expect("a seed file");
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "seed"]);
    git(&root, &["checkout", "-q", "-b", "integration/m10"]);
    git(&root, &["checkout", "-q", "-b", "shprd/r1/T1"]);
    fs::write(root.join("kept.txt"), "one\nTWO\nthree\nfour\n").expect("the change");
    fs::write(root.join("new.txt"), "hello\n").expect("a new file");
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "the task's work"]);
    git(&root, &["checkout", "-q", "integration/m10"]);
    // Another task lands after T1 was cut: its file is on the integration branch
    // but is not T1's work.
    fs::write(root.join("landed.txt"), "another task's work\n").expect("a landed file");
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "another task landed"]);

    start_run(&root);
    let fixture = Fixture::prepared(repo);
    append(
        &fixture,
        EventKind::TaskAdded {
            task: "T1".to_string(),
            title: "a task with work".to_string(),
            scope: vec!["kept.txt".to_string(), "new.txt".to_string()],
            acceptance: vec!["it works".to_string()],
            tags: Vec::new(),
            skills: Vec::new(),
            deps: Vec::new(),
        },
    );
    append(
        &fixture,
        EventKind::Spawned {
            task: "T1".to_string(),
            attempt: 1,
            agent: "claude".to_string(),
            model: None,
            worktree: root.display().to_string(),
            branch: "shprd/r1/T1".to_string(),
        },
    );
    // The attempt reports, so the task is a finished hand-in rather than work in
    // progress: exactly the state the accept card is read in.
    append(
        &fixture,
        EventKind::Reported {
            task: "T1".to_string(),
            attempt: 1,
            status: shprd_core::event::ReportStatus::Completed,
            summary: "changed the file and added one".to_string(),
            tests: vec!["cargo test --workspace: ok".to_string()],
            checks: vec![shprd_core::event::CheckReport::passed(
                "tests",
                "cargo test --workspace",
            )],
            changed_paths: vec!["kept.txt".to_string(), "new.txt".to_string()],
            commit: None,
            skills_used: Vec::new(),
            proofs: Vec::new(),
        },
    );
    fixture
}

/// One task's row in a `/api/state` body, by id.
fn task_in(state: &Reply, id: &str) -> Value {
    state.json()["tasks"]
        .as_array()
        .expect("the tasks")
        .iter()
        .find(|task| task["id"] == json!(id))
        .unwrap_or_else(|| panic!("{id}'s row is on the board: {}", state.json()))
        .clone()
}

/// What the closures were asked for, as a copy: a guard never crosses an await.
fn calls(log: &Calls) -> Vec<(String, String)> {
    log.lock().expect("the lock").clone()
}

/// An engine whose closures record what they were asked for and answer `start`.
fn recording_engine(answer: Result<&'static str, &'static str>) -> (Engine, Calls, Calls) {
    let started: Calls = Arc::default();
    let stopped: Calls = Arc::default();
    let start_seen = Arc::clone(&started);
    let stop_seen = Arc::clone(&stopped);
    let engine = Engine::new(Arc::new(move |repo: &Path, task: &str| {
        start_seen
            .lock()
            .expect("the lock")
            .push((repo.display().to_string(), task.to_string()));
        answer.map(str::to_string).map_err(str::to_string)
    }))
    .with_stop(Arc::new(move |repo: &Path, task: &str| {
        stop_seen
            .lock()
            .expect("the lock")
            .push((repo.display().to_string(), task.to_string()));
        answer.map(|_| ()).map_err(str::to_string)
    }));
    (engine, started, stopped)
}

// -- POST /api/tasks -------------------------------------------------------

#[tokio::test]
async fn the_person_adds_a_task_and_it_is_named_from_the_title() {
    let fixture = Fixture::new();
    let reply = fixture
        .add(json!({
            "title": "Add the board's add-task form",
            "scope": ["host/herdr-desktop"],
            "acceptance": ["a person can add a task from the panel"],
        }))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
    assert_eq!(reply.json()["ok"], json!(true));
    let task = &reply.json()["task"];
    assert_eq!(task["id"], json!("add-the-board-s-add-task-form"));
    assert_eq!(task["derived"]["status"], json!("to_do"));
    assert_eq!(task["scope"], json!(["host/herdr-desktop"]));

    // It is in the log the CLI reads, written as the person.
    let events = fixture.events();
    let added: Vec<&shprd_core::event::Event> = events
        .iter()
        .filter(|event| matches!(&event.kind, EventKind::TaskAdded { task, .. } if task.starts_with("add-the-board")))
        .collect();
    assert_eq!(added.len(), 1, "one task, one event");
    assert_eq!(
        added[0].actor,
        Actor::Human,
        "a task added from a board is the person's own"
    );
}

#[tokio::test]
async fn two_tasks_with_one_title_are_two_tasks() {
    let fixture = Fixture::new();
    let body = json!({
        "title": "Tabs",
        "scope": ["a"],
        "acceptance": ["it works"],
    });
    let first = fixture.add(body.clone()).await;
    assert_eq!(first.json()["task"]["id"], json!("tabs"));
    let second = fixture.add(body).await;
    assert_eq!(second.status, StatusCode::OK, "{:?}", second.body);
    assert_eq!(second.json()["task"]["id"], json!("tabs-2"));
}

#[tokio::test]
async fn a_caller_that_has_an_id_keeps_it_and_a_duplicate_is_refused() {
    let fixture = Fixture::new();
    let body = json!({
        "id": "M10.5",
        "title": "Mods B",
        "scope": ["host/"],
        "acceptance": ["it works"],
    });
    let first = fixture.add(body.clone()).await;
    assert_eq!(first.json()["task"]["id"], json!("M10.5"));
    let again = fixture.add(body).await;
    assert_eq!(again.status, StatusCode::CONFLICT);
    assert_eq!(again.code(), "refused");
    assert!(
        again.message().contains("M10.5"),
        "the refusal names the task: {}",
        again.message()
    );
}

#[tokio::test]
async fn adding_a_task_needs_the_write_header() {
    let fixture = Fixture::new();
    let reply = fixture
        .send_without_the_header(
            Method::POST,
            "/api/tasks",
            json!({"title": "x", "scope": ["a"], "acceptance": ["y"]}),
        )
        .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert_eq!(reply.code(), "missing_header");
    assert!(
        !fixture
            .events()
            .iter()
            .any(|event| matches!(&event.kind, EventKind::TaskAdded { .. })),
        "nothing was written"
    );
}

#[tokio::test]
async fn a_task_with_no_title_scope_or_acceptance_is_refused() {
    for body in [
        json!({"title": "   ", "scope": ["a"], "acceptance": ["y"]}),
        json!({"title": "x", "scope": [], "acceptance": ["y"]}),
        json!({"title": "x", "scope": ["a"], "acceptance": []}),
        json!({"title": "x", "scope": ["../escape"], "acceptance": ["y"]}),
        // Not a task at all: no title field to complain about.
        json!({"scope": ["a"], "acceptance": ["y"]}),
    ] {
        let fixture = Fixture::new();
        let reply = fixture.add(body.clone()).await;
        assert!(
            reply.status == StatusCode::CONFLICT || reply.status == StatusCode::BAD_REQUEST,
            "{body} answered {} {:?}",
            reply.status,
            reply.body
        );
        assert!(
            !fixture
                .events()
                .iter()
                .any(|event| matches!(&event.kind, EventKind::TaskAdded { .. })),
            "{body} was refused and wrote nothing"
        );
    }
}

// -- GET /api/tasks/{id}/changes -------------------------------------------

#[tokio::test]
async fn the_changes_of_a_task_are_its_branch_against_its_base() {
    let fixture = with_a_task_branch();
    let reply = fixture
        .send(Method::GET, "/api/tasks/T1/changes", None)
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
    let body = reply.json();
    assert_eq!(body["task"], json!("T1"));
    // The base is where the attempt was cut from — the run's integration branch,
    // not the branch the integration branch itself came from.
    assert_eq!(body["base"], json!("integration/m10"));
    assert_eq!(body["branch"], json!("shprd/r1/T1"));
    // The merge base is the seed commit both branches start at: the diff is the
    // task's own work, not everything that landed while it worked.
    let seed = {
        let out = std::process::Command::new("git")
            .args(["rev-parse", "integration/m10~1"])
            .current_dir(fixture.repo.path())
            .env("HOME", fixture.repo.path())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .expect("git runs");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    assert_eq!(body["merge_base"], json!(seed));

    let files = body["files"].as_array().expect("the files");
    assert_eq!(files.len(), 2, "{files:?}");
    let new = files
        .iter()
        .find(|file| file["path"] == json!("new.txt"))
        .expect("the new file");
    assert_eq!(new["added"], json!(1));
    assert_eq!(new["deleted"], json!(0));
    assert_eq!(new["binary"], json!(false));
    let kept = files
        .iter()
        .find(|file| file["path"] == json!("kept.txt"))
        .expect("the changed file");
    assert_eq!(kept["added"], json!(2), "TWO and four");
    assert_eq!(kept["deleted"], json!(1), "two");
    assert_eq!(body["added"], json!(3));
    assert_eq!(body["deleted"], json!(1));
}

/// Bug 8 of the M10 gate: another task lands on the integration branch after
/// this one branched, and the card must still list only this task's files.
#[tokio::test]
async fn changes_after_another_task_landed_list_only_this_tasks_files() {
    let fixture = with_a_task_branch();
    // The integration branch has moved ahead of T1's cut point.
    let landed = {
        let out = std::process::Command::new("git")
            .args(["show", "--name-only", "--format=", "integration/m10"])
            .current_dir(fixture.repo.path())
            .env("HOME", fixture.repo.path())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .expect("git runs");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    assert_eq!(
        landed, "landed.txt",
        "another task's file is ahead on the base"
    );

    let reply = fixture
        .send(Method::GET, "/api/tasks/T1/changes", None)
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
    let body = reply.json();
    let paths: Vec<&str> = body["files"]
        .as_array()
        .expect("the files")
        .iter()
        .map(|file| file["path"].as_str().expect("a path"))
        .collect();
    assert_eq!(paths, vec!["kept.txt", "new.txt"], "{body}");
    assert!(
        !paths.contains(&"landed.txt"),
        "the card lists the other task's file: {body}"
    );
}

#[tokio::test]
async fn a_task_with_no_attempt_has_nothing_to_compare() {
    let fixture = Fixture::new();
    let added = fixture
        .add(json!({"title": "fresh", "scope": ["a"], "acceptance": ["y"]}))
        .await;
    assert_eq!(added.status, StatusCode::OK);
    let reply = fixture
        .send(Method::GET, "/api/tasks/fresh/changes", None)
        .await;
    assert_eq!(reply.status, StatusCode::CONFLICT);
    assert_eq!(reply.code(), "refused");
    assert!(
        reply.message().contains("no attempt"),
        "the refusal says why: {}",
        reply.message()
    );
}

#[tokio::test]
async fn the_changes_of_an_unknown_task_are_a_typed_404() {
    let fixture = Fixture::new();
    let reply = fixture
        .send(Method::GET, "/api/tasks/no-such-task/changes", None)
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(reply.code(), "unknown_task");
}

#[tokio::test]
async fn a_branch_that_is_gone_is_refused_rather_than_answered_empty() {
    let fixture = with_a_task_branch();
    git(
        fixture.repo.path(),
        &["branch", "-m", "shprd/r1/T1", "elsewhere"],
    );
    let reply = fixture
        .send(Method::GET, "/api/tasks/T1/changes", None)
        .await;
    assert_eq!(reply.status, StatusCode::CONFLICT);
    assert_eq!(reply.code(), "refused");
    assert!(
        reply.message().contains("shprd/r1/T1"),
        "the refusal names the branch: {}",
        reply.message()
    );
}

// -- the person's verbs on a task ------------------------------------------

#[tokio::test]
async fn a_start_and_a_retry_go_through_the_engines_spawn_path() {
    let fixture = with_a_task_branch();
    let (engine, started, _) = recording_engine(Ok("started the worker"));
    let fixture = Fixture {
        app: fixture.app.with_engine(engine),
        ..fixture
    };
    let start = fixture
        .act(json!({"kind": "task.start", "task": "T1"}))
        .await;
    assert_eq!(start.status, StatusCode::OK, "{:?}", start.body);
    assert_eq!(start.json()["task"]["id"], json!("T1"));
    // The engine is the same spawn path the CLI has: it was asked for the repo
    // and the task, and nothing was invented here.
    let seen = calls(&started);
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].1, "T1");
    assert_eq!(seen[0].0, fixture.repo.path().display().to_string());

    let retry = fixture
        .act(json!({"kind": "task.retry", "task": "T1"}))
        .await;
    assert_eq!(retry.status, StatusCode::OK, "{:?}", retry.body);
    assert_eq!(calls(&started).len(), 2, "a retry is the same spawn path");
}

#[tokio::test]
async fn a_start_without_an_engine_is_refused_and_says_why() {
    let fixture = Fixture::new();
    let reply = fixture
        .act(json!({"kind": "task.start", "task": "T1"}))
        .await;
    assert_eq!(reply.status, StatusCode::CONFLICT);
    assert_eq!(reply.code(), "refused");
    assert!(
        reply.message().contains("without a host"),
        "the refusal says why: {}",
        reply.message()
    );
}

#[tokio::test]
async fn an_engine_failure_is_the_persons_refusal() {
    let fixture = Fixture::new();
    let (engine, _, _) = recording_engine(Err("no installed worker CLI: pass --agent"));
    let reply = fixture
        .app
        .with_engine(engine)
        .router()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/action")
                .header(header::AUTHORIZATION, format!("Bearer {}", fixture.token))
                .header("x-shprd", "1")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"kind": "task.start", "task": "T1"}).to_string(),
                ))
                .expect("the request"),
        )
        .await
        .expect("an answer");
    let status = reply.status();
    let bytes = reply
        .into_body()
        .collect()
        .await
        .expect("the body reads")
        .to_bytes();
    let body: Value = serde_json::from_slice(&bytes).expect("JSON");
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], json!("refused"));
    assert!(
        body["message"]
            .as_str()
            .expect("a message")
            .contains("no installed worker CLI"),
        "{body}"
    );
}

#[tokio::test]
async fn a_stop_closes_the_tasks_space_and_an_unknown_task_is_a_404() {
    let fixture = with_a_task_branch();
    let (engine, _, stopped) = recording_engine(Ok("closed the space"));
    let fixture = Fixture {
        app: fixture.app.with_engine(engine),
        ..fixture
    };
    let stop = fixture
        .act(json!({"kind": "task.stop", "task": "T1"}))
        .await;
    assert_eq!(stop.status, StatusCode::OK, "{:?}", stop.body);
    assert_eq!(calls(&stopped).len(), 1);
    let unknown = fixture
        .act(json!({"kind": "task.stop", "task": "T9"}))
        .await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    assert_eq!(unknown.code(), "unknown_task");
    assert_eq!(
        calls(&stopped).len(),
        1,
        "nothing was closed for a task the run does not hold"
    );
}

/// A service over a repository whose one task has a spawned attempt and no
/// report: exactly the state a person's Stop is pressed in.
fn with_a_working_task() -> Fixture {
    let repo = TempDir::new().expect("repo dir");
    start_run(repo.path());
    let fixture = Fixture::prepared(repo);
    append(
        &fixture,
        EventKind::TaskAdded {
            task: "T1".to_string(),
            title: "a task at work".to_string(),
            scope: vec!["kept.txt".to_string()],
            acceptance: vec!["it works".to_string()],
            tags: Vec::new(),
            skills: Vec::new(),
            deps: Vec::new(),
        },
    );
    append(
        &fixture,
        EventKind::Spawned {
            task: "T1".to_string(),
            attempt: 1,
            agent: "claude".to_string(),
            model: None,
            worktree: "/wt/r1/T1".to_string(),
            branch: "shprd/r1/T1".to_string(),
        },
    );
    fixture
}

#[tokio::test]
async fn a_stop_of_a_working_task_moves_to_needs_you_and_a_retry_is_allowed() {
    // Bug 1 of the M10 gate: the stop closed the space and wrote nothing, so the
    // task read Working for ever and every retry was refused with "attempt 1
    // cannot be archived and replaced until it fails or is integrated".
    let fixture = with_a_working_task();
    let (engine, started, stopped) = recording_engine(Ok("closed the space"));
    let fixture = Fixture {
        app: fixture.app.with_engine(engine),
        ..fixture
    };
    let before = fixture.send(Method::GET, "/api/state", None).await;
    assert_eq!(before.status, StatusCode::OK, "{:?}", before.body);
    assert_eq!(
        task_in(&before, "T1")["derived"]["status"],
        json!("working"),
        "a spawned task reads working: {}",
        before.json()
    );

    let stop = fixture
        .act(json!({"kind": "task.stop", "task": "T1"}))
        .await;
    assert_eq!(stop.status, StatusCode::OK, "{:?}", stop.body);
    assert_eq!(calls(&stopped).len(), 1, "the space was closed once");
    // It reads *Needs you · Agent stopped*, Shepherdr's words for a task with no
    // agent left (M7a §5, `agents.py:170`).
    assert_eq!(stop.json()["task"]["derived"]["status"], json!("needs_you"));
    assert_eq!(stop.json()["task"]["derived"]["need"], json!("stopped"));
    assert_eq!(
        stop.json()["task"]["derived"]["reason"],
        json!("Agent stopped")
    );
    // The stored state is the one a retry may replace, which is what makes the
    // row's own Retry verb work again.
    assert_eq!(stop.json()["task"]["status"], json!("failed"));

    // A retry is the engine's spawn path, and it is no longer refused.
    let retry = fixture
        .act(json!({"kind": "task.retry", "task": "T1"}))
        .await;
    assert_eq!(retry.status, StatusCode::OK, "{:?}", retry.body);
    assert_eq!(calls(&started).len(), 1, "a retry is the spawn path");

    // The stop is in the log, as the person's own act.
    let recorded = fixture
        .events()
        .into_iter()
        .find_map(|event| match event.kind {
            EventKind::TaskStopped { task, attempt } => Some((event.actor, task, attempt)),
            _ => None,
        })
        .expect("the stop is recorded");
    assert_eq!(recorded.0, Actor::Human);
    assert_eq!(recorded.1, "T1");
    assert_eq!(recorded.2, 1);
}

#[tokio::test]
async fn a_second_stop_is_refused_with_its_reason() {
    let fixture = with_a_working_task();
    let (engine, _, stopped) = recording_engine(Ok("closed the space"));
    let fixture = Fixture {
        app: fixture.app.with_engine(engine),
        ..fixture
    };
    let first = fixture
        .act(json!({"kind": "task.stop", "task": "T1"}))
        .await;
    assert_eq!(first.status, StatusCode::OK, "{:?}", first.body);

    let again = fixture
        .act(json!({"kind": "task.stop", "task": "T1"}))
        .await;
    assert_eq!(again.status, StatusCode::CONFLICT, "{:?}", again.body);
    assert_eq!(again.code(), "refused");
    assert!(
        again.message().contains("already stopped"),
        "the refusal says why: {}",
        again.message()
    );
    assert!(
        again.message().contains("task.retry"),
        "and what to do instead: {}",
        again.message()
    );
    assert_eq!(
        calls(&stopped).len(),
        1,
        "a refused stop closes nothing: the host is not touched"
    );
    // One event, not two: the refused press wrote nothing.
    assert_eq!(
        fixture
            .events()
            .iter()
            .filter(|event| matches!(event.kind, EventKind::TaskStopped { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn a_stop_of_a_task_nobody_started_is_refused() {
    let fixture = Fixture::new();
    fixture
        .add(json!({"title": "not started", "scope": ["a"], "acceptance": ["y"]}))
        .await;
    let (engine, _, stopped) = recording_engine(Ok("closed the space"));
    let fixture = Fixture {
        app: fixture.app.with_engine(engine),
        ..fixture
    };
    let reply = fixture
        .act(json!({"kind": "task.stop", "task": "not-started"}))
        .await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{:?}", reply.body);
    assert!(
        reply.message().contains("nothing has been started"),
        "the refusal says why: {}",
        reply.message()
    );
    assert_eq!(calls(&stopped).len(), 0, "nothing was closed");
}

#[tokio::test]
async fn a_stopped_task_reads_the_same_from_the_cli_and_the_board() {
    // The one thing two readers must not disagree about: `shprd status` derives
    // from the fold alone, and the board reads the snapshot. A stopped attempt
    // carries the fact, so both say *Agent stopped*.
    let fixture = with_a_working_task();
    let (engine, _, _) = recording_engine(Ok("closed the space"));
    let fixture = Fixture {
        app: fixture.app.with_engine(engine),
        ..fixture
    };
    let stop = fixture
        .act(json!({"kind": "task.stop", "task": "T1"}))
        .await;
    assert_eq!(stop.status, StatusCode::OK, "{:?}", stop.body);

    let events = fixture.events();
    let state = shprd_core::state::RunState::fold(&events).expect("the log folds");
    let reading =
        shprd_views::status::reading(&state, "T1", 1_700_000_000_000).expect("the task reads");
    assert_eq!(
        reading.needs,
        Some(shprd_views::derived::Need::Stopped),
        "the CLI's reader says the same: {reading:?}"
    );
    assert_eq!(reading.reason.as_deref(), Some("Agent stopped"));
}

#[tokio::test]
async fn dropping_a_task_records_the_persons_reason_once() {
    let fixture = Fixture::new();
    let add = fixture
        .add(json!({"title": "not needed", "scope": ["a"], "acceptance": ["y"]}))
        .await;
    assert_eq!(add.status, StatusCode::OK);
    let drop = fixture
        .act(json!({
            "kind": "task.drop",
            "task": "not-needed",
            "reason": "the scope moved to another task",
        }))
        .await;
    assert_eq!(drop.status, StatusCode::OK, "{:?}", drop.body);
    assert_eq!(drop.json()["task"]["status"], json!("dropped"));
    assert_eq!(
        drop.json()["task"]["dropped_reason"],
        json!("the scope moved to another task")
    );
    // A second drop is a wrong state, and the task stays as it was.
    let again = fixture
        .act(json!({"kind": "task.drop", "task": "not-needed", "reason": "again"}))
        .await;
    assert_eq!(again.status, StatusCode::CONFLICT);
    assert_eq!(again.code(), "refused");
}

#[tokio::test]
async fn a_drop_with_no_reason_is_refused() {
    let fixture = Fixture::new();
    fixture
        .add(json!({"title": "some task", "scope": ["a"], "acceptance": ["y"]}))
        .await;
    let reply = fixture
        .act(json!({"kind": "task.drop", "task": "some-task", "reason": "  "}))
        .await;
    assert_eq!(reply.status, StatusCode::CONFLICT);
    assert_eq!(reply.code(), "refused");
}

#[tokio::test]
async fn accepting_a_task_pins_the_head_its_branch_is_at() {
    let fixture = with_a_task_branch();
    let reply = fixture
        .act(json!({"kind": "task.accept", "task": "T1"}))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
    let tip = {
        let out = std::process::Command::new("git")
            .args(["rev-parse", "shprd/r1/T1"])
            .current_dir(fixture.repo.path())
            .env("HOME", fixture.repo.path())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .expect("git runs");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let approved = fixture
        .events()
        .into_iter()
        .find_map(|event| match event.kind {
            EventKind::TaskApproved {
                task,
                number,
                head_sha,
            } => Some((task, number, head_sha)),
            _ => None,
        })
        .expect("the approval was recorded");
    assert_eq!(approved.0, "T1");
    assert_eq!(approved.1, 0, "a local landing has no pull request number");
    assert_eq!(
        approved.2, tip,
        "the approval names the commit it was given for"
    );
}

#[tokio::test]
async fn accepting_a_task_with_no_attempt_is_refused() {
    let fixture = Fixture::new();
    fixture
        .add(json!({"title": "fresh", "scope": ["a"], "acceptance": ["y"]}))
        .await;
    let reply = fixture
        .act(json!({"kind": "task.accept", "task": "fresh"}))
        .await;
    assert_eq!(reply.status, StatusCode::CONFLICT);
    assert!(
        reply.message().contains("no attempt"),
        "the refusal says why: {}",
        reply.message()
    );
}

#[tokio::test]
async fn accepting_is_refused_where_nothing_waits_for_an_acceptance() {
    let fixture = with_a_task_branch().settings("[tasks]\nmerge = \"auto\"\n");
    let reply = fixture
        .act(json!({"kind": "task.accept", "task": "T1"}))
        .await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{:?}", reply.body);
    assert_eq!(reply.code(), "refused");
    assert!(
        reply.message().contains("tasks.merge"),
        "the refusal names the setting: {}",
        reply.message()
    );
    assert!(
        !fixture
            .events()
            .iter()
            .any(|event| matches!(&event.kind, EventKind::TaskApproved { .. })),
        "nothing was written"
    );
}

#[tokio::test]
async fn accepting_is_allowed_where_the_policy_asks() {
    let fixture = with_a_task_branch().settings("[tasks]\nmerge = \"ask\"\n");
    let reply = fixture
        .act(json!({"kind": "task.accept", "task": "T1"}))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
}

// -- what the board reads back ---------------------------------------------

#[tokio::test]
async fn a_tasks_pull_request_reaches_the_board() {
    let fixture = with_a_task_branch();
    append(
        &fixture,
        EventKind::TaskPrOpened {
            task: "T1".to_string(),
            number: 42,
            url: "https://example.invalid/pr/42".to_string(),
            head: "shprd/r1/T1".to_string(),
        },
    );
    let state = fixture.send(Method::GET, "/api/state", None).await;
    assert_eq!(state.status, StatusCode::OK);
    let task = state.json()["tasks"]
        .as_array()
        .expect("the tasks")
        .iter()
        .find(|task| task["id"] == json!("T1"))
        .expect("T1's row");
    let pr = &task["pr"];
    assert_eq!(pr["number"], json!(42));
    assert_eq!(pr["state"], json!("open"));
    assert_eq!(pr["url"], json!("https://example.invalid/pr/42"));
    assert_eq!(pr["approved"], json!(false));
}
