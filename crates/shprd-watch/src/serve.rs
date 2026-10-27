//! Client for `opencode serve` / `kilo serve`.
//!
//! Both servers expose the same two endpoints, which is where the live retry
//! state of a rate-limited turn comes from:
//!
//! - `GET /session/status` — one snapshot of every session's state;
//! - `GET /event` — Server-Sent Events carrying the same states as they change.
//!
//! Only what the watchdog needs is modelled. The client is read-only: it never
//! writes to the server.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::Value;

/// Anything above this is an epoch-milliseconds timestamp, not a delay.
///
/// `next` is documented as neither; 10^12 ms is 2001, so no plausible relative
/// delay (that would be 31 years) crosses the line.
const EPOCH_MS_FLOOR: u64 = 1_000_000_000_000;

/// How much of a retry message is kept.
const MESSAGE_LIMIT: usize = 200;

/// The latest known state of one session on a serve instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// The session is not working.
    Idle,
    /// A turn is running.
    Busy,
    /// A turn is rate-limited and the server will retry by itself.
    Retry {
        /// How many attempts have failed; 0 when the server omits it.
        attempt: u32,
        /// The server's reason, truncated to [`MESSAGE_LIMIT`] characters.
        message: String,
        /// When the next attempt is due, in epoch milliseconds.
        ///
        /// `None` when the server gave no usable `next` value.
        next_at_ms: Option<u64>,
    },
    /// The server reported a state this build does not know.
    Unknown,
}

impl Status {
    /// Parse one `SessionStatus` JSON object.
    ///
    /// `now_ms` only matters for a relative `next`, which becomes an absolute
    /// deadline. An unrecognized or missing `type` yields [`Status::Unknown`].
    #[must_use]
    pub fn from_json(value: &Value, now_ms: u64) -> Self {
        match value.get("type").and_then(Value::as_str) {
            Some("idle") => Self::Idle,
            Some("busy") => Self::Busy,
            Some("retry") => Self::Retry {
                attempt: value
                    .get("attempt")
                    .and_then(Value::as_u64)
                    .and_then(|n| u32::try_from(n).ok())
                    .unwrap_or(0),
                message: value
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .chars()
                    .take(MESSAGE_LIMIT)
                    .collect(),
                next_at_ms: value
                    .get("next")
                    .and_then(Value::as_u64)
                    .map(|next| resolve_next(next, now_ms)),
            },
            _ => Self::Unknown,
        }
    }
}

/// Turn the server's ambiguous `next` into an absolute deadline.
///
/// A value above [`EPOCH_MS_FLOOR`] is already a timestamp; anything else is a
/// delay counted from `now_ms`.
#[must_use]
pub fn resolve_next(next: u64, now_ms: u64) -> u64 {
    if next > EPOCH_MS_FLOOR {
        next
    } else {
        now_ms.saturating_add(next)
    }
}

/// One event read off the `/event` stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServeEvent {
    /// A session's state changed.
    Status {
        /// Session the state belongs to.
        session: String,
        /// The new state.
        status: Status,
    },
    /// A session failed a turn.
    Error {
        /// Session the error belongs to, when the server names one.
        session: Option<String>,
        /// Error name, for example `UnknownError`.
        name: String,
        /// Human-readable message.
        message: String,
    },
    /// An event this build does not care about, or could not read.
    Other,
}

/// Why a serve request could not answer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ServeError {
    /// The base URL is not a loopback address; the client refuses to leave the
    /// machine.
    #[error("not a loopback base URL")]
    NotLocal,
    /// The request never produced a response (connect, read, timeout).
    #[error("network error")]
    Network,
    /// The server answered with a status other than 200.
    #[error("HTTP {0}")]
    Http(u16),
    /// The body was not the JSON shape this client understands.
    #[error("unrecognized response shape")]
    Shape,
}

/// Parse a `GET /session/status` body into one entry per session.
///
/// # Errors
///
/// [`ServeError::Shape`] when the body is not a JSON object.
pub fn parse_statuses(body: &str, now_ms: u64) -> Result<BTreeMap<String, Status>, ServeError> {
    let value: Value = serde_json::from_str(body).map_err(|_| ServeError::Shape)?;
    let object = value.as_object().ok_or(ServeError::Shape)?;
    Ok(object
        .iter()
        .map(|(session, status)| (session.clone(), Status::from_json(status, now_ms)))
        .collect())
}

/// Whether `base` points at this machine.
///
/// The watchdog only ever talks to a serve instance started here, so anything
/// else is refused before a connection is made.
#[must_use]
pub fn is_local_base(base: &str) -> bool {
    let rest = base
        .strip_prefix("http://")
        .or_else(|| base.strip_prefix("https://"));
    let Some(rest) = rest else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = if let Some(rest) = authority.strip_prefix('[') {
        rest.split(']').next().unwrap_or_default()
    } else {
        authority.split(':').next().unwrap_or_default()
    };
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

/// A read-only client for one `opencode serve` / `kilo serve` instance.
#[derive(Debug, Clone)]
pub struct ServeClient {
    /// Server root, for example `http://127.0.0.1:4096`.
    base: String,
    /// Ceiling for a whole request, and for connecting the event stream.
    timeout: Duration,
}

impl ServeClient {
    /// A client for `base`, which must be loopback (see [`is_local_base`]).
    ///
    /// The base is not validated here; every request checks it, so a client can
    /// be built before the port is known.
    pub fn new(base: impl Into<String>, timeout: Duration) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_owned(),
            timeout,
        }
    }

    /// A client for `port` on the loopback interface.
    #[must_use]
    pub fn local(port: u16, timeout: Duration) -> Self {
        Self::new(format!("http://127.0.0.1:{port}"), timeout)
    }

    /// The base URL this client talks to.
    #[must_use]
    pub fn base(&self) -> &str {
        &self.base
    }

    /// Refuse a base that is not loopback.
    fn ensure_local(&self) -> Result<(), ServeError> {
        if is_local_base(&self.base) {
            Ok(())
        } else {
            Err(ServeError::NotLocal)
        }
    }
}

impl ServeClient {
    /// An agent whose whole request, including the body, must finish within
    /// [`ServeClient::timeout`].
    fn agent(&self) -> ureq::Agent {
        ureq::Agent::config_builder()
            .timeout_global(Some(self.timeout))
            .build()
            .into()
    }

    /// The state of one session, or of every session when `session` is `None`.
    ///
    /// When `session` is `Some`, the returned map holds that session alone, and
    /// carries [`Status::Unknown`] if the server did not list it: a caller
    /// asking about one session should not have to tell "absent" from "unknown".
    ///
    /// `now_ms` resolves the ambiguous `next` of a retry status.
    ///
    /// # Errors
    ///
    /// [`ServeError::NotLocal`] for a non-loopback base, [`ServeError::Network`]
    /// when nothing answers, [`ServeError::Http`] for a non-200 status and
    /// [`ServeError::Shape`] for a body that is not the expected JSON.
    pub fn status(
        &self,
        session: Option<&str>,
        now_ms: u64,
    ) -> Result<BTreeMap<String, Status>, ServeError> {
        self.ensure_local()?;
        let url = format!("{}/session/status", self.base);
        let body = match self.agent().get(&url).call() {
            Ok(mut resp) => resp
                .body_mut()
                .read_to_string()
                .map_err(|_| ServeError::Network)?,
            Err(ureq::Error::StatusCode(code)) => return Err(ServeError::Http(code)),
            Err(_) => return Err(ServeError::Network),
        };
        let mut statuses = parse_statuses(&body, now_ms)?;
        if let Some(session) = session {
            let only = statuses.remove(session).unwrap_or(Status::Unknown);
            statuses.clear();
            statuses.insert(session.to_owned(), only);
        }
        Ok(statuses)
    }
}

/// Epoch milliseconds now, saturating at 0 before 1970.
#[must_use]
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

/// Parse one SSE `data:` payload into an event.
///
/// kilo's specification duplicates the event schema names, so events are
/// matched on `type` alone. Anything unrecognized or malformed is
/// [`ServeEvent::Other`]: a stream never stops over one line it cannot read.
#[must_use]
pub fn parse_event(data: &str, now_ms: u64) -> ServeEvent {
    let Ok(value) = serde_json::from_str::<Value>(data) else {
        return ServeEvent::Other;
    };
    let properties = value.get("properties");
    match value.get("type").and_then(Value::as_str) {
        Some("session.status") => {
            let Some(session) = properties
                .and_then(|p| p.get("sessionID"))
                .and_then(Value::as_str)
            else {
                return ServeEvent::Other;
            };
            let status = properties
                .and_then(|p| p.get("status"))
                .map_or(Status::Unknown, |s| Status::from_json(s, now_ms));
            ServeEvent::Status {
                session: session.to_owned(),
                status,
            }
        }
        Some("session.error") => {
            let error = properties.and_then(|p| p.get("error"));
            ServeEvent::Error {
                session: properties
                    .and_then(|p| p.get("sessionID"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                name: error
                    .and_then(|e| e.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("UnknownError")
                    .to_owned(),
                message: error
                    .and_then(|e| e.get("data"))
                    .and_then(|d| d.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            }
        }
        _ => ServeEvent::Other,
    }
}

impl ServeClient {
    /// Open the `/event` stream.
    ///
    /// The connect is bounded by [`ServeClient::timeout`]; the reads are not,
    /// because a quiet stream is normal. Dropping the returned
    /// [`EventStream`] closes the connection.
    ///
    /// # Errors
    ///
    /// [`ServeError::NotLocal`] for a non-loopback base, [`ServeError::Network`]
    /// when the connect fails and [`ServeError::Http`] for a non-200 status.
    pub fn events(&self) -> Result<EventStream, ServeError> {
        self.ensure_local()?;
        // No global timeout: it would cut a long-lived stream short.
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_connect(Some(self.timeout))
            .timeout_global(None)
            .build()
            .into();
        let url = format!("{}/event", self.base);
        let resp = match agent.get(&url).call() {
            Ok(resp) => resp,
            Err(ureq::Error::StatusCode(code)) => return Err(ServeError::Http(code)),
            Err(_) => return Err(ServeError::Network),
        };
        Ok(EventStream {
            reader: std::io::BufReader::new(resp.into_body().into_reader()),
            data: String::new(),
            done: false,
        })
    }
}

/// The `/event` stream, read one SSE field at a time.
pub struct EventStream {
    /// Raw response body.
    reader: std::io::BufReader<ureq::BodyReader<'static>>,
    /// `data:` lines of the event being assembled, joined by newlines.
    data: String,
    /// Set once the body ended or failed; the iterator then stops.
    done: bool,
}

impl EventStream {
    /// Read one line into `line`; false at end of body or on a read error.
    fn next_line(&mut self, line: &mut String) -> bool {
        line.clear();
        match std::io::BufRead::read_line(&mut self.reader, line) {
            Ok(0) | Err(_) => false,
            Ok(_) => true,
        }
    }

    /// Turn the assembled `data:` payload into an event, if any.
    fn take_event(&mut self) -> Option<ServeEvent> {
        if self.data.is_empty() {
            return None;
        }
        let payload = std::mem::take(&mut self.data);
        Some(parse_event(&payload, now_ms()))
    }
}

impl Iterator for EventStream {
    type Item = Result<ServeEvent, ServeError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let mut line = String::new();
        loop {
            if !self.next_line(&mut line) {
                self.done = true;
                return self.take_event().map(Ok);
            }
            let text = line.trim_end_matches(['\n', '\r']);
            if text.is_empty() {
                // Blank line: the event is complete.
                if let Some(event) = self.take_event() {
                    return Some(Ok(event));
                }
            } else if text.starts_with(':') {
                // Comment, keep-alive: nothing to parse.
            } else if let Some(value) = text.strip_prefix("data:") {
                if !self.data.is_empty() {
                    self.data.push('\n');
                }
                self.data.push_str(value.strip_prefix(' ').unwrap_or(value));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::io::{Read, Write};
    use std::net::{Shutdown, TcpListener, TcpStream};
    use std::thread::JoinHandle;

    /// A 200 response carrying `body` as JSON.
    fn http_ok(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    /// A response with an error status and no body.
    fn http_code(code: u16) -> String {
        format!("HTTP/1.1 {code} Nope\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
    }

    /// A 200 response carrying an SSE body.
    fn sse(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n{body}"
        )
    }

    /// Reply to one connection per canned response, then stop.
    ///
    /// Returns the loopback port and the server thread's handle.
    fn fake_server(responses: Vec<String>) -> (u16, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("local addr").port();
        let handle = std::thread::spawn(move || {
            for response in responses {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                drain(&mut stream);
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
                let _ = stream.shutdown(Shutdown::Write);
            }
        });
        (port, handle)
    }

    /// Consume the request head so the write does not race the read.
    fn drain(stream: &mut TcpStream) {
        let mut buf = [0u8; 1024];
        let _ = stream.read(&mut buf);
    }
    const NOW: u64 = 1_700_000_000_000;

    #[test]
    fn status_parsing_reads_both_next_interpretations() {
        let body = r#"{"a":{"type":"idle"},"b":{"type":"busy"},
            "c":{"type":"retry","attempt":3,"message":"slow down","next":1700000009999},
            "d":{"type":"retry","attempt":1,"message":"wait","next":2500},
            "e":{"type":"paused"}}"#;
        let statuses = parse_statuses(body, NOW).expect("parses");
        assert_eq!(statuses.get("a"), Some(&Status::Idle));
        assert_eq!(statuses.get("b"), Some(&Status::Busy));
        assert_eq!(
            statuses.get("c"),
            Some(&Status::Retry {
                attempt: 3,
                message: "slow down".to_owned(),
                next_at_ms: Some(1_700_000_009_999),
            })
        );
        assert_eq!(
            statuses.get("d"),
            Some(&Status::Retry {
                attempt: 1,
                message: "wait".to_owned(),
                next_at_ms: Some(NOW + 2500),
            })
        );
        assert_eq!(statuses.get("e"), Some(&Status::Unknown));
        assert_eq!(statuses.len(), 5);
    }

    #[test]
    fn status_parsing_truncates_message_and_rejects_non_object() {
        let long = "x".repeat(500);
        let body = format!(r#"{{"r":{{"type":"retry","attempt":2,"message":"{long}"}}}}"#);
        let statuses = parse_statuses(&body, NOW).expect("parses");
        let Some(Status::Retry { message, .. }) = statuses.get("r") else {
            panic!("expected retry, got {:?}", statuses.get("r"));
        };
        assert_eq!(message.chars().count(), MESSAGE_LIMIT);
        assert_eq!(parse_statuses("[]", NOW), Err(ServeError::Shape));
        assert_eq!(parse_statuses("not json", NOW), Err(ServeError::Shape));
    }

    #[test]
    fn local_base_check_accepts_only_loopback() {
        assert!(is_local_base("http://127.0.0.1:4096"));
        assert!(is_local_base("http://localhost:8080/"));
        assert!(is_local_base("http://[::1]:1234"));
        assert!(!is_local_base("https://example.com"));
        assert!(!is_local_base("http://10.0.0.5:4096"));
        assert!(!is_local_base("127.0.0.1:4096"));
    }

    #[test]
    fn status_client_reads_snapshot_and_filters_by_session() {
        let body = r#"{"s1":{"type":"busy"},"s2":{"type":"idle"}}"#;
        let (port, server) = fake_server(vec![http_ok(body), http_ok(body)]);
        let client = ServeClient::local(port, Duration::from_secs(5));
        let all = client.status(None, NOW).expect("snapshot");
        assert_eq!(all.len(), 2);
        assert_eq!(all.get("s1"), Some(&Status::Busy));
        let one = client.status(Some("s2"), NOW).expect("filtered");
        assert_eq!(one.len(), 1);
        assert_eq!(one.get("s2"), Some(&Status::Idle));
        server.join().expect("server thread");
    }

    #[test]
    fn status_client_reports_unknown_for_a_session_the_server_omits() {
        let (port, server) = fake_server(vec![http_ok("{}")]);
        let client = ServeClient::local(port, Duration::from_secs(5));
        let map = client.status(Some("gone"), NOW).expect("snapshot");
        assert_eq!(map.get("gone"), Some(&Status::Unknown));
        server.join().expect("server thread");
    }

    #[test]
    fn status_client_maps_http_errors_and_refuses_remote_bases() {
        let (port, server) = fake_server(vec![http_code(404)]);
        let client = ServeClient::local(port, Duration::from_secs(5));
        assert_eq!(client.status(None, NOW), Err(ServeError::Http(404)));
        server.join().expect("server thread");
        let remote = ServeClient::new("http://192.168.1.20:4096", Duration::from_secs(1));
        assert_eq!(remote.status(None, NOW), Err(ServeError::NotLocal));
        assert!(matches!(remote.events(), Err(ServeError::NotLocal)));
    }

    #[test]
    fn event_stream_yields_status_error_and_skips_noise() {
        let payload = concat!(
            ": keep-alive\n\n",
            "event: session.status\n",
            "data: {\"type\":\"session.status\",\"properties\":{\"sessionID\":\"s1\",",
            "\"status\":{\"type\":\"retry\",\"attempt\":2,\"message\":\"rate limited\",",
            "\"next\":1700000009999}}}\n\n",
            "data: {\"type\":\"session.error\",\"properties\":{\"sessionID\":\"s1\",",
            "\"error\":{\"name\":\"UnknownError\",\"data\":{\"message\":\"boom\",",
            "\"statusCode\":429}}}}\n\n",
            "data: {not json\n\n",
            "data: {\"type\":\"server.connected\",\"properties\":{}}\n\n",
        );
        let (port, server) = fake_server(vec![sse(payload)]);
        let client = ServeClient::local(port, Duration::from_secs(5));
        let events: Vec<ServeEvent> = client
            .events()
            .expect("stream opens")
            .map(|event| event.expect("no transport error"))
            .collect();
        server.join().expect("server thread");
        assert_eq!(
            events,
            vec![
                ServeEvent::Status {
                    session: "s1".to_owned(),
                    status: Status::Retry {
                        attempt: 2,
                        message: "rate limited".to_owned(),
                        next_at_ms: Some(1_700_000_009_999),
                    },
                },
                ServeEvent::Error {
                    session: Some("s1".to_owned()),
                    name: "UnknownError".to_owned(),
                    message: "boom".to_owned(),
                },
                ServeEvent::Other,
                ServeEvent::Other,
            ]
        );
    }

    #[test]
    fn event_parsing_defaults_and_ignores_the_unknown() {
        assert_eq!(parse_event("", NOW), ServeEvent::Other);
        assert_eq!(parse_event("{\"type\":\"x\"}", NOW), ServeEvent::Other);
        let bare =
            r#"{"type":"session.status","properties":{"sessionID":"s","status":{"type":"busy"}}}"#;
        assert_eq!(
            parse_event(bare, NOW),
            ServeEvent::Status {
                session: "s".to_owned(),
                status: Status::Busy,
            }
        );
        let no_status = r#"{"type":"session.status","properties":{"sessionID":"s"}}"#;
        assert_eq!(
            parse_event(no_status, NOW),
            ServeEvent::Status {
                session: "s".to_owned(),
                status: Status::Unknown,
            }
        );
        let no_session = r#"{"type":"session.error","properties":{"error":{"name":"E"}}}"#;
        assert_eq!(
            parse_event(no_session, NOW),
            ServeEvent::Error {
                session: None,
                name: "E".to_owned(),
                message: String::new(),
            }
        );
    }
}
