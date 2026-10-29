//! `shprd sync`: bring the base branch into the run's integration branch.
//!
//! A long run outlives the base: a pull request merges while tasks are still
//! being made, the integration branch falls behind, and landing either
//! conflicts or proposes a merge of work the base already has. This verb closes
//! that gap. It merges the base into the run's own branch — never the base, and
//! never a rebase — so a task's commits keep the ids the pull request body
//! lists.
//!
//! A human and the orchestrator both run it, so it needs no terminal: nothing
//! outside the run's worktree is written. A merge that conflicts is undone and
//! escalated, because a half-merged worktree would be a trap for the next verb
//! that reads the branch.

use std::path::Path;

use anyhow::anyhow;
use shprd_core::event::{Actor, EventKind};
use shprd_core::rules::RuleError;
use shprd_git::integrate;
use shprd_git::sync::{self, SyncError, SyncOutcome};

use crate::cmd::land::short;
use crate::ctx::{CliError, Ctx, resolve_home, resolve_layout};

/// Merge the base branch into the integration branch.
pub fn sync_verb(ctx: &Ctx, home: Option<&Path>) -> Result<(), CliError> {
    let state = ctx.state()?;
    let run = state.run.as_ref().ok_or_else(|| {
        CliError::Invalid(anyhow!(
            "no run yet: start one with `shprd run start` before syncing the base"
        ))
    })?;
    if state.finished {
        return Err(CliError::Invalid(anyhow!(
            "the run {} has finished: there is nothing left to sync",
            run.run_id
        )));
    }
    let run_id = run.run_id.clone();
    let base = run.base_branch.clone();
    // Refused before any git runs: an agent may not move the head the gate and
    // the pull request are pinned to.
    if let Actor::Agent { .. } = &ctx.actor {
        return Err(CliError::Refused(RuleError::SyncNotAllowed {
            actor: ctx.actor.clone(),
        }));
    }
    let config = crate::cmd::land::load_config(ctx, home)?;
    let home = resolve_home(home)?;
    let layout = resolve_layout(Some(&home), &config.janitor.namespace)?;
    let repo_name = crate::cmd::land::repo_name(ctx)?;
    let worktree = integrate::open_integration(&ctx.root, &layout, &repo_name, &run_id, &base)
        .map_err(|err| CliError::other(anyhow!(err).context("opening the integration worktree")))?;
    let branch = worktree.branch.clone();
    match sync::sync_base(&worktree, &base) {
        Ok(SyncOutcome::UpToDate { head }) => {
            if !ctx.json {
                println!("{branch} is up to date with {base} at {}", short(&head));
            }
            Ok(())
        }
        Ok(SyncOutcome::Merged { base_sha, int_sha }) => {
            let event = ctx.commit(EventKind::BaseSynced {
                base_sha,
                int_sha: int_sha.clone(),
            })?;
            crate::cmd::emit(ctx, &event)?;
            if !ctx.json {
                println!("synced {branch} with {base} at {}", short(&int_sha));
            }
            Ok(())
        }
        Err(SyncError::Conflict {
            base: conflict_base,
            branch: conflict_branch,
            files,
            path,
        }) => Err(escalate(
            ctx,
            &conflict_base,
            &conflict_branch,
            &files,
            &path,
        )),
        Err(SyncError::Dirty { path }) => Err(CliError::Invalid(anyhow!(
            "the integration worktree at {} has uncommitted changes: commit or discard them, \
             then run `shprd sync` again",
            path.display()
        ))),
        Err(SyncError::NotRestored { expected }) => Err(CliError::other(anyhow!(
            "the conflicted merge could not be undone: the integration worktree is not back at \
             {expected}, so inspect it before anything else runs"
        ))),
        Err(SyncError::Git(err)) => Err(CliError::other(
            anyhow!(err).context("merging the base into the integration branch"),
        )),
    }
}

/// Records the conflict, names the files, and says what a human can do.
///
/// Nothing of the merge is left in the worktree, so the escalation is the only
/// record that the base and the run disagree, and the returned error is what
/// exits 2.
fn escalate(ctx: &Ctx, base: &str, branch: &str, files: &[String], path: &Path) -> CliError {
    let advice = format!(
        "resolve by adding a task scoped to those files, or merge by hand in {}",
        path.display()
    );
    let body = format!(
        "syncing {base} into {branch} conflicted in {} file(s): {}",
        files.len(),
        files.join(", ")
    );
    let kind = EventKind::Escalated {
        task: None,
        body: body.clone(),
        default_action: advice.clone(),
    };
    if let Err(err) = ctx.commit(kind) {
        eprintln!("warning: could not record the escalation: {err}");
    }
    CliError::Invalid(anyhow!("{body}: {advice}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use shprd_core::event::{Event, EventKind};
    use shprd_core::state::RunState;

    /// The log of a run that started and integrated one task.
    fn started() -> RunState {
        let events = vec![Event {
            seq: 1,
            at_ms: 1,
            actor: Actor::Human,
            kind: EventKind::RunStarted {
                run_id: "r1".to_string(),
                repo: "/repo".to_string(),
                base_branch: "main".to_string(),
                integration_branch: "integration/m1".to_string(),
            },
        }];
        match RunState::fold(&events) {
            Ok(state) => state,
            Err(err) => panic!("the fixture must fold: {err}"),
        }
    }

    #[test]
    fn an_agent_may_not_sync_the_base() {
        // The verb refuses before git runs, so an agent's attempt changes
        // nothing on disk; the rule refuses the event too.
        let state = started();
        let kind = EventKind::BaseSynced {
            base_sha: "abc1234".to_string(),
            int_sha: "def5678".to_string(),
        };
        let agent = Actor::Agent {
            id: "claude".to_string(),
        };
        match state.check_as(&agent, &kind, 2_000) {
            Err(err) => assert!(matches!(err, RuleError::SyncNotAllowed { .. }), "{err}"),
            Ok(()) => panic!("an agent must not sync the base"),
        }
    }
}
