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
//! - [`Step::Land`] a task `shprd_core::rules::ready_to_land` accepts, when its
//!   merge policy allows it;
//! - [`Step::MarkLanded`] a task whose pull request merged outside shprd;
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
}

impl Step {
    /// The task this step is about.
    pub fn task(&self) -> &str {
        match self {
            Self::Start { task }
            | Self::Review { task }
            | Self::Land { task, .. }
            | Self::MarkLanded { task, .. }
            | Self::NeedsYou { task, .. } => task,
        }
    }

    /// The step's name, as a result line spells it.
    pub fn action(&self) -> &'static str {
        match self {
            Self::Start { .. } => "start",
            Self::Review { .. } => "review",
            Self::Land { .. } => "land",
            Self::MarkLanded { .. } => "mark_landed",
            Self::NeedsYou { .. } => "needs_you",
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
    let base = run.base_branch.clone();
    let max_running = input.config.tick.max_running.max(1);
    let mut running = running_count(state, input.facts, input.phases);
    let mut steps = Vec::new();
    for (id, task) in &state.tasks {
        let phase = input
            .phases
            .get(id)
            .copied()
            .unwrap_or_else(|| Phase::of(task.status));
        if let Some(step) = step_for(input, task, phase, &base, &mut running, max_running) {
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
    base: &str,
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
        let policy = input.config.land.policy_for(base);
        let land = Step::Land {
            task: task.id.clone(),
            policy,
        };
        return match policy {
            MergePolicy::Auto => Some(land),
            // The checks are the forge's to report: while they run, or while the
            // forge cannot say, nothing depends on them being green.
            MergePolicy::Checks => (pr.and_then(|pr| pr.checks) == Some(true)).then_some(land),
            MergePolicy::Ask => {
                if approval_matches(task, pr) {
                    Some(land)
                } else if already_asked(state, task) {
                    // Said once already: a pass must not repeat itself.
                    None
                } else {
                    Some(Step::NeedsYou {
                        task: task.id.clone(),
                        body: needs_you_body(&task.id, pr.and_then(|pr| pr.number)),
                    })
                }
            }
        };
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

/// Whether a task handed in work that passed its own checks and has no review
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
    let Some(approved) = task
        .pr
        .as_ref()
        .and_then(|known| known.approved_head.as_deref())
    else {
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
