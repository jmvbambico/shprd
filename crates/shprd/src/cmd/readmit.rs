//! `shprd readmit`: a human clears an agent's dry mark, in shprd.
//!
//! A dry mark has two writers: the watchdog, which puts an agent on cooldown
//! when its process exits or its provider runs out of quota, and this verb,
//! for the same reason `shprd approve` exists. The watchdog clears the mark
//! itself once a reset time passes, so an agent dried with no reset time would
//! otherwise wait forever: this is the human's way out, and the log says who
//! took it.
//!
//! Unlike `approve` there is no terminal check. Re-admitting is an operational
//! decision the maintainer makes in whatever shell is handy, not a security
//! gate, so `SHPRD_ACTOR` naming a human is the whole condition.

use anyhow::anyhow;
use shprd_core::event::{Actor, AgentId, EventKind};
use shprd_core::state::RunState;

use crate::cli::ReadmitArgs;
use crate::ctx::{CliError, Ctx};

/// The variable naming who acts, repeated here because the refusal names it.
const ACTOR_ENV: &str = "SHPRD_ACTOR";

/// Records that `agent` is usable again, which the run's rules only allow a
/// human or the engine to do.
pub fn readmit_verb(ctx: &Ctx, args: &ReadmitArgs) -> Result<(), CliError> {
    require_human(&ctx.actor)?;
    let state = ctx.state()?;
    let agent = stored_agent(&state, &args.agent).ok_or_else(|| {
        CliError::Invalid(anyhow!(
            "{} is not known to this run: nothing here marked it dry",
            args.agent
        ))
    })?;
    let event = ctx.commit(EventKind::Available {
        agent: agent.clone(),
        reason: args.reason.clone(),
    })?;
    crate::cmd::emit(ctx, &event)?;
    if !ctx.json {
        println!("re-admitted {agent}: {}", args.reason);
    }
    Ok(())
}

/// Refuses unless the actor is a human, whatever the terminal is.
///
/// An agent pane always has `SHPRD_ACTOR` set, so this is what keeps a dry
/// mark from being cleared by whoever it is holding back.
fn require_human(actor: &Actor) -> Result<(), CliError> {
    if matches!(actor, Actor::Human) {
        return Ok(());
    }
    Err(CliError::Invalid(anyhow!(
        "only a human re-admits an agent, and {actor:?} is not one: \
         the maintainer runs `shprd readmit <agent>` (and {ACTOR_ENV} names the actor)"
    )))
}

/// The id the log stores for the agent the human named.
///
/// A `Dry` event is keyed by the program, the harness spec's id (`pi`), while
/// the roster spells a worker `program/model`. Both name the same agent, so
/// both are accepted: the spelling the log holds wins, and a dry one is
/// preferred over a spelling that is known but already available, which lets
/// the rules say "not dry" about the right agent.
fn stored_agent(state: &RunState, named: &str) -> Option<AgentId> {
    let mut candidates: Vec<&str> = vec![named];
    if let Some((program, model)) = named.split_once('/')
        && !program.is_empty()
        && !model.is_empty()
    {
        candidates.push(program);
    }
    if let Some(dry) = candidates
        .iter()
        .find(|candidate| state.agents.get(**candidate).is_some_and(|info| info.dry))
    {
        return Some((*dry).to_string());
    }
    candidates
        .iter()
        .find(|candidate| state.agents.contains_key(**candidate))
        .map(|found| (*found).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use shprd_core::event::Event;

    fn dried_state(agent: &str) -> RunState {
        let events = vec![
            Event {
                seq: 1,
                at_ms: 1,
                actor: Actor::Human,
                kind: EventKind::RunStarted {
                    run_id: "r1".to_string(),
                    repo: "/repo".to_string(),
                    base_branch: "main".to_string(),
                    integration_branch: "integration/m1".to_string(),
                },
            },
            Event {
                seq: 2,
                at_ms: 2,
                actor: Actor::Engine,
                kind: EventKind::Dry {
                    agent: agent.to_string(),
                    reason: "agent process exited".to_string(),
                    reset_at_ms: None,
                },
            },
        ];
        match RunState::fold(&events) {
            Ok(folded) => folded,
            Err(err) => panic!("the fixture must fold: {err}"),
        }
    }

    #[test]
    fn the_program_id_names_the_agent_a_dry_mark_stores() {
        let state = dried_state("pi");
        assert_eq!(stored_agent(&state, "pi").as_deref(), Some("pi"));
        assert_eq!(
            stored_agent(&state, "pi/Command Code/deepseek").as_deref(),
            Some("pi"),
            "a roster spelling maps to the program the log keys"
        );
    }

    #[test]
    fn an_agent_the_log_never_mentions_has_no_id() {
        assert_eq!(stored_agent(&RunState::default(), "pi"), None);
    }

    #[test]
    fn only_a_human_re_admits() {
        assert!(require_human(&Actor::Human).is_ok());
        for actor in [
            Actor::Engine,
            Actor::Orchestrator,
            Actor::Agent {
                id: "pi".to_string(),
            },
        ] {
            match require_human(&actor) {
                Err(err) => assert!(err.to_string().contains("only a human"), "{err}"),
                Ok(()) => panic!("{actor:?} must not re-admit an agent"),
            }
        }
    }
}
