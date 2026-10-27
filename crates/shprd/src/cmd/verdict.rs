//! `shprd agent review-verdict`: decide a review, then record what follows.
//!
//! The reviewer only describes findings. This verb applies the configured
//! policy in [`shprd_core::gate`] — so the pass/fail call always comes from
//! code — and records either the pass, or the failure plus its consequence.

use std::path::Path;

use anyhow::{Context, anyhow};
use shprd_core::config::{self, Review};
use shprd_core::event::{EventKind, Finding};
use shprd_core::gate::{self, Next, REVIEW_GATE};

use crate::cli::ReviewVerdictArgs;
use crate::ctx::{CliError, Ctx};

/// What a human is asked to decide when a review escalates.
const ESCALATION_DEFAULT: &str = "decide whether to accept, re-scope or drop";

/// Decides the review for one attempt and records the outcome.
pub fn verdict_verb(
    ctx: &Ctx,
    args: &ReviewVerdictArgs,
    home: Option<&Path>,
) -> Result<(), CliError> {
    let policy = review_policy(ctx, home)?;
    let findings = read_findings(&args.findings)?;
    let decided = gate::verdict(&findings, policy.must_fix);
    if decided.passed {
        // A pass keeps every finding it was given: minor advice is still worth
        // printing in the pull request, so an empty list would lose it.
        let kind = EventKind::GatePassed {
            task: args.task.clone(),
            attempt: args.attempt,
            gate: REVIEW_GATE.to_string(),
            findings,
        };
        record(ctx, kind)?;
        println!("review passed");
        return Ok(());
    }
    let blocking = decided.blocking.len();
    let kind = EventKind::GateFailed {
        task: args.task.clone(),
        attempt: args.attempt,
        gate: REVIEW_GATE.to_string(),
        findings,
    };
    record(ctx, kind)?;
    let task = reviewed_task(ctx, &args.task)?;
    match gate::after_failure(&task, policy.rounds) {
        Next::SendBack => println!("review failed: {blocking} blocking (send back)"),
        Next::Escalate => {
            let rounds = gate::failed_review_rounds(&task);
            let kind = EventKind::Escalated {
                task: Some(args.task.clone()),
                body: format!("review failed {rounds} times"),
                default_action: ESCALATION_DEFAULT.to_string(),
            };
            record(ctx, kind)?;
            println!("review failed: {blocking} blocking (escalated)");
        }
    }
    Ok(())
}

/// The review policy in force: the global file, then the repo override.
fn review_policy(ctx: &Ctx, home: Option<&Path>) -> Result<Review, CliError> {
    let home = crate::ctx::resolve_home(home)?;
    let global = home.join(".config").join("shprd").join("config.toml");
    let repo = ctx.root.join(".shprd").join("config.toml");
    let loaded = config::load(Some(&global), Some(&repo))
        .map_err(|err| CliError::other(anyhow!(err).context("loading the shprd config")))?;
    Ok(loaded.review)
}

/// Reads the JSON array of findings the reviewer reported.
///
/// Unreadable or invalid findings are the caller's mistake, so they exit 2 as
/// a rule refusal does, naming the file.
fn read_findings(path: &Path) -> Result<Vec<Finding>, CliError> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading findings from {}", path.display()))
        .map_err(CliError::Invalid)?;
    serde_json::from_str(&text)
        .with_context(|| format!("parsing findings in {}", path.display()))
        .map_err(CliError::Invalid)
}

/// The folded task, or an error naming the id when the plan has no such task.
fn reviewed_task(ctx: &Ctx, id: &str) -> Result<shprd_core::state::Task, CliError> {
    let state = ctx.state()?;
    state
        .tasks
        .get(id)
        .cloned()
        .ok_or_else(|| CliError::Invalid(anyhow!("no such task: {id}")))
}

/// Appends one event, then rewrites the dagr document, warning on failure.
fn record(ctx: &Ctx, kind: EventKind) -> Result<(), CliError> {
    ctx.commit(kind)?;
    if !ctx.no_dagr
        && let Err(err) = crate::cmd::dagr::sync(ctx)
    {
        eprintln!("warning: dagr sync failed: {err}");
    }
    Ok(())
}
