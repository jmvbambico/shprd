//! Verb implementations, one module per verb family.

pub mod agent;
pub mod approve;
pub mod ask;
pub mod confirm;
pub mod dagr;
pub mod deliver;
pub mod doctor;
pub mod drop;
pub mod fullgate;
pub mod handoff;
pub mod integrate;
pub mod janitor;
pub mod land;
pub mod live_fallback;
pub mod live_news;
pub mod override_rule;
pub mod prompts;
pub mod readmit;
pub mod review;
pub mod roster;
pub mod run;
pub mod run_lock;
pub mod serve;
pub mod set;
pub mod settings;
pub mod spawn;
pub mod sync;
pub mod tick;
pub mod verdict;
pub mod view;
pub mod watch;

use anyhow::anyhow;
use shprd_core::event::{Event, EventKind};

use crate::cli::{AskCommand, Cli, Command, DagrCommand, RunCommand, TaskVerb};
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
    // `settings` reads files, not a log, so it answers outside a repository
    // too: the person's own file applies everywhere.
    if let Some(Command::Settings { cmd }) = &cli.command {
        let root = settings_root(cli.global.repo.as_deref());
        return settings::run(
            cmd,
            cli.global.json,
            cli.global.home.as_deref(),
            root.as_deref(),
        );
    }
    // `prompts` reads and writes files, not a log, so it answers outside a
    // repository too.
    if let Some(Command::Prompts { cmd }) = &cli.command {
        return prompts::run(cmd, cli.global.json, cli.global.home.as_deref());
    }
    // `doctor` diagnoses the machine, so it answers outside a repository too.
    if let Some(Command::Doctor(args)) = &cli.command {
        return doctor::run(args, cli.global.json, cli.global.home.as_deref());
    }
    let root = resolve_root(cli.global.repo.as_deref())?;
    let actor = resolve_actor(cli.global.actor.as_deref())?;
    let ctx = Ctx::new(root, actor, cli.global.json, cli.global.no_dagr);

    match cli.command {
        None => view::status(&ctx, cli.global.home.as_deref()),
        Some(Command::Run { cmd }) => match cmd {
            RunCommand::Start(args) => run::start(&ctx, &args),
            RunCommand::Finish(args) => run::finish_verb(&ctx, &args, cli.global.home.as_deref()),
        },
        Some(Command::Agent { cmd }) => agent::dispatch(&ctx, cmd, cli.global.home.as_deref()),
        Some(Command::Task { cmd }) => match cmd {
            TaskVerb::Status(args) => agent::task_status(&ctx, &args),
        },
        Some(Command::Ask { cmd }) => match cmd {
            AskCommand::Hook(args) => ask::hook(&args, cli.global.repo.as_deref()),
            AskCommand::Raise(args) => ask::raise(&args, &ctx),
            AskCommand::Install(args) => ask::install(&args, cli.global.repo.as_deref()),
        },
        Some(Command::State) => dump(&ctx),
        Some(Command::Brief) => view::brief(&ctx),
        Some(Command::Status) => view::status(&ctx, cli.global.home.as_deref()),
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
        // Unreachable in practice: `settings` returns above.
        Some(Command::Settings { cmd }) => {
            let root = settings_root(cli.global.repo.as_deref());
            settings::run(&cmd, ctx.json, cli.global.home.as_deref(), root.as_deref())
        }
        // Unreachable in practice: `prompts` returns above.
        Some(Command::Prompts { cmd }) => prompts::run(&cmd, ctx.json, None),
        Some(Command::Doctor(args)) => doctor::run(&args, ctx.json, None),
        Some(Command::Watch(args)) => watch::run_verb(&ctx, &args, cli.global.home.as_deref()),
        Some(Command::Gate(args)) => fullgate::gate_verb(&ctx, &args, cli.global.home.as_deref()),
        Some(Command::Sync) => sync::sync_verb(&ctx, cli.global.home.as_deref()),
        Some(Command::Land(args)) => land::land_verb(&ctx, &args, cli.global.home.as_deref()),
        Some(Command::Approve(args)) => {
            approve::approve_verb(&ctx, &args, cli.global.home.as_deref())
        }
        Some(Command::Readmit(args)) => readmit::readmit_verb(&ctx, &args),
        Some(Command::Override(args)) => override_rule::override_verb(&ctx, &args),
        Some(Command::Drop(args)) => drop::drop_verb(&ctx, &args, cli.global.home.as_deref()),
        Some(Command::Serve(args)) => serve::run(&ctx, &args, cli.global.home.as_deref()),
        Some(Command::Tick(args)) => tick::tick_verb(&ctx, &args, cli.global.home.as_deref()),
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
        EventKind::TaskStatus { .. } => "task_status",
        EventKind::AskRaised { .. } => "ask_raised",
        EventKind::AskAnswered { .. } => "ask_answered",
        EventKind::GatePassed { .. } => "gate_passed",
        EventKind::GateFailed { .. } => "gate_failed",
        EventKind::Integrated { .. } => "integrated",
        EventKind::TaskDropped { .. } => "task_dropped",
        EventKind::ReviewRequested { .. } => "review_requested",
        EventKind::Dry { .. } => "dry",
        EventKind::Available { .. } => "available",
        EventKind::HandedOff { .. } => "handed_off",
        EventKind::Escalated { .. } => "escalated",
        EventKind::Override { .. } => "override",
        EventKind::Resource { .. } => "resource",
        EventKind::FullGate { .. } => "full_gate",
        EventKind::PrOpened { .. } => "pr_opened",
        EventKind::PrMerged { .. } => "pr_merged",
        EventKind::Approved { .. } => "approved",
        EventKind::TaskPrOpened { .. } => "task_pr_opened",
        EventKind::TaskApproved { .. } => "task_approved",
        EventKind::TaskLanded { .. } => "task_landed",
        EventKind::OrchestratorLaunched { .. } => "orchestrator_launched",
        EventKind::WatchLaunched { .. } => "watch_launched",
        EventKind::OrchestratorNotified { .. } => "orchestrator_notified",
        EventKind::BaseSynced { .. } => "base_synced",
        EventKind::Nudged { .. } => "nudged",
        EventKind::Backup { .. } => "backup",
        EventKind::ProviderGaveUp { .. } => "provider_gave_up",
        EventKind::RunFinished { .. } => "run_finished",
    }
}

/// The repository the settings are for: `--repo`, else this checkout, else none.
///
/// Settings work outside a repository, so a checkout that cannot be found is
/// not an error: only the person's own file applies then.
fn settings_root(explicit: Option<&std::path::Path>) -> Option<std::path::PathBuf> {
    match explicit {
        Some(repo) => Some(repo.to_path_buf()),
        None => resolve_root(None).ok(),
    }
}
