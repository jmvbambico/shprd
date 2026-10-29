//! `shprd run …`: the run's lifecycle verbs.

use std::path::Path;

use shprd_core::event::EventKind;
use shprd_mux::Mux;

use crate::cli::{EndRunArgs, RunStartArgs};
use crate::cmd::{emit, spawn};
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

/// Records that the run is over, closing the tabs it left open.
///
/// It refuses exactly as `down` does: a run with unfinished work is a human's
/// decision, and `--abandon` is how they make it.
pub fn finish(
    mux: &dyn Mux,
    ctx: &Ctx,
    home: Option<&Path>,
    abandon: bool,
) -> Result<(), CliError> {
    let state = ctx.state()?;
    let kind = crate::cmd::up::finish_kind(&state, abandon)?;
    let event = ctx.commit(kind)?;
    crate::cmd::up::print_abandoned(&state, &event);
    spawn::close_run_tabs(mux, ctx, home);
    crate::cmd::up::close_watch_pane(mux, ctx);
    emit(ctx, &event)
}

/// `shprd run finish` against the host the human is working in.
pub fn finish_verb(ctx: &Ctx, args: &EndRunArgs, home: Option<&Path>) -> Result<(), CliError> {
    finish(&spawn::real_mux(), ctx, home, args.abandon)
}
