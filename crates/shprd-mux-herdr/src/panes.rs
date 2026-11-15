//! Panes: the process-bearing rectangles shprd starts agents in.
//!
//! A split returns herdr's `pane_info`; a submitted write goes through
//! `agent prompt` so it can never be mistaken for a shell command, falling back
//! to `pane send-text` plus `Enter` for a pane herdr has not detected as an
//! agent; keys are `pane send-keys`; a wait is `pane wait-output` for output and
//! a process-info poll for an exit; and a pane's agent state comes from
//! `agent get`, with "herdr sees no agent here" read as [`AgentState::Unknown`]
//! rather than as an error.

use serde_json::Value;
use shprd_mux::{AgentState, Direction, Host as _, Key, MuxError, Outcome, PaneId, Result, Waited};
use std::path::Path;
use std::time::Duration;

use crate::HerdrMux;
use crate::shell;

impl HerdrMux {
    /// Split `pane`, returning the pane herdr created.
    ///
    /// `ratio` is clamped into `0.0..=1.0` and is the share the *existing*
    /// pane keeps, which is how the trait defines it and how herdr's `--ratio`
    /// behaves.
    pub(crate) fn split_pane(
        &self,
        pane: &PaneId,
        direction: Direction,
        ratio: f32,
        cwd: &Path,
    ) -> Result<PaneId> {
        let ratio = if ratio.is_nan() {
            0.5
        } else {
            ratio.clamp(0.0, 1.0)
        };
        let ratio = format!("{ratio:.3}");
        let cwd = cwd.to_string_lossy().into_owned();
        let direction = match direction {
            Direction::Right => "right",
            Direction::Down => "down",
        };
        let result = self.call_verb(
            "split",
            "pane",
            pane.as_str(),
            "split pane",
            &[
                "pane",
                "split",
                "--pane",
                pane.as_str(),
                "--direction",
                direction,
                "--ratio",
                &ratio,
                "--cwd",
                &cwd,
                "--no-focus",
            ],
        )?;
        Self::new_pane_id(
            &result
                .ok_or_else(|| MuxError::command("split pane", "herdr answered without a pane"))?,
        )
        .map(PaneId::new)
    }

    /// The `pane_id` out of a `pane_info` result.
    pub(crate) fn new_pane_id(result: &Value) -> Result<String> {
        result
            .get("pane")
            .and_then(|pane| pane.get("pane_id"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                MuxError::command("split pane", "herdr answered without the new pane's id")
            })
    }

    /// Type a string into a pane without submitting it.
    pub(crate) fn type_text(&self, pane: &PaneId, text: &str) -> Result<()> {
        self.call_as(
            "pane",
            pane.as_str(),
            "send text",
            &["pane", "send-text", pane.as_str(), text],
        )?;
        Ok(())
    }
}

impl HerdrMux {
    /// Type `text` into `pane`, pressing enter afterwards when `submit`.
    ///
    /// The two paths are different on purpose. A submit is one turn of input for
    /// an agent and one line for a shell, so it goes through `agent prompt`,
    /// which herdr refuses to mistake for a shell command. Typing without a
    /// submit is keystroke-level work, so it is `pane send-text` and nothing
    /// else.
    ///
    /// # Why `agent prompt` is not used for everything
    ///
    /// `agent prompt` only answers for a pane herdr has *detected* as an agent;
    /// a shell, or a CLI that has just started, is not one yet. A caller that
    /// wanted a shell line would get `agent_not_found` instead. So a refused
    /// prompt falls back to `pane send-text` plus `Enter`, which is the same
    /// effect for a shell and still one turn for an agent.
    pub(crate) fn write_to(&self, pane: &PaneId, text: &str, submit: bool) -> Result<()> {
        if !submit {
            return self.type_text(pane, text);
        }
        match self.call_verb(
            "write",
            "pane",
            pane.as_str(),
            "prompt agent",
            &["agent", "prompt", pane.as_str(), text],
        ) {
            Ok(_) => Ok(()),
            Err(MuxError::NotFound { .. }) => {
                // No agent detected here: a shell line, or a CLI still starting.
                self.type_text(pane, text)?;
                self.press_keys(pane, &[Key::Enter])
            }
            Err(error) => Err(error),
        }
    }

    /// Run `argv` in `pane` with `env` applied.
    ///
    /// herdr 0.9.5 has `pane run`, but it takes a shell line of its own and
    /// offers no way to remove a variable, so this builds one quoted line
    /// (`exec env -u NAME NAME=VALUE cmd...`) and sends it as text plus Enter.
    pub(crate) fn run_argv(
        &self,
        pane: &PaneId,
        argv: &[String],
        env: &[(String, Option<String>)],
    ) -> Result<()> {
        let line = shell::line(argv, env)?;
        self.type_text(pane, &line)?;
        self.press_keys(pane, &[Key::Enter])
    }

    /// Press `keys` in `pane`, in order, in one `pane send-keys` call.
    ///
    /// herdr's own key names are the ones [`shprd_mux::Key::word`] emits, so a
    /// key is never translated into a byte sequence here: `ctrl+c` is a word
    /// the host knows, and typing the byte would be a different thing.
    pub(crate) fn press_keys(&self, pane: &PaneId, keys: &[Key]) -> Result<()> {
        if keys.is_empty() {
            return Ok(());
        }
        let words: Vec<String> = keys.iter().map(Key::word).collect();
        let mut args: Vec<&str> = vec!["pane", "send-keys", pane.as_str()];
        args.extend(words.iter().map(String::as_str));
        self.call_verb("keys", "pane", pane.as_str(), "send keys", &args)?;
        Ok(())
    }

    /// The last `lines` lines of a pane's screen.
    ///
    /// `pane read` prints terminal text, not JSON; trailing newlines are
    /// dropped so the caller sees the last non-empty line.
    pub(crate) fn read_pane(&self, pane: &PaneId, lines: usize) -> Result<String> {
        let lines = lines.max(1).to_string();
        let text = self.call_text(
            "read pane",
            &[
                "pane",
                "read",
                pane.as_str(),
                "--lines",
                &lines,
                "--source",
                "recent",
            ],
        )?;
        Ok(text.trim_end_matches('\n').to_string())
    }

    /// Wait up to `timeout` for `wanted` in `pane`.
    ///
    /// # Output waits use the host
    ///
    /// `pane wait-output` searches the snapshot at once and then polls, which is
    /// exactly what a caller wants and cheaper than polling `pane read` through
    /// this adapter (one process per poll). Its exit code says only that the
    /// timeout expired, so a non-zero answer is read as
    /// [`Outcome::running`] — and a host that does not answer the verb at all
    /// degrades to the trait's polling default.
    ///
    /// # Exit waits poll
    ///
    /// herdr has no "wait until this pane's program ends" that does not also
    /// mean "wait until this *agent* reaches a status", and an agent that
    /// finishes its turn is not a process that exited. So an exit wait asks
    /// [`HerdrMux::exit_of`] — `pane process-info` on a pane the host still
    /// knows, [`MuxError::NotFound`] on one it does not — on a short interval.
    pub(crate) fn wait_in(
        &self,
        pane: &PaneId,
        wanted: &Waited,
        timeout: Duration,
    ) -> Result<Outcome> {
        // A wait for either outcome spends one budget, not one per side: the
        // output wait gets the whole of it, and the exit poll gets what is left.
        let started = std::time::Instant::now();
        let text = match wanted {
            Waited::Exit => None,
            Waited::Output(text) | Waited::Either(text) => Some(text.as_str()),
        };
        if let Some(text) = text {
            let millis = u64::try_from(timeout.as_millis())
                .unwrap_or(u64::MAX)
                .to_string();
            let args = [
                "pane",
                "wait-output",
                "--match",
                text,
                pane.as_str(),
                "--timeout",
                &millis,
            ];
            match self.call_verb("wait", "pane", pane.as_str(), "wait for output", &args) {
                Ok(_) => return Ok(Outcome::matched(text)),
                // The verb timed out, or the host does not have it: both mean
                // "nothing matched yet", and the exit side is still asked.
                Err(MuxError::Command { .. } | MuxError::Unsupported(_)) => {}
                Err(MuxError::NotFound { .. }) => {
                    return Ok(Outcome::exited(None));
                }
                Err(error) => return Err(error),
            }
        }
        if matches!(wanted, Waited::Exit | Waited::Either(_)) {
            let remaining = timeout.saturating_sub(started.elapsed());
            return self.poll_exit(pane, remaining);
        }
        Ok(Outcome::running())
    }

    /// Poll [`HerdrMux::exit_of`] until the pane's program is gone or the time
    /// is up.
    fn poll_exit(&self, pane: &PaneId, timeout: Duration) -> Result<Outcome> {
        /// How long between two process-info calls while waiting for an exit.
        const POLL: Duration = Duration::from_millis(250);
        let started = std::time::Instant::now();
        loop {
            match self.exit_of(pane) {
                Ok(Some(outcome)) => return Ok(outcome),
                Ok(None) => {}
                // The pane a program ran in is gone, and with it the program.
                Err(MuxError::NotFound { .. }) => return Ok(Outcome::exited(None)),
                Err(error) => return Err(error),
            }
            let waited = started.elapsed();
            if waited >= timeout {
                return Ok(Outcome::running());
            }
            std::thread::sleep(POLL.min(timeout - waited));
        }
    }

    /// End the program in `pane` without closing the pane.
    ///
    /// herdr has no `pane kill`: the closest verbs are `pane close` (which takes
    /// the pane away, and with it a split's other half) and interrupting the
    /// agent (`agent send-keys ctrl+c`), which only answers for a detected
    /// agent. So a kill here is the interrupt where one is possible and the
    /// close where it is not — the second of which the trait's callers already
    /// treat as "the process is gone", because that is what it means.
    pub(crate) fn kill_pane(&self, pane: &PaneId) -> Result<()> {
        match self.call_verb(
            "kill",
            "pane",
            pane.as_str(),
            "interrupt pane",
            &["pane", "send-keys", pane.as_str(), "ctrl+c"],
        ) {
            Ok(_) => Ok(()),
            Err(MuxError::NotFound { .. }) => Err(MuxError::not_found("pane", pane)),
            Err(error @ MuxError::Forbidden { .. }) => Err(error),
            Err(_) => self.close_pane_of(pane),
        }
    }

    /// The agent state herdr reports for `pane`.
    ///
    /// A pane with no agent in it answers `agent_not_found`, which the trait
    /// defines as [`AgentState::Unknown`], not as a failure.
    pub(crate) fn state_of(&self, pane: &PaneId) -> Result<AgentState> {
        let result = match self.call_verb(
            "agent state",
            "pane",
            pane.as_str(),
            "agent state",
            &["agent", "get", pane.as_str()],
        ) {
            Ok(result) => result,
            Err(MuxError::NotFound { .. }) => return Ok(AgentState::Unknown),
            Err(error) => return Err(error),
        };
        Ok(Self::status_from(result.as_ref()))
    }

    /// Close `pane` and nothing else: the tab and every other pane in it stay.
    ///
    /// `herdr pane close <pane_id>` takes the id positionally. A pane the host
    /// no longer knows is already closed, which is what the caller wanted, so
    /// [`MuxError::NotFound`] is success here.
    pub(crate) fn close_pane_of(&self, pane: &PaneId) -> Result<()> {
        match self.call_verb(
            "close pane",
            "pane",
            pane.as_str(),
            "close pane",
            &["pane", "close", pane.as_str()],
        ) {
            Ok(_) | Err(MuxError::NotFound { .. }) => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// The process group in the foreground of `pane`.
    ///
    /// `pane process-info` answers with the process and the process group
    /// herdr sees in the pane; the group is what shprd compares against the
    /// one it recorded, so a pane a host restored with its own command is
    /// recognisable. An answer without a group is `Ok(None)` — "cannot tell",
    /// which callers read as "leave it alone". A pane herdr no longer knows is
    /// not that: `pane_not_found` becomes [`MuxError::NotFound`], because a
    /// pane that is gone is a process that stopped, and a caller that reads it
    /// as "cannot tell" never replaces it.
    pub(crate) fn process_group_of(&self, pane: &PaneId) -> Result<Option<u32>> {
        let result = self.call_as(
            "pane",
            pane.as_str(),
            "pane process group",
            &["pane", "process-info", "--pane", pane.as_str()],
        )?;

        Ok(Self::process_group_from(result.as_ref()))
    }

    /// The `foreground_process_group_id` out of a `process_info` result.
    pub(crate) fn process_group_from(result: Option<&Value>) -> Option<u32> {
        result
            .and_then(|result| result.get("process_info"))
            .and_then(|info| info.get("foreground_process_group_id"))
            .and_then(Value::as_u64)
            .and_then(|id| u32::try_from(id).ok())
    }

    /// The state in an `agent_info` result; `None` or a missing field is
    /// [`AgentState::Unknown`].
    pub(crate) fn status_from(result: Option<&Value>) -> AgentState {
        result
            .and_then(|result| result.get("agent"))
            .and_then(|agent| agent.get("agent_status"))
            .and_then(Value::as_str)
            .map_or(AgentState::Unknown, map_status)
    }
}

/// Map one of herdr's status words onto [`AgentState`].
///
/// herdr reports `idle`, `working`, `blocked`, `done` and `unknown`; anything
/// else (a newer herdr, a typo) is [`AgentState::Unknown`] so an unknown word
/// can never look like a finished agent.
pub(crate) fn map_status(word: &str) -> AgentState {
    match word {
        "working" => AgentState::Working,
        "idle" => AgentState::Idle,
        "blocked" => AgentState::Blocked,
        "done" => AgentState::Done,
        _ => AgentState::Unknown,
    }
}

#[cfg(test)]
mod tests {
    // Tests may panic: a failed assertion is the point.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use serde_json::json;

    const SPLIT: &str = include_str!("../tests/fixtures/pane_split.json");
    const AGENT: &str = include_str!("../tests/fixtures/agent_get.json");
    const NO_AGENT: &str = include_str!("../tests/fixtures/agent_get_not_found.json");
    const NOT_FOUND: &str = include_str!("../tests/fixtures/pane_read_not_found.json");
    const READ: &str = include_str!("../tests/fixtures/pane_read.txt");
    const PROCESS_INFO: &str = include_str!("../tests/fixtures/pane_process_info.json");
    /// tuios's refusal of a verb that needs the `admin` grant, verbatim from
    /// M7g §2.7.
    const FORBIDDEN: &str = r#"{"id":"cli:tab:create","error":{"code":"forbidden","message":"new-window is refused for plugin shprd.probe: new-window needs the admin grant. The plugin holds read,write. Nothing was done."}}"#;

    fn result(text: &str) -> Value {
        serde_json::from_str::<Value>(text).unwrap()["result"].clone()
    }

    #[test]
    fn a_split_returns_the_pane_herdr_created() {
        assert_eq!(HerdrMux::new_pane_id(&result(SPLIT)).unwrap(), "w1:p5");
    }

    #[test]
    fn a_split_without_a_pane_id_is_an_error() {
        assert!(HerdrMux::new_pane_id(&json!({"type": "pane_info"})).is_err());
    }

    #[test]
    fn herdr_status_words_map_onto_agent_states() {
        assert_eq!(map_status("working"), AgentState::Working);
        assert_eq!(map_status("idle"), AgentState::Idle);
        assert_eq!(map_status("blocked"), AgentState::Blocked);
        assert_eq!(map_status("done"), AgentState::Done);
        assert_eq!(map_status("unknown"), AgentState::Unknown);
        assert_eq!(map_status("thinking"), AgentState::Unknown);
        assert_eq!(map_status(""), AgentState::Unknown);
    }

    #[test]
    fn agent_get_reports_the_running_agents_state() {
        assert_eq!(
            HerdrMux::status_from(Some(&result(AGENT))),
            AgentState::Working
        );
    }

    #[test]
    fn a_pane_with_no_agent_has_no_state_not_a_failure() {
        assert!(NO_AGENT.contains("agent_not_found"));
        let answer = crate::cli::Answer {
            stdout: String::new(),
            stderr: NOT_FOUND.to_string(),
            ok: false,
        };
        assert!(answer.object().unwrap_err().is_not_found());
        assert_eq!(HerdrMux::status_from(None), AgentState::Unknown);
        assert_eq!(HerdrMux::status_from(Some(&json!({}))), AgentState::Unknown);
    }

    #[test]
    fn process_info_reports_the_foreground_process_group() {
        assert_eq!(
            HerdrMux::process_group_from(Some(&result(PROCESS_INFO))),
            Some(88158)
        );
    }

    #[test]
    fn a_process_info_without_a_group_says_cannot_tell() {
        assert_eq!(HerdrMux::process_group_from(None), None);
        assert_eq!(HerdrMux::process_group_from(Some(&json!({}))), None);
        assert_eq!(
            HerdrMux::process_group_from(Some(
                &json!({"process_info": {"foreground_process_group_id": null}})
            )),
            None
        );
        assert_eq!(
            HerdrMux::process_group_from(Some(
                &json!({"process_info": {"foreground_process_group_id": -1}})
            )),
            None
        );
    }

    /// A `herdr` that answers every command with the JSON herdr prints for a
    /// pane it no longer knows, which is how a pane a human closed looks.
    #[cfg(unix)]
    fn herdr_without_the_pane(dir: &Path) -> HerdrMux {
        use std::os::unix::fs::PermissionsExt;

        let bin = dir.join("herdr");
        let script = format!(
            "#!/bin/sh\ncat >&2 <<'JSON'\n{}\nJSON\nexit 1\n",
            NOT_FOUND.trim()
        );
        // Written beside the final path and renamed into place: the path that is
        // executed is never open for writing, so no sibling thread's child can
        // have a write descriptor on it and make the exec fail with `ETXTBSY`.
        let temp = dir.join("herdr.tmp");
        std::fs::write(&temp, script).unwrap();
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::rename(&temp, &bin).unwrap();
        HerdrMux::with_bin(&bin)
    }

    #[test]
    #[cfg(unix)]
    fn a_pane_the_host_forgot_is_not_found_not_cannot_tell() {
        let dir = tempfile::tempdir().unwrap();
        let mux = herdr_without_the_pane(dir.path());
        let err = mux.process_group_of(&PaneId::new("nope:p9")).unwrap_err();
        assert!(
            matches!(err, MuxError::NotFound { .. }),
            "a vanished pane is gone, not a host that cannot tell: {err}"
        );
    }

    #[test]
    fn pane_read_fixture_is_terminal_text_not_json() {
        assert!(READ.contains("hello"));
    }

    /// A `herdr` that refuses the creating verbs the way tuios does without the
    /// `admin` grant, and answers everything else as herdr would.
    #[cfg(unix)]
    fn herdr_that_forbids_spawning(dir: &Path) -> HerdrMux {
        use std::os::unix::fs::PermissionsExt;

        let bin = dir.join("herdr");
        let script = format!(
            "#!/bin/sh\ncat >&2 <<'JSON'\n{}\nJSON\nexit 1\n",
            FORBIDDEN.trim()
        );
        let temp = dir.join("herdr.tmp");
        std::fs::write(&temp, script).unwrap();
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::rename(&temp, &bin).unwrap();
        HerdrMux::with_bin(&bin)
    }

    /// tuios without `admin`: the refusal is typed, and the flags stop
    /// promising the verb.
    ///
    /// Both halves matter to a caller. The error says what a person can grant
    /// ("needs the admin grant"); the flag is what lets a caller plan around it
    /// instead of asking again every pass.
    #[test]
    #[cfg(unix)]
    fn a_forbidden_spawn_is_reported_and_removes_the_capability() {
        let dir = tempfile::tempdir().unwrap();
        let mux = herdr_that_forbids_spawning(dir.path());
        assert!(mux.capabilities().spawn, "nothing has been refused yet");
        let space = shprd_mux::SpaceId::new("w1");

        let err = shprd_mux::Host::spawn(
            &mux,
            &space,
            "worker",
            &["claude".to_string()],
            &[],
            Path::new("/tmp"),
        )
        .unwrap_err();

        match &err {
            MuxError::Forbidden { verb, reason } => {
                assert_eq!(*verb, "spawn");
                assert!(
                    reason.contains("admin grant"),
                    "the host's own instructions are kept: {reason}"
                );
            }
            other => panic!("a forbidden reply must be Forbidden, got {other:?}"),
        }
        assert!(
            !mux.capabilities().spawn,
            "the failed spawn is not promised again"
        );
        assert!(mux.capabilities().split, "the other verbs are untouched");
    }
}
