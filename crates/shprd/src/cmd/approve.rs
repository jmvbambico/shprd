//! `shprd approve`: a human approves a pull request, in shprd.
//!
//! GitHub never lets a pull request's author approve it, so a solo maintainer
//! cannot satisfy the approval the `ask` merge policy waits for there. This verb
//! records the approval in the run log instead — the run's pull request, or with
//! a task named, that task's own — and it is deliberately the human's verb only:
//! an agent pane always has `SHPRD_ACTOR` set, and the question is asked of a
//! terminal, which no agent shell tool has.
//!
//! The terminal check is the barrier that survives a missing `SHPRD_ACTOR`:
//! a pane restored without its environment still has no TTY. Only a debug
//! build may answer for the record with `--yes`, which is how the end-to-end
//! tests drive this verb; a release binary always asks.
//!
//! The approval is pinned to a head, so the verb takes the run's integration
//! lock before it reads that head, like `land`, `sync` and `gate` do: a sync or
//! a cherry-pick landing while the head is read and the approval is written
//! would stamp an approval on a head nobody was shown. The lock is held while
//! the question is asked and until the approval is recorded, so the head the
//! human answers about is the head the log holds.

use std::path::Path;

use anyhow::anyhow;
use shprd_core::event::EventKind;
use shprd_git::git::Git;
use shprd_git::integrate;

use crate::cli::ApproveArgs;
use crate::cmd::confirm;
use crate::cmd::land;
use crate::cmd::run_lock;
use crate::ctx::{CliError, Ctx, resolve_home, resolve_layout};

/// Approves the run's pull request, or with a task, that task's own.
pub fn approve_verb(ctx: &Ctx, args: &ApproveArgs, home: Option<&Path>) -> Result<(), CliError> {
    confirm::require_human(
        &confirm::named_actor(),
        "approves a pull request",
        "ask the maintainer to run `shprd approve`",
    )?;
    if let Some(task) = &args.task {
        return approve_task(ctx, args, task);
    }
    approve_run(ctx, args, home)
}

/// Approves one task's landing at the commit its branch is at.
///
/// That is the task's own pull request when a landing opened one, and the
/// task itself when there is no forge: a local landing has no pull request, so
/// the approval is recorded on the task and landing reads it from there.
///
/// The approval names that commit, so a task whose branch moved after it was
/// approved has to be approved again: what the human read is what they agreed
/// to. The commit is read from the attempt's worktree, which is where landing
/// reads it too, so the two cannot disagree about it.
fn approve_task(ctx: &Ctx, args: &ApproveArgs, task_id: &str) -> Result<(), CliError> {
    let state = ctx.state()?;
    let task = state.tasks.get(task_id).ok_or_else(|| {
        CliError::Invalid(anyhow!(
            "no task {task_id:?} in this run: `shprd status` lists the ones it holds"
        ))
    })?;
    // A repository with no forge lands locally and has no pull request to
    // approve; the head and the approval live on the task itself then.
    let (number, label) = match task.pr.as_ref() {
        Some(pr) => {
            if pr.merged {
                return Err(CliError::Invalid(anyhow!(
                    "{} is already merged: there is nothing to approve",
                    pr.url
                )));
            }
            (pr.number, pr.url.clone())
        }
        None => (0, format!("{task_id}'s local landing")),
    };
    let attempt = task
        .latest_attempt()
        .ok_or_else(|| CliError::Invalid(anyhow!("task {task_id:?} has no attempt to approve")))?;
    let head = Git::new(&attempt.worktree).head_sha().map_err(|err| {
        CliError::other(anyhow!(err).context(format!(
            "reading the head of {} in {}",
            attempt.branch, attempt.worktree
        )))
    })?;
    let question = format!("approve {label} at {}? [y/N] ", land::short(&head));
    if !confirm::answered(args.yes, "shprd approve", &question)? {
        return Err(CliError::Invalid(anyhow!(
            "not approved: {label} still waits for one"
        )));
    }
    let event = ctx.commit(EventKind::TaskApproved {
        task: task_id.to_string(),
        number,
        head_sha: head.clone(),
    })?;
    crate::cmd::emit(ctx, &event)?;
    if !ctx.json {
        println!("approved {label} at {}", land::short(&head));
    }
    Ok(())
}

/// Approves the run's pull request at the integration head a human is shown.
fn approve_run(ctx: &Ctx, args: &ApproveArgs, home: Option<&Path>) -> Result<(), CliError> {
    let config = land::load_config(ctx, home)?;
    let state = ctx.state()?;
    let run = state.run.as_ref().ok_or_else(|| {
        CliError::Invalid(anyhow!(
            "no run yet: start one with `shprd run start` before approving"
        ))
    })?;
    let pr = state.pr.as_ref().ok_or_else(|| {
        CliError::Invalid(anyhow!(
            "no pull request yet: run `shprd land` first, then approve it"
        ))
    })?;
    let number = pr.number;
    let url = pr.url.clone();
    let run_id = run.run_id.clone();
    let base_branch = run.base_branch.clone();
    let home = resolve_home(home)?;
    let layout = resolve_layout(Some(&home), &config.janitor.namespace)?;
    let repo_name = land::repo_name(ctx)?;
    // Held from the head read to the recorded approval: the head is what the
    // approval is about, so it must not move between the two.
    let _lock = run_lock::lock_run(&home, &repo_name, &run_id)?;
    let worktree =
        integrate::open_integration(&ctx.root, &layout, &repo_name, &run_id, &base_branch)
            .map_err(|err| {
                CliError::other(anyhow!(err).context("opening the integration worktree"))
            })?;
    let head = Git::new(&worktree.path)
        .head_sha()
        .map_err(|err| CliError::other(anyhow!(err).context("reading the integration head")))?;
    let question = format!("approve {url} at {}? [y/N] ", land::short(&head));
    if !confirm::answered(args.yes, "shprd approve", &question)? {
        return Err(CliError::Invalid(anyhow!(
            "not approved: {url} still waits for one"
        )));
    }
    let event = ctx.commit(EventKind::Approved {
        number,
        head_sha: head.clone(),
    })?;
    crate::cmd::emit(ctx, &event)?;
    if !ctx.json {
        println!("approved {url} at {}", land::short(&head));
    }
    Ok(())
}
