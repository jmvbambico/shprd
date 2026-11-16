//! The chat route's acceptance tests: cursor paging over a recorded session,
//! the space a page is read for, and a turn typed back into the agent.
//!
//! Nothing here needs a terminal, a model or a network. Each test writes its own
//! home — a settings file naming the coordinator's program and a recorded
//! session under it — so the service reads a transcript a test wrote and never
//! the developer's own. The send path is proved with a [`ChatWriter`] that
//! records what would have been typed.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use shprd_agents::spec::TranscriptFormat;
use shprd_agents::transcript::cwd_slug;
use shprd_core::event::{Actor, EventKind};
use shprd_core::log::{EventLog, SystemClock};
use shprd_server::{App, ChatWriter};
use tempfile::TempDir;
use tower::ServiceExt;

/// The port the fixtures claim in their `Origin` and `service.json`.
const PORT: u16 = 18_082;

/// A chat writer that keeps every line it was asked to type.
#[derive(Debug, Default)]
struct Recorder {
    lines: Mutex<Vec<(Option<String>, String)>>,
    refuse: bool,
}

impl Recorder {
    /// What was typed, as `(space, text)`.
    fn lines(&self) -> Vec<(Option<String>, String)> {
        self.lines.lock().unwrap().clone()
    }
}

impl ChatWriter for Recorder {
    fn send(&self, space: Option<&str>, text: &str) -> Result<(), String> {
        if self.refuse {
            return Err("no open pane for the coordinator".to_string());
        }
        self.lines
            .lock()
            .unwrap()
            .push((space.map(str::to_owned), text.to_owned()));
        Ok(())
    }
}

/// One prepared service, its temp directories and the writer it types through.
struct Fixture {
    app: App,
    token: String,
    repo: TempDir,
    home: TempDir,
    state: TempDir,
    recorder: Arc<Recorder>,
}

impl Fixture {
    /// A service whose coordinator runs `program`, with a run started.
    fn new(program: &str) -> Self {
        let repo = tempfile::tempdir().expect("repo dir");
        start_run(repo.path());
        let home = tempfile::tempdir().expect("home dir");
        write_settings(home.path(), program);
        Self::prepared(repo, home)
    }

    /// [`Fixture::new`]'s service, with a recorder it types through.
    fn prepared(repo: TempDir, home: TempDir) -> Self {
        Self::prepared_with(repo, home, Arc::new(Recorder::default()))
    }

    /// A service over `repo` and `home`, typing through `recorder`.
    fn prepared_with(repo: TempDir, home: TempDir, recorder: Arc<Recorder>) -> Self {
        let state = tempfile::tempdir().expect("state dir");
        let writer: Arc<dyn ChatWriter> = recorder.clone();
        let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data");
        let app = App::prepare(repo.path(), PORT, &Actor::Human, state.path(), None)
            .expect("prepare")
            .with_home(home.path().to_path_buf())
            // The agents' own records live under the home *they* ran with, which
            // in this fixture is the same temp home the settings live in: a
            // transcript a test wrote is found, and the developer's real
            // `~/.claude` is never read.
            .with_transcript_roots(shprd_agents::transcript::Roots::at(home.path()))
            .with_data(data)
            .with_prompts_dir(home.path().join(".config").join("shprd").join("prompts"))
            .with_chat_writer(writer);
        let token = std::fs::read_to_string(app.info().token_path)
            .expect("the token file")
            .trim()
            .to_string();
        Self {
            app,
            token,
            repo,
            home,
            state,
            recorder,
        }
    }

    /// The service with a host that refuses every write, as a space with no
    /// open pane does.
    fn with_a_refusing_host(mut self) -> Self {
        self.app = self.app.with_chat_writer(Arc::new(Recorder {
            refuse: true,
            ..Recorder::default()
        }));
        self
    }

    /// Where the coordinator's recorded session lives for this repository.
    fn coordinator_record(&self) -> PathBuf {
        let slug = cwd_slug(TranscriptFormat::ClaudeJsonl, self.repo.path());
        self.home
            .path()
            .join(".claude")
            .join("projects")
            .join(slug)
            .join("s1.jsonl")
    }

    /// Writes a claude session with `messages` for the directory `cwd`'s slug.
    fn write_session(&self, cwd: &Path, name: &str, messages: &[(bool, &str)]) {
        // The session's folder is the slug the reader looks for, which is the
        // one for the resolved directory: this is the same rule, so a fixture
        // and a lookup cannot disagree about which folder a session is in.
        let slug = cwd_slug(TranscriptFormat::ClaudeJsonl, cwd);
        let dir = self.home.path().join(".claude").join("projects").join(slug);
        std::fs::create_dir_all(&dir).unwrap();
        let mut text = String::new();
        for (from_the_person, words) in messages {
            let role = if *from_the_person {
                "user"
            } else {
                "assistant"
            };
            let value = json!({
                "type": role,
                "isSidechain": false,
                "timestamp": "2026-11-04T10:00:00.000Z",
                "message": {"role": role, "content": words},
            });
            text.push_str(&value.to_string());
            text.push('\n');
        }
        std::fs::write(dir.join(name), text).unwrap();
    }

    /// Appends one message to the coordinator's record, as a CLI would.
    fn append(&self, from_the_person: bool, words: &str) {
        use std::io::Write;
        let role = if from_the_person { "user" } else { "assistant" };
        let value = json!({
            "type": role,
            "isSidechain": false,
            "message": {"role": role, "content": words},
        });
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(self.coordinator_record())
            .unwrap();
        writeln!(file, "{value}").unwrap();
    }

    /// Adds the task `id` and starts attempt 1 of it in `worktree`.
    fn spawn_task(&self, id: &str, agent: &str, worktree: &Path) {
        let log = self.repo.path().join(".shprd").join("events.jsonl");
        let mut log = EventLog::open(&log).expect("the log opens");
        log.append(
            Actor::Orchestrator,
            EventKind::TaskAdded {
                task: id.to_string(),
                title: format!("task {id}"),
                scope: vec!["crates/shprd-core".to_string()],
                acceptance: vec!["it works".to_string()],
                tags: Vec::new(),
                skills: Vec::new(),
                deps: Vec::new(),
            },
            &SystemClock,
        )
        .expect("the task is added");
        log.append(
            Actor::Engine,
            EventKind::Spawned {
                task: id.to_string(),
                attempt: 1,
                agent: agent.to_string(),
                model: None,
                worktree: worktree.display().to_string(),
                branch: format!("work/{id}"),
            },
            &SystemClock,
        )
        .expect("the attempt is spawned");
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
        let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        Reply { status, body }
    }

    /// `GET /api/chat` for `space`, from `since`.
    async fn chat(&self, space: &str, since: &str) -> Reply {
        self.send(
            Call::get(&format!("/api/chat?space={space}&since={since}")).with_token(&self.token),
        )
        .await
    }
}

/// One request being built.
struct Call {
    method: Method,
    uri: String,
    token: Option<String>,
    write_header: bool,
    body: Option<Value>,
}

impl Call {
    fn get(uri: &str) -> Self {
        Self {
            method: Method::GET,
            uri: uri.to_string(),
            token: None,
            write_header: false,
            body: None,
        }
    }

    fn post(uri: &str, body: Value) -> Self {
        Self {
            method: Method::POST,
            uri: uri.to_string(),
            token: None,
            write_header: false,
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
        let body = match &self.body {
            Some(value) => {
                builder = builder.header(header::CONTENT_TYPE, "application/json");
                Body::from(value.to_string())
            }
            None => Body::empty(),
        };
        builder.body(body).expect("the request builds")
    }
}

/// One answer.
struct Reply {
    status: StatusCode,
    body: Value,
}

impl Reply {
    fn note(&self) -> &str {
        self.body
            .get("note")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    fn messages(&self) -> &[Value] {
        self.body
            .get("messages")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    fn cursor(&self) -> String {
        self.body
            .get("cursor")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// Starts a run, the log's first event every rule needs.
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

/// Writes the person's settings, naming `program` as the coordinator.
///
/// `roles.coordinator.agent` is the key the settings page and `shprd settings
/// set roles.coordinator.agent <program>` write, so it is the one a chat has to
/// read: a test that used another id would pass while a person's real file
/// failed.
fn write_settings(home: &Path, program: &str) {
    write_settings_toml(home, &format!("[roles.coordinator]\nagent = {program:?}\n"));
}

/// Writes `settings.toml` exactly as given.
fn write_settings_toml(home: &Path, text: &str) {
    let dir = home.join(".config").join("shprd");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("settings.toml"), text).unwrap();
}

#[tokio::test]
async fn a_coordinator_page_reads_the_agents_record() {
    let fixture = Fixture::new("claude");
    fixture.write_session(
        fixture.repo.path(),
        "s1.jsonl",
        &[
            (true, "What did the last pass do?"),
            (false, "It landed T1 and started T2."),
        ],
    );
    let reply = fixture.chat("coordinator", "").await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body["space"], "coordinator");
    assert_eq!(reply.body["program"], "claude");
    assert_eq!(reply.body["session"], "s1");
    assert_eq!(reply.body["more"], false);
    assert_eq!(reply.messages().len(), 2);
    assert_eq!(reply.messages()[0]["role"], "user");
    assert_eq!(reply.messages()[0]["text"], "What did the last pass do?");
    assert_eq!(reply.messages()[1]["role"], "assistant");
    assert!(reply.messages()[1]["at_ms"].is_i64());
}

#[tokio::test]
async fn a_cursor_pages_forward_and_a_repeated_cursor_sees_nothing_again() {
    let fixture = Fixture::new("claude");
    fixture.write_session(fixture.repo.path(), "s1.jsonl", &[(true, "first")]);
    let first = fixture.chat("coordinator", "").await;
    assert_eq!(first.messages().len(), 1);
    fixture.append(false, "second");
    fixture.append(true, "third");
    let second = fixture.chat("coordinator", &first.cursor()).await;
    assert_eq!(second.messages().len(), 2, "{}", second.body);
    assert_eq!(second.messages()[1]["text"], "third");
    let again = fixture.chat("coordinator", &second.cursor()).await;
    assert!(again.messages().is_empty());
    assert_eq!(again.cursor(), second.cursor());
    assert_eq!(again.note(), "nothing new");
}

#[tokio::test]
async fn a_long_record_pages_with_more_and_then_the_rest() {
    let fixture = Fixture::new("claude");
    let messages: Vec<(bool, String)> = (0..250).map(|n| (true, format!("message {n}"))).collect();
    let borrowed: Vec<(bool, &str)> = messages
        .iter()
        .map(|(who, text)| (*who, text.as_str()))
        .collect();
    fixture.write_session(fixture.repo.path(), "s1.jsonl", &borrowed);
    let first = fixture.chat("coordinator", "").await;
    assert_eq!(first.messages().len(), 200);
    assert_eq!(first.body["more"], true);
    let second = fixture.chat("coordinator", &first.cursor()).await;
    assert_eq!(second.messages().len(), 50);
    assert_eq!(second.body["more"], false);
    assert_eq!(second.messages()[49]["text"], "message 249");
}

#[tokio::test]
async fn a_task_space_reads_its_workers_record_from_its_worktree() {
    let fixture = Fixture::new("pi");
    let worktree = fixture.home.path().join("worktrees").join("T1");
    std::fs::create_dir_all(&worktree).unwrap();
    fixture.spawn_task("T1", "claude", &worktree);
    fixture.write_session(&worktree, "worker.jsonl", &[(false, "T1 is done.")]);
    let reply = fixture.chat("T1", "").await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body["space"], "T1");
    assert_eq!(reply.body["program"], "claude");
    assert_eq!(reply.body["session"], "worker");
    assert_eq!(reply.messages()[0]["text"], "T1 is done.");
}

#[tokio::test]
async fn a_task_that_never_started_says_so_rather_than_answering_nothing() {
    let fixture = Fixture::new("claude");
    let log = fixture.repo.path().join(".shprd").join("events.jsonl");
    let mut log = EventLog::open(&log).expect("the log opens");
    log.append(
        Actor::Orchestrator,
        EventKind::TaskAdded {
            task: "T9".to_string(),
            title: "queued".to_string(),
            scope: vec!["crates/shprd-core".to_string()],
            acceptance: vec!["it works".to_string()],
            tags: Vec::new(),
            skills: Vec::new(),
            deps: Vec::new(),
        },
        &SystemClock,
    )
    .expect("the task is added");
    let reply = fixture.chat("T9", "").await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert!(reply.messages().is_empty());
    assert!(reply.note().contains("has not started"), "{}", reply.note());
}

#[tokio::test]
async fn a_task_nobody_added_is_a_typed_refusal() {
    let fixture = Fixture::new("claude");
    let reply = fixture.chat("T404", "").await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.body);
    assert_eq!(reply.body["code"], "unknown_task");
}

#[tokio::test]
async fn a_program_with_no_reader_says_so() {
    let fixture = Fixture::new("kilo");
    let reply = fixture.chat("coordinator", "").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(
        reply.note().contains("cannot read kilo"),
        "{}",
        reply.note()
    );
}

#[tokio::test]
async fn the_harness_table_is_read_from_the_copy_the_binary_ships() {
    // An installed `shprd` has no data directory at all: the table comes from
    // the copy embedded at build time. The chat has to find a program's
    // transcript rule there, or a chat would only work in a checkout.
    let repo = tempfile::tempdir().expect("repo dir");
    start_run(repo.path());
    let home = tempfile::tempdir().expect("home dir");
    write_settings(home.path(), "claude");
    let state = tempfile::tempdir().expect("state dir");
    let app = App::prepare(repo.path(), PORT, &Actor::Human, state.path(), None)
        .expect("prepare")
        .with_home(home.path().to_path_buf())
        .with_transcript_roots(shprd_agents::transcript::Roots::at(home.path()))
        .with_data_dir(shprd_agents::data::DataDir::resolve(None, None));
    let token = std::fs::read_to_string(app.info().token_path)
        .expect("the token file")
        .trim()
        .to_string();
    let fixture = Fixture {
        app,
        token,
        repo,
        home,
        state,
        recorder: Arc::new(Recorder::default()),
    };
    let reply = fixture.chat("coordinator", "").await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body["program"], "claude");
    assert!(
        reply.note().contains("has not written a session"),
        "the program's transcript rule was found, so the only thing missing is a session: {}",
        reply.note()
    );
}

#[tokio::test]
async fn a_coordinator_with_no_program_says_which_key_to_set() {
    let repo = tempfile::tempdir().expect("repo dir");
    start_run(repo.path());
    let home = tempfile::tempdir().expect("home dir");
    let fixture = Fixture::prepared(repo, home);
    let reply = fixture.chat("coordinator", "").await;
    assert!(
        reply.note().contains("roles.coordinator.agent"),
        "the advice names the key the settings model uses: {}",
        reply.note()
    );
}

#[tokio::test]
async fn a_role_that_is_not_the_coordinator_is_not_the_coordinators_agent() {
    // `orchestrator` is not a role id the settings model uses: a person's
    // custom helper by that name must not become the chat's agent, which is
    // exactly what the first cut of this route did.
    let repo = tempfile::tempdir().expect("repo dir");
    start_run(repo.path());
    let home = tempfile::tempdir().expect("home dir");
    write_settings_toml(
        home.path(),
        "[roles.orchestrator]\nagent = \"claude\"\nkind = \"helper\"\n",
    );
    let fixture = Fixture::prepared(repo, home);
    let reply = fixture.chat("coordinator", "").await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body["program"], Value::Null);
    assert!(
        reply.note().contains("no coordinator program is set"),
        "the helper role is not the coordinator: {}",
        reply.note()
    );
}

#[tokio::test]
async fn a_custom_role_of_kind_coordinator_is_the_coordinators_agent() {
    // A role of kind `coordinator` is the coordinator wherever it sits in the
    // order, which is how `shprd doctor` reads it too.
    let repo = tempfile::tempdir().expect("repo dir");
    start_run(repo.path());
    let home = tempfile::tempdir().expect("home dir");
    write_settings_toml(
        home.path(),
        "[roles]\norder = [\"lead\", \"coordinator\", \"worker\", \"reviewer\", \"backup\"]\n\n\
         [roles.lead]\nagent = \"claude\"\nkind = \"coordinator\"\n",
    );
    let fixture = Fixture::prepared(repo, home);
    fixture.write_session(fixture.repo.path(), "s1.jsonl", &[(true, "hello")]);
    let reply = fixture.chat("coordinator", "").await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body["program"], "claude");
    assert_eq!(reply.messages()[0]["text"], "hello");
}

#[tokio::test]
async fn an_agent_that_never_wrote_a_session_says_so() {
    let fixture = Fixture::new("claude");
    let reply = fixture.chat("coordinator", "").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(
        reply.note().contains("has not written a session"),
        "{}",
        reply.note()
    );
}

#[tokio::test]
async fn sending_types_the_line_into_the_coordinators_pane() {
    let fixture = Fixture::new("claude");
    let reply = fixture
        .send(
            Call::post(
                "/api/chat",
                json!({"space": "coordinator", "text": "  hello  "}),
            )
            .with_token(&fixture.token)
            .with_write_header(),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body["sent"], true);
    assert_eq!(
        fixture.recorder.lines(),
        [(None, "hello".to_string())],
        "the coordinator is the space with no task id, and its words are trimmed"
    );
}

#[tokio::test]
async fn sending_to_a_task_names_that_task_and_an_unknown_one_is_refused_before_typing() {
    let fixture = Fixture::new("claude");
    let worktree = fixture.home.path().join("worktrees").join("T1");
    std::fs::create_dir_all(&worktree).unwrap();
    fixture.spawn_task("T1", "claude", &worktree);
    let reply = fixture
        .send(
            Call::post(
                "/api/chat",
                json!({"space": "T1", "text": "how is it going?"}),
            )
            .with_token(&fixture.token)
            .with_write_header(),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(
        fixture.recorder.lines(),
        [(Some("T1".to_string()), "how is it going?".to_string())]
    );
    let refused = fixture
        .send(
            Call::post("/api/chat", json!({"space": "T404", "text": "hello"}))
                .with_token(&fixture.token)
                .with_write_header(),
        )
        .await;
    assert_eq!(refused.status, StatusCode::NOT_FOUND);
    assert_eq!(
        fixture.recorder.lines().len(),
        1,
        "nothing was typed for a task that does not exist"
    );
}

#[tokio::test]
async fn sending_an_empty_line_is_refused() {
    let fixture = Fixture::new("claude");
    let reply = fixture
        .send(
            Call::post("/api/chat", json!({"space": "coordinator", "text": "   "}))
                .with_token(&fixture.token)
                .with_write_header(),
        )
        .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    assert!(fixture.recorder.lines().is_empty());
}

#[tokio::test]
async fn sending_is_a_write_so_it_needs_the_header_the_token_and_a_body_of_json() {
    let fixture = Fixture::new("claude");
    let no_header = fixture
        .send(
            Call::post("/api/chat", json!({"space": "coordinator", "text": "hi"}))
                .with_token(&fixture.token),
        )
        .await;
    assert_eq!(no_header.status, StatusCode::FORBIDDEN);
    let no_token = fixture
        .send(
            Call::post("/api/chat", json!({"space": "coordinator", "text": "hi"}))
                .with_write_header(),
        )
        .await;
    assert_eq!(no_token.status, StatusCode::UNAUTHORIZED);
    let not_json = Call {
        method: Method::POST,
        uri: "/api/chat".to_string(),
        token: Some(fixture.token.clone()),
        write_header: true,
        body: None,
    };
    let malformed = fixture.send(not_json).await;
    assert_eq!(malformed.status, StatusCode::BAD_REQUEST);
    assert!(fixture.recorder.lines().is_empty());
}

#[tokio::test]
async fn a_service_with_no_host_refuses_a_send_instead_of_pretending() {
    let repo = tempfile::tempdir().expect("repo dir");
    start_run(repo.path());
    let home = tempfile::tempdir().expect("home dir");
    write_settings(home.path(), "claude");
    let state = tempfile::tempdir().expect("state dir");
    let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data");
    let app = App::prepare(repo.path(), PORT, &Actor::Human, state.path(), None)
        .expect("prepare")
        .with_home(home.path().to_path_buf())
        .with_data(data);
    let token = std::fs::read_to_string(app.info().token_path)
        .expect("the token file")
        .trim()
        .to_string();
    let fixture = Fixture {
        app,
        token,
        repo,
        home,
        state,
        recorder: Arc::new(Recorder::default()),
    };
    let reply = fixture
        .send(
            Call::post("/api/chat", json!({"space": "coordinator", "text": "hi"}))
                .with_token(&fixture.token)
                .with_write_header(),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.body);
    assert!(
        reply.body["message"]
            .as_str()
            .unwrap()
            .contains("Nothing was sent")
    );
    assert!(fixture.recorder.lines().is_empty() || fixture.recorder.refuse);
}

#[tokio::test]
async fn a_host_that_refuses_the_write_is_a_refusal_with_the_reason() {
    let fixture = Fixture::new("claude").with_a_refusing_host();
    let reply = fixture
        .send(
            Call::post("/api/chat", json!({"space": "coordinator", "text": "hi"}))
                .with_token(&fixture.token)
                .with_write_header(),
        )
        .await;
    assert!(
        reply.body["message"]
            .as_str()
            .unwrap()
            .contains("no open pane for the coordinator"),
        "{}",
        reply.body
    );
}

#[tokio::test]
async fn a_service_reads_only_the_home_it_was_given() {
    // The service's home is a temp dir with no session at all. The developer's
    // real records are never read, even though this machine has plenty: the
    // answer is the empty one, not somebody else's conversation.
    let repo = tempfile::tempdir().expect("repo dir");
    start_run(repo.path());
    let home = tempfile::tempdir().expect("home dir");
    write_settings(home.path(), "claude");
    let fixture = Fixture::prepared(repo, home);
    let reply = fixture.chat("coordinator", "").await;
    assert!(reply.messages().is_empty());
    assert_eq!(reply.body["session"], Value::Null);
}

#[tokio::test]
async fn the_state_directory_is_not_the_session_directory() {
    // A guard for the fixture itself: the state directory holds `service.json`,
    // and the session lives under the home, so a page read from the wrong root
    // would be a missing session rather than somebody else's conversation.
    let fixture = Fixture::new("claude");
    assert!(fixture.state.path().join("service.json").exists());
    assert!(
        fixture
            .coordinator_record()
            .to_string_lossy()
            .contains("/.claude/"),
        "the session is found through the recorded home, never the state directory"
    );
}

/// The gate's bug, as a route test: a service whose settings home is one
/// directory and whose agents run with another reads the transcript from the
/// agents' home. `SHPRD_HOME` (the settings home) is never where a CLI writes a
/// session, so a chat that looked there answered "has not written a session"
/// while the session sat in the real `~/.claude`.
#[tokio::test]
async fn a_transcript_is_read_from_the_home_the_agent_ran_with() {
    let repo = tempfile::tempdir().expect("repo dir");
    start_run(repo.path());
    // The engine's own home, which holds the settings and no sessions at all.
    let home = tempfile::tempdir().expect("home dir");
    write_settings(home.path(), "claude");
    // The home the agent CLI really ran with, which is where its record is.
    let agent_home = tempfile::tempdir().expect("agent home");
    let state = tempfile::tempdir().expect("state dir");
    let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data");
    let app = App::prepare(repo.path(), PORT, &Actor::Human, state.path(), None)
        .expect("prepare")
        .with_home(home.path().to_path_buf())
        .with_data(data)
        .with_transcript_roots(shprd_agents::transcript::Roots::at(agent_home.path()));
    let token = std::fs::read_to_string(app.info().token_path)
        .expect("the token file")
        .trim()
        .to_string();
    let fixture = Fixture {
        app,
        token,
        repo,
        home,
        state,
        recorder: Arc::new(Recorder::default()),
    };
    // The session is written under the *agent* home, in the folder Claude Code's
    // own fold names (which turns this temp path's underscores into `-`).
    let slug = cwd_slug(TranscriptFormat::ClaudeJsonl, fixture.repo.path());
    let dir = agent_home
        .path()
        .join(".claude")
        .join("projects")
        .join(slug);
    std::fs::create_dir_all(&dir).unwrap();
    let record = dir.join("real.jsonl");
    std::fs::write(
        &record,
        format!(
            "{}\n",
            json!({
                "type": "user",
                "isSidechain": false,
                "timestamp": "2026-11-04T10:00:00.000Z",
                "message": {"role": "user", "content": "What does the gate say?"},
            })
        ),
    )
    .unwrap();
    // Nothing was written under the settings home, so a chat that used it would
    // find nothing.
    assert!(!fixture.home.path().join(".claude").exists());
    let reply = fixture.chat("coordinator", "").await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body["session"], "real");
    assert_eq!(reply.messages().len(), 1, "{}", reply.body);
    assert_eq!(reply.messages()[0]["text"], "What does the gate say?");
}

/// A service that was told no roots says so rather than looking in a directory
/// that can never hold a session.
#[tokio::test]
async fn a_service_with_no_agent_roots_says_it_was_not_told() {
    let repo = tempfile::tempdir().expect("repo dir");
    start_run(repo.path());
    let home = tempfile::tempdir().expect("home dir");
    write_settings(home.path(), "claude");
    let state = tempfile::tempdir().expect("state dir");
    let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data");
    let app = App::prepare(repo.path(), PORT, &Actor::Human, state.path(), None)
        .expect("prepare")
        .with_home(home.path().to_path_buf())
        .with_data(data);
    let token = std::fs::read_to_string(app.info().token_path)
        .expect("the token file")
        .trim()
        .to_string();
    let fixture = Fixture {
        app,
        token,
        repo,
        home,
        state,
        recorder: Arc::new(Recorder::default()),
    };
    let reply = fixture.chat("coordinator", "").await;
    assert_eq!(
        reply.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "{}",
        reply.body
    );
    assert!(
        reply.body["message"]
            .as_str()
            .unwrap_or_default()
            .contains("not told the home"),
        "{}",
        reply.body
    );
}

/// A task's space reads pi's session the same way its coordinator's does: the
/// program's own folder under the home the agent ran with, and the worktree as
/// the directory the session belongs to.
#[tokio::test]
async fn a_task_space_reads_a_pi_session_from_its_worktree() {
    let fixture = Fixture::new("claude");
    let worktree = fixture.home.path().join("worktrees").join("T2");
    std::fs::create_dir_all(&worktree).unwrap();
    fixture.spawn_task("T2", "pi", &worktree);
    let slug = cwd_slug(TranscriptFormat::PiJsonl, &worktree);
    let dir = fixture
        .home
        .path()
        .join(".pi")
        .join("agent")
        .join("sessions")
        .join(slug);
    std::fs::create_dir_all(&dir).unwrap();
    let header = json!({
        "type": "session",
        "version": 3,
        "id": "worker",
        "timestamp": "2026-11-04T10:00:00.000Z",
        "cwd": worktree.display().to_string(),
    });
    let line = json!({
        "type": "message",
        "id": "u1",
        "message": {
            "role": "user",
            "timestamp": 1_794_616_780_000i64,
            "content": [{"type": "text", "text": "T2 is done."}],
        },
    });
    std::fs::write(
        dir.join("2026-11-04T10-00-00-000Z_worker.jsonl"),
        format!("{header}\n{line}\n"),
    )
    .unwrap();
    let reply = fixture.chat("T2", "").await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body["program"], "pi");
    assert_eq!(reply.body["session"], "2026-11-04T10-00-00-000Z_worker");
    assert_eq!(reply.messages().len(), 1, "{}", reply.body);
    assert_eq!(reply.messages()[0]["text"], "T2 is done.");
}

/// A task's space reads an OpenCode database session, picked by the worktree the
/// session row names rather than by being the newest on the machine.
#[tokio::test]
async fn a_task_space_reads_an_opencode_database_session_from_its_worktree() {
    let fixture = Fixture::new("claude");
    let worktree = fixture.home.path().join("worktrees").join("T3");
    std::fs::create_dir_all(&worktree).unwrap();
    fixture.spawn_task("T3", "opencode", &worktree);
    let dir = fixture
        .home
        .path()
        .join(".local")
        .join("share")
        .join("opencode");
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("opencode.db");
    let connection = rusqlite::Connection::open(&db).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT NOT NULL,
                time_updated INTEGER NOT NULL);
             CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
                time_created INTEGER NOT NULL, data TEXT NOT NULL);
             CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT NOT NULL,
                session_id TEXT NOT NULL, time_created INTEGER NOT NULL, data TEXT NOT NULL);",
        )
        .unwrap();
    // A newer session for another directory, which must not win.
    connection
        .execute(
            "INSERT INTO session VALUES ('ses_elsewhere', '/work/other', 9999999999)",
            (),
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO session VALUES ('ses_task', ?1, 5)",
            (worktree.display().to_string(),),
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO message VALUES ('msg_1', 'ses_task', 1, ?1)",
            (r#"{"role":"assistant","time":{"created":1000}}"#,),
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO part VALUES ('prt_1', 'msg_1', 'ses_task', 1, ?1)",
            (r#"{"type":"text","text":"T3 is done."}"#,),
        )
        .unwrap();
    drop(connection);
    let reply = fixture.chat("T3", "").await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body["program"], "opencode");
    assert_eq!(reply.body["session"], "ses_task");
    assert_eq!(reply.messages().len(), 1, "{}", reply.body);
    assert_eq!(reply.messages()[0]["text"], "T3 is done.");
}
