//! Read-only verbs: brief, status and PR body.
//!
//! Each one folds the log, calls one projector from `shprd_views` and prints
//! the result. None of them writes anything.

use shprd_core::log::{Clock, SystemClock};

use crate::ctx::{CliError, Ctx};

/// `shprd brief`: the rehydration block for an agent that lost its context.
pub fn brief(ctx: &Ctx) -> Result<(), CliError> {
    let (state, events) = ctx.load()?;
    println!("{}", shprd_views::brief::brief(&state, &events));
    Ok(())
}

/// `shprd status`: where every task sits, one line each.
pub fn status(ctx: &Ctx) -> Result<(), CliError> {
    let state = ctx.state()?;
    println!(
        "{}",
        shprd_views::status::status(&state, SystemClock.now_ms())
    );
    Ok(())
}

/// `shprd pr-body`: the pull-request body: the log, rendered as Markdown.
pub fn pr_body(ctx: &Ctx) -> Result<(), CliError> {
    let (state, events) = ctx.load()?;
    println!("{}", shprd_views::pr::pr_body(&state, &events));
    Ok(())
}
