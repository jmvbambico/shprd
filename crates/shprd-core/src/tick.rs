//! The tick: one pure pass that moves every task forward as far as it may.
//!
//! Shepherdr runs one pass every fifteen seconds and moves every task as far as
//! it legally can. This module is that pass for shprd, and it is a pure function
//! of what it is given: the folded run, the derived status of every task (the
//! eight words `shprd-views` derives), the config and a snapshot of the facts
//! that live outside the log — what the forge says about a task's pull request
//! and which tasks have an agent running right now. Nothing here touches a file,
//! a network or a clock, so the whole pass is tested with fixtures.
//!
//! The actions are deliberately few:
//!
//! - [`Step::Start`] a queued task whose `deps` have all landed, up to the
//!   configured concurrency cap ([`crate::config::Tick::max_running`]);
//! - [`Step::Review`] a task that handed in work which passed its own checks,
//!   when it has no review request for that attempt: the engine starts the
//!   reviewer, not the worker (see `docs/decisions.md`, 2026-11-14);
//! - [`Step::Verdict`] a task whose reviewer has written its findings and not
//!   decided them: the file is the reviewer's whole output, so the engine is
//!   what turns it into a gate result;
//! - [`Step::Land`] a task `shprd_core::rules::ready_to_land` accepts, when its
//!   merge policy allows it;
//! - [`Step::MarkLanded`] a task whose pull request merged outside shprd;
//! - [`Step::SendBack`] a task whose review failed while review rounds are
//!   left: a new attempt, through the same spawn path a person's
//!   `agent spawn --task` uses, which already carries the reviewer's findings
//!   into the new brief;
//! - [`Step::Escalate`] a task whose review rounds are spent: one card in front
//!   of a person, recorded even when the run has no coordinator;
//! - [`Step::NeedsYou`] for a task whose policy is `ask`, recorded once and not
//!   again on every pass.
//!
//! Hand-in and agent nudges are not the tick's business.
//!
//! Two properties hold and are tested: the same input always yields the same
//! actions, and a state that already recorded an action's effect yields none of
//! it again.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::config::{Config, MergePolicy};
use crate::event::{LandOutcome, ReportStatus};
use crate::gate::{self, Next};
use crate::rules;
use crate::state::{RunState, Task, TaskStatus};

/// The eight words a task's row shows, as the planner reads them.
///
/// It mirrors `shprd_views::derived::DerivedStatus`. It is a separate type
/// because `shprd-core` sits below `shprd-views`, so the projector cannot be a
/// dependency here; the service, which depends on both, maps one onto the other
/// and hands the planner this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Added, nothing started on it.
    ToDo,
    /// Starts by itself at a time.
    Scheduled,
    /// Waits for something before it may start.
    Queued,
    /// An agent is on it, or the engine is finishing its work.
    Working,
    /// Nothing moves without the person.
    NeedsYou,
    /// Its agent is at rest and said ready.
    Ready,
    /// Its merge is under way.
    Merging,
    /// Finished, or it never will be.
    Done,
}

impl Phase {
    /// The phase of a folded status, when the caller has no derived row.
    ///
    /// The fold knows less than the derived status — it cannot see a live
    /// terminal — so this is only the fallback for a caller that passes no
    /// derived row at all.
    pub fn of(status: TaskStatus) -> Self {
        match status {
            TaskStatus::Queued => Self::ToDo,
            TaskStatus::Working => Self::Working,
            TaskStatus::Reported | TaskStatus::Passed => Self::Ready,
            TaskStatus::Failed => Self::NeedsYou,
            TaskStatus::Integrated | TaskStatus::Dropped => Self::Done,
        }
    }
}

/// What the forge says about one task's pull request.
///
/// Every field is the forge's word: `checks` is `None` while they run or when
/// the forge cannot say, which the planner reads as "not known to pass".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrFacts {
    /// Whether the pull request is open.
    pub open: bool,
    /// Whether the forge has merged it.
    pub merged: bool,
    /// Whether its checks passed, when the forge reports a verdict.
    pub checks: Option<bool>,
    /// The commit the pull request's head branch is at.
    pub head: Option<String>,
    /// The forge's number for it, when known.
    pub number: Option<u64>,
}

/// The facts a pass needs that the log does not hold.
///
/// A fact that could not be fetched (the forge is down, a repository has no
/// remote) is simply absent: [`Facts::prs`] has no entry for that task and
/// [`Facts::live`] names no agent. The planner then takes no action that
/// depends on it, which is the difference between "not merged" and "not known".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Facts {
    /// What the forge says, by task id.
    pub prs: BTreeMap<String, PrFacts>,
    /// The tasks that have an agent running now.
    pub live: BTreeSet<String>,
    /// True when the repository has no remote or no forge, so a landing goes to
    /// the run's integration branch locally rather than through a pull request.
    ///
    /// The `checks` policy then reads the hand-in's own checks report, because a
    /// repository with no forge has no CI to ask.
    #[serde(default)]
    pub local: bool,
    /// The tasks whose reviewer's findings file has settled.
    ///
    /// A reviewer writes `findings.json` and stops; a file that is younger than
    /// [`crate::review::FINDINGS_SETTLE`] may still be being written, so it is not
    /// in this set and the pass decides nothing from it. The planner reads only
    /// this fact: reading and judging the file is the executor's work.
    #[serde(default)]
    pub settled_findings: BTreeSet<String>,
}

impl Facts {
    /// The pull request facts for `task`, if the forge could be asked.
    pub fn pr(&self, task: &str) -> Option<&PrFacts> {
        self.prs.get(task)
    }

    /// Whether `task` has an agent running right now.
    pub fn is_live(&self, task: &str) -> bool {
        self.live.contains(task)
    }
}

/// One thing a pass decided to do, and to which task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Step {
    /// Put a worker on a queued task whose dependencies have all landed.
    Start {
        /// The task to start.
        task: String,
    },
    /// Start the engine's reviewer on a task that just handed in.
    Review {
        /// The task to review.
        task: String,
    },
    /// Decide the review of a task whose reviewer has written its findings.
    Verdict {
        /// The task whose review is decided.
        task: String,
    },
    /// Land a task through its own pull request.
    Land {
        /// The task to land.
        task: String,
        /// The policy the landing runs under.
        policy: MergePolicy,
    },
    /// Record that a task's pull request was merged outside shprd.
    MarkLanded {
        /// The task whose work is in.
        task: String,
        /// What the forge reported.
        outcome: LandOutcome,
        /// The commit that landed, when the forge named it.
        head: Option<String>,
        /// The pull request that carried it, when known.
        number: Option<u64>,
    },
    /// Tell a human that a ready task waits on them.
    NeedsYou {
        /// The task that needs them.
        task: String,
        /// What needs deciding, in one line.
        body: String,
    },
    /// Send a task whose review failed back for another attempt.
    ///
    /// The attempt is started by the same spawn path a person's
    /// `shprd agent spawn --task` runs, so the new brief carries "why the
    /// previous attempt was sent back" from the failed review gate the log
    /// holds, and nothing about spawning is written twice.
    SendBack {
        /// The task to try again.
        task: String,
        /// The attempt that failed its review and that this replaces.
        attempt: u32,
    },
    /// Ask a person what to do with a task whose review rounds are spent.
    ///
    /// It is planned for a failed review whose card cannot be answered yet, so
    /// it appears under the service's tick alone, with no coordinator and no
    /// orchestrator to tell. The executor records exactly one card per failure,
    /// and its choices once, as an ask a board answers through the API.
    Escalate {
        /// The task that needs them.
        task: String,
        /// What failed, and the person's choices, in one line.
        body: String,
        /// The action taken if nobody answers.
        default_action: String,
    },
}

impl Step {
    /// The task this step is about.
    pub fn task(&self) -> &str {
        match self {
            Self::Start { task }
            | Self::Review { task }
            | Self::Verdict { task }
            | Self::Land { task, .. }
            | Self::MarkLanded { task, .. }
            | Self::NeedsYou { task, .. }
            | Self::SendBack { task, .. }
            | Self::Escalate { task, .. } => task,
        }
    }

    /// The step's name, as a result line spells it.
    pub fn action(&self) -> &'static str {
        match self {
            Self::Start { .. } => "start",
            Self::Review { .. } => "review",
            Self::Verdict { .. } => "verdict",
            Self::Land { .. } => "land",
            Self::MarkLanded { .. } => "mark_landed",
            Self::NeedsYou { .. } => "needs_you",
            Self::SendBack { .. } => "send_back",
            Self::Escalate { .. } => "escalate",
        }
    }
}

/// Everything one pass is planned from.
#[derive(Debug)]
pub struct Input<'a> {
    /// The folded run.
    pub state: &'a RunState,
    /// Every task's derived status, by id.
    pub phases: &'a BTreeMap<String, Phase>,
    /// The facts the log does not hold.
    pub facts: &'a Facts,
    /// The resolved config: the merge policy and the tick's own settings.
    pub config: &'a Config,
    /// Whether the engine reviews a task when it hands in.
    ///
    /// It comes from the settings: a reviewer role that may review and names a
    /// program to run it. With no reviewer configured there is nothing to
    /// start, so a pass plans no [`Step::Review`].
    pub review: bool,
}

/// Plans one pass: every action to take now, in task id order.
///
/// A finished run, or one that never started, gets nothing: there is nothing to
/// advance. Otherwise each task is asked, in id order, for the one thing that
/// moves it — so the answer never depends on the order the map happens to be
/// visited in.
pub fn plan(input: &Input<'_>) -> Vec<Step> {
    let state = input.state;
    if state.finished {
        return Vec::new();
    }
    let Some(run) = state.run.as_ref() else {
        return Vec::new();
    };
    // The branch a landing actually writes: a repository with no forge lands
    // inside the run's integration worktree, so that is the branch the policy
    // and the protected list are asked about. Asking about the base branch
    // instead would judge every local landing by `main` and turn `auto` into
    // `ask` for a branch nobody protects.
    let target = if input.facts.local {
        run.integration_branch.clone()
    } else {
        run.base_branch.clone()
    };
    let max_running = input.config.tick.max_running.max(1);
    let mut running = running_count(state, input.facts, input.phases);
    let mut steps = Vec::new();
    for (id, task) in &state.tasks {
        let phase = input
            .phases
            .get(id)
            .copied()
            .unwrap_or_else(|| Phase::of(task.status));
        if let Some(step) = step_for(input, task, phase, &target, &mut running, max_running) {
            steps.push(step);
        }
    }
    steps
}

/// The one thing that moves `task` forward now, if there is one.
///
/// The order matters: a merge the forge already made outranks landing it again,
/// landing outranks starting anything, and a task is only ever started when
/// nothing else it could do is left.
fn step_for(
    input: &Input<'_>,
    task: &Task,
    phase: Phase,
    landing_target: &str,
    running: &mut u32,
    max_running: u32,
) -> Option<Step> {
    let state = input.state;
    let facts = input.facts;
    let pr = facts.pr(&task.id);
    // Its work is already in, whoever merged it: record the fact. The log is
    // what every other rule reads, so a task left at `Passed` would keep
    // blocking its dependents for ever.
    if !matches!(task.status, TaskStatus::Integrated | TaskStatus::Dropped)
        && pr.is_some_and(|pr| pr.merged)
    {
        let pr = pr?;
        return Some(Step::MarkLanded {
            task: task.id.clone(),
            outcome: LandOutcome::AlreadyMerged,
            head: pr.head.clone(),
            number: pr
                .number
                .or_else(|| task.pr.as_ref().map(|known| known.number)),
        });
    }
    // The worker hands in; the engine reviews (decisions.md, 2026-11-14). A
    // hand-in that passed its own checks and has no request yet gets one, and
    // that recorded request is what stops the next pass asking again.
    if input.review && awaiting_review(task) {
        return Some(Step::Review {
            task: task.id.clone(),
        });
    }
    // A reviewer only describes what it found: the file it writes is turned into
    // a gate result by the engine, and a settled file nobody decided yet is that
    // work. The recorded gate is what stops the next pass deciding it again, and
    // a file already escalated as unreadable is a person's to look at.
    if awaiting_verdict(task).is_some()
        && facts.settled_findings.contains(&task.id)
        && !invalid_findings_escalated(state, task)
    {
        return Some(Step::Verdict {
            task: task.id.clone(),
        });
    }
    // A landing already looked at this attempt and found nothing of its own to
    // merge, so there is nothing left to do: the branch has no work beyond its
    // base, and landing it again would record the same fact every pass without
    // moving the task. The fold clears this when a new attempt starts, so only
    // that attempt is held back.
    let nothing_to_merge = task.nothing_to_merge.is_some();
    // Not while its agent has taken the work up again: a landing judges the
    // commit the branch is at, and an agent that is writing produces another.
    let busy = facts.is_live(&task.id) || matches!(phase, Phase::Working | Phase::Merging);
    if !busy && !nothing_to_merge && rules::ready_to_land(task).is_ok() {
        let policy = input.config.land.policy_for(landing_target);
        let land = Step::Land {
            task: task.id.clone(),
            policy,
        };
        return match policy {
            MergePolicy::Auto => Some(land),
            // The checks are the forge's to report: while they run, or while the
            // forge cannot say, nothing depends on them being green. A local
            // landing has no forge to ask, so its checks are the hand-in's own
            // report, which `ready_to_land` already read and accepted.
            MergePolicy::Checks if facts.local => Some(land),
            MergePolicy::Checks => (pr.and_then(|pr| pr.checks) == Some(true)).then_some(land),
            MergePolicy::Ask => {
                if approval_matches(task, pr) {
                    Some(land)
                } else if already_asked(state, task) {
                    // Said once already: a pass must not repeat itself.
                    None
                } else {
                    let body = if facts.local {
                        needs_you_body_local(&task.id)
                    } else {
                        needs_you_body(&task.id, pr.and_then(|pr| pr.number))
                    };
                    Some(Step::NeedsYou {
                        task: task.id.clone(),
                        body,
                    })
                }
            }
        };
    }
    // A review that failed is the loop's own work: another attempt while the
    // rounds it may use are left, and a person's card once they are spent. This
    // is what keeps a run from going quiet on a `Needs you` row nobody can act
    // on.
    if let Some(step) = after_failed_review(input, task, phase, running, max_running) {
        return Some(step);
    }
    // A queued task whose dependencies have all landed is the `--after` wait
    // releasing, and the concurrency cap is what keeps one repository from
    // running every task of the plan at once.
    if task.status != TaskStatus::Queued
        || !matches!(phase, Phase::ToDo | Phase::Queued)
        || !deps_landed(state, task)
        || *running >= max_running
    {
        return None;
    }
    *running += 1;
    Some(Step::Start {
        task: task.id.clone(),
    })
}

/// What a task whose review failed is owed now, if anything.
///
/// The two answers are exclusive: rounds left sends the task back to an agent,
/// and rounds spent asks a person. Neither is planned twice for one failure —
/// the send-back records the new attempt, which is the next pass's newest
/// attempt, and the card is recognised by the ask that carries its choices.
fn after_failed_review(
    input: &Input<'_>,
    task: &Task,
    phase: Phase,
    running: &mut u32,
    max_running: u32,
) -> Option<Step> {
    let state = input.state;
    if task.status != TaskStatus::Failed {
        return None;
    }
    gate::latest_failed_review(task)?;
    let attempt = task.latest_attempt()?.n;
    // The allowance is the configured one plus every round a person granted by
    // answering this task's card, so one answer buys one attempt.
    let allowed = gate::allowance(state, task, input.config.review.rounds);
    match gate::after_failure(task, allowed) {
        // A send-back is a spawn, so it takes a slot and an agent that is still
        // live on the task keeps it: the attempt it replaces must be finished.
        Next::SendBack => {
            if input.facts.is_live(&task.id) || matches!(phase, Phase::Working | Phase::Merging) {
                return None;
            }
            // A send-back that did not start is a person's to start: one card
            // saying so is what keeps a broken host from costing a spawn a
            // pass, every pass, with nobody told.
            let marker = gate::send_back_failure_prefix(&task.id, attempt);
            if already_told(state, &task.id, &marker) {
                return None;
            }
            if *running >= max_running {
                return None;
            }
            *running += 1;
            Some(Step::SendBack {
                task: task.id.clone(),
                attempt,
            })
        }
        // The card is recorded once — the verdict decision normally writes it,
        // and this covers a failure that reached the log any other way — and it
        // is planned until it can be answered: the choices are an ask, which is
        // what a board answers through the API.
        Next::Escalate => {
            let ask = gate::rounds_spent_ask(attempt);
            if state.asks.contains_key(&(task.id.clone(), ask)) {
                return None;
            }
            Some(Step::Escalate {
                task: task.id.clone(),
                body: gate::rounds_spent_body(task, attempt, gate::failed_review_rounds(task)),
                default_action: gate::ESCALATION_ACTION.to_string(),
            })
        }
    }
}

/// Whether an escalation for `task` whose body begins with `marker` is recorded.
fn already_told(state: &RunState, task: &str, marker: &str) -> bool {
    state.escalations.iter().any(|escalation| {
        escalation.task.as_deref() == Some(task) && escalation.body.starts_with(marker)
    })
}

/// How many tasks count as running: one per task an agent is live on, or whose
/// derived status is working or merging.
///
/// A task that reported and waits on its landing is not running an agent any
/// more, so it frees its slot; its worktree is still held, which is a different
/// limit shprd does not need yet.
fn running_count(state: &RunState, facts: &Facts, phases: &BTreeMap<String, Phase>) -> u32 {
    state
        .tasks
        .iter()
        .filter(|(id, task)| is_running(id, task, facts, phases))
        .count()
        .try_into()
        .unwrap_or(u32::MAX)
}

/// Whether one task counts as running.
fn is_running(id: &str, task: &Task, facts: &Facts, phases: &BTreeMap<String, Phase>) -> bool {
    facts.is_live(id)
        || matches!(phases.get(id), Some(Phase::Working | Phase::Merging))
        || task.status == TaskStatus::Working
}

/// Whether a task hands in work that passed its own checks and has no review
/// request for that attempt.
///
/// The check report is the worker's own word that it ran the project's gates and
/// they exited zero, which is what `rules::ready_to_land` also demands. A request
/// already in the log means a reviewer is out, so the pass leaves it alone; a
/// verdict moves the task out of `Reported`, so it is not reviewable either.
fn awaiting_review(task: &Task) -> bool {
    if task.status != TaskStatus::Reported {
        return false;
    }
    let Some(attempt) = task.latest_attempt() else {
        return false;
    };
    if !attempt.reviews.is_empty() {
        return false;
    }
    let Some(report) = &attempt.report else {
        return false;
    };
    report.status == ReportStatus::Completed
        && !report.checks.is_empty()
        && report.checks.iter().all(|check| check.exit_code == Some(0))
}

/// The attempt of `task` a review is still owed for, if there is one.
///
/// A review was asked for and no review gate result is recorded, so the task
/// sits in `Reported` until the reviewer's file is decided, however long the
/// reviewer takes. Every actor that decides a review asks this before it does —
/// the planner, the service's verdict step and the watchdog — so one review is
/// never decided twice, whichever of them notices the file first.
pub fn awaiting_verdict(task: &Task) -> Option<u32> {
    if task.status != TaskStatus::Reported {
        return None;
    }
    let attempt = task.latest_attempt()?;
    let decided = attempt
        .gates
        .iter()
        .any(|gate| gate.gate == crate::gate::REVIEW_GATE);
    (!attempt.reviews.is_empty() && !decided).then_some(attempt.n)
}

/// Whether an unreadable findings file for this task was already escalated.
///
/// The escalation is the record that a person has been told; the recovery is
/// theirs — they read the packet, or run `shprd agent review-verdict` once the
/// reviewer has rewritten a valid file.
fn invalid_findings_escalated(state: &RunState, task: &Task) -> bool {
    let marker = crate::review::invalid_findings_prefix(&task.id);
    state.escalations.iter().any(|escalation| {
        escalation.task.as_deref() == Some(task.id.as_str()) && escalation.body.starts_with(&marker)
    })
}

/// Whether every task `task` waits for has landed.
fn deps_landed(state: &RunState, task: &Task) -> bool {
    task.deps.iter().all(|dep| {
        state
            .tasks
            .get(dep)
            .is_some_and(|found| found.status == TaskStatus::Integrated)
    })
}

/// Whether a human's approval was given for the head the landing would push.
///
/// An approval names one commit: an approval of an older head says nothing
/// about the commits now on top of it. The head is the forge's word when it
/// could be asked, else the commit the hand-in named.
fn approval_matches(task: &Task, pr: Option<&PrFacts>) -> bool {
    let Some(approved) = task.approved_head.as_deref() else {
        return false;
    };
    let head = pr.and_then(|pr| pr.head.as_deref()).or_else(|| {
        task.latest_attempt()
            .and_then(|attempt| attempt.report.as_ref())
            .and_then(|report| report.commit.as_deref())
    });
    head == Some(approved)
}

/// Whether this task's `ask` was already recorded, so a pass says it once.
///
/// The comparison is on what every such message shares — "`<task>` is ready to
/// land" — rather than the whole body: the body names the pull request's number
/// when the forge has one, and a pull request a human opened between two passes
/// would otherwise turn one question into two.
fn already_asked(state: &RunState, task: &Task) -> bool {
    let marker = needs_you_prefix(&task.id);
    state.escalations.iter().any(|escalation| {
        escalation.task.as_deref() == Some(task.id.as_str()) && escalation.body.starts_with(&marker)
    })
}

/// What every `needs you` message for `task` begins with.
pub fn needs_you_prefix(task: &str) -> String {
    format!("{task} is ready to land")
}

/// What a human is told when a ready task's `ask` has no forge to approve on.
///
/// A local landing has no pull request, so the only approval path is shprd's
/// own `shprd approve <task>`: the message must not send the reader to a forge.
pub fn needs_you_body_local(task: &str) -> String {
    format!("{}: run `shprd approve {task}`", needs_you_prefix(task))
}

/// What a human is told when a ready task's policy is `ask`.
pub fn needs_you_body(task: &str, number: Option<u64>) -> String {
    let which = match number {
        Some(number) => format!("pull request #{number}"),
        None => String::from("its pull request"),
    };
    format!(
        "{} and its merge policy asks: approve {which} on the forge, or run `shprd approve {task}`",
        needs_you_prefix(task)
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::config::{Land, Tick};
    use crate::event::{Actor, CheckReport, Event, EventKind, ReportStatus, ReviewSource};
    use crate::state::RunState;

    fn ev(seq: u64, kind: EventKind) -> Event {
        Event {
            seq,
            at_ms: seq * 1000,
            actor: Actor::Engine,
            kind,
        }
    }

    fn started(seq: u64) -> Event {
        ev(
            seq,
            EventKind::RunStarted {
                run_id: "r1".to_string(),
                repo: "/repo".to_string(),
                base_branch: "main".to_string(),
                integration_branch: "integration/m8".to_string(),
            },
        )
    }

    fn added(seq: u64, id: &str, deps: &[&str]) -> Event {
        ev(
            seq,
            EventKind::TaskAdded {
                task: id.to_string(),
                title: format!("task {id}"),
                scope: vec![format!("src/{id}")],
                acceptance: vec!["it works".to_string()],
                tags: Vec::new(),
                skills: Vec::new(),
                deps: deps.iter().map(|dep| (*dep).to_string()).collect(),
            },
        )
    }

    fn spawned(seq: u64, id: &str, attempt: u32) -> Event {
        ev(
            seq,
            EventKind::Spawned {
                task: id.to_string(),
                attempt,
                agent: "claude".to_string(),
                model: Some("m1".to_string()),
                worktree: format!("/wt/{id}"),
                branch: format!("shprd/r1/{id}"),
            },
        )
    }

    /// A hand-in with one passing check and a commit.
    fn reported(seq: u64, id: &str, attempt: u32) -> Event {
        ev(
            seq,
            EventKind::Reported {
                task: id.to_string(),
                attempt,
                status: ReportStatus::Completed,
                summary: "done".to_string(),
                tests: vec!["cargo test".to_string()],
                checks: vec![CheckReport {
                    gate: "tests".to_string(),
                    command: "cargo test".to_string(),
                    exit_code: Some(0),
                    output_tail: "ok".to_string(),
                }],
                changed_paths: vec![format!("src/{id}/x.rs")],
                commit: Some(format!("{id}-commit")),
                skills_used: Vec::new(),
                proofs: Vec::new(),
            },
        )
    }

    fn review_requested(seq: u64, id: &str, attempt: u32) -> Event {
        ev(
            seq,
            EventKind::ReviewRequested {
                task: id.to_string(),
                attempt,
                reviewer: "codex".to_string(),
                degraded: false,
            },
        )
    }

    fn review_passed(seq: u64, id: &str, attempt: u32, request_seq: u64) -> Event {
        ev(
            seq,
            EventKind::GatePassed {
                task: id.to_string(),
                attempt,
                gate: "review".to_string(),
                findings: Vec::new(),
                source: Some(ReviewSource {
                    reviewer: "codex".to_string(),
                    findings_sha256:
                        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
                            .to_string(),
                    request_seq,
                }),
            },
        )
    }

    fn review_failed(seq: u64, id: &str, attempt: u32, request_seq: u64) -> Event {
        ev(
            seq,
            EventKind::GateFailed {
                task: id.to_string(),
                attempt,
                gate: "review".to_string(),
                findings: vec![crate::event::Finding {
                    severity: crate::event::Severity::Major,
                    title: format!("the {id} retry loses work"),
                    location: None,
                    detail: String::from("the batch is dropped on a retry"),
                }],
                source: Some(ReviewSource {
                    reviewer: "codex".to_string(),
                    findings_sha256:
                        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
                            .to_string(),
                    request_seq,
                }),
            },
        )
    }

    fn gated(seq: u64, id: &str, attempt: u32, gate: &str) -> Event {
        ev(
            seq,
            EventKind::GatePassed {
                task: id.to_string(),
                attempt,
                gate: gate.to_string(),
                findings: Vec::new(),
                source: None,
            },
        )
    }

    fn integrated(seq: u64, id: &str) -> Event {
        ev(
            seq,
            EventKind::Integrated {
                task: id.to_string(),
                commits: vec![format!("{id}-commit")],
            },
        )
    }

    fn escalated(seq: u64, task: &str, body: &str) -> Event {
        ev(
            seq,
            EventKind::Escalated {
                task: Some(task.to_string()),
                body: body.to_string(),
                default_action: "wait for a human".to_string(),
            },
        )
    }

    fn folded(events: &[Event]) -> RunState {
        RunState::fold(events).unwrap_or_else(|err| panic!("the fixture does not fold: {err}"))
    }

    /// A run with `ids` added, each depending on nothing.
    fn run_with(ids: &[&str]) -> Vec<Event> {
        let mut log = vec![started(1)];
        for (index, id) in ids.iter().enumerate() {
            log.push(added(2 + index as u64, id, &[]));
        }
        log
    }

    /// A task driven from added to ready to land, in one attempt.
    fn ready_log(id: &str) -> Vec<Event> {
        vec![
            started(1),
            added(2, id, &[]),
            spawned(3, id, 1),
            reported(4, id, 1),
            review_requested(5, id, 1),
            review_passed(6, id, 1, 5),
        ]
    }

    /// The config a pass runs under: `merge`, and `max_running` slots.
    fn config(merge: MergePolicy, max_running: u32) -> Config {
        Config {
            land: Land {
                merge,
                protected: Vec::new(),
            },
            tick: Tick {
                max_running,
                ..Tick::default()
            },
            ..Config::default()
        }
    }

    fn phases(state: &RunState) -> BTreeMap<String, Phase> {
        state
            .tasks
            .iter()
            .map(|(id, task)| (id.clone(), Phase::of(task.status)))
            .collect()
    }

    /// Plans a pass over `state` with `facts` under `config`, with the engine
    /// reviewing a hand-in when `review` says so.
    fn plan_with(state: &RunState, facts: &Facts, config: &Config, review: bool) -> Vec<Step> {
        plan(&Input {
            state,
            phases: &phases(state),
            facts,
            config,
            review,
        })
    }

    /// Plans a pass with review off, which is what most rules are about.
    fn plan_for(state: &RunState, facts: &Facts, config: &Config) -> Vec<Step> {
        plan_with(state, facts, config, false)
    }

    #[test]
    fn a_queued_task_whose_dependency_landed_starts() {
        // A is done: spawned, handed in, gated and integrated. B waits for it.
        let log = vec![
            started(1),
            added(2, "A", &[]),
            spawned(3, "A", 1),
            reported(4, "A", 1),
            gated(5, "A", 1, "lint"),
            integrated(6, "A"),
            added(7, "B", &["A"]),
        ];
        let state = folded(&log);
        let steps = plan_for(&state, &Facts::default(), &config(MergePolicy::Ask, 4));
        assert_eq!(
            steps,
            vec![Step::Start {
                task: "B".to_string()
            }]
        );
    }

    #[test]
    fn a_queued_task_whose_dependency_has_not_landed_waits() {
        let mut log = run_with(&["A"]);
        log.push(added(3, "B", &["A"]));
        let state = folded(&log);
        let steps = plan_for(&state, &Facts::default(), &config(MergePolicy::Ask, 4));
        // A starts; B waits for it to land.
        assert_eq!(
            steps,
            vec![Step::Start {
                task: "A".to_string()
            }]
        );
    }

    #[test]
    fn the_concurrency_cap_stops_a_pass_at_its_limit() {
        // Three queued tasks, one slot: one starts, and it is the first by id.
        let state = folded(&run_with(&["A", "B", "C"]));
        let steps = plan_for(&state, &Facts::default(), &config(MergePolicy::Ask, 1));
        assert_eq!(
            steps,
            vec![Step::Start {
                task: "A".to_string()
            }]
        );
    }

    #[test]
    fn running_tasks_count_against_the_cap() {
        let mut log = run_with(&["A", "B"]);
        log.push(spawned(4, "A", 1));
        let state = folded(&log);
        let steps = plan_for(&state, &Facts::default(), &config(MergePolicy::Ask, 1));
        assert!(steps.is_empty(), "A already runs, so B waits: {steps:?}");
        let steps = plan_for(&state, &Facts::default(), &config(MergePolicy::Ask, 2));
        assert_eq!(
            steps,
            vec![Step::Start {
                task: "B".to_string()
            }]
        );
    }

    #[test]
    fn a_live_agent_counts_against_the_cap_even_when_the_fold_says_nothing() {
        let state = folded(&run_with(&["A", "B"]));
        let mut facts = Facts::default();
        facts.live.insert("A".to_string());
        let steps = plan_for(&state, &facts, &config(MergePolicy::Ask, 1));
        assert!(steps.is_empty(), "{steps:?}");
    }

    #[test]
    fn an_auto_policy_lands_a_ready_task() {
        let state = folded(&ready_log("A"));
        let steps = plan_for(&state, &Facts::default(), &config(MergePolicy::Auto, 4));
        assert_eq!(
            steps,
            vec![Step::Land {
                task: "A".to_string(),
                policy: MergePolicy::Auto,
            }]
        );
    }

    #[test]
    fn a_local_checks_policy_lands_on_the_hand_ins_own_report() {
        // A repository with no forge has no CI to ask: the checks policy reads
        // the hand-in's passing report, which `ready_to_land` already accepted.
        let state = folded(&ready_log("A"));
        let facts = Facts {
            local: true,
            ..Facts::default()
        };
        let steps = plan_for(&state, &facts, &config(MergePolicy::Checks, 4));
        assert_eq!(
            steps,
            vec![Step::Land {
                task: "A".to_string(),
                policy: MergePolicy::Checks,
            }]
        );
    }

    #[test]
    fn a_checks_policy_waits_for_the_forge_to_report_success() {
        let state = folded(&ready_log("A"));
        let mut facts = Facts::default();
        facts.prs.insert(
            "A".to_string(),
            PrFacts {
                open: true,
                merged: false,
                checks: None,
                head: Some("A-commit".to_string()),
                number: Some(7),
            },
        );
        let steps = plan_for(&state, &facts, &config(MergePolicy::Checks, 4));
        assert!(steps.is_empty(), "checks are still running: {steps:?}");

        facts.prs.get_mut("A").expect("inserted").checks = Some(false);
        let steps = plan_for(&state, &facts, &config(MergePolicy::Checks, 4));
        assert!(steps.is_empty(), "failed checks do not land: {steps:?}");

        facts.prs.get_mut("A").expect("inserted").checks = Some(true);
        let steps = plan_for(&state, &facts, &config(MergePolicy::Checks, 4));
        assert_eq!(
            steps,
            vec![Step::Land {
                task: "A".to_string(),
                policy: MergePolicy::Checks,
            }]
        );
    }

    #[test]
    fn an_ask_policy_says_it_once_and_never_lands_itself() {
        let log = ready_log("A");
        let state = folded(&log);
        let steps = plan_for(&state, &Facts::default(), &config(MergePolicy::Ask, 4));
        let body = needs_you_body("A", None);
        assert_eq!(
            steps,
            vec![Step::NeedsYou {
                task: "A".to_string(),
                body: body.clone(),
            }]
        );

        // Once the escalation is recorded, the same pass says nothing.
        let mut asked = log;
        asked.push(escalated(7, "A", &body));
        let state = folded(&asked);
        let steps = plan_for(&state, &Facts::default(), &config(MergePolicy::Ask, 4));
        assert!(steps.is_empty(), "the ask was already recorded: {steps:?}");
    }

    #[test]
    fn an_ask_is_not_repeated_when_the_pull_request_number_arrives() {
        // The first pass asked before the forge had a number; the second knows
        // it. That is one question, not two.
        let log = ready_log("A");
        let state = folded(&log);
        let first = plan_for(&state, &Facts::default(), &config(MergePolicy::Ask, 4));
        let Step::NeedsYou { body, .. } = first.into_iter().next().expect("an ask") else {
            panic!("the ask policy says so");
        };
        let mut asked = log;
        asked.push(escalated(7, "A", &body));
        let state = folded(&asked);
        let mut facts = Facts::default();
        facts.prs.insert(
            "A".to_string(),
            PrFacts {
                open: true,
                merged: false,
                checks: None,
                head: Some("A-commit".to_string()),
                number: Some(12),
            },
        );
        let steps = plan_for(&state, &facts, &config(MergePolicy::Ask, 4));
        assert!(steps.is_empty(), "already asked: {steps:?}");
    }

    #[test]
    fn a_protected_base_branch_asks_even_under_auto() {
        let state = folded(&ready_log("A"));
        let mut config = config(MergePolicy::Auto, 4);
        config.land.protected = vec!["main".to_string()];
        let steps = plan_for(&state, &Facts::default(), &config);
        assert!(
            matches!(steps.as_slice(), [Step::NeedsYou { .. }]),
            "{steps:?}"
        );
    }

    #[test]
    fn a_local_landing_is_judged_by_the_branch_it_writes() {
        // A local landing writes the run's integration branch, so that is the
        // branch the protected list is asked about. Asking about `main` would
        // turn `auto` into `ask` for every task of every local run.
        let state = folded(&ready_log("A"));
        let facts = Facts {
            local: true,
            ..Facts::default()
        };
        let mut config = config(MergePolicy::Auto, 4);
        config.land.protected = vec!["main".to_string(), "master".to_string()];
        let steps = plan_for(&state, &facts, &config);
        assert_eq!(
            steps,
            vec![Step::Land {
                task: "A".to_string(),
                policy: MergePolicy::Auto,
            }],
            "integration/m8 is not protected: {steps:?}"
        );
        // The branch it writes is what is judged, so protecting that one asks.
        config.land.protected = vec!["integration/m8".to_string()];
        let steps = plan_for(&state, &facts, &config);
        assert!(
            matches!(steps.as_slice(), [Step::NeedsYou { .. }]),
            "{steps:?}"
        );
    }

    #[test]
    fn a_forge_landing_is_judged_by_the_pull_requests_base() {
        let state = folded(&ready_log("A"));
        let mut config = config(MergePolicy::Auto, 4);
        config.land.protected = vec!["main".to_string()];
        // The pull request is opened into the base branch, so a forge landing
        // into `main` is a landing into a protected branch.
        let steps = plan_for(&state, &Facts::default(), &config);
        assert!(
            matches!(steps.as_slice(), [Step::NeedsYou { .. }]),
            "{steps:?}"
        );
    }

    #[test]
    fn a_settled_findings_file_is_decided_once() {
        // A reviewer was asked and has written its file; the pass decides it.
        let log = vec![
            started(1),
            added(2, "A", &[]),
            spawned(3, "A", 1),
            reported(4, "A", 1),
            review_requested(5, "A", 1),
        ];
        let state = folded(&log);
        let mut facts = Facts::default();
        facts.settled_findings.insert("A".to_string());
        let steps = plan_with(&state, &facts, &config(MergePolicy::Auto, 4), true);
        assert_eq!(
            steps,
            vec![Step::Verdict {
                task: "A".to_string()
            }]
        );

        // The recorded gate is the record that stops a second decision.
        let mut decided = log;
        decided.push(gated(6, "A", 1, "review"));
        let state = folded(&decided);
        let steps = plan_with(&state, &facts, &config(MergePolicy::Auto, 4), true);
        assert!(steps.is_empty(), "already decided: {steps:?}");
    }

    #[test]
    fn a_findings_file_still_being_written_is_not_decided() {
        let log = vec![
            started(1),
            added(2, "A", &[]),
            spawned(3, "A", 1),
            reported(4, "A", 1),
            review_requested(5, "A", 1),
        ];
        let state = folded(&log);
        let steps = plan_with(
            &state,
            &Facts::default(),
            &config(MergePolicy::Auto, 4),
            true,
        );
        assert!(steps.is_empty(), "the file has not settled: {steps:?}");
    }

    #[test]
    fn a_verdict_is_not_planned_for_a_review_nobody_requested() {
        // Findings left over from an earlier review must not decide an attempt
        // whose review was never dispatched.
        let log = vec![
            started(1),
            added(2, "A", &[]),
            spawned(3, "A", 1),
            reported(4, "A", 1),
        ];
        let state = folded(&log);
        let mut facts = Facts::default();
        facts.settled_findings.insert("A".to_string());
        let steps = plan_with(&state, &facts, &config(MergePolicy::Auto, 4), false);
        assert!(steps.is_empty(), "{steps:?}");
    }

    #[test]
    fn an_approved_ask_lands_itself() {
        let mut log = ready_log("A");
        log.push(ev(
            7,
            EventKind::TaskPrOpened {
                task: "A".to_string(),
                number: 7,
                url: "https://example.invalid/pull/7".to_string(),
                head: "shprd/r1/A".to_string(),
            },
        ));
        log.push(Event {
            seq: 8,
            at_ms: 8_000,
            actor: Actor::Human,
            kind: EventKind::TaskApproved {
                task: "A".to_string(),
                number: 7,
                head_sha: "A-commit".to_string(),
            },
        });
        let state = folded(&log);
        // The approval names the commit the hand-in reported, which is the only
        // head there is when the forge cannot be asked.
        let steps = plan_for(&state, &Facts::default(), &config(MergePolicy::Ask, 4));
        assert_eq!(
            steps,
            vec![Step::Land {
                task: "A".to_string(),
                policy: MergePolicy::Ask,
            }]
        );
    }

    #[test]
    fn a_pull_request_merged_outside_shprd_is_recorded_as_landed() {
        let mut log = ready_log("A");
        log.push(ev(
            7,
            EventKind::TaskPrOpened {
                task: "A".to_string(),
                number: 7,
                url: "https://example.invalid/pull/7".to_string(),
                head: "shprd/r1/A".to_string(),
            },
        ));
        let state = folded(&log);
        let mut facts = Facts::default();
        facts.prs.insert(
            "A".to_string(),
            PrFacts {
                open: false,
                merged: true,
                checks: Some(true),
                head: Some("A-commit".to_string()),
                number: Some(7),
            },
        );
        let steps = plan_for(&state, &facts, &config(MergePolicy::Auto, 4));
        assert_eq!(
            steps,
            vec![Step::MarkLanded {
                task: "A".to_string(),
                outcome: LandOutcome::AlreadyMerged,
                head: Some("A-commit".to_string()),
                number: Some(7),
            }]
        );
    }

    #[test]
    fn a_merged_task_is_not_marked_twice() {
        let mut log = ready_log("A");
        log.push(ev(
            7,
            EventKind::TaskLanded {
                task: "A".to_string(),
                base: "main".to_string(),
                head: "A-commit".to_string(),
                number: Some(7),
                outcome: LandOutcome::AlreadyMerged,
            },
        ));
        let state = folded(&log);
        let mut facts = Facts::default();
        facts.prs.insert(
            "A".to_string(),
            PrFacts {
                open: false,
                merged: true,
                checks: Some(true),
                head: Some("A-commit".to_string()),
                number: Some(7),
            },
        );
        let steps = plan_for(&state, &facts, &config(MergePolicy::Auto, 4));
        assert!(steps.is_empty(), "{steps:?}");
    }

    #[test]
    fn a_merged_pull_request_whose_head_is_unknown_still_marks_the_task() {
        // A forge that reports the merge but no head is still a merge: the
        // task's own work has landed.
        let state = folded(&ready_log("A"));
        let mut facts = Facts::default();
        facts.prs.insert(
            "A".to_string(),
            PrFacts {
                open: false,
                merged: true,
                ..PrFacts::default()
            },
        );
        let steps = plan_for(&state, &facts, &config(MergePolicy::Ask, 4));
        assert!(matches!(
            steps.as_slice(),
            [Step::MarkLanded {
                outcome: LandOutcome::AlreadyMerged,
                ..
            }]
        ));
    }

    #[test]
    fn a_task_whose_branch_had_nothing_to_merge_is_not_landed_again() {
        // The landing found nothing of the branch's own to merge, so nothing
        // moved — and a pass must not record that same fact every tick.
        let mut log = ready_log("A");
        log.push(ev(
            7,
            EventKind::TaskLanded {
                task: "A".to_string(),
                base: "main".to_string(),
                head: "A-commit".to_string(),
                number: None,
                outcome: LandOutcome::NothingToMerge,
            },
        ));
        let state = folded(&log);
        assert_eq!(
            state
                .tasks
                .get("A")
                .and_then(|task| task.nothing_to_merge.as_deref()),
            Some("A-commit")
        );
        let steps = plan_for(&state, &Facts::default(), &config(MergePolicy::Auto, 4));
        assert!(steps.is_empty(), "nothing is left to land: {steps:?}");
    }

    #[test]
    fn a_task_whose_agent_took_the_work_up_again_is_not_landed() {
        let state = folded(&ready_log("A"));
        let mut facts = Facts::default();
        facts.live.insert("A".to_string());
        let steps = plan_for(&state, &facts, &config(MergePolicy::Auto, 4));
        assert!(steps.is_empty(), "its agent is working again: {steps:?}");
    }

    #[test]
    fn a_handed_in_task_is_reviewed_once() {
        let log = vec![
            started(1),
            added(2, "A", &[]),
            spawned(3, "A", 1),
            reported(4, "A", 1),
        ];
        let state = folded(&log);
        let steps = plan_with(
            &state,
            &Facts::default(),
            &config(MergePolicy::Auto, 4),
            true,
        );
        assert_eq!(
            steps,
            vec![Step::Review {
                task: "A".to_string()
            }],
            "the engine reviews the hand-in"
        );

        // The request is the record that stops the next pass asking again.
        let mut asked = log;
        asked.push(review_requested(5, "A", 1));
        let state = folded(&asked);
        let steps = plan_with(
            &state,
            &Facts::default(),
            &config(MergePolicy::Auto, 4),
            true,
        );
        assert!(steps.is_empty(), "the review is already out: {steps:?}");
    }

    #[test]
    fn a_hand_in_whose_checks_failed_is_not_reviewed() {
        let mut failed = reported(4, "A", 1);
        if let EventKind::Reported { checks, .. } = &mut failed.kind {
            checks[0].exit_code = Some(1);
        }
        let log = vec![started(1), added(2, "A", &[]), spawned(3, "A", 1), failed];
        let state = folded(&log);
        let steps = plan_with(
            &state,
            &Facts::default(),
            &config(MergePolicy::Auto, 4),
            true,
        );
        assert!(
            steps.is_empty(),
            "a failed check is not a hand-in: {steps:?}"
        );
    }

    #[test]
    fn a_hand_in_is_not_reviewed_when_no_reviewer_is_configured() {
        // With no reviewer in the settings there is nothing to start, so the
        // pass leaves the hand-in alone rather than failing a step every tick.
        let log = vec![
            started(1),
            added(2, "A", &[]),
            spawned(3, "A", 1),
            reported(4, "A", 1),
        ];
        let state = folded(&log);
        let steps = plan_for(&state, &Facts::default(), &config(MergePolicy::Auto, 4));
        assert!(steps.is_empty(), "{steps:?}");
    }

    #[test]
    fn a_task_that_is_not_ready_to_land_is_left_alone() {
        // Reported, never reviewed: not ready, and not startable either.
        let log = vec![
            started(1),
            added(2, "A", &[]),
            spawned(3, "A", 1),
            reported(4, "A", 1),
        ];
        let state = folded(&log);
        let steps = plan_for(&state, &Facts::default(), &config(MergePolicy::Auto, 4));
        assert!(steps.is_empty(), "{steps:?}");
    }

    #[test]
    fn a_finished_run_plans_nothing() {
        let mut log = run_with(&["A"]);
        log.push(ev(
            3,
            EventKind::RunFinished {
                abandoned: Vec::new(),
            },
        ));
        let state = folded(&log);
        let steps = plan_for(&state, &Facts::default(), &config(MergePolicy::Auto, 4));
        assert!(steps.is_empty(), "{steps:?}");
    }

    #[test]
    fn a_run_that_never_started_plans_nothing() {
        let state = RunState::default();
        let steps = plan_for(&state, &Facts::default(), &config(MergePolicy::Auto, 4));
        assert!(steps.is_empty(), "{steps:?}");
    }

    #[test]
    fn the_state_after_a_start_plans_no_second_start() {
        let before = folded(&run_with(&["A"]));
        let first = plan_for(&before, &Facts::default(), &config(MergePolicy::Auto, 4));
        assert_eq!(
            first,
            vec![Step::Start {
                task: "A".to_string()
            }]
        );
        // What recording that start leaves behind: the attempt is in the log, so
        // the task is Working rather than Queued.
        let mut log = run_with(&["A"]);
        log.push(spawned(3, "A", 1));
        let after = folded(&log);
        let steps = plan_for(&after, &Facts::default(), &config(MergePolicy::Auto, 4));
        assert!(steps.is_empty(), "A is already started: {steps:?}");
    }

    #[test]
    fn the_state_after_a_landing_plans_no_second_landing() {
        let before = folded(&ready_log("A"));
        let first = plan_for(&before, &Facts::default(), &config(MergePolicy::Auto, 4));
        assert!(matches!(first.as_slice(), [Step::Land { .. }]), "{first:?}");
        // What recording that landing leaves behind.
        let mut log = ready_log("A");
        log.push(ev(
            7,
            EventKind::TaskLanded {
                task: "A".to_string(),
                base: "main".to_string(),
                head: "A-commit".to_string(),
                number: Some(7),
                outcome: LandOutcome::Merged,
            },
        ));
        let after = folded(&log);
        let steps = plan_for(&after, &Facts::default(), &config(MergePolicy::Auto, 4));
        assert!(steps.is_empty(), "A has landed: {steps:?}");
    }

    /// A log whose one task handed in and had its review fail on attempt 1.
    fn review_failed_log(id: &str) -> Vec<Event> {
        vec![
            started(1),
            added(2, id, &[]),
            spawned(3, id, 1),
            reported(4, id, 1),
            review_requested(5, id, 1),
            review_failed(6, id, 1, 5),
        ]
    }

    /// The same task on attempt 2, whose review failed as well: two spent
    /// rounds of the default two.
    fn two_failed_rounds_log(id: &str) -> Vec<Event> {
        let mut log = review_failed_log(id);
        log.push(spawned(7, id, 2));
        log.push(reported(8, id, 2));
        log.push(review_requested(9, id, 2));
        log.push(review_failed(10, id, 2, 9));
        log
    }

    #[test]
    fn a_failed_review_with_rounds_left_is_sent_back_by_itself() {
        // The bug the M9 gate found: the review failed with a round left and
        // the pass planned nothing, so the run went quiet on a row nobody could
        // act on.
        let state = folded(&review_failed_log("A"));
        assert_eq!(
            state.tasks.get("A").map(|task| task.status),
            Some(TaskStatus::Failed)
        );
        let steps = plan_for(&state, &Facts::default(), &config(MergePolicy::Auto, 4));
        assert_eq!(
            steps,
            vec![Step::SendBack {
                task: String::from("A"),
                attempt: 1,
            }],
            "{steps:?}"
        );
    }

    #[test]
    fn a_failed_review_with_no_rounds_left_escalates_instead() {
        let state = folded(&two_failed_rounds_log("A"));
        let steps = plan_for(&state, &Facts::default(), &config(MergePolicy::Auto, 4));
        assert_eq!(steps.len(), 1, "never both: {steps:?}");
        let Step::Escalate {
            task,
            body,
            default_action,
        } = &steps[0]
        else {
            panic!("a task out of rounds asks a person: {steps:?}");
        };
        assert_eq!(task, "A");
        assert!(
            body.starts_with(&gate::rounds_spent_prefix("A", 2, 2)),
            "{body}"
        );
        assert!(
            body.contains("the A retry loses work"),
            "it says what failed: {body}"
        );
        assert!(body.contains("retry with another round"), "{body}");
        assert_eq!(default_action, gate::ESCALATION_ACTION);
    }

    #[test]
    fn one_failed_review_is_never_both_and_never_twice() {
        // One failure, two passes over it: the same single step, and never a
        // send-back beside an escalation.
        let state = folded(&review_failed_log("A"));
        let first = plan_for(&state, &Facts::default(), &config(MergePolicy::Auto, 4));
        let second = plan_for(&state, &Facts::default(), &config(MergePolicy::Auto, 4));
        assert_eq!(first, second);
        assert_eq!(first.len(), 1, "{first:?}");
        assert!(matches!(first[0], Step::SendBack { .. }), "{first:?}");
    }

    #[test]
    fn the_attempt_after_a_send_back_plans_no_second_one() {
        // What recording the send-back leaves behind: attempt 2 is the newest,
        // so the failure on attempt 1 has been answered.
        let mut log = review_failed_log("A");
        log.push(spawned(7, "A", 2));
        let state = folded(&log);
        let steps = plan_for(&state, &Facts::default(), &config(MergePolicy::Auto, 4));
        assert!(steps.is_empty(), "A was sent back once: {steps:?}");
    }

    #[test]
    fn a_recorded_card_is_not_planned_again() {
        // The verdict decision may already have recorded the card, so the step
        // is planned until the card can be answered; once it is, nothing is.
        let log = two_failed_rounds_log("A");
        let state = folded(&log);
        let first = plan_for(&state, &Facts::default(), &config(MergePolicy::Auto, 4));
        let Step::Escalate { body, .. } = &first[0] else {
            panic!("a card: {first:?}");
        };
        let mut asked = log;
        asked.push(escalated(11, "A", body));
        let state = folded(&asked);
        let steps = plan_for(&state, &Facts::default(), &config(MergePolicy::Auto, 4));
        assert!(
            matches!(steps.as_slice(), [Step::Escalate { .. }]),
            "the card still needs its answer route: {steps:?}"
        );

        let mut answered = asked;
        answered.push(ev(
            12,
            EventKind::AskRaised {
                task: String::from("A"),
                ask: gate::rounds_spent_ask(2),
                ask_kind: crate::ask::AskKind::Question,
                text: String::from("out of rounds"),
                options: Vec::new(),
                created_at_ms: 12_000,
                expires_at_ms: None,
            },
        ));
        let state = folded(&answered);
        let steps = plan_for(&state, &Facts::default(), &config(MergePolicy::Auto, 4));
        assert!(steps.is_empty(), "the card was said once: {steps:?}");
    }

    #[test]
    fn a_send_back_waits_while_an_agent_is_live_on_the_task() {
        let state = folded(&review_failed_log("A"));
        let mut facts = Facts::default();
        facts.live.insert(String::from("A"));
        let steps = plan_for(&state, &facts, &config(MergePolicy::Auto, 4));
        assert!(steps.is_empty(), "its agent is still there: {steps:?}");
    }

    #[test]
    fn a_send_back_takes_one_of_the_running_slots() {
        // Two tasks whose reviews failed, one slot: one goes back now and the
        // other waits for a slot, so a pass can never exceed the cap.
        let mut log = review_failed_log("A");
        log.push(added(7, "B", &[]));
        log.push(spawned(8, "B", 1));
        log.push(reported(9, "B", 1));
        log.push(review_requested(10, "B", 1));
        log.push(review_failed(11, "B", 1, 10));
        let state = folded(&log);
        let steps = plan_for(&state, &Facts::default(), &config(MergePolicy::Auto, 1));
        assert_eq!(
            steps,
            vec![Step::SendBack {
                task: String::from("A"),
                attempt: 1,
            }],
            "{steps:?}"
        );
        let steps = plan_for(&state, &Facts::default(), &config(MergePolicy::Auto, 2));
        assert_eq!(steps.len(), 2, "a second slot frees B: {steps:?}");
    }

    #[test]
    fn a_send_back_that_did_not_start_is_a_persons_card() {
        let mut log = review_failed_log("A");
        log.push(escalated(
            7,
            "A",
            &format!("{}no workspace", gate::send_back_failure_prefix("A", 1)),
        ));
        let state = folded(&log);
        let steps = plan_for(&state, &Facts::default(), &config(MergePolicy::Auto, 4));
        assert!(steps.is_empty(), "the card was said once: {steps:?}");
    }

    #[test]
    fn a_failed_non_review_gate_is_left_to_its_own_escalation() {
        // A landing that conflicted records a failed `integrate` gate and an
        // escalation of its own; the review passed, so it is not sent back.
        let mut log = ready_log("A");
        log.push(ev(
            7,
            EventKind::GateFailed {
                task: String::from("A"),
                attempt: 1,
                gate: String::from("integrate"),
                findings: vec![crate::event::Finding {
                    severity: crate::event::Severity::Major,
                    title: String::from("conflicts with the integration branch"),
                    location: None,
                    detail: String::new(),
                }],
                source: None,
            },
        ));
        let state = folded(&log);
        let steps = plan_for(&state, &Facts::default(), &config(MergePolicy::Auto, 4));
        assert!(steps.is_empty(), "{steps:?}");
    }

    #[test]
    fn a_retry_answer_grants_one_more_round() {
        // The card's first choice is answered through the API: one more round
        // for this task, which the next pass spends by sending it back once.
        let mut log = two_failed_rounds_log("A");
        let ask = gate::rounds_spent_ask(2);
        log.push(ev(
            11,
            EventKind::AskRaised {
                task: String::from("A"),
                ask: ask.clone(),
                ask_kind: crate::ask::AskKind::Question,
                text: String::from("out of rounds"),
                options: Vec::new(),
                created_at_ms: 11_000,
                expires_at_ms: None,
            },
        ));
        log.push(ev(
            12,
            EventKind::AskAnswered {
                task: String::from("A"),
                ask,
                answer: String::from(gate::RETRY_CHOICE),
            },
        ));
        let state = folded(&log);
        assert_eq!(
            gate::granted_rounds(&state, state.tasks.get("A").expect("A")),
            1
        );
        let steps = plan_for(&state, &Facts::default(), &config(MergePolicy::Auto, 4));
        assert_eq!(
            steps,
            vec![Step::SendBack {
                task: String::from("A"),
                attempt: 2,
            }],
            "{steps:?}"
        );
    }

    #[test]
    fn an_answer_that_is_not_a_retry_grants_nothing() {
        let mut log = two_failed_rounds_log("A");
        let ask = gate::rounds_spent_ask(2);
        log.push(ev(
            11,
            EventKind::AskRaised {
                task: String::from("A"),
                ask: ask.clone(),
                ask_kind: crate::ask::AskKind::Question,
                text: String::from("out of rounds"),
                options: Vec::new(),
                created_at_ms: 11_000,
                expires_at_ms: None,
            },
        ));
        log.push(ev(
            12,
            EventKind::AskAnswered {
                task: String::from("A"),
                ask,
                answer: String::from("take it as it is"),
            },
        ));
        let state = folded(&log);
        let steps = plan_for(&state, &Facts::default(), &config(MergePolicy::Auto, 4));
        assert!(steps.is_empty(), "the person decided: {steps:?}");
    }

    #[test]
    fn the_same_input_plans_the_same_actions() {
        let state = folded(&run_with(&["A", "B", "C"]));
        let facts = Facts::default();
        let config = config(MergePolicy::Auto, 2);
        let first = plan_for(&state, &facts, &config);
        let second = plan_for(&state, &facts, &config);
        assert_eq!(first, second);
        assert_eq!(first.len(), 2, "the cap holds: {first:?}");
    }
}
