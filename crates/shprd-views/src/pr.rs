//! The pull-request body: the log, rendered as Markdown.
//!
//! Facts only. Every row comes from an event, so a reviewer can check the body
//! against `run.jsonl` line by line; nothing here summarises, praises or
//! guesses.

use shprd_core::event::{Event, EventKind, Finding, Severity};
use shprd_core::state::{OverrideRecord, RunState, Task};

/// The Markdown body describing `state`, as recorded in `events`.
///
/// Sections: `## Tasks` (one row per task with agent, model, integrated
/// commits, reported tests and skills used), `## Gates` (each gate with its
/// pass or fail and its findings), `## Overrides` (every override event) and
/// `## Escalations` (the count, then the bodies). A section with nothing to
/// report says `none`.
pub fn pr_body(state: &RunState, events: &[Event]) -> String {
    let mut out: Vec<String> = Vec::new();
    out.push("## Tasks".to_string());
    out.push(String::new());
    let tasks: Vec<&Task> = state.tasks.values().collect();
    if tasks.is_empty() {
        out.push("none".to_string());
    } else {
        out.push("| id | title | agent | model | commits | tests | skills |".to_string());
        out.push("| --- | --- | --- | --- | --- | --- | --- |".to_string());
        for task in &tasks {
            out.push(task_row(task, events));
        }
    }

    out.push(String::new());
    out.push("## Gates".to_string());
    out.push(String::new());
    let mut gates = Vec::new();
    for task in &tasks {
        gates.extend(gate_lines(task));
    }
    if gates.is_empty() {
        out.push("none".to_string());
    } else {
        out.extend(gates);
    }

    out.push(String::new());
    out.push("## Overrides".to_string());
    out.push(String::new());
    let overrides = override_lines(state);
    if overrides.is_empty() {
        out.push("none".to_string());
    } else {
        out.extend(overrides);
    }

    out.push(String::new());
    out.push("## Escalations".to_string());
    out.push(String::new());
    out.push(format!("count: {}", state.escalations.len()));
    if state.escalations.is_empty() {
        out.push("none".to_string());
    } else {
        for escalation in &state.escalations {
            out.push(format!("- {}", cell(&escalation.body)));
        }
    }
    out.join("\n")
}

/// One table row: the task, who ran it, what landed, what it ran, what it read.
fn task_row(task: &Task, events: &[Event]) -> String {
    let attempt = task.latest_attempt();
    let commits = integrated_commits(task.id.as_str(), events);
    let report = attempt.and_then(|attempt| attempt.report.as_ref());
    let cells = [
        task.id.clone(),
        task.title.clone(),
        attempt
            .map(|attempt| attempt.agent.clone())
            .unwrap_or_default(),
        attempt
            .and_then(|attempt| attempt.model.clone())
            .unwrap_or_default(),
        commits,
        report
            .map(|report| report.tests.join("; "))
            .unwrap_or_default(),
        report
            .map(|report| report.skills_used.join(", "))
            .unwrap_or_default(),
    ];
    let rendered: Vec<String> = cells.iter().map(|text| blank_as_dash(text)).collect();
    format!("| {} |", rendered.join(" | "))
}

/// The commits the `Integrated` event for `id` recorded, joined for a cell.
fn integrated_commits(id: &str, events: &[Event]) -> String {
    let mut commits: Vec<&str> = Vec::new();
    for event in events {
        if let EventKind::Integrated {
            task,
            commits: landed,
        } = &event.kind
            && task == id
        {
            commits.extend(landed.iter().map(String::as_str));
        }
    }
    commits.join(", ")
}

/// The gates run on a task's latest attempt, each with its findings.
fn gate_lines(task: &Task) -> Vec<String> {
    let mut lines = Vec::new();
    let Some(attempt) = task.latest_attempt() else {
        return lines;
    };
    for gate in &attempt.gates {
        let verdict = if gate.passed { "pass" } else { "fail" };
        lines.push(format!("- {} `{}`: {verdict}", task.id, gate.gate));
        for finding in &gate.findings {
            lines.push(format!(
                "  - {}: {}",
                severity_word(finding.severity),
                cell(finding_text(finding))
            ));
        }
    }
    lines
}

/// Every override the state holds, as `task rule reason` (run-wide ones say
/// so), oldest first.
///
/// Task overrides are read off each task and run-wide ones off the run, then
/// merged by event sequence so the body lists them in the order they happened.
fn override_lines(state: &RunState) -> Vec<String> {
    let mut records: Vec<(u64, &str, &OverrideRecord)> = Vec::new();
    for task in state.tasks.values() {
        for record in &task.overrides {
            records.push((record.seq, task.id.as_str(), record));
        }
    }
    for record in &state.run_overrides {
        records.push((record.seq, "run-wide", record));
    }
    records.sort_by_key(|(seq, _, _)| *seq);
    records
        .into_iter()
        .map(|(_, task, record)| format!("- {task} `{}`: {}", record.rule, cell(&record.reason)))
        .collect()
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

/// A Markdown table cell: pipes and newlines would break the row.
fn cell(text: &str) -> String {
    text.replace('|', "\\|").replace('\n', " ")
}

/// An empty cell reads as `-`, so a reader can tell it apart from a blank.
fn blank_as_dash(text: &str) -> String {
    if text.trim().is_empty() {
        "-".to_string()
    } else {
        cell(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture;

    /// The body for the fixture log.
    fn fixture_body() -> String {
        pr_body(&fixture::state(), &fixture::events())
    }

    #[test]
    fn task_table_lists_the_facts_of_each_task() {
        let body = fixture_body();
        assert!(
            body.contains(
                "| M1.1 | event log | claude | opus | abc123, def456 | \
                 cargo test -p shprd-core: ok | tdd |"
            ),
            "{body}"
        );
        assert!(
            body.contains(
                "| M1.2 | views | codex | gpt | - | cargo test -p shprd-views: ok | tdd, simplify |"
            ),
            "{body}"
        );
        assert!(
            body.contains("| M1.3 | roster | - | - | - | - | - |"),
            "a task nobody ran is all dashes: {body}"
        );
    }

    #[test]
    fn gates_show_their_verdict_and_findings() {
        let body = fixture_body();
        assert!(body.contains("## Gates"), "{body}");
        assert!(body.contains("- M1.1 `lint`: pass"), "{body}");
        assert!(body.contains("- M1.2 `clippy`: fail"), "{body}");
        assert!(body.contains("  - major: unwrap used"), "{body}");
    }

    #[test]
    fn every_override_in_the_log_is_listed() {
        let body = fixture_body();
        assert!(
            body.contains("- M1.2 `integrate_after_failed_gate`: the finding is cosmetic"),
            "{body}"
        );
        assert!(
            body.contains("- run-wide `no_main_push`: release branch"),
            "{body}"
        );
    }

    #[test]
    fn overrides_and_escalations_say_none_when_there_are_none() {
        let state = fixture::many_tasks(1);
        let body = pr_body(&state, &[]);
        assert!(body.contains("## Overrides\n\nnone"), "{body}");
        assert!(body.contains("count: 0"), "{body}");
    }

    #[test]
    fn escalations_show_the_count_and_every_body() {
        let body = fixture_body();
        assert!(body.contains("## Escalations"), "{body}");
        assert!(body.contains("count: 2"), "{body}");
        assert!(
            body.contains("- two gates disagree on the panic policy"),
            "{body}"
        );
    }
}
