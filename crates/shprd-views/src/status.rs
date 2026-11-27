//! The status line: where every task sits, in one glance.
//!
//! `shprd status` prints this; nothing here asks a model anything. The word a
//! row shows is the derived status (M8.5's rules, [`crate::derived`]), so a
//! task an open ask is waiting on reads **Needs you** here exactly as it does
//! in the service's board; the stored lifecycle state is printed beside it in
//! this view's JSON, where a script reads `status`, `derived.status`, `need`
//! and `reason` as separate fields.

use serde_json::{Value, json};
use shprd_core::state::{AgentInfo, RunState, TaskStatus};

use crate::clock;
use crate::derived::{self, Derived, TaskSignal};

/// One line per task, then one line per agent that is dry at `now_ms`.
///
/// A directory with no run says `no run here · start one with shprd run start`,
/// and a run whose plan is still empty says `run <id> · no tasks yet · add one
/// with shprd agent task add`: an empty screen would otherwise look like a
/// broken run.
///
/// Each task line is `<glyph> <task> <derived word> · <title>`, with the
/// derived row's reason between the word and the title when it has one. The
/// word is the one a person acts on: `○ To do`, `◷ Queued` (waiting on a task
/// that has not landed), `◐ Working` (with what it waits on when the fold
/// knows), `● Needs you` (with why), `✓ Ready`, `↻ Merging`, `✔ Done`. A task
/// that has landed also says `(landed)`, since Done alone would not say whether
/// its work is in. Tasks are listed in id order. A dry agent is
/// `◌ <agent> dry until HH:MM (reason) — shprd readmit <agent>` in UTC; an
/// agent dry with no known reset time shows `unknown`.
pub fn status(state: &RunState, now_ms: u64) -> String {
    let mut lines: Vec<String> = Vec::new();
    match &state.run {
        None => lines.push("no run here · start one with shprd run start".to_string()),
        Some(run) if state.tasks.is_empty() => lines.push(format!(
            "run {} · no tasks yet · add one with shprd agent task add",
            run.run_id
        )),
        Some(_) => {
            for task in state.tasks.values() {
                let Some(reading) = reading(state, &task.id, now_ms) else {
                    continue;
                };
                lines.push(line(&task.id, &task.title, task.status, &reading));
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

/// The same board as one JSON document, for a script or a card.
///
/// The stored lifecycle state stays its own field (`status`): the derived word
/// and what it needs are [`status`]'s projection of the log, and a client that
/// wants the fold's state does not have to reverse it. An agent dry at `now_ms`
/// is listed for the same reason the human line names it.
pub fn json(state: &RunState, now_ms: u64) -> Value {
    let run = match &state.run {
        Some(info) => json!({
            "run_id": info.run_id,
            "repo": info.repo,
            "base_branch": info.base_branch,
            "integration_branch": info.integration_branch,
            "finished": state.finished,
        }),
        None => Value::Null,
    };
    let tasks: Vec<Value> = state
        .tasks
        .values()
        .map(|task| {
            let derived = reading(state, &task.id, now_ms).unwrap_or_default();
            json!({
                "id": task.id,
                "title": task.title,
                "status": stored_name(task.status),
                "derived": {
                    "status": derived.status,
                    "word": derived.status.word(),
                    "need": derived.needs,
                    "reason": derived.reason,
                },
            })
        })
        .collect();
    let dry: Vec<Value> = state
        .agents
        .iter()
        .filter(|(_, info)| is_dry_now(info, now_ms))
        .map(|(agent, info)| {
            json!({
                "agent": agent,
                "until_ms": info.dry_until_ms,
                "reason": info.dry_reason,
            })
        })
        .collect();
    json!({
        "run": run,
        "tasks": tasks,
        "dry_agents": dry,
    })
}

/// What one task reads as for a caller that has only the log, or `None` when
/// the run has no such task.
///
/// It is [`derived::derive_from_fold`] with the fold's facts filled in: the
/// CLI and the service's snapshot both come through here, so the two never
/// disagree about the word a person sees.
pub fn reading(state: &RunState, id: &str, now_ms: u64) -> Option<Derived> {
    let signal = TaskSignal::from_state(state, id)?;
    Some(derived::derive_from_fold(&signal, now_ms))
}

/// One task's line: its derived word, why, and its title.
fn line(id: &str, title: &str, stored: TaskStatus, reading: &Derived) -> String {
    let mut line = format!(
        "{} {} {}",
        reading.status.glyph(),
        id,
        reading.status.word()
    );
    if stored == TaskStatus::Integrated {
        // Done alone cannot say whether the work is in or the task was dropped.
        line.push_str(" (landed)");
    }
    if let Some(reason) = &reading.reason {
        line.push_str(" · ");
        line.push_str(reason);
    }
    line.push_str(" · ");
    line.push_str(title);
    line
}

/// The stored lifecycle state's name, as `state --json` and the API spell it.
fn stored_name(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Queued => "queued",
        TaskStatus::Working => "working",
        TaskStatus::Reported => "reported",
        TaskStatus::Passed => "passed",
        TaskStatus::Failed => "failed",
        TaskStatus::Integrated => "integrated",
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
    use crate::derived::DerivedStatus;
    use crate::fixture;
    use shprd_core::ask::AskKind;
    use shprd_core::state::{AskRecord, RunInfo};

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

    /// The fixture with one open ask on `task`.
    fn asked(task: &str) -> RunState {
        let mut state = fixture::state();
        state.asks.insert(
            (task.to_string(), "ask-1".to_string()),
            AskRecord {
                id: "ask-1".to_string(),
                task: task.to_string(),
                kind: AskKind::Question,
                text: "Should the sum be variadic?".to_string(),
                options: Vec::new(),
                created_at_ms: fixture::NOW,
                expires_at_ms: None,
                raised_seq: 20,
                answer: None,
            },
        );
        state
    }

    #[test]
    fn a_directory_with_no_run_says_how_to_start_one() {
        assert_eq!(
            status(&RunState::default(), fixture::NOW),
            "no run here · start one with shprd run start"
        );
    }

    #[test]
    fn a_run_with_no_tasks_says_how_to_add_one() {
        assert_eq!(
            status(&empty_run("r1"), fixture::NOW),
            "run r1 · no tasks yet · add one with shprd agent task add"
        );
    }

    #[test]
    fn each_status_gets_its_own_symbol() {
        let text = status(&fixture::state(), fixture::NOW);
        assert_eq!(line_of(&text, "M1.1"), "✔ M1.1 Done (landed) · event log");
        assert_eq!(line_of(&text, "M1.2"), "✓ M1.2 Ready · views");
        assert_eq!(line_of(&text, "M1.3"), "○ M1.3 To do · roster");
    }

    #[test]
    fn a_passed_task_reads_ready_until_its_work_lands() {
        let events = fixture::events();
        let passed = match RunState::fold(&events[..7]) {
            Ok(state) => state,
            Err(err) => panic!("the fixture prefix must fold: {err}"),
        };
        assert_eq!(
            line_of(&status(&passed, fixture::NOW), "M1.1"),
            "✓ M1.1 Ready · event log"
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
            "◐ M1.2 Working · views"
        );
        let reported = match RunState::fold(&events[..10]) {
            Ok(state) => state,
            Err(err) => panic!("the fixture prefix must fold: {err}"),
        };
        assert_eq!(
            line_of(&status(&reported, fixture::NOW), "M1.2"),
            "◐ M1.2 Working · Running its gates · views"
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
            "✔ M1.3 Done · Dropped · not needed · roster"
        );
    }

    #[test]
    fn a_task_whose_agent_was_stopped_reads_agent_stopped() {
        // M10 gate bug 1: the person's own Stop closed the space and left the
        // task Working for ever. Now the attempt carries the fact, so the CLI's
        // reader — which has no terminals — says what Shepherdr says for a task
        // with no agent left (M7a §5, `agents.py:170`).
        let mut state = fixture::state();
        match state.tasks.get_mut("M1.2") {
            Some(task) => {
                task.status = TaskStatus::Failed;
                if let Some(attempt) = task.attempts.last_mut() {
                    attempt.stopped = true;
                }
            }
            None => panic!("the fixture has M1.2"),
        }
        assert_eq!(
            line_of(&status(&state, fixture::NOW), "M1.2"),
            "● M1.2 Needs you · Agent stopped · views"
        );
    }

    #[test]
    fn an_open_ask_is_visible_as_needs_you() {
        // The bug the M9 gate found: a card an agent is waiting on changed
        // nothing a person reading `shprd status` could see.
        let text = status(&asked("M1.3"), fixture::NOW);
        assert_eq!(
            line_of(&text, "M1.3"),
            "● M1.3 Needs you · Should the sum be variadic? · roster"
        );
    }

    #[test]
    fn the_json_keeps_the_stored_state_and_adds_the_derived_one() {
        let doc = json(&asked("M1.3"), fixture::NOW);
        let tasks = match doc.get("tasks").and_then(Value::as_array) {
            Some(tasks) => tasks,
            None => panic!("no tasks array in {doc}"),
        };
        let task = match tasks.iter().find(|task| task["id"] == "M1.3") {
            Some(task) => task,
            None => panic!("M1.3 missing from {doc}"),
        };
        assert_eq!(task["status"], json!("queued"), "the fold's own state");
        assert_eq!(task["derived"]["status"], json!("needs_you"));
        assert_eq!(task["derived"]["word"], json!("Needs you"));
        assert_eq!(task["derived"]["need"], json!("asks"));
        assert_eq!(
            task["derived"]["reason"],
            json!("Should the sum be variadic?")
        );

        // A task with nothing waiting on it keeps the stored state and reads
        // the derived one from the fold alone.
        let plain = json(&fixture::state(), fixture::NOW);
        let tasks = match plain.get("tasks").and_then(Value::as_array) {
            Some(tasks) => tasks,
            None => panic!("no tasks array in {plain}"),
        };
        let integrated = match tasks.iter().find(|task| task["id"] == "M1.1") {
            Some(task) => task,
            None => panic!("M1.1 missing from {plain}"),
        };
        assert_eq!(integrated["status"], json!("integrated"));
        assert_eq!(integrated["derived"]["status"], json!("done"));
        assert_eq!(integrated["derived"]["need"], Value::Null);
    }

    #[test]
    fn the_json_of_a_directory_with_no_run_is_empty_but_whole() {
        let doc = json(&RunState::default(), fixture::NOW);
        assert_eq!(doc["run"], Value::Null);
        assert_eq!(doc["tasks"], json!([]));
        assert_eq!(doc["dry_agents"], json!([]));
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

    #[test]
    fn the_fixture_reads_as_its_people_would() {
        let state = fixture::state();
        let read = |id: &str| match reading(&state, id, fixture::NOW) {
            Some(reading) => reading,
            None => panic!("the fixture has {id}"),
        };
        assert_eq!(read("M1.1").status, DerivedStatus::Done);
        assert_eq!(read("M1.2").status, DerivedStatus::Ready);
        assert_eq!(read("M1.3"), Derived::default());
        assert_eq!(reading(&state, "M9.9", fixture::NOW), None);
    }
}
