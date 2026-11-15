//! The prompt routes: read one prompt's words, and save the person's own.
//!
//! A prompt is a file in the person's prompts directory
//! ([`shprd_core::prompts`]), so `GET /api/prompts/:id` answers what an agent is
//! told today and `PUT /api/prompts/:id` writes what a prompt editor saved. The
//! read installs a shipped prompt on first use, exactly as the command line
//! does; the write always stores the person's text, because they asked for it.
//!
//! Nothing here ever replaces an edit on its own: the shipped words go back only
//! through an explicit reset, which is the command line's `shprd prompts reset`.

use std::path::PathBuf;

use axum::body::Bytes;
use axum::http::StatusCode;
use shprd_core::prompts::{self, Installed, PromptError};

use crate::api::AppState;
use crate::dto::{ErrorCode, PromptDto, PromptPutDto, PromptStateDto};
use crate::error::ApiReject;

/// `GET /api/prompts/:id`: one prompt, its file and whether it is the default.
pub(crate) fn read(state: &AppState, id: &str) -> Result<PromptDto, ApiReject> {
    let dir = prompts_dir(state)?;
    let installed = prompts::ensure(&dir, id).map_err(refusal)?;
    Ok(document(installed))
}

/// `PUT /api/prompts/:id`: store the person's words over the file.
pub(crate) fn write(state: &AppState, id: &str, body: &Bytes) -> Result<PromptDto, ApiReject> {
    let request: PromptPutDto = serde_json::from_slice(body).map_err(|err| {
        ApiReject::bad_request(format!(
            "the body is not a prompt write: {err}; send {{\"text\": \"the role's words\"}}"
        ))
    })?;
    let dir = prompts_dir(state)?;
    let installed = prompts::write(&dir, id, &request.text).map_err(refusal)?;
    Ok(document(installed))
}

/// The prompts directory this service reads and writes.
///
/// It is the person's own, resolved by the binary from `$XDG_CONFIG_HOME` (else
/// `<home>/.config`) and passed in; a test points it at a temp directory. The
/// service does not resolve it itself, so it never reads the process's XDG
/// settings and can never touch the developer's prompts by accident.
fn prompts_dir(state: &AppState) -> Result<PathBuf, ApiReject> {
    state.prompts_dir.clone().ok_or_else(|| {
        ApiReject::internal(
            "this service was not told where the prompts are: `shprd serve` sets it from \
             $XDG_CONFIG_HOME, else <home>/.config",
        )
    })
}

/// The DTO one installed prompt becomes.
fn document(installed: Installed) -> PromptDto {
    PromptDto {
        id: installed.id,
        path: installed.path.display().to_string(),
        text: installed.text,
        state: match installed.state {
            prompts::State::Default => PromptStateDto::Default,
            prompts::State::Edited => PromptStateDto::Edited,
        },
    }
}

/// The refusal a prompt failure produces.
///
/// An id no prompt exists for is a 404 the page can explain; an id that could
/// name a file outside the prompts directory is the request's fault; anything
/// left is the disk's.
fn refusal(error: PromptError) -> ApiReject {
    match &error {
        PromptError::Unknown { .. } => ApiReject::new(
            StatusCode::NOT_FOUND,
            ErrorCode::NotFound,
            error.to_string(),
        ),
        PromptError::BadId { .. } | PromptError::BadRef { .. } => {
            ApiReject::bad_request(error.to_string())
        }
        PromptError::Read { .. } | PromptError::Write { .. } | PromptError::Encoding { .. } => {
            ApiReject::internal(error.to_string())
        }
    }
}
