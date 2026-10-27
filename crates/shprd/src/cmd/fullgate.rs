//! `shprd gate`: run the integration branch's gates once, in its worktree.
//!
//! The commands come from `[gates] integration` in the config, because they are
//! the repository's own definition of "green". They run in the integration
//! worktree, never in the user's checkout, with the clean worker environment,
//! and the whole verdict is appended to the log as one
//! [`EventKind::FullGate`] event: a pass or a fail is a fact a later reader can
//! check without the machine that produced it.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use anyhow::anyhow;
use shprd_core::config::{self, Config};
use shprd_core::event::EventKind;
use shprd_git::gate::{self, GateRun};
use shprd_git::git::Git;
use shprd_git::integrate;
use shprd_roster::env::{EnvContext, Role, launch_env};

use crate::cli::FullGateArgs;
use crate::ctx::{CliError, Ctx, resolve_home, resolve_layout};

/// The message printed when the config defines no integration gate.
const NO_GATE: &str =
    "no integration gate configured: set [gates] integration in .shprd/config.toml";

/// Runs the integration gate and records what happened.
///
/// Exits 2 when there is nothing to run or the worktree is not clean, 1 when a
/// step fails, and 0 only when every step passed. The event is appended in
/// every case the commands ran: a failed gate is still a result.
pub fn gate_verb(ctx: &Ctx, args: &FullGateArgs, home: Option<&Path>) -> Result<(), CliError> {
    let state = ctx.state()?;
    let run = state
        .run
        .as_ref()
        .ok_or_else(|| CliError::other(anyhow!("no run yet: start one with `shprd run start`")))?;
    if state.finished {
        return Err(CliError::other(anyhow!(
            "the run has finished: there is no gate to run"
        )));
    }
    let config = load_config(ctx, home)?;
    let commands = &config.gates.integration;
    if commands.is_empty() {
        return Err(CliError::Invalid(anyhow!(NO_GATE)));
    }
    let home = resolve_home(home)?;
    let layout = resolve_layout(Some(&home), &config.janitor.namespace)?;
    let repo_name = ctx
        .root
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .ok_or_else(|| CliError::other(anyhow!("{} has no directory name", ctx.root.display())))?;
    let worktree = integrate::open_integration(
        &ctx.root,
        &layout,
        &repo_name,
        &run.run_id,
        &run.base_branch,
    )
    .map_err(|err| CliError::other(anyhow!(err).context("opening the integration worktree")))?;
    let git = Git::new(&worktree.path);
    let clean = git
        .is_clean()
        .map_err(|err| CliError::other(anyhow!(err).context("reading the integration worktree")))?;
    if !clean {
        return Err(CliError::Invalid(anyhow!(
            "the integration worktree at {} has uncommitted changes",
            worktree.path.display()
        )));
    }

    let env = worker_env(&worktree.path, &run.run_id);
    let timeout = Duration::from_secs(args.timeout_secs);
    let outcome = gate::run_gate(&worktree.path, commands, timeout, &env);
    let head = git
        .head_sha()
        .map_err(|err| CliError::other(anyhow!(err).context("reading the integration head")))?;
    if !ctx.json {
        print_steps(&outcome);
    }
    let event = ctx.commit(EventKind::FullGate {
        head,
        passed: outcome.passed,
        steps: outcome.steps.clone(),
    })?;
    super::emit(ctx, &event)?;
    if outcome.passed {
        return Ok(());
    }
    Err(CliError::other(anyhow!(
        "the integration gate failed: fix it on the integration branch and run `shprd gate` again"
    )))
}

/// The config's `[gates]`, with the repo file winning over the global one.
fn load_config(ctx: &Ctx, home: Option<&Path>) -> Result<Config, CliError> {
    let home = resolve_home(home)?;
    let global = home.join(".config").join("shprd").join("config.toml");
    let repo = ctx.root.join(".shprd").join("config.toml");
    config::load(Some(&global), Some(&repo))
        .map_err(|err| CliError::other(anyhow!(err).context("loading the config")))
}

/// The clean environment a worker CLI gets, as `Command` env pairs.
///
/// A gate is a worker's command in every sense that matters: it must not see a
/// remote credential, and it must not wait on a git prompt.
fn worker_env(worktree: &Path, run_id: &str) -> Vec<(String, Option<String>)> {
    let base: BTreeMap<String, String> = std::env::vars().collect();
    let ctx = EnvContext {
        actor: "worker".to_string(),
        task: None,
        run: Some(run_id.to_string()),
        repo: worktree.to_path_buf(),
    };
    launch_env(&base, Role::Worker, &ctx).as_pairs()
}

/// Prints one line per step: a mark, the command, and how long it took.
fn print_steps(outcome: &GateRun) {
    for step in &outcome.steps {
        let mark = if step.exit_code == Some(0) {
            "✓"
        } else {
            "✗"
        };
        println!("{mark} {} ({}s)", step.command, step.duration_ms / 1000);
    }
}
