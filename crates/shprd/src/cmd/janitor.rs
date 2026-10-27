//! `shprd janitor` and `shprd restore`: the archive-aware cleanup verbs.
//!
//! The plan comes from [`shprd_git::janitor::plan`], which reads only the run's
//! own ledger. `--apply` runs it and appends one `resource` event per step that
//! succeeded, through the same check-then-append path every other verb uses.
//! A dry run writes nothing at all.

use std::path::Path;

use anyhow::anyhow;
use serde_json::json;
use shprd_core::event::{EventKind, ResourceAction, ResourceKind};
use shprd_core::log::{Clock, SystemClock};

use crate::cli::{JanitorArgs, RestoreArgs};
use crate::cmd::emit;
use crate::ctx::{CliError, Ctx, resolve_home, resolve_layout};
use shprd_core::config::{self as config, Config};
use shprd_git::archive;
use shprd_git::janitor;
use shprd_git::worktree::Layout;

/// Runs `shprd janitor`: print the plan, or apply it.
pub fn plan_verb(
    ctx: &Ctx,
    args: &JanitorArgs,
    home: Option<&std::path::Path>,
) -> Result<(), CliError> {
    let (loaded, layout) = config_and_layout(ctx, home)?;
    let archive_days = args.archive_days.unwrap_or(loaded.janitor.archive_days);
    let (state, events) = ctx.load()?;
    let run = run_id(&state)?;
    let now_ms = SystemClock.now_ms();
    let actions = janitor::plan(&state, &events, &layout, now_ms, archive_days);

    if !args.apply {
        print_plan(ctx, &actions);
        return Ok(());
    }

    let outcomes = janitor::execute(&ctx.root, &layout, &run, &actions);
    let mut failures = 0u32;
    for outcome in &outcomes {
        match &outcome.result {
            Ok(kinds) => {
                for kind in kinds {
                    let event = ctx.commit(kind.clone())?;
                    emit(ctx, &event)?;
                }
            }
            Err(err) => {
                failures = failures.saturating_add(1);
                eprintln!("error: {}: {err}", outcome.action.describe());
            }
        }
    }
    if failures > 0 {
        return Err(CliError::other(anyhow!(
            "{failures} janitor action(s) failed; the rest were applied"
        )));
    }
    Ok(())
}

/// Runs `shprd restore <task>`: recreate the branch from its archive ref.
pub fn restore_verb(
    ctx: &Ctx,
    args: &RestoreArgs,
    home: Option<&std::path::Path>,
) -> Result<(), CliError> {
    let (_config, layout) = config_and_layout(ctx, home)?;
    let state = ctx.state()?;
    let run = match &args.run {
        Some(run) => run.clone(),
        None => run_id(&state)?,
    };
    let branch = archive::restore(&ctx.root, &layout, &run, &args.task)
        .map_err(|err| CliError::other(anyhow!(err).context("restoring the archived branch")))?;
    let kind = EventKind::Resource {
        action: ResourceAction::Created,
        kind: ResourceKind::Branch,
        name: branch.clone(),
        task: Some(args.task.clone()),
    };
    let event = ctx.commit(kind)?;
    emit(ctx, &event)
}

/// The resolved config and the layout its janitor settings describe.
///
/// The global file is `<home>/.config/shprd/config.toml`, the repo file is
/// `<repo>/.shprd/config.toml`, and the repo file wins.
fn config_and_layout(ctx: &Ctx, home: Option<&Path>) -> Result<(Config, Layout), CliError> {
    let home = resolve_home(home)?;
    let global = home.join(".config").join("shprd").join("config.toml");
    let repo = ctx.root.join(".shprd").join("config.toml");
    let loaded = config::load(Some(&global), Some(&repo))
        .map_err(|err| CliError::other(anyhow!(err).context("loading the config")))?;
    let layout = resolve_layout(Some(&home), &loaded.janitor.namespace)?;
    Ok((loaded, layout))
}

/// The run id the plan is about.
fn run_id(state: &shprd_core::state::RunState) -> Result<String, CliError> {
    match &state.run {
        Some(info) => Ok(info.run_id.clone()),
        None => Err(CliError::other(anyhow!(
            "no run in the log: start one with `shprd run start`"
        ))),
    }
}

/// Prints the plan, one line per step, and nothing else.
fn print_plan(ctx: &Ctx, actions: &[janitor::Action]) {
    if ctx.json {
        let lines: Vec<String> = actions.iter().map(|action| action.describe()).collect();
        println!("{}", json!({ "plan": lines }));
        return;
    }
    for action in actions {
        println!("would {}", action.describe());
    }
    if actions.is_empty() {
        println!("nothing to clean up");
    }
}
