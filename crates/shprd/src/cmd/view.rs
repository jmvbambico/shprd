//! Read-only verbs: brief, status and PR body.
//!
//! Each one folds the log, calls one projector from `shprd_views` and prints
//! the result. None of them writes anything.

use std::path::Path;

use anyhow::anyhow;
use shprd_core::log::{Clock, SystemClock};
use shprd_core::state::RunState;

use crate::ctx::{CliError, Ctx, resolve_home};

/// `shprd brief`: the rehydration block for an agent that lost its context.
pub fn brief(ctx: &Ctx) -> Result<(), CliError> {
    let (state, events) = ctx.load()?;
    println!("{}", shprd_views::brief::brief(&state, &events));
    Ok(())
}

/// `shprd status`: where every task sits, one line each (or one JSON document
/// with `--json`).
///
/// The word each line shows is the derived one, so a task an open ask waits on
/// reads "Needs you" here as it does on the service's board; `--json` keeps the
/// stored state beside it, as `status` and `derived` fields.
///
/// A run can sit still because the last advance pass failed a step — an agent
/// that would not start, a landing the forge refused — and nothing in the log
/// says so, because a pass is the engine's own act and not an event. The
/// service leaves that pass's outcome in the state directory, so this verb says
/// it in one line on stderr, where it cannot disturb a `--json` document.
pub fn status(ctx: &Ctx, home: Option<&Path>) -> Result<(), CliError> {
    let state = ctx.state()?;
    let now_ms = SystemClock.now_ms();
    if ctx.json {
        let doc = shprd_views::status::json(&state, now_ms);
        let text = serde_json::to_string(&doc)
            .map_err(|err| CliError::other(anyhow!(err).context("serializing the status")))?;
        println!("{text}");
    } else {
        println!("{}", shprd_views::status::status(&state, now_ms));
    }
    warn_about_last_pass(ctx, &state, home);
    Ok(())
}

/// Says so when the last advance pass failed a step, and says nothing when it
/// did not.
///
/// A home that cannot be resolved leaves no state directory to read, and a pass
/// that has not run leaves no record: neither is worth a line, and neither may
/// fail the verb. A record that names another run of this repository is
/// history — its failure has been answered by starting the run it was about
/// over — so it says nothing either.
fn warn_about_last_pass(ctx: &Ctx, state: &RunState, home: Option<&Path>) {
    let Some(last) = last_pass(ctx, home) else {
        return;
    };
    if last.failed.is_empty() {
        return;
    }
    if let (Some(recorded), Some(current)) = (&last.run, &state.run)
        && recorded != &current.run_id
    {
        return;
    }
    eprintln!("warning: {}", last.warning());
}

/// The last pass the service (or `shprd tick`) ran against this repository.
fn last_pass(ctx: &Ctx, home: Option<&Path>) -> Option<shprd_server::LastPass> {
    let home = resolve_home(home).ok()?;
    shprd_server::LastPass::read(&shprd_server::state_dir(&home), &ctx.root)
}

/// `shprd pr-body`: the pull-request body: the log, rendered as Markdown.
pub fn pr_body(ctx: &Ctx) -> Result<(), CliError> {
    let (state, events) = ctx.load()?;
    println!("{}", shprd_views::pr::pr_body(&state, &events));
    Ok(())
}
