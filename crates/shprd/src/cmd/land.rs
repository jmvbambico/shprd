//! `shprd land`: the run's work leaves as one pull request.
//!
//! Landing is the only place shprd talks to a forge, and the only place it
//! pushes. Everything it decides comes from the log: the run must be active,
//! no task may still be in flight, at least one task must have landed, and the
//! run-wide gate must have passed on the very head being pushed. A refusal
//! writes nothing and exits 2.
//!
//! With `--merge` the same verb applies `[land] merge`: a human's approval is
//! always enough, and the `allow` policy merges an unapproved pull request only
//! when its base branch is not protected.

use std::path::{Path, PathBuf};

use anyhow::anyhow;
use shprd_core::config::{self, Config};
use shprd_core::event::{Event, EventKind, ResourceAction, ResourceKind};
use shprd_core::gate::REVIEW_GATE;
use shprd_core::state::{RunState, Task, TaskStatus};
use shprd_git::forge::{FakeForge, Forge, GhForge};
use shprd_git::git::Git;
use shprd_git::integrate;

use crate::cli::LandArgs;
use crate::ctx::{CliError, Ctx, resolve_home, resolve_layout};

/// Environment variable naming a scripted forge, used by the integration tests.
///
/// Production runs always use `gh`; only a test sets this, and then every forge
/// call goes to the script named here instead of the network.
pub const FAKE_FORGE_ENV: &str = "SHPRD_FAKE_FORGE";

/// The line a body ends with, as the run's policy requires.
const REVIEW_PASSED: &str = "cross-vendor-review: passed";

/// The line a body ends with when no independent review could be had.
const REVIEW_DEGRADED: &str = "degraded-review";

/// The attribution line every generated pull request ends with.
const ATTRIBUTION: &str = "🤖 Generated with [Claude Code](https://claude.com/claude-code)";

/// Opens the run's pull request, or with `--merge` applies the merge policy.
pub fn land_verb(ctx: &Ctx, args: &LandArgs, home: Option<&Path>) -> Result<(), CliError> {
    let config = load_config(ctx, home)?;
    let (state, events) = ctx.load()?;
    let run = state.run.as_ref().ok_or_else(|| {
        CliError::Invalid(anyhow!(
            "no run yet: start one with `shprd run start` before landing"
        ))
    })?;
    let run_id = run.run_id.clone();
    let base_branch = run.base_branch.clone();
    if state.finished {
        return Err(CliError::Invalid(anyhow!(
            "the run {run_id} has finished: there is nothing left to land"
        )));
    }
    check_in_flight(&state)?;
    let integrated = integrated_tasks(&state);
    if integrated.is_empty() {
        return Err(CliError::Invalid(anyhow!(
            "no task is integrated: land a task with `shprd agent integrate` first"
        )));
    }
    let layout = resolve_layout(home, &config.janitor.namespace)?;
    let repo_name = repo_name(ctx)?;
    let worktree =
        integrate::open_integration(&ctx.root, &layout, &repo_name, &run_id, &base_branch)
            .map_err(|err| {
                CliError::other(anyhow!(err).context("opening the integration worktree"))
            })?;
    let git = Git::new(&worktree.path);
    let head = git
        .head_sha()
        .map_err(|err| CliError::other(anyhow!(err).context("reading the integration head")))?;
    check_full_gate(&state, &head)?;

    if args.merge {
        return Err(CliError::other(anyhow!(
            "landing with --merge lands in the next step"
        )));
    }
    open(ctx, &state, &events, &worktree, &integrated)
}

/// Refuses while any task is still working or waiting to be gated.
fn check_in_flight(state: &RunState) -> Result<(), CliError> {
    let busy = state
        .tasks
        .values()
        .find(|task| matches!(task.status, TaskStatus::Working | TaskStatus::Reported));
    match busy {
        Some(task) => Err(CliError::Invalid(anyhow!(
            "task {:?} is {:?}: land once every task is integrated",
            task.id,
            task.status
        ))),
        None => Ok(()),
    }
}

/// Every task that landed, in plan order, which is at least one by the caller.
fn integrated_tasks(state: &RunState) -> Vec<&Task> {
    state
        .tasks
        .values()
        .filter(|task| task.status == TaskStatus::Integrated)
        .collect()
}

/// Refuses unless the run-wide gate passed on exactly this head.
fn check_full_gate(state: &RunState, head: &str) -> Result<(), CliError> {
    match &state.last_full_gate {
        None => Err(CliError::Invalid(anyhow!(
            "no full gate has run: run `shprd gate` before landing"
        ))),
        Some((gate_head, false, _)) => Err(CliError::Invalid(anyhow!(
            "the last full gate failed on head {gate_head}: fix it and run `shprd gate` again"
        ))),
        Some((gate_head, true, _)) if gate_head != head => Err(CliError::Invalid(anyhow!(
            "the last full gate passed on head {gate_head}, but the integration branch is at \
             {head}: run `shprd gate` again"
        ))),
        Some((_, true, _)) => Ok(()),
    }
}

/// Pushes the branch, refreshes or opens the pull request, records it.
fn open(
    ctx: &Ctx,
    state: &RunState,
    events: &[Event],
    worktree: &integrate::IntegrationWorktree,
    integrated: &[&Task],
) -> Result<(), CliError> {
    let run = state
        .run
        .as_ref()
        .ok_or_else(|| CliError::other(anyhow!("no run to land")))?;
    let body = render_body(state, events);
    let body_file = write_body(worktree, &body)?;
    let title = match integrated.first() {
        Some(task) => format!("{}: {}", run.run_id, task.title),
        None => run.run_id.clone(),
    };
    push(worktree, state, ctx)?;
    let forge = forge(&worktree.path)?;
    let branch = worktree.branch.clone();
    let pr = match forge.find_open(&branch).map_err(|err| {
        CliError::other(anyhow!(err).context("asking the forge for the pull request"))
    })? {
        Some(pr) => {
            forge.edit_body(pr.number, &body_file).map_err(|err| {
                CliError::other(anyhow!(err).context("refreshing the pull request"))
            })?;
            pr
        }
        None => forge
            .create(&run.base_branch, &branch, &title, &body_file)
            .map_err(|err| CliError::other(anyhow!(err).context("opening the pull request")))?,
    };
    let event = ctx.commit(EventKind::PrOpened {
        number: pr.number,
        url: pr.url.clone(),
        head: branch,
    })?;
    crate::cmd::emit(ctx, &event)?;
    if !ctx.json {
        println!("{}", pr.url);
    }
    Ok(())
}

/// Pushes the integration branch, recording the remote branch once.
fn push(
    worktree: &integrate::IntegrationWorktree,
    state: &RunState,
    ctx: &Ctx,
) -> Result<(), CliError> {
    let git = Git::new(&worktree.path);
    git.run(&["push", "-u", "origin", &worktree.branch])
        .map_err(|err| {
            CliError::other(anyhow!(err).context(format!(
                "pushing {} to origin from {}",
                worktree.branch,
                worktree.path.display()
            )))
        })?;
    let key = (ResourceKind::RemoteBranch, worktree.branch.clone());
    if matches!(
        state.resources.get(&key).map(|record| record.status),
        Some(shprd_core::state::ResourceStatus::Live)
    ) {
        return Ok(());
    }
    ctx.commit(EventKind::Resource {
        action: ResourceAction::Created,
        kind: ResourceKind::RemoteBranch,
        name: worktree.branch.clone(),
        task: None,
    })?;
    Ok(())
}

/// Where the body is written: beside the integration worktree, in shprd's own
/// state directory, so no file of the user's repository is touched.
fn write_body(worktree: &integrate::IntegrationWorktree, body: &str) -> Result<PathBuf, CliError> {
    let dir = worktree
        .path
        .parent()
        .ok_or_else(|| CliError::other(anyhow!("the integration worktree has no parent")))?;
    let path = dir.join("pr-body.md");
    std::fs::write(&path, body).map_err(|err| {
        CliError::other(anyhow!(err).context(format!("writing {}", path.display())))
    })?;
    Ok(path)
}

/// The pull request body: the run's facts, the review marker, the attribution.
fn render_body(state: &RunState, events: &[Event]) -> String {
    let mut body = shprd_views::pr::pr_body(state, events);
    if !body.ends_with('\n') {
        body.push('\n');
    }
    body.push('\n');
    body.push_str(review_marker(state, events));
    body.push('\n');
    body.push('\n');
    body.push_str(ATTRIBUTION);
    body.push('\n');
    body
}

/// `cross-vendor-review: passed` only when every landed task's newest verdict
/// is a passed review from an independent reviewer.
///
/// A review that could not be independent says so — in its findings, in an
/// escalation, or in the override a human wrote to accept it — so anything that
/// mentions degraded downgrades the whole body's marker.
fn review_marker(state: &RunState, events: &[Event]) -> &'static str {
    if degraded_mentioned(events) {
        return REVIEW_DEGRADED;
    }
    for task in integrated_tasks(state) {
        let Some(attempt) = task.latest_attempt() else {
            return REVIEW_DEGRADED;
        };
        let verdict = attempt
            .gates
            .iter()
            .rev()
            .find(|gate| gate.gate == REVIEW_GATE);
        match verdict {
            Some(gate) if gate.passed => {}
            _ => return REVIEW_DEGRADED,
        }
        if attempt
            .gates
            .iter()
            .filter(|gate| gate.gate == REVIEW_GATE)
            .flat_map(|gate| gate.findings.iter())
            .any(|finding| mentions_degraded(&finding.title) || mentions_degraded(&finding.detail))
        {
            return REVIEW_DEGRADED;
        }
    }
    REVIEW_PASSED
}

/// True when any escalation body or override reason says the review was
/// degraded.
fn degraded_mentioned(events: &[Event]) -> bool {
    events.iter().any(|event| match &event.kind {
        EventKind::Escalated { body, .. } => mentions_degraded(body),
        EventKind::Override { reason, rule, .. } => {
            mentions_degraded(reason) || mentions_degraded(rule)
        }
        _ => false,
    })
}

/// True when `text` says a review was degraded.
fn mentions_degraded(text: &str) -> bool {
    text.to_lowercase().contains("degraded")
}

/// The forge to drive: `gh` here, or the script `SHPRD_FAKE_FORGE` names.
fn forge(dir: &Path) -> Result<Box<dyn Forge>, CliError> {
    match std::env::var_os(FAKE_FORGE_ENV) {
        Some(path) if !path.is_empty() => {
            let script = PathBuf::from(path);
            let fake = FakeForge::from_script(&script).map_err(|err| {
                CliError::other(anyhow!(err).context("reading the scripted forge"))
            })?;
            Ok(Box::new(fake))
        }
        _ => Ok(Box::new(GhForge::new(dir))),
    }
}

/// The config's `[land]`, with the repo file winning over the global one.
fn load_config(ctx: &Ctx, home: Option<&Path>) -> Result<Config, CliError> {
    let home = resolve_home(home)?;
    let global = home.join(".config").join("shprd").join("config.toml");
    let repo = ctx.root.join(".shprd").join("config.toml");
    config::load(Some(&global), Some(&repo))
        .map_err(|err| CliError::other(anyhow!(err).context("loading the config")))
}

/// The name of the repository directory, which keeps repos apart on disk.
fn repo_name(ctx: &Ctx) -> Result<String, CliError> {
    ctx.root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| CliError::other(anyhow!("{} has no directory name", ctx.root.display())))
}
