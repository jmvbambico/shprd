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
use shprd_core::settings::{Key, Resolved, ResolvedRole, Source, Sourced, TaskKey, Value};
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

/// How the program a task's agent runs can raise a needs-you card.
///
/// It is read from the program's harness — `[asks] supported` in
/// `data/harness/<program>.toml` — and never from a list of program names, so a
/// program that gains or loses an ask hook changes its brief by changing its
/// harness file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskRoute {
    /// The harness names an ask hook: the agent asks with the program's own
    /// question tool, and the hook turns it into a card.
    Hook,
    /// The harness names no ask hook: the agent runs `shprd ask raise` itself.
    Command,
}

impl AskRoute {
    /// The route a program's harness says it takes.
    pub fn of_harness(supported: bool) -> Self {
        if supported { Self::Hook } else { Self::Command }
    }
}

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
        Section::new("asks:", ask_lines(state)),
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

/// Every ask a worker raised, oldest first, with what the person answered.
///
/// This is what the log says happened, so a coordinator reports a worker's question
/// from here and never from a guess: an ask the person answered reads `answered`,
/// one nobody has answered reads `open`.
fn ask_lines(state: &RunState) -> Vec<String> {
    let mut asks: Vec<_> = state.asks.values().collect();
    asks.sort_by_key(|ask| ask.raised_seq);
    asks.iter()
        .map(|ask| {
            let said = clock::clip(&ask.text, TEXT_CHARS);
            match &ask.answer {
                Some(answer) => format!(
                    "  {}: asked \"{said}\", the person answered \"{}\"",
                    ask.task,
                    clock::clip(&answer.text, TEXT_CHARS)
                ),
                None => format!("  {}: asked \"{said}\", not answered yet", ask.task),
            }
        })
        .collect()
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
    /// The repository's worker gate commands, from the resolved `[gates]`
    /// config. Empty when this repository configures none.
    pub gates: &'a [String],
    /// The archive ref the previous attempt's commits are held under, when this
    /// attempt replaced one.
    pub archive: Option<&'a str>,
    /// The role's standing instructions, when the role has a prompt.
    pub prompt: Option<&'a str>,
    /// How this program raises a needs-you card, from its harness.
    pub ask: AskRoute,
    /// Whether the run asks every task for one proof artifact, from the
    /// resolved `tasks.proof` setting.
    pub proof: bool,
    /// The settings as this task resolves them, for the Settings block.
    pub settings: Option<&'a Resolved>,
    /// The role whose values the Settings block describes.
    pub role: Option<&'a ResolvedRole>,
    /// What the launch already knows the agent should be told: an effort level
    /// its model cannot use, for one. Empty for most briefs.
    pub warnings: &'a [String],
    /// Which of the worker's program lines this task got and why, as one
    /// sentence, for the Settings block.
    pub program_line: Option<&'a str>,
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
    // The person's words for how a task starts come before the task itself, so
    // the agent reads them first; the words for how it ends come with the rules
    // it reports under.
    let notes = input.settings.map(|resolved| resolved.tasks());
    if let Some(start) = notes
        .as_ref()
        .map(|tasks| tasks.start_notes.trim())
        .filter(|text| !text.is_empty())
    {
        out.push_str("## Before you start\n\n");
        out.push_str(start);
        out.push_str("\n\n");
    }
    out.push_str(&task_sections(input));
    if let Some(finish) = notes
        .as_ref()
        .map(|tasks| tasks.finish_notes.trim())
        .filter(|text| !text.is_empty())
    {
        out.push_str("\n## Before you report\n\n");
        out.push_str("Do this before you run `shprd agent report`:\n\n");
        out.push_str(finish);
        out.push('\n');
    }
    if let Some(resolved) = input.settings {
        out.push('\n');
        out.push_str(&settings_block(resolved, input.role, input.program_line));
    }
    if !input.warnings.is_empty() {
        out.push_str("\n## Warnings\n\n");
        for warning in input.warnings {
            out.push_str(&format!("- {warning}\n"));
        }
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
            "the attempt before this one was archived at `{ref_name}`. If it committed \
             anything, your branch starts from its last commit, so that work is already in \
             your history (`git log` shows it): carry on from it rather than redoing it. \
             If your branch does not hold it, `git cherry-pick {ref_name}` brings it over\n\n"
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
    out.push_str(
        "- `.shprd/` and `.dagr/` are shprd's own: the run's log, asks and graph live there, and \
         the worktree's git already ignores them. Never commit them, never add them to the \
         repository's `.gitignore`, and leave them out of your changes\n",
    );
    out.push_str(&gate_rule(input.gates));
    out.push_str(&format!(
        "- say where you are as you go: `shprd task status {t} --msg <one short line> [--progress 0-100]`\n",
        t = input.task.id
    ));
    out.push_str(&proof_rule(input.proof));
    out.push_str(&format!(
        "- report when you are done: `shprd agent report --task {t} --attempt {a} --status <completed|blocked|failed> --summary <one line>` \
         plus `--check <gate>|<command>|<exit code>[|<last 20 lines>]` for every gate you ran (a completed hand-in is refused \
         without them, and refused if any exited non-zero), `--test <cmd>` for each command you ran, `--changed <path>` for each \
         path you changed, `--commit <sha>` for your commit, `--skill-used <name>` for each skill above you actually used \
         and `--proof <path>` for each artifact you left behind\n",
        t = input.task.id,
        a = input.attempt
    ));
    out.push_str(&asking_section(input.ask));
    out
}

/// The "Asking the person" section: how this program raises a needs-you card.
///
/// A program whose harness has an ask hook raises the card with its own
/// question tool, and the hook does the rest. A program whose harness has none
/// cannot: its question tool reaches nobody, so the brief names the verb the
/// worker runs itself, with its flags and what it prints, so a blocked worker
/// never ends its turn on a question no person will see.
fn asking_section(route: AskRoute) -> String {
    match route {
        AskRoute::Hook => "\n## Asking the person\n\n\
            This program's CLI has an ask hook: ask with your CLI's own question tool (Claude Code: \
            `AskUserQuestion`, with 2-4 clear options and your recommendation first) and the hook \
            turns it into a needs-you card the person answers, from the board or their phone. The \
            tool waits until they do, so your next turn carries their answer.\n"
            .to_string(),
        AskRoute::Command => {
            let mut out = String::from(
                "\n## Asking the person\n\n\
                 This program's CLI has no ask hook, so a question with its own question tool reaches \
                 nobody: nothing turns it into a card and the person never sees it. Raise the card \
                 yourself, from your worktree, and wait for the answer:\n\n",
            );
            out.push_str("```\n");
            out.push_str("shprd ask raise --kind question \\\n");
            out.push_str("    --text \"Can I take the window for 30s to screenshot the menu?\" \\\n");
            out.push_str(
                "    --option \"Take it, 30s\" --option \"Wait for my go-ahead\" --option \"Skip the screenshot\"\n",
            );
            out.push_str("```\n\n");
            out.push_str(
                "It blocks until the person answers, prints their answer on stdout and exits 0: carry \
                 on with that answer. `--kind permission` asks to run a tool and `--kind plan` offers a \
                 plan; a permission offers Allow and Deny and a plan Approve and Reject, unless you name your own options. If it exits non-zero, \
                 nobody answered within its wait (30 minutes by default; `--timeout-secs` changes it): the \
                 card stays open, so raise it again if the decision still blocks you, or hand in as \
                 `blocked` with the question in your summary.\n",
            );
            out
        }
    }
}

/// The Rules line that tells the agent which gates to run.
///
/// The commands come from the run's repository (`[gates] worker` in its config),
/// so a Python repository is told its tests and a Rust one its cargo gates. A
/// repository that configures none says so instead of borrowing another
/// project's commands, and the agent is asked to find the repository's own test
/// command and name what it ran.
fn gate_rule(gates: &[String]) -> String {
    if gates.is_empty() {
        return "- this repository configures no worker gates: run the repository's own test command \
                if you can find one, and say which command you ran\n"
            .to_string();
    }
    let commands: Vec<String> = gates
        .iter()
        .filter(|gate| !gate.trim().is_empty())
        .map(|gate| format!("`{gate}`"))
        .collect();
    if commands.is_empty() {
        return gate_rule(&[]);
    }
    format!(
        "- run the worker gates before you report: {}\n",
        commands.join(", ")
    )
}

/// The Rules line that tells the agent where its proof goes, when it needs one.
///
/// It sits before the hand-in rule, not after it, because a hand-in is spent
/// once: a worker that reads "say where it is in your summary" only after
/// reporting has already written a summary it cannot change, and the re-report
/// is refused. The proof path goes in `--proof`, where it costs the summary
/// nothing, so the first report is right.
fn proof_rule(proof: bool) -> String {
    if !proof {
        return String::new();
    }
    "- leave one real proof of the work behind (a screenshot of the change, a command's actual \
     output, a test run) under `.shprd/proof/` inside your worktree (git ignores it; never commit proof into the \
     repository), and pass its path as `--proof <path>`, once per artifact: the artifact must be \
     in the worktree, because that is the folder the reviewer is given, and a path outside it, or \
     a tracked file, is refused. The proof path never goes in the summary, which is \
     one plain line for the person. A mock is no proof of what people see; a bug fix shows the same \
     repro failing before and passing after\n"
        .to_string()
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
pub fn settings_block(
    resolved: &Resolved,
    role: Option<&ResolvedRole>,
    program_line: Option<&str>,
) -> String {
    let mut out = String::from("## Your settings\n\n");
    out.push_str(
        "Each line ends with the layer that set it: `this task`, `this repo`, `you`, \
         `the repo's file` or `built in`. Where this brief says otherwise, the brief wins.\n\n",
    );
    // The person's own name for the role, where it reads naturally: the agent is
    // told what the person will call it. The role's key never changes.
    if let Some(role) = role.filter(|role| role.title.source != Source::BuiltIn) {
        out.push_str(&format!(
            "You are the {} (the `{}` role) on this task.\n\n",
            role.display_title(),
            role.id
        ));
    }
    if let Some(note) = program_line {
        out.push_str(&format!("- Program line: {note}\n"));
    }
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
    // A task that got a program line says which and what comes next, which is
    // the whole of the old fallback list's job.
    if program_line.is_none() {
        out.push_str(&format!("- Fallback: {fallback}\n"));
    }
    for (name, key, absent) in [
        ("Merge", TaskKey::Merge, "ask"),
        ("Review rounds", TaskKey::ReviewRounds, "0 (off)"),
        ("Must fix", TaskKey::MustFix, "major"),
        ("Proof", TaskKey::Proof, "none"),
        ("Worktrees", TaskKey::Worktrees, "on"),
        ("Base branch", TaskKey::Base, "the repo's own branch"),
        ("Branch pattern", TaskKey::Branch, "{run}/{task}"),
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

    /// The worker gates a repository's config sets, read the way a spawn reads
    /// them: a `config.toml` under `[gates] worker`.
    fn gates_of(text: &str) -> Vec<String> {
        let dir = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(err) => panic!("a temp dir: {err}"),
        };
        let path = dir.path().join("config.toml");
        if let Err(err) = std::fs::write(&path, text) {
            panic!("the fixture config should be written: {err}");
        }
        match shprd_core::config::load(Some(&path), None) {
            Ok(config) => config.gates.worker,
            Err(err) => panic!("the fixture config should load: {err}"),
        }
    }

    /// The Rules block of a brief for `gates`.
    fn rules_with(gates: &[String]) -> String {
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
            gates,
            archive: None,
            prompt: None,
            ask: AskRoute::Hook,
            proof: true,
            settings: None,
            role: None,
            warnings: &[],
            program_line: None,
        });
        match text.split_once("## Rules\n\n") {
            Some((_, rules)) => rules.to_string(),
            None => panic!("the brief has a Rules section: {text}"),
        }
    }

    #[test]
    fn a_python_repositories_brief_carries_its_own_gates() {
        let gates =
            gates_of("[gates]\nworker = [\"python3 -m pytest -q\", \"python3 -m ruff check .\"]\n");
        let rules = rules_with(&gates);
        assert!(
            rules.contains(
                "- run the worker gates before you report: `python3 -m pytest -q`, `python3 -m ruff check .`\n"
            ),
            "{rules}"
        );
        assert!(
            !rules.contains("cargo"),
            "shprd's own gates must not leak: {rules}"
        );
    }

    #[test]
    fn a_repository_with_no_gates_says_so_and_asks_for_its_own_test_command() {
        let rules = rules_with(&gates_of(""));
        assert!(
            rules.contains(
                "- this repository configures no worker gates: run the repository's own test command if you can find one, and say which command you ran\n"
            ),
            "{rules}"
        );
        assert!(
            !rules.contains("cargo") && !rules.contains("pytest"),
            "a repository with no gates must not be given anyone else's: {rules}"
        );
    }

    /// The worker gates this very repository's config sets. The file is
    /// shprd's own, so the test fails if its three cargo gates leave the config
    /// and go back into the code.
    #[test]
    fn shprds_own_gates_come_from_its_config() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.shprd/config.toml");
        let config = match shprd_core::config::load(Some(&path), None) {
            Ok(config) => config,
            Err(err) => panic!("{} should load: {err}", path.display()),
        };
        let gates = config.gates.worker;
        let rules = rules_with(&gates);
        assert!(
            rules.contains(
                "- run the worker gates before you report: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`\n"
            ),
            "{rules}"
        );
    }

    #[test]
    fn settings_block_lists_every_value_with_the_layer_that_set_it() {
        let (resolved, role) = settings_fixture();
        let block = settings_block(&resolved, Some(&role), None);
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
- Branch pattern: {run}/{task} (built in)
- Tasks at once: 4 (built in)
- Start notes: none (built in)
- Finish notes: none (built in)
";
        assert_eq!(block, expected);
    }

    #[test]
    fn a_settings_block_without_a_role_says_the_program_decides() {
        let (resolved, _) = settings_fixture();
        let block = settings_block(&resolved, None, None);
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
        let gates = vec![
            "cargo fmt --all --check".to_string(),
            "cargo test --workspace".to_string(),
        ];
        let text = task_brief(&BriefInput {
            task: &plan,
            run: "m9",
            attempt: 1,
            agent: "claude",
            model: Some("opus"),
            branch: "task/30-m9-4",
            worktree: "/home/me/.shprd/worktrees/shprd/m9/M9.4",
            skills: &skills,
            gates: &gates,
            archive: None,
            prompt: Some("You work on one shprd task.\nStay within scope.\n"),
            ask: AskRoute::Hook,
            proof: true,
            settings: Some(&resolved),
            role: Some(&role),
            warnings: &[],
            program_line: None,
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
- `.shprd/` and `.dagr/` are shprd's own: the run's log, asks and graph live there, and the worktree's git already ignores them. Never commit them, never add them to the repository's `.gitignore`, and leave them out of your changes
- run the worker gates before you report: `cargo fmt --all --check`, `cargo test --workspace`
- say where you are as you go: `shprd task status M9.4 --msg <one short line> [--progress 0-100]`
- leave one real proof of the work behind (a screenshot of the change, a command's actual output, a test run) under `.shprd/proof/` inside your worktree (git ignores it; never commit proof into the repository), and pass its path as `--proof <path>`, once per artifact: the artifact must be in the worktree, because that is the folder the reviewer is given, and a path outside it, or a tracked file, is refused. The proof path never goes in the summary, which is one plain line for the person. A mock is no proof of what people see; a bug fix shows the same repro failing before and passing after
- report when you are done: `shprd agent report --task M9.4 --attempt 1 --status <completed|blocked|failed> --summary <one line>` plus `--check <gate>|<command>|<exit code>[|<last 20 lines>]` for every gate you ran (a completed hand-in is refused without them, and refused if any exited non-zero), `--test <cmd>` for each command you ran, `--changed <path>` for each path you changed, `--commit <sha>` for your commit, `--skill-used <name>` for each skill above you actually used and `--proof <path>` for each artifact you left behind

## Asking the person

This program's CLI has an ask hook: ask with your CLI's own question tool (Claude Code: `AskUserQuestion`, with 2-4 clear options and your recommendation first) and the hook turns it into a needs-you card the person answers, from the board or their phone. The tool waits until they do, so your next turn carries their answer.

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
- Branch pattern: {run}/{task} (built in)
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
            gates: &[],
            archive: None,
            prompt: None,
            ask: AskRoute::Hook,
            proof: true,
            settings: None,
            role: None,
            warnings: &[],
            program_line: None,
        });
        assert!(
            text.starts_with("# M9.4 role templates and a brief\n"),
            "{text}"
        );
        assert!(!text.contains("## Your settings"), "{text}");
    }

    #[test]
    fn a_warning_from_the_launch_is_part_of_the_brief() {
        let plan = brief_task();
        let warnings = vec![
            "`pi` lists `m` as not thinking, so the effort `low` has no effect: the flag is dropped"
                .to_string(),
        ];
        let text = task_brief(&BriefInput {
            task: &plan,
            run: "m9",
            attempt: 1,
            agent: "pi",
            model: Some("m"),
            branch: "task/30-m9-4",
            worktree: "/wt/M9.4",
            skills: &[],
            gates: &[],
            archive: None,
            prompt: None,
            ask: AskRoute::Hook,
            proof: true,
            settings: None,
            role: None,
            warnings: &warnings,
            program_line: None,
        });
        assert!(
            text.contains(
                "## Warnings\n\n- `pi` lists `m` as not thinking, so the effort `low` has no effect: the flag is dropped\n"
            ),
            "{text}"
        );
    }

    #[test]
    fn a_brief_without_warnings_has_no_warning_section() {
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
            gates: &[],
            archive: None,
            prompt: None,
            ask: AskRoute::Hook,
            proof: true,
            settings: None,
            role: None,
            warnings: &[],
            program_line: None,
        });
        assert!(!text.contains("## Warnings"), "{text}");
    }

    /// The plainest brief, with `ask` naming how its program asks.
    fn plain_brief(ask: AskRoute) -> String {
        let plan = brief_task();
        task_brief(&BriefInput {
            task: &plan,
            run: "m9",
            attempt: 1,
            agent: "claude",
            model: None,
            branch: "task/30-m9-4",
            worktree: "/wt/M9.4",
            skills: &[],
            gates: &[],
            archive: None,
            prompt: None,
            ask,
            proof: true,
            settings: None,
            role: None,
            warnings: &[],
            program_line: None,
        })
    }

    /// A program whose harness says it has an ask hook keeps the old wording:
    /// its own question tool raises the card through the hook, so the brief
    /// must not send it to a verb the hook already runs for it.
    #[test]
    fn a_program_with_an_ask_hook_is_told_to_use_its_own_question_tool() {
        let text = plain_brief(AskRoute::Hook);
        assert!(
            text.ends_with(
                "\n## Asking the person\n\nThis program's CLI has an ask hook: ask with your CLI's own question tool (Claude Code: `AskUserQuestion`, with 2-4 clear options and your recommendation first) and the hook turns it into a needs-you card the person answers, from the board or their phone. The tool waits until they do, so your next turn carries their answer.\n"
            ),
            "{text}"
        );
        assert!(!text.contains("shprd ask raise"), "{text}");
    }

    /// A program with no ask hook is told the verb it runs itself, exactly:
    /// the command, the flags, what it prints and what a non-zero exit means.
    /// The wording is the whole fix, so it is pinned here.
    #[test]
    fn a_program_without_an_ask_hook_is_told_the_raise_verb_and_how_it_waits() {
        let text = plain_brief(AskRoute::Command);
        let expected = "\n## Asking the person\n\nThis program's CLI has no ask hook, so a question with its own question tool reaches nobody: nothing turns it into a card and the person never sees it. Raise the card yourself, from your worktree, and wait for the answer:\n\n```\nshprd ask raise --kind question \\\n    --text \"Can I take the window for 30s to screenshot the menu?\" \\\n    --option \"Take it, 30s\" --option \"Wait for my go-ahead\" --option \"Skip the screenshot\"\n```\n\nIt blocks until the person answers, prints their answer on stdout and exits 0: carry on with that answer. `--kind permission` asks to run a tool and `--kind plan` offers a plan; a permission offers Allow and Deny and a plan Approve and Reject, unless you name your own options. If it exits non-zero, nobody answered within its wait (30 minutes by default; `--timeout-secs` changes it): the card stays open, so raise it again if the decision still blocks you, or hand in as `blocked` with the question in your summary.\n";
        let Some(start) = text.find("\n## Asking the person") else {
            panic!("the brief has an Asking section: {text}");
        };
        assert_eq!(&text[start..], expected, "{text}");
        assert!(
            !text.contains("ask with your CLI's own question tool"),
            "a program with no hook must not be told to use its own tool: {text}"
        );
    }

    /// shprd's own state in the worktree is never the worker's to commit, and
    /// the ignoring is git's, not a change to the repository's `.gitignore`.
    #[test]
    fn the_rules_say_shprds_state_directories_are_not_the_workers() {
        let text = plain_brief(AskRoute::Hook);
        assert!(
            text.contains(
                "- `.shprd/` and `.dagr/` are shprd's own: the run's log, asks and graph live there, and the worktree's git already ignores them. Never commit them, never add them to the repository's `.gitignore`, and leave them out of your changes\n"
            ),
            "{text}"
        );
    }

    /// The proof rule comes before the hand-in rule, and it says where the proof
    /// path goes. A hand-in is spent once, so a worker that only learns about
    /// `--proof` after reporting has no report left to correct.
    #[test]
    fn the_proof_rule_is_read_before_the_hand_in_rule() {
        let text = plain_brief(AskRoute::Hook);
        let proof = text
            .find("- leave one real proof of the work behind")
            .unwrap_or_else(|| panic!("the brief names the proof rule: {text}"));
        let report = text
            .find("- report when you are done:")
            .unwrap_or_else(|| panic!("the brief names the hand-in rule: {text}"));
        assert!(proof < report, "the proof rule comes first: {text}");
        assert!(
            text.contains("pass its path as `--proof <path>`"),
            "the rule says where the proof path goes: {text}"
        );
        assert!(
            text.contains("never goes in the summary"),
            "the rule keeps the proof path out of the summary: {text}"
        );
        assert!(
            text.contains("`--proof <path>` for each artifact you left behind"),
            "the hand-in rule repeats the flag: {text}"
        );
    }

    /// A run that does not ask for proof says nothing about it, so the rule
    /// never becomes a line a worker skips over.
    #[test]
    fn a_run_without_proof_has_no_proof_rule() {
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
            gates: &[],
            archive: None,
            prompt: None,
            ask: AskRoute::Hook,
            proof: false,
            settings: None,
            role: None,
            warnings: &[],
            program_line: None,
        });
        assert!(!text.contains("leave one real proof"), "{text}");
        assert!(
            text.contains("`--proof <path>` for each artifact you left behind"),
            "the flag is still named by the hand-in rule: {text}"
        );
    }

    /// The route is the harness's fact, not a list of program names.
    #[test]
    fn the_ask_route_comes_from_the_harness() {
        assert_eq!(AskRoute::of_harness(true), AskRoute::Hook);
        assert_eq!(AskRoute::of_harness(false), AskRoute::Command);
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

    /// A role the person named is told its name in the brief; one nobody named is not.
    #[test]
    fn the_settings_block_says_the_name_the_person_gave_the_role() {
        use shprd_core::settings::{File, Key, Request, RoleField, Value, resolve};
        let mut file = File::default();
        file.you.set(
            Key::Role {
                id: "worker".to_owned(),
                field: RoleField::Title,
            },
            Value::Text("Dev".to_owned()),
        );
        let resolved = resolve(&file, &Request::default());
        let Some(role) = resolved.role("worker") else {
            panic!("the worker role resolves");
        };
        let text = settings_block(&resolved, Some(&role), None);
        assert!(
            text.contains("You are the Dev (the `worker` role)"),
            "{text}"
        );
        let (plain, plain_role) = settings_fixture();
        assert!(!settings_block(&plain, Some(&plain_role), None).contains("You are the"));
    }
}
