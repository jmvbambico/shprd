//! `shprd agent handoff`: move a task from one agent to the next.
//!
//! A hand-off keeps the attempt's worktree, branch and log entry and changes
//! only who works in them: the outgoing agent's work is checkpointed, a
//! hand-off brief says where to continue, and the tab is replaced. Every
//! decision is made before the first side effect, the way [`spawn`] does it.

use anyhow::anyhow;
use shprd_core::log::{Clock, SystemClock};
use shprd_core::state::{RunState, Task, TaskStatus};
use shprd_roster::models::AgentRef;

use crate::cmd::spawn::{self, Pick, Program};
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
