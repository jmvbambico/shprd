//! The tick inside the service: plan one pass, run it, record what it did.
//!
//! `shprd_core::tick` decides *what* to do; this module is the part that has
//! effects. One pass:
//!
//! 1. reads the run log and the derived status of every task;
//! 2. asks a [`FactsSource`] for what the log cannot hold — the forge's word on
//!    each pull request, which tasks have an agent running now, and which
//!    reviewers have written their findings;
//! 3. plans ([`shprd_core::tick::plan`]);
//! 4. runs each [`Step`] through the verbs the CLI already has — a start goes
//!    through the binary's spawn path, a review through its `agent review`, a
//!    verdict through the same [`shprd_core::gate::decide`] the review verdict
//!    verb uses, a landing through `shprd_git::land`;
//! 5. appends one ledger event per step that changed something and publishes it
//!    on the service's [`Hub`], so the web app and a Herdr mod see it;
//! 6. when a [`FallbackSource`] is wired, plans and runs the provider ladder —
//!    a nudge, a handover to the next line, or a give-up — and records what it did.
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
use shprd_core::config::{Config, MergePolicy};
use shprd_core::event::{
    Actor, EventKind, FallbackAction, LandOutcome, ProviderClass, ResourceAction, ResourceKind,
};
use shprd_core::gate;
use shprd_core::log::{Clock, SystemClock};
use shprd_core::rules::RuleError;
use shprd_core::state::{ResourceStatus, RunState, Task, TaskStatus};
use shprd_core::tick::{self, Facts, Input, Phase, PrFacts, Step};
use shprd_git::forge::Forge;
use shprd_git::git::Git;
use shprd_git::integrate;
use shprd_git::land::{self, LandError, Landing, LocalLanding};
use shprd_git::worktree::Layout;
use shprd_views::derived::{
    self, Activity, Derived, DerivedStatus, LiveAgent, LiveAgents, TaskSignal,
};
use shprd_views::pr;
use shprd_watch::fallback::{
    self, Action as LadderAction, AgentFacts, BackupDone, Config as LadderConfig, Role,
    State as LadderState,
};
use shprd_watch::records::ErrorKind as CutKind;

use crate::clean;
use crate::error::ServerError;
use crate::hub::Hub;
use crate::ledger;
use crate::news::{self, NewsDesk, NewsSent};
use crate::pass;
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

    /// The live tasks whose agent is idle, each with when it was first seen
    /// idle (ms since the epoch).
    ///
    /// An agent that ended its turn without handing in is alive but not at
    /// work; the pass reads it as quiet, so the task stops holding a slot and
    /// reads "Needs you" once the grace has passed. The default says no agent
    /// is idle, which is the old behaviour.
    fn idle_since(&self, _state: &RunState, _now_ms: u64) -> BTreeMap<String, u64> {
        BTreeMap::new()
    }
}

/// A forge a pass may read and write from its own thread.
///
/// `Send + Sync` because a pass runs on a worker thread: `gh` is neither a
/// problem nor a surprise there, and the scripted forge a test uses keeps its
/// state behind a lock.
pub type SharedForge = Arc<dyn Forge + Send + Sync>;

/// Which tasks have an agent running, as the host reports it.
pub type LiveFn = Arc<dyn Fn(&RunState) -> BTreeSet<String> + Send + Sync>;

/// Which live tasks have an agent that is idle, as the host reports it.
pub type IdleFn = Arc<dyn Fn(&RunState) -> BTreeSet<String> + Send + Sync>;

/// The production facts: the forge the binary drives, and the host's agents.
pub struct LiveFacts {
    /// The forge a landing would use, and the one its facts come from.
    pub forge: SharedForge,
    /// Whether a task has a live agent, as the host and the watchdog see it.
    pub live: LiveFn,
    /// Which live tasks have an idle agent; `None` reads every live agent as
    /// at work.
    pub idle: Option<IdleFn>,
    /// When each task was first seen idle, kept across passes.
    seen_idle: std::sync::Mutex<BTreeMap<String, u64>>,
}

impl LiveFacts {
    /// Facts from a forge and the host's live set, with no idle reading.
    pub fn new(forge: SharedForge, live: LiveFn) -> Self {
        Self {
            forge,
            live,
            idle: None,
            seen_idle: std::sync::Mutex::new(BTreeMap::new()),
        }
    }

    /// The same facts with the host's idle reading.
    pub fn with_idle(mut self, idle: IdleFn) -> Self {
        self.idle = Some(idle);
        self
    }
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

    fn idle_since(&self, state: &RunState, now_ms: u64) -> BTreeMap<String, u64> {
        let Some(idle) = &self.idle else {
            return BTreeMap::new();
        };
        let now_idle = idle(state);
        let Ok(mut seen) = self.seen_idle.lock() else {
            return BTreeMap::new();
        };
        // A task that is no longer idle starts its grace over next time.
        seen.retain(|task, _| now_idle.contains(task));
        for task in now_idle {
            seen.entry(task).or_insert(now_ms);
        }
        seen.clone()
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
    /// The action's name: `start`, `review`, `verdict`, `land`, `mark_landed`,
    /// `needs_you`, `send_back`, `escalate`, `nudge`, `backup` or `give_up`.
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

    /// This report with the real cause of its failure on the lines after the
    /// plain one: the command, its exit code and the last lines of its stderr.
    /// The first line is what a person reads (`shprd status` shows only it); the
    /// rest is what the pass's record keeps for whoever has to find out why.
    fn with_cause(mut self, cause: Option<String>) -> Self {
        if let Some(cause) = cause {
            self.detail = format!("{}\n{cause}", self.detail);
        }
        self
    }
}

/// A landing that failed: the line a person reads, and the real cause.
struct LandFail {
    /// The plain line, when the failure is one a person is already told in full
    /// (a clash, a refusal); `None` for a raw error, which gets the generic line.
    line: Option<String>,
    /// The command, its exit code and the last lines of its stderr.
    cause: String,
}

impl LandFail {
    /// A failure of `step` (a few plain words: "reading the branch tip") whose
    /// raw error is `err`.
    fn at(task: &str, step: &str, err: impl std::fmt::Display) -> Self {
        Self {
            line: Some(format!(
                "Couldn't merge {task}: it hit a problem on this machine while {step}, and the details are in the shprd log"
            )),
            cause: land::tail_lines(&format!("{step}: {err}")),
        }
    }
}

impl From<String> for LandFail {
    fn from(cause: String) -> Self {
        Self {
            line: None,
            cause: land::tail_lines(&cause),
        }
    }
}

/// What one pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TickReport {
    /// The run the pass advanced, when the repository has one.
    ///
    /// It is what lets a caller that started before `shprd run start` say the
    /// run exists now, without asking the log again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// Every step the pass ran, in plan order.
    pub steps: Vec<StepReport>,
    /// What the pass told each space's coordinator, when a news desk is wired.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub news: Vec<NewsSent>,
    /// How long the loop should wait before the next pass, in seconds.
    ///
    /// It is the interval this pass actually read from the settings, so
    /// `tasks.tick_secs` (and the old `[tick] interval_secs`) takes effect on a
    /// running service with no restart: the loop sleeps what the last pass
    /// answered rather than the interval the service started with (M10 gate bug
    /// 4). `None` on a pass that did not resolve one, which leaves the loop's
    /// own interval standing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_secs: Option<u64>,
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

    /// How long the loop should wait before the next pass.
    ///
    /// The interval from this pass's settings when it resolved one, else
    /// `fallback` (the interval the loop was started with).
    pub fn next_interval(&self, fallback: Duration) -> Duration {
        match self.interval_secs {
            Some(secs) if secs > 0 => Duration::from_secs(secs),
            _ => fallback,
        }
    }
}

/// What the tick needs to start an agent on a task.
///
/// The spawn path belongs to the binary — it writes a brief, creates a worktree
/// and opens a tab — so the service calls the same function the CLI's
/// `agent spawn` does, passed in as a closure. It returns one line about what it
/// did.
pub type Starter = Arc<dyn Fn(&Path, &str) -> Result<String, String> + Send + Sync>;

/// What the tick needs to start the reviewer on a task that handed in.
///
/// It is the binary's `agent review`, passed in the way [`Starter`] is, so the
/// engine and a person start a review through exactly one code path. The
/// closure returns one line about the reviewer it opened.
pub type Reviewer = Arc<dyn Fn(&Path, &str) -> Result<String, String> + Send + Sync>;

/// What the tick needs to move a review off a reviewer that is out of quota.
///
/// The binary passes its own `review::switch_exhausted`, which looks at every
/// running reviewer's pane, remembers a spent program as dry and starts the
/// review again on the next reviewer in the settings list. The closure takes
/// the repository and reports what it did.
pub type ReviewSwitcher = Arc<dyn Fn(&Path) -> Vec<shprd_core::review::ReviewSwitch> + Send + Sync>;

/// What the tick needs to close a reviewer's tab once its verdict is recorded.
///
/// The verdict itself is the service's to decide and record; the tab belongs to
/// the host, so the binary passes the same `review::close_reviewer` the verb
/// calls. A failure closes nothing and is logged: the verdict is already in the
/// log, and failing the step would tell the caller the review decided nothing.
pub type ReviewCloser = Arc<dyn Fn(&Path, &str, u32) -> Result<(), String> + Send + Sync>;

/// What the tick needs to stop a landed task's agents.
///
/// A landing is the engine's record and the agents are the host's, so the binary
/// passes the same `spawn::stop_task_agents` the CLI's `agent integrate` uses:
/// the worker's and the reviewer's handles are in the records shprd wrote for
/// the task, and only those are in reach. A host that refuses the close is
/// logged and never fails the step: the landing is already in the log.
pub type WorkerCloser = Arc<dyn Fn(&Path, &str) -> Result<(), String> + Send + Sync>;

/// Everything one pass runs against.
pub struct Worker {
    /// The repository whose run log is advanced.
    pub repo: PathBuf,
    /// The home directory shprd keeps state in.
    pub home: PathBuf,
    /// The state directory the last pass's outcome is written to
    /// ([`crate::pass::LastPass`]).
    ///
    /// The binary resolves it from the state home once (`$XDG_STATE_HOME`, else
    /// `<home>/.local/state`) and passes it in, so a pass never reads the
    /// environment to find it.
    pub state_dir: PathBuf,
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
    /// True when the repository has no remote or no forge: a landing then goes
    /// to the run's integration branch in shprd's own worktree, with no push and
    /// no pull request.
    pub local: bool,
    /// Starts an agent on a task.
    pub start: Starter,
    /// Starts the engine's reviewer on a hand-in.
    ///
    /// `None` when the caller has no review path (a record-only pass): the
    /// planner then plans no `review` step. Whether a reviewer is configured is
    /// asked of the settings on every pass, not fixed here.
    pub review: Option<Reviewer>,
    /// Closes the reviewer's tab once its verdict is recorded.
    ///
    /// `None` when the caller has no host (a test, a record-only pass): a
    /// verdict is still recorded, and nothing is left open to close.
    pub close_reviewer: Option<ReviewCloser>,
    /// Moves a review off a reviewer that is out of quota.
    ///
    /// `None` when the caller has no host (a test, a record-only pass).
    pub switch_reviewers: Option<ReviewSwitcher>,
    /// Closes a landed task's space once the landing is recorded.
    ///
    /// `None` when the caller has no host (a test, a record-only pass): a
    /// landing is still recorded, and nothing is left open to close. A landed
    /// task cannot come back to its space — only a failed review sends a task
    /// back, and only before it is integrated — so this is the last moment the
    /// worker's tab is still useful.
    pub close_worker: Option<WorkerCloser>,
    /// Who the pass's writes act as. The engine, always: the tick is shprd's
    /// own automation rather than a human's or an agent's action.
    pub actor: Actor,
    /// The step the binary runs after a write (its dagr projection).
    pub after_write: Option<AfterWrite>,
    /// The service's change counter, so a write wakes the UIs.
    pub hub: Hub,
    /// Where a pass's news goes.
    ///
    /// `None` when the caller wired nobody to tell: a pass then advances tasks
    /// as before and keeps no news. A wired desk still tells nobody when the run
    /// has no coordinator pane recorded, and keeps the lines for a later pass.
    pub news: Option<NewsDesk>,
}

impl Worker {
    /// Runs one pass and records what it did.
    ///
    /// A log that cannot be read is the one failure that ends the pass: there is
    /// nothing to plan from. Everything after that is per step.
    pub fn tick(&self) -> Result<TickReport, ServerError> {
        let (state, _) = match ledger::load(&self.repo) {
            Ok(loaded) => loaded,
            Err(err) => {
                // Nothing else can move until the log can be read, so this is
                // the failure a person must hear about — recorded the same way
                // a failed step is, then raised as before.
                self.record(&pass::LastPass::unreadable(
                    &self.repo,
                    SystemClock.now_ms(),
                    err.to_string(),
                ));
                return Err(ServerError::Tick(err.to_string()));
            }
        };
        // A worktree an older build made has no `info/exclude` entries, so its
        // worker would see shprd's own state as an untracked change. The repair
        // is one file read per live worktree and writes nothing when the entries
        // are already there, so every pass makes it.
        self.repair_worktree_excludes(&state);
        // The settings are read on every pass, never kept from startup: a person
        // who changes the merge policy or the concurrency cap while the service
        // runs gets it on the next pass, exactly as the roles and the fallbacks
        // already do.
        let (config, interval) = self.pass_config();
        let facts = Facts {
            prs: self.state_prs(&state),
            live: self.facts.live(&state),
            local: self.local,
            settled_findings: self.settled_findings(&state),
            protected: self.protected_tasks(&state),
        };
        // The ladder's facts are read before the phases because a provider cut
        // is what makes a task read "Needs you" rather than "Working".
        let ladder = self.ladder_plan(&state);
        let phases = self.phases(&state, &facts, &ladder.retries);
        let steps = tick::plan(&Input {
            state: &state,
            phases: &phases,
            facts: &facts,
            config: &config,
            // `tasks.review_rounds = 0` is Off: no review is asked for at all.
            review: self.review.is_some() && self.reviews_hand_ins() && config.review.rounds > 0,
        });
        let mut report = TickReport {
            run: state.run.as_ref().map(|run| run.run_id.clone()),
            interval_secs: interval,
            ..TickReport::default()
        };
        // A task whose step has failed the same way often enough has been put in
        // front of a person on an earlier pass. Repeating the identical warning
        // every fifteen seconds tells nobody anything new, so a step in that set
        // says it on the debug channel instead. The step still runs and its
        // failure is still recorded: what goes quiet is the terminal.
        let quiet = self.quiet_failures(&state);
        let landing = steps.iter().any(|step| matches!(step, Step::Land { .. }));
        self.clear_scope_waits(&state);
        for step in steps {
            let result = self.run_step(&config, &state, &step, &quiet);
            report.steps.push(result);
        }
        // With no land pending, the `_int` worktree must not hold the integration
        // branch: a base sync takes it back and nothing else lets it go.
        if !landing {
            self.release_idle_integration(&state);
            self.realign_integration(&state);
        }
        // Finished tasks give their worktrees back after the steps, so a task
        // this pass just landed has its windows closed now and is cleaned on the
        // next pass, once those windows are gone.
        self.auto_clean(&facts.live, &mut report);
        // The ladder runs after the advance steps so a task this pass just
        // started is looked at on the next one, with its record already open.
        for action in &ladder.actions {
            report.steps.push(self.run_ladder(&state, action));
        }
        // A review that has been out too long is a card, whatever its reviewer
        // is doing: the pass cannot see into the reviewer, only how long ago it
        // was asked.
        self.watch_reviews(SystemClock.now_ms());
        // And the news last, from the log as the steps left it, so a task this
        // pass started is news on this pass rather than the next one.
        report.news = self.news_pass(&ladder.retries);
        self.record_failures(&report);
        self.record(&pass::LastPass::of(
            &report,
            &self.repo,
            SystemClock.now_ms(),
        ));
        Ok(report)
    }

    /// Cleans up the worktrees, branches and windows of tasks that landed or were
    /// dropped, and the review packets nobody needs any more.
    ///
    /// `tasks.auto_clean` (on unless the person turned it off) gates the whole
    /// sweep. A task is cleaned only when [`clean::eligible`] says so: finished,
    /// nothing of it running, no open card. A finished task that still has a
    /// worker or reviewer window gets that window closed and waits for the next
    /// pass. The removals are the janitor's own plan, taken under the run's
    /// integration lock without waiting; one that fails is tried again on every
    /// later pass and, after [`clean::FAILURES_BEFORE_A_CARD`] passes in a row, is
    /// one run-wide card for the coordinator naming the path and git's error.
    fn auto_clean(&self, live: &BTreeSet<String>, report: &mut TickReport) {
        use shprd_core::settings::Loaded;

        let Ok(loaded) = Loaded::read(&self.home, Some(&self.repo)) else {
            return;
        };
        let tasks = loaded.resolve(None).tasks();
        if !tasks.auto_clean {
            return;
        }
        let Ok((state, events)) = ledger::load(&self.repo) else {
            return;
        };
        let Some(run) = state.run.clone() else {
            return;
        };
        let now_ms = SystemClock.now_ms();
        for task in clean::lingering(&state, live) {
            self.close_worker_task(&task);
            // A reviewer tab keeps the task live as much as its worker's does,
            // so every attempt's reviewer is closed too.
            if let Some(entry) = state.tasks.get(&task) {
                for attempt in &entry.attempts {
                    self.close_reviewer_task(&task, attempt.n);
                }
            }
        }
        let layout = Layout::from_settings(&self.home, &self.repo);
        let ready = clean::eligible(&state, live, now_ms);
        let mut failed: Vec<(String, String, String)> = Vec::new();
        let previous = clean::CleanFailures::read(&self.state_dir, &self.repo);
        let actions = clean::only_for(
            shprd_git::janitor::plan(&state, &events, &layout, now_ms, u32::MAX),
            &ready,
        );
        let repo_name = self
            .repo
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
        // Another verb holds the lock: leave everything, and the failure record,
        // exactly as they were, and look again next pass.
        let lock = match repo_name
            .as_deref()
            .map(|name| integrate::try_lock_run(&self.home, name, &run.run_id))
        {
            Some(Ok(Some(lock))) => Some(lock),
            Some(Ok(None)) | None => return,
            Some(Err(err)) => {
                tracing::warn!(%err, "could not take the integration lock for the cleanup");
                return;
            }
        };
        if !actions.is_empty() {
            for outcome in shprd_git::janitor::execute(&self.repo, &layout, &actions) {
                let task = outcome.action.task().cloned().unwrap_or_default();
                match outcome.result {
                    Ok(kinds) => {
                        for kind in kinds {
                            if let Err(err) = self.commit(kind) {
                                tracing::warn!(%task, %err, "could not record a cleanup");
                            }
                        }
                        report.steps.push(StepReport::ok(
                            "clean",
                            &task,
                            outcome.action.describe(),
                            self.last_seq(),
                        ));
                    }
                    Err(error) => {
                        tracing::warn!(%task, step = %outcome.action.describe(), %error, "a cleanup failed; it is tried again");
                        failed.push((task, outcome.action.describe(), error));
                    }
                }
            }
        }
        drop(lock);
        let record = previous.next(&self.repo, &failed);
        for failure in record.reached_the_count() {
            self.raise_clean_card(failure);
        }
        if record != previous
            && let Err(err) = record.write(&self.state_dir)
        {
            tracing::warn!(%err, "could not write the cleanup failure record");
        }
        let keep: BTreeSet<String> = if state.finished {
            BTreeSet::new()
        } else {
            BTreeSet::from([run.run_id])
        };
        let reviews = self.home.join(".shprd").join("reviews");
        for dir in clean::expire_review_packets(
            &reviews,
            std::time::SystemTime::now(),
            tasks.archive_after,
            &keep,
        ) {
            tracing::info!(dir = %dir.display(), "removed old review packets");
        }
    }

    /// One run-wide card for the coordinator about a removal that kept failing.
    ///
    /// It is the engine's own, so it is operational (the coordinator settles it)
    /// and never "For you". It hangs on the run rather than the task, because a
    /// finished task draws no card of its own.
    fn raise_clean_card(&self, failure: &clean::CleanFailure) {
        let body = clean::card_body(failure);
        if let Err(err) = self.commit(EventKind::Escalated {
            task: None,
            body,
            default_action: String::from(
                "shprd tries the cleanup again on every pass; free the path (stop what holds it) or remove it by hand, then `shprd janitor --apply`",
            ),
        }) {
            tracing::warn!(%err, "the card for a failed cleanup could not be recorded");
        }
    }

    /// With a forge, brings the local copy of the run's target branch back to
    /// origin's, so a landing that once moved it on its own is undone and the
    /// local ref never stays apart from the remote's.
    fn realign_integration(&self, state: &RunState) {
        if self.local {
            return;
        }
        let Some(run) = state.run.as_ref() else {
            return;
        };
        let Some(repo_name) = self
            .repo
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
        else {
            return;
        };
        let Ok(Some(_lock)) = integrate::try_lock_run(&self.home, &repo_name, &run.run_id) else {
            return;
        };
        // The idle sweep has just let go of the worktree, so the branch is
        // free to move unless the person has it checked out.
        let repo = self.repo.clone();
        let outcome = integrate::realign_outcome(&repo, &run.base_branch, |host| {
            shprd_git::land::gh_authed(&repo, host)
        });
        if outcome == integrate::Realign::Moved {
            tracing::warn!(branch = %run.base_branch, "moved the local branch back to the remote's copy");
        }
        for kind in diverged_events(&run.base_branch, &outcome, state) {
            let told = match &kind {
                EventKind::Escalated {
                    body,
                    default_action,
                    ..
                } => Some(shprd_views::news::run_wide_line(body, default_action)),
                _ => None,
            };
            if let Ok(seq) = self.commit(kind)
                && let Some(line) = told
            {
                self.notify(Some(seq));
                self.tell_coordinator(state, &line);
            }
        }
    }

    /// Tells the coordinator one line of news about the run itself, when a news
    /// desk is wired and a coordinator is reachable. A host that refuses the
    /// write is logged; the card stays on the board for the coordinator's next
    /// look.
    fn tell_coordinator(&self, state: &RunState, line: &str) {
        let Some(desk) = &self.news else {
            return;
        };
        let Some(pane) = desk.coordinator.pane(state) else {
            return;
        };
        if let Err(reason) = desk.coordinator.tell(&pane, line) {
            tracing::warn!(%reason, "a run-wide card could not be told to the coordinator");
        }
    }

    /// Detaches the run's `_int` worktree when a command left it on the
    /// integration branch and no landing is pending, so the person can `git
    /// switch` to that branch in their own checkout.
    ///
    /// A run whose integration branch is shprd's own `<namespace><run>/int` is
    /// left alone: no person's checkout wants that branch, and the commands
    /// that open it expect it attached.
    ///
    /// The sweep takes the run's integration lock without waiting, because the
    /// worktree it is looking at is the one every other verb writes: a clean
    /// worktree sitting on the branch is also what a landing looks like between
    /// its own open and its merge, and detaching `HEAD` then would land the work
    /// on a detached head while the branch never moved. A landing, a sync, a gate
    /// or an approval that holds the lock is therefore left alone, and the next
    /// idle pass releases the branch after it.
    fn release_idle_integration(&self, state: &RunState) {
        let Some(run) = state.run.as_ref() else {
            return;
        };
        let Some(repo_name) = self
            .repo
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
        else {
            return;
        };
        let layout = Layout::from_settings(&self.home, &self.repo);
        if run.integration_branch == layout.branch_name(&run.run_id, "int") {
            return;
        }
        // Held for exactly the detach: a verb that starts between the check and
        // the switch waits for it, and one already running makes this a no-op.
        let _lock = match integrate::try_lock_run(&self.home, &repo_name, &run.run_id) {
            Ok(Some(lock)) => lock,
            Ok(None) => {
                tracing::debug!(run = %run.run_id, "a verb holds the integration lock, so the idle sweep left the worktree alone");
                return;
            }
            Err(err) => {
                tracing::warn!(%err, "could not take the integration lock for the idle sweep");
                return;
            }
        };
        let path = layout.path(&repo_name, &run.run_id, "_int");
        if integrate::release_if_held(&path) {
            tracing::debug!(path = %path.display(), "released the integration worktree");
        }
    }

    /// Puts a card in front of a person for every review that has been out for
    /// [`shprd_core::review::STUCK_AFTER_MS`] without a verdict.
    ///
    /// The card is an answerable ask, like the one a spent review raises: its
    /// `review again` closes the reviewer and starts a new one. A request that
    /// already has a card since it was made is not raised again.
    pub fn watch_reviews(&self, now_ms: u64) {
        // A reviewer out of quota is moved on first, so a review that has just
        // been handed to the next reviewer is not also a stuck one.
        if let Some(switch) = &self.switch_reviewers {
            for outcome in switch(&self.repo) {
                self.report_switch(outcome);
            }
        }
        let Ok((state, events)) = ledger::load(&self.repo) else {
            return;
        };
        for stuck in shprd_core::review::stuck_reviews(
            &state,
            &events,
            now_ms,
            shprd_core::review::STUCK_AFTER_MS,
        ) {
            let text = shprd_core::review::stuck_body(
                &stuck.task,
                stuck.attempt,
                &stuck.reviewer,
                stuck.minutes,
            );
            if let Err(err) = self.raise_card(
                &stuck.task,
                &stuck.ask,
                &text,
                shprd_core::review::stuck_options(),
            ) {
                tracing::warn!(task = %stuck.task, %err, "the card for a stuck review could not be recorded");
            }
        }
    }

    /// Puts a review that no reviewer can take in front of the person, once.
    ///
    /// A switch to the next reviewer needs no card: the log and the task's card
    /// already say it. Only a review with no reviewer left is a question, and it
    /// is the same answerable card a stuck review raises, so `review again` and
    /// `drop it` mean what they always do.
    fn report_switch(&self, outcome: shprd_core::review::ReviewSwitch) {
        let shprd_core::review::ReviewSwitch::NoneLeft { task, attempt, why } = outcome else {
            return;
        };
        let Ok((state, _)) = ledger::load(&self.repo) else {
            return;
        };
        let Some(request) = state
            .tasks
            .get(&task)
            .and_then(|task| task.latest_attempt())
            .and_then(|current| current.reviews.last())
        else {
            return;
        };
        let prefix = format!("{}{attempt}-", shprd_core::review::STUCK_ASK_PREFIX);
        let own: Vec<_> = state
            .asks
            .values()
            .filter(|ask| ask.task == task && ask.id.starts_with(&prefix))
            .collect();
        // A card still waiting is the one question; an answered one that led
        // nowhere (the retry was refused too) is asked again.
        if own
            .iter()
            .any(|ask| ask.raised_seq > request.seq && ask.answer.is_none())
        {
            return;
        }
        let text = shprd_core::review::none_left_body(&task, attempt, &why);
        let id = shprd_core::review::stuck_ask(attempt, own.len() + 1);
        if let Err(err) = self.raise_card(&task, &id, &text, shprd_core::review::stuck_options()) {
            tracing::warn!(%task, %err, "the card for a review with no reviewer left could not be recorded");
        }
    }

    /// Counts this pass's failures against the last one's, and asks a person
    /// once a step has failed the same way three passes in a row.
    ///
    /// A step that keeps failing is the person's to fix — a task whose scope
    /// overlaps another's, an integration branch nobody made — so the run is
    /// right to stop, and wrong to say so only in a terminal. The first two
    /// passes are warnings, because a host that was busy or a worktree another
    /// writer held clears itself; the third is a card, recorded once, naming the
    /// error and the verb that fixes it.
    fn record_failures(&self, report: &TickReport) {
        let previous = pass::Failures::read(&self.state_dir, &self.repo);
        let failures = pass::Failures::next(previous.as_ref(), report, &self.repo);
        for run in &failures.runs {
            // Only on the pass that reached the count: the record carries the
            // count forward, so a card written every pass after it would bury
            // the log in the same decision.
            if run.passes != pass::REPEATS_BEFORE_A_CARD {
                continue;
            }
            if let Err(err) = self.escalate_repeated_failure(run) {
                tracing::warn!(task = %run.task, step = %run.step, %err, "the card for a repeated failure could not be recorded");
            }
        }
        if let Err(err) = failures.write(&self.state_dir) {
            tracing::warn!(%err, "could not write the repeated-failure record");
        }
    }

    /// The failures already put in front of a person, as `(task, step)`.
    ///
    /// A step that has failed the same way three passes running is a card once:
    /// the record carries the count forward, so every pass after the third would
    /// write the same card into the log again. The escalation itself is drawn
    /// from the first card, and it leaves the board when the step stops failing
    /// — which is what [`shprd_views::cards`] reads off the log.
    fn quiet_failures(&self, state: &RunState) -> BTreeSet<(String, String)> {
        let Some(failures) = pass::Failures::read(&self.state_dir, &self.repo) else {
            return BTreeSet::new();
        };
        failures
            .runs
            .iter()
            .filter(|run| run.passes >= pass::REPEATS_BEFORE_A_CARD)
            .filter(|run| {
                let marker =
                    repeated_failure_prefix(&run.task, &run.step, attempt_of(state, &run.task));
                state.escalations.iter().any(|escalation| {
                    escalation.task.as_deref() == Some(run.task.as_str())
                        && escalation.body.starts_with(&marker)
                })
            })
            .map(|run| (run.task.clone(), run.step.clone()))
            .collect()
    }

    /// Puts a step that keeps failing in front of a person, once.
    ///
    /// The body begins with [`repeated_failure_prefix`], which [`Self::quiet_failures`]
    /// compares, so one run of failures costs one card rather than one every
    /// fifteen seconds. It names the step and the error, and what the person has
    /// to do: shprd cannot narrow a scope or make a branch for them.
    fn escalate_repeated_failure(&self, run: &pass::FailureRun) -> Result<(), String> {
        let state = ledger::load(&self.repo).map(|(state, _)| state).ok();
        let attempt = state
            .as_ref()
            .map_or(0, |state| attempt_of(state, &run.task));
        let marker = repeated_failure_prefix(&run.task, &run.step, attempt);
        let told = state.is_some_and(|state| {
            state.escalations.iter().any(|escalation| {
                escalation.task.as_deref() == Some(run.task.as_str())
                    && escalation.body.starts_with(&marker)
            })
        });
        if told {
            return Ok(());
        }
        let error = run.error.lines().next().unwrap_or_default().trim();
        // The card for a review with no reviewer left is already raised, in
        // plain words, by the review step itself.
        if error.contains(shprd_core::review::NO_REVIEWER_LEFT) {
            return Ok(());
        }
        let seq = self.commit(EventKind::Escalated {
            task: Some(run.task.clone()),
            body: format!("{marker}{error}"),
            default_action: repeated_failure_action(&run.step, error, &self.state_dir),
        })?;
        self.notify(Some(seq));
        Ok(())
    }

    /// Writes what this pass did where `shprd status` and `GET /api/state` read
    /// it.
    ///
    /// The log is the record and this is its mirror for the people looking at
    /// the run: a state directory that cannot be written is a warning, never a
    /// failed pass.
    fn record(&self, last: &pass::LastPass) {
        if let Err(err) = last.write(&self.state_dir) {
            tracing::warn!(%err, "could not write the last pass's outcome");
        }
    }

    /// Makes sure every live task worktree excludes shprd's own state.
    ///
    /// [`shprd_git::worktree::create`] writes the exclusions when it makes a
    /// worktree, but a run that predates that (or a worktree a human restored)
    /// has none, and its brief promises the worker that `.shprd/` and `.dagr/`
    /// are already ignored. A failure is a warning, never a failed pass: the
    /// work in the task matters more than a tidy `git status`.
    fn repair_worktree_excludes(&self, state: &RunState) {
        for ((kind, name), record) in &state.resources {
            if *kind != ResourceKind::Worktree || record.status != ResourceStatus::Live {
                continue;
            }
            let path = Path::new(name);
            if !path.is_dir() {
                continue;
            }
            if let Err(err) = shprd_git::worktree::ensure_state_excluded(path) {
                tracing::warn!(%err, worktree = %path.display(), "could not exclude shprd's state in the worktree");
            }
        }
    }

    /// Whether the settings name a reviewer the engine may start.
    ///
    /// It is read every pass, like the worker fallback list, so setting a
    /// reviewer while the service runs takes effect on the next pass. The answer
    /// is a `reviewer` role that may review and names a program to run; the
    /// pre-M9 `[roster] reviewer` reaches the same answer through the settings'
    /// legacy layer. A settings file that cannot be read falls back to the
    /// config's own roster entry, so a broken file cannot switch review off.
    fn reviews_hand_ins(&self) -> bool {
        match shprd_core::settings::Loaded::read(&self.home, Some(&self.repo)) {
            Ok(loaded) => loaded.resolve(None).role("reviewer").is_some_and(|role| {
                // A reviewer list kept under `agents` alone is a reviewer too.
                role.may_review()
                    && role
                        .lines()
                        .iter()
                        .any(|line| !line.agent.trim().is_empty())
            }),
            Err(_) => self.config.roster.reviewer.is_some(),
        }
    }

    /// The config this pass runs under, and how long the loop should wait before
    /// the next one.
    ///
    /// See [`pass_config`] for what is read and why, and what the interval means.
    fn pass_config(&self) -> (Config, Option<u64>) {
        pass_config(&self.home, &self.repo, &self.config)
    }

    /// The tasks whose reviewer's findings file has settled and may be decided.
    ///
    /// The reviewer runs outside the repository and cannot record anything: it
    /// writes `findings.json` and stops. The file is the service's to read, and a
    /// file younger than [`shprd_core::review::FINDINGS_SETTLE`] may still be
    /// being written, so only a settled one is offered to the planner.
    fn settled_findings(&self, state: &RunState) -> BTreeSet<String> {
        let Some(run) = state.run.as_ref() else {
            return BTreeSet::new();
        };
        let reviews = self.home.join(".shprd").join("reviews");
        state
            .tasks
            .values()
            .filter(|task| {
                let Some(attempt) = tick::awaiting_verdict(task) else {
                    return false;
                };
                let path =
                    shprd_core::review::findings_path(&reviews, &run.run_id, &task.id, attempt);
                shprd_core::review::settled_mtime(&path).is_some()
            })
            .map(|task| task.id.clone())
            .collect()
    }

    /// The tasks ready to land whose real diff touches a protected path they did
    /// not declare: they wait for the person's Accept whatever the merge policy.
    ///
    /// Only a task the planner could land is read, so a pass runs git for the
    /// few tasks at the gate and not for the whole plan. A task git cannot read
    /// here is not flagged; its landing reads the same branch and fails loudly.
    fn protected_tasks(&self, state: &RunState) -> BTreeSet<String> {
        state
            .tasks
            .values()
            .filter(|task| {
                !matches!(task.status, TaskStatus::Integrated | TaskStatus::Dropped)
                    && shprd_core::rules::ready_to_land(task).is_ok()
            })
            .filter(|task| {
                !shprd_git::changed::protected_for_task(&self.repo, state, task).is_empty()
            })
            .map(|task| task.id.clone())
            .collect()
    }

    /// The programs a cut worker is handed to, in order: the worker role's program
    /// lines, then the configured workers.
    ///
    /// The role's list is what the brief's Settings block prints, so the ladder
    /// and the block agree. A settings file that cannot be read (or a role that
    /// names nobody) leaves the configured workers, which is the order a run had
    /// before the role settings existed. A `program/model` entry counts as its
    /// program, because a backup is chosen by program.
    fn worker_fallbacks(&self) -> (Vec<String>, usize) {
        let mut programs: Vec<String> =
            match shprd_core::settings::Loaded::read(&self.home, Some(&self.repo)) {
                Ok(loaded) => loaded
                    .resolve(None)
                    .role("worker")
                    .map(|role| {
                        // The worker's program lines, in order: the ladder
                        // moves a cut task to the next one. The first line is
                        // listed too, so a task on a later line can wrap round.
                        role.lines().iter().map(|line| line.agent_ref()).collect()
                    })
                    .unwrap_or_default(),
                Err(_) => Vec::new(),
            };
        let lines = programs.len();
        for worker in &self.config.roster.workers {
            let program = worker.split('/').next().unwrap_or(worker).trim();
            if !program.is_empty()
                && !programs
                    .iter()
                    .any(|kept| kept.split('/').next() == Some(program))
            {
                programs.push(program.to_owned());
            }
        }
        (programs, lines)
    }

    /// Tells the coordinator what changed, when a news desk is wired.
    ///
    /// The state is folded again so the pass's own writes are news on this pass.
    /// A fold that fails here is logged and not raised: the advance steps have
    /// already been recorded, and the next pass reads the log it can.
    fn news_pass(&self, retries: &BTreeMap<String, derived::ProviderCut>) -> Vec<NewsSent> {
        let Some(desk) = &self.news else {
            return Vec::new();
        };
        let (state, events) = match ledger::load(&self.repo) {
            Ok(loaded) => loaded,
            Err(err) => {
                tracing::warn!(%err, "the news pass could not read the run");
                return Vec::new();
            }
        };
        let now_ms = desk.clock.now_ms();
        let live = self.facts.live(&state);
        let idle = self.facts.idle_since(&state, now_ms);
        let rows = self.rows(
            &state,
            &live,
            &idle,
            retries,
            Some(&news::started_at(&events)),
            now_ms,
        );
        let snapshot = news::readings(&state, &self.repo, &rows, &events, &live, now_ms);
        // The settle window is read afresh, so a change saved in the settings is
        // the next pass's window and not the next service start's.
        let desk = NewsDesk {
            coordinator: std::sync::Arc::clone(&desk.coordinator),
            settle_ms: settle_override_ms(&self.home, &self.repo).unwrap_or(desk.settle_ms),
            clock: std::sync::Arc::clone(&desk.clock),
        };
        news::tell(&desk, &self.repo, &state, &snapshot)
    }

    /// Reads the ladder's facts and plans its actions.
    ///
    /// The list is the worker role's program lines (`roles.worker.agents`, with
    /// the old `fallback` read as lines too), which the brief's Settings block prints, then the configured
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
        let (worker_list, worker_lines) = self.worker_fallbacks();
        let config = LadderConfig {
            waits_ms: Vec::new(),
            fallbacks: BTreeMap::from([(String::from("worker"), worker_list.clone())]),
            lines: BTreeMap::from([(String::from("worker"), worker_lines)]),
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
    ///
    /// A local landing has no forge to ask, so there is no pull request fact to
    /// have; the planner reads the hand-in's checks instead.
    fn state_prs(&self, state: &RunState) -> BTreeMap<String, PrFacts> {
        let mut prs = BTreeMap::new();
        if self.local {
            return prs;
        }
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

    /// Every task's phase, from the fold, the live overlay and the provider cut
    /// waiting to be retried.
    fn phases(
        &self,
        state: &RunState,
        facts: &Facts,
        retries: &BTreeMap<String, derived::ProviderCut>,
    ) -> BTreeMap<String, Phase> {
        let now_ms = SystemClock.now_ms();
        let idle = self.facts.idle_since(state, now_ms);
        self.rows(state, &facts.live, &idle, retries, None, now_ms)
            .into_iter()
            .map(|(id, (row, _))| (id, phase_of(row.status)))
            .collect()
    }

    /// Every task's derived status, from the fold, the live overlay and the
    /// provider cut waiting to be retried, with the signal each was derived
    /// from.
    ///
    /// The overlay is built from the live set: a task whose agent is running is
    /// reported at work, which is what keeps the planner from landing a commit
    /// an agent is still writing. A provider cut keeps the task Working while
    /// the ladder waits, and reads Needs you once the tries run out
    /// (`agents.py:127-131`).
    ///
    /// The derived status and its signal are returned together because the news
    /// needs both: the row a person sees, and the facts behind it (why an
    /// attempt failed, which reviews it had).
    ///
    /// `started` is when each task's newest attempt was spawned, when the caller
    /// has the log at hand: the status rules then hold a just-started task as
    /// at work for a moment, even before the host reports its agent. A caller
    /// without the log passes `None` and gets the rules without that grace.
    fn rows(
        &self,
        state: &RunState,
        live: &BTreeSet<String>,
        idle: &BTreeMap<String, u64>,
        retries: &BTreeMap<String, derived::ProviderCut>,
        started: Option<&BTreeMap<String, u64>>,
        now_ms: u64,
    ) -> BTreeMap<String, (Derived, TaskSignal)> {
        state
            .tasks
            .keys()
            .map(|id| {
                let mut signal = TaskSignal::from_state(state, id).unwrap_or_default();
                signal.provider_cut = retries.get(id).cloned();
                signal.started_ms = started.and_then(|at| at.get(id).copied());
                let agents = if live.contains(id) {
                    // An agent seen idle is quiet since it was first seen so: the
                    // status rules give it their grace and then read the task as
                    // waiting for a person, which frees its slot.
                    let quiet = idle.get(id).copied();
                    LiveAgents::new(vec![LiveAgent {
                        address: id.clone(),
                        activity: Some(if quiet.is_some() {
                            Activity::Idle
                        } else {
                            Activity::Working
                        }),
                        since_ms: quiet,
                        ..LiveAgent::default()
                    }])
                } else {
                    LiveAgents::default()
                };
                let row = derived::derive_status(&signal, &agents, now_ms);
                (id.clone(), (row, signal))
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

    /// Starts the backup handover and records it, once per line.
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
        // A failed handover is that line's one try: the person is told why, and
        // the next pass moves on to the next untried line (if there is one).
        let body = format!("{task}: {to} didn't start: {detail}");
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
    fn run_step(
        &self,
        config: &Config,
        state: &RunState,
        step: &Step,
        quiet: &BTreeSet<(String, String)>,
    ) -> StepReport {
        let mut cause: Option<String> = None;
        let result = match step {
            Step::Start { task } => self.start_task(task),
            Step::Review { task } => self.review_task(state, task),
            Step::SkipReview { task } => self.skip_review(state, task),
            Step::Verdict { task } => self.verdict_task(config, state, task),
            Step::Land { task, policy } => match self.land_task(state, task, *policy) {
                Ok(report) => Ok(report),
                Err(fail) => {
                    // A person reads one plain sentence; the command line, the
                    // exit code and stderr go to the log and the pass's record.
                    tracing::warn!(task = %task, cause = %fail.cause, "a landing failed");
                    let line = fail.line.unwrap_or_else(|| {
                        format!(
                            "Couldn't merge {task}: it hit a problem on this machine while landing, and the details are in the shprd log"
                        )
                    });
                    cause = Some(fail.cause);
                    Err(line)
                }
            },
            Step::MarkLanded {
                task,
                outcome,
                head,
                number,
            } => self.mark_landed(state, task, *outcome, head.as_deref(), *number),
            Step::NeedsYou { task, body } => self.needs_you(task, body),
            Step::SendBack { task, attempt } => self.send_back(state, task, *attempt),
            Step::Escalate {
                task,
                body,
                default_action,
            } => self.escalate_review(state, task, body, default_action),
        };
        match result {
            Ok(report) => {
                self.clear_waiting(state, step.task());
                report
            }
            Err(reason) => {
                self.note_waiting(state, step.task(), &reason);
                // Logged, not raised: a failing step is news about one task, and
                // the pass must reach the others. A failure a person has already
                // been given a card for is logged quietly: the card is the
                // telling, and a line every pass is noise on top of it.
                let key = (step.task().to_string(), step.action().to_string());
                if quiet.contains(&key) {
                    tracing::debug!(action = step.action(), task = step.task(), %reason, "tick step still failing, already in front of a person");
                } else {
                    tracing::warn!(action = step.action(), task = step.task(), %reason, "tick step failed");
                }
                StepReport::failed(step, reason).with_cause(cause)
            }
        }
    }

    /// Clears the "Waiting: <path> is in use by <task>" line a queued task kept
    /// from when declared paths were a lock.
    ///
    /// Nothing waits on a path any more, but a task held at the running cap gets
    /// no step, so its old line would otherwise stay on its card. Once cleared
    /// the line reads "No longer waiting", so this writes nothing on later passes.
    fn clear_scope_waits(&self, state: &RunState) {
        for (id, task) in &state.tasks {
            let stale = task.status == TaskStatus::Queued
                && task.last_status.as_ref().is_some_and(|status| {
                    status.message.starts_with(WAITING_PREFIX)
                        && status.message.contains(" is in use by ")
                });
            if stale {
                self.clear_waiting(state, id);
            }
        }
    }

    /// Puts why a step is stuck on the task's card, once.
    ///
    /// The status line reads "Waiting: <reason>"; an identical line already on
    /// the card is not written again, so a step that fails every pass says it
    /// once. Best effort: a failed write is not worth failing the pass for.
    fn note_waiting(&self, state: &RunState, task: &str, reason: &str) {
        let message = waiting_message(reason);
        let current = state
            .tasks
            .get(task)
            .and_then(|found| found.last_status.as_ref())
            .map(|status| status.message.as_str());
        if current == Some(message.as_str()) {
            return;
        }
        let progress = state
            .tasks
            .get(task)
            .and_then(|found| found.last_status.as_ref())
            .and_then(|status| status.progress);
        let _ = self.commit(EventKind::TaskStatus {
            task: task.to_string(),
            message,
            progress,
        });
    }

    /// Clears a "Waiting:" line once a step for the task has succeeded.
    fn clear_waiting(&self, state: &RunState, task: &str) {
        let Some(status) = state
            .tasks
            .get(task)
            .and_then(|found| found.last_status.as_ref())
        else {
            return;
        };
        if !status.message.starts_with(WAITING_PREFIX) {
            return;
        }
        let _ = self.commit(EventKind::TaskStatus {
            task: task.to_string(),
            message: String::from("No longer waiting"),
            progress: status.progress,
        });
    }

    /// Starts an agent on a queued task through the caller's spawn path.
    fn start_task(&self, task: &str) -> Result<StepReport, String> {
        self.spawn_attempt(
            &Step::Start {
                task: task.to_string(),
            },
            task,
        )
    }

    /// Sends a task whose review failed back for another attempt.
    ///
    /// The attempt is started by the caller's spawn path, which is the one
    /// `shprd agent spawn --task` and a start use: the failed attempt is
    /// archived, and the new brief carries "why the previous attempt was sent
    /// back" from the review gate the log holds, so nothing about spawning or
    /// about the findings is written a second time here.
    ///
    /// A send-back that does not start is escalated once, in the task's name:
    /// the planner stops planning the same failure, so without the card the
    /// task would sit at `Failed` for ever with nothing in the ledger saying
    /// why. The card names the one verb that recovers it.
    fn send_back(&self, state: &RunState, task: &str, attempt: u32) -> Result<StepReport, String> {
        let step = Step::SendBack {
            task: task.to_string(),
            attempt,
        };
        match (self.start)(&self.repo, task) {
            Ok(detail) => self.report_spawn(&step, detail),
            Err(reason) => {
                let told = self.escalate_send_back_failure(state, task, attempt, &reason);
                match told {
                    Ok(()) => Err(reason),
                    Err(escalation) => Err(format!(
                        "{reason}; the escalation could not be recorded either: {escalation}"
                    )),
                }
            }
        }
    }

    /// Puts a task whose review rounds are spent in front of a person, once.
    ///
    /// The verdict decision normally records this card as it decides the last
    /// round; the planner plans this step for a failure that reached the log any
    /// other way, so the run asks its person under the service's tick alone —
    /// with no coordinator to tell and nothing typed into a terminal. The body
    /// says what failed and names the verb behind each choice.
    fn escalate_review(
        &self,
        state: &RunState,
        task: &str,
        body: &str,
        default_action: &str,
    ) -> Result<StepReport, String> {
        let step = Step::Escalate {
            task: task.to_string(),
            body: body.to_string(),
            default_action: default_action.to_string(),
        };
        let found = state
            .tasks
            .get(task)
            .ok_or_else(|| format!("no task {task:?} in this run"))?;
        let attempt = found
            .latest_attempt()
            .ok_or_else(|| format!("task {task:?} has no attempt to decide"))?
            .n;
        let mut last = None;
        // One card per failure: the marker names the attempt and its failed
        // rounds, and the verdict decision writes the same one, so the two
        // never make two cards.
        let marker = shprd_core::gate::rounds_spent_prefix(
            task,
            attempt,
            shprd_core::gate::failed_review_rounds(found),
        );
        let told = state.escalations.iter().any(|escalation| {
            escalation.task.as_deref() == Some(task) && escalation.body.starts_with(&marker)
        });
        if !told {
            let seq = self.commit(EventKind::Escalated {
                task: Some(task.to_string()),
                body: body.to_string(),
                default_action: default_action.to_string(),
            })?;
            self.notify(Some(seq));
            last = Some(seq);
        }
        // The choices are a card the person answers through the API, which is
        // what makes the escalation something a board can act on rather than
        // read. It is raised once, by the id that names the attempt.
        let ask = shprd_core::gate::rounds_spent_ask(attempt);
        if !state.asks.contains_key(&(task.to_string(), ask.clone())) {
            let seq = self.raise_rounds_card(task, &ask, body)?;
            if seq.is_some() {
                last = seq;
            }
        }
        Ok(StepReport::done(&step, body.to_string(), last))
    }

    /// Writes the answerable card for a review that spent its rounds.
    ///
    /// Both halves are the ask machinery the CLI uses: the queue file, which is
    /// what `POST /api/asks/answer` claims, and the `AskRaised` event, which is
    /// what the fold, `/api/asks` and the derived status read. The card never
    /// expires, because the decision is the person's to make when they get to
    /// it, and its id names the failed attempt so one failure has one card.
    fn raise_rounds_card(&self, task: &str, id: &str, text: &str) -> Result<Option<u64>, String> {
        let options: Vec<String> = shprd_core::gate::ROUNDS_SPENT_OPTIONS
            .iter()
            .map(|choice| (*choice).to_string())
            .collect();
        self.raise_card(task, id, text, options)
    }

    /// Writes an answerable card with `options`: the queue file and the event.
    fn raise_card(
        &self,
        task: &str,
        id: &str,
        text: &str,
        options: Vec<String>,
    ) -> Result<Option<u64>, String> {
        let now_ms = SystemClock.now_ms();
        let ask = shprd_core::ask::Ask::new(
            id.to_string(),
            task.to_string(),
            shprd_core::ask::AskKind::Question,
            text.to_string(),
            now_ms,
        )
        .with_options(options.clone());
        let queue = shprd_core::ask::AskQueue::for_repo(&self.repo);
        match queue.raise(&ask) {
            Ok(()) | Err(shprd_core::ask::AskError::AlreadyRaised { .. }) => {}
            Err(err) => return Err(format!("writing the card: {err}")),
        }
        let event = match ledger::commit(
            &self.repo,
            &self.actor,
            EventKind::AskRaised {
                task: task.to_string(),
                ask: id.to_string(),
                ask_kind: shprd_core::ask::AskKind::Question,
                text: text.to_string(),
                options,
                created_at_ms: now_ms,
                expires_at_ms: None,
            },
        ) {
            Ok(event) => Some(event.seq),
            // Another writer raised it first: the card is there, which is what
            // the step wanted.
            Err(ledger::CommitError::Refused(RuleError::AskAlreadyRaised { .. })) => None,
            Err(err) => return Err(format!("recording the card: {err}")),
        };
        after_write(&self.after_write, &self.repo);
        Ok(event)
    }

    /// Runs the caller's spawn path for one attempt and reports what it did.
    ///
    /// The spawn records the attempt and its resources itself, so the step's
    /// sequence is the log's newest line rather than one this executor wrote.
    fn report_spawn(&self, step: &Step, detail: String) -> Result<StepReport, String> {
        let seq = self.last_seq();
        self.notify(seq);
        after_write(&self.after_write, &self.repo);
        Ok(StepReport::done(step, detail, seq))
    }

    /// Starts the attempt `step` names through the caller's spawn path.
    fn spawn_attempt(&self, step: &Step, task: &str) -> Result<StepReport, String> {
        let detail = (self.start)(&self.repo, task)?;
        self.report_spawn(step, detail)
    }

    /// Records once that a send-back the pass planned did not start.
    ///
    /// The body begins with [`shprd_core::gate::send_back_failure_prefix`], which
    /// the planner compares, so the same failed attempt costs one spawn attempt
    /// and one card rather than one of each every pass.
    fn escalate_send_back_failure(
        &self,
        state: &RunState,
        task: &str,
        attempt: u32,
        reason: &str,
    ) -> Result<(), String> {
        let marker = shprd_core::gate::send_back_failure_prefix(task, attempt);
        let already = state.escalations.iter().any(|escalation| {
            escalation.task.as_deref() == Some(task) && escalation.body.starts_with(&marker)
        });
        if already {
            return Ok(());
        }
        let seq = self.commit(EventKind::Escalated {
            task: Some(task.to_string()),
            body: format!("{marker}{reason}"),
            default_action: format!(
                "fix what the card names, then run `shprd agent spawn --task {task}`: the engine \
                 plans no second send-back for this attempt"
            ),
        })?;
        self.notify(Some(seq));
        Ok(())
    }

    /// Starts the reviewer on a task that handed in, through the caller's
    /// review path.
    ///
    /// It is `shprd agent review` with the same arguments the verb uses, so the
    /// cross-vendor rule, the packet, the brief and the kickoff are one code
    /// path, and the request it records is what stops the next pass repeating
    /// the step.
    ///
    /// A review that does not start is escalated once, in the task's own name:
    /// the request may already be in the log, which stops the next pass planning
    /// the step again, so without the escalation the task would sit at Reported
    /// for ever with nothing in the ledger saying why. The escalation names the
    /// failure and the one verb that recovers it.
    fn review_task(&self, state: &RunState, task: &str) -> Result<StepReport, String> {
        if let Some(report) = self.clash_before_review(state, task)? {
            return Ok(report);
        }
        let review = self
            .review
            .as_ref()
            .ok_or_else(|| String::from("no reviewer is configured"))?;
        let detail = match review(&self.repo, task) {
            Ok(detail) => detail,
            Err(reason) => {
                let escalated = self.escalate_review_failure(state, task, &reason);
                return match escalated {
                    Ok(()) => Err(reason),
                    Err(escalation) => Err(format!(
                        "{reason}; the escalation could not be recorded either: {escalation}"
                    )),
                };
            }
        };
        let step = Step::Review {
            task: task.to_string(),
        };
        let seq = self.last_seq();
        self.notify(seq);
        after_write(&self.after_write, &self.repo);
        Ok(StepReport::done(&step, detail, seq))
    }

    /// Sends a hand-in back before its review when its branch would clash with
    /// the integration branch as it stands now.
    ///
    /// The clash is recorded exactly as a landing records one (a failed
    /// `integrate` gate and a ledger note), so the planner redoes the task at
    /// once without spending a review round, and the person never Accepts work
    /// that cannot land. `None` when the branch merges cleanly or git cannot
    /// tell, and the review goes ahead.
    fn clash_before_review(
        &self,
        state: &RunState,
        task: &str,
    ) -> Result<Option<StepReport>, String> {
        let (Some(run), Some(found)) = (state.run.as_ref(), state.tasks.get(task)) else {
            return Ok(None);
        };
        let Some(attempt) = found.latest_attempt() else {
            return Ok(None);
        };
        let Some((clashing, files)) =
            land::clash_before_review(&self.repo, &run.integration_branch, &attempt.branch)
        else {
            return Ok(None);
        };
        // The note names the commit when git can resolve it, else the branch.
        let head = Git::new(&self.repo)
            .rev_parse(&attempt.branch)
            .unwrap_or_else(|_| attempt.branch.clone());
        let err = LandError::Conflict {
            commit: head,
            files,
            pr: None,
        };
        // The finding names the ref the work clashed with, so the redo merges
        // the very tip it has to land on, not a stale local ref.
        let seq = self.record_conflict(found, &clashing, &err)?;
        after_write(&self.after_write, &self.repo);
        let step = Step::Review {
            task: task.to_string(),
        };
        Ok(Some(StepReport::done(
            &step,
            conflict_message(task, &err),
            seq,
        )))
    }

    /// Passes a task on its own checks because review is off.
    ///
    /// Recorded as a `ReviewSkipped` event, so the log says the pass had no
    /// review behind it; the rules refuse it for anything but a clean hand-in
    /// that no review was requested for.
    fn skip_review(&self, state: &RunState, task: &str) -> Result<StepReport, String> {
        let step = Step::SkipReview {
            task: task.to_string(),
        };
        let attempt = state
            .tasks
            .get(task)
            .and_then(|found| found.latest_attempt())
            .ok_or_else(|| format!("task {task:?} has no attempt to pass"))?
            .n;
        let seq = self.commit(EventKind::ReviewSkipped {
            task: task.to_string(),
            attempt,
        })?;
        self.notify(Some(seq));
        Ok(StepReport::done(
            &step,
            format!("{task}: review is off, passed on its checks"),
            Some(seq),
        ))
    }

    /// Decides the review of a task whose reviewer has written its findings.
    ///
    /// The reviewer cannot record anything itself: it writes `findings.json` and
    /// stops. This is what turns that file into the gate result the run waits
    /// for, through the same [`shprd_core::gate::decide`] the `review-verdict`
    /// verb and the watchdog use, so the policy, the provenance and the events
    /// cannot drift between the three. The plan only offers a settled file, and
    /// the recorded gate result is what stops the next pass deciding it again.
    ///
    /// A file that is not a findings list is a reviewer's mistake, not a
    /// decision: it is escalated once, in the task's name, and left for a person.
    fn verdict_task(
        &self,
        config: &Config,
        state: &RunState,
        task: &str,
    ) -> Result<StepReport, String> {
        let step = Step::Verdict {
            task: task.to_string(),
        };
        let found = state
            .tasks
            .get(task)
            .ok_or_else(|| format!("no task {task:?} in this run"))?;
        let attempt = tick::awaiting_verdict(found)
            .ok_or_else(|| format!("task {task:?} has no review waiting on a verdict"))?;
        let run = state
            .run
            .as_ref()
            .ok_or_else(|| String::from("the run has not started"))?;
        let reviews = self.home.join(".shprd").join("reviews");
        let path = shprd_core::review::findings_path(&reviews, &run.run_id, task, attempt);
        let bytes =
            std::fs::read(&path).map_err(|err| format!("reading {}: {err}", path.display()))?;
        let findings = match shprd_core::review::parse_findings(&bytes, &path) {
            Ok(findings) => findings,
            Err(err) => {
                let seq = self.escalate_invalid_findings(state, task, &err.to_string())?;
                return Ok(StepReport::done(
                    &step,
                    format!("{task}: the reviewer's findings could not be read: {err}"),
                    seq,
                ));
            }
        };
        let source = gate::provenance(found, attempt, &bytes).ok_or_else(|| {
            format!(
                "no review was requested for {task} attempt {attempt}: a verdict needs a reviewer"
            )
        })?;
        // A round a person granted by answering the card is one more round the
        // decision may use before it asks again.
        let policy = shprd_core::config::Review {
            rounds: gate::allowance(state, found, config.review.rounds),
            must_fix: config.review.must_fix,
        };
        let decided = gate::decide(found, attempt, findings, source, policy);
        let mut last = None;
        for kind in decided.events {
            match self.commit_verdict(kind) {
                Ok(Some(seq)) => {
                    last = Some(seq);
                    self.notify(Some(seq));
                }
                // A `shprd watch`, or another process, decided this attempt's
                // review between this pass reading the log and appending the
                // verdict. The rules refuse a second verdict for one attempt, so
                // the loop stops here; closing the tab is still right, and the
                // run is in the state this step wanted.
                Ok(None) => {
                    self.close_reviewer_task(task, attempt);
                    return Ok(StepReport::done(
                        &step,
                        format!("{task}'s review was already decided"),
                        self.last_seq(),
                    ));
                }
                Err(reason) => return Err(reason),
            }
        }
        self.close_reviewer_task(task, attempt);
        let detail = if decided.passed {
            format!("{task}'s review passed")
        } else {
            format!(
                "{task}'s review failed: {} blocking finding(s)",
                decided.blocking
            )
        };
        Ok(StepReport::done(&step, detail, last))
    }

    /// Appends one verdict event, folding "already decided" into `Ok(None)`.
    ///
    /// The log refuses a second review verdict for one attempt, and that refusal
    /// is news about the run rather than a failure of this pass: another decider
    /// got there first.
    fn commit_verdict(&self, kind: EventKind) -> Result<Option<u64>, String> {
        match ledger::commit(&self.repo, &self.actor, kind) {
            Ok(event) => {
                after_write(&self.after_write, &self.repo);
                Ok(Some(event.seq))
            }
            Err(ledger::CommitError::Refused(RuleError::ReviewAlreadyDecided { .. })) => Ok(None),
            Err(err) => Err(err.to_string()),
        }
    }

    /// Closes the reviewer's tab after a verdict, logging a host that refuses.
    ///
    /// A close that fails is never fatal: the gate result is already recorded,
    /// and failing the step would report the review as undecided.
    fn close_reviewer_task(&self, task: &str, attempt: u32) {
        if let Some(close) = &self.close_reviewer
            && let Err(err) = close(&self.repo, task, attempt)
        {
            tracing::warn!(task, %err, "the reviewer's tab did not close after its verdict");
        }
    }

    /// Records once that a reviewer's findings file could not be read.
    ///
    /// The body begins with [`shprd_core::review::invalid_findings_prefix`], so a
    /// pass after the first finds the escalation and says nothing again. The
    /// planner reads the same marker, so the file is left for a person rather
    /// than decided over and over.
    fn escalate_invalid_findings(
        &self,
        state: &RunState,
        task: &str,
        reason: &str,
    ) -> Result<Option<u64>, String> {
        let marker = shprd_core::review::invalid_findings_prefix(task);
        let already = state.escalations.iter().any(|escalation| {
            escalation.task.as_deref() == Some(task) && escalation.body.starts_with(&marker)
        });
        if already {
            return Ok(None);
        }
        let seq = self.commit(EventKind::Escalated {
            task: Some(task.to_string()),
            body: format!("{marker}{reason}"),
            default_action: String::from(shprd_core::review::INVALID_FINDINGS_ACTION),
        })?;
        self.notify(Some(seq));
        Ok(Some(seq))
    }

    /// Records once that the engine's review of `task` did not start.
    ///
    /// The body begins with [`review_failure_prefix`], so a second failed pass
    /// finds the first escalation and says nothing again. The default action is
    /// the way forward for the state the failure left: a request may already be
    /// in the log (which stops the next pass planning the step, so only
    /// `--again` recovers it), or none is (so the engine retries on the next
    /// pass once the cause is fixed).
    fn escalate_review_failure(
        &self,
        state: &RunState,
        task: &str,
        reason: &str,
    ) -> Result<(), String> {
        // Every reviewer is out of quota: one plain card with the choices in
        // words, not an escalation naming commands.
        if let Some((_, why)) =
            reason.split_once(&format!("{}: ", shprd_core::review::NO_REVIEWER_LEFT))
        {
            let attempt = attempt_of(state, task);
            let prefix = format!("{}{attempt}-", shprd_core::review::STUCK_ASK_PREFIX);
            let open = state
                .asks
                .values()
                .any(|ask| ask.task == task && ask.id.starts_with(&prefix) && ask.answer.is_none());
            if open {
                return Ok(());
            }
            let n = state
                .asks
                .values()
                .filter(|ask| ask.task == task && ask.id.starts_with(&prefix))
                .count();
            let text = shprd_core::review::none_left_body(task, attempt, why);
            let id = shprd_core::review::stuck_ask(attempt, n + 1);
            return self
                .raise_card(task, &id, &text, shprd_core::review::stuck_options())
                .map(|_| ());
        }
        let marker = review_failure_prefix(task);
        let already = state.escalations.iter().any(|escalation| {
            escalation.task.as_deref() == Some(task) && escalation.body.starts_with(&marker)
        });
        if already {
            return Ok(());
        }
        let default_action = if self.review_requested(task) {
            format!(
                "fix what the escalation names, then run `shprd agent review --task {task} \
                 --again`: it closes the reviewer tab and starts a new review"
            )
        } else {
            format!(
                "fix what the escalation names; the engine retries the review on its next \
                 pass, or run `shprd agent review --task {task}` by hand"
            )
        };
        let seq = self.commit(EventKind::Escalated {
            task: Some(task.to_string()),
            body: format!("{marker}{reason}"),
            default_action,
        })?;
        self.notify(Some(seq));
        Ok(())
    }

    /// Whether the log, as the failed review left it, holds a request for the
    /// task's newest attempt.
    ///
    /// The state a pass was planned from is older than the failed review, so it
    /// cannot answer this: the review records its request before the step that
    /// fails. A log that cannot be read answers `false`, which only makes the
    /// escalation suggest a plain review.
    fn review_requested(&self, task: &str) -> bool {
        ledger::load(&self.repo)
            .ok()
            .and_then(|(latest, _)| {
                latest
                    .tasks
                    .get(task)
                    .and_then(|task| task.latest_attempt())
                    .map(|attempt| !attempt.reviews.is_empty())
            })
            .unwrap_or(false)
    }

    /// Lands a task through its own pull request, as `shprd land` does.
    ///
    /// The composition is the same one the verb uses: the rule is re-asked here
    /// (a pass may have been planned from a log that has since moved), the
    /// branch's head is read, an approval counts only for that exact commit, and
    /// every outcome is recorded once per head. The policy is the one the plan
    /// decided the landing under — the pass's own settings resolution asked about
    /// the branch the landing writes — so the report and the planner never
    /// disagree about who merged what.
    fn land_task(
        &self,
        state: &RunState,
        task: &str,
        policy: MergePolicy,
    ) -> Result<StepReport, LandFail> {
        let step = Step::Land {
            task: task.to_string(),
            policy,
        };
        let found = state
            .tasks
            .get(task)
            .ok_or_else(|| format!("no task {task:?} in this run"))?;
        if matches!(
            found.status,
            shprd_core::state::TaskStatus::Integrated | shprd_core::state::TaskStatus::Dropped
        ) {
            // Already landed: record nothing and open no pull request.
            return Ok(StepReport::done(&step, "already landed", None));
        }
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
        let head = git
            .head_sha()
            .map_err(|err| LandFail::at(task, "reading the branch tip", err))?;
        let approved = found
            .approved_head
            .as_deref()
            .is_some_and(|a| shprd_core::tick::same_commit(a, &head));
        let has_work = attempt
            .report
            .as_ref()
            .and_then(|report| report.commit.as_ref())
            .is_some()
            || found.pr.is_some();
        if self.local {
            return self.land_locally(
                &step,
                found,
                &attempt.branch,
                run,
                policy,
                approved,
                has_work,
            );
        }
        let base = run.base_branch.clone();
        let title = format!("{task}: {}", found.title);
        let body = write_body(
            &self.home,
            &run.run_id,
            task,
            &pr::task_pr_body_flagged(
                found,
                &shprd_git::changed::protected_for_task(&self.repo, state, found),
            ),
        )
        .map_err(|err| LandFail::at(task, "writing the pull request text", err))?;
        let forge = self.forge();
        let landing = Landing {
            task,
            branch: &attempt.branch,
            base: &base,
            title: &title,
            body: &body,
            has_work,
        };
        let landed = match land::land(forge, &git, &landing, policy, approved) {
            Ok(landed) => landed,
            // A branch that clashes with what landed after it is closed out and
            // sent back for a fresh attempt: the planner does the sending.
            Err(err @ LandError::Conflict { .. }) => {
                if let LandError::Conflict {
                    pr: Some(number), ..
                } = &err
                {
                    // Best effort: a pull request that cannot be closed is left
                    // for a human, and the new attempt opens its own.
                    let _ = forge.close(*number, &stale_pr_comment(task));
                }
                // Not a failure: the clash is recorded as the gate the planner
                // sends the task back on, so this pass has done its job.
                let seq = self.record_conflict(found, &base, &err)?;
                return Ok(StepReport::done(&step, conflict_message(task, &err), seq));
            }
            Err(err) => return Err(land_fail(err, task)),
        };
        let seq = self.record_landing(state, task, &base, &landed)?;
        Ok(StepReport::done(
            &step,
            landed_line(task, &base, &landed),
            seq,
        ))
    }

    /// Lands a task into the run's integration branch, with no forge involved.
    ///
    /// The worktree is shprd's own integration worktree on
    /// `run.integration_branch`, so the user's checkout is never touched. The
    /// policy is the same one the forge path applies; `checks` reads the
    /// hand-in's own checks report because there is no CI to ask.
    #[expect(
        clippy::too_many_arguments,
        reason = "one local landing's whole input: the step, the task, its branch, the run, the policy and what the git read found"
    )]
    fn land_locally(
        &self,
        step: &Step,
        task: &Task,
        branch: &str,
        run: &shprd_core::state::RunInfo,
        policy: MergePolicy,
        approved: bool,
        has_work: bool,
    ) -> Result<StepReport, LandFail> {
        let repo_name = self
            .repo
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .ok_or_else(|| {
                format!(
                    "cannot name the repository: {} has no last path component",
                    self.repo.display()
                )
            })?;
        let layout = Layout::from_settings(&self.home, &self.repo);
        let worktree = integrate::open_integration_branch(
            &self.repo,
            &layout,
            &repo_name,
            &run.run_id,
            &run.integration_branch,
            &run.base_branch,
        )
        .map_err(|err| open_fail(&task.id, &run.integration_branch, &err))?;
        self.record_integration_worktree(&worktree);
        let landing = LocalLanding {
            task: &task.id,
            branch,
            base: &run.integration_branch,
            has_work,
        };
        let result = land::land_local(
            &worktree,
            &landing,
            policy,
            approved,
            local_checks_passed(task),
        );
        // Whatever the landing did, the worktree lets go of the branch so the
        // person can `git switch` to it in their own checkout between lands.
        if let Err(err) = integrate::release_integration(&worktree) {
            tracing::warn!(%err, path = %worktree.path.display(), "could not detach the integration worktree");
        }
        let landed = match result {
            Ok(landed) => landed,
            Err(err @ LandError::Conflict { .. }) => {
                let seq = self.record_conflict(task, &run.integration_branch, &err)?;
                return Ok(StepReport::done(
                    step,
                    conflict_message(&task.id, &err),
                    seq,
                ));
            }
            Err(err) => return Err(land_fail(err, &task.id)),
        };
        let seq = self.record_local_landing(&task.id, &run.integration_branch, &landed)?;
        Ok(StepReport::done(
            step,
            local_landed_line(&task.id, &run.integration_branch, &landed),
            seq,
        ))
    }

    /// Records a local landing's outcome once.
    ///
    /// A local landing has no pull request, so there is no `TaskPrOpened` to
    /// record. The outcome is idempotent for the same reason the forge path's
    /// is: a landing that finished moves the task to `Integrated`, and the
    /// planner then has nothing left to land.
    fn record_local_landing(
        &self,
        task: &str,
        base: &str,
        landed: &land::LocalLanded,
    ) -> Result<Option<u64>, String> {
        let Some(outcome) = landed.outcome() else {
            return Ok(None);
        };
        self.record_landed(task, base, landed.head(), None, outcome)
            .map(Some)
    }

    /// Records the run's integration worktree in the resource ledger, once, so
    /// the janitor can list it and take it away.
    fn record_integration_worktree(&self, worktree: &integrate::IntegrationWorktree) {
        let name = worktree.path.display().to_string();
        let held = ledger::load(&self.repo).is_ok_and(|(state, _)| {
            state
                .resources
                .get(&(ResourceKind::Worktree, name.clone()))
                .is_some_and(|record| record.status == ResourceStatus::Live)
        });
        if held {
            return;
        }
        let recorded = self.commit(EventKind::Resource {
            action: ResourceAction::Created,
            kind: ResourceKind::Worktree,
            name,
            task: None,
        });
        if let Err(err) = recorded {
            tracing::warn!(%err, "the integration worktree could not be recorded");
        }
    }

    /// Records a landing that clashed with the integration branch as a failed
    /// `integrate` gate.
    ///
    /// The failed gate takes the task out of `Passed`, so the next pass does not
    /// try the same merge again, and it is what the planner reads to send the
    /// task back as a new attempt. The ledger note is not a question: nobody has
    /// to decide anything.
    fn record_conflict(
        &self,
        task: &Task,
        base: &str,
        err: &LandError,
    ) -> Result<Option<u64>, String> {
        let LandError::Conflict { files, .. } = err else {
            return Ok(None);
        };
        let attempt = task.attempts.last().map(|attempt| attempt.n).unwrap_or(1);
        let failed = self.commit(EventKind::GateFailed {
            task: task.id.clone(),
            attempt,
            gate: shprd_core::rules::LANDING_CONFLICT_GATE.to_string(),
            findings: vec![land::conflict_finding(base, files)],
            source: None,
        })?;
        self.notify(Some(failed));
        let escalated = self.commit(EventKind::Escalated {
            task: Some(task.id.clone()),
            body: conflict_message(&task.id, err),
            default_action: String::from("redo the task on top of the latest"),
        })?;
        self.notify(Some(escalated));
        Ok(Some(failed))
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
                self.notify(Some(event));
            }
        }
        if let Some(outcome) = landed.outcome() {
            last = Some(self.record_landed(task, base, landed.head(), landed.number(), outcome)?);
        }
        Ok(last)
    }

    /// Records that a task landed, and closes the space that carried it.
    ///
    /// One record for all three ways a task lands — a forge merge, a local
    /// merge into the integration branch, and a merge someone made outside
    /// shprd — because the space question is the same for all of them: a landed
    /// task cannot come back to it. The close is logged when the host refuses,
    /// as the reviewer's is, since the landing is already in the log.
    fn record_landed(
        &self,
        task: &str,
        base: &str,
        head: &str,
        number: Option<u64>,
        outcome: LandOutcome,
    ) -> Result<u64, String> {
        let event = self.commit(EventKind::TaskLanded {
            task: task.to_string(),
            base: base.to_string(),
            head: head.to_string(),
            number,
            outcome,
        })?;
        self.notify(Some(event));
        self.close_worker_task(task);
        // A landing is what makes shprd rebuild itself, when the person asked
        // for that: the trigger reads the setting and starts the helper, and is
        // nothing where no service installed one.
        if outcome.is_landed() {
            crate::self_update::landed(&self.repo, base, self.local);
        }
        Ok(event)
    }

    /// Closes a landed task's space, logging a host that refuses.
    ///
    /// A close that fails is never fatal: the landing is recorded, and failing
    /// the step would report a merge that already happened as a failure. Run end
    /// sweeps whatever a refusal leaves behind.
    fn close_worker_task(&self, task: &str) {
        if let Some(close) = &self.close_worker
            && let Err(err) = close(&self.repo, task)
        {
            tracing::warn!(task, %err, "the task's space did not close after its landing");
        }
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
        let event = self.record_landed(task, &run.base_branch, &head, number, outcome)?;
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
                    entry.backups.push(BackupDone {
                        from: record.agent.clone(),
                        to: record.to_agent.clone().unwrap_or_default(),
                        started: record.started.unwrap_or(false),
                    });
                }
            }
        }
    }
    ladder
}

/// What a realign outcome writes about the run's diverged-branch card.
///
/// - `Kept`: one run-wide card naming the local-only commits, per distinct set.
///   An open card or a person's dismissal of the same words stands (a dismissal
///   sticks); only a card the engine itself retired is raised again.
/// - `Moved` or `Aligned`: the cause is gone, so every open card of the branch is
///   retired by the engine on this first pass.
/// - `Unchanged`: nothing was learned, so nothing is written.
fn diverged_events(branch: &str, outcome: &integrate::Realign, state: &RunState) -> Vec<EventKind> {
    let marker = diverged_marker(branch);
    match outcome {
        integrate::Realign::Kept(commits) => {
            let body = format!("{marker}: {}", commits.join("; "));
            if state.escalations.iter().any(|escalation| {
                escalation.body == body && !state.retired_needs.contains(&escalation.seq)
            }) {
                return Vec::new();
            }
            let default_action = format!(
                "push `{branch}` to the remote if those commits should land, or move it back \
                 with `git branch -f {branch} origin/{branch}` if they should not; shprd leaves \
                 the local branch as it is"
            );
            vec![EventKind::Escalated {
                task: None,
                body,
                default_action,
            }]
        }
        integrate::Realign::Moved | integrate::Realign::Aligned => state
            .escalations
            .iter()
            .filter(|escalation| {
                escalation.task.is_none()
                    && !escalation.by_agent
                    && escalation.body.starts_with(&marker)
                    && !state.dismissed_needs.contains(&escalation.seq)
            })
            .map(|escalation| EventKind::NeedDismissed {
                need: escalation.seq,
            })
            .collect(),
        integrate::Realign::Unchanged => Vec::new(),
    }
}

/// The words every "local branch has commits the remote lacks" card begins with.
fn diverged_marker(branch: &str) -> String {
    format!("The local `{branch}` has commits the remote lacks")
}

/// What every escalation body for a review the engine could not start begins
/// with.
///
/// It is the marker a later pass compares, so one failed review is one
/// escalation rather than one per pass.
fn review_failure_prefix(task: &str) -> String {
    format!("{task}: the engine's review did not start: ")
}

/// What every escalation body for a step that keeps failing begins with.
///
/// It names the task, the step and the attempt the failure was about, so the
/// next pass recognises its own card and writes no second one, a failure of a
/// different step is a card of its own, and a new attempt after the fix is a
/// card of its own too. It is shaped like
/// [`shprd_core::gate::send_back_failure_prefix`], so a board's staleness rule
/// reads it the same way: the card is history once a newer attempt exists.
pub fn repeated_failure_prefix(task: &str, step: &str, attempt: u32) -> String {
    format!(
        "{task}: its `{step}` step has failed the same way {} passes in a row ({}): ",
        pass::REPEATS_BEFORE_A_CARD,
        shprd_core::event::attempt_label(task, attempt)
    )
}

/// How many attempts the task has had, as the fold counts them.
///
/// A repeated-failure card names it, so the card is about the attempt the
/// failure happened in and leaves the board when the work moves on.
fn attempt_of(state: &RunState, task: &str) -> u32 {
    state
        .tasks
        .get(task)
        .and_then(|task| task.latest_attempt().map(|attempt| attempt.n))
        .unwrap_or(0)
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

/// The line a local landing prints.
fn local_landed_line(task: &str, base: &str, landed: &land::LocalLanded) -> String {
    match landed {
        land::LocalLanded::Merged { head } => {
            format!(
                "merged {task} into {base} at {}",
                &head[..head.len().min(7)]
            )
        }
        land::LocalLanded::AlreadyMerged { .. } => format!("{task} is already in {base}"),
        land::LocalLanded::SquashMerged { .. } => {
            format!("{task}'s work is already in {base}, as other commits")
        }
        land::LocalLanded::NothingToMerge { .. } => {
            format!("nothing to merge: {task}'s branch has no work beyond {base}")
        }
        land::LocalLanded::Waiting { reason, .. } => reason.local_advice(task),
    }
}

/// Whether the task's own hand-in says its checks passed.
///
/// A repository with no forge has no CI to ask, so the hand-in's checks are the
/// local report the `checks` policy reads.
fn local_checks_passed(task: &Task) -> bool {
    task.latest_attempt()
        .and_then(|attempt| attempt.report.as_ref())
        .is_some_and(|report| {
            !report.checks.is_empty()
                && report.checks.iter().all(|check| check.exit_code == Some(0))
        })
}

/// A landing error as a failure: the plain line, and the real cause for the log.
fn land_fail(err: LandError, task: &str) -> LandFail {
    LandFail {
        line: Some(err.plain(task)),
        cause: err.cause(),
    }
}

/// The words a landing that could not open the integration worktree starts its
/// failure with. A retry of that step cannot succeed until a person acts, and
/// [`escalate_repeated_failure`](Service::escalate_repeated_failure) reads this
/// to say so.
const OPEN_FAIL: &str = "git would not open shprd's integration worktree";

/// What a person can do about a step that keeps failing: a land that could not
/// open the integration worktree will not clear by itself, so it never says the
/// next pass retries it.
fn repeated_failure_action(step: &str, error: &str, state_dir: &Path) -> String {
    if step == "land" && error.contains(OPEN_FAIL) {
        format!(
            "this will not clear by itself: free the branch (switch the checkout that holds it to another branch) or start the run on another integration branch; the pass after that lands it. Git's own words are above; the full error is in the log of `shprd serve` and the pass record under {}",
            state_dir.display()
        )
    } else if step == "land" {
        String::from("the merge is tried again on the next pass")
    } else {
        format!("fix what the card names, then the engine retries `{step}` on its next pass")
    }
}

/// The failure of opening the integration worktree: git's own reason in the
/// line a person reads, and the full error for the log.
fn open_fail(task: &str, branch: &str, err: &integrate::IntegrateError) -> LandFail {
    let text = err.to_string();
    let reason = text
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .unwrap_or("no reason given");
    LandFail {
        line: Some(format!(
            "Couldn't merge {task}: {OPEN_FAIL} for {branch:?}: {reason}"
        )),
        cause: land::tail_lines(&format!("opening the integration worktree: {text}")),
    }
}

/// What a clashing landing says: the plain sentence, then the files.
fn conflict_message(task: &str, err: &LandError) -> String {
    match err {
        LandError::Conflict { files, .. } if !files.is_empty() => format!(
            "{} (files in conflict: {})",
            err.plain(task),
            files.join(", ")
        ),
        _ => err.plain(task),
    }
}

/// What the stale pull request is closed with.
fn stale_pr_comment(task: &str) -> String {
    format!(
        "{task} clashed with work that landed after it was cut. It is being redone on top \
         of the latest, and the new attempt opens its own pull request."
    )
}

/// The settle window `tasks.news_settle_secs` names, in ms, when some settings
/// layer other than the built-in one sets it.
///
/// [`None`] leaves the window the news desk was wired with, so a caller that
/// handed a pass its own desk (a test, say) keeps it until someone sets the key.
pub fn settle_override_ms(home: &Path, repo: &Path) -> Option<u64> {
    use shprd_core::settings::{Key, Source, TaskKey};

    let loaded = shprd_core::settings::Loaded::read(home, Some(repo)).ok()?;
    let resolved = loaded.resolve(None);
    let named = resolved
        .get(&Key::Task(TaskKey::NewsSettleSecs))
        .is_some_and(|sourced| sourced.source != Source::BuiltIn);
    named.then(|| resolved.tasks().news_settle_secs.saturating_mul(1_000))
}

/// The config a pass runs under, and how long the loop should wait before the
/// next one.
///
/// `tasks.merge`, `tasks.max_running`, the review rounds and must-fix are what
/// the person sees in the brief's Settings block and in `shprd settings show`,
/// so they are what the pass must act on. Only a key a layer actually names
/// wins: a settings file that says nothing about the merge policy leaves the
/// resolved config's own answer standing, which is what a test that hands a pass
/// its config directly expects. The settings resolution already reads the old
/// `config.toml` keys as its lowest named layer, so a run whose person has not
/// migrated keeps the values its config file asks for. A settings file that
/// cannot be read is not a reason to stop the pass: the resolved config's own
/// values stand, and the next pass reads the file again.
///
/// The interval is returned beside the config because it is the one workflow
/// value that is not the pass's own business: `tasks.tick_secs` decides how long
/// the *loop* waits, so the loop has to be told what this pass read (M10 gate bug
/// 4). It comes back `None` when no settings layer names it, so a service started
/// with a `[tick] interval_secs` keeps that value. `shprd serve` calls this too,
/// so the banner prints the interval the first pass will act on rather than only
/// the one its config file named.
pub fn pass_config(home: &Path, repo: &Path, base: &Config) -> (Config, Option<u64>) {
    use shprd_core::settings::{Key, Source, TaskKey};

    let Ok(loaded) = shprd_core::settings::Loaded::read(home, Some(repo)) else {
        return (base.clone(), None);
    };
    let resolved = loaded.resolve(None);
    let tasks = resolved.tasks();
    // Every key resolves, because the built-in layer sets them all: a key is
    // "set" only when a layer other than the defaults named it.
    let named = |key: TaskKey| {
        resolved
            .get(&Key::Task(key))
            .is_some_and(|sourced| sourced.source != Source::BuiltIn)
    };
    let mut config = base.clone();
    let mut interval = None;
    if named(TaskKey::Merge) {
        config.land.merge = tasks.merge;
    }
    if named(TaskKey::MaxRunning) {
        config.tick.max_running = tasks.max_running;
    }
    // Without worktrees every task would work in the one checkout, so the pass
    // starts them one at a time whatever `tasks.max_running` says.
    if !tasks.worktrees {
        config.tick.max_running = config.tick.max_running.min(1);
    }
    // The review's policy is what the send-back and the out-of-rounds card are
    // decided by, so the person's own answer to the brief's `Review rounds` line
    // is what the pass must act on.
    if named(TaskKey::ReviewRounds) {
        config.review.rounds = tasks.review_rounds;
    }
    if named(TaskKey::MustFix) {
        config.review.must_fix = tasks.must_fix;
    }
    if named(TaskKey::TickSecs) {
        config.tick.interval_secs = tasks.tick_secs;
        interval = Some(tasks.tick_secs);
    }
    (config, interval)
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
///
/// `interval` is only the starting wait: the settings are read on every pass, so
/// a person who sets `tasks.tick_secs` (or the older `[tick] interval_secs`) gets
/// it on the wait after the next pass with no restart (M10 gate bug 4). Each
/// pass answers the interval it read in
/// [`TickReport::interval_secs`](crate::tick::TickReport::interval_secs), and the
/// loop sleeps that, falling back to `interval` when the pass named none.
pub async fn run(
    worker: Arc<Worker>,
    interval: Duration,
    notice: Option<crate::service::PassNotice>,
    shutdown: impl std::future::Future<Output = ()> + Send,
) {
    let permit = Arc::new(tokio::sync::Mutex::new(()));
    let mut wait = interval;
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
                Ok(Ok(report)) => {
                    tracing::info!(
                        steps = report.steps.len(),
                        ok = report.ok(),
                        "tick pass finished"
                    );
                    wait = report.next_interval(interval);
                    if let Some(notice) = &notice {
                        notice(&report);
                    }
                }
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
            _ = tokio::time::sleep(wait) => {}
            _ = &mut shutdown => return,
        }
    }
}

/// What a card's status line begins with while a step is refused.
const WAITING_PREFIX: &str = "Waiting: ";

/// The status line for a step that failed with `reason`.
fn waiting_message(reason: &str) -> String {
    let line = format!(
        "{WAITING_PREFIX}{}",
        reason.split_whitespace().collect::<Vec<_>>().join(" ")
    );
    if line.chars().count() <= shprd_core::event::ENGINE_STATUS_MAX_CHARS {
        return line;
    }
    let mut capped: String = line
        .chars()
        .take(shprd_core::event::ENGINE_STATUS_MAX_CHARS - 1)
        .collect();
    capped.push('\u{2026}');
    capped
}

#[cfg(test)]
mod waiting_tests {
    use super::{OPEN_FAIL, open_fail, repeated_failure_action, waiting_message};

    #[test]
    fn another_refusal_carries_its_reason_capped() {
        assert_eq!(waiting_message("no host"), "Waiting: no host");
        assert!(waiting_message(&"x".repeat(500)).chars().count() <= 120);
    }

    #[test]
    fn an_unfixable_open_failure_never_says_it_is_tried_again() {
        let err = shprd_git::integrate::IntegrateError::Git(shprd_git::git::GitError::Failed {
            args: vec![String::from("worktree"), String::from("add")],
            code: Some(128),
            stderr: String::from("fatal: 'prototype' is already checked out at '/repo'"),
        });
        let line = open_fail("T1", "prototype", &err).line.unwrap_or_default();
        let dir = std::path::Path::new("/state");
        let action = repeated_failure_action("land", &line, dir);
        assert!(!action.contains("tried again"), "{action}");
        assert!(action.contains("will not clear by itself"), "{action}");
        assert!(action.contains("/state"), "{action}");
        let other = repeated_failure_action("land", "Couldn't merge T1: boom", dir);
        assert!(other.contains("tried again on the next pass"), "{other}");
    }

    #[test]
    fn a_failed_open_of_the_integration_worktree_says_gits_reason_and_that_a_retry_cannot_fix_it() {
        let err = shprd_git::integrate::IntegrateError::Git(shprd_git::git::GitError::Failed {
            args: vec![String::from("worktree"), String::from("add")],
            code: Some(128),
            stderr: String::from(
                "Preparing worktree (checking out 'prototype')\nfatal: 'prototype' is already checked out at '/repo'",
            ),
        });
        let fail = open_fail("T1", "prototype", &err);
        let line = fail.line.unwrap_or_default();
        assert!(line.starts_with("Couldn't merge T1: "), "{line}");
        assert!(line.contains(OPEN_FAIL), "{line}");
        assert!(line.contains("already checked out at '/repo'"), "{line}");
        assert!(!line.contains("in the shprd log"), "{line}");
        assert!(fail.cause.contains("opening the integration worktree"));
    }
}

#[cfg(test)]
mod diverged_tests {
    use super::diverged_events;
    use shprd_core::event::EventKind;
    use shprd_core::state::{Escalation, RunState};
    use shprd_git::integrate::Realign;

    const BODY: &str = "The local `main` has commits the remote lacks: abc1234 fix";

    fn card(seq: u64) -> Escalation {
        Escalation {
            task: None,
            body: BODY.to_string(),
            default_action: "push".to_string(),
            seq,
            by_agent: false,
        }
    }

    fn kept() -> Realign {
        Realign::Kept(vec!["abc1234 fix".to_string()])
    }

    #[test]
    fn a_diverged_ref_raises_one_card_and_the_same_set_is_not_told_again() {
        let mut state = RunState::default();
        let first = diverged_events("main", &kept(), &state);
        assert!(
            matches!(&first[..], [EventKind::Escalated { task: None, body, .. }] if body == BODY)
        );
        state.escalations.push(card(5));
        assert!(diverged_events("main", &kept(), &state).is_empty());
    }

    #[test]
    fn a_dismissal_sticks_for_the_same_commits_and_a_new_set_is_a_new_card() {
        let mut state = RunState::default();
        state.escalations.push(card(5));
        state.dismissed_needs.insert(5);
        assert!(diverged_events("main", &kept(), &state).is_empty());
        let other = Realign::Kept(vec!["def5678 other".to_string()]);
        assert_eq!(diverged_events("main", &other, &state).len(), 1);
    }

    #[test]
    fn the_engine_retires_the_card_once_the_cause_is_gone() {
        let mut state = RunState::default();
        state.escalations.push(card(5));
        for gone in [Realign::Moved, Realign::Aligned] {
            let events = diverged_events("main", &gone, &state);
            assert!(matches!(
                &events[..],
                [EventKind::NeedDismissed { need: 5 }]
            ));
        }
        // Not learned anything: nothing is written.
        assert!(diverged_events("main", &Realign::Unchanged, &state).is_empty());
        // Already dismissed: not dismissed twice.
        state.dismissed_needs.insert(5);
        assert!(diverged_events("main", &Realign::Aligned, &state).is_empty());
    }

    #[test]
    fn a_card_the_engine_retired_is_raised_again_when_the_cause_returns() {
        let mut state = RunState::default();
        state.escalations.push(card(5));
        state.dismissed_needs.insert(5);
        state.retired_needs.insert(5);
        assert_eq!(diverged_events("main", &kept(), &state).len(), 1);
    }
}
