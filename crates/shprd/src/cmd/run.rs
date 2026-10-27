//! `shprd run …`: the run's lifecycle verbs.

use shprd_core::event::EventKind;

use crate::cli::RunStartArgs;
use crate::cmd::emit;
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

/// Records that the run is over.
pub fn finish(ctx: &Ctx) -> Result<(), CliError> {
    let event = ctx.commit(EventKind::RunFinished {})?;
    emit(ctx, &event)
}
