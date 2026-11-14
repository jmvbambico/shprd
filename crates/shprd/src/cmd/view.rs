//! Read-only verbs: brief, status and PR body.
//!
//! Each one folds the log, calls one projector from `shprd_views` and prints
//! the result. None of them writes anything.

use anyhow::anyhow;
use shprd_core::log::{Clock, SystemClock};

use crate::ctx::{CliError, Ctx};

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
pub fn status(ctx: &Ctx) -> Result<(), CliError> {
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
    Ok(())
}

/// `shprd pr-body`: the pull-request body: the log, rendered as Markdown.
pub fn pr_body(ctx: &Ctx) -> Result<(), CliError> {
    let (state, events) = ctx.load()?;
    println!("{}", shprd_views::pr::pr_body(&state, &events));
    Ok(())
}
