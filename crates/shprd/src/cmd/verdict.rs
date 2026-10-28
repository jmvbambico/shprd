//! `shprd agent review-verdict`: decide a review, then record what follows.
//!
//! The reviewer only describes findings. This verb applies the configured
//! policy in [`shprd_core::gate`] — so the pass/fail call always comes from
//! code — and records either the pass, or the failure plus its consequence.

use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow};
use shprd_core::config::{self, Review};
use shprd_core::event::{Actor, EventKind, Finding, ReviewSource};
use shprd_core::gate::{self, Next, REVIEW_GATE};
use shprd_core::hash;
use shprd_core::rules::RuleError;
use shprd_mux::Mux;

use crate::cli::ReviewVerdictArgs;
use crate::cmd::{review, spawn};
use crate::ctx::{CliError, Ctx};

/// What a human is asked to decide when a review escalates.
const ESCALATION_DEFAULT: &str = "decide whether to accept, re-scope or drop";

/// What a submitted verdict decided, so a caller can say so its own way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// No finding blocked: the review gate passed.
    Passed,
    /// The gate failed and the task goes back for another attempt.
    SentBack {
        /// How many findings blocked.
        blocking: usize,
    },
    /// The gate failed and a human is now asked what to do.
    Escalated {
        /// How many findings blocked.
        blocking: usize,
    },
}

/// Decides the review for one attempt and records the outcome.
pub fn verdict_verb(
    ctx: &Ctx,
    args: &ReviewVerdictArgs,
    home: Option<&Path>,
) -> Result<(), CliError> {
    require_engine(ctx, &args.task)?;
    let mux = spawn::real_mux();
    let outcome = submit_verdict(&mux, ctx, home, &args.task, args.attempt)?;
    match outcome {
        Outcome::Passed => println!("review passed"),
        Outcome::SentBack { blocking } => {
            println!("review failed: {blocking} blocking (send back)")
        }
        Outcome::Escalated { blocking } => {
            println!("review failed: {blocking} blocking (escalated)");
        }
    }
    Ok(())
}

/// Applies the review policy to the reviewer's packet and records what it
/// decides.
///
/// The reviewer only writes findings; the call to pass or fail is made here, by
/// [`gate`], so a verdict is reproducible from the file alone. The file is the
/// packet's own [`review::findings_path`], never one a caller names: a verdict
/// must come from the reviewer, so the only findings that can decide a review
/// are the ones written into its packet. `home` picks the config the policy
/// comes from, as `--home` does for every other verb. The verdict is also the
/// reviewer's last word: once it is recorded, the reviewer's tab is closed, and
/// `mux` is the host that closes it.
pub fn submit_verdict(
    mux: &dyn Mux,
    ctx: &Ctx,
    home: Option<&Path>,
    task: &str,
    attempt: u32,
) -> Result<Outcome, CliError> {
    let home = crate::ctx::resolve_home(home)?;
    let policy = review_policy(ctx, Some(&home))?;
    let findings_path = packet_path(ctx, &home, task, attempt)?;
    let bytes = std::fs::read(&findings_path)
        .with_context(|| format!("reading findings from {}", findings_path.display()))
        .map_err(CliError::Invalid)?;
    let findings = parse_findings(&bytes, &findings_path)?;
    let source = provenance(ctx, task, attempt, &bytes)?;
    let decided = gate::verdict(&findings, policy.must_fix);
    if decided.passed {
        // A pass keeps every finding it was given: minor advice is still worth
        // printing in the pull request, so an empty list would lose it.
        let kind = EventKind::GatePassed {
            task: task.to_string(),
            attempt,
            gate: REVIEW_GATE.to_string(),
            findings,
            source: Some(source),
        };
        record(ctx, kind)?;
        close_reviewer(mux, ctx, &home, task, attempt);
        return Ok(Outcome::Passed);
    }
    let blocking = decided.blocking.len();
    let kind = EventKind::GateFailed {
        task: task.to_string(),
        attempt,
        gate: REVIEW_GATE.to_string(),
        findings,
        source: Some(source),
    };
    record(ctx, kind)?;
    // The verdict is the review's last word, so the reviewer's tab is done:
    // closing it before the consequence is recorded keeps the tab's life tied
    // to the review, never to what the failure costs.
    close_reviewer(mux, ctx, &home, task, attempt);
    let reviewed = reviewed_task(ctx, task)?;
    match gate::after_failure(&reviewed, policy.rounds) {
        Next::SendBack => Ok(Outcome::SentBack { blocking }),
        Next::Escalate => {
            let rounds = gate::failed_review_rounds(&reviewed);
            let kind = EventKind::Escalated {
                task: Some(task.to_string()),
                body: format!("review failed {rounds} times"),
                default_action: ESCALATION_DEFAULT.to_string(),
            };
            record(ctx, kind)?;
            Ok(Outcome::Escalated { blocking })
        }
    }
}

/// Refuses the verb outright for any actor but the engine.
///
/// The rule in [`shprd_core::rules`] refuses the same thing when the event is
/// appended; this says it before a file is read, and names the verb's own
/// escape hatch for a human, so an orchestrator that tries to review itself is
/// told what to do instead of watching a verdict it cannot record.
fn require_engine(ctx: &Ctx, task: &str) -> Result<(), CliError> {
    if matches!(ctx.actor, Actor::Engine) {
        return Ok(());
    }
    Err(CliError::Refused(RuleError::VerdictNotFromReviewer {
        task: task.to_string(),
    }))
}

/// The provenance a review verdict carries: who reviewed, and what it read.
///
/// The reviewer is the one the review dispatch recorded, never the actor that
/// ran the verb: the engine decides, but the finding's author is the reviewer.
/// The hash pins the exact bytes folded into the gate result, so a reader can
/// tell whether the packet still says what the verdict decided.
fn provenance(ctx: &Ctx, task: &str, attempt: u32, bytes: &[u8]) -> Result<ReviewSource, CliError> {
    let state = ctx.state()?;
    let found = state
        .tasks
        .get(task)
        .ok_or_else(|| CliError::Invalid(anyhow!("no such task: {task}")))?;
    let current = found
        .attempts
        .last()
        .filter(|current| current.n == attempt)
        .ok_or_else(|| CliError::Invalid(anyhow!("task {task} has no attempt {attempt}")))?;
    let reviewer = current.reviews.last().map(|review| review.reviewer.clone());
    let reviewer = reviewer.ok_or_else(|| {
        CliError::Invalid(anyhow!(
            "no review was requested for {task} attempt {attempt}: a verdict needs a reviewer"
        ))
    })?;
    Ok(ReviewSource {
        reviewer,
        findings_sha256: hash::sha256_hex(bytes),
    })
}

/// The findings file the reviewer for one attempt wrote.
///
/// It is derived, never passed in: the packet folder names the run, the task and
/// the attempt, and only a reviewer writes inside it.
fn packet_path(ctx: &Ctx, home: &Path, task: &str, attempt: u32) -> Result<PathBuf, CliError> {
    let run = active_run(ctx).ok_or_else(|| {
        CliError::Invalid(anyhow!(
            "no run has started: there is no review packet for {task} attempt {attempt}"
        ))
    })?;
    let reviews = home.join(".shprd").join("reviews");
    Ok(review::findings_path(&reviews, &run, task, attempt))
}

/// Closes the reviewer's tab for one attempt, once its verdict is recorded.
///
/// A close the host refuses is warned about, never fatal: the verdict the run
/// waits for is already in the log, and failing here would tell the caller the
/// review decided nothing. A run that has already finished has no reviewer left
/// to stop.
fn close_reviewer(mux: &dyn Mux, ctx: &Ctx, home: &Path, task: &str, attempt: u32) {
    let Some(run) = active_run(ctx) else {
        return;
    };
    let briefs = home.join(".shprd").join("briefs");
    if let Err(err) = review::close_reviewer(mux, &briefs, &run, task, attempt) {
        eprintln!("warning: {err}");
    }
}

/// The run in flight's id, whether or not it has finished: a verdict for a
/// finished run still names the run whose records hold the reviewer's tab.
fn active_run(ctx: &Ctx) -> Option<String> {
    ctx.state().ok()?.run.as_ref().map(|run| run.run_id.clone())
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
pub(crate) fn read_findings(path: &Path) -> Result<Vec<Finding>, CliError> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("reading findings from {}", path.display()))
        .map_err(CliError::Invalid)?;
    parse_findings(&bytes, path)
}

/// Parses the JSON array of findings the reviewer reported.
fn parse_findings(bytes: &[u8], path: &Path) -> Result<Vec<Finding>, CliError> {
    serde_json::from_slice(bytes)
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
