//! Typed failures: what stops the service from starting, and what refuses a
//! request.

use std::path::PathBuf;

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use shprd_core::log::LogError;

use crate::dto::{ErrorBody, ErrorCode};

/// Something stopped the service from starting or from serving.
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    /// A discovery file or the state directory could not be written.
    #[error("i/o error on {path}: {source}")]
    Io {
        /// The path involved.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// A secret could not be generated.
    #[error("could not generate a secret: {reason}")]
    Secret {
        /// Why the source of randomness failed.
        reason: String,
    },
    /// Another service already holds this state directory.
    ///
    /// Two services cannot share one `service.json`: the second would take the
    /// record over and stopping either one would leave the other running with no
    /// way to find it.
    #[error(
        "a shprd service is already running for {} (port {}, pid {}); stop it, or use another state directory",
        dir.display(),
        port.map(|port| port.to_string()).unwrap_or_else(|| "?".to_string()),
        pid.map(|pid| pid.to_string()).unwrap_or_else(|| "?".to_string()),
    )]
    AlreadyRunning {
        /// The state directory the running service holds.
        dir: PathBuf,
        /// The port it serves, when its record could be read.
        port: Option<u16>,
        /// Its process, when its record could be read.
        pid: Option<u32>,
    },
    /// A service was here and its process is gone, but its lock is still held.
    ///
    /// It is the moment just after a kill: the kernel has not reaped the process
    /// yet, so the lock it holds is still there while nothing answers on its port
    /// (M10 gate bug 7). A start waits this out before it gives up, and says so
    /// rather than claiming a service is running.
    #[error(
        "a shprd service is still starting or shutting down for {} ({}); try again in a moment",
        dir.display(),
        match pid {
            Some(pid) => format!("pid {pid}"),
            None => "no record of it yet".to_string(),
        },
    )]
    ShuttingDown {
        /// The state directory the dying service holds.
        dir: PathBuf,
        /// The port it served, when its record could be read.
        port: Option<u16>,
        /// Its process, when its record could be read.
        pid: Option<u32>,
    },
    /// The socket could not be bound.
    #[error("could not listen on {address}: {source}")]
    Bind {
        /// The address it tried.
        address: std::net::SocketAddr,
        /// Why the bind failed.
        source: std::io::Error,
    },
    /// A discovery file could not be turned into JSON.
    #[error("could not serialize {what}: {source}")]
    Json {
        /// What was being written.
        what: &'static str,
        /// The underlying error.
        source: serde_json::Error,
    },
    /// The listener reported an address it cannot serve.
    #[error("the listener has no local address: {0}")]
    NoAddress(#[source] std::io::Error),
    /// Serving failed after the socket was bound.
    #[error("the server stopped: {0}")]
    Serve(#[source] std::io::Error),
    /// A TypeScript binding could not be generated.
    #[error("could not generate TypeScript bindings: {0}")]
    Bindings(#[from] ts_rs::ExportError),
    /// A tick pass could not read or fold the run log.
    ///
    /// Only the read fails the pass: a step that goes wrong is reported in the
    /// pass's own report and the other steps still run.
    #[error("a tick pass could not read the run: {0}")]
    Tick(String),
}

impl ServerError {
    /// Wraps an I/O failure with the path it happened on.
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

/// A refused request, with its status and typed body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiReject {
    /// The HTTP status the client sees.
    pub status: StatusCode,
    /// The body the client sees.
    pub body: ErrorBody,
    /// How long the client must wait, for a `429`: the `Retry-After` header.
    pub retry_after: Option<u64>,
}

impl ApiReject {
    /// Builds a refusal from a status, a code and one line.
    pub fn new(status: StatusCode, code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            status,
            body: ErrorBody {
                code,
                message: message.into(),
            },
            retry_after: None,
        }
    }

    /// The token was missing or wrong.
    pub fn unauthorized() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            ErrorCode::Unauthorized,
            "send `Authorization: Bearer <token>` with the token in the state directory",
        )
    }

    /// The request was addressed to a name this service does not answer to.
    ///
    /// `421 Misdirected Request` is the researched design's answer to an unknown
    /// `Host`: it is what DNS rebinding looks like from inside the process, and a
    /// redirect would tell the attacker which name is right.
    pub fn wrong_host(host: Option<&str>) -> Self {
        Self::new(
            StatusCode::MISDIRECTED_REQUEST,
            ErrorCode::WrongHost,
            match host {
                Some(host) => format!("{host:?} is not a name this service answers to"),
                None => {
                    "the request carried no Host, so it is not a page on this service".to_string()
                }
            },
        )
    }

    /// A rate limit refused the request; `after` is `Retry-After`'s seconds.
    pub fn too_many_requests(after: u64) -> Self {
        let mut reject = Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            ErrorCode::TooManyRequests,
            format!("too many attempts: try again in {after}s"),
        );
        reject.retry_after = Some(after);
        reject
    }

    /// A destructive action needs the password again, through this session.
    pub fn reauth() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            ErrorCode::Reauth,
            "enter your password again to do this",
        )
    }

    /// The request came from another origin.
    pub fn cross_origin(origin: &str) -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            ErrorCode::CrossOrigin,
            format!("{origin:?} is not this service's origin"),
        )
    }

    /// A write did not carry the custom header.
    pub fn missing_header() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            ErrorCode::MissingHeader,
            "writes need `x-shprd: 1`",
        )
    }

    /// The body was not a well-formed request.
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, ErrorCode::BadRequest, message)
    }

    /// The request named something this service has not listed.
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, ErrorCode::NotFound, message)
    }

    /// The action's kind is not one this service knows.
    pub fn unknown_action(kind: &str) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            ErrorCode::UnknownAction,
            format!(
                "unknown action kind {kind:?}: known kinds are {}",
                crate::dto::Action::KINDS.join(", ")
            ),
        )
    }

    /// The request named a task the run does not have.
    pub fn unknown_task(task: &str) -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            ErrorCode::UnknownTask,
            format!("no task {task:?} in this run"),
        )
    }

    /// A rule refused the action; nothing was written.
    pub fn refused(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, ErrorCode::Refused, message)
    }

    /// The agent's terminal waits on a question, so a line typed now would be
    /// refused by the host (or land in the dialog).
    pub fn question_waiting() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            ErrorCode::QuestionWaiting,
            "a question is waiting above: answer it first. Your message was not sent",
        )
    }

    /// The event log could not be read or written.
    pub fn ledger(err: &LogError) -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            ErrorCode::Ledger,
            err.to_string(),
        )
    }

    /// Anything else.
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            ErrorCode::Internal,
            message,
        )
    }
}

impl From<LogError> for ApiReject {
    fn from(err: LogError) -> Self {
        Self::ledger(&err)
    }
}

impl From<crate::graph::DocumentError> for ApiReject {
    /// A document that is not there yet is a `404` — the graph block draws
    /// before a run exists — and anything else is this service's failure to
    /// read its own projection.
    fn from(err: crate::graph::DocumentError) -> Self {
        match err {
            crate::graph::DocumentError::Missing { .. } => {
                Self::new(StatusCode::NOT_FOUND, ErrorCode::NotFound, err.to_string())
            }
            crate::graph::DocumentError::Io { .. }
            | crate::graph::DocumentError::NotADocument { .. } => Self::internal(err.to_string()),
        }
    }
}

impl IntoResponse for ApiReject {
    fn into_response(self) -> Response {
        let mut response = (self.status, Json(self.body)).into_response();
        if let Some(after) = self.retry_after
            && let Ok(value) = axum::http::HeaderValue::from_str(&after.to_string())
        {
            response
                .headers_mut()
                .insert(axum::http::header::RETRY_AFTER, value);
        }
        response
    }
}
