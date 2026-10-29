//! The status line: where every task sits, in one glance.
//!
//! `shprd status` prints this; nothing here asks a model anything.

use shprd_core::state::{AgentInfo, RunState, TaskStatus};

use crate::clock;

/// One line per task, then one line per agent that is dry at `now_ms`.
///
/// A directory with no run says `no run here · start one with shprd up`, and a
/// run whose plan is still empty says `run <id> · no tasks yet · talk to the ★
/// orchestrator tab`: an empty screen would otherwise look like a broken run.
///
/// Each task line starts with a symbol: `○` queued, `●` working, `◇` reported
/// and awaiting review, `✓` for a passed task (`✓ <task> passed <title>`) or
/// one whose work landed (`✓ <task> landed <title>`), `✗` failed, or `⊘` for a
/// task a human dropped (`⊘ <task> dropped <title>`). Tasks are listed in id order. A dry agent is `◌ <agent> dry
/// until HH:MM (reason) — shprd readmit <agent>` in UTC; an agent dry with no
/// known reset time shows `unknown`.
pub fn status(state: &RunState, now_ms: u64) -> String {
    let mut lines: Vec<String> = Vec::new();
    match &state.run {
        None => lines.push("no run here · start one with shprd up".to_string()),
        Some(run) if state.tasks.is_empty() => lines.push(format!(
            "run {} · no tasks yet · talk to the ★ orchestrator tab",
            run.run_id
        )),
        Some(_) => {
            for task in state.tasks.values() {
                lines.push(format!(
                    "{} {} {} {}",
                    symbol(task.status),
                    task.id,
                    word(task.status),
                    task.title
                ));
            }
        }
    }
    for (agent, info) in &state.agents {
        if is_dry_now(info, now_ms) {
            lines.push(dry_line(agent, info));
        }
    }
    lines.join("\n")
}

/// The symbol shown in front of a task in `status`.
fn symbol(status: TaskStatus) -> char {
    match status {
        TaskStatus::Queued => '○',
        TaskStatus::Working => '●',
        TaskStatus::Reported => '◇',
        TaskStatus::Passed | TaskStatus::Integrated => '✓',
        TaskStatus::Failed => '✗',
        TaskStatus::Dropped => '⊘',
    }
}

/// The word printed after the task id.
fn word(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Queued => "queued",
        TaskStatus::Working => "working",
        TaskStatus::Reported => "reported",
        TaskStatus::Passed => "passed",
        TaskStatus::Integrated => "landed",
        TaskStatus::Failed => "failed",
        TaskStatus::Dropped => "dropped",
    }
}

/// One dry agent's line: how long it is out, why, and how to bring it back.
///
/// The hint is the point: a dry mark whose reset time is `unknown` never
/// clears itself, so a human has to be told the verb that clears it.
fn dry_line(agent: &str, info: &AgentInfo) -> String {
    let reason = match &info.dry_reason {
        Some(reason) => format!(" ({reason})"),
        None => String::new(),
    };
    format!(
        "◌ {agent} dry until {}{reason} — shprd readmit {agent}",
        clock::until(info.dry_until_ms)
    )
}

/// Whether `info` is dry at `now_ms`; a dry agent with no reset stays dry.
fn is_dry_now(info: &AgentInfo, now_ms: u64) -> bool {
    info.dry && info.dry_until_ms.is_none_or(|until| until > now_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture;
    use shprd_core::state::RunInfo;

    /// The single line mentioning `needle`.
    fn line_of(text: &str, needle: &str) -> String {
        match text.lines().find(|line| line.contains(needle)) {
            Some(line) => line.to_string(),
            None => panic!("no line holds {needle:?}:\n{text}"),
        }
    }

    /// A run with a name and no tasks yet.
    fn empty_run(run_id: &str) -> RunState {
        RunState {
            run: Some(RunInfo {
                run_id: run_id.to_string(),
                repo: "repo".to_string(),
                base_branch: "main".to_string(),
                integration_branch: format!("integration/{run_id}"),
            }),
            ..RunState::default()
        }
    }

    #[test]
    fn a_directory_with_no_run_says_how_to_start_one() {
        assert_eq!(
            status(&RunState::default(), fixture::NOW),
            "no run here · start one with shprd up"
        );
    }

    #[test]
    fn a_run_with_no_tasks_points_at_the_orchestrator() {
        assert_eq!(
            status(&empty_run("r1"), fixture::NOW),
            "run r1 · no tasks yet · talk to the ★ orchestrator tab"
        );
    }

    #[test]
    fn each_status_gets_its_own_symbol() {
        let text = status(&fixture::state(), fixture::NOW);
        assert_eq!(line_of(&text, "M1.1"), "✓ M1.1 landed event log");
        assert_eq!(line_of(&text, "M1.2"), "✗ M1.2 failed views");
        assert_eq!(line_of(&text, "M1.3"), "○ M1.3 queued roster");
    }

    #[test]
    fn a_passed_task_says_passed_until_its_work_lands() {
        let events = fixture::events();
        let passed = match RunState::fold(&events[..7]) {
            Ok(state) => state,
            Err(err) => panic!("the fixture prefix must fold: {err}"),
        };
        assert_eq!(
            line_of(&status(&passed, fixture::NOW), "M1.1"),
            "✓ M1.1 passed event log"
        );
    }

    #[test]
    fn working_and_reported_tasks_get_their_own_symbols() {
        let events = fixture::events();
        let working = match RunState::fold(&events[..9]) {
            Ok(state) => state,
            Err(err) => panic!("the fixture prefix must fold: {err}"),
        };
        assert_eq!(
            line_of(&status(&working, fixture::NOW), "M1.2"),
            "● M1.2 working views"
        );
        let reported = match RunState::fold(&events[..10]) {
            Ok(state) => state,
            Err(err) => panic!("the fixture prefix must fold: {err}"),
        };
        assert_eq!(
            line_of(&status(&reported, fixture::NOW), "M1.2"),
            "◇ M1.2 reported views"
        );
    }

    #[test]
    fn a_dropped_task_has_its_own_symbol_and_word() {
        let mut state = fixture::state();
        match state.tasks.get_mut("M1.3") {
            Some(task) => {
                task.status = TaskStatus::Dropped;
                task.dropped_reason = Some("not needed".to_string());
            }
            None => panic!("the fixture has M1.3"),
        }
        assert_eq!(
            line_of(&status(&state, fixture::NOW), "M1.3"),
            "⊘ M1.3 dropped roster"
        );
    }

    #[test]
    fn a_dry_agent_says_how_to_re_admit_it() {
        let state = fixture::state();
        assert_eq!(
            line_of(&status(&state, fixture::NOW), "codex"),
            "◌ codex dry until 22:23 (rate limited) — shprd readmit codex"
        );
    }

    #[test]
    fn a_dry_agent_with_no_reset_time_is_told_apart() {
        let mut state = fixture::state();
        let info = state.agents.entry("pi".to_string()).or_default();
        info.dry = true;
        info.dry_until_ms = None;
        info.dry_reason = Some("agent process exited".to_string());
        assert_eq!(
            line_of(&status(&state, fixture::NOW), "pi"),
            "◌ pi dry until unknown (agent process exited) — shprd readmit pi"
        );
    }

    #[test]
    fn dry_agents_are_listed_only_while_they_are_dry() {
        let state = fixture::state();
        let now = status(&state, fixture::NOW);
        assert!(
            now.contains("◌ codex dry until 22:23"),
            "a dry agent is listed: {now}"
        );
        let later = status(&state, fixture::DRY_UNTIL);
        assert!(
            !later.contains('◌'),
            "a cooled-down agent drops out: {later}"
        );
    }
}
