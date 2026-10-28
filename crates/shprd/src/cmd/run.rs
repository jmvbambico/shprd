//! `shprd run …`: the run's lifecycle verbs.

use std::path::Path;

use shprd_core::event::EventKind;
use shprd_mux::Mux;

use crate::cli::RunStartArgs;
use crate::cmd::{emit, review, spawn};
use crate::ctx::{CliError, Ctx};

/// Records the run's first event.
pub fn start(ctx: &Ctx, args: &RunStartArgs) -> Result<(), CliError> {
    let repo = ctx.root.to_string_lossy().to_string();
    let kind = EventKind::RunStarted {
        run_id: args.run_id.clone(),
        repo,
        base_branch: args.base.clone(),
        integration_branch: args.integration.clone(),
    };
    let event = ctx.commit(kind)?;
    emit(ctx, &event)
}

/// Records that the run is over, closing the reviewer tabs it left open.
pub fn finish(mux: &dyn Mux, ctx: &Ctx, home: Option<&Path>) -> Result<(), CliError> {
    let event = ctx.commit(EventKind::RunFinished {})?;
    review::close_run_reviewers(mux, ctx, home);
    emit(ctx, &event)
}

/// `shprd run finish` against the host the human is working in.
pub fn finish_verb(ctx: &Ctx, home: Option<&Path>) -> Result<(), CliError> {
    finish(&spawn::real_mux(), ctx, home)
}
