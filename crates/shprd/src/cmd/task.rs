//! `shprd task reopen <TASK>`: a human moves a handed-in task back to Working.
//!
//! A task that handed in is `Reported` and waits for its review; when that
//! review cannot start or the work has to change, nothing moved it on. This is
//! the way out: the same attempt, worktree and branch, with the hand-in and the
//! review cleared so the worker can hand in again. The earlier events stay in
//! the log. It is the human's alone, the rule refuses any other actor.

use anyhow::anyhow;
use shprd_core::event::EventKind;

use crate::cli::TaskReopenArgs;
use crate::ctx::{CliError, Ctx};

/// Reopens a Reported or Passed task for its newest attempt.
pub fn reopen(ctx: &Ctx, args: &TaskReopenArgs) -> Result<(), CliError> {
    let state = ctx.state()?;
    let task = state.tasks.get(&args.task).ok_or_else(|| {
        CliError::Invalid(anyhow!(
            "task {:?} is not in the run's plan: `shprd status` lists them",
            args.task
        ))
    })?;
    let attempt = task
        .latest_attempt()
        .map(|attempt| attempt.n)
        .ok_or_else(|| {
            CliError::Invalid(anyhow!(
                "task {:?} has no attempt to reopen: it never started",
                args.task
            ))
        })?;
    let event = ctx.commit(EventKind::TaskReopened {
        task: args.task.clone(),
        attempt,
    })?;
    crate::cmd::emit(ctx, &event)?;
    if !ctx.json {
        println!(
            "reopened {} (attempt {attempt}): it is Working again",
            args.task
        );
    }
    Ok(())
}
