//! Running one `herdr` command and reading the JSON object it prints.
//!
//! Every herdr CLI command answers with a single object: `{"id":…,"result":…}`
//! on success or `{"id":…,"error":{"code":…,"message":…}}` on failure. A few
//! commands print no object at all on success (`pane send-text`), and `pane
//! read` prints the terminal text itself, so the runner distinguishes "object",
//! "nothing" and "text" instead of insisting on JSON everywhere.

use serde_json::Value;
use shprd_mux::{MuxError, Result};
use std::process::Command;

use crate::HerdrMux;

/// The longest detail string this adapter will ever put in a [`MuxError`].
const DETAIL_LIMIT: usize = 200;

/// A herdr error, as the CLI reported it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Failure {
    /// The machine-readable code, e.g. `"pane_not_found"`; `None` when herdr
    /// printed something that was not an error object.
    pub(crate) code: Option<String>,
    /// The human-readable message herdr printed, or our own diagnosis.
    pub(crate) message: String,
}

impl Failure {
    /// A failure we made up, because herdr did not answer in a shape we know.
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            code: None,
            message: message.into(),
        }
    }

    /// `"code: message"`, truncated, ready for [`MuxError::Command`].
    pub(crate) fn detail(&self) -> String {
        let text = match &self.code {
            Some(code) => format!("{code}: {}", self.message),
            None => self.message.clone(),
        };
        truncate(&text)
    }

    /// True when herdr said the object named in the code does not exist.
    pub(crate) fn is_not_found(&self) -> bool {
        self.code
            .as_deref()
            .is_some_and(|code| code.ends_with("not_found"))
    }
}

/// Keep a diagnosis short enough to log on one line.
pub(crate) fn truncate(text: &str) -> String {
    let flat = text.replace('\n', " ");
    if flat.chars().count() <= DETAIL_LIMIT {
        return flat;
    }
    let mut cut: String = flat.chars().take(DETAIL_LIMIT - 3).collect();
    cut.push_str("...");
    cut
}

/// What one finished command left behind, before any interpretation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Answer {
    /// Everything herdr wrote to stdout.
    pub(crate) stdout: String,
    /// Everything herdr wrote to stderr.
    pub(crate) stderr: String,
    /// Whether the process exited zero.
    pub(crate) ok: bool,
}

impl Answer {
    /// Interpret the answer as the JSON object herdr prints, if it printed one.
    ///
    /// `Ok(None)` means "no object", which is a success for commands that print
    /// nothing. An error object on either stream becomes a [`Failure`], and so
    /// does a non-zero exit with no JSON at all.
    pub(crate) fn object(&self) -> std::result::Result<Option<Value>, Failure> {
        let from_stdout = parse_object(&self.stdout);
        if let Some(Err(failure)) = &from_stdout {
            return Err(failure.clone());
        }
        if let Some(Ok(value)) = from_stdout {
            return Ok(Some(value));
        }
        if let Some(Err(failure)) = parse_object(&self.stderr) {
            return Err(failure);
        }
        if self.ok {
            return Ok(None);
        }
        let text = if self.stderr.trim().is_empty() {
            self.stdout.trim()
        } else {
            self.stderr.trim()
        };
        Err(Failure::new(if text.is_empty() {
            "herdr exited non-zero without a message".to_string()
        } else {
            text.to_string()
        }))
    }
}

/// Parse one JSON error object out of `text`, ignoring surrounding noise.
fn parse_object(text: &str) -> Option<std::result::Result<Value, Failure>> {
    let start = text.find('{')?;
    let value: Value = serde_json::from_str(&text[start..]).ok()?;
    Some(match value.get("error") {
        Some(error) => Err(Failure {
            code: error
                .get("code")
                .and_then(Value::as_str)
                .map(str::to_string),
            message: error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("herdr reported an error without a message")
                .to_string(),
        }),
        None => Ok(value),
    })
}

impl HerdrMux {
    /// Run `herdr` with `args` and capture both streams.
    fn answer(&self, args: &[&str]) -> std::result::Result<Answer, Failure> {
        let mut command = Command::new(&self.bin);
        command.args(args);
        if let Some(socket) = &self.socket {
            command.env("HERDR_SOCKET_PATH", socket);
        }
        if let Some(home) = &self.home {
            command.env("HOME", home);
        }
        let output = command.output().map_err(|error| {
            Failure::new(format!("could not start {}: {error}", self.bin.display()))
        })?;
        Ok(Answer {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            ok: output.status.success(),
        })
    }

    /// The `result` object of a command that must answer with one.
    pub(crate) fn call(&self, context: &str, args: &[&str]) -> Result<Value> {
        match self.call_optional(context, args)? {
            Some(value) => Ok(value),
            None => Err(MuxError::command(context, "herdr printed no result object")),
        }
    }

    /// A command whose success carries no object at all.
    pub(crate) fn call_optional(&self, context: &str, args: &[&str]) -> Result<Option<Value>> {
        let answer = self
            .answer(args)
            .map_err(|failure| failure.into_mux_error(context))?;
        answer
            .object()
            .map(|value| value.and_then(|value| value.get("result").cloned()))
            .map_err(|failure| failure.into_mux_error(context))
    }

    /// Like [`HerdrMux::call_optional`], but "herdr does not know `id`" becomes
    /// [`MuxError::NotFound`] instead of a command failure, as [`crate::Mux`]
    /// promises for every method that takes an identifier.
    pub(crate) fn call_as(
        &self,
        what: &'static str,
        id: &str,
        context: &str,
        args: &[&str],
    ) -> Result<Option<Value>> {
        let answer = self
            .answer(args)
            .map_err(|failure| failure.into_mux_error(context))?;
        match answer.object() {
            Ok(value) => Ok(value.and_then(|value| value.get("result").cloned())),
            Err(failure) if failure.is_not_found() => Err(MuxError::not_found(what, id)),
            Err(failure) => Err(failure.into_mux_error(context)),
        }
    }

    /// A command that prints text rather than JSON, e.g. `pane read`.
    pub(crate) fn call_text(&self, context: &str, args: &[&str]) -> Result<String> {
        let answer = self
            .answer(args)
            .map_err(|failure| failure.into_mux_error(context))?;
        match answer.object() {
            Ok(_) => Ok(answer.stdout),
            Err(failure) => Err(failure.into_mux_error(context)),
        }
    }
}

impl Failure {
    /// Turn this failure into the trait error, naming what shprd was doing.
    pub(crate) fn into_mux_error(self, context: &str) -> MuxError {
        MuxError::command(context.to_string(), self.detail())
    }
}

#[cfg(test)]
mod tests {
    // Tests may panic: a failed assertion is the point.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    const WORKSPACE_LIST: &str = include_str!("../tests/fixtures/workspace_list.json");
    const PANE_NOT_FOUND: &str = include_str!("../tests/fixtures/pane_read_not_found.json");
    const SERVER_DOWN: &str = r#"{"id":"cli:workspace:list","error":{"code":"server_not_running","message":"no herdr server is running at /tmp/fixture/herdr.sock; run `herdr` to start or attach it"}}"#;
    const PANE_READ: &str = include_str!("../tests/fixtures/pane_read.txt");

    fn answer(stdout: &str, stderr: &str, ok: bool) -> Answer {
        Answer {
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            ok,
        }
    }

    #[test]
    fn reads_the_result_object_of_a_successful_command() {
        let object = answer(WORKSPACE_LIST, "", true).object().ok().flatten();
        assert_eq!(
            object
                .as_ref()
                .and_then(|value| value.get("id"))
                .and_then(Value::as_str),
            Some("cli:workspace:list")
        );
        assert!(
            object
                .as_ref()
                .and_then(|value| value["result"].get("workspaces"))
                .and_then(Value::as_array)
                .is_some()
        );
    }

    #[test]
    fn an_error_object_on_stderr_becomes_a_failure() {
        let failure = answer("", PANE_NOT_FOUND, false).object().err().unwrap();
        assert_eq!(failure.code.as_deref(), Some("pane_not_found"));
        assert!(failure.is_not_found());
        assert_eq!(failure.detail(), "pane_not_found: pane nope:p9 not found");
    }

    #[test]
    fn an_error_object_on_stdout_is_a_failure_too() {
        let failure = answer(SERVER_DOWN, "", false).object().err().unwrap();
        assert_eq!(failure.code.as_deref(), Some("server_not_running"));
        assert!(!failure.is_not_found());
    }

    #[test]
    fn a_command_that_prints_nothing_succeeds_with_no_object() {
        assert_eq!(answer("", "", true).object(), Ok(None));
    }

    #[test]
    fn a_silent_non_zero_exit_reports_stderr_text() {
        let failure = answer("", "kaboom: no socket", false)
            .object()
            .err()
            .unwrap();
        assert_eq!(failure.code, None);
        assert_eq!(failure.detail(), "kaboom: no socket");
    }

    #[test]
    fn terminal_text_is_not_mistaken_for_json() {
        assert_eq!(answer(PANE_READ, "", true).object(), Ok(None));
    }

    #[test]
    fn a_line_that_looks_like_json_still_reports_as_text() {
        let text = "user@host % echo '{}'\n{}\n";
        assert!(answer(text, "", true).object().is_ok());
    }

    #[test]
    fn details_are_truncated_to_two_hundred_characters() {
        let long = "x".repeat(500);
        let failure = Failure {
            code: Some("boom".to_string()),
            message: long,
        };
        let detail = failure.detail();
        assert_eq!(detail.chars().count(), 200);
        assert!(detail.ends_with("..."));
        assert!(detail.starts_with("boom: "));
    }

    #[test]
    fn details_never_span_lines() {
        let failure = Failure::new("first\nsecond");
        assert_eq!(failure.detail(), "first second");
    }
}
