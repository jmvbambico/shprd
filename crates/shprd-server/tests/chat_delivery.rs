//! Chat delivery is off the request: a send returns before a slow host has
//! finished typing, and a delivered send or a grown transcript publishes a
//! change notice, so a client's chat does not wait for its poll.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use shprd_agents::spec::TranscriptFormat;
use shprd_agents::transcript::{Roots, cwd_slug};
use shprd_core::event::{Actor, EventKind};
use shprd_core::log::{EventLog, SystemClock};
use shprd_server::hub::Hub;
use shprd_server::{App, ChatWriter};
use tower::ServiceExt;

const HOST: &str = "127.0.0.1:18080";
const PORT: u16 = 18_083;

/// A host whose delivery takes `hold` (the draft hold), or fails after it.
struct SlowHost {
    hold: Duration,
    fail: bool,
    typed: Mutex<Vec<String>>,
    calls: AtomicUsize,
}

impl ChatWriter for SlowHost {
    fn send(&self, _space: Option<&str>, text: &str) -> Result<(), String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(self.hold);
        if self.fail {
            return Err("the pane is gone".into());
        }
        self.typed.lock().unwrap().push(text.to_owned());
        Ok(())
    }
}

struct Fixture {
    app: App,
    token: String,
    repo: tempfile::TempDir,
    home: tempfile::TempDir,
    _state: tempfile::TempDir,
    hub: Hub,
    host: Arc<SlowHost>,
}

impl Fixture {
    fn new(hold_ms: u64, fail: bool) -> Self {
        let repo = tempfile::tempdir().unwrap();
        let mut log = EventLog::open(&repo.path().join(".shprd/events.jsonl")).unwrap();
        log.append(
            Actor::Human,
            EventKind::RunStarted {
                run_id: "r1".into(),
                repo: repo.path().display().to_string(),
                base_branch: "main".into(),
                integration_branch: "integration/m11".into(),
            },
            &SystemClock,
        )
        .unwrap();
        let home = tempfile::tempdir().unwrap();
        let cfg = home.path().join(".config/shprd");
        std::fs::create_dir_all(&cfg).unwrap();
        std::fs::write(
            cfg.join("settings.toml"),
            "[roles.coordinator]\nagent = \"claude\"\n",
        )
        .unwrap();
        let state = tempfile::tempdir().unwrap();
        let hub = Hub::touching(state.path().join("shprd"));
        let host = Arc::new(SlowHost {
            hold: Duration::from_millis(hold_ms),
            fail,
            typed: Mutex::default(),
            calls: AtomicUsize::new(0),
        });
        let writer: Arc<dyn ChatWriter> = host.clone();
        let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data");
        let app = App::prepare_with_hub(
            repo.path(),
            PORT,
            &Actor::Human,
            state.path(),
            None,
            hub.clone(),
        )
        .unwrap()
        .with_home(home.path().to_path_buf())
        .with_transcript_roots(Roots::at(home.path()))
        .with_data(data)
        .with_chat_writer(writer);
        let token = std::fs::read_to_string(app.info().token_path)
            .unwrap()
            .trim()
            .to_string();
        Self {
            app,
            token,
            repo,
            home,
            _state: state,
            hub,
            host,
        }
    }

    fn record(&self) -> std::path::PathBuf {
        let slug = cwd_slug(TranscriptFormat::ClaudeJsonl, self.repo.path());
        let dir = self.home.path().join(".claude/projects").join(slug);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("s1.jsonl")
    }

    async fn call(&self, post: bool, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut b = Request::builder()
            .method(if post { "POST" } else { "GET" })
            .uri(uri)
            .header(header::HOST, HOST)
            .header(header::AUTHORIZATION, format!("Bearer {}", self.token));
        if post {
            b = b
                .header("x-shprd", "1")
                .header(header::CONTENT_TYPE, "application/json");
        }
        let req = b
            .body(body.map_or_else(Body::empty, |v| Body::from(v.to_string())))
            .unwrap();
        let res = self.app.router().oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }
}

fn line(role: &str, text: &str) -> String {
    format!(
        "{}\n",
        json!({"type": role, "isSidechain": false, "message": {"role": role, "content": text}})
    )
}

#[tokio::test]
async fn a_send_returns_before_a_slow_delivery_finishes_and_is_typed_once() {
    let fixture = Fixture::new(1500, false);
    let began = Instant::now();
    let (status, body) = fixture
        .call(
            true,
            "/api/chat",
            Some(json!({"space": "coordinator", "text": "hello"})),
        )
        .await;
    let returned = began.elapsed();
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["sent"], true);
    assert!(
        returned < Duration::from_millis(600),
        "the send waited {returned:?} for a 1.5 s delivery"
    );
    assert!(
        fixture.host.typed.lock().unwrap().is_empty(),
        "still typing"
    );
    // The delivery finishes in the background, once.
    let until = Instant::now() + Duration::from_secs(5);
    while fixture.host.typed.lock().unwrap().is_empty() && Instant::now() < until {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        *fixture.host.typed.lock().unwrap(),
        vec!["hello".to_string()]
    );
    assert_eq!(fixture.host.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_delivery_that_fails_late_reaches_the_person_on_the_next_read() {
    let fixture = Fixture::new(400, true);
    let (status, _) = fixture
        .call(
            true,
            "/api/chat",
            Some(json!({"space": "coordinator", "text": "hi"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    tokio::time::sleep(Duration::from_millis(700)).await;
    let (_, page) = fixture
        .call(false, "/api/chat?space=coordinator&since=", None)
        .await;
    let note = page["note"].as_str().unwrap_or_default();
    assert!(
        note.contains("not delivered") && note.contains("the pane is gone"),
        "{page}"
    );
    // Said once.
    let (_, again) = fixture
        .call(false, "/api/chat?space=coordinator&since=", None)
        .await;
    assert!(
        !again["note"]
            .as_str()
            .unwrap_or_default()
            .contains("not delivered")
    );
}

#[tokio::test]
async fn a_delivered_send_and_a_grown_transcript_publish_a_change() {
    let fixture = Fixture::new(300, false);
    let record = fixture.record();
    std::fs::write(&record, line("user", "first")).unwrap();
    // A read starts the transcript watcher.
    let (status, _) = fixture
        .call(false, "/api/chat?space=coordinator&since=", None)
        .await;
    assert_eq!(status, StatusCode::OK);

    // A waiter on the hub is woken when the delivery finishes.
    let hub = fixture.hub.clone();
    let waiter = tokio::spawn(async move {
        let t = Instant::now();
        hub.wait().await;
        t.elapsed()
    });
    tokio::time::sleep(Duration::from_millis(30)).await;
    let (status, _) = fixture
        .call(
            true,
            "/api/chat",
            Some(json!({"space": "coordinator", "text": "go"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let woke = tokio::time::timeout(Duration::from_secs(3), waiter)
        .await
        .expect("a delivered send publishes a change")
        .unwrap();
    assert!(
        woke >= Duration::from_millis(250),
        "published at delivery, not before: {woke:?}"
    );

    // A reply chunk grows the transcript: a change follows within ~200 ms.
    let hub = fixture.hub.clone();
    let waiter = tokio::spawn(async move {
        hub.wait().await;
        Instant::now()
    });
    tokio::time::sleep(Duration::from_millis(30)).await;
    let grew = Instant::now();
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&record)
            .unwrap();
        f.write_all(line("assistant", "reply").as_bytes()).unwrap();
    }
    let at = tokio::time::timeout(Duration::from_secs(3), waiter)
        .await
        .expect("a transcript change publishes a change")
        .unwrap();
    let latency = at.duration_since(grew);
    assert!(
        latency < Duration::from_millis(400),
        "change notice took {latency:?}"
    );
    println!("transcript growth -> change notice: {latency:?}");
}
