//! The brief: the block an agent is handed after losing its context.
//!
//! A brief is a pure function of the run state and the event log, and never
//! longer than [`BRIEF_MAX_LINES`] lines, so it fits in a prompt next to the
//! task it describes. No model writes any of it.

use shprd_core::event::{Event, Finding, Severity};
use shprd_core::state::{OverrideRecord, RunState, Task, TaskStatus};

use crate::clock;

/// Hard cap on the number of lines a brief may occupy.
pub const BRIEF_MAX_LINES: usize = 40;

/// Maximum characters of a task title shown in the task table.
const TITLE_CHARS: usize = 40;

/// Maximum characters of a finding or escalation shown in the brief.
const TEXT_CHARS: usize = 80;

/// Largest number of escalations shown, most recent last.
const ESCALATIONS_SHOWN: usize = 3;

/// The rehydration block for `state`.
///
/// The event log is accepted for symmetry with the other projectors, but every
/// fact in a brief now lives in the folded state, including run-wide overrides,
/// so the log is not consulted. Pass the log anyway: callers have it, and the
/// argument keeps the projector signatures interchangeable.
///
/// It carries a header line with the run and its branches, then the sections
/// that have content: the task table, the tasks waiting at a gate, the failed
/// gates, the dry agents, the most recent escalations and every override. Over
/// [`BRIEF_MAX_LINES`] lines, lists are cut and marked `… and N more`, so the
/// same state always renders the same brief and it always fits.
pub fn brief(state: &RunState, _events: &[Event]) -> String {
    let Some(run) = &state.run else {
        return "no run".to_string();
    };
    let header = format!(
        "run {} · base {} · integration {}",
        run.run_id, run.base_branch, run.integration_branch
    );
    let mut sections: Vec<Section> = vec![
        Section::new("tasks:", task_lines(state)),
        Section::new("waiting on gates:", waiting_lines(state)),
        Section::new("failed gates:", failed_lines(state)),
        Section::new("dry agents:", dry_lines(state)),
        Section::new("escalations:", escalation_lines(state)),
        Section::new("overrides:", override_lines(state)),
    ];
    sections.retain(|section| !section.lines.is_empty());
    assemble(&header, &sections).join("\n")
}

/// A titled block of brief lines.
struct Section {
    /// The line printed above the block.
    title: &'static str,
    /// The block's lines, each already indented.
    lines: Vec<String>,
}

impl Section {
    /// A section holding `lines`.
    fn new(title: &'static str, lines: Vec<String>) -> Self {
        Self { title, lines }
    }
}

/// Renders `sections` under `header`, trimming the longest lists first until
/// the whole block fits in [`BRIEF_MAX_LINES`] lines.
fn assemble(header: &str, sections: &[Section]) -> Vec<String> {
    let mut keep: Vec<usize> = sections.iter().map(|section| section.lines.len()).collect();
    while total_lines(header, sections, &keep) > BRIEF_MAX_LINES {
        let Some(index) = widest_shrinkable(&keep) else {
            break;
        };
        keep[index] -= 1;
    }
    let mut lines = vec![header.to_string()];
    for (section, &kept) in sections.iter().zip(&keep) {
        lines.push(section.title.to_string());
        lines.extend(section.lines.iter().take(kept).cloned());
        if kept < section.lines.len() {
            lines.push(format!("  … and {} more", section.lines.len() - kept));
        }
    }
    lines.truncate(BRIEF_MAX_LINES);
    lines
}

/// How many lines `sections` would occupy if each kept `keep[i]` entries.
fn total_lines(header: &str, sections: &[Section], keep: &[usize]) -> usize {
    let mut total = usize::from(!header.is_empty());
    for (section, &kept) in sections.iter().zip(keep) {
        total += 1 + kept + usize::from(kept < section.lines.len());
    }
    total
}

/// The index of the list that should lose a line, or `None` when no list can
/// shrink without disappearing: a present section always keeps one line.
fn widest_shrinkable(keep: &[usize]) -> Option<usize> {
    keep.iter()
        .enumerate()
        .filter(|(_, kept)| **kept > 1)
        .max_by_key(|(_, kept)| **kept)
        .map(|(index, _)| index)
}

/// The task table, one line per task in id order.
fn task_lines(state: &RunState) -> Vec<String> {
    state
        .tasks
        .values()
        .map(|task| {
            format!(
                "  {} {} {} a{} {}",
                task.id,
                label(task.status),
                agent_of(task),
                attempt_of(task),
                clock::clip(&task.title, TITLE_CHARS)
            )
        })
        .collect()
}

/// The tasks that reported and are waiting on a gate.
fn waiting_lines(state: &RunState) -> Vec<String> {
    state
        .tasks
        .values()
        .filter(|task| task.status == TaskStatus::Reported)
        .map(|task| format!("  {} {}", task.id, clock::clip(&task.title, TITLE_CHARS)))
        .collect()
}

/// The last failed gate of each task that has one, with its first finding.
fn failed_lines(state: &RunState) -> Vec<String> {
    let mut lines = Vec::new();
    for task in state.tasks.values() {
        let Some(attempt) = task.latest_attempt() else {
            continue;
        };
        let Some(gate) = attempt.gates.iter().rev().find(|gate| !gate.passed) else {
            continue;
        };
        let finding = match gate.findings.first() {
            Some(finding) => format!(
                " {}: {}",
                severity_word(finding.severity),
                clock::clip(finding_text(finding), TEXT_CHARS)
            ),
            None => " no finding".to_string(),
        };
        lines.push(format!("  {} {}{finding}", task.id, gate.gate));
    }
    lines
}

/// The agents on cooldown, with the time they come back and why.
fn dry_lines(state: &RunState) -> Vec<String> {
    state
        .agents
        .iter()
        .filter(|(_, info)| info.dry)
        .map(|(agent, info)| {
            format!(
                "  {agent} until {} {}",
                clock::until(info.dry_until_ms),
                info.dry_reason.as_deref().unwrap_or("-")
            )
        })
        .collect()
}

/// The escalation count, then the bodies of the last few.
fn escalation_lines(state: &RunState) -> Vec<String> {
    let escalations = &state.escalations;
    if escalations.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![format!("  {} open", escalations.len())];
    let first = escalations.len().saturating_sub(ESCALATIONS_SHOWN);
    for escalation in &escalations[first..] {
        lines.push(format!("  - {}", clock::clip(&escalation.body, TEXT_CHARS)));
    }
    lines
}

/// Every override the state holds, oldest first, as recorded by a human.
///
/// Task overrides come from each task; run-wide ones have no task and print as
/// `-`. The two lists are merged by event sequence so the order is the order
/// they happened in, not the order tasks happen to be named.
fn override_lines(state: &RunState) -> Vec<String> {
    let mut records: Vec<(u64, &str, &OverrideRecord)> = Vec::new();
    for task in state.tasks.values() {
        for record in &task.overrides {
            records.push((record.seq, task.id.as_str(), record));
        }
    }
    for record in &state.run_overrides {
        records.push((record.seq, "-", record));
    }
    records.sort_by_key(|(seq, _, _)| *seq);
    records
        .into_iter()
        .map(|(_, task, record)| format!("  {task} {}: {}", record.rule, record.reason))
        .collect()
}

/// The word printed for a task status in the task table.
fn label(status: TaskStatus) -> &'static str {
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

/// The agent on the latest attempt, or `-` while nobody has run the task.
fn agent_of(task: &Task) -> &str {
    match task.latest_attempt() {
        Some(attempt) => attempt.agent.as_str(),
        None => "-",
    }
}

/// The number of the latest attempt, or 0 while nobody has run the task.
fn attempt_of(task: &Task) -> u32 {
    task.latest_attempt().map(|attempt| attempt.n).unwrap_or(0)
}

/// The word printed for a finding's severity.
fn severity_word(severity: Severity) -> &'static str {
    match severity {
        Severity::Critical => "critical",
        Severity::Major => "major",
        Severity::Minor => "minor",
        Severity::Trivial => "trivial",
    }
}

/// A finding's title, falling back to its detail when the title is empty.
fn finding_text(finding: &Finding) -> &str {
    if finding.title.trim().is_empty() {
        &finding.detail
    } else {
        &finding.title
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture;

    /// The brief for the fixture log.
    fn fixture_brief() -> String {
        brief(&fixture::state(), &fixture::events())
    }

    #[test]
    fn brief_without_a_run_says_so() {
        assert_eq!(brief(&RunState::default(), &[]), "no run");
    }

    #[test]
    fn header_carries_the_run_and_its_branches() {
        let text = fixture_brief();
        assert_eq!(
            text.lines().next(),
            Some("run run-7 · base main · integration integration/m1")
        );
    }

    #[test]
    fn task_table_has_one_line_per_task() {
        let text = fixture_brief();
        assert!(
            text.contains("  M1.1 integrated claude a1 event log"),
            "{text}"
        );
        assert!(text.contains("  M1.2 failed codex a1 views"), "{text}");
        assert!(text.contains("  M1.3 queued - a0 roster"), "{text}");
    }

    #[test]
    fn sections_with_nothing_to_say_are_omitted() {
        let text = fixture_brief();
        assert!(!text.contains("waiting on gates:"), "{text}");
        assert!(text.contains("failed gates:"), "{text}");
        assert!(
            !brief(&fixture::many_tasks(2), &[]).contains("failed gates:"),
            "a run with no failed gate must not print the section"
        );
    }

    #[test]
    fn reported_tasks_are_listed_as_waiting_on_a_gate() {
        let events = fixture::events();
        let reported = match RunState::fold(&events[..10]) {
            Ok(state) => state,
            Err(err) => panic!("the fixture prefix must fold: {err}"),
        };
        let text = brief(&reported, &events);
        assert!(text.contains("waiting on gates:\n  M1.2 views"), "{text}");
        assert!(!text.contains("failed gates:"), "{text}");
    }

    #[test]
    fn failed_gates_show_the_first_finding() {
        let text = fixture_brief();
        assert!(text.contains("  M1.2 clippy major: unwrap used"), "{text}");
    }

    #[test]
    fn dry_agents_show_their_reset_time_and_reason() {
        let text = fixture_brief();
        assert!(text.contains("  codex until 22:23 rate limited"), "{text}");
    }

    #[test]
    fn escalations_show_the_count_and_the_newest_bodies() {
        let text = fixture_brief();
        assert!(text.contains("escalations:\n  2 open"), "{text}");
        assert!(
            text.contains("  - the clippy finding is cosmetic, integrate anyway?"),
            "{text}"
        );
    }

    #[test]
    fn overrides_come_from_the_log() {
        let text = fixture_brief();
        assert!(
            text.contains("  M1.2 integrate_after_failed_gate: the finding is cosmetic"),
            "{text}"
        );
        assert!(
            text.contains("  - no_main_push: release branch"),
            "a run-wide override has no home in state, so it comes from the log: {text}"
        );
    }

    #[test]
    fn brief_never_exceeds_the_line_cap() {
        let state = fixture::many_tasks(60);
        let text = brief(&state, &[]);
        let lines = text.lines().count();
        assert!(lines <= BRIEF_MAX_LINES, "{lines} lines:\n{text}");
        assert!(
            text.contains("  … and "),
            "the task list must be cut: {text}"
        );
        assert!(
            text.lines().count() <= 40,
            "the advertised cap must hold: {lines}"
        );
    }
}
