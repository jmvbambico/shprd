//! The `state --json` dump and its human one-liner.
//!
//! The JSON is built here, from `shprd-core`'s public fields, so the core
//! crate grows no serialization derives just to feed the CLI.

use serde_json::{Value, json};
use shprd_core::state::{RunState, Task, TaskStatus};

/// The snake-case name of a task status.
pub fn status_name(status: TaskStatus) -> &'static str {
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

/// Builds the JSON summary of a folded run state.
pub fn build(state: &RunState) -> Value {
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

    let tasks: Vec<Value> = state.tasks.values().map(task_json).collect();

    let dry: Vec<Value> = state
        .agents
        .iter()
        .filter(|(_, info)| info.dry)
        .map(|(id, info)| {
            json!({
                "agent": id,
                "until_ms": info.dry_until_ms,
                "reason": info.dry_reason,
            })
        })
        .collect();

    json!({
        "run": run,
        "tasks": tasks,
        "dry_agents": dry,
        "escalations": state.escalations.len(),
        "last_seq": state.last_seq,
    })
}

/// Builds one task's JSON object.
fn task_json(task: &Task) -> Value {
    let attempts = task.attempts.len();
    let latest_agent = task
        .latest_attempt()
        .map(|attempt| attempt.agent.clone())
        .unwrap_or_else(|| "none".to_string());
    json!({
        "id": task.id,
        "title": task.title,
        "status": status_name(task.status),
        "attempts": attempts,
        "latest_agent": latest_agent,
        "scope": task.scope,
    })
}

/// One short human line describing a folded run state.
pub fn describe(state: &RunState) -> String {
    let run = match &state.run {
        Some(info) => info.run_id.as_str(),
        None => "(no run)",
    };
    let mut counts = [0usize; 7];
    for task in state.tasks.values() {
        let slot = match task.status {
            TaskStatus::Queued => 0,
            TaskStatus::Working => 1,
            TaskStatus::Reported => 2,
            TaskStatus::Passed => 3,
            TaskStatus::Failed => 4,
            TaskStatus::Integrated => 5,
            TaskStatus::Dropped => 6,
        };
        counts[slot] += 1;
    }
    format!(
        "run {run}: {} tasks ({}/{} done, {} dropped), {} working, {} failed, {} escalations",
        state.tasks.len(),
        counts[5],
        state.tasks.len(),
        counts[6],
        counts[1],
        counts[4],
        state.escalations.len()
    )
}
