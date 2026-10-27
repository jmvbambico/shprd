//! Verb implementations, one module per verb family.

pub mod agent;
pub mod run;

use anyhow::anyhow;
use shprd_core::event::{Event, EventKind};

use crate::cli::{Cli, Command, RunCommand};
use crate::ctx::{CliError, Ctx, resolve_actor, resolve_root};

/// Runs whichever verb the command line asked for.
pub fn dispatch(cli: Cli) -> Result<(), CliError> {
    let root = resolve_root(cli.global.repo.as_deref())?;
    let actor = resolve_actor(cli.global.actor.as_deref())?;
    let ctx = Ctx::new(root, actor, cli.global.json);

    match cli.command {
        Command::Run { cmd } => match cmd {
            RunCommand::Start(args) => run::start(&ctx, &args),
            RunCommand::Finish => run::finish(&ctx),
        },
        Command::Agent { cmd } => agent::dispatch(&ctx, cmd),
        Command::State => dump(&ctx),
    }
}

/// Prints the folded state: JSON when asked, else one short human summary.
pub fn dump(ctx: &Ctx) -> Result<(), CliError> {
    let state = ctx.state()?;
    if ctx.json {
        let text = serde_json::to_string_pretty(&crate::summary::build(&state))
            .map_err(|err| CliError::other(anyhow!(err).context("serializing the state")))?;
        println!("{text}");
    } else {
        println!("{}", crate::summary::describe(&state));
    }
    Ok(())
}

/// Prints an appended event: JSON when asked, else one short human line.
pub fn emit(ctx: &Ctx, event: &Event) -> Result<(), CliError> {
    if ctx.json {
        let text = serde_json::to_string(event)
            .map_err(|err| CliError::other(anyhow!(err).context("serializing the event")))?;
        println!("{text}");
    } else {
        println!("ok {} seq={}", kind_name(&event.kind), event.seq);
    }
    Ok(())
}

/// The snake-case name of an event kind, as it appears in the log.
pub fn kind_name(kind: &EventKind) -> &'static str {
    match kind {
        EventKind::RunStarted { .. } => "run_started",
        EventKind::TaskAdded { .. } => "task_added",
        EventKind::Spawned { .. } => "spawned",
        EventKind::Reported { .. } => "reported",
        EventKind::GatePassed { .. } => "gate_passed",
        EventKind::GateFailed { .. } => "gate_failed",
        EventKind::Integrated { .. } => "integrated",
        EventKind::Dry { .. } => "dry",
        EventKind::HandedOff { .. } => "handed_off",
        EventKind::Escalated { .. } => "escalated",
        EventKind::Override { .. } => "override",
        EventKind::RunFinished {} => "run_finished",
    }
}
