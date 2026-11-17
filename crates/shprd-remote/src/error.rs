//! Typed failures, and the one refusal a person can act on.

use std::path::PathBuf;

/// Why a password a person typed was not accepted.
///
/// These are the checks [`crate::password::PasswordStore::check_rules`] makes
/// before a password is hashed. They are separate from [`RemoteError`] because
/// each one is something the person can fix by typing a different password,
/// while an [`RemoteError`] is the machine's problem.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PasswordRefusal {
    /// Shorter than [`crate::password::MIN_LENGTH`] characters.
    #[error(
        "a password needs at least {} characters, so it is not worth guessing",
        crate::password::MIN_LENGTH
    )]
    TooShort,
    /// Longer than [`crate::password::MAX_LENGTH`] bytes.
    #[error("a password may be at most {} bytes", crate::password::MAX_LENGTH)]
    TooLong,
    /// One of the few passwords every guessing tool tries first.
    #[error("that password is one of the commonest ones: pick something nobody would guess")]
    Common,
    /// The machine's own name, which a tunnel's URL already shows.
    #[error("that password is this machine's name: pick something nobody would guess")]
    Hostname,
}

/// Something this crate could not do.
///
/// Every variant is a refusal rather than a fallback: a state file that will
/// not parse, or a hash that will not verify, is not treated as "no password"
/// or "any session". The gate turns one of these into a `500` and refuses the
/// request.
#[derive(Debug, thiserror::Error)]
pub enum RemoteError {
    /// A state file could not be read, written or made private.
    #[error("i/o error on {path}: {source}")]
    Io {
        /// The path involved.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// A state file holds something that is not the JSON this crate wrote.
    #[error("{what} is not valid: {source}")]
    Json {
        /// Which file it was.
        what: &'static str,
        /// The parse error.
        source: serde_json::Error,
    },
    /// The password hash could not be produced or read.
    #[error("the password hash is unusable: {reason}")]
    Hash {
        /// What argon2 or the PHC string said.
        reason: String,
    },
    /// A secret could not be drawn from the operating system.
    #[error("could not generate a secret: {reason}")]
    Secret {
        /// Why the source of randomness failed.
        reason: String,
    },
    /// A stored password or session is in a state this crate will not use.
    #[error("{0}")]
    Refused(String),
}

impl RemoteError {
    /// Wraps an I/O failure with the path it happened on.
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}
