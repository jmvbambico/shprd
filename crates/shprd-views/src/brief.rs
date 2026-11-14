//! The briefs an agent reads: the one a task's agent starts with, and the
//! rehydration block an agent gets after losing its context.
//!
//! [`task_brief`] is what one attempt is handed before its first turn: the
//! role's standing words, the task itself (title, acceptance, scope, skills and
//! the rules of the attempt) and a Settings block naming every value that shapes
//! the work and the layer that set it.
//!
//! [`brief`] is the same run told briefly: the table of tasks, the gates, the
//! dry agents and the overrides. It is a pure function of the run state and the
//! event log, and never longer than [`BRIEF_MAX_LINES`] lines, so it fits in a
//! prompt next to the task it describes. No model writes any of it.

use shprd_core::event::{Event, Finding, Severity};
use shprd_core::settings::{Key, Resolved, ResolvedRole, Sourced, TaskKey, Value};
use shprd_core::state::{GateResult, OverrideRecord, RunState, Task, TaskStatus};

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

/// What a task's agent is told before its first turn, as data, so the rendering
/// is testable on its own.
///
/// It is the role's standing words, then the task itself — its title, its
/// acceptance criteria, its scope, the skills to load and the rules of the
/// attempt — then the [`settings_block`].
pub struct BriefInput<'a> {
    /// The task the attempt works on.
    pub task: &'a Task,
    /// The run id.
    pub run: &'a str,
    /// The attempt number.
    pub attempt: u32,
    /// The program the attempt runs.
    pub agent: &'a str,
    /// The model it runs, when one was chosen.
    pub model: Option<&'a str>,
    /// The branch the agent commits on.
    pub branch: &'a str,
    /// The worktree the agent works in.
    pub worktree: &'a str,
    /// The skills the attempt must load first.
    pub skills: &'a [String],
    /// The archive ref the previous attempt's commits are held under, when this
    /// attempt replaced one.
    pub archive: Option<&'a str>,
    /// The role's standing instructions, when the role has a prompt.
    pub prompt: Option<&'a str>,
    /// The settings as this task resolves them, for the Settings block.
    pub settings: Option<&'a Resolved>,
    /// The role whose values the Settings block describes.
    pub role: Option<&'a ResolvedRole>,
}

/// The longest detail the brief keeps from a previous attempt's finding, so one
/// verbose finding cannot push the rest of the brief out of view.
const DETAIL_LIMIT: usize = 300;

/// Renders the brief an agent reads before its first turn.
///
/// It is the role's prompt when the role has one, then only what the agent
/// cannot look up faster itself: where it works, what "done" means, what it may
/// touch, which skills to load, how to report, and the Settings block naming
/// every value that shapes the work and the layer that set it.
pub fn task_brief(input: &BriefInput<'_>) -> String {
    let mut out = String::new();
    if let Some(prompt) = input.prompt {
        out.push_str(prompt.trim_end());
        out.push_str("\n\n");
    }
    out.push_str(&task_sections(input));
    if let Some(resolved) = input.settings {
        out.push('\n');
        out.push_str(&settings_block(resolved, input.role));
    }
    out
}

/// The task's own half of the brief: everything but the role's words and the
/// Settings block.
fn task_sections(input: &BriefInput<'_>) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {} {}\n\n", input.task.id, input.task.title));
    out.push_str(&format!(
        "run `{}`, attempt {}, agent `{}`{}\n",
        input.run,
        input.attempt,
        input.agent,
        match input.model {
            Some(model) => format!(" ({model})"),
            None => String::new(),
        }
    ));
    out.push_str(&format!(
        "branch `{}` in worktree `{}`\n\n",
        input.branch, input.worktree
    ));
    if let Some(ref_name) = input.archive {
        out.push_str(&format!(
            "the attempt before this one was archived at `{ref_name}`: \
             `git cherry-pick {ref_name}` brings over whatever of its work you still need\n\n"
        ));
    }
    if let Some(gate) = previous_failure(input.task, input.attempt) {
        out.push_str("## Why the previous attempt was sent back\n\n");
        out.push_str(&format!("gate `{}` failed:\n\n", gate.gate));
        for finding in &gate.findings {
            out.push_str(&finding_line(finding));
        }
        out.push_str("Fix these first; keep what already works.\n");
    }
    out.push_str("## Acceptance criteria\n\n");
    for item in &input.task.acceptance {
        out.push_str(&format!("- {item}\n"));
    }
    out.push_str("\n## Scope — change only these paths\n\n");
    for path in &input.task.scope {
        out.push_str(&format!("- {path}\n"));
    }
    out.push_str("\n## Skills to load first\n\n");
    if input.skills.is_empty() {
        out.push_str("- none\n");
    }
    for skill in input.skills {
        out.push_str(&format!("- {skill}\n"));
    }
    out.push_str("\n## Rules\n\n");
    out.push_str(&format!(
        "- work only in `{}`; change nothing outside the scope above\n",
        input.worktree
    ));
    out.push_str(&format!(
        "- commit on your branch `{}`; never push, never open a pull request\n",
        input.branch
    ));
    out.push_str("- run the worker gates before you report: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`\n");
    out.push_str(&format!(
        "- say where you are as you go: `shprd task status {t} --msg <one short line> [--progress 0-100]`\n",
        t = input.task.id
    ));
    out.push_str(&format!(
        "- report when you are done: `shprd agent report --task {t} --attempt {a} --status <completed|blocked|failed> --summary <one line>` \
         plus `--check <gate>|<command>|<exit code>[|<last 20 lines>]` for every gate you ran (a completed hand-in is refused \
         without them, and refused if any exited non-zero), `--test <cmd>` for each command you ran, `--changed <path>` for each \
         path you changed, `--commit <sha>` for your commit and `--skill-used <name>` for each skill above you actually used\n",
        t = input.task.id,
        a = input.attempt
    ));
    out
}

/// The settings that shape one task, each with the layer that set it.
///
/// One line per value the person can change: the role's program, model, effort,
/// fast mode, access mode and fallback list, then the workflow values a task is
/// worked under (merge, review rounds, the bar for fixing, proof, worktrees, the
/// base and branch, how many run at once, and the two note boxes). The line ends
/// with the layer that answered — [`Source::label`]'s words — because a value a
/// role inherited and a value the person set for this repository must not read
/// alike. A value nothing sets is named as shprd's own behaviour, with `built in`
/// as its layer.
pub fn settings_block(resolved: &Resolved, role: Option<&ResolvedRole>) -> String {
    let mut out = String::from("## Your settings\n\n");
    out.push_str(
        "Each line ends with the layer that set it: `this task`, `this repo`, `you`, \
         `the repo's file` or `built in`. Where this brief says otherwise, the brief wins.\n\n",
    );
    let role_line = |name: &str, field: Option<&Sourced<String>>, absent: &str| match field {
        Some(sourced) if !sourced.value.trim().is_empty() => {
            format!("- {name}: {} ({})\n", sourced.value, sourced.source.label())
        }
        _ => format!("- {name}: {absent} (built in)\n"),
    };
    out.push_str(&role_line(
        "Program",
        role.and_then(|role| role.agent.as_ref()),
        "the program's own default",
    ));
    out.push_str(&role_line(
        "Model",
        role.and_then(|role| role.model.as_ref()),
        "the program's own default",
    ));
    out.push_str(&role_line(
        "Effort",
        role.and_then(|role| role.effort.as_ref()),
        "the program's own default",
    ));
    if let Some(role) = role {
        out.push_str(&format!(
            "- Fast mode: {} ({})\n",
            if role.fast.value { "on" } else { "off" },
            role.fast.source.label()
        ));
    } else {
        out.push_str("- Fast mode: off (built in)\n");
    }
    out.push_str(&role_line(
        "Permissions",
        role.and_then(|role| role.perm.as_ref()),
        "the program's own default",
    ));
    let fallback = role
        .filter(|role| !role.fallback.value.is_empty())
        .map(|role| {
            format!(
                "{} ({})",
                role.fallback.value.join(", "),
                role.fallback.source.label()
            )
        })
        .unwrap_or_else(|| "none: the configured workers (built in)".to_owned());
    out.push_str(&format!("- Fallback: {fallback}\n"));
    for (name, key, absent) in [
        ("Merge", TaskKey::Merge, "ask"),
        ("Review rounds", TaskKey::ReviewRounds, "0 (off)"),
        ("Must fix", TaskKey::MustFix, "major"),
        ("Proof", TaskKey::Proof, "none"),
        ("Worktrees", TaskKey::Worktrees, "on"),
        ("Base branch", TaskKey::Base, "the repo's own branch"),
        ("Branch pattern", TaskKey::Branch, "task/{n}-{slug}"),
        ("Tasks at once", TaskKey::MaxRunning, "10"),
        ("Start notes", TaskKey::StartNotes, "none"),
        ("Finish notes", TaskKey::FinishNotes, "none"),
    ] {
        out.push_str(&task_line(resolved, name, key, absent));
    }
    out
}

/// One workflow line of the Settings block.
fn task_line(resolved: &Resolved, name: &str, key: TaskKey, absent: &str) -> String {
    match resolved.get(&Key::Task(key)) {
        Some(sourced) => {
            let words = match &sourced.value {
                Value::Text(text) if text.trim().is_empty() => absent.to_owned(),
                other => value_words(other),
            };
            format!("- {name}: {words} ({})\n", sourced.source.label())
        }
        None => format!("- {name}: {absent} (built in)\n"),
    }
}

/// A settings value in the words a Settings line uses.
fn value_words(value: &Value) -> String {
    match value {
        Value::Text(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        Value::Switch(on) => if *on { "on" } else { "off" }.to_owned(),
        Value::List(items) => {
            if items.is_empty() {
                "none".to_owned()
            } else {
                items.join(", ")
            }
        }
    }
}

/// The gate that sent the previous attempt back, when its latest gate failed.
///
/// A new attempt starts from the same task, so the brief repeats what the review
/// or the gates rejected instead of making the agent rediscover it.
fn previous_failure(task: &Task, attempt: u32) -> Option<&GateResult> {
    let earlier = task
        .attempts
        .iter()
        .filter(|earlier| earlier.n < attempt)
        .max_by_key(|earlier| earlier.n)?;
    let gate = earlier.gates.last()?;
    (!gate.passed).then_some(gate)
}

/// One finding as the brief lists it: `- <severity>: <title> (<where>) — <why>`.
fn finding_line(finding: &Finding) -> String {
    let detail: String = finding.detail.chars().take(DETAIL_LIMIT).collect();
    let detail = if detail.len() < finding.detail.len() {
        format!("{detail}…")
    } else {
        detail
    };
    let title = if finding.title.trim().is_empty() {
        finding
            .location
            .as_deref()
            .unwrap_or(finding.detail.as_str())
    } else {
        finding.title.as_str()
    };
    let where_ = match &finding.location {
        Some(location) => format!(" ({location})"),
        None => String::new(),
    };
    format!(
        "- {}: {title}{where_} — {detail}\n",
        severity_word(finding.severity)
    )
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

    /// A settings fixture: one value from each layer that can set one, so the
    /// Settings block's sources are all exercised — the task's own program, the
    /// person's review-rounds default, this repository's merge policy and
    /// effort, the repository's committed base branch, and a built-in value.
    fn settings_fixture() -> (Resolved, ResolvedRole) {
        use shprd_core::settings::{File, Key, Layer, Request, RoleField, TaskKey, Value, resolve};

        let mut file = File::default();
        file.you.set(
            Key::Role {
                id: "worker".to_owned(),
                field: RoleField::Model,
            },
            Value::Text("opus".to_owned()),
        );
        file.you
            .set(Key::Task(TaskKey::ReviewRounds), Value::Number(2));
        let mut repo = Layer::new();
        repo.set(
            Key::Role {
                id: "worker".to_owned(),
                field: RoleField::Effort,
            },
            Value::Text("high".to_owned()),
        );
        repo.set(Key::Task(TaskKey::Merge), Value::Text("checks".to_owned()));
        file.repos.insert("shprd".to_owned(), repo);
        let mut task = Layer::new();
        task.set(
            Key::Role {
                id: "worker".to_owned(),
                field: RoleField::Agent,
            },
            Value::Text("claude".to_owned()),
        );
        let mut repo_file = Layer::new();
        repo_file.set(Key::Task(TaskKey::Base), Value::Text("main".to_owned()));

        let resolved = resolve(
            &file,
            &Request {
                repo: Some("shprd"),
                repo_file: Some(&repo_file),
                task: Some(&task),
            },
        );
        let role = match resolved.role("worker") {
            Some(role) => role,
            None => panic!("the fixture resolves the worker role"),
        };
        (resolved, role)
    }

    /// The brief's fixture task.
    fn brief_task() -> Task {
        fixture::queued(
            "M9.4",
            "role templates and a brief",
            &["data/templates", "crates/shprd-views"],
        )
    }

    #[test]
    fn settings_block_lists_every_value_with_the_layer_that_set_it() {
        let (resolved, role) = settings_fixture();
        let block = settings_block(&resolved, Some(&role));
        let expected = "## Your settings

Each line ends with the layer that set it: `this task`, `this repo`, `you`, `the repo's file` or `built in`. Where this brief says otherwise, the brief wins.

- Program: claude (this task)
- Model: opus (you)
- Effort: high (this repo)
- Fast mode: off (built in)
- Permissions: the program's own default (built in)
- Fallback: none: the configured workers (built in)
- Merge: checks (this repo)
- Review rounds: 2 (you)
- Must fix: major (built in)
- Proof: on (built in)
- Worktrees: on (built in)
- Base branch: main (the repo's file)
- Branch pattern: task/{n}-{slug} (built in)
- Tasks at once: 4 (built in)
- Start notes: none (built in)
- Finish notes: none (built in)
";
        assert_eq!(block, expected);
    }

    #[test]
    fn a_settings_block_without_a_role_says_the_program_decides() {
        let (resolved, _) = settings_fixture();
        let block = settings_block(&resolved, None);
        assert!(
            block.contains("- Program: the program's own default (built in)"),
            "{block}"
        );
        assert!(block.contains("- Fast mode: off (built in)"), "{block}");
    }

    #[test]
    fn task_brief_is_the_role_prompt_the_task_and_the_settings_block() {
        let (resolved, role) = settings_fixture();
        let plan = brief_task();
        let skills = vec!["tdd".to_string()];
        let text = task_brief(&BriefInput {
            task: &plan,
            run: "m9",
            attempt: 1,
            agent: "claude",
            model: Some("opus"),
            branch: "task/30-m9-4",
            worktree: "/home/me/.shprd/worktrees/shprd/m9/M9.4",
            skills: &skills,
            archive: None,
            prompt: Some("You work on one shprd task.\nStay within scope.\n"),
            settings: Some(&resolved),
            role: Some(&role),
        });
        let expected = "You work on one shprd task.
Stay within scope.

# M9.4 role templates and a brief

run `m9`, attempt 1, agent `claude` (opus)
branch `task/30-m9-4` in worktree `/home/me/.shprd/worktrees/shprd/m9/M9.4`

## Acceptance criteria

- M9.4 does what it says

## Scope — change only these paths

- data/templates
- crates/shprd-views

## Skills to load first

- tdd

## Rules

- work only in `/home/me/.shprd/worktrees/shprd/m9/M9.4`; change nothing outside the scope above
- commit on your branch `task/30-m9-4`; never push, never open a pull request
- run the worker gates before you report: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`
- say where you are as you go: `shprd task status M9.4 --msg <one short line> [--progress 0-100]`
- report when you are done: `shprd agent report --task M9.4 --attempt 1 --status <completed|blocked|failed> --summary <one line>` plus `--check <gate>|<command>|<exit code>[|<last 20 lines>]` for every gate you ran (a completed hand-in is refused without them, and refused if any exited non-zero), `--test <cmd>` for each command you ran, `--changed <path>` for each path you changed, `--commit <sha>` for your commit and `--skill-used <name>` for each skill above you actually used

## Your settings

Each line ends with the layer that set it: `this task`, `this repo`, `you`, `the repo's file` or `built in`. Where this brief says otherwise, the brief wins.

- Program: claude (this task)
- Model: opus (you)
- Effort: high (this repo)
- Fast mode: off (built in)
- Permissions: the program's own default (built in)
- Fallback: none: the configured workers (built in)
- Merge: checks (this repo)
- Review rounds: 2 (you)
- Must fix: major (built in)
- Proof: on (built in)
- Worktrees: on (built in)
- Base branch: main (the repo's file)
- Branch pattern: task/{n}-{slug} (built in)
- Tasks at once: 4 (built in)
- Start notes: none (built in)
- Finish notes: none (built in)
";
        assert_eq!(text, expected);
    }

    #[test]
    fn a_brief_without_a_prompt_or_settings_is_only_the_task() {
        let plan = brief_task();
        let text = task_brief(&BriefInput {
            task: &plan,
            run: "m9",
            attempt: 1,
            agent: "claude",
            model: None,
            branch: "task/30-m9-4",
            worktree: "/wt/M9.4",
            skills: &[],
            archive: None,
            prompt: None,
            settings: None,
            role: None,
        });
        assert!(
            text.starts_with("# M9.4 role templates and a brief\n"),
            "{text}"
        );
        assert!(!text.contains("## Your settings"), "{text}");
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
