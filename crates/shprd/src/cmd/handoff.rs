//! `shprd agent handoff`: move a task from one agent to the next.
//!
//! A hand-off keeps the attempt's worktree, branch and log entry and changes
//! only who works in them: the outgoing agent's work is checkpointed, a
//! hand-off brief says where to continue, and the tab is replaced. Every
//! decision is made before the first side effect, the way [`spawn`] does it.

use std::path::{Path, PathBuf};

use anyhow::anyhow;
use shprd_core::event::{Event, EventKind};
use shprd_core::log::{Clock, SystemClock};
use shprd_core::state::{Attempt, RunState, Task, TaskStatus};
use shprd_git::git::Git;
use shprd_mux::{Mux, Workspace};
use shprd_roster::models::AgentRef;

use crate::cmd::spawn::{self, BriefInput, Pick, Program, SpawnEnv};
use crate::ctx::CliError;

/// What the caller asked for.
#[derive(Debug, Clone)]
pub struct HandoffRequest {
    /// The task to hand over.
    pub task: String,
    /// Why it is being handed over.
    pub reason: String,
    /// The agent taking it over, as `<program>/<model>`; `None` picks the next
    /// configured worker after the outgoing one.
    pub to: Option<String>,
}

/// The agent handing the task over: the one its latest attempt ran.
fn from_agent(task: &Task) -> Result<String, CliError> {
    if task.status != TaskStatus::Working {
        return Err(CliError::Invalid(anyhow!(
            "task {:?} is {:?}, not Working: only a task an agent is working on can be handed off",
            task.id,
            task.status
        )));
    }
    task.latest_attempt()
        .map(|attempt| attempt.agent.clone())
        .ok_or_else(|| CliError::Invalid(anyhow!("task {:?} has no attempt to hand off", task.id)))
}

/// The agent that takes the task over, or `None` when none can.
///
/// An explicit `--to` wins even when its CLI is not installed, as it does for a
/// spawn: the human may be pointing at a wrapper. It must still parse against
/// the harness table, so a typo is refused before any effect. Without one the
/// roster decides, which can come up empty.
fn pick_to(
    state: &RunState,
    programs: &[Program],
    workers: &[String],
    from: &str,
    explicit: Option<&str>,
) -> Result<Option<Pick>, CliError> {
    let specs = spawn::spec_table(programs);
    let Some(raw) = explicit else {
        return Ok(next_worker(
            state,
            programs,
            workers,
            from,
            SystemClock.now_ms(),
        ));
    };
    let parsed = AgentRef::parse(raw, &specs).map_err(|err| {
        CliError::Invalid(anyhow!(err).context(format!("`{raw}` is not a usable agent")))
    })?;
    let program = spawn::program_of(programs, &parsed.program)?;
    Ok(Some(Pick {
        agent: parsed.program,
        model: parsed.model,
        launch: program.spec.launch.clone(),
    }))
}

/// The next configured worker after `from`, wrapping around the roster once.
///
/// A worker is skipped when it is the one handing over, when its string does
/// not parse, when its CLI is not installed, or when the log has it dry right
/// now: handing work to an agent that cannot take it would only cost a tab.
fn next_worker(
    state: &RunState,
    programs: &[Program],
    workers: &[String],
    from: &str,
    now_ms: u64,
) -> Option<Pick> {
    let specs = spawn::spec_table(programs);
    let start = workers
        .iter()
        .position(|raw| AgentRef::parse(raw, &specs).is_ok_and(|parsed| parsed.program == from))
        .map(|index| index + 1)
        .unwrap_or(0);
    for offset in 0..workers.len() {
        let raw = &workers[(start + offset) % workers.len()];
        let Ok(parsed) = AgentRef::parse(raw, &specs) else {
            eprintln!("warning: skipping configured worker `{raw}`");
            continue;
        };
        if parsed.program == from || dry_now(state, &parsed.program, now_ms) {
            continue;
        }
        let found = programs
            .iter()
            .find(|program| program.spec.id == parsed.program && program.installed);
        if let Some(program) = found {
            return Some(Pick {
                agent: parsed.program,
                model: parsed.model,
                launch: program.spec.launch.clone(),
            });
        }
    }
    None
}

/// Whether the log has `agent` on cooldown as of `now_ms`.
///
/// A cooldown without an end is dry until a human re-admits the agent; one with
/// an end expires on its own, so a reset in the past no longer counts.
fn dry_now(state: &RunState, agent: &str, now_ms: u64) -> bool {
    match state.agents.get(agent) {
        Some(info) if info.dry => match info.dry_until_ms {
            Some(until) => until > now_ms,
            None => true,
        },
        _ => false,
    }
}

/// Commits whatever the outgoing agent left uncommitted, and returns the head
/// the incoming agent resumes from.
///
/// A clean worktree already is a checkpoint, so nothing is committed then: the
/// hand-off must never add an empty commit to the attempt's history.
fn checkpoint(worktree: &Path, from: &str) -> Result<String, CliError> {
    let git = Git::new(worktree);
    let clean = git.is_clean().map_err(|err| {
        CliError::other(anyhow!(err).context(format!(
            "looking for uncommitted work in {}",
            worktree.display()
        )))
    })?;
    if !clean {
        let message = format!("wip(shprd): checkpoint before hand-off from {from}");
        git.run(&["add", "-A"])
            .map_err(|err| CliError::other(anyhow!(err).context("staging the outgoing work")))?;
        // The commit carries its own identity and skips the hooks: an
        // unattended checkpoint must not depend on the machine's git config.
        git.run(&[
            "-c",
            "user.name=shprd",
            "-c",
            "user.email=shprd@localhost",
            "commit",
            "--no-verify",
            "-m",
            &message,
        ])
        .map_err(|err| CliError::other(anyhow!(err).context("committing the checkpoint")))?;
    }
    git.head_sha()
        .map_err(|err| CliError::other(anyhow!(err).context("reading the checkpoint")))
}

/// What the attempt changed against the run's integration branch, as git
/// summarizes it. A branch git cannot compare is reported, never fatal.
fn diffstat(worktree: &Path, base: &str) -> String {
    match Git::new(worktree).run(&["diff", "--stat", &format!("{base}..HEAD")]) {
        Ok(text) if text.trim().is_empty() => "(no changes yet)".to_string(),
        Ok(text) => text,
        Err(err) => format!("(not available: {err})"),
    }
}

/// Where a hand-off brief lives:
/// `<briefs>/<run>/<task>-a<attempt>-h<handoff>.md`.
fn handoff_brief_path(briefs: &Path, run: &str, task: &str, attempt: u32, handoff: u32) -> PathBuf {
    briefs
        .join(run)
        .join(format!("{task}-a{attempt}-h{handoff}.md"))
}

/// How many times this attempt was handed over before, from the log.
///
/// Only hand-offs after the attempt's own `Spawned` event count, so a retry
/// whose attempt number repeats in a later run still starts at zero, and a
/// task handed over twice in one attempt numbers its briefs `h1` and `h2`.
fn previous_handoffs(events: &[Event], task: &str, attempt: u32) -> u32 {
    let spawned_at = events
        .iter()
        .filter(|event| match &event.kind {
            EventKind::Spawned {
                task: spawned,
                attempt: spawned_attempt,
                ..
            } => spawned == task && *spawned_attempt == attempt,
            _ => false,
        })
        .map(|event| event.seq)
        .max();
    let Some(spawned_at) = spawned_at else {
        return 0;
    };
    let count = events
        .iter()
        .filter(|event| {
            event.seq > spawned_at
                && matches!(&event.kind, EventKind::HandedOff { task: handed, .. } if handed == task)
        })
        .count();
    u32::try_from(count).unwrap_or(u32::MAX)
}

/// How many lines of the outgoing agent's pane the brief carries.
const PANE_TAIL_LINES: usize = 30;

/// The end of the pane the outgoing agent ran in, when its tab still exists.
///
/// Reading the pane is the cheapest way to tell the incoming agent what the
/// other one was in the middle of; a tab the human already closed is not an
/// error, the brief just says nothing about it.
fn old_pane_tail(
    mux: &dyn Mux,
    workspace: &Workspace,
    env: &SpawnEnv<'_>,
    task: &str,
    attempt: &Attempt,
) -> Option<String> {
    let record = spawn::tab_path(&env.briefs_dir, &env.run, task, attempt.n);
    let wanted = std::fs::read_to_string(record).ok()?;
    let wanted = wanted.trim().to_string();
    let tabs = mux.list_tabs(&workspace.id).ok()?;
    let tab = tabs.iter().find(|tab| tab.id.as_str() == wanted)?;
    let text = mux.read(&tab.root_pane, PANE_TAIL_LINES).ok()?;
    let text = text.trim_end().to_string();
    if text.is_empty() { None } else { Some(text) }
}

/// What the hand-off section of the brief says, as data.
struct Transfer<'a> {
    /// The agent handing the task over.
    from: &'a str,
    /// The agent taking it over.
    to: &'a str,
    /// Why the human handed it over.
    reason: &'a str,
    /// The commit the incoming agent resumes from.
    checkpoint: &'a str,
    /// The run's integration branch, which the diff is taken against.
    base_branch: &'a str,
    /// `git diff --stat` in the task's worktree.
    diffstat: &'a str,
    /// The end of the outgoing agent's pane, when it was readable.
    pane_tail: Option<&'a str>,
}

/// Renders the hand-off brief: the brief the attempt started with, then what
/// the newcomer needs that the outgoing agent did not have.
fn handoff_brief(base: &BriefInput<'_>, transfer: &Transfer<'_>) -> String {
    let mut out = spawn::render_brief(base);
    out.push_str("\n## You are taking over\n\n");
    out.push_str(&format!(
        "- this attempt was handed from `{}` to `{}`\n",
        transfer.from, transfer.to
    ));
    out.push_str(&format!("- why: {}\n", transfer.reason));
    out.push_str(&format!("- checkpoint: `{}`\n", transfer.checkpoint));
    out.push_str("- continue from the checkpoint; do not start over; do not rewrite history\n");
    out.push_str(&format!(
        "\n### Changes since `{}`\n\n```\n{}\n```\n",
        transfer.base_branch, transfer.diffstat
    ));
    out.push_str("\n### The end of the previous agent's pane\n\n");
    match transfer.pane_tail {
        Some(tail) => out.push_str(&format!("```\n{tail}\n```\n")),
        None => out.push_str("(not available: that tab is gone)\n"),
    }
    out
}
