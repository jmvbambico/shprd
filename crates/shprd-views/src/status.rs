//! The status line: where every task sits, in one glance.
//!
//! `shprd status` prints this; nothing here asks a model anything.

use shprd_core::state::{AgentInfo, RunState, TaskStatus};

use crate::clock;

/// One line per task, then one line per agent that is dry at `now_ms`.
///
/// Each task line starts with a symbol: `○` queued, `●` working, `◇` reported
/// and awaiting review, `✓` passed (with ` landed` appended once integrated)
/// or `✗` failed. Tasks are listed in id order. A dry agent is `◌ <agent> dry
/// until HH:MM` in UTC; an agent dry with no known reset time shows `unknown`.
pub fn status(state: &RunState, now_ms: u64) -> String {
    let mut lines: Vec<String> = Vec::new();
    for task in state.tasks.values() {
        lines.push(format!(
            "{} {} {} {}",
            symbol(task.status),
            task.id,
            word(task.status),
            task.title
        ));
    }
    for (agent, info) in &state.agents {
        if is_dry_now(info, now_ms) {
            lines.push(format!(
                "◌ {agent} dry until {}",
                clock::until(info.dry_until_ms)
            ));
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
    }
}

/// The word printed after the task id.
fn word(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Queued => "queued",
        TaskStatus::Working => "working",
        TaskStatus::Reported => "reported",
        TaskStatus::Passed => "passed",
        TaskStatus::Integrated => "passed landed",
        TaskStatus::Failed => "failed",
    }
}

/// Whether `info` is dry at `now_ms`; a dry agent with no reset stays dry.
fn is_dry_now(info: &AgentInfo, now_ms: u64) -> bool {
    info.dry && info.dry_until_ms.is_none_or(|until| until > now_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture;

    /// The single line mentioning `needle`.
    fn line_of(text: &str, needle: &str) -> String {
        match text.lines().find(|line| line.contains(needle)) {
            Some(line) => line.to_string(),
            None => panic!("no line holds {needle:?}:\n{text}"),
        }
    }

    #[test]
    fn each_status_gets_its_own_symbol() {
        let text = status(&fixture::state(), fixture::NOW);
        assert_eq!(line_of(&text, "M1.1"), "✓ M1.1 passed landed event log");
        assert_eq!(line_of(&text, "M1.2"), "✗ M1.2 failed views");
        assert_eq!(line_of(&text, "M1.3"), "○ M1.3 queued roster");
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
