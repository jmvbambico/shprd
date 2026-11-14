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

impl IntoResponse for ApiReject {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}
