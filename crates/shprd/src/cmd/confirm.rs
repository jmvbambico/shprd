//! Asking a human at a terminal, for the verbs only a human may run.
//!
//! `SHPRD_ACTOR` names who acts, but it is an environment variable, so a pane
//! can claim to be a human by setting it. The terminal is the barrier that
//! holds anyway: an agent's shell tool has no TTY, so a question asked of the
//! terminal is a question only a person can answer. `shprd approve` and the
//! abandon of `shprd down` are both this kind of verb, and both ask here.
//!
//! Only a debug build may answer for the record with `--yes`, which is how the
//! end-to-end tests drive these verbs; a release binary always asks.

use std::io::{BufRead, IsTerminal, Write};

use anyhow::anyhow;

use crate::ctx::CliError;

/// The variable naming who acts, as `ctx` reads it.
///
/// Repeated here because these verbs refuse on the variable itself: an agent
/// pane sets it, so its presence is what makes a pane not a human.
const ACTOR_ENV: &str = "SHPRD_ACTOR";

/// Whether the terminal may go unasked, which only a debug build allows.
///
/// `--yes` answers the question on the human's behalf, so it needs no
/// terminal, but it is a test affordance: a release build ignores it and
/// always asks. That keeps `--yes` unable to turn a pane into a human, and
/// leaves the TTY check as the barrier that holds when `SHPRD_ACTOR` is
/// missing.
pub fn may_skip_tty(yes: bool) -> bool {
    yes && cfg!(debug_assertions)
}

/// Refuses unless a human is the one acting.
///
/// `act` completes "only a human ..." (`approves a pull request`), and
/// `advice` says what the reader should do instead. `SHPRD_ACTOR` names the
/// actor, so an agent pane fails here whatever it passes; this is the check
/// that does not depend on the terminal.
pub fn require_human(act: &str, advice: &str) -> Result<(), CliError> {
    let named = std::env::var(ACTOR_ENV).unwrap_or_default();
    let named = named.trim();
    if !named.is_empty() && named != "human" {
        return Err(CliError::Invalid(anyhow!(
            "only a human {act}, and {ACTOR_ENV}={named} is not one: {advice}"
        )));
    }
    Ok(())
}

/// Asks the question on the terminal and reads the answer.
///
/// `verb` is the command the refusal names, e.g. `shprd approve`: a reader who
/// hit it has to be told where the question is answerable. Without a terminal
/// there is nobody to ask, so the answer would be a guess: that is a refusal,
/// not a default.
pub fn ask_human(verb: &str, question: &str) -> Result<bool, CliError> {
    if !std::io::stdin().is_terminal() {
        return Err(CliError::Invalid(anyhow!(
            "{verb} asks a human at a terminal, but stdin is not a terminal: \
             run `{verb}` yourself in yours"
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

/// The question, asked unless this build may skip it, reduced to yes or no.
///
/// The answer is all the caller gets: what a 'no' means is the caller's
/// decision, and a skipped question counts as a yes, not as a guess.
pub fn answered(yes: bool, verb: &str, question: &str) -> Result<bool, CliError> {
    if may_skip_tty(yes) {
        return Ok(true);
    }
    ask_human(verb, question)
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
