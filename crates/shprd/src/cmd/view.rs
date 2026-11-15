//! Read-only verbs: brief, status and PR body.
//!
//! Each one folds the log, calls one projector from `shprd_views` and prints
//! the result. None of them writes anything.

use std::path::Path;

use anyhow::anyhow;
use shprd_core::log::{Clock, SystemClock};
use shprd_core::state::RunState;

use crate::ctx::{CliError, Ctx, resolve_home};

/// `shprd brief`: the rehydration block for an agent that lost its context.
pub fn brief(ctx: &Ctx) -> Result<(), CliError> {
    let (state, events) = ctx.load()?;
    println!("{}", shprd_views::brief::brief(&state, &events));
    Ok(())
}

/// `shprd status`: where every task sits, one line each (or one JSON document
/// with `--json`).
///
/// The word each line shows is the derived one, so a task an open ask waits on
/// reads "Needs you" here as it does on the service's board; `--json` keeps the
/// stored state beside it, as `status` and `derived` fields.
///
/// A run can sit still because the last advance pass failed a step — an agent
/// that would not start, a landing the forge refused — and nothing in the log
/// says so, because a pass is the engine's own act and not an event. The
/// service leaves that pass's outcome in the state directory, so this verb says
/// it in one line on stderr, where it cannot disturb a `--json` document.
pub fn status(ctx: &Ctx, home: Option<&Path>) -> Result<(), CliError> {
    let state = ctx.state()?;
    let now_ms = SystemClock.now_ms();
    if ctx.json {
        let doc = shprd_views::status::json(&state, now_ms);
        let text = serde_json::to_string(&doc)
            .map_err(|err| CliError::other(anyhow!(err).context("serializing the status")))?;
        println!("{text}");
    } else {
        println!("{}", shprd_views::status::status(&state, now_ms));
    }
    warn_about_last_pass(ctx, &state, home);
    Ok(())
}

/// Says so when the last advance pass failed a step, and says nothing when it
/// did not.
///
/// A home that cannot be resolved leaves no state directory to read, and a pass
/// that has not run leaves no record: neither is worth a line, and neither may
/// fail the verb.
fn warn_about_last_pass(ctx: &Ctx, state: &RunState, home: Option<&Path>) {
    let Some(last) = last_pass(ctx, home) else {
        return;
    };
    if worth_a_warning(state, &last) {
        eprintln!("warning: {}", last.warning());
    }
}

/// Whether the recorded pass is still the person's to hear about.
///
/// Three passes are history rather than news: one that failed nothing; one that
/// named another run of this repository, whose failure was answered by starting
/// the run that is on now; and one from a run that has finished, because nothing
/// can act on it any more and no pass will run to clear the record.
fn worth_a_warning(state: &RunState, last: &shprd_server::LastPass) -> bool {
    if last.failed.is_empty() || state.finished {
        return false;
    }
    match (&last.run, &state.run) {
        (Some(recorded), Some(current)) => recorded == &current.run_id,
        // A record that names a run the log no longer has is history too.
        (Some(_), None) => false,
        // A pass that could not read the run names none: that one always says.
        (None, _) => true,
    }
}

/// The last pass the service (or `shprd tick`) ran against this repository.
fn last_pass(ctx: &Ctx, home: Option<&Path>) -> Option<shprd_server::LastPass> {
    let home = resolve_home(home).ok()?;
    shprd_server::LastPass::read(&shprd_server::state_dir(&home), &ctx.root)
}

/// `shprd pr-body`: the pull-request body: the log, rendered as Markdown.
pub fn pr_body(ctx: &Ctx) -> Result<(), CliError> {
    let (state, events) = ctx.load()?;
    println!("{}", shprd_views::pr::pr_body(&state, &events));
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::worth_a_warning;
    use shprd_core::state::{RunInfo, RunState};
    use shprd_server::{FailedStep, LastPass};

    /// A run's identity, as the fold holds it.
    fn run(id: &str) -> RunInfo {
        RunInfo {
            run_id: id.to_string(),
            repo: String::from("/repo"),
            base_branch: String::from("main"),
            integration_branch: String::from("integration/m9"),
        }
    }

    /// A record of a pass that failed one step.
    fn failed_pass(run: Option<&str>) -> LastPass {
        LastPass {
            repo: String::from("/repo"),
            at_ms: 1_700_000_000_000,
            steps: 1,
            run: run.map(str::to_string),
            failed: vec![FailedStep {
                task: String::from("T9"),
                step: String::from("land"),
                error: String::from("the forge refused"),
            }],
        }
    }

    #[test]
    fn a_failed_pass_on_the_current_run_is_worth_a_warning() {
        let state = RunState {
            run: Some(run("r2")),
            ..RunState::default()
        };
        assert!(worth_a_warning(&state, &failed_pass(Some("r2"))));
        // And a pass that could not read the run at all names none.
        assert!(worth_a_warning(&state, &failed_pass(None)));
    }

    #[test]
    fn a_finished_runs_failure_is_never_warned_again() {
        // The M9 fix I review: the record is never superseded, so a finished
        // run's last failure would be warned beside every `status` for ever,
        // though nothing can act on it and no pass will run to clear it.
        let mut state = RunState {
            run: Some(run("r2")),
            ..RunState::default()
        };
        state.finished = true;
        assert!(!worth_a_warning(&state, &failed_pass(Some("r2"))));
        assert!(!worth_a_warning(&state, &failed_pass(None)));
    }

    #[test]
    fn another_runs_failure_and_a_clean_pass_say_nothing() {
        let state = RunState {
            run: Some(run("r2")),
            ..RunState::default()
        };
        assert!(!worth_a_warning(&state, &failed_pass(Some("r1"))));
        let clean = LastPass {
            failed: Vec::new(),
            ..failed_pass(Some("r2"))
        };
        assert!(!worth_a_warning(&state, &clean));
        // A log with no run at all leaves the pass with no run to be about.
        assert!(!worth_a_warning(
            &RunState::default(),
            &failed_pass(Some("r2"))
        ));
    }
}
