//! The terminal socket: a pane's screen pushed to a page, and keys typed back.
//!
//! `GET /api/terminal?space=&pane=` upgrades to a WebSocket attached to **one**
//! pane, chosen by the query and checked before the upgrade. A socket is born
//! attached and stays attached to that pane, which is the whole of "one attach
//! per socket": there is no message that re-aims it.
//!
//! What it does, and what it deliberately does not:
//!
//! - **Watch by default.** The service reads the pane's screen through the host
//!   ([`TerminalHost::read`]) and pushes it whole whenever it changes. It never
//!   resizes the pane — the agent's own window is the reference, and a page of a
//!   different size must not reflow it.
//! - **Type on demand.** Input is closed until the page sends `input_open` (once,
//!   on the first key the person presses). Before that, `input` and `key` are
//!   refused with a notice.
//! - **No pane but the one the service resolves.** `space` is `coordinator` or a
//!   task id, and the host resolves it exactly as a chat line is routed
//!   ([`TerminalHost::resolve`]); a `pane` in the query must equal the resolved
//!   one, so a client cannot name a pane of its own choosing.
//! - **Bounded everywhere.** At most [`MAX_SOCKETS`] terminals are open at once
//!   (`1013`), a socket sends at most [`INPUT_BURST`] messages in
//!   [`INPUT_WINDOW`] (`1013`), a message is at most [`MAX_INPUT`] bytes, a
//!   screen is at most [`MAX_SCREEN`] bytes (the start is cut, and the page is
//!   told), a slow reader's send is abandoned after [`SEND_TIMEOUT`] (`1013`),
//!   and the socket obeys the same ping, idle and session limits as `/api/ws`
//!   ([`WsLimits`]).
//! - **The gate.** The route is behind the same gate as every other, so a socket
//!   upgrade needs a credential **and** this service's own `Origin`. A bearer
//!   token in the query string is accepted on `/api/ws` only, never here: no
//!   session token appears in a URL.
//!
//! Nothing typed is ever logged, and nothing is replayed: input that did not
//! reach the pane is the person's to retype, which the page says.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::api::AppState;
use crate::dto::ErrorCode;
use crate::dto::{TerminalClientMsg, TerminalServerMsg};
use crate::error::ApiReject;

/// How many terminal sockets may be open at once.
pub const MAX_SOCKETS: usize = 8;

/// How many client messages one socket may send in [`INPUT_WINDOW`].
pub const INPUT_BURST: usize = 200;

/// The window [`INPUT_BURST`] is counted in.
pub const INPUT_WINDOW: Duration = Duration::from_secs(10);

/// The most text one `input` message may carry, in bytes.
pub const MAX_INPUT: usize = 16 * 1024;

/// The most one screen may weigh, in bytes: a screen past it loses its start.
pub const MAX_SCREEN: usize = 1024 * 1024;

/// How long a send to a slow page may take before the socket is dropped.
pub const SEND_TIMEOUT: Duration = Duration::from_secs(10);

/// How many lines of the pane one read asks for.
pub const READ_LINES: usize = 200;

/// How often the pane is looked at.
const POLL: Duration = Duration::from_millis(400);

/// A key the page may press, parsed from the word it sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TermKey {
    /// Return.
    Enter,
    /// Escape.
    Esc,
    /// Tab.
    Tab,
    /// Up arrow.
    Up,
    /// Down arrow.
    Down,
    /// Left arrow.
    Left,
    /// Right arrow.
    Right,
    /// Page up.
    PageUp,
    /// Page down.
    PageDown,
    /// Backspace.
    Backspace,
    /// Delete.
    Delete,
    /// Home.
    Home,
    /// End.
    End,
    /// Ctrl-C.
    CtrlC,
}

impl TermKey {
    /// The key a word names, or `None` for a word that is not on the list.
    pub fn parse(word: &str) -> Option<Self> {
        Some(match word {
            "enter" => Self::Enter,
            "esc" => Self::Esc,
            "tab" => Self::Tab,
            "up" => Self::Up,
            "down" => Self::Down,
            "left" => Self::Left,
            "right" => Self::Right,
            "pageup" => Self::PageUp,
            "pagedown" => Self::PageDown,
            "backspace" => Self::Backspace,
            "delete" => Self::Delete,
            "home" => Self::Home,
            "end" => Self::End,
            "ctrl+c" => Self::CtrlC,
            _ => return None,
        })
    }
}

/// What the terminal socket needs from the multiplexer.
///
/// A trait rather than the host itself for the reason [`crate::ChatWriter`] is:
/// a test drives the socket with a fake pane and no terminal, and the binary
/// wires the host it already drives. Every method is blocking (a host call is a
/// process or a socket round trip), so the socket runs them off the runtime.
pub trait TerminalHost: Send + Sync {
    /// The pane `space` (`coordinator` or a task id) runs its agent in.
    ///
    /// `pane`, when the client named one, must be that pane: `Err` otherwise.
    /// `Err` is also a space with no pane open. The message is shown to the
    /// person as the reason, so it says what to do.
    fn resolve(&self, space: &str, pane: Option<&str>) -> Result<String, String>;

    /// The last `lines` lines of `pane`'s screen.
    fn read(&self, pane: &str, lines: usize) -> Result<String, String>;

    /// Type `text` into `pane` without pressing enter.
    fn write(&self, pane: &str, text: &str) -> Result<(), String>;

    /// Press `keys` in `pane`, in order.
    fn keys(&self, pane: &str, keys: &[TermKey]) -> Result<(), String>;
}

/// The host a service holds, with a `Debug` that never prints one, and the count
/// of terminals open right now.
#[derive(Clone, Default)]
pub struct TerminalPort {
    host: Option<Arc<dyn TerminalHost>>,
    open: Arc<AtomicUsize>,
}

impl TerminalPort {
    /// A port with no host: a terminal is refused rather than pretended.
    pub fn new() -> Self {
        Self::default()
    }

    /// The port that watches and types through `host`.
    pub fn with(host: Arc<dyn TerminalHost>) -> Self {
        Self {
            host: Some(host),
            open: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// The host, when one is wired.
    pub fn get(&self) -> Option<&Arc<dyn TerminalHost>> {
        self.host.as_ref()
    }
}

impl std::fmt::Debug for TerminalPort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalPort")
            .field("wired", &self.host.is_some())
            .field("open", &self.open.load(Ordering::Relaxed))
            .finish()
    }
}

/// Holds one of the [`MAX_SOCKETS`] places, and gives it back on drop.
struct Place(Arc<AtomicUsize>);

impl Place {
    /// Takes a place, or `None` when all of them are taken.
    fn take(open: &Arc<AtomicUsize>) -> Option<Self> {
        let mut now = open.load(Ordering::Acquire);
        loop {
            if now >= MAX_SOCKETS {
                return None;
            }
            match open.compare_exchange(now, now + 1, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => return Some(Self(open.clone())),
                Err(actual) => now = actual,
            }
        }
    }
}

impl Drop for Place {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// The query: which space, and optionally which of its panes.
#[derive(Debug, Deserialize)]
pub(crate) struct TerminalQuery {
    /// `coordinator` or a task id.
    space: Option<String>,
    /// The pane the client expects; checked, never trusted.
    pane: Option<String>,
}

/// `GET /api/terminal`: attach a socket to one pane.
///
/// Everything that can be refused is refused here, before the upgrade, as a typed
/// body: no host, a full house, a space with no pane.
pub(crate) async fn socket(
    State(state): State<AppState>,
    Query(query): Query<TerminalQuery>,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    let upgrade = match upgrade {
        Ok(upgrade) => upgrade,
        Err(rejection) => {
            return ApiReject::new(
                rejection.status(),
                ErrorCode::BadRequest,
                format!(
                    "{}: this route is a WebSocket and stays open",
                    rejection.body_text()
                ),
            )
            .into_response();
        }
    };
    let Some(host) = state.terminal.get().cloned() else {
        return ApiReject::refused(
            "this service cannot reach a terminal: restart `shprd serve` from a build with the \
             host wired",
        )
        .into_response();
    };
    let Some(place) = Place::take(&state.terminal.open) else {
        return ApiReject::new(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::Refused,
            format!("{MAX_SOCKETS} terminals are open already: close one and try again"),
        )
        .into_response();
    };
    let space = query
        .space
        .as_deref()
        .map(str::trim)
        .filter(|space| !space.is_empty())
        .unwrap_or(crate::chat::COORDINATOR)
        .to_owned();
    let wanted = query.pane;
    let resolved = {
        let host = host.clone();
        let space = space.clone();
        tokio::task::spawn_blocking(move || host.resolve(&space, wanted.as_deref())).await
    };
    let pane = match resolved {
        Ok(Ok(pane)) => pane,
        Ok(Err(reason)) => return ApiReject::refused(reason).into_response(),
        Err(err) => {
            return ApiReject::internal(format!("the pane lookup did not finish: {err}"))
                .into_response();
        }
    };
    upgrade
        .max_message_size(MAX_INPUT + 1024)
        .on_upgrade(move |socket| pump(socket, state, host, space, pane, place))
}

/// What a message did to the socket's life.
enum Next {
    Go,
    Close(u16, &'static str),
}

/// Watches one pane until the page goes away or a bound is reached.
async fn pump(
    mut socket: WebSocket,
    state: AppState,
    host: Arc<dyn TerminalHost>,
    space: String,
    pane: String,
    _place: Place,
) {
    let limits = state.ws;
    let hello = TerminalServerMsg::Hello {
        space,
        pane: pane.clone(),
    };
    if send(&mut socket, &hello).await.is_err() {
        return;
    }
    let opened = Instant::now();
    let mut heard = opened;
    let mut ping_at = opened + limits.ping;
    let mut poll_at = opened;
    let mut input_open = false;
    let mut seq: u64 = 0;
    let mut last: Option<String> = None;
    let mut times: VecDeque<Instant> = VecDeque::new();
    loop {
        let wait = poll_at.saturating_duration_since(Instant::now());
        tokio::select! {
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    heard = Instant::now();
                    if rate_exceeded(&mut times, heard) {
                        close(&mut socket, 1013, "too many messages; slow down and reconnect").await;
                        return;
                    }
                    match client_message(&mut socket, &host, &pane, &mut input_open, &text).await {
                        Next::Go => {}
                        Next::Close(code, reason) => {
                            close(&mut socket, code, reason).await;
                            return;
                        }
                    }
                }
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => heard = Instant::now(),
                // A binary frame is not part of the protocol.
                Some(Ok(Message::Binary(_))) => {
                    close(&mut socket, 1003, "text messages only").await;
                    return;
                }
                Some(Ok(Message::Close(_)) | Err(_)) | None => return,
            },
            _ = tokio::time::sleep(wait) => {}
        }

        let now = Instant::now();
        if now.duration_since(opened) >= limits.session {
            close(
                &mut socket,
                1001,
                "the session limit was reached; reconnect",
            )
            .await;
            return;
        }
        if now.duration_since(heard) >= limits.idle {
            close(
                &mut socket,
                1001,
                "idle; reconnect when there is something to see",
            )
            .await;
            return;
        }
        if now >= ping_at {
            if socket.send(Message::Ping(Vec::new().into())).await.is_err() {
                return;
            }
            ping_at = now + limits.ping;
        }
        if now < poll_at {
            continue;
        }
        poll_at = now + POLL;
        let read = {
            let host = host.clone();
            let pane = pane.clone();
            tokio::task::spawn_blocking(move || host.read(&pane, READ_LINES)).await
        };
        let text = match read {
            Ok(Ok(text)) => text,
            Ok(Err(reason)) => {
                let _ = send(&mut socket, &TerminalServerMsg::Ended { reason }).await;
                close(&mut socket, 1001, "the pane is gone").await;
                return;
            }
            Err(_) => return,
        };
        if last.as_deref() == Some(text.as_str()) {
            continue;
        }
        let (shown, cut) = cap_screen(&text);
        seq += 1;
        let screen = TerminalServerMsg::Screen {
            text: shown.to_owned(),
            seq,
            cut,
        };
        match tokio::time::timeout(SEND_TIMEOUT, send(&mut socket, &screen)).await {
            Ok(Ok(())) => {}
            Ok(Err(())) => return,
            Err(_) => {
                close(&mut socket, 1013, "the page is not keeping up; reconnect").await;
                return;
            }
        }
        last = Some(text);
    }
}

/// Counts one more client message; `true` when the window is over its budget.
fn rate_exceeded(times: &mut VecDeque<Instant>, now: Instant) -> bool {
    while let Some(first) = times.front() {
        if now.duration_since(*first) > INPUT_WINDOW {
            times.pop_front();
        } else {
            break;
        }
    }
    times.push_back(now);
    times.len() > INPUT_BURST
}

/// `text` cut to [`MAX_SCREEN`] bytes from its start, on a character boundary.
fn cap_screen(text: &str) -> (&str, bool) {
    if text.len() <= MAX_SCREEN {
        return (text, false);
    }
    let mut from = text.len() - MAX_SCREEN;
    while !text.is_char_boundary(from) {
        from += 1;
    }
    (&text[from..], true)
}

/// Handles one text message from the page.
async fn client_message(
    socket: &mut WebSocket,
    host: &Arc<dyn TerminalHost>,
    pane: &str,
    input_open: &mut bool,
    text: &str,
) -> Next {
    let Ok(message) = serde_json::from_str::<TerminalClientMsg>(text) else {
        return notice(socket, "that message is not part of the terminal protocol").await;
    };
    match message {
        TerminalClientMsg::Ping => {
            let _ = send(socket, &TerminalServerMsg::Pong).await;
            Next::Go
        }
        TerminalClientMsg::InputOpen => {
            *input_open = true;
            let _ = send(socket, &TerminalServerMsg::InputReady).await;
            Next::Go
        }
        TerminalClientMsg::Input { data } => {
            if !*input_open {
                return notice(socket, "input is not open: nothing was typed").await;
            }
            if data.is_empty() {
                return Next::Go;
            }
            if data.len() > MAX_INPUT {
                return notice(
                    socket,
                    "that is too much to type at once: nothing was typed",
                )
                .await;
            }
            let host = host.clone();
            let pane = pane.to_owned();
            let done = tokio::task::spawn_blocking(move || host.write(&pane, &data)).await;
            reported(socket, done).await
        }
        TerminalClientMsg::Key { key } => {
            if !*input_open {
                return notice(socket, "input is not open: nothing was typed").await;
            }
            let Some(key) = TermKey::parse(&key) else {
                return notice(socket, "that key is not one a terminal may press").await;
            };
            let host = host.clone();
            let pane = pane.to_owned();
            let done = tokio::task::spawn_blocking(move || host.keys(&pane, &[key])).await;
            reported(socket, done).await
        }
    }
}

/// Tells the page why a write failed, without echoing what was typed.
async fn reported(
    socket: &mut WebSocket,
    done: Result<Result<(), String>, tokio::task::JoinError>,
) -> Next {
    match done {
        Ok(Ok(())) => Next::Go,
        Ok(Err(reason)) => notice(socket, &format!("{reason}. Nothing was typed")).await,
        Err(_) => notice(socket, "the terminal did not answer. Nothing was typed").await,
    }
}

/// Sends a [`TerminalServerMsg::Notice`].
async fn notice(socket: &mut WebSocket, message: &str) -> Next {
    let notice = TerminalServerMsg::Notice {
        message: message.to_owned(),
    };
    match send(socket, &notice).await {
        Ok(()) => Next::Go,
        Err(()) => Next::Close(1001, "gone"),
    }
}

/// Closes the socket with `code` and `reason`.
async fn close(socket: &mut WebSocket, code: u16, reason: &'static str) {
    let frame = CloseFrame {
        code,
        reason: reason.into(),
    };
    let _ = socket.send(Message::Close(Some(frame))).await;
}

/// Sends one message as JSON text.
async fn send(socket: &mut WebSocket, message: &TerminalServerMsg) -> Result<(), ()> {
    let text = serde_json::to_string(message).map_err(|_| ())?;
    socket
        .send(Message::Text(text.into()))
        .await
        .map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_screen_past_the_cap_loses_its_start_on_a_character_boundary() {
        let text = "é".repeat(MAX_SCREEN); // two bytes each
        let (shown, cut) = cap_screen(&text);
        assert!(cut);
        assert!(shown.len() <= MAX_SCREEN);
        assert!(shown.chars().all(|c| c == 'é'));
        let (whole, cut) = cap_screen("short");
        assert_eq!(whole, "short");
        assert!(!cut);
    }

    #[test]
    fn the_rate_window_forgets_old_messages() {
        let start = Instant::now();
        let mut times = VecDeque::new();
        for _ in 0..INPUT_BURST {
            assert!(!rate_exceeded(&mut times, start));
        }
        assert!(rate_exceeded(&mut times, start), "the 201st in a window");
        let later = start + INPUT_WINDOW + Duration::from_secs(1);
        assert!(!rate_exceeded(&mut times, later), "the window moved on");
    }

    #[test]
    fn only_named_keys_parse() {
        assert_eq!(TermKey::parse("ctrl+c"), Some(TermKey::CtrlC));
        assert_eq!(TermKey::parse("enter"), Some(TermKey::Enter));
        assert_eq!(TermKey::parse("ctrl+d"), None);
        assert_eq!(TermKey::parse("a"), None);
    }

    #[test]
    fn places_are_capped_and_given_back() {
        let open = Arc::new(AtomicUsize::new(0));
        let held: Vec<_> = (0..MAX_SOCKETS)
            .filter_map(|_| Place::take(&open))
            .collect();
        assert_eq!(held.len(), MAX_SOCKETS);
        assert!(Place::take(&open).is_none());
        drop(held);
        assert!(Place::take(&open).is_some());
    }
}
