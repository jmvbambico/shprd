//! The tick inside the service: plan one pass, run it, record what it did.
//!
//! `shprd_core::tick` decides *what* to do; this module is the part that has
//! effects. One pass:
//!
//! 1. reads the run log and the derived status of every task;
//! 2. asks a [`FactsSource`] for what the log cannot hold — the forge's word on
//!    each pull request, and which tasks have an agent running now;
//! 3. plans ([`shprd_core::tick::plan`]);
//! 4. runs each [`Step`] through the verbs the CLI already has — a start goes
//!    through the binary's spawn path, a landing through `shprd_git::land`;
//! 5. appends one ledger event per step that changed something and publishes it
//!    on the service's [`Hub`], so the web app and a Herdr mod see it;
//! 6. when a [`FallbackSource`] is wired, plans and runs the provider ladder —
//!    a nudge, one backup handover, or a give-up — and records what it did.
//!
//! A step that fails is recorded and logged and the pass carries on: one broken
//! landing must not stop a task from starting, and one bad pass must not stop
//! the loop. Two passes never overlap — the interval loop holds a permit — and a
//! pass that loses the race to another process's write simply re-reads the log.
//!
//! Nothing here reaches a network, a terminal or a model: the forge is a
//! [`Forge`], which a test hands a scripted one, the host is whoever answers
//! [`FactsSource::live`], and the spawn is a closure the caller passes
//! ([`Worker::start`]). The binary wires the real three up; the tests wire a
//! fake of each.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use shprd_core::config::Config;
use shprd_core::event::{Actor, EventKind, FallbackAction, LandOutcome, ProviderClass};
use shprd_core::log::{Clock, SystemClock};
use shprd_core::state::{RunState, Task};
use shprd_core::tick::{self, Facts, Input, Phase, PrFacts, Step};
use shprd_git::forge::{Forge, ForgeError};
use shprd_git::git::Git;
use shprd_git::land::{self, LandError, Landing};
use shprd_views::derived::{self, DerivedStatus, LiveAgents};
use shprd_views::pr;
use shprd_watch::fallback::{
    self, Action as LadderAction, AgentFacts, BackupDone, Config as LadderConfig, Role,
    State as LadderState,
};
use shprd_watch::records::ErrorKind as CutKind;

use crate::error::ServerError;
use crate::hub::Hub;
use crate::ledger;
use crate::service::AfterWrite;

/// Where the facts a pass needs come from.
///
/// It is a trait rather than a struct so a test drives the planner with a fake
/// forge and a fake host, and never a network or a terminal. Implementations
/// block — they shell out to `gh` in production — so the tick runs them on a
/// blocking thread.
pub trait FactsSource: Send + Sync {
    /// What the forge says about `task`'s pull request, or `None` when it could
    /// not be asked.
    ///
    /// `None` is the important half: a forge that is down, a repository with no
    /// remote or a branch nobody proposed says nothing, and the planner then
    /// takes no action that depends on the answer.
    fn pr(&self, task: &Task, state: &RunState) -> Option<PrFacts>;

    /// The tasks that have an agent running right now.
    fn live(&self, state: &RunState) -> BTreeSet<String>;
}

/// A forge a pass may read and write from its own thread.
///
/// `Send + Sync` because a pass runs on a worker thread: `gh` is neither a
/// problem nor a surprise there, and the scripted forge a test uses keeps its
/// state behind a lock.
pub type SharedForge = Arc<dyn Forge + Send + Sync>;

/// Which tasks have an agent running, as the host reports it.
pub type LiveFn = Arc<dyn Fn(&RunState) -> BTreeSet<String> + Send + Sync>;

/// The production facts: the forge the binary drives, and the host's agents.
pub struct LiveFacts {
    /// The forge a landing would use, and the one its facts come from.
    pub forge: SharedForge,
    /// Whether a task has a live agent, as the host and the watchdog see it.
    pub live: LiveFn,
}

impl FactsSource for LiveFacts {
    /// The forge's word on the task's pull request.
    ///
    /// The pull request is found by the task's branch, which is how
    /// `shprd_git::land` finds it too, so a pull request a human opened by hand
    /// for that branch counts as the task's.
    fn pr(&self, task: &Task, _state: &RunState) -> Option<PrFacts> {
        let branch = task
            .latest_attempt()
            .map(|attempt| attempt.branch.as_str())?;
        let known = task.pr.as_ref();
        let forge = self.forge.as_ref();
        match forge.find_open(branch) {
            Ok(Some(found)) => {
                let status = forge.status(found.number).ok()?;
                Some(PrFacts {
                    open: !status.merged,
                    merged: status.merged,
                    checks: status.checks_passed,
                    head: Some(status.head_sha),
                    number: Some(found.number),
                })
            }
            Ok(None) => {
                // Nothing is open. A pull request the log knows about may still
                // have merged, and that is the fact worth having.
                let number = known.map(|known| known.number)?;
                let status = forge.status(number).ok()?;
                Some(PrFacts {
                    open: !status.merged,
                    merged: status.merged,
                    checks: status.checks_passed,
                    head: Some(status.head_sha),
                    number: Some(number),
                })
            }
            // The forge could not be asked: the pass takes no action that
            // depends on its answer.
            Err(_) => None,
        }
    }

    fn live(&self, state: &RunState) -> BTreeSet<String> {
        (self.live)(state)
    }
}

/// A fallback host, shared with the pass that runs it.
pub type FallbackSourceArc = Arc<dyn FallbackSource>;

/// Where the fallback ladder's facts and effects come from.
///
/// The ladder itself is pure ([`shprd_watch::fallback::plan`]); this is the
/// only part that reads a transcript, probes a CLI or writes to a terminal, so
/// a test drives the whole ladder with a scripted host and no agent process.
pub trait FallbackSource: Send + Sync {
    /// Every live agent and what its own record says about it.
    ///
    /// A reviewer is included with [`shprd_watch::fallback::Role::Reviewer`] so
    /// the planner can leave it alone; an agent whose record cannot be read is
    /// simply absent, because an unreadable transcript is not a cut.
    fn agents(&self, state: &RunState) -> Vec<AgentFacts>;

    /// The programs installed on this machine.
    fn installed(&self) -> BTreeSet<String>;

    /// The programs signed in on this machine.
    fn signed_in(&self) -> BTreeSet<String>;

    /// Send `text` to the agent working on `task`.
    fn nudge(&self, task: &str, agent: &str, text: &str) -> Result<String, String>;

    /// Start `to` in `task`'s attempt's own worktree and branch, with `brief`.
    fn backup(
        &self,
        task: &str,
        attempt: u32,
        from: &str,
        to: &str,
        brief: &str,
    ) -> Result<String, String>;
}

/// What running one plan step did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepReport {
    /// The action's name: `start`, `land`, `mark_landed` or `needs_you`.
    pub action: String,
    /// The task it was about.
    pub task: String,
    /// True when the step did what the plan wanted.
    pub ok: bool,
    /// One line saying what happened, for a human or a log.
    pub detail: String,
    /// The log sequence the step appended, when it appended one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
}

impl StepReport {
    /// A step that worked.
    fn done(step: &Step, detail: impl Into<String>, seq: Option<u64>) -> Self {
        Self::ok(step.action(), step.task(), detail, seq)
    }

    /// A step that failed; the pass goes on with the next one.
    fn failed(step: &Step, detail: impl Into<String>) -> Self {
        Self::err(step.action(), step.task(), detail)
    }

    /// A ladder action that worked.
    fn ok(action: &str, task: &str, detail: impl Into<String>, seq: Option<u64>) -> Self {
        Self {
            action: action.to_string(),
            task: task.to_string(),
            ok: true,
            detail: detail.into(),
            seq,
        }
    }

    /// A ladder action that failed.
    fn err(action: &str, task: &str, detail: impl Into<String>) -> Self {
        Self {
            action: action.to_string(),
            task: task.to_string(),
            ok: false,
            detail: detail.into(),
            seq: None,
        }
    }
}

/// What one pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TickReport {
    /// Every step the pass ran, in plan order.
    pub steps: Vec<StepReport>,
}

impl TickReport {
    /// The log sequence the pass last published, if it published anything.
    pub fn last_seq(&self) -> Option<u64> {
        self.steps.iter().filter_map(|step| step.seq).max()
    }

    /// True when every step that ran also worked.
    pub fn ok(&self) -> bool {
        self.steps.iter().all(|step| step.ok)
    }
}

/// What the tick needs to start an agent on a task.
///
/// The spawn path belongs to the binary — it writes a brief, creates a worktree
/// and opens a tab — so the service calls the same function the CLI's
/// `agent spawn` does, passed in as a closure. It returns one line about what it
/// did.
pub type Starter = Arc<dyn Fn(&Path, &str) -> Result<String, String> + Send + Sync>;

/// Everything one pass runs against.
pub struct Worker {
    /// The repository whose run log is advanced.
    pub repo: PathBuf,
    /// The home directory shprd keeps state in.
    pub home: PathBuf,
    /// The resolved config: the merge policy and the tick's own settings.
    pub config: Config,
    /// The facts outside the log.
    pub facts: Arc<dyn FactsSource>,
    /// The fallback ladder's transcript facts and effects.
    ///
    /// `None` when the caller has no watchdog wired: a pass then advances tasks
    /// as before and never nudges or backs anything up.
    pub fallback: Option<Arc<dyn FallbackSource>>,
    /// The forge a landing drives. It is the same one the facts come from, so a
    /// pass reads and writes one forge's story rather than two.
    pub forge: SharedForge,
    /// Starts an agent on a task.
    pub start: Starter,
    /// Who the pass's writes act as. The engine, always: the tick is shprd's
    /// own automation rather than a human's or an agent's action.
    pub actor: Actor,
    /// The step the binary runs after a write (its dagr projection).
    pub after_write: Option<AfterWrite>,
    /// The service's change counter, so a write wakes the UIs.
    pub hub: Hub,
}

impl Worker {
    /// Runs one pass and records what it did.
    ///
    /// A log that cannot be read is the one failure that ends the pass: there is
    /// nothing to plan from. Everything after that is per step.
    pub fn tick(&self) -> Result<TickReport, ServerError> {
        let (state, _) =
            ledger::load(&self.repo).map_err(|err| ServerError::Tick(err.to_string()))?;
        let facts = Facts {
            prs: self.state_prs(&state),
            live: self.facts.live(&state),
        };
        // The ladder's facts are read before the phases because a provider cut
        // is what makes a task read "Needs you" rather than "Working".
        let ladder = self.ladder_plan(&state);
        let phases = self.phases(&state, &facts, &ladder.retries);
        let steps = tick::plan(&Input {
            state: &state,
            phases: &phases,
            facts: &facts,
            config: &self.config,
        });
        let mut report = TickReport::default();
        for step in steps {
            let result = self.run_step(&state, &step);
            report.steps.push(result);
        }
        // The ladder runs after the advance steps so a task this pass just
        // started is looked at on the next one, with its record already open.
        for action in &ladder.actions {
            report.steps.push(self.run_ladder(&state, action));
        }
        Ok(report)
    }

    /// The programs a cut worker is handed to, in order: the worker role's own
    /// fallback list, then the configured workers.
    ///
    /// The role's list is what the brief's Settings block prints, so the ladder
    /// and the block agree. A settings file that cannot be read (or a role that
    /// names nobody) leaves the configured workers, which is the order a run had
    /// before the role settings existed. A `program/model` entry counts as its
    /// program, because a backup is chosen by program.
    fn worker_fallbacks(&self) -> Vec<String> {
        let mut programs: Vec<String> =
            match shprd_core::settings::Loaded::read(&self.home, Some(&self.repo)) {
                Ok(loaded) => loaded
                    .resolve(None)
                    .role("worker")
                    .map(|role| role.fallback.value)
                    .unwrap_or_default(),
                Err(_) => Vec::new(),
            };
        for worker in &self.config.roster.workers {
            let program = worker.split('/').next().unwrap_or(worker).trim();
            if !program.is_empty() && !programs.iter().any(|kept| kept == program) {
                programs.push(program.to_owned());
            }
        }
        programs
    }

    /// Reads the ladder's facts and plans its actions.
    ///
    /// The fallback list is the worker role's own (`roles.worker.fallback`),
    /// which is the list the brief's Settings block prints, then the configured
    /// workers, so a setting nobody made keeps the roster's order.
    fn ladder_plan(&self, state: &RunState) -> LadderPass {
        let Some(source) = &self.fallback else {
            return LadderPass::default();
        };
        let agents = source.agents(state);
        // Nothing was cut off, so the ladder has nothing to do. The sign-in
        // probe behind `installed`/`signed_in` launches every CLI, so it is
        // only paid for when a backup could actually be chosen.
        if !agents.iter().any(|agent| agent.cut.is_some()) {
            return LadderPass::default();
        }
        let config = LadderConfig {
            waits_ms: Vec::new(),
            fallbacks: BTreeMap::from([(String::from("worker"), self.worker_fallbacks())]),
            installed: source.installed(),
            signed_in: source.signed_in(),
        };
        let actions = fallback::plan(&ladder_state(state), &agents, SystemClock.now_ms(), &config);
        let waits = config.waits().to_vec();
        let mut retries = BTreeMap::new();
        for agent in &agents {
            // A reviewer is never retried, so its cut is not the task's status.
            if agent.role == Role::Reviewer {
                continue;
            }
            let Some(cut) = &agent.cut else {
                continue;
            };
            let reading = fallback::retry(cut, &waits);
            retries.insert(
                agent.task.clone(),
                derived::ProviderCut {
                    // The live overlay `phases` builds names each task by its
                    // id, so that is the address the status rule compares.
                    agent: agent.task.clone(),
                    reason: reading.reason,
                    over: reading.over,
                    due_ms: reading.due_ms,
                },
            );
        }
        LadderPass { actions, retries }
    }

    /// The forge's word on every task with a branch, by task id.
    fn state_prs(&self, state: &RunState) -> BTreeMap<String, PrFacts> {
        let mut prs = BTreeMap::new();
        for (id, task) in &state.tasks {
            if task.latest_attempt().is_none() {
                continue;
            }
            if let Some(facts) = self.facts.pr(task, state) {
                prs.insert(id.clone(), facts);
            }
        }
        prs
    }

    /// Every task's derived status, from the fold, the live overlay and the
    /// provider cut waiting to be retried.
    ///
    /// The overlay is built from [`Facts::live`]: a task whose agent is running
    /// is reported at work, which is what keeps the planner from landing a
    /// commit an agent is still writing. A provider cut keeps the task Working
    /// while the ladder waits, and reads Needs you once the tries run out
    /// (`agents.py:127-131`).
    fn phases(
        &self,
        state: &RunState,
        facts: &Facts,
        retries: &BTreeMap<String, derived::ProviderCut>,
    ) -> BTreeMap<String, Phase> {
        let now_ms = SystemClock.now_ms();
        state
            .tasks
            .keys()
            .map(|id| {
                let mut signal =
                    shprd_views::derived::TaskSignal::from_state(state, id).unwrap_or_default();
                signal.provider_cut = retries.get(id).cloned();
                let live = if facts.is_live(id) {
                    LiveAgents::new(vec![shprd_views::derived::LiveAgent {
                        address: id.clone(),
                        activity: Some(shprd_views::derived::Activity::Working),
                        ..shprd_views::derived::LiveAgent::default()
                    }])
                } else {
                    LiveAgents::default()
                };
                let derived = derived::derive_status(&signal, &live, now_ms);
                (id.clone(), phase_of(derived.status))
            })
            .collect()
    }

    /// Runs one ladder action, recording its event and publishing it.
    fn run_ladder(&self, state: &RunState, action: &LadderAction) -> StepReport {
        let result = match action {
            LadderAction::Nudge {
                task,
                attempt,
                agent,
                text,
                cut,
                kind,
                reason,
                tries,
            } => self.nudge_ladder(task, *attempt, agent, text, cut, *kind, reason, *tries),
            LadderAction::Backup {
                task,
                attempt,
                from,
                to,
                cut,
                kind,
                reason,
                handover,
            } => self.start_backup(
                state, task, *attempt, from, to, cut, *kind, reason, handover,
            ),
            LadderAction::GiveUp {
                task,
                attempt,
                agent,
                cut,
                kind,
                reason,
                tries,
            } => self.give_up(task, *attempt, agent, cut, *kind, reason, *tries),
        };
        match result {
            Ok(report) => report,
            Err(reason) => {
                tracing::warn!(action = action.name(), task = action.task(), %reason, "ladder action failed");
                StepReport::err(action.name(), action.task(), reason)
            }
        }
    }

    /// Sends a carry-on message and records that it was sent.
    #[expect(clippy::too_many_arguments, reason = "one nudge's whole input")]
    fn nudge_ladder(
        &self,
        task: &str,
        attempt: u32,
        agent: &str,
        text: &str,
        cut: &str,
        kind: CutKind,
        reason: &str,
        tries: u32,
    ) -> Result<StepReport, String> {
        let source = self
            .fallback
            .as_ref()
            .ok_or_else(|| String::from("no fallback source is wired"))?;
        let detail = source.nudge(task, agent, text)?;
        let seq = self.commit(EventKind::Nudged {
            task: task.to_string(),
            attempt,
            agent: agent.to_string(),
            cut: cut.to_string(),
            class: provider_class(kind),
            reason: reason.to_string(),
            tries,
        })?;
        self.notify(Some(seq));
        Ok(StepReport::ok("nudge", task, detail, Some(seq)))
    }

    /// Starts the backup handover and records it, once per task.
    #[expect(clippy::too_many_arguments, reason = "one handover's whole input")]
    fn start_backup(
        &self,
        state: &RunState,
        task: &str,
        attempt: u32,
        from: &str,
        to: &str,
        cut: &str,
        kind: CutKind,
        reason: &str,
        handover: &fallback::Handover,
    ) -> Result<StepReport, String> {
        let source = self
            .fallback
            .as_ref()
            .ok_or_else(|| String::from("no fallback source is wired"))?;
        let attempt_record = state
            .tasks
            .get(task)
            .and_then(|task| task.latest_attempt())
            .ok_or_else(|| format!("task {task:?} has no attempt to continue"))?;
        let brief = fallback::handover_brief(task, from, to, reason, handover);
        let started = source.backup(task, attempt, from, to, &brief);
        let (started, detail) = match started {
            Ok(detail) => (true, detail),
            Err(err) => (false, err),
        };
        let seq = self.commit(EventKind::Backup {
            task: task.to_string(),
            attempt,
            from_agent: from.to_string(),
            to_agent: to.to_string(),
            cut: cut.to_string(),
            class: provider_class(kind),
            reason: reason.to_string(),
            worktree: attempt_record.worktree.clone(),
            branch: attempt_record.branch.clone(),
            started,
        })?;
        self.notify(Some(seq));
        if started {
            return Ok(StepReport::ok("backup", task, detail, Some(seq)));
        }
        // A failed handover is still the task's one try, and the person is told
        // why rather than the next pass trying the same thing again.
        let body = format!("{task}: backup didn't start: {detail}");
        self.commit(EventKind::Escalated {
            task: Some(task.to_string()),
            body: body.clone(),
            default_action: String::from("pick the task up by hand"),
        })?;
        Err(body)
    }

    /// Records that the ladder ended and a person is needed.
    #[expect(clippy::too_many_arguments, reason = "one give-up's whole input")]
    fn give_up(
        &self,
        task: &str,
        attempt: u32,
        agent: &str,
        cut: &str,
        kind: CutKind,
        reason: &str,
        tries: u32,
    ) -> Result<StepReport, String> {
        let seq = self.commit(EventKind::ProviderGaveUp {
            task: task.to_string(),
            attempt,
            agent: agent.to_string(),
            cut: cut.to_string(),
            class: provider_class(kind),
            reason: reason.to_string(),
            tries,
        })?;
        self.notify(Some(seq));
        let body = format!("{task}: {reason}");
        let escalated = self.commit(EventKind::Escalated {
            task: Some(task.to_string()),
            body: body.clone(),
            default_action: String::from("pick the task up by hand"),
        })?;
        self.notify(Some(escalated));
        Ok(StepReport::ok("give_up", task, body, Some(seq)))
    }

    /// Runs one step, mapping a failure to a reported one.
    fn run_step(&self, state: &RunState, step: &Step) -> StepReport {
        let result = match step {
            Step::Start { task } => self.start_task(task),
            Step::Land { task, policy: _ } => self.land_task(state, task),
            Step::MarkLanded {
                task,
                outcome,
                head,
                number,
            } => self.mark_landed(state, task, *outcome, head.as_deref(), *number),
            Step::NeedsYou { task, body } => self.needs_you(task, body),
        };
        match result {
            Ok(report) => report,
            Err(reason) => {
                // Logged, not raised: a failing step is news about one task, and
                // the pass must reach the others.
                tracing::warn!(action = step.action(), task = step.task(), %reason, "tick step failed");
                StepReport::failed(step, reason)
            }
        }
    }

    /// Starts an agent on a queued task through the caller's spawn path.
    fn start_task(&self, task: &str) -> Result<StepReport, String> {
        let detail = (self.start)(&self.repo, task)?;
        let step = Step::Start {
            task: task.to_string(),
        };
        let seq = self.last_seq();
        self.notify(seq);
        after_write(&self.after_write, &self.repo);
        Ok(StepReport::done(&step, detail, seq))
    }

    /// Lands a task through its own pull request, as `shprd land` does.
    ///
    /// The composition is the same one the verb uses: the rule is re-asked here
    /// (a pass may have been planned from a log that has since moved), the
    /// branch's head is read, an approval counts only for that exact commit, and
    /// every outcome is recorded once per head.
    fn land_task(&self, state: &RunState, task: &str) -> Result<StepReport, String> {
        let step = Step::Land {
            task: task.to_string(),
            policy: self.config.land.merge,
        };
        let found = state
            .tasks
            .get(task)
            .ok_or_else(|| format!("no task {task:?} in this run"))?;
        shprd_core::rules::ready_to_land(found).map_err(|err| err.to_string())?;
        let attempt = found
            .latest_attempt()
            .ok_or_else(|| format!("task {task:?} has no attempt to land"))?;
        let run = state
            .run
            .as_ref()
            .ok_or_else(|| String::from("the run has not started"))?;
        let worktree = PathBuf::from(&attempt.worktree);
        let git = Git::new(&worktree);
        let head = git.head_sha().map_err(|err| err.to_string())?;
        let approved =
            found.pr.as_ref().and_then(|pr| pr.approved_head.as_deref()) == Some(head.as_str());
        let has_work = attempt
            .report
            .as_ref()
            .and_then(|report| report.commit.as_ref())
            .is_some()
            || found.pr.is_some();
        let base = run.base_branch.clone();
        let title = format!("{task}: {}", found.title);
        let body = write_body(
            &self.home,
            &run.run_id,
            task,
            &pr::task_pr_body_marked(found),
        )?;
        let forge = self.forge();
        let policy = self.config.land.policy_for(&base);
        let landing = Landing {
            task,
            branch: &attempt.branch,
            base: &base,
            title: &title,
            body: &body,
            has_work,
        };
        let landed = land::land(forge, &git, &landing, policy, approved)
            .map_err(|err| describe_land_error(err, task))?;
        let seq = self.record_landing(state, task, &base, &landed)?;
        Ok(StepReport::done(
            &step,
            landed_line(task, &base, &landed),
            seq,
        ))
    }

    /// Records what a landing touched, once per pull request and outcome.
    fn record_landing(
        &self,
        state: &RunState,
        task: &str,
        base: &str,
        landed: &land::Landed,
    ) -> Result<Option<u64>, String> {
        let mut last = None;
        if let Some(found) = landed.pr() {
            let known = state.tasks.get(task).and_then(|task| task.pr.as_ref());
            let already =
                known.is_some_and(|known| known.number == found.number && known.head == found.head);
            if !already {
                let event = self.commit(EventKind::TaskPrOpened {
                    task: task.to_string(),
                    number: found.number,
                    url: found.url.clone(),
                    head: found.head.clone(),
                })?;
                last = Some(event);
            }
        }
        if let Some(outcome) = landed.outcome() {
            let event = self.commit(EventKind::TaskLanded {
                task: task.to_string(),
                base: base.to_string(),
                head: landed.head().to_string(),
                number: landed.number(),
                outcome,
            })?;
            last = Some(event);
        }
        if let Some(seq) = last {
            self.notify(Some(seq));
        }
        Ok(last)
    }

    /// Records a merge the forge made outside shprd.
    fn mark_landed(
        &self,
        state: &RunState,
        task: &str,
        outcome: LandOutcome,
        head: Option<&str>,
        number: Option<u64>,
    ) -> Result<StepReport, String> {
        let step = Step::MarkLanded {
            task: task.to_string(),
            outcome,
            head: head.map(str::to_string),
            number,
        };
        let run = state
            .run
            .as_ref()
            .ok_or_else(|| String::from("the run has not started"))?;
        let known = state
            .tasks
            .get(task)
            .and_then(|task| task.pr.as_ref())
            .map(|pr| pr.head.clone());
        let head = head
            .map(str::to_string)
            .or(known)
            .or_else(|| {
                state
                    .tasks
                    .get(task)
                    .and_then(|task| task.latest_attempt())
                    .and_then(|attempt| attempt.report.as_ref())
                    .and_then(|report| report.commit.clone())
            })
            .ok_or_else(|| format!("no commit recorded for task {task:?}"))?;
        let number = number.or_else(|| {
            state
                .tasks
                .get(task)
                .and_then(|task| task.pr.as_ref())
                .map(|pr| pr.number)
        });
        let event = self.commit(EventKind::TaskLanded {
            task: task.to_string(),
            base: run.base_branch.clone(),
            head: head.clone(),
            number,
            outcome,
        })?;
        self.notify(Some(event));
        Ok(StepReport::done(
            &step,
            format!("{task} was merged outside shprd at {head}"),
            Some(event),
        ))
    }

    /// Records that a ready task waits on a human, once.
    fn needs_you(&self, task: &str, body: &str) -> Result<StepReport, String> {
        let step = Step::NeedsYou {
            task: task.to_string(),
            body: body.to_string(),
        };
        let event = self.commit(EventKind::Escalated {
            task: Some(task.to_string()),
            body: body.to_string(),
            default_action: format!("wait for a human to approve {task}"),
        })?;
        self.notify(Some(event));
        Ok(StepReport::done(&step, body.to_string(), Some(event)))
    }

    /// Appends one event under the log's lock and runs the binary's after-write
    /// step, exactly as a CLI verb does.
    fn commit(&self, kind: EventKind) -> Result<u64, String> {
        let event = ledger::commit(&self.repo, &self.actor, kind).map_err(|err| err.to_string())?;
        after_write(&self.after_write, &self.repo);
        Ok(event.seq)
    }

    /// The forge a landing drives.
    ///
    /// `shprd serve` hands the service the same one the `land` verb drives, so a
    /// scripted forge in a debug build is used by both paths.
    fn forge(&self) -> &dyn Forge {
        self.forge.as_ref()
    }

    /// Publishes a write to everyone waiting on the service.
    fn notify(&self, seq: Option<u64>) {
        if let Some(seq) = seq {
            self.hub.publish(seq);
        }
    }

    /// The log's last sequence now, for a step whose write happened elsewhere.
    fn last_seq(&self) -> Option<u64> {
        ledger::last_seq(&self.repo).ok()
    }
}

/// What one pass's fallback ladder read and planned.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct LadderPass {
    /// The actions to take, in task then agent order.
    actions: Vec<LadderAction>,
    /// Every task's provider cut, as the status rules read it.
    retries: BTreeMap<String, derived::ProviderCut>,
}

/// The ledger's memory of the ladder, rebuilt from the folded task records.
fn ladder_state(state: &RunState) -> LadderState {
    let mut ladder = LadderState::default();
    for (id, task) in &state.tasks {
        let entry = ladder.tasks.entry(id.clone()).or_default();
        for record in &task.fallbacks {
            match record.action {
                FallbackAction::Nudged => {
                    entry
                        .agents
                        .entry(record.agent.clone())
                        .or_default()
                        .nudged
                        .insert(record.cut.clone());
                }
                FallbackAction::GaveUp => {
                    entry
                        .agents
                        .entry(record.agent.clone())
                        .or_default()
                        .gave_up
                        .insert(record.cut.clone());
                }
                FallbackAction::Backup => {
                    entry.backup = Some(BackupDone {
                        from: record.agent.clone(),
                        to: record.to_agent.clone().unwrap_or_default(),
                    });
                }
            }
        }
    }
    ladder
}

/// The log's spelling of a provider class.
fn provider_class(kind: CutKind) -> ProviderClass {
    match kind {
        CutKind::RateLimit => ProviderClass::RateLimit,
        CutKind::Quota => ProviderClass::Quota,
        CutKind::Auth => ProviderClass::Auth,
        CutKind::Overloaded => ProviderClass::Overloaded,
        CutKind::Other => ProviderClass::Other,
    }
}

/// Runs the binary's after-write step, ignoring its failure.
fn after_write(step: &Option<AfterWrite>, repo: &Path) {
    if let Some(step) = step {
        step.call(repo);
    }
}

/// Writes a task's pull-request body under shprd's home, never in the tree.
fn write_body(home: &Path, run: &str, task: &str, body: &str) -> Result<PathBuf, String> {
    let dir = home.join(".shprd").join("pr");
    std::fs::create_dir_all(&dir).map_err(|err| format!("creating {}: {err}", dir.display()))?;
    let path = dir.join(format!("{}-{}.md", file_stem(run), file_stem(task)));
    std::fs::write(&path, body).map_err(|err| format!("writing {}: {err}", path.display()))?;
    Ok(path)
}

/// An id reduced to something a file name can hold.
fn file_stem(id: &str) -> String {
    let stem: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    if stem.is_empty() {
        String::from("task")
    } else {
        stem
    }
}

/// The line a landing prints, word for word what `shprd land` prints.
fn landed_line(task: &str, base: &str, landed: &land::Landed) -> String {
    match landed {
        land::Landed::Merged { pr, .. } => format!("merged {} (task {task})", pr.url),
        land::Landed::AlreadyMerged { pr: Some(pr), .. } => {
            format!("already merged {} (task {task})", pr.url)
        }
        land::Landed::AlreadyMerged { pr: None, .. } => format!("{task} is already in {base}"),
        land::Landed::SquashMerged { .. } => {
            format!("{task}'s work is already in {base}, as other commits")
        }
        land::Landed::NothingToMerge { .. } => {
            format!("nothing to merge: {task}'s branch has no work beyond {base}")
        }
        land::Landed::Waiting { pr, reason, .. } => {
            format!("{} ({})", reason.advice(task), pr.url)
        }
    }
}

/// Why a landing failed, in the words the CLI would use.
fn describe_land_error(err: LandError, task: &str) -> String {
    match err {
        LandError::ChecksFailed { number } => {
            format!("task {task:?}: the checks on pull request {number} failed")
        }
        LandError::HeadMoved { expected, found } => format!(
            "task {task:?}: the pull request is at {found:?}, but the branch is at {expected:?}"
        ),
        LandError::NotMerged { number } => {
            format!("the forge did not report pull request {number} merged")
        }
        LandError::Forge(ForgeError::Failed { args, code, stderr }) => format!(
            "landing task {task:?}: forge {} failed (exit {code:?}): {stderr}",
            args.join(" ")
        ),
        other => format!("landing task {task:?}: {other}"),
    }
}

/// Maps the eight derived words onto the planner's phase.
fn phase_of(status: DerivedStatus) -> Phase {
    match status {
        DerivedStatus::ToDo => Phase::ToDo,
        DerivedStatus::Scheduled => Phase::Scheduled,
        DerivedStatus::Queued => Phase::Queued,
        DerivedStatus::Working => Phase::Working,
        DerivedStatus::NeedsYou => Phase::NeedsYou,
        DerivedStatus::Ready => Phase::Ready,
        DerivedStatus::Merging => Phase::Merging,
        DerivedStatus::Done => Phase::Done,
    }
}

/// Runs one pass every `interval`, never two at once.
///
/// The loop holds one [`tokio::sync::Mutex`] permit for a whole pass, so a tick
/// that is slow — a landing that waits on a push — delays the next one instead
/// of running beside it. A pass that fails to read the log is logged and the
/// loop carries on: the run is still there when the next one looks.
pub async fn run(
    worker: Arc<Worker>,
    interval: Duration,
    shutdown: impl std::future::Future<Output = ()> + Send,
) {
    let permit = Arc::new(tokio::sync::Mutex::new(()));
    tokio::pin!(shutdown);
    loop {
        let running = Arc::clone(&worker);
        let held = Arc::clone(&permit);
        // The pass blocks (a fold, an fsync, a push), so it runs off the reactor.
        let pass = tokio::task::spawn_blocking(move || {
            let _held = held.blocking_lock();
            running.tick()
        });
        tokio::select! {
            joined = pass => match joined {
                Ok(Ok(report)) => tracing::info!(
                    steps = report.steps.len(),
                    ok = report.ok(),
                    "tick pass finished"
                ),
                Ok(Err(err)) => tracing::warn!(%err, "tick pass could not read the run"),
                Err(err) => tracing::warn!(%err, "tick pass did not finish"),
            },
            _ = &mut shutdown => {
                // A pass already running is allowed to finish: it may be in the
                // middle of a landing, and stopping it would leave a pull
                // request nobody recorded.
                tracing::info!("tick stopping after the pass in flight");
                return;
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            _ = &mut shutdown => return,
        }
    }
}
