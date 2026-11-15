//! Running one `herdr` command and reading the JSON object it prints.
//!
//! Every herdr CLI command answers with a single object: `{"id":…,"result":…}`
//! on success or `{"id":…,"error":{"code":…,"message":…}}` on failure. A few
//! commands print no object at all on success (`pane send-text`), and `pane
//! read` prints the terminal text itself, so the runner distinguishes "object",
//! "nothing" and "text" instead of insisting on JSON everywhere.

use serde_json::Value;
use shprd_mux::{MuxError, Result};
use std::io;
use std::process::Command;
use std::thread;
use std::time::Duration;

use crate::HerdrMux;

/// The longest detail string this adapter will ever put in a [`MuxError`].
const DETAIL_LIMIT: usize = 200;

/// How long to wait before each retry of a spawn the kernel refused with
/// `ETXTBSY`, one delay per retry after the first attempt.
///
/// This mirrors the delays in `shprd_core::spawn`, which this crate cannot
/// share: the adapter sits below `shprd-core` in the dependency graph and must
/// not know about runs, events or config.
const BUSY_RETRY_DELAYS: [Duration; 4] = [
    Duration::from_millis(10),
    Duration::from_millis(20),
    Duration::from_millis(40),
    Duration::from_millis(80),
];

/// `ETXTBSY`, as the kernel reports it in [`io::Error::raw_os_error`].
#[cfg(unix)]
const ETXTBSY: i32 = 26;

/// Runs `attempt` until it no longer fails with `ETXTBSY`.
///
/// `execve` fails with `ETXTBSY` ("Text file busy") while any process holds a
/// descriptor open for writing on the file being executed — a `herdr` binary
/// that was just installed, or the child another thread forked between its
/// `fork` and its `execve`. Only that error is retried, so a missing or broken
/// binary still fails on the first try; so does a `herdr` that stays busy after
/// the last retry, with the error the kernel reported.
fn retry_busy<T>(mut attempt: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let mut backoffs = BUSY_RETRY_DELAYS.iter();
    loop {
        match attempt() {
            Ok(value) => return Ok(value),
            Err(error) => {
                if !is_text_file_busy(&error) {
                    return Err(error);
                }
                match backoffs.next() {
                    Some(backoff) => thread::sleep(*backoff),
                    None => return Err(error),
                }
            }
        }
    }
}

/// [`Command::output`], retrying the spawn while the kernel reports `ETXTBSY`.
fn output_retrying_busy(command: &mut Command) -> io::Result<std::process::Output> {
    retry_busy(|| command.output())
}

/// Whether `error` is the kernel refusing to execute a file that is open for
/// writing.
fn is_text_file_busy(error: &io::Error) -> bool {
    if error.kind() == io::ErrorKind::ExecutableFileBusy {
        return true;
    }
    #[cfg(unix)]
    {
        error.raw_os_error() == Some(ETXTBSY)
    }
    #[cfg(not(unix))]
    {
        false
    }
}

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

    /// True when the host allowed the verb and refused *this* caller it.
    ///
    /// herdr never answers this; tuios does, for every verb that needs the
    /// `admin` grant (`new-window`, `new-session`, `pane.split`), with
    /// `{"code":"forbidden","message":"… needs the admin grant …"}` (M7g §2.7).
    /// The difference matters: a missing verb cannot be fixed, and a missing
    /// grant is one line of the host's own instructions away.
    pub(crate) fn is_forbidden(&self) -> bool {
        matches!(
            self.code.as_deref(),
            Some("forbidden" | "grant_required" | "permission_denied")
        )
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
    /// The environment variables a `herdr` call must not inherit.
    ///
    /// A herdr pane exports the pane it is in (`HERDR_ENV`, `HERDR_PANE_ID`,
    /// `HERDR_TAB_ID`, `HERDR_WORKSPACE_ID`, `HERDR_SESSION`), and `herdr` uses
    /// them as the *default target*: `pane split` with no target splits the
    /// calling pane. An adapter explicitly pointed at a server therefore removes
    /// them, so a sandbox or a live test that names a private server can only act
    /// on that server — never on the pane it happened to be started from. An
    /// adapter with no server of its own keeps them: that is the person's own
    /// session, which is exactly what they asked for.
    const PINNED_SESSION_VARS: [&'static str; 6] = [
        "HERDR_ENV",
        "HERDR_PANE_ID",
        "HERDR_TAB_ID",
        "HERDR_WORKSPACE_ID",
        "HERDR_SESSION",
        "HC_SESSION",
    ];

    /// The `herdr` command this adapter runs for `args`, before it is started.
    ///
    /// Kept apart from [`HerdrMux::answer`] so the environment a command is
    /// given can be asserted without a process: see the test at the bottom of
    /// this file.
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(&self.bin);
        command.args(args);
        if let Some(socket) = &self.socket {
            command.env("HERDR_SOCKET_PATH", socket);
            // A named server means a named place to work; the pane that started
            // this process is not in it.
            for name in Self::PINNED_SESSION_VARS {
                command.env_remove(name);
            }
        }
        if let Some(home) = &self.home {
            command.env("HOME", home);
        }
        command
    }

    /// Run `herdr` with `args` and capture both streams.
    fn answer(&self, args: &[&str]) -> std::result::Result<Answer, Failure> {
        let mut command = self.command(args);
        let output = output_retrying_busy(&mut command).map_err(|error| {
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

    /// [`HerdrMux::call_as`] for a verb the host may refuse this caller: a
    /// `forbidden` code becomes [`MuxError::Forbidden`] naming `verb`, so a
    /// caller can tell "not allowed" from "not there".
    pub(crate) fn call_verb(
        &self,
        verb: &'static str,
        what: &'static str,
        id: &str,
        context: &str,
        args: &[&str],
    ) -> Result<Option<Value>> {
        let answer = self.answer(args).map_err(|failure| {
            let error = failure.into_mux_error_for(verb, context);
            self.note_if_forbidden(verb, &error);
            error
        })?;
        match answer.object() {
            Ok(value) => Ok(value.and_then(|value| value.get("result").cloned())),
            Err(failure) if failure.is_not_found() => Err(MuxError::not_found(what, id)),
            Err(failure) => {
                let error = failure.into_mux_error_for(verb, context);
                self.note_if_forbidden(verb, &error);
                Err(error)
            }
        }
    }

    /// Remember a refusal, so [`shprd_mux::Host::capabilities`] stops promising
    /// a verb the host will not allow this caller.
    fn note_if_forbidden(&self, verb: &'static str, error: &MuxError) {
        if matches!(error, MuxError::Forbidden { .. }) {
            self.note_refusal(verb);
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

    /// Turn this failure into the trait error for `verb`, keeping a host's
    /// refusal to allow it (`forbidden` on tuios) apart from a failure.
    ///
    /// The refusal's message is the host's own, verbatim: it names the grant
    /// and where to give it, which is the only thing a person can act on.
    pub(crate) fn into_mux_error_for(self, verb: &'static str, context: &str) -> MuxError {
        if self.is_forbidden() {
            return MuxError::forbidden(verb, truncate(&self.message));
        }
        self.into_mux_error(context)
    }
}

#[cfg(test)]
mod tests {
    // Tests may panic: a failed assertion is the point.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn a_busy_spawn_is_retried_until_it_succeeds() {
        let mut attempts = 0;
        let outcome = retry_busy(|| {
            attempts += 1;
            if attempts < 3 {
                Err(io::Error::from_raw_os_error(26))
            } else {
                Ok("ran")
            }
        });
        assert_eq!(outcome.unwrap(), "ran");
        assert_eq!(attempts, 3);
    }

    #[test]
    fn a_spawn_that_stays_busy_returns_the_last_busy_error() {
        let mut attempts = 0;
        let outcome: io::Result<()> = retry_busy(|| {
            attempts += 1;
            Err(io::Error::from_raw_os_error(26))
        });
        assert_eq!(outcome.unwrap_err().raw_os_error(), Some(26));
        assert_eq!(attempts, BUSY_RETRY_DELAYS.len() + 1);
    }

    #[test]
    fn another_error_is_not_retried() {
        let mut attempts = 0;
        let outcome: io::Result<()> = retry_busy(|| {
            attempts += 1;
            Err(io::Error::from(io::ErrorKind::NotFound))
        });
        assert_eq!(outcome.unwrap_err().kind(), io::ErrorKind::NotFound);
        assert_eq!(attempts, 1);
    }

    const WORKSPACE_LIST: &str = include_str!("../tests/fixtures/workspace_list.json");
    /// tuios's refusal of a verb that needs the `admin` grant, verbatim from
    /// M7g §2.7: the message names the grant and where a person gives it.
    const FORBIDDEN: &str = r#"{"id":"cli:tab:create","error":{"code":"forbidden","message":"new-window is refused for plugin shprd.probe: new-window needs the admin grant. The plugin holds read,write. Nothing was done. A plugin asks for grants in [permissions] of tuios-plugin.toml."}}"#;
    /// tuios's answer for a method it does not implement at all, verbatim from
    /// M7g §2.7: this is "no such verb", not "not allowed".
    const UNSUPPORTED: &str = r#"{"id":"cli:workspace:move","error":{"code":"unsupported","message":"tuios does not answer workspace.move. See the herdr compatibility section of docs/AGENT_STATE.md for what it answers"}}"#;
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
    fn a_forbidden_reply_is_the_host_refusing_this_caller_not_a_broken_host() {
        let failure = answer("", FORBIDDEN, false).object().err().unwrap();
        assert_eq!(failure.code.as_deref(), Some("forbidden"));
        assert!(failure.is_forbidden());
        assert!(!failure.is_not_found());
        let error = failure.into_mux_error_for("spawn", "create tab");
        match &error {
            MuxError::Forbidden { verb, reason } => {
                assert_eq!(*verb, "spawn");
                assert!(
                    reason.contains("admin grant"),
                    "the host's instruction is kept verbatim: {reason}"
                );
            }
            other => panic!("a forbidden reply must be Forbidden, got {other:?}"),
        }
        assert_eq!(
            error.to_string(),
            format!(
                "this host refused the spawn: {}",
                match &error {
                    MuxError::Forbidden { reason, .. } => reason.clone(),
                    _ => String::new(),
                }
            )
        );
    }

    #[test]
    fn a_forbidden_reply_through_call_as_names_the_default_verb() {
        // `call_as` has no verb to name, so a refusal that reaches it is still a
        // command failure rather than being swallowed: only `call_verb` knows
        // what was being attempted.
        let failure = answer("", FORBIDDEN, false).object().err().unwrap();
        assert!(matches!(
            failure.into_mux_error("create tab"),
            MuxError::Command { .. }
        ));
    }

    #[test]
    fn an_unimplemented_verb_is_not_read_as_a_permission_problem() {
        let failure = answer("", UNSUPPORTED, false).object().err().unwrap();
        assert_eq!(failure.code.as_deref(), Some("unsupported"));
        assert!(!failure.is_forbidden());
        assert!(matches!(
            failure.into_mux_error_for("doc", "open a document"),
            MuxError::Command { .. }
        ));
    }

    #[test]
    fn the_other_words_a_host_uses_for_a_refusal_are_recognised() {
        for code in ["grant_required", "permission_denied"] {
            let failure = Failure {
                code: Some(code.to_string()),
                message: "no".to_string(),
            };
            assert!(failure.is_forbidden(), "{code} is a refusal");
        }
        let missing = Failure {
            code: Some("pane_not_found".to_string()),
            message: "no".to_string(),
        };
        assert!(!missing.is_forbidden());
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

    /// An adapter that names its own server removes the pane variables a herdr
    /// pane exported to this process: they are `herdr`'s own default target, so
    /// keeping them would let one command act on the pane that started it even
    /// though the adapter was told which server to work in.
    #[test]
    fn an_adapter_with_its_own_server_does_not_carry_the_callers_pane() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("s.sock");
        let mux =
            HerdrMux::with_bin(dir.path().join("herdr")).with_environment(&socket, dir.path());

        let command = mux.command(&["version"]);
        let value = |name: &str| {
            command
                .get_envs()
                .find(|(key, _)| *key == std::ffi::OsStr::new(name))
                .map(|(_, value)| value.map(std::borrow::ToOwned::to_owned))
        };
        for name in HerdrMux::PINNED_SESSION_VARS {
            assert_eq!(
                value(name),
                Some(None),
                "{name} is cleared, so herdr cannot default to the pane that started shprd"
            );
        }
        assert_eq!(
            value("HERDR_SOCKET_PATH").flatten().as_deref(),
            Some(socket.as_os_str()),
            "the server the adapter does name still reaches the child"
        );
    }

    /// An adapter with no server of its own is the person's own session, so it
    /// leaves the pane variables alone: that is exactly what they asked for.
    #[test]
    fn an_adapter_with_no_server_keeps_the_environment_it_was_given() {
        let mux = HerdrMux::with_bin("/usr/bin/true");
        let command = mux.command(&["version"]);
        assert!(
            command
                .get_envs()
                .find(|(key, _)| *key == "HERDR_SOCKET_PATH")
                .is_none(),
            "nothing is overridden when the adapter was not told which host to use"
        );
    }
}
