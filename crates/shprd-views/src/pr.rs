// The per-task pull-request body is adapted from shepherdr (MIT),
// mods/tasks/bin/herdr_task/github.py (pr_body), commit a3f327a.
// Copyright (c) 2026 Elias Stravik
// https://github.com/eliasstravik/shepherdr
//
//! The pull-request body: the log, rendered as Markdown.
//!
//! Two shapes. [`pr_body`] is the run's own pull request, the one the
//! integration branch used to land from. [`task_pr_body`] is one task's pull
//! request, which is how work lands from M8.3 on: its own branch, its base, its
//! hand-in and the gates it passed.
//!
//! Facts only. Every row comes from an event, so a reviewer can check the body
//! against `run.jsonl` line by line; nothing here summarises, praises or
//! guesses.

use shprd_core::event::{Event, EventKind, Finding, Severity};
use shprd_core::state::{OverrideRecord, RunState, Task, TaskStatus};

/// The Markdown body describing `state`, as recorded in `events`.
///
/// Sections: `## Tasks` (one row per task with agent, model, integrated
/// commits, reported tests and skills used), `## Dropped` (every task a human
/// dropped, with the reason they gave), `## Gates` (every attempt's gates,
/// each with its pass or fail and its findings), `## Overrides` (every override
/// event) and
/// `## Escalations` (the count, then the bodies). A section with nothing to
/// report says `none`.
pub fn pr_body(state: &RunState, events: &[Event]) -> String {
    pr_body_flagged(state, events, &[])
}

/// [`pr_body`] with the integration branch's undeclared protected paths: one
/// line, `Touches protected paths: …`, under `## Protected paths`, so the person
/// gating the pull request sees them. Nothing is flagged when `protected` is
/// empty, and the section then says `none`.
pub fn pr_body_flagged(state: &RunState, events: &[Event], protected: &[String]) -> String {
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
    out.push("## Protected paths".to_string());
    out.push(String::new());
    out.push(shprd_core::gate::protected_line(protected).unwrap_or_else(|| "none".to_string()));

    out.push(String::new());
    out.push("## Dropped".to_string());
    out.push(String::new());
    let dropped = dropped_lines(&tasks);
    if dropped.is_empty() {
        out.push("none".to_string());
    } else {
        out.extend(dropped);
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
    out.push("## Full gate".to_string());
    out.push(String::new());
    let full_gates = full_gate_lines(events);
    if full_gates.is_empty() {
        out.push("none".to_string());
    } else {
        out.extend(full_gates);
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

/// The line a task's pull-request body ends with when its review was
/// independent.
///
/// It is the same marker the run-level body uses, so a reader of either pull
/// request reads one claim about the review.
pub const REVIEW_PASSED: &str = "cross-vendor-review: passed";

/// The attribution line every generated pull request ends with.
pub const ATTRIBUTION: &str = "🤖 Generated with [Claude Code](https://claude.com/claude-code)";

/// The Markdown body of one task's own pull request, as it is pushed: the
/// task's facts, the review line, the attribution.
///
/// The review marker is a claim the log already proves: every landing verb asks
/// [`shprd_core::rules::ready_to_land`] before it builds a body, and that rule
/// refuses a task whose review was not independent and engine-recorded.
///
/// It lives here rather than in the binary so the service's tick and the CLI's
/// `land` verb compose one body, byte for byte: a body a reviewer read through
/// one path is the body the other refreshes.
pub fn task_pr_body_marked(task: &Task) -> String {
    task_pr_body_flagged(task, &[])
}

/// [`task_pr_body_marked`] with the protected paths the task's diff touches
/// that it did not declare: one `Touches protected paths: …` line under the
/// summary, so the person accepting the pull request sees it.
pub fn task_pr_body_flagged(task: &Task, protected: &[String]) -> String {
    let mut body = task_pr_body(task);
    if let Some(line) = shprd_core::gate::protected_line(protected) {
        body = match body.split_once("\n## Task\n") {
            Some((head, tail)) => format!("{head}\n{line}\n\n## Task\n{tail}"),
            None => format!("{body}\n\n{line}"),
        };
    }
    if !body.ends_with('\n') {
        body.push('\n');
    }
    body.push('\n');
    body.push_str(REVIEW_PASSED);
    body.push('\n');
    body.push('\n');
    body.push_str(ATTRIBUTION);
    body.push('\n');
    body
}

/// The Markdown body of one task's own pull request.
///
/// Adapted from Shepherdr's per-task body (MIT): the task's ref, what its
/// hand-in said, and the facts a reviewer checks it against. Shepherdr's
/// `## Details` and `## Proof` have no shprd equivalent yet, so the sections
/// are the task's acceptance and scope, the tests it reported, and its gates.
/// The brief is left out on purpose, as Shepherdr leaves it out: it stays with
/// the task.
///
/// Every line is a fact the folded task holds, so the body needs no log of its
/// own: what a review reads here is what the task's attempts recorded.
pub fn task_pr_body(task: &Task) -> String {
    let mut out: Vec<String> = Vec::new();
    out.push(format!("Task: {}", task.id));
    out.push(String::new());

    out.push("## Summary".to_string());
    out.push(String::new());
    let attempt = task.latest_attempt();
    let report = attempt.and_then(|attempt| attempt.report.as_ref());
    match report {
        Some(report) if !report.summary.trim().is_empty() => {
            out.push(cell(report.summary.trim()));
        }
        _ => out.push("none".to_string()),
    }

    out.push(String::new());
    out.push("## Task".to_string());
    out.push(String::new());
    out.push("| id | title | agent | model | branch | commit |".to_string());
    out.push("| --- | --- | --- | --- | --- | --- |".to_string());
    out.push(task_detail_row(task));

    out.push(String::new());
    out.push("## Gates".to_string());
    out.push(String::new());
    let gates = gate_lines(task);
    if gates.is_empty() {
        out.push("none".to_string());
    } else {
        out.extend(gates);
    }

    out.push(String::new());
    out.push("## Tests".to_string());
    out.push(String::new());
    push_bullets(
        &mut out,
        report.map(|report| &report.tests[..]).unwrap_or(&[]),
    );

    out.push(String::new());
    out.push("## Acceptance".to_string());
    out.push(String::new());
    push_bullets(&mut out, &task.acceptance);

    out.push(String::new());
    out.push("## Scope".to_string());
    out.push(String::new());
    push_bullets(&mut out, &task.scope);

    out.join("\n")
}

/// One task row for the per-task body: what it is, who ran it, where its work is.
fn task_detail_row(task: &Task) -> String {
    let attempt = task.latest_attempt();
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
        attempt
            .map(|attempt| attempt.branch.clone())
            .unwrap_or_default(),
        report
            .and_then(|report| report.commit.clone())
            .unwrap_or_default(),
    ];
    let rendered: Vec<String> = cells.iter().map(|text| blank_as_dash(text)).collect();
    format!("| {} |", rendered.join(" | "))
}

/// One bullet per entry, or `none` when there are none.
fn push_bullets(out: &mut Vec<String>, entries: &[String]) {
    if entries.is_empty() {
        out.push("none".to_string());
        return;
    }
    for entry in entries {
        out.push(format!("- {}", cell(entry)));
    }
}

/// One line per dropped task: which task, and why it is not in the work.
///
/// The reason is the whole point of the section. A reader comparing the plan
/// with the pull request sees what is missing, and this says who decided that
/// and why, instead of leaving them to guess from an absent row.
fn dropped_lines(tasks: &[&Task]) -> Vec<String> {
    tasks
        .iter()
        .filter(|task| task.status == TaskStatus::Dropped)
        .map(|task| {
            format!(
                "- {}: {}",
                task.id,
                cell(task.dropped_reason.as_deref().unwrap_or("dropped"))
            )
        })
        .collect()
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

/// Every gate run on every attempt of `task`, each with its findings.
///
/// Attempts are listed in the order they ran, so a retry shows both the gate
/// that failed and the one that passed: `- <task> a<n> <gate>: pass|fail`,
/// with each finding indented beneath it.
fn gate_lines(task: &Task) -> Vec<String> {
    let mut lines = Vec::new();
    for attempt in &task.attempts {
        for gate in &attempt.gates {
            let verdict = if gate.passed { "pass" } else { "fail" };
            lines.push(format!(
                "- {} a{} `{}`: {verdict}",
                task.id, attempt.n, gate.gate
            ));
            for finding in &gate.findings {
                lines.push(format!(
                    "  - {}: {}",
                    severity_word(finding.severity),
                    cell(finding_text(finding))
                ));
            }
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

/// Every run-wide gate in the log: its head, its verdict, and each step.
///
/// A gate is one event, so this needs no task and no state: the log is the
/// whole fact. Steps are listed in the order they ran, which is the order that
/// stopped at the first failure.
fn full_gate_lines(events: &[Event]) -> Vec<String> {
    let mut lines = Vec::new();
    for event in events {
        let EventKind::FullGate {
            head,
            passed,
            steps,
        } = &event.kind
        else {
            continue;
        };
        let verdict = if *passed { "passed" } else { "failed" };
        lines.push(format!("- head `{}`: {verdict}", cell(head)));
        for step in steps {
            let mark = if step.exit_code == Some(0) {
                "✓"
            } else {
                "✗"
            };
            let seconds = step.duration_ms / 1000;
            lines.push(format!(
                "  - {mark} `{}` ({seconds}s, exit {})",
                cell(&step.command),
                match step.exit_code {
                    Some(code) => code.to_string(),
                    None => "none".to_string(),
                }
            ));
        }
    }
    lines
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
    use shprd_core::event::{Actor, GateStep, ReportStatus, ReviewSource};
    use shprd_core::state::RunState;

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
    fn a_task_body_is_the_task_and_its_facts() {
        let state = fixture::state();
        let task = match state.tasks.get("M1.1") {
            Some(task) => task,
            None => panic!("the fixture has M1.1"),
        };
        let body = task_pr_body(task);
        assert!(body.starts_with("Task: M1.1"), "{body}");
        assert!(
            body.contains("## Summary\n\nwrote the log"),
            "the hand-in's summary is the summary: {body}"
        );
        assert!(
            body.contains("| M1.1 | event log | claude | opus | feature/m1-events | abc123 |"),
            "{body}"
        );
        assert!(body.contains("- M1.1 a1 `review`: pass"), "{body}");
        assert!(
            body.contains("## Tests\n\n- cargo test -p shprd-core: ok"),
            "{body}"
        );
        assert!(body.contains("## Acceptance\n\n- M1.1 is done"), "{body}");
        assert!(
            body.contains("## Scope\n\n- crates/shprd-core"),
            "the scope a reviewer checks the diff against: {body}"
        );
        assert!(
            !body.contains("## Dropped") && !body.contains("## Escalations"),
            "one task's pull request says nothing about the run: {body}"
        );
    }

    #[test]
    fn a_task_nobody_ran_says_none_where_it_has_no_facts() {
        let state = fixture::state();
        let task = match state.tasks.get("M1.3") {
            Some(task) => task,
            None => panic!("the fixture has M1.3"),
        };
        let body = task_pr_body(task);
        assert!(body.contains("## Summary\n\nnone"), "{body}");
        assert!(body.contains("## Gates\n\nnone"), "{body}");
        assert!(body.contains("## Tests\n\nnone"), "{body}");
        assert!(
            body.contains("| M1.3 | roster | - | - | - | - |"),
            "a task with no attempt is all dashes: {body}"
        );
    }

    #[test]
    fn gates_show_their_verdict_and_findings() {
        let body = fixture_body();
        assert!(body.contains("## Gates"), "{body}");
        assert!(body.contains("- M1.1 a1 `review`: pass"), "{body}");
        assert!(body.contains("- M1.2 a1 `clippy`: fail"), "{body}");
        assert!(body.contains("  - major: unwrap used"), "{body}");
    }

    /// One event, stamped `at_ms` from its sequence number.
    fn ev(seq: u64, kind: EventKind) -> Event {
        Event {
            seq,
            at_ms: seq * 1_000,
            actor: Actor::Engine,
            kind,
        }
    }

    /// `T1` reporting attempt `attempt`.
    fn reported(seq: u64, attempt: u32) -> Event {
        ev(
            seq,
            EventKind::Reported {
                task: "T1".to_string(),
                attempt,
                status: ReportStatus::Completed,
                summary: "did it".to_string(),
                tests: vec![],
                checks: Vec::new(),
                changed_paths: vec!["crates/shprd-views/src/pr.rs".to_string()],
                commit: None,
                skills_used: vec![],
                proofs: Vec::new(),
            },
        )
    }

    /// A log where `T1`'s first attempt failed a gate and its retry passed.
    fn retried_events() -> Vec<Event> {
        vec![
            ev(
                1,
                EventKind::RunStarted {
                    run_id: "run-9".to_string(),
                    repo: "/tmp/repo".to_string(),
                    base_branch: "main".to_string(),
                    integration_branch: "integration/m1".to_string(),
                },
            ),
            ev(
                2,
                EventKind::TaskAdded {
                    task: "T1".to_string(),
                    title: "retries".to_string(),
                    scope: vec!["crates/shprd-views".to_string()],
                    acceptance: vec!["T1 is done".to_string()],
                    tags: vec![],
                    skills: vec![],
                    deps: vec![],
                },
            ),
            ev(
                3,
                EventKind::Spawned {
                    task: "T1".to_string(),
                    attempt: 1,
                    agent: "codex".to_string(),
                    model: None,
                    worktree: "/tmp/wt".to_string(),
                    branch: "feature/t1".to_string(),
                },
            ),
            reported(4, 1),
            ev(
                5,
                EventKind::GateFailed {
                    task: "T1".to_string(),
                    attempt: 1,
                    gate: "review".to_string(),
                    findings: vec![Finding {
                        severity: Severity::Major,
                        title: "unwrap used".to_string(),
                        location: None,
                        detail: "replace it".to_string(),
                    }],
                    // No `ReviewRequested` precedes these: the fixture is a log
                    // from before a verdict had to name the request it answers.
                    source: Some(ReviewSource {
                        reviewer: "codex/m1".to_string(),
                        findings_sha256: "ba7816bf8f01cfea".to_string(),
                        request_seq: 0,
                    }),
                },
            ),
            ev(
                6,
                EventKind::Spawned {
                    task: "T1".to_string(),
                    attempt: 2,
                    agent: "codex".to_string(),
                    model: None,
                    worktree: "/tmp/wt".to_string(),
                    branch: "feature/t1".to_string(),
                },
            ),
            reported(7, 2),
            ev(
                8,
                EventKind::GatePassed {
                    task: "T1".to_string(),
                    attempt: 2,
                    gate: "review".to_string(),
                    findings: vec![],
                    // No `ReviewRequested` precedes these: the fixture is a log
                    // from before a verdict had to name the request it answers.
                    source: Some(ReviewSource {
                        reviewer: "codex/m1".to_string(),
                        findings_sha256: "ba7816bf8f01cfea".to_string(),
                        request_seq: 0,
                    }),
                },
            ),
        ]
    }

    #[test]
    fn gates_of_every_attempt_are_listed() {
        let events = retried_events();
        let state = match RunState::fold(&events) {
            Ok(state) => state,
            Err(err) => panic!("the retry log must fold: {err}"),
        };
        let body = pr_body(&state, &events);
        assert!(body.contains("- T1 a1 `review`: fail"), "{body}");
        assert!(body.contains("  - major: unwrap used"), "{body}");
        assert!(body.contains("- T1 a2 `review`: pass"), "{body}");
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
    fn a_dropped_task_is_listed_with_the_reason_a_human_gave() {
        let mut state = fixture::many_tasks(1);
        match state.tasks.get_mut("T00") {
            Some(task) => {
                task.status = TaskStatus::Dropped;
                task.dropped_reason = Some("the human changed their mind".to_string());
            }
            None => panic!("the fixture has T00"),
        }
        let body = pr_body(&state, &[]);
        assert!(
            body.contains("## Dropped\n\n- T00: the human changed their mind"),
            "{body}"
        );
        assert!(
            pr_body(&fixture::many_tasks(1), &[]).contains("## Dropped\n\nnone"),
            "a run that dropped nothing says none"
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
    fn a_full_gate_lists_every_step_with_its_verdict() {
        let events = vec![ev(
            1,
            EventKind::FullGate {
                head: "abc1234".to_string(),
                passed: false,
                steps: vec![
                    GateStep {
                        command: "cargo fmt --all --check".to_string(),
                        exit_code: Some(0),
                        duration_ms: 1_500,
                        output_tail: String::new(),
                    },
                    GateStep {
                        command: "cargo test --workspace".to_string(),
                        exit_code: Some(101),
                        duration_ms: 60_000,
                        output_tail: "FAILED".to_string(),
                    },
                ],
            },
        )];
        let body = pr_body(&RunState::default(), &events);
        assert!(body.contains("## Full gate"), "{body}");
        assert!(body.contains("- head `abc1234`: failed"), "{body}");
        assert!(
            body.contains("  - ✓ `cargo fmt --all --check` (1s, exit 0)"),
            "{body}"
        );
        assert!(
            body.contains("  - ✗ `cargo test --workspace` (60s, exit 101)"),
            "{body}"
        );
        assert!(
            pr_body(&RunState::default(), &[]).contains("## Full gate\n\nnone"),
            "a run with no full gate says none"
        );
    }

    #[test]
    fn pull_request_events_add_nothing_to_the_body() {
        let events = vec![
            ev(
                1,
                EventKind::PrOpened {
                    number: 7,
                    url: "https://example.invalid/pr/7".to_string(),
                    head: "shprd/r1/int".to_string(),
                },
            ),
            ev(
                2,
                EventKind::PrMerged {
                    number: 7,
                    head: "abc1234".to_string(),
                },
            ),
        ];
        let body = pr_body(&RunState::default(), &events);
        assert_eq!(
            body,
            pr_body(&RunState::default(), &[]),
            "the forge's facts are not the reader's facts: the log is"
        );
        assert!(!body.contains("example.invalid"), "{body}");
        assert!(!body.contains("abc1234"), "{body}");
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

    #[test]
    fn a_flagged_task_body_and_run_body_name_the_protected_paths() {
        let state = RunState::default();
        let body = pr_body_flagged(&state, &[], &["AGENTS.md".to_string()]);
        assert!(
            body.contains("## Protected paths\n\nTouches protected paths: AGENTS.md\n"),
            "{body}"
        );
        assert!(pr_body(&state, &[]).contains("## Protected paths\n\nnone\n"));
        let state = fixture::state();
        let task = match state.tasks.get("M1.1") {
            Some(task) => task,
            None => panic!("the fixture has M1.1"),
        };
        let flagged = task_pr_body_flagged(task, &[".agents/x.yaml".to_string()]);
        assert!(
            flagged.contains("Touches protected paths: .agents/x.yaml\n\n## Task\n"),
            "{flagged}"
        );
        assert!(!task_pr_body_marked(task).contains("Touches protected paths"));
    }
}
