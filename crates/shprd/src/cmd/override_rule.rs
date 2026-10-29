//! `shprd override`: a human sets a rule aside for a task, in shprd.
//!
//! One rule honours an override today: `integrate_after_failed_gate`, which
//! lets a task integrate although one of its gates failed, a review included.
//! That is a decision about work a reviewer rejected, so it is the human's and
//! nobody else's. An orchestrator or a worker that recorded the override
//! itself could push through exactly the work a review stopped, which is the
//! hole this verb closes.
//!
//! Two barriers hold, the same pair `shprd approve` and `shprd down --abandon`
//! use. `SHPRD_ACTOR` is an environment variable, so an agent pane always has
//! it set and [`confirm::require_human`] refuses on the name itself; a pane
//! restored without it still has no TTY, so the question is one only a person
//! can answer. Only a debug build may answer for the record with `--yes`,
//! which is how the end-to-end tests drive this verb.
//!
//! The question comes before every side effect: it names the task, the rule,
//! and the gate or review that failed, because an override is a decision about
//! that failure and not about a rule in the abstract.

use anyhow::anyhow;
use shprd_core::event::EventKind;
use shprd_core::rules::{INTEGRATE_AFTER_FAILED_GATE, OVERRIDE_RULES, is_override_rule};
use shprd_core::state::RunState;

use crate::cli::OverrideArgs;
use crate::cmd::confirm;
use crate::ctx::{CliError, Ctx};

/// Records a human's override of `rule`, after asking at the terminal.
pub fn override_verb(ctx: &Ctx, args: &OverrideArgs) -> Result<(), CliError> {
    confirm::require_human(
        &confirm::named_actor(),
        "overrides a rule",
        "ask the maintainer to run `shprd override`",
    )?;
    let rule = args.rule.trim().to_string();
    if !is_override_rule(&rule) {
        return Err(CliError::Invalid(anyhow!(
            "only a rule the run reads can be overridden, and {rule:?} is not one: \
             the rules an override can name are {}",
            OVERRIDE_RULES.join(", ")
        )));
    }
    let task = args.task.as_deref().unwrap_or_default().trim().to_string();
    let state = ctx.state()?;
    let question = question(&state, &task, &rule, &args.reason)?;
    if !confirm::answered(args.yes, "shprd override", &question)? {
        return Err(CliError::Invalid(anyhow!(
            "not overridden: nothing was recorded, so nothing about this run changed"
        )));
    }
    let event = ctx.commit(EventKind::Override {
        task: task.clone(),
        rule: rule.clone(),
        reason: args.reason.clone(),
    })?;
    crate::cmd::emit(ctx, &event)?;
    if !ctx.json {
        let subject = if task.is_empty() {
            "the run".to_string()
        } else {
            format!("task {task}")
        };
        println!("overrode {rule} for {subject}: {}", args.reason);
    }
    Ok(())
}

/// The question the human answers, naming what the override lets happen.
///
/// It reads the folded state rather than trusting the command line: the gate
/// it names is the one the log says failed, and the finding beside it is that
/// gate's own, so a human who is asked twice about two different failures can
/// tell them apart.
pub fn question(
    state: &RunState,
    task: &str,
    rule: &str,
    reason: &str,
) -> Result<String, CliError> {
    if task.is_empty() {
        return Ok(format!(
            "override `{rule}` for the whole run ({reason})? [y/N] "
        ));
    }
    let found = state.tasks.get(task).ok_or_else(|| {
        CliError::Invalid(anyhow!(
            "task {task} is not in this run's plan: `shprd status` lists the tasks there are"
        ))
    })?;
    Ok(format!("let {}? [y/N] ", what_it_allows(found, rule)))
}

/// What the overridden rule lets `task` do, in one clause.
fn what_it_allows(task: &shprd_core::state::Task, rule: &str) -> String {
    if rule != INTEGRATE_AFTER_FAILED_GATE {
        // One rule honours an override today, but the sentence is built from
        // the rule so a second one reads as a question and not as a guess.
        return format!("{} override `{rule}`", task.id);
    }
    let failed = task
        .attempts
        .last()
        .and_then(|attempt| attempt.gates.iter().rev().find(|gate| !gate.passed));
    match failed {
        Some(gate) => format!(
            "{} integrate although its {}{}",
            task.id,
            gate_words(gate.gate.as_str()),
            finding_words(gate)
        ),
        None => format!(
            "{} integrate although no gate of its latest attempt failed",
            task.id
        ),
    }
}

/// The gate, in words: the review gate is "review", a check is "lint gate".
fn gate_words(gate: &str) -> String {
    if gate == shprd_core::gate::REVIEW_GATE {
        "review failed".to_string()
    } else {
        format!("{gate} gate failed")
    }
}

/// The first finding of `gate`, as ` (major: clippy warning)`, or nothing.
fn finding_words(gate: &shprd_core::state::GateResult) -> String {
    match gate.findings.first() {
        Some(finding) => format!(
            " ({:?}: {})",
            finding.severity,
            finding.title.trim().to_lowercase()
        ),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use shprd_core::event::{Actor, Event, EventKind};
    use shprd_core::state::RunState;

    use super::question;

    /// A task whose only attempt failed its `gate` with one major finding.
    fn state_with_failed_gate(gate: &str) -> RunState {
        let mut events = vec![
            event(
                1,
                EventKind::RunStarted {
                    run_id: "r1".to_string(),
                    repo: "/repo".to_string(),
                    base_branch: "main".to_string(),
                    integration_branch: "integration/m1".to_string(),
                },
            ),
            event(
                2,
                EventKind::TaskAdded {
                    task: "janitor-reviews".to_string(),
                    title: "janitor".to_string(),
                    scope: vec!["crates/janitor".to_string()],
                    acceptance: vec!["it works".to_string()],
                    tags: Vec::new(),
                    skills: Vec::new(),
                    deps: Vec::new(),
                },
            ),
            event(
                3,
                EventKind::Spawned {
                    task: "janitor-reviews".to_string(),
                    attempt: 1,
                    agent: "w1".to_string(),
                    model: None,
                    worktree: "/wt".to_string(),
                    branch: "feature/x".to_string(),
                },
            ),
            event(
                4,
                EventKind::Reported {
                    task: "janitor-reviews".to_string(),
                    attempt: 1,
                    status: shprd_core::event::ReportStatus::Completed,
                    summary: "done".to_string(),
                    tests: Vec::new(),
                    changed_paths: vec!["crates/janitor/x.rs".to_string()],
                    commit: Some("abc123".to_string()),
                    skills_used: Vec::new(),
                },
            ),
            event(
                5,
                EventKind::GateFailed {
                    task: "janitor-reviews".to_string(),
                    attempt: 1,
                    gate: gate.to_string(),
                    findings: vec![shprd_core::event::Finding {
                        severity: shprd_core::event::Severity::Major,
                        title: "Clippy warning".to_string(),
                        location: None,
                        detail: "unwrap used".to_string(),
                    }],
                    source: None,
                },
            ),
        ];
        // The verdict that follows is not what this test reads.
        events.truncate(5);
        match RunState::fold(&events) {
            Ok(state) => state,
            Err(err) => panic!("fold failed: {err}"),
        }
    }

    fn event(seq: u64, kind: EventKind) -> Event {
        Event {
            seq,
            at_ms: seq * 1000,
            actor: Actor::Engine,
            kind,
        }
    }

    #[test]
    fn the_question_names_the_task_the_gate_and_the_finding() {
        let state = state_with_failed_gate(shprd_core::gate::REVIEW_GATE);
        let asked = match question(
            &state,
            "janitor-reviews",
            shprd_core::rules::INTEGRATE_AFTER_FAILED_GATE,
            "the finding is cosmetic",
        ) {
            Ok(asked) => asked,
            Err(err) => panic!("question failed: {err}"),
        };
        assert_eq!(
            asked,
            "let janitor-reviews integrate although its review failed (Major: clippy warning)? [y/N] "
        );
    }

    #[test]
    fn a_check_gate_is_named_as_a_gate() {
        let state = state_with_failed_gate("clippy");
        let asked = question(
            &state,
            "janitor-reviews",
            shprd_core::rules::INTEGRATE_AFTER_FAILED_GATE,
            "cosmetic",
        )
        .unwrap_or_default();
        assert!(
            asked.starts_with("let janitor-reviews integrate although its clippy gate failed"),
            "{asked}"
        );
    }

    #[test]
    fn an_unknown_task_is_refused_before_anything_is_asked() {
        let state = state_with_failed_gate("clippy");
        let err = question(
            &state,
            "nope",
            shprd_core::rules::INTEGRATE_AFTER_FAILED_GATE,
            "cosmetic",
        );
        assert!(
            err.is_err(),
            "an unknown task cannot be named in a question"
        );
    }
}
