//! Verb implementations, one module per verb family.

pub mod agent;
pub mod dagr;
pub mod doctor;
pub mod handoff;
pub mod janitor;
pub mod keys;
pub mod launch;
pub mod roster;
pub mod run;
pub mod set;
pub mod spawn;
pub mod up;
pub mod view;
pub mod watch;

use anyhow::anyhow;
use shprd_core::event::{Event, EventKind};

use crate::cli::{Cli, Command, DagrCommand, RunCommand};
use crate::ctx::{CliError, Ctx, resolve_actor, resolve_root};

/// Runs whichever verb the command line asked for.
pub fn dispatch(cli: Cli) -> Result<(), CliError> {
    // `roster` reads no run log, so it answers outside a repository too.
    if let Some(Command::Roster(args)) = &cli.command {
        return roster::run(args, cli.global.json);
    }
    // `set` writes a config file, not a log, so it too needs no repository.
    if let Some(Command::Set(args)) = &cli.command {
        return set::run(args, cli.global.home.as_deref(), cli.global.repo.as_deref());
    }
    // `keys` edits herdr's config file, so it too needs no repository.
    if let Some(Command::Keys(args)) = &cli.command {
        return keys::run(args, cli.global.home.as_deref());
    }
    // `doctor` diagnoses the machine, so it answers outside a repository too.
    if let Some(Command::Doctor(args)) = &cli.command {
        return doctor::run(args, cli.global.json, cli.global.home.as_deref());
    }
    let root = resolve_root(cli.global.repo.as_deref())?;
    let actor = resolve_actor(cli.global.actor.as_deref())?;
    let ctx = Ctx::new(root, actor, cli.global.json, cli.global.no_dagr);

    match cli.command {
        None => view::status(&ctx),
        Some(Command::Run { cmd }) => match cmd {
            RunCommand::Start(args) => run::start(&ctx, &args),
            RunCommand::Finish => run::finish(&ctx),
        },
        Some(Command::Agent { cmd }) => agent::dispatch(&ctx, cmd, cli.global.home.as_deref()),
        Some(Command::State) => dump(&ctx),
        Some(Command::Brief) => view::brief(&ctx),
        Some(Command::Status) => view::status(&ctx),
        Some(Command::PrBody) => view::pr_body(&ctx),
        Some(Command::Dagr { cmd }) => match cmd {
            DagrCommand::Sync => dagr::sync_verb(&ctx),
        },
        Some(Command::Janitor(args)) => janitor::plan_verb(&ctx, &args, cli.global.home.as_deref()),
        Some(Command::Restore(args)) => {
            janitor::restore_verb(&ctx, &args, cli.global.home.as_deref())
        }
        Some(Command::Roster(args)) => roster::run(&args, ctx.json),
        // Unreachable in practice: `set` returns above, before any log is read.
        Some(Command::Set(args)) => set::run(
            &args,
            cli.global.home.as_deref(),
            cli.global.repo.as_deref(),
        ),
        Some(Command::Doctor(args)) => doctor::run(&args, ctx.json, None),
        Some(Command::Keys(args)) => keys::run(&args, None),
        Some(Command::Up) => up::up_verb(&ctx, cli.global.home.as_deref()),
        Some(Command::Down) => up::down(&ctx),
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

/// Prints an appended event, then syncs the dagr document.
///
/// The sync is deliberately soft: the event is already in the log, so a failed
/// projection must not turn a successful write into a failure.
pub fn emit(ctx: &Ctx, event: &Event) -> Result<(), CliError> {
    let printed = print_event(ctx, event);
    auto_sync(ctx);
    printed
}

/// Prints an appended event: JSON when asked, else one short human line.
fn print_event(ctx: &Ctx, event: &Event) -> Result<(), CliError> {
    if ctx.json {
        let text = serde_json::to_string(event)
            .map_err(|err| CliError::other(anyhow!(err).context("serializing the event")))?;
        println!("{text}");
    } else {
        println!("ok {} seq={}", kind_name(&event.kind), event.seq);
    }
    Ok(())
}

/// Rewrites the dagr document, warning on stderr when that fails.
///
/// With no run yet there is nothing to project, so this skips silently: an
/// event written before `run started` is not a failure.
fn auto_sync(ctx: &Ctx) {
    if ctx.no_dagr {
        return;
    }
    if let Err(err) = dagr::sync(ctx) {
        eprintln!("warning: dagr sync failed: {err}");
    }
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
        EventKind::Available { .. } => "available",
        EventKind::HandedOff { .. } => "handed_off",
        EventKind::Escalated { .. } => "escalated",
        EventKind::Override { .. } => "override",
        EventKind::Resource { .. } => "resource",
        EventKind::RunFinished {} => "run_finished",
    }
}
