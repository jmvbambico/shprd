//! `shprd agent propose <verb> <task> --reason <TEXT>`: an agent asks the
//! person to run one of the human-only verbs.
//!
//! `drop`, `reopen`, `sync`, `override` and `readmit` are the person's: some
//! need a terminal, all refuse a non-human actor. This verb raises one
//! needs-you card on the task and runs nothing. The card's `confirm …` choice,
//! answered by the person on the board, is what the service turns into the
//! verb's event, written as the person; `leave it` writes nothing.
//!
//! The card's shape is a contract with the service's answer step
//! (`shprd-server`'s `actions.rs`, which cannot see this crate): the id is
//! `propose-<verb>-<n>`, the first option starts with `confirm `, and the text
//! carries the reason after a `reason: ` line.

use anyhow::anyhow;
use shprd_core::event::EventKind;

use crate::cli::{ProposeArgs, ProposedVerb};
use crate::ctx::{CliError, Ctx};

/// What every proposal card's id begins with.
pub const PROPOSE_ASK_PREFIX: &str = "propose-";

/// The choice that declines a proposal.
pub const LEAVE_IT_CHOICE: &str = "leave it";

/// The verb's own word, as the id and the choice spell it.
pub fn verb_word(verb: ProposedVerb) -> &'static str {
    match verb {
        ProposedVerb::Drop => "drop",
        ProposedVerb::Reopen => "reopen",
        ProposedVerb::Sync => "sync",
        ProposedVerb::Override => "override",
        ProposedVerb::Readmit => "readmit",
    }
}

/// The choice that confirms: `confirm drop`, `confirm override <rule>`, …
pub fn confirm_choice(verb: ProposedVerb, subject: &str) -> String {
    match subject.is_empty() {
        true => format!("confirm {}", verb_word(verb)),
        false => format!("confirm {} {subject}", verb_word(verb)),
    }
}

/// What the card says: the task, the verb and the reason.
pub fn body(task: &str, verb: ProposedVerb, subject: &str, reason: &str) -> String {
    let what = match subject.is_empty() {
        true => verb_word(verb).to_string(),
        false => format!("{} {subject}", verb_word(verb)),
    };
    format!("{task}: run `{what}`?\nreason: {}", reason.trim())
}

/// Why a proposal of `rule` cannot be confirmed as written, or `None`.
///
/// A scope override covers only the paths its reason lists, so a proposal
/// without them would be confirmed into an override that lets nothing through.
pub fn scope_proposal_problem(rule: &str, reason: &str) -> Option<String> {
    (rule == shprd_core::rules::ACCEPT_OUTSIDE_SCOPE
        && shprd_core::rules::scope_override_paths(reason).is_empty())
    .then(|| {
        format!(
            "{rule} needs the paths it lets through: start `--reason` with \
             `paths=src/a.rs,src/b; why`"
        )
    })
}

/// Raises the card that proposes `verb` on a task.
pub fn propose_verb(ctx: &Ctx, args: &ProposeArgs) -> Result<(), CliError> {
    let state = ctx.state()?;
    if !state.tasks.contains_key(&args.task) {
        return Err(CliError::Invalid(anyhow!(
            "no task {:?} in this run: name one `shprd status` lists",
            args.task
        )));
    }
    if args.reason.trim().is_empty() {
        return Err(CliError::Invalid(anyhow!(
            "`--reason` is empty: tell the person why you propose it"
        )));
    }
    let subject = match args.verb {
        ProposedVerb::Override => {
            let rule = args.rule.as_deref().unwrap_or_default().trim();
            if !shprd_core::rules::is_override_rule(rule) {
                return Err(CliError::Invalid(anyhow!(
                    "an override proposal names a rule with `--rule`: one of {}",
                    shprd_core::rules::OVERRIDE_RULES.join(", ")
                )));
            }
            if let Some(why) = scope_proposal_problem(rule, &args.reason) {
                return Err(CliError::Invalid(anyhow!(why)));
            }
            rule.to_string()
        }
        ProposedVerb::Readmit => {
            let named = args.agent.as_deref().unwrap_or_default().trim();
            if named.is_empty() {
                return Err(CliError::Invalid(anyhow!(
                    "a readmit proposal names the agent with `--agent`"
                )));
            }
            crate::cmd::readmit::stored_agent(&state, named).ok_or_else(|| {
                CliError::Invalid(anyhow!(
                    "{named} is not known to this run: nothing here marked it dry"
                ))
            })?
        }
        _ => String::new(),
    };
    let word = verb_word(args.verb);
    let earlier = state
        .asks
        .values()
        .filter(|ask| {
            ask.task == args.task && ask.id.starts_with(&format!("{PROPOSE_ASK_PREFIX}{word}-"))
        })
        .count();
    let id = format!("{PROPOSE_ASK_PREFIX}{word}-{}", earlier + 1);
    let mut text = body(&args.task, args.verb, &subject, &args.reason);
    if subject == shprd_core::rules::ACCEPT_OUTSIDE_SCOPE
        && crate::cmd::override_rule::has_high_risk_path(
            &args.reason,
            &shprd_core::ask::high_risk_globs_in(&ctx.root),
        )
    {
        text.push_str(&format!(
            "\nhigh-risk paths: confirming needs the task id `{}` typed back",
            args.task
        ));
    }
    let options = vec![
        confirm_choice(args.verb, &subject),
        LEAVE_IT_CHOICE.to_string(),
    ];
    let now_ms = shprd_core::log::Clock::now_ms(&shprd_core::log::SystemClock);
    let ask = shprd_core::ask::Ask::new(
        id.clone(),
        args.task.clone(),
        shprd_core::ask::AskKind::Question,
        text.clone(),
        now_ms,
    )
    .with_options(options.clone());
    let queue = shprd_core::ask::AskQueue::for_repo(&ctx.root);
    queue
        .raise(&ask)
        .map_err(|err| CliError::other(anyhow!(err).context("writing the ask")))?;
    let raised = EventKind::AskRaised {
        task: args.task.clone(),
        ask: id.clone(),
        ask_kind: shprd_core::ask::AskKind::Question,
        text,
        options,
        created_at_ms: now_ms,
        expires_at_ms: None,
    };
    let event = match ctx.commit(raised) {
        Ok(event) => event,
        Err(err) => {
            let _ = queue.remove(&args.task, &id);
            return Err(err);
        }
    };
    crate::cmd::emit(ctx, &event)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_card_names_the_verb_and_the_reason() {
        assert_eq!(confirm_choice(ProposedVerb::Drop, ""), "confirm drop");
        assert_eq!(
            confirm_choice(ProposedVerb::Override, "integrate_after_failed_gate"),
            "confirm override integrate_after_failed_gate"
        );
        let text = body("T1", ProposedVerb::Drop, "", " stuck ");
        assert!(text.ends_with("\nreason: stuck"), "{text}");
    }

    #[test]
    fn a_scope_proposal_must_list_its_paths() {
        let rule = shprd_core::rules::ACCEPT_OUTSIDE_SCOPE;
        assert!(scope_proposal_problem(rule, "needed it").is_some());
        assert!(scope_proposal_problem(rule, "paths=src/a.rs; needed").is_none());
        assert!(scope_proposal_problem("integrate_after_failed_gate", "x").is_none());
    }
}
