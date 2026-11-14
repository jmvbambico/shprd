//! `shprd approve`: a human approves the run's pull request, in shprd.
//!
//! GitHub never lets a pull request's author approve it, so a solo maintainer
//! cannot satisfy the approval `land --merge` waits for there. This verb
//! records the approval in the run log instead, and it is deliberately the
//! human's verb only: an agent pane always has `SHPRD_ACTOR` set, and the
//! question is asked of a terminal, which no agent shell tool has.
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

/// Approves the run's pull request at the integration head a human is shown.
pub fn approve_verb(ctx: &Ctx, args: &ApproveArgs, home: Option<&Path>) -> Result<(), CliError> {
    confirm::require_human(
        &confirm::named_actor(),
        "approves a pull request",
        "ask the maintainer to run `shprd approve`",
    )?;
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
