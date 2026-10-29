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

use std::path::Path;

use anyhow::anyhow;
use shprd_core::event::EventKind;
use shprd_git::git::Git;
use shprd_git::integrate;

use crate::cli::ApproveArgs;
use crate::cmd::confirm;
use crate::cmd::land;
use crate::ctx::{CliError, Ctx, resolve_layout};

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
    let layout = resolve_layout(home, &config.janitor.namespace)?;
    let repo_name = land::repo_name(ctx)?;
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
