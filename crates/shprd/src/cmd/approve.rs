//! `shprd approve`: a human approves the run's pull request, in shprd.
//!
//! GitHub never lets a pull request's author approve it, so a solo maintainer
//! cannot satisfy the approval `land --merge` waits for there. This verb
//! records the approval in the run log instead, and it is deliberately the
//! human's verb only: an agent pane always has `SHPRD_ACTOR` set, and the
//! question is asked of a terminal, which no agent shell tool has.
//!
//! The terminal check is the barrier that survives a missing `SHPRD_ACTOR`:
//! a pane restored without its environment still has no TTY. Only a debug
//! build may answer for the record with `--yes`, which is how the end-to-end
//! tests drive this verb; a release binary always asks.

use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;

use anyhow::anyhow;
use shprd_core::event::EventKind;
use shprd_git::git::Git;
use shprd_git::integrate;

use crate::cli::ApproveArgs;
use crate::cmd::land;
use crate::ctx::{CliError, Ctx, resolve_layout};

/// The variable naming who acts, as `ctx` reads it.
///
/// Repeated here because this verb refuses on the variable itself: an agent
/// pane sets it, so its presence is what makes a pane not a human.
const ACTOR_ENV: &str = "SHPRD_ACTOR";

/// Approves the run's pull request at the integration head a human is shown.
pub fn approve_verb(ctx: &Ctx, args: &ApproveArgs, home: Option<&Path>) -> Result<(), CliError> {
    require_human()?;
    let config = land::load_config(ctx, home)?;
    let state = ctx.state()?;
    let run = state.run.as_ref().ok_or_else(|| {
        CliError::Invalid(anyhow!(
            "no run yet: start one with `shprd run start` before approving"
        ))
    })?;
    let pr = state.pr.as_ref().ok_or_else(|| {
        CliError::Invalid(anyhow!(
            "no pull request yet: run `shprd land` first, then approve it"
        ))
    })?;
    let number = pr.number;
    let url = pr.url.clone();
    let run_id = run.run_id.clone();
    let base_branch = run.base_branch.clone();
    let layout = resolve_layout(home, &config.janitor.namespace)?;
    let repo_name = land::repo_name(ctx)?;
    let worktree =
        integrate::open_integration(&ctx.root, &layout, &repo_name, &run_id, &base_branch)
            .map_err(|err| {
                CliError::other(anyhow!(err).context("opening the integration worktree"))
            })?;
    let head = Git::new(&worktree.path)
        .head_sha()
        .map_err(|err| CliError::other(anyhow!(err).context("reading the integration head")))?;
    if !may_skip_tty(args.yes)
        && !asked(&format!("approve {url} at {}? [y/N] ", land::short(&head)))?
    {
        return Err(CliError::Invalid(anyhow!(
            "not approved: {url} still waits for one"
        )));
    }
    let event = ctx.commit(EventKind::Approved {
        number,
        head_sha: head.clone(),
    })?;
    crate::cmd::emit(ctx, &event)?;
    if !ctx.json {
        println!("approved {url} at {}", land::short(&head));
    }
    Ok(())
}

/// Whether the terminal may go unasked, which only a debug build allows.
///
/// `--yes` answers the question on the human's behalf, so it needs no
/// terminal, but it is a test affordance: a release build ignores it and
/// always asks. That keeps `--yes` unable to turn a pane into a human, and
/// leaves the TTY check as the barrier that holds when `SHPRD_ACTOR` is
/// missing.
fn may_skip_tty(yes: bool) -> bool {
    yes && cfg!(debug_assertions)
}

/// Refuses unless a human is the one acting.
///
/// `SHPRD_ACTOR` names the actor, so an agent pane fails here whatever it
/// passes; this is the check that does not depend on the terminal.
fn require_human() -> Result<(), CliError> {
    let named = std::env::var(ACTOR_ENV).unwrap_or_default();
    let named = named.trim();
    if !named.is_empty() && named != "human" {
        return Err(CliError::Invalid(anyhow!(
            "only a human approves a pull request, and {ACTOR_ENV}={named} is not one: \
             ask the maintainer to run `shprd approve`"
        )));
    }
    Ok(())
}

/// Asks the question on the terminal and reads the answer.
///
/// Without a terminal there is nobody to ask, so the answer would be a guess:
/// that is a refusal, not a default.
fn asked(question: &str) -> Result<bool, CliError> {
    if !std::io::stdin().is_terminal() {
        return Err(CliError::Invalid(anyhow!(
            "approving asks a human at a terminal, but stdin is not a terminal: \
             run `shprd approve` yourself in yours"
        )));
    }
    print!("{question}");
    std::io::stdout()
        .flush()
        .map_err(|err| CliError::other(anyhow!(err).context("writing the question")))?;
    let mut answer = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut answer)
        .map_err(|err| CliError::other(anyhow!(err).context("reading the answer")))?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes" | "YES" | "Yes"))
}

#[cfg(test)]
mod tests {
    use super::may_skip_tty;

    /// The terminal check, from the inside: only a debug build may skip it.
    #[test]
    fn only_a_debug_build_may_skip_the_terminal() {
        assert!(
            !may_skip_tty(false),
            "without --yes the question is always asked"
        );
        assert_eq!(
            may_skip_tty(true),
            cfg!(debug_assertions),
            "a release build ignores --yes and still asks"
        );
    }

    /// The same statement the way a release build reads it, so the assertion
    /// above cannot be satisfied by a `true` that only tests provide.
    #[cfg(not(debug_assertions))]
    #[test]
    fn a_release_build_cannot_be_told_to_skip_the_terminal() {
        assert!(!may_skip_tty(true), "--yes does not bypass the TTY check");
    }

    /// A debug build does honour `--yes`, which is what `tests/land.rs` uses.
    #[cfg(debug_assertions)]
    #[test]
    fn a_debug_build_honours_yes_for_the_tests() {
        assert!(may_skip_tty(true));
    }
}
