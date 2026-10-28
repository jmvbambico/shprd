//! Panes: the process-bearing rectangles shprd starts agents in.
//!
//! A split returns herdr's `pane_info`; a prompt goes through `agent prompt`
//! so it can never be mistaken for a shell command; and a pane's agent state
//! comes from `agent get`, with "herdr sees no agent here" read as
//! [`AgentState::Unknown`] rather than as an error.

use serde_json::Value;
use shprd_mux::{AgentState, Direction, MuxError, PaneId, Result};
use std::path::Path;

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
        let result = self.call_as(
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
        self.call_as(
            "pane",
            pane.as_str(),
            "submit command",
            &["pane", "send-keys", pane.as_str(), "Enter"],
        )?;
        Ok(())
    }

    /// Submit `text` to the agent in `pane` as one turn of input.
    pub(crate) fn prompt_agent(&self, pane: &PaneId, text: &str) -> Result<()> {
        self.call_as(
            "pane",
            pane.as_str(),
            "prompt agent",
            &["agent", "prompt", pane.as_str(), text],
        )?;
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

    /// The agent state herdr reports for `pane`.
    ///
    /// A pane with no agent in it answers `agent_not_found`, which the trait
    /// defines as [`AgentState::Unknown`], not as a failure.
    pub(crate) fn state_of(&self, pane: &PaneId) -> Result<AgentState> {
        let result = match self.call_as(
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
        match self.call_as(
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
    /// recognisable. A pane herdr no longer knows, or an answer without a
    /// group, is `Ok(None)` — "cannot tell", which callers read as "leave it
    /// alone".
    pub(crate) fn process_group_of(&self, pane: &PaneId) -> Result<Option<u32>> {
        let result = match self.call_as(
            "pane",
            pane.as_str(),
            "pane process group",
            &["pane", "process-info", "--pane", pane.as_str()],
        ) {
            Ok(result) => result,
            Err(MuxError::NotFound { .. }) => return Ok(None),
            Err(error) => return Err(error),
        };
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

    #[test]
    fn pane_read_fixture_is_terminal_text_not_json() {
        assert!(READ.contains("hello"));
    }
}
