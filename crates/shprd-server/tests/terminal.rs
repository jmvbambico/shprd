//! The terminal socket's acceptance tests, against a fake pane.
//!
//! A real loopback socket, a real gate, and a [`TerminalHost`] that is a
//! recorder: no terminal and no multiplexer is ever reached.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use shprd_core::event::{Actor, EventKind};
use shprd_core::log::{EventLog, SystemClock};
use shprd_server::{App, TermKey, TerminalHost, WsLimits};
use tempfile::TempDir;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

/// A pane that shows a screen a test sets and records what is typed.
#[derive(Debug, Default)]
struct FakePane {
    screen: Mutex<String>,
    typed: Mutex<Vec<String>>,
    gone: Mutex<bool>,
}

impl TerminalHost for FakePane {
    fn resolve(&self, space: &str, pane: Option<&str>) -> Result<String, String> {
        if space == "nobody" {
            return Err("no open tab for task \"nobody\"".to_string());
        }
        match pane {
            Some("%1") | None => Ok("%1".to_string()),
            Some(other) => Err(format!("{other:?} is not the pane {space} runs in")),
        }
    }

    fn read(&self, _pane: &str, _lines: usize) -> Result<String, String> {
        if *self.gone.lock().unwrap() {
            return Err("pane not found".to_string());
        }
        Ok(self.screen.lock().unwrap().clone())
    }

    fn write(&self, _pane: &str, text: &str) -> Result<(), String> {
        self.typed.lock().unwrap().push(format!("text:{text}"));
        Ok(())
    }

    fn keys(&self, _pane: &str, keys: &[TermKey]) -> Result<(), String> {
        for key in keys {
            self.typed.lock().unwrap().push(format!("key:{key:?}"));
        }
        Ok(())
    }
}

/// A service on a real loopback port, with the fake pane wired in.
struct Live {
    port: u16,
    token: String,
    pane: Arc<FakePane>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    _state: TempDir,
    _repo: TempDir,
}

impl Live {
    fn start(wired: bool) -> Self {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("a free port");
        listener.set_nonblocking(true).expect("non-blocking");
        let port = listener.local_addr().expect("address").port();
        let state = tempfile::tempdir().expect("state dir");
        let repo = tempfile::tempdir().expect("repo dir");
        let path = repo.path().join(".shprd").join("events.jsonl");
        let mut log = EventLog::open(&path).expect("the log opens");
        log.append(
            Actor::Human,
            EventKind::RunStarted {
                run_id: "r1".to_string(),
                repo: repo.path().display().to_string(),
                base_branch: "main".to_string(),
                integration_branch: "integration/m8".to_string(),
            },
            &SystemClock,
        )
        .expect("the run starts");
        let pane = Arc::new(FakePane::default());
        let mut app = App::prepare(repo.path(), port, &Actor::Human, state.path(), None)
            .expect("prepare")
            .with_ws_limits(WsLimits {
                ping: Duration::from_secs(5),
                idle: Duration::from_secs(20),
                session: Duration::from_secs(60),
            });
        if wired {
            app = app.with_terminal_host(pane.clone());
        }
        let token = std::fs::read_to_string(app.info().token_path)
            .expect("the token file")
            .trim()
            .to_string();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let router = app.router();
        tokio::spawn(async move {
            let listener = tokio::net::TcpListener::from_std(listener).expect("listener");
            axum::serve(listener, router)
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await
                .expect("serves");
        });
        Self {
            port,
            token,
            pane,
            stop: Some(stop),
            _state: state,
            _repo: repo,
        }
    }

    /// A handshake request, with the token as a header (never in the query) and
    /// this service's own origin.
    fn request(
        &self,
        query: &str,
        origin: Option<&str>,
        bearer: bool,
    ) -> tokio_tungstenite::tungstenite::handshake::client::Request {
        let mut request = format!("ws://127.0.0.1:{}/api/terminal{query}", self.port)
            .into_client_request()
            .expect("a request");
        if bearer {
            request.headers_mut().insert(
                "authorization",
                HeaderValue::from_str(&format!("Bearer {}", self.token)).expect("header"),
            );
        }
        if let Some(origin) = origin {
            request
                .headers_mut()
                .insert("origin", HeaderValue::from_str(origin).expect("header"));
        }
        request
    }

    fn own_origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    async fn connect(&self, query: &str) -> Socket {
        let origin = self.own_origin();
        let (socket, _) =
            tokio_tungstenite::connect_async(self.request(query, Some(&origin), true))
                .await
                .expect("the terminal connects");
        socket
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// The next text message as JSON, within five seconds.
async fn next_json(socket: &mut Socket) -> Value {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .expect("a message in time")
            .expect("the socket is open")
            .expect("a frame");
        match message {
            Message::Text(text) => return serde_json::from_str(&text).expect("json"),
            Message::Ping(_) | Message::Pong(_) => continue,
            other => panic!("expected text, got {other:?}"),
        }
    }
}

/// The next message of `kind`, skipping others.
async fn next_of(socket: &mut Socket, kind: &str) -> Value {
    loop {
        let value = next_json(socket).await;
        if value["type"] == kind {
            return value;
        }
    }
}

async fn send(socket: &mut Socket, value: Value) {
    socket
        .send(Message::Text(value.to_string().into()))
        .await
        .expect("sends");
}

/// The close code the server ends the socket with.
async fn close_code(socket: &mut Socket) -> u16 {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .expect("a close in time");
        match message {
            Some(Ok(Message::Close(Some(frame)))) => return u16::from(frame.code),
            Some(Ok(Message::Text(_) | Message::Ping(_) | Message::Pong(_))) => continue,
            other => panic!("expected a close, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn a_socket_says_hello_then_pushes_the_screen_when_it_changes() {
    let live = Live::start(true);
    *live.pane.screen.lock().unwrap() = "$ cargo test".to_string();
    let mut socket = live.connect("?space=coordinator").await;
    let hello = next_json(&mut socket).await;
    assert_eq!(
        hello,
        json!({"type": "hello", "space": "coordinator", "pane": "%1"})
    );
    let first = next_of(&mut socket, "screen").await;
    assert_eq!(first["text"], "$ cargo test");
    assert_eq!(first["seq"], 1);
    *live.pane.screen.lock().unwrap() = "$ cargo test\nok".to_string();
    let second = next_of(&mut socket, "screen").await;
    assert_eq!(second["text"], "$ cargo test\nok");
    assert_eq!(second["seq"], 2);
}

#[tokio::test]
async fn a_socket_only_watches_until_input_is_opened() {
    let live = Live::start(true);
    let mut socket = live.connect("?space=T1").await;
    next_json(&mut socket).await;
    send(&mut socket, json!({"type": "input", "data": "rm -rf"})).await;
    let refused = next_of(&mut socket, "notice").await;
    assert!(refused["message"].as_str().unwrap().contains("not open"));
    assert!(
        live.pane.typed.lock().unwrap().is_empty(),
        "nothing was typed"
    );

    send(&mut socket, json!({"type": "input_open"})).await;
    next_of(&mut socket, "input_ready").await;
    send(&mut socket, json!({"type": "input", "data": "ls"})).await;
    send(&mut socket, json!({"type": "key", "key": "enter"})).await;
    send(&mut socket, json!({"type": "key", "key": "ctrl+d"})).await;
    let bad = next_of(&mut socket, "notice").await;
    assert!(
        bad["message"]
            .as_str()
            .unwrap()
            .contains("not one a terminal")
    );
    assert_eq!(
        *live.pane.typed.lock().unwrap(),
        vec!["text:ls".to_string(), "key:Enter".to_string()]
    );
}

#[tokio::test]
async fn a_message_over_the_input_cap_types_nothing() {
    let live = Live::start(true);
    let mut socket = live.connect("?space=T1").await;
    next_json(&mut socket).await;
    send(&mut socket, json!({"type": "input_open"})).await;
    next_of(&mut socket, "input_ready").await;
    let big = "x".repeat(shprd_server::terminal::MAX_INPUT + 1);
    send(&mut socket, json!({"type": "input", "data": big})).await;
    let notice = next_of(&mut socket, "notice").await;
    assert!(notice["message"].as_str().unwrap().contains("too much"));
    assert!(live.pane.typed.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_flood_of_input_closes_the_socket_with_1013() {
    let live = Live::start(true);
    let mut socket = live.connect("?space=T1").await;
    next_json(&mut socket).await;
    for _ in 0..(shprd_server::terminal::INPUT_BURST + 5) {
        if socket
            .send(Message::Text(json!({"type": "ping"}).to_string().into()))
            .await
            .is_err()
        {
            break;
        }
    }
    assert_eq!(close_code(&mut socket).await, 1013);
}

#[tokio::test]
async fn a_pane_that_is_not_the_spaces_is_refused_before_the_upgrade() {
    let live = Live::start(true);
    let origin = live.own_origin();
    let refused =
        tokio_tungstenite::connect_async(live.request("?space=T1&pane=%2", Some(&origin), true))
            .await;
    assert!(refused.is_err(), "a pane the client picked is not attached");
    let none =
        tokio_tungstenite::connect_async(live.request("?space=nobody", Some(&origin), true)).await;
    assert!(none.is_err(), "a space with no pane is refused");
    // The named pane, when it is the right one, is fine.
    let mut socket = live.connect("?space=T1&pane=%251").await;
    let hello = next_json(&mut socket).await;
    assert_eq!(hello["pane"], "%1");
}

#[tokio::test]
async fn the_gate_refuses_a_foreign_origin_and_a_missing_credential() {
    let live = Live::start(true);
    let foreign = tokio_tungstenite::connect_async(live.request(
        "?space=T1",
        Some("https://evil.example"),
        true,
    ))
    .await;
    assert!(foreign.is_err(), "a foreign Origin is refused");
    let origin = live.own_origin();
    let anonymous =
        tokio_tungstenite::connect_async(live.request("?space=T1", Some(&origin), false)).await;
    assert!(anonymous.is_err(), "no credential is refused");
    // The token in the query string is accepted on /api/ws only.
    let mut request = format!(
        "ws://127.0.0.1:{}/api/terminal?space=T1&token={}",
        live.port, live.token
    )
    .into_client_request()
    .expect("request");
    request
        .headers_mut()
        .insert("origin", HeaderValue::from_str(&origin).expect("header"));
    let queried = tokio_tungstenite::connect_async(request).await;
    assert!(
        queried.is_err(),
        "a token in the query is not a credential here"
    );
}

#[tokio::test]
async fn a_service_with_no_host_refuses() {
    let live = Live::start(false);
    let origin = live.own_origin();
    let refused =
        tokio_tungstenite::connect_async(live.request("?space=T1", Some(&origin), true)).await;
    assert!(refused.is_err());
}

#[tokio::test]
async fn a_pane_that_vanishes_ends_the_socket() {
    let live = Live::start(true);
    let mut socket = live.connect("?space=T1").await;
    next_json(&mut socket).await;
    *live.pane.gone.lock().unwrap() = true;
    let ended = next_of(&mut socket, "ended").await;
    assert!(ended["reason"].as_str().unwrap().contains("not found"));
    assert_eq!(close_code(&mut socket).await, 1001);
}

#[tokio::test]
async fn only_eight_terminals_open_at_once() {
    let live = Live::start(true);
    let mut held = Vec::new();
    for _ in 0..shprd_server::terminal::MAX_SOCKETS {
        let mut socket = live.connect("?space=T1").await;
        next_json(&mut socket).await;
        held.push(socket);
    }
    let origin = live.own_origin();
    let ninth =
        tokio_tungstenite::connect_async(live.request("?space=T1", Some(&origin), true)).await;
    assert!(ninth.is_err(), "the ninth is refused");
    drop(held);
}
