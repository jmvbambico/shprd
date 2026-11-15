//! Derived task status: the eight words a person sees, and why a task says one.
//!
//! Ported from Shepherdr.
//!
//! - Source: <https://github.com/eliasstravik/shepherdr>
//! - Paths: `mods/tasks/bin/herdr_task/agents.py` (`task_status()` and the
//!   helpers it uses) and `core/tasks.js` (the window's copy of the same
//!   rules, `place()`)
//! - Commit: `a3f327a` (tag `v3.1.34`)
//! - License: MIT — Copyright (c) 2026 Elias Stravik
//!
//! Shepherdr never stores what a person sees: it stores six states and derives
//! eight words from them plus each agent terminal's own `working | blocked |
//! idle | done` status (`agents.py:102-170`). This module is that function for
//! shprd: a pure function of the fold ([`TaskSignal`], built from
//! [`shprd_core::state::RunState`]) and a live-agent overlay ([`LiveAgents`])
//! that the multiplexer and the watchdog pass in as data. No I/O, no clock, no
//! model.
//!
//! Every rule cites the Shepherdr line it comes from. Where shprd's state is
//! spelled differently, the mapping is stated in the rule's comment and in
//! [`TaskSignal::from_state`].

use std::collections::BTreeMap;

use shprd_core::ask::AskKind;
use shprd_core::event::ReportStatus;
use shprd_core::state::{RunState, Task, TaskStatus};

use crate::clock;

/// How long a problem or a quiet agent lasts before the task reads Needs you,
/// ms (`agents.py:203`): an agent often ends a turn a moment before it says
/// ready or takes up a problem, so a task must not flicker at the person.
pub const GRACE_MS: u64 = 20_000;

/// How long after an agent was started a task still reads as starting, ms
/// (`agents.py:586`, `core/tasks.js:STARTING_MS`).
pub const STARTING_MS: u64 = 90_000;

/// How long an agent's own status line counts as "on it", ms (`agents.py:266`).
pub const ON_IT_MS: u64 = 30 * 60_000;

/// The most characters a row's second line may hold (`agents.py:298`).
pub const LINE_MAX: usize = 40;

/// What an agent says when it wants an answer (`agents.py:302`, `REASONS`).
pub const QUESTION: &str = "Question for you";

/// What a Claude Code terminal showing its folder-trust dialog says
/// (`agents.py:302-307`, `REASONS["trust"]`).
pub const TRUST: &str = "Asks to trust this folder";

/// What a task whose agent went quiet without saying ready says
/// (`agents.py:302`, `REASONS["reply"]`).
pub const REPLY: &str = "Waits for your reply";

/// What a task with no agent left says (`agents.py:173`, `STOPPED`).
pub const STOPPED: &str = "Agent stopped";

/// What a ready task whose agent took up work again says
/// (`agents.py:222`, `WORKING_AGAIN`).
pub const WORKING_AGAIN: &str = "Working again";

/// What a ready task doing its asked merge says (`agents.py:181`).
pub const MERGING: &str = "Merging";

/// What a ready task in the forge's merge queue says (`agents.py:101`).
pub const IN_QUEUE_LINE: &str = "In GitHub's merge queue";

/// What a ready task whose merge waits on a review says (`agents.py:102`).
pub const APPROVAL_LINE: &str = "Merges once it's approved";

/// What a ready task that merges itself says (`agents.py:181`).
pub const MERGES_ITSELF: &str = "Merges once its checks pass";

/// What a failed attempt says when nothing better is known.
pub const FAILED: &str = "Its attempt failed";

/// What a waiting task says when a dependency has to land first.
pub const WAITING: &str = "after";

/// What a dropped task says when no reason was recorded.
pub const DROPPED: &str = "Dropped";

/// The eight words a task row can show (Shepherdr `STATUSES`,
/// `agents.py:104-105`; `core/tasks.js:STATUS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DerivedStatus {
    /// Added, nothing started on it (`todo`).
    ToDo,
    /// Starts by itself at a time (`scheduled`).
    Scheduled,
    /// Waits for something before it may start (`queued`).
    Queued,
    /// An agent is on it, or the engine is finishing its work.
    Working,
    /// Nothing moves without the person.
    NeedsYou,
    /// Its agent is at rest and said ready; what is left is the person.
    Ready,
    /// Its merge is under way, and nothing about it is the person's.
    Merging,
    /// Finished, or it never will be.
    Done,
}

impl DerivedStatus {
    /// The words a row shows for it (`agents.py:104-105`, `STATUSES`).
    pub fn word(self) -> &'static str {
        match self {
            Self::ToDo => "To do",
            Self::Scheduled => "Scheduled",
            Self::Queued => "Queued",
            Self::Working => "Working",
            Self::NeedsYou => "Needs you",
            Self::Ready => "Ready",
            Self::Merging => "Merging",
            Self::Done => "Done",
        }
    }

    /// The glyph a row draws for it, one per column (`lib/dot.js` and
    /// `mods/tasks/pie/`, as the M7b spec lists them).
    ///
    /// Shepherdr draws these as 14 px SVGs: an amber ring with a wedge for
    /// working, a red dot for needs you, a green disc and tick for ready and
    /// done, a ring with a gap for merging, a ring with a clock for queued, a
    /// ring with a calendar for scheduled, and a plain ring for to do. A
    /// terminal has one cell, so each is one character that keeps the meaning
    /// and the order: a dot beats a wedge, a wedge beats a ring, a tick beats
    /// them all.
    pub fn glyph(self) -> char {
        match self {
            Self::ToDo => '○',
            Self::Scheduled => '▤',
            Self::Queued => '◷',
            Self::Working => '◐',
            Self::NeedsYou => '●',
            Self::Ready => '✓',
            Self::Merging => '↻',
            Self::Done => '✔',
        }
    }
}

/// A row for a task nothing has started on, which is what a view shows when a
/// reading is missing rather than failing.
impl Default for Derived {
    fn default() -> Self {
        Self::new(DerivedStatus::ToDo, None, None)
    }
}

/// What kind of Needs you a task is (`agents.py:106-170`, the `needs` field).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Need {
    /// Something shprd found that only the person can settle: failing checks,
    /// a conflict, a failed attempt.
    Attention,
    /// The agent asks something: a question tool, a permission, a folder it
    /// wants trusted.
    Asks,
    /// The agent went quiet without saying ready.
    Reply,
    /// No agent is left on it.
    Stopped,
    /// Its provider cut it off and the tries are used up.
    Provider,
}

/// One task as a person sees it: its word, what it needs, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Derived {
    /// The row's word.
    pub status: DerivedStatus,
    /// Why it needs the person, when it does.
    pub needs: Option<Need>,
    /// The row's second line, at most [`LINE_MAX`] characters.
    pub reason: Option<String>,
}

impl Derived {
    /// A row with the given word, need and reason.
    fn new(status: DerivedStatus, needs: Option<Need>, reason: Option<String>) -> Self {
        Self {
            status,
            needs,
            reason,
        }
    }

    /// Working, with what it waits on when it knows.
    fn working(reason: Option<&str>) -> Self {
        Self::new(DerivedStatus::Working, None, reason.map(ToOwned::to_owned))
    }

    /// Needs you, of the given kind, with a short reason.
    fn needs(needs: Need, reason: &str) -> Self {
        Self::new(DerivedStatus::NeedsYou, Some(needs), Some(fit(reason)))
    }
}

/// What an agent terminal reports now (Shepherdr's `hc` status,
/// `agents.py:80-94`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    /// It is at work.
    Working,
    /// It stopped to ask the person something (`agents.py:132-133`).
    Blocked,
    /// It ended its turn.
    Idle,
    /// It finished for good.
    Done,
}

/// What kind of terminal an agent is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentKind {
    /// The task's own worker.
    #[default]
    Worker,
    /// A reviewer the task itself runs: it never counts as the task's agent
    /// being at work (`agents.py:240-241`, `:271`).
    Reviewer,
    /// A worker's own helper (SHEP-54, `agents.py:51-58`): never one of the
    /// task's agents, but its work keeps its worker Working (`agents.py:894`).
    Helper,
}

/// An agent's own status line, set with `herdr-task status`
/// (`agents.py:458-470`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Progress {
    /// What the agent said it is doing, in its own words.
    pub text: String,
    /// When it set the line, ms since the epoch.
    pub set_ms: u64,
    /// The time it said it has left, ms, when it gave one.
    pub left_ms: Option<u64>,
    /// Whether shprd's own automation set the line rather than the agent
    /// (Shepherdr's `progress.by`, `agents.py:278`): its words are never the
    /// agent's reason and never put the agent "on it".
    pub by_herdr: bool,
}

/// One live agent terminal of a task.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LiveAgent {
    /// Its address, the key every per-agent record uses.
    pub address: String,
    /// What it reports now; `None` when it has not reported a status yet
    /// (`agents.py:150-152`).
    pub activity: Option<Activity>,
    /// Worker, reviewer or helper.
    pub kind: AgentKind,
    /// When it last changed status, ms since the epoch (Shepherdr's
    /// `status_ms`): what the [`GRACE_MS`] wait is counted from.
    pub since_ms: Option<u64>,
    /// Finished turns, as its terminal counts them (Shepherdr's `turn`): what
    /// tells work after a ready apart from the turn it said ready in
    /// (`agents.py:222-241`).
    pub turn: Option<u32>,
    /// Its own status line, when it set one.
    pub progress: Option<Progress>,
    /// Its own work still running: a background job, a helper of its own at
    /// work, a command open in the foreground (`agents.py:883-911`). The
    /// watchdog reads its log to know; it is never guessed here.
    pub own_work: bool,
    /// Its own status line says it waits on CI or a review, and its last
    /// message asks the person nothing (`agents.py:839-854`). Like
    /// [`LiveAgent::own_work`], this is the watchdog's reading of its log.
    pub waits_on_own: bool,
    /// A Claude Code folder-trust dialog is on its screen
    /// (`agents.py:305-321`). Only the reader of its screen sets this, and
    /// only for a Claude terminal.
    pub trust_dialog: bool,
}

/// The live overlay: every terminal of one task, right now.
///
/// A task the fold puts nowhere near a running agent is still passed an empty
/// overlay, never a stale one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LiveAgents {
    /// Every terminal of the task, its worker's own helpers included.
    pub agents: Vec<LiveAgent>,
}

impl LiveAgents {
    /// An overlay of the given terminals.
    pub fn new(agents: Vec<LiveAgent>) -> Self {
        Self { agents }
    }

    /// The task's own agents: its workers and reviewers, never a worker's
    /// helper (`agents.py:51-58` leaves helpers out of `live`).
    fn mine(&self) -> Vec<&LiveAgent> {
        self.agents
            .iter()
            .filter(|agent| agent.kind != AgentKind::Helper)
            .collect()
    }
}

/// A problem shprd found that only the person can settle (Shepherdr's
/// `attention` and `attention_at`, `agents.py:118-126`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attention {
    /// What it found, in a few words.
    pub reason: String,
    /// When it found it, ms since the epoch.
    pub at_ms: u64,
}

/// An agent cut off by its provider, which shprd retries
/// (Shepherdr's `retry`, `agents.py:127-131`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCut {
    /// The agent that was cut off.
    pub agent: String,
    /// Why, in the provider's few words.
    pub reason: String,
    /// Whether the tries are used up, so waiting can no longer fix it.
    pub over: bool,
    /// When the next try is due, ms since the epoch, when one is set.
    pub due_ms: Option<u64>,
}

/// One ask still waiting for the person, as the status rules see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskFacts {
    /// Question, permission or plan.
    pub kind: AskKind,
    /// What the person is asked.
    pub text: String,
    /// When it stops being open, ms since the Unix epoch, when it does.
    pub expires_at_ms: Option<u64>,
}

/// What a task's landing waits on (Shepherdr's `merge_requested`,
/// `merge_queued`, `auto_merge` and its `pr` record, `agents.py:184-193`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MergeFacts {
    /// Someone asked for this task's merge and its agent is doing it.
    pub requested: bool,
    /// It sits in the forge's merge queue.
    pub queued: bool,
    /// Its merge is automatic once the forge allows it.
    pub auto: bool,
    /// The forge wants a review it does not have yet.
    pub needs_approval: bool,
    /// Its pull request exists.
    pub has_pr: bool,
    /// The forge has merged it.
    pub merged: bool,
}

/// Everything shprd knows about one task as the status rules read it.
///
/// [`TaskSignal::from_state`] fills what the fold holds. The rest are facts the
/// fold drops (when an attempt started, the turns its agents were on when it
/// reported) or never had (the problem shprd found, a provider cut, what its
/// pull request waits on): the watchdog keeps those, as Shepherdr's task record
/// does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskSignal {
    /// Where the fold puts the task.
    pub status: TaskStatus,
    /// Dependencies that have not landed yet, by id: a task that may not start
    /// is Shepherdr's `queued` (`queues.py:163-173`).
    pub waiting_on: Vec<String>,
    /// When it starts by itself, ms since the epoch, when it does
    /// (Shepherdr's `scheduled_for`, `agents.py:108-110`).
    pub scheduled_for_ms: Option<u64>,
    /// When its newest attempt was started, ms since the epoch: what
    /// [`STARTING_MS`] is counted from (`agents.py:589-595`).
    pub started_ms: Option<u64>,
    /// What it waits on that needs nobody, in shprd's own words (Shepherdr's
    /// `pr["wait"]`, kept by its tick, `agents.py:193`): an attempt reported
    /// and its gates or review still to run reads Working, like a pull request
    /// with checks still running.
    pub wait: Option<String>,
    /// When its agent reported, ms since the epoch (Shepherdr's `ready_at`).
    pub ready_at_ms: Option<u64>,
    /// The first turn that was new work, per agent address, as of the report
    /// (Shepherdr's `ready_turns`, `agents.py:222-228`). `None` when the report
    /// predates the count, which falls back to [`STARTING_MS`].
    pub ready_turns: Option<BTreeMap<String, u32>>,
    /// The problem shprd found, when one is open.
    pub attention: Option<Attention>,
    /// When a problem was just handed to the agent, ms since the epoch
    /// (Shepherdr's `handed_at`).
    pub handed_at_ms: Option<u64>,
    /// The provider cut this task is waiting to retry, when there is one.
    pub provider_cut: Option<ProviderCut>,
    /// What its landing waits on.
    pub merge: MergeFacts,
    /// Why its newest attempt failed, in the agent's own words, when it is
    /// [`TaskStatus::Failed`].
    pub failure: Option<String>,
    /// Why a person dropped it, when one did.
    pub dropped_reason: Option<String>,
    /// Asks still waiting for the person, oldest first.
    ///
    /// The fold records every raised ask and its answer; the derived status
    /// shows the first one still open, so a card and the board agree without a
    /// second source of truth.
    pub open_asks: Vec<AskFacts>,
    /// The escalations this task still has in front of the person, oldest
    /// first, as [`crate::cards::card`] reads them.
    ///
    /// An escalation is the engine's own card — a review that spent its
    /// rounds, a review that could not start, a landing that conflicted — and
    /// it is the person's to settle exactly as an ask is. Reading it here is
    /// what makes `shprd status` and the board agree with `needs` instead of a
    /// row that says Working while a card sits unread beside it.
    pub cards: Vec<CardFacts>,
}

/// One escalation still in front of a person, as the status rules see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardFacts {
    /// What needs deciding, in the card's own words.
    pub body: String,
}

impl Default for TaskSignal {
    /// The facts of a task just added to the plan and nothing else.
    fn default() -> Self {
        Self {
            status: TaskStatus::Queued,
            waiting_on: Vec::new(),
            scheduled_for_ms: None,
            started_ms: None,
            wait: None,
            ready_at_ms: None,
            ready_turns: None,
            attention: None,
            handed_at_ms: None,
            provider_cut: None,
            merge: MergeFacts::default(),
            failure: None,
            dropped_reason: None,
            open_asks: Vec::new(),
            cards: Vec::new(),
        }
    }
}

impl TaskSignal {
    /// The task-side facts the fold holds for `id`, or `None` when the run has
    /// no such task.
    ///
    /// What the fold cannot give stays empty: an attempt's start time is the
    /// log's `Spawned` event, the turns its agents were on when it reported are
    /// the watchdog's, and a problem that has since been settled is not an
    /// escalation still sitting in the state. A caller with those facts sets
    /// the fields.
    pub fn from_state(state: &RunState, id: &str) -> Option<Self> {
        let task = state.tasks.get(id)?;
        Some(Self {
            status: task.status,
            waiting_on: waiting_on(state, task),
            wait: reported_wait(task),
            failure: failure_reason(task),
            dropped_reason: task.dropped_reason.clone(),
            open_asks: state
                .asks
                .values()
                .filter(|ask| ask.task == id && ask.answer.is_none())
                .map(|ask| AskFacts {
                    kind: ask.kind,
                    text: ask.text.clone(),
                    expires_at_ms: ask.expires_at_ms,
                })
                .collect(),
            cards: state
                .escalations
                .iter()
                .filter(|escalation| escalation.task.as_deref() == Some(id))
                .filter_map(|escalation| crate::cards::card(state, escalation))
                .map(|card| CardFacts { body: card.body })
                .collect(),
            ..Self::default()
        })
    }
}

/// The dependencies that have not landed yet.
///
/// A dependency the run does not know is not landed either, so it holds the
/// task back.
fn waiting_on(state: &RunState, task: &Task) -> Vec<String> {
    task.deps
        .iter()
        .filter(|dep| {
            state
                .tasks
                .get(*dep)
                .is_none_or(|found| found.status != TaskStatus::Integrated)
        })
        .cloned()
        .collect()
}

/// What a reported task waits on: its review, while one is out, else its gates.
///
/// A review verdict carries the provenance of the review that produced it
/// ([`shprd_core::state::GateResult::source`]), so the newest unanswered review
/// request is the difference between the two.
fn reported_wait(task: &Task) -> Option<String> {
    if task.status != TaskStatus::Reported {
        return None;
    }
    let answered = task
        .latest_attempt()
        .map(|attempt| {
            attempt
                .gates
                .iter()
                .filter(|gate| gate.source.is_some())
                .map(|gate| gate.seq)
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0);
    let asked = task
        .latest_attempt()
        .and_then(|attempt| attempt.reviews.last())
        .map_or(0, |request| request.seq);
    Some(if asked > answered {
        "In review".to_string()
    } else {
        "Running its gates".to_string()
    })
}

/// Why a failed task failed: its agent's own words, else the finding that
/// failed a gate.
fn failure_reason(task: &Task) -> Option<String> {
    if task.status != TaskStatus::Failed {
        return None;
    }
    let attempt = task.latest_attempt()?;
    if let Some(report) = &attempt.report
        && report.status != ReportStatus::Completed
    {
        return Some(report.summary.clone());
    }
    if let Some(gate) = attempt.gates.iter().rev().find(|gate| !gate.passed) {
        return Some(match gate.findings.first() {
            Some(finding) => finding.title.clone(),
            None => format!("{} failed", gate.gate),
        });
    }
    None
}

/// One task's status, need and reason, from the fold's facts and the live
/// overlay at `now_ms`.
///
/// The rules run in Shepherdr's order (`agents.py:106-170`): a problem it just
/// found waits out the grace, a problem nobody is on needs the person, a
/// provider cut keeps it working until the tries run out, an agent that asks
/// needs the person, then the ready/parked rules, and only then "is anything
/// actually happening": an agent that has not reported yet, a task just
/// started, its own background work, the grace after an agent goes quiet, a
/// quiet agent, and a task with no agent left.
pub fn derive_status(signal: &TaskSignal, live: &LiveAgents, now_ms: u64) -> Derived {
    // A task that starts by itself at a time says when (agents.py:108-110),
    // before any state of its own is read: it is nothing to do with an agent.
    if let Some(at) = signal.scheduled_for_ms {
        let reason = starts_words(at, now_ms);
        return Derived::new(DerivedStatus::Scheduled, None, Some(reason));
    }
    let running = match signal.status {
        TaskStatus::Working => true,
        TaskStatus::Reported | TaskStatus::Passed => false,
        _ => return parked(signal),
    };
    let mine = live.mine();
    // An ask the agent is waiting on needs the person at once, whatever else is
    // happening: the hook that raised it holds the agent until an answer comes
    // back. An ask whose hook timed out is no longer open and says nothing.
    if let Some(ask) = open_ask(signal, now_ms) {
        return Derived::needs(Need::Asks, &ask_reason(ask));
    }
    // The engine's own card — a review that spent its rounds, a review that
    // could not start — is the person's to settle, so it is read here too: a
    // row that kept saying Working while a card sat in `needs` is how a person
    // reading the board alone missed that nothing else would move.
    if let Some(card) = signal.cards.first() {
        return Derived::needs(Need::Attention, &card.body);
    }
    // A problem found a moment ago reads as the task would without it
    // (agents.py:118-120): its agent is usually told and takes it up, and the
    // row must not flicker at the person for a few seconds.
    if let Some(attention) = &signal.attention
        && now_ms.saturating_sub(attention.at_ms) < GRACE_MS
    {
        let without = TaskSignal {
            attention: None,
            ..signal.clone()
        };
        return derive_status(&without, live, now_ms);
    }
    // A problem nobody is on needs the person (agents.py:121-126).
    if let Some(attention) = &signal.attention
        && !on_it(signal, &mine, now_ms)
    {
        return Derived::needs(Need::Attention, &attention.reason);
    }
    // Cut off by its provider: still Working while shprd waits to tell it to
    // carry on, Needs you once the tries are used up (agents.py:127-131). Only
    // a running task can be cut off, and a question it asks meanwhile wins.
    if running && let Some(cut) = &signal.provider_cut {
        let live_here = mine.iter().any(|agent| agent.address == cut.agent);
        let blocked = mine
            .iter()
            .any(|agent| agent.activity == Some(Activity::Blocked));
        if live_here && (!blocked || cut.over) {
            if cut.over {
                return Derived::needs(Need::Provider, &cut.reason);
            }
            let line = retry_line(cut, now_ms);
            return Derived::working(Some(line.as_str()));
        }
    }
    // What really asks needs the person at once (agents.py:132-133).
    if mine
        .iter()
        .any(|agent| agent.activity == Some(Activity::Blocked))
    {
        return Derived::needs(Need::Asks, QUESTION);
    }
    // Claude Code's folder-trust dialog (agents.py:134-135): shown before the
    // agent reports anything, so it is read off a terminal that has neither
    // reported a status nor taken a turn.
    if trust_asked(&mine) {
        return Derived::needs(Need::Asks, TRUST);
    }
    // A task that said ready and whose agent is at work again is Working until
    // its next ready (agents.py:136-139).
    if busy_again(signal, &mine, now_ms) {
        return Derived::working(Some(WORKING_AGAIN));
    }
    // A problem its agent is on (agents.py:140-141): Working, with the agent's
    // own status line when it set one.
    if signal.attention.is_some() {
        return Derived::working(if running { None } else { Some(WORKING_AGAIN) });
    }
    if !running {
        // A ready task whose merge or checks need nobody is Working until it is
        // the person's turn (agents.py:142-149, ready_wait()).
        if let Some((status, reason)) = ready_wait(signal, &mine) {
            return Derived::new(status, None, Some(reason));
        }
        // Said ready but not at rest yet: still at work in the turn it said
        // ready in, or its own work still running (agents.py:244-256).
        if !mine.is_empty() && not_at_rest(&mine, &live.agents) {
            return Derived::working(None);
        }
        return Derived::new(DerivedStatus::Ready, None, None);
    }
    // An agent that has not reported a status yet is starting
    // (agents.py:150-152).
    if mine.iter().any(|agent| {
        !matches!(
            agent.activity,
            Some(Activity::Idle | Activity::Done | Activity::Blocked)
        )
    }) {
        return Derived::working(None);
    }
    // Just started: its terminal still runs the shell that starts the agent, or
    // the agent is idle before its first message lands (agents.py:153-156).
    if starting(signal, now_ms) {
        return Derived::working(None);
    }
    // Quiet, but on its own work (agents.py:157-161).
    if !mine.is_empty() && own_work(&live.agents, false) {
        return Derived::working(None);
    }
    // Just gone quiet: still Working for the grace, as its agent often ends a
    // turn a moment before it says ready (agents.py:162-165).
    if !mine.is_empty() && settling(&mine, now_ms) {
        return Derived::working(None);
    }
    // Quiet without saying ready: alive, it ended its turn on its question; its
    // process gone, the agent stopped (agents.py:166-170).
    if !mine.is_empty() {
        return Derived::needs(Need::Reply, REPLY);
    }
    Derived::needs(Need::Stopped, STOPPED)
}

/// A task's status as a reader that holds only the log sees it.
///
/// [`derive_status`] reads the agent terminals as well as the fold. A CLI and
/// an API have no terminals to read: passing an empty overlay would make every
/// task the fold still has [`TaskStatus::Working`] read "Needs you · Agent
/// stopped", which is a claim about a terminal nobody looked at. So a working
/// task is read with its agent assumed at work — its own recorded state — and
/// everything else, an open ask included, derives from the fold alone.
///
/// What this reader cannot say, it does not: a task whose agent went quiet
/// without saying ready, or one whose process exited, reads as Working here
/// until the fold says otherwise. The service's tick, which has the host's
/// word, reads those from [`derive_status`] itself.
pub fn derive_from_fold(signal: &TaskSignal, now_ms: u64) -> Derived {
    // An open ask is a fact of the log, and the harness hook that raised it is
    // holding an agent until an answer comes back, so it is read before the
    // stored state — a hook can raise one before the spawn is folded, which
    // would otherwise park the task and hide the card. A task that is already
    // finished says nothing it still needs.
    if !matches!(
        signal.status,
        TaskStatus::Failed | TaskStatus::Dropped | TaskStatus::Integrated
    ) {
        if let Some(ask) = open_ask(signal, now_ms) {
            return Derived::needs(Need::Asks, &ask_reason(ask));
        }
        // An escalation is the engine's own card, and it is the person's to
        // settle: a row that said Working while a card sat unread beside it is
        // how a person reading `shprd status` alone missed that they were
        // needed.
        if let Some(card) = signal.cards.first() {
            return Derived::needs(Need::Attention, &card.body);
        }
    }
    if matches!(signal.status, TaskStatus::Working) {
        let at_work = LiveAgents::new(vec![LiveAgent {
            activity: Some(Activity::Working),
            ..LiveAgent::default()
        }]);
        return derive_status(signal, &at_work, now_ms);
    }
    derive_status(signal, &LiveAgents::default(), now_ms)
}

/// The first ask still open at `now_ms`, if one is.
///
/// A hook that timed out leaves its ask behind; it is no longer open, so it
/// says nothing about what the task needs.
fn open_ask(signal: &TaskSignal, now_ms: u64) -> Option<&AskFacts> {
    signal
        .open_asks
        .iter()
        .find(|ask| ask.expires_at_ms.is_none_or(|until| until > now_ms))
}

/// The statuses that are no agent's business right now: a task nothing has
/// started on, a failed attempt, one that landed and one a person dropped
/// (Shepherdr `agents.py:111-114`, where anything but running or ready reads as
/// its stored state).
///
/// shprd's states map onto Shepherdr's like this: `Queued` is `todo`, or
/// `queued` while a dependency holds it back; `Failed` is a problem the person
/// must settle (there is no failed state in Shepherdr: a failed attempt reads
/// Needs you); `Integrated` is `done`; and `Dropped` is done too, since it is
/// terminal, with the reason kept in the row.
fn parked(signal: &TaskSignal) -> Derived {
    match signal.status {
        TaskStatus::Queued => {
            if signal.waiting_on.is_empty() {
                Derived::new(DerivedStatus::ToDo, None, None)
            } else {
                let reason = format!("{WAITING} {}", signal.waiting_on.join(", "));
                Derived::new(DerivedStatus::Queued, None, Some(fit(&reason)))
            }
        }
        TaskStatus::Failed => {
            Derived::needs(Need::Attention, signal.failure.as_deref().unwrap_or(FAILED))
        }
        TaskStatus::Dropped => {
            let reason = match &signal.dropped_reason {
                Some(why) => format!("{DROPPED} · {why}"),
                None => DROPPED.to_string(),
            };
            Derived::new(DerivedStatus::Done, None, Some(fit(&reason)))
        }
        TaskStatus::Integrated => Derived::new(DerivedStatus::Done, None, None),
        // Working, Reported and Passed are the live branch, which never sends
        // them here; a status added later reads as nothing started rather than
        // panicking a view.
        _ => Derived::new(DerivedStatus::ToDo, None, None),
    }
}

/// What a ready task waits on that needs nobody, and the words for it; `None`
/// when what is left is the person's (`agents.py:184-193`).
///
/// Shepherdr keeps the status Working and puts the word in the row
/// (`agents.py:181`; `core/tasks.js:readyWait()`), and keeps `Merging` in its
/// own vocabulary of statuses as well (`core/tasks.js:STATUS`, `RANK_OF`,
/// `ORDER`, and its spinner glyph). shprd's panels filter rows by status, so a
/// merge under way is the Merging status here, and the other waits the same
/// Working with their words.
fn ready_wait(signal: &TaskSignal, mine: &[&LiveAgent]) -> Option<(DerivedStatus, String)> {
    let merge = signal.merge;
    if merge.requested && !mine.is_empty() {
        return Some((DerivedStatus::Merging, MERGING.to_string()));
    }
    if merge.queued && merge.has_pr {
        return Some((DerivedStatus::Working, IN_QUEUE_LINE.to_string()));
    }
    if merge.auto && merge.has_pr && !merge.merged {
        let reason = if merge.needs_approval {
            APPROVAL_LINE
        } else {
            MERGES_ITSELF
        };
        return Some((DerivedStatus::Working, reason.to_string()));
    }
    signal
        .wait
        .as_ref()
        .map(|wait| (DerivedStatus::Working, fit(wait)))
}

/// Someone is on what needs the person (`agents.py:269-281`, `core/tasks.js:onIt()`).
///
/// On it: one of its agents is at work or asking (then its question shows), it
/// was just handed the problem and is starting, or its agent said what it is
/// doing since the problem came up and that is still fresh. An agent that set
/// its line and waits on CI in the background is idle to the multiplexer, which
/// is what the status line is for.
///
/// Shepherdr keeps one status line per task; shprd keeps one per agent, so the
/// line read is the agent's own, and one shprd's automation set counts for
/// nothing (`progress.by`).
fn on_it(signal: &TaskSignal, mine: &[&LiveAgent], now_ms: u64) -> bool {
    if mine.iter().any(|agent| {
        agent.kind != AgentKind::Reviewer
            && matches!(agent.activity, Some(Activity::Working | Activity::Blocked))
    }) {
        return true;
    }
    if signal
        .handed_at_ms
        .is_some_and(|at| now_ms.saturating_sub(at) < STARTING_MS)
    {
        return true;
    }
    let attention_at = signal.attention.as_ref().map_or(0, |found| found.at_ms);
    mine.iter().any(|agent| {
        let Some(line) = &agent.progress else {
            return false;
        };
        if line.by_herdr || line.set_ms == 0 || line.set_ms < attention_at {
            return false;
        }
        now_ms.saturating_sub(line.set_ms) < ON_IT_MS.max(line.left_ms.unwrap_or(0))
    })
}

/// A ready task whose agent is at work again since it said ready
/// (`agents.py:229-241`, `core/tasks.js:busyAgain()`).
///
/// Not while it merges as asked, and not a reviewer's turn. Herdr counts an
/// agent's finished turns, so the task keeps, per agent, the first turn that is
/// new work; a report that predates the count falls back to "any work once
/// [`STARTING_MS`] have passed".
fn busy_again(signal: &TaskSignal, mine: &[&LiveAgent], now_ms: u64) -> bool {
    if !matches!(signal.status, TaskStatus::Reported | TaskStatus::Passed) {
        return false;
    }
    if signal.merge.requested {
        return false;
    }
    if signal.ready_turns.is_none()
        && now_ms.saturating_sub(signal.ready_at_ms.unwrap_or(0)) < STARTING_MS
    {
        return false;
    }
    mine.iter().any(|agent| {
        agent.kind != AgentKind::Reviewer
            && agent.activity == Some(Activity::Working)
            && agent.turn.unwrap_or(0)
                >= signal
                    .ready_turns
                    .as_ref()
                    .and_then(|turns| turns.get(&agent.address))
                    .copied()
                    .unwrap_or(0)
    })
}

/// A ready task's agent still at it (`agents.py:251-256`,
/// `core/tasks.js:place()`): at work in the turn it said ready in, or its own
/// work still running (its hard signs only: a question asked does not end it).
fn not_at_rest(mine: &[&LiveAgent], agents: &[LiveAgent]) -> bool {
    if mine
        .iter()
        .any(|agent| agent.kind != AgentKind::Reviewer && agent.activity == Some(Activity::Working))
    {
        return true;
    }
    own_work(agents, true)
}

/// Its quiet agent is on its own work (`agents.py:883-911`): a background job,
/// a helper of its own at work, or a command open in the foreground. What
/// really runs counts whatever its last message says. Its status line alone
/// counts only when its last message asks nothing, and never for a ready task.
fn own_work(agents: &[LiveAgent], ready: bool) -> bool {
    agents.iter().any(|agent| {
        if agent.kind == AgentKind::Reviewer || agent.activity == Some(Activity::Blocked) {
            return false;
        }
        agent.own_work
            || (!ready && agent.waits_on_own)
            || (agent.kind == AgentKind::Helper && agent.activity == Some(Activity::Working))
    })
}

/// The terminal showing Claude Code's folder-trust dialog, if one is
/// (`agents.py:311-321`).
fn trust_asked(mine: &[&LiveAgent]) -> bool {
    mine.iter()
        .any(|agent| agent.trust_dialog && agent.activity.is_none() && agent.turn.unwrap_or(0) == 0)
}

/// An agent was started for it under [`STARTING_MS`] ago and has not ended
/// (`agents.py:589-595`).
///
/// The fold settles "has not ended": an attempt that reports moves the task to
/// Reported, and this rule only reads a task the fold still has Working.
fn starting(signal: &TaskSignal, now_ms: u64) -> bool {
    signal
        .started_ms
        .is_some_and(|at| now_ms.saturating_sub(at) < STARTING_MS)
}

/// When its agents last changed status, ms, or 0 when none says
/// (`agents.py:206-208`).
fn quiet_since(mine: &[&LiveAgent]) -> u64 {
    mine.iter()
        .filter_map(|agent| agent.since_ms)
        .max()
        .unwrap_or(0)
}

/// Its agents went quiet under [`GRACE_MS`] ago (`agents.py:211-214`).
fn settling(mine: &[&LiveAgent], now_ms: u64) -> bool {
    let since = quiet_since(mine);
    since != 0 && now_ms.saturating_sub(since) < GRACE_MS
}

/// A provider retry's status line: "Provider overloaded · retry in 2m"
/// (`agents.py:289-293`).
fn retry_line(cut: &ProviderCut, now_ms: u64) -> String {
    match cut.due_ms {
        Some(due) => {
            let minutes = due.saturating_sub(now_ms).div_ceil(60_000).max(1);
            format!("{} · retry in {minutes}m", cut.reason)
        }
        None => format!("{} · retrying", cut.reason),
    }
}

/// The few words a row shows for an open ask.
fn ask_reason(ask: &AskFacts) -> String {
    match ask.kind {
        AskKind::Plan => "Plan to approve".to_string(),
        AskKind::Question | AskKind::Permission => {
            let text = ask.text.trim();
            if text.is_empty() {
                QUESTION.to_string()
            } else {
                fit(text)
            }
        }
    }
}

/// A scheduled task's reason: "Starts 09:00 UTC", or "Starts now" once due.
///
/// Shepherdr says it in the reader's local days ("Starts Fri 9:00 (in 6h)",
/// `when.py:164-168`); shprd's clock is UTC and has no calendar yet, so the
/// hour stands in for the day words.
fn starts_words(at_ms: u64, now_ms: u64) -> String {
    if at_ms <= now_ms {
        "Starts now".to_string()
    } else {
        format!("Starts {} UTC", clock::hhmm(at_ms))
    }
}

/// One line of at most [`LINE_MAX`] characters, cut at a word when longer
/// (`agents.py:326-334`): never an agent's message pasted whole, never a
/// model's words.
pub fn fit(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = line.chars().collect();
    if chars.len() <= LINE_MAX {
        return line;
    }
    let mut cut: String = chars[..LINE_MAX - 1].iter().collect();
    if chars.get(LINE_MAX - 1) != Some(&' ')
        && let Some(space) = cut.rfind(' ')
    {
        cut.truncate(space);
    }
    let cut = cut.trim_end_matches([' ', ',', ';', ':', '.', '-']);
    format!("{cut}…")
}

#[cfg(test)]
mod tests {
    use shprd_core::event::{Actor, Event, EventKind};

    use super::*;
    use crate::fixture;

    /// The clock every case counts from: 2023-11-14 22:13:20 UTC.
    const NOW: u64 = fixture::NOW;

    /// The worker's address, the one most cases use.
    const W: &str = "w1";

    /// A worker that has reported the given status, a moment ago.
    fn worker(activity: Option<Activity>) -> LiveAgent {
        LiveAgent {
            address: W.to_string(),
            activity,
            since_ms: Some(NOW),
            turn: Some(1),
            ..LiveAgent::default()
        }
    }

    /// The overlay of one worker.
    fn one(activity: Option<Activity>) -> LiveAgents {
        LiveAgents::new(vec![worker(activity)])
    }

    /// A task the fold puts in the given state and nothing else.
    fn task(status: TaskStatus) -> TaskSignal {
        TaskSignal {
            status,
            ..TaskSignal::default()
        }
    }

    /// A task in the given state with a worker that has been idle since well
    /// before now, so no grace holds anything back.
    fn settled(status: TaskStatus) -> (TaskSignal, LiveAgents) {
        let mut agent = worker(Some(Activity::Idle));
        agent.since_ms = Some(NOW - GRACE_MS - 10_000);
        (task(status), LiveAgents::new(vec![agent]))
    }

    /// A problem shprd found a minute ago.
    fn problem(reason: &str) -> Attention {
        Attention {
            reason: reason.to_string(),
            at_ms: NOW - 60_000,
        }
    }

    /// One case of a table-driven test.
    #[derive(Debug)]
    struct Case {
        /// The rule under test, and where it comes from.
        rule: &'static str,
        /// The task's facts.
        signal: TaskSignal,
        /// The terminals on it.
        live: LiveAgents,
        /// The word the row must show.
        status: DerivedStatus,
        /// The need it must show.
        needs: Option<Need>,
        /// The reason it must show.
        reason: Option<&'static str>,
    }

    /// Runs every case and reports all failures at once.
    fn assert_cases(cases: Vec<Case>) {
        let mut failures: Vec<String> = Vec::new();
        for case in cases {
            let got = derive_status(&case.signal, &case.live, NOW);
            let want = Derived {
                status: case.status,
                needs: case.needs,
                reason: case.reason.map(ToOwned::to_owned),
            };
            if got != want {
                failures.push(format!("{}: got {got:?}, want {want:?}", case.rule));
            }
        }
        assert!(
            failures.is_empty(),
            "{} case(s) read wrongly:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }

    #[test]
    fn a_scheduled_task_says_when_it_starts() {
        let mut signal = task(TaskStatus::Queued);
        signal.scheduled_for_ms = Some(NOW + 2 * 3_600_000);
        let got = derive_status(&signal, &LiveAgents::default(), NOW);
        assert_eq!(got.status, DerivedStatus::Scheduled);
        assert_eq!(got.reason.as_deref(), Some("Starts 00:13 UTC"));
        signal.scheduled_for_ms = Some(NOW);
        assert_eq!(
            derive_status(&signal, &LiveAgents::default(), NOW).reason,
            Some("Starts now".to_string())
        );
    }

    #[test]
    fn parked_tasks_read_as_their_state_says() {
        let mut waiting = task(TaskStatus::Queued);
        waiting.waiting_on = vec!["M8.1".to_string(), "M8.2".to_string()];
        let mut failed = task(TaskStatus::Failed);
        failed.failure = Some("Scope conflict on crates/shprd-views".to_string());
        let mut dropped = task(TaskStatus::Dropped);
        dropped.dropped_reason = Some("not needed".to_string());
        assert_cases(vec![
            Case {
                rule: "queued with nothing in its way is to do (agents.py:111-114)",
                signal: task(TaskStatus::Queued),
                live: LiveAgents::default(),
                status: DerivedStatus::ToDo,
                needs: None,
                reason: None,
            },
            Case {
                rule: "queued behind a dependency says what it waits for (queues.py:163-169)",
                signal: waiting,
                live: LiveAgents::default(),
                status: DerivedStatus::Queued,
                needs: None,
                reason: Some("after M8.1, M8.2"),
            },
            Case {
                rule: "a failed attempt is the person's (agents.py:166-170, mapped)",
                signal: failed,
                live: LiveAgents::default(),
                status: DerivedStatus::NeedsYou,
                needs: Some(Need::Attention),
                reason: Some("Scope conflict on crates/shprd-views"),
            },
            Case {
                rule: "a failure with no words still needs the person",
                signal: task(TaskStatus::Failed),
                live: LiveAgents::default(),
                status: DerivedStatus::NeedsYou,
                needs: Some(Need::Attention),
                reason: Some(FAILED),
            },
            Case {
                rule: "a landed task is done (agents.py:111-114)",
                signal: task(TaskStatus::Integrated),
                live: LiveAgents::default(),
                status: DerivedStatus::Done,
                needs: None,
                reason: None,
            },
            Case {
                rule: "a dropped task is done, and says why",
                signal: dropped,
                live: LiveAgents::default(),
                status: DerivedStatus::Done,
                needs: None,
                reason: Some("Dropped · not needed"),
            },
        ]);
    }

    #[test]
    fn a_problem_it_just_found_waits_out_the_grace() {
        let (mut signal, _) = settled(TaskStatus::Working);
        let mut quiet = worker(Some(Activity::Idle));
        quiet.since_ms = Some(NOW - 1_000);
        let live = LiveAgents::new(vec![quiet]);
        // Inside the grace the row reads as it would without the problem: its
        // agent is quiet, and only just, so it is still Working.
        signal.attention = Some(Attention {
            reason: "Checks failed on the integration branch".to_string(),
            at_ms: NOW - GRACE_MS + 1_000,
        });
        assert_eq!(
            derive_status(&signal, &live, NOW),
            Derived::working(None),
            "a fresh problem is not shown yet (agents.py:118-120)"
        );
        // Once it has lasted the grace it is the person's.
        signal.attention = Some(Attention {
            reason: "Checks failed on the integration branch".to_string(),
            at_ms: NOW - GRACE_MS,
        });
        assert_eq!(
            derive_status(&signal, &live, NOW),
            Derived::needs(Need::Attention, "Checks failed on the integration branch"),
            "once it has lasted the grace it needs the person (agents.py:121-126)"
        );
    }

    #[test]
    fn a_problem_someone_is_on_keeps_it_working() {
        let (mut signal, _) = settled(TaskStatus::Working);
        signal.attention = Some(problem("Checks failed"));
        // Its agent is at work: nobody has to be told (agents.py:269-273).
        assert_eq!(
            derive_status(&signal, &one(Some(Activity::Working)), NOW),
            Derived::working(None)
        );
        // It was just handed the problem and is starting (agents.py:274-275).
        let mut handed = signal.clone();
        handed.handed_at_ms = Some(NOW - 1_000);
        assert_eq!(
            derive_status(&handed, &one(Some(Activity::Idle)), NOW),
            Derived::working(None)
        );
        // It said what it is doing since the problem came up
        // (agents.py:276-281).
        let mut agent = worker(Some(Activity::Idle));
        agent.progress = Some(Progress {
            text: "Waiting on review round 1".to_string(),
            set_ms: NOW - 60_000,
            left_ms: None,
            by_herdr: false,
        });
        assert_eq!(
            derive_status(&signal, &LiveAgents::new(vec![agent.clone()]), NOW),
            Derived::working(None)
        );
        // shprd's own words are never the agent's (agents.py:278).
        let mut hush = agent.clone();
        if let Some(line) = hush.progress.as_mut() {
            line.by_herdr = true;
        }
        assert_eq!(
            derive_status(&signal, &LiveAgents::new(vec![hush]), NOW),
            Derived::needs(Need::Attention, "Checks failed")
        );
        // A line set before the problem came up is stale (agents.py:278).
        let mut newer = signal.clone();
        newer.attention = Some(Attention {
            reason: "Checks failed".to_string(),
            at_ms: NOW - 30_000,
        });
        let mut stale = agent;
        if let Some(line) = stale.progress.as_mut() {
            line.set_ms = NOW - 60_000;
        }
        assert_eq!(
            derive_status(&newer, &LiveAgents::new(vec![stale]), NOW),
            Derived::needs(Need::Attention, "Checks failed")
        );
    }

    #[test]
    fn a_provider_cut_keeps_it_working_until_the_tries_run_out() {
        let (mut signal, _) = settled(TaskStatus::Working);
        signal.provider_cut = Some(ProviderCut {
            agent: W.to_string(),
            reason: "Provider overloaded".to_string(),
            over: false,
            due_ms: Some(NOW + 2 * 60_000),
        });
        let idle = one(Some(Activity::Idle));
        assert_eq!(
            derive_status(&signal, &idle, NOW),
            Derived::working(Some("Provider overloaded · retry in 2m")),
            "still working while shprd waits to carry on (agents.py:127-131)"
        );
        let mut over = signal.clone();
        if let Some(cut) = over.provider_cut.as_mut() {
            cut.over = true;
        }
        assert_eq!(
            derive_status(&over, &idle, NOW),
            Derived::needs(Need::Provider, "Provider overloaded")
        );
        // No due time says so (agents.py:289-293).
        let mut no_due = signal.clone();
        if let Some(cut) = no_due.provider_cut.as_mut() {
            cut.due_ms = None;
        }
        assert_eq!(
            derive_status(&no_due, &idle, NOW).reason.as_deref(),
            Some("Provider overloaded · retrying")
        );
        // The cut agent must still be there (agents.py:128).
        let mut other = worker(Some(Activity::Idle));
        other.address = "w2".to_string();
        other.since_ms = Some(NOW - GRACE_MS - 10_000);
        assert_eq!(
            derive_status(&signal, &LiveAgents::new(vec![other]), NOW),
            Derived::needs(Need::Reply, REPLY),
            "a cut agent that is gone is not a retry"
        );
    }

    #[test]
    fn what_asks_needs_the_person_at_once() {
        let (signal, _) = settled(TaskStatus::Working);
        let mut reviewer = worker(Some(Activity::Blocked));
        reviewer.kind = AgentKind::Reviewer;
        // A question, and a reviewer asking one too (agents.py:132-133).
        assert_cases(vec![
            Case {
                rule: "an agent asking a question (agents.py:132-133)",
                signal: signal.clone(),
                live: one(Some(Activity::Blocked)),
                status: DerivedStatus::NeedsYou,
                needs: Some(Need::Asks),
                reason: Some(QUESTION),
            },
            Case {
                rule: "a reviewer asking a question counts too (agents.py:132-133)",
                signal: signal.clone(),
                live: LiveAgents::new(vec![reviewer]),
                status: DerivedStatus::NeedsYou,
                needs: Some(Need::Asks),
                reason: Some(QUESTION),
            },
        ]);
        // Claude Code's folder-trust dialog (agents.py:134-135).
        let mut trusting = worker(None);
        trusting.turn = None;
        trusting.trust_dialog = true;
        let got = derive_status(&signal, &LiveAgents::new(vec![trusting.clone()]), NOW);
        assert_eq!(got.needs, Some(Need::Asks));
        assert_eq!(got.reason.as_deref(), Some(TRUST));
        // An agent that has taken a turn is past it (agents.py:314).
        trusting.turn = Some(2);
        assert_ne!(
            derive_status(&signal, &LiveAgents::new(vec![trusting]), NOW)
                .reason
                .as_deref(),
            Some(TRUST)
        );
    }

    #[test]
    fn a_ready_task_back_at_work_is_working_again() {
        let (mut signal, _) = settled(TaskStatus::Reported);
        signal.ready_at_ms = Some(NOW - 10 * 60_000);
        signal.ready_turns = Some(BTreeMap::from([(W.to_string(), 3)]));
        // A turn at or after the one it said ready in is new work
        // (agents.py:229-241).
        let mut agent = worker(Some(Activity::Working));
        agent.turn = Some(3);
        assert_eq!(
            derive_status(&signal, &LiveAgents::new(vec![agent.clone()]), NOW),
            Derived::working(Some(WORKING_AGAIN))
        );
        agent.turn = Some(4);
        assert_eq!(
            derive_status(&signal, &LiveAgents::new(vec![agent]), NOW)
                .reason
                .as_deref(),
            Some(WORKING_AGAIN)
        );
        // The turn it said ready in is the one it is still resting from: that is
        // "not at rest", not "working again" (agents.py:244-256).
        let mut resting = worker(Some(Activity::Working));
        resting.turn = Some(2);
        assert_eq!(
            derive_status(&signal, &LiveAgents::new(vec![resting]), NOW),
            Derived::working(None)
        );
        // A reviewer's turn never counts (agents.py:240).
        let mut reviewer = worker(Some(Activity::Working));
        reviewer.kind = AgentKind::Reviewer;
        reviewer.turn = Some(9);
        assert_eq!(
            derive_status(&signal, &LiveAgents::new(vec![reviewer]), NOW),
            Derived::new(DerivedStatus::Ready, None, None)
        );
        // A merge it was asked for is its work, not new work (agents.py:235).
        let mut merging = signal.clone();
        merging.merge.requested = true;
        let mut at_work = worker(Some(Activity::Working));
        at_work.turn = Some(5);
        assert_eq!(
            derive_status(&merging, &LiveAgents::new(vec![at_work]), NOW).status,
            DerivedStatus::Merging
        );
    }

    #[test]
    fn a_report_before_turns_were_kept_waits_the_starting_grace() {
        let (mut signal, _) = settled(TaskStatus::Reported);
        signal.ready_at_ms = Some(NOW - 1_000);
        signal.ready_turns = None;
        let mut agent = worker(Some(Activity::Working));
        agent.turn = Some(7);
        // Inside STARTING_MS any work is the same turn (agents.py:238-239).
        assert_eq!(
            derive_status(&signal, &LiveAgents::new(vec![agent.clone()]), NOW).reason,
            None
        );
        signal.ready_at_ms = Some(NOW - STARTING_MS);
        assert_eq!(
            derive_status(&signal, &LiveAgents::new(vec![agent]), NOW)
                .reason
                .as_deref(),
            Some(WORKING_AGAIN)
        );
    }

    #[test]
    fn a_ready_task_that_still_waits_keeps_working() {
        let (signal, _) = settled(TaskStatus::Reported);
        let idle = one(Some(Activity::Idle));
        let with_pr = MergeFacts {
            has_pr: true,
            ..MergeFacts::default()
        };
        let asked = MergeFacts {
            requested: true,
            ..with_pr
        };
        let read = |merge: MergeFacts, wait: &str| {
            let mut signal = signal.clone();
            signal.merge = merge;
            signal.wait = if wait.is_empty() {
                None
            } else {
                Some(wait.to_string())
            };
            derive_status(&signal, &idle, NOW)
        };
        // The merge it was asked for (agents.py:186-187).
        assert_eq!(read(asked, "").status, DerivedStatus::Merging);
        assert_eq!(read(asked, "").reason.as_deref(), Some(MERGING));
        // In the queue, waiting on a review, or merging by itself
        // (agents.py:188-192).
        let queued = MergeFacts {
            queued: true,
            ..with_pr
        };
        assert_eq!(read(queued, "").reason.as_deref(), Some(IN_QUEUE_LINE));
        let auto = MergeFacts {
            auto: true,
            ..with_pr
        };
        assert_eq!(read(auto, "").reason.as_deref(), Some(MERGES_ITSELF));
        let approval = MergeFacts {
            auto: true,
            needs_approval: true,
            ..with_pr
        };
        assert_eq!(read(approval, "").reason.as_deref(), Some(APPROVAL_LINE));
        // A merged pull request is nothing to wait on (agents.py:190).
        let merged = MergeFacts {
            auto: true,
            merged: true,
            ..with_pr
        };
        assert_eq!(read(merged, "").status, DerivedStatus::Ready);
        // shprd's own words for what it waits on (agents.py:193).
        assert_eq!(
            read(MergeFacts::default(), "In review").reason.as_deref(),
            Some("In review")
        );
        // And a wait loses to the merge it was asked for (agents.py:186-193).
        assert_eq!(read(asked, "In review").status, DerivedStatus::Merging);
        // A task at rest is Ready, with nothing to say (agents.py:142-149).
        assert_eq!(
            read(MergeFacts::default(), ""),
            Derived::new(DerivedStatus::Ready, None, None)
        );
    }

    #[test]
    fn a_ready_task_is_not_ready_while_it_still_moves() {
        let (mut signal, _) = settled(TaskStatus::Passed);
        // A turn below the one it said ready in: still at rest in that turn
        // (agents.py:251-256).
        signal.ready_at_ms = Some(NOW - 600_000);
        signal.ready_turns = Some(BTreeMap::from([(W.to_string(), 2)]));
        let mut at_work = worker(Some(Activity::Working));
        at_work.turn = Some(1);
        assert_eq!(
            derive_status(&signal, &LiveAgents::new(vec![at_work]), NOW),
            Derived::working(None)
        );
        // Its own background job still running: no Ready yet (agents.py:244-256).
        let mut owning = worker(Some(Activity::Idle));
        owning.own_work = true;
        assert_eq!(
            derive_status(&signal, &LiveAgents::new(vec![owning]), NOW),
            Derived::working(None)
        );
        // Its status line alone is no hard sign for a ready task
        // (agents.py:887-888): the person is not told about work that has
        // stopped moving.
        let mut saying = worker(Some(Activity::Idle));
        saying.waits_on_own = true;
        assert_eq!(
            derive_status(&signal, &LiveAgents::new(vec![saying]), NOW),
            Derived::new(DerivedStatus::Ready, None, None)
        );
    }

    #[test]
    fn a_quiet_agent_is_working_until_the_grace_runs_out() {
        let (signal, _) = settled(TaskStatus::Working);
        assert_cases(vec![
            Case {
                rule: "an agent that has not reported yet (agents.py:150-152)",
                signal: signal.clone(),
                live: one(None),
                status: DerivedStatus::Working,
                needs: None,
                reason: None,
            },
            Case {
                rule: "an agent just gone quiet (agents.py:162-165)",
                signal: signal.clone(),
                live: one(Some(Activity::Idle)),
                status: DerivedStatus::Working,
                needs: None,
                reason: None,
            },
            Case {
                rule: "a quiet agent long after the grace (agents.py:166-169)",
                signal: signal.clone(),
                live: settled(TaskStatus::Working).1,
                status: DerivedStatus::NeedsYou,
                needs: Some(Need::Reply),
                reason: Some(REPLY),
            },
            Case {
                rule: "no agent left (agents.py:170)",
                signal: signal.clone(),
                live: LiveAgents::default(),
                status: DerivedStatus::NeedsYou,
                needs: Some(Need::Stopped),
                reason: Some(STOPPED),
            },
        ]);
    }

    #[test]
    fn a_task_just_started_is_working_even_with_no_agent_yet() {
        let (mut signal, _) = settled(TaskStatus::Working);
        signal.started_ms = Some(NOW - STARTING_MS + 1_000);
        assert_eq!(
            derive_status(&signal, &LiveAgents::default(), NOW),
            Derived::working(None),
            "an agent was started under STARTING_MS ago (agents.py:153-156)"
        );
        signal.started_ms = Some(NOW - STARTING_MS);
        assert_eq!(
            derive_status(&signal, &LiveAgents::default(), NOW).needs,
            Some(Need::Stopped)
        );
    }

    #[test]
    fn a_quiet_agent_on_its_own_work_is_working() {
        let (signal, _) = settled(TaskStatus::Working);
        let mut owning = worker(Some(Activity::Idle));
        owning.own_work = true;
        assert_eq!(
            derive_status(&signal, &LiveAgents::new(vec![owning]), NOW),
            Derived::working(None),
            "its own background job is still running (agents.py:157-161)"
        );
        let mut saying = worker(Some(Activity::Idle));
        saying.waits_on_own = true;
        assert_eq!(
            derive_status(&signal, &LiveAgents::new(vec![saying]), NOW),
            Derived::working(None),
            "its status line says it waits on CI (agents.py:839-854)"
        );
        // A helper of its own at work keeps its worker Working
        // (agents.py:894, SHEP-54).
        let mut helper = worker(Some(Activity::Working));
        helper.kind = AgentKind::Helper;
        helper.address = "h1".to_string();
        assert_eq!(
            derive_status(
                &signal,
                &LiveAgents::new(vec![worker(Some(Activity::Idle)), helper]),
                NOW
            ),
            Derived::working(None)
        );
        // An agent asking the person is not on its own work (agents.py:890).
        let mut blocked = worker(Some(Activity::Blocked));
        blocked.own_work = true;
        assert_eq!(
            derive_status(&signal, &LiveAgents::new(vec![blocked]), NOW).needs,
            Some(Need::Asks)
        );
    }

    #[test]
    fn the_earlier_rule_wins_when_two_apply() {
        let cut = ProviderCut {
            agent: W.to_string(),
            reason: "Provider overloaded".to_string(),
            over: true,
            due_ms: None,
        };
        let mut problem_task = settled(TaskStatus::Working).0;
        problem_task.attention = Some(problem("Checks failed"));
        let mut cut_task = settled(TaskStatus::Working).0;
        cut_task.provider_cut = Some(cut);
        let mut at_work = worker(Some(Activity::Working));
        at_work.turn = Some(2);
        let mut back = settled(TaskStatus::Reported).0;
        back.ready_at_ms = Some(NOW - 600_000);
        back.ready_turns = Some(BTreeMap::from([(W.to_string(), 2)]));
        let mut back_and_problem = back.clone();
        back_and_problem.attention = Some(problem("Checks failed"));
        let mut trusting_and_asking = worker(Some(Activity::Blocked));
        trusting_and_asking.trust_dialog = true;
        assert_cases(vec![
            Case {
                rule: "a problem beats a quiet agent (agents.py:121-133)",
                signal: problem_task.clone(),
                live: settled(TaskStatus::Working).1,
                status: DerivedStatus::NeedsYou,
                needs: Some(Need::Attention),
                reason: Some("Checks failed"),
            },
            Case {
                rule: "a used-up cut beats the question its agent asks (agents.py:127-133)",
                signal: cut_task,
                live: one(Some(Activity::Blocked)),
                status: DerivedStatus::NeedsYou,
                needs: Some(Need::Provider),
                reason: Some("Provider overloaded"),
            },
            Case {
                rule: "a question beats a task back at work (agents.py:132-139)",
                signal: back,
                live: one(Some(Activity::Blocked)),
                status: DerivedStatus::NeedsYou,
                needs: Some(Need::Asks),
                reason: Some(QUESTION),
            },
            Case {
                rule: "a question beats a folder to trust (agents.py:132-135)",
                signal: problem_task,
                live: LiveAgents::new(vec![trusting_and_asking]),
                status: DerivedStatus::NeedsYou,
                needs: Some(Need::Asks),
                reason: Some(QUESTION),
            },
            Case {
                rule: "back at work beats the problem it was handed (agents.py:136-141)",
                signal: back_and_problem,
                live: LiveAgents::new(vec![at_work]),
                status: DerivedStatus::Working,
                needs: None,
                reason: Some(WORKING_AGAIN),
            },
        ]);
    }

    #[test]
    fn the_fixture_reads_as_its_people_would() {
        let state = fixture::state();
        let read = |id: &str| match TaskSignal::from_state(&state, id) {
            Some(signal) => derive_status(&signal, &LiveAgents::default(), fixture::NOW),
            None => panic!("the fixture has {id}"),
        };
        // The folded fixture: landed, failed at a gate, and waiting its turn.
        assert_eq!(read("M1.1").status, DerivedStatus::Done);
        assert_eq!(read("M1.2").status, DerivedStatus::NeedsYou);
        assert_eq!(read("M1.2").needs, Some(Need::Attention));
        assert_eq!(read("M1.3"), Derived::new(DerivedStatus::ToDo, None, None));
        assert_eq!(TaskSignal::from_state(&state, "M9.9"), None);
    }

    #[test]
    fn the_fold_gives_the_facts_the_rules_read() {
        // A reported task whose gates are still to run reads Working.
        let events = fixture::events();
        let reported = match RunState::fold(&events[..10]) {
            Ok(state) => state,
            Err(err) => panic!("the fixture prefix must fold: {err}"),
        };
        let signal = match TaskSignal::from_state(&reported, "M1.2") {
            Some(signal) => signal,
            None => panic!("the fold has M1.2"),
        };
        assert_eq!(signal.status, TaskStatus::Reported);
        assert_eq!(signal.wait.as_deref(), Some("Running its gates"));
        assert_eq!(signal.ready_turns, None);
        assert_eq!(
            derive_status(&signal, &LiveAgents::default(), fixture::NOW).status,
            DerivedStatus::Working
        );
        // Once a review is out, that is what it waits on.
        let mut asked = reported.clone();
        let review = Event {
            seq: 11,
            at_ms: fixture::NOW + 11_000,
            actor: Actor::Engine,
            kind: EventKind::ReviewRequested {
                task: "M1.2".to_string(),
                attempt: 1,
                reviewer: "codex".to_string(),
                degraded: false,
            },
        };
        match asked.apply(&review) {
            Ok(()) => {}
            Err(err) => panic!("a review request must apply: {err}"),
        }
        match TaskSignal::from_state(&asked, "M1.2") {
            Some(signal) => assert_eq!(signal.wait.as_deref(), Some("In review")),
            None => panic!("the fold has M1.2"),
        }
        // A queued task with nothing in its way is to do.
        match TaskSignal::from_state(&reported, "M1.3") {
            Some(signal) => assert!(signal.waiting_on.is_empty()),
            None => panic!("the fold has M1.3"),
        }
        // A failed task says why, from the finding that failed its gate.
        match TaskSignal::from_state(&fixture::state(), "M1.2") {
            Some(signal) => {
                assert_eq!(signal.status, TaskStatus::Failed);
                assert_eq!(signal.failure.as_deref(), Some("unwrap used"));
            }
            None => panic!("the fixture has M1.2"),
        }
    }

    #[test]
    fn a_reason_is_one_short_line() {
        assert_eq!(fit("  two   spaces "), "two spaces");
        assert_eq!(fit(&"a".repeat(LINE_MAX)), "a".repeat(LINE_MAX));
        let fitted = fit("the agent said something that runs on and on past the line");
        assert!(fitted.chars().count() <= LINE_MAX, "{fitted}");
        assert!(fitted.ends_with('…'), "{fitted}");
        // The last character kept is a space: the cut is already at a word.
        assert_eq!(
            fit(&format!("{} bbb", "a".repeat(LINE_MAX - 1))),
            format!("{}…", "a".repeat(LINE_MAX - 1))
        );
        // A word that would be cut is dropped whole (agents.py:326-334).
        assert_eq!(
            fit(&format!("{} {}", "a".repeat(20), "b".repeat(25))),
            format!("{}…", "a".repeat(20))
        );
    }

    #[test]
    fn every_status_has_its_own_word_and_glyph() {
        let all = [
            DerivedStatus::ToDo,
            DerivedStatus::Scheduled,
            DerivedStatus::Queued,
            DerivedStatus::Working,
            DerivedStatus::NeedsYou,
            DerivedStatus::Ready,
            DerivedStatus::Merging,
            DerivedStatus::Done,
        ];
        let mut words: Vec<&str> = all.iter().map(|status| status.word()).collect();
        let mut glyphs: Vec<char> = all.iter().map(|status| status.glyph()).collect();
        words.sort_unstable();
        words.dedup();
        glyphs.sort_unstable();
        glyphs.dedup();
        assert_eq!(words.len(), all.len());
        assert_eq!(glyphs.len(), all.len());
        assert_eq!(DerivedStatus::NeedsYou.word(), "Needs you");
    }

    #[test]
    fn an_open_ask_makes_the_task_need_you() {
        let mut signal = task(TaskStatus::Working);
        signal.open_asks.push(AskFacts {
            kind: AskKind::Permission,
            text: "Run: cargo test".to_string(),
            expires_at_ms: None,
        });
        let got = derive_status(&signal, &one(Some(Activity::Working)), NOW);
        assert_eq!(got.status, DerivedStatus::NeedsYou);
        assert_eq!(got.needs, Some(Need::Asks));
        assert_eq!(got.reason.as_deref(), Some("Run: cargo test"));
    }

    #[test]
    fn a_question_and_a_plan_say_what_they_need() {
        let mut question = task(TaskStatus::Working);
        question.open_asks.push(AskFacts {
            kind: AskKind::Question,
            text: "Which database should this use?".to_string(),
            expires_at_ms: None,
        });
        let got = derive_status(&question, &one(Some(Activity::Working)), NOW);
        assert_eq!(got.status, DerivedStatus::NeedsYou);
        assert!(got.reason.unwrap_or_default().starts_with("Which database"));

        let mut plan = task(TaskStatus::Working);
        plan.open_asks.push(AskFacts {
            kind: AskKind::Plan,
            text: "1. Add a queue".to_string(),
            expires_at_ms: None,
        });
        let got = derive_status(&plan, &one(Some(Activity::Working)), NOW);
        assert_eq!(got.status, DerivedStatus::NeedsYou);
        assert_eq!(got.reason.as_deref(), Some("Plan to approve"));
    }

    #[test]
    fn an_answered_or_expired_ask_does_not_need_you() {
        let mut signal = task(TaskStatus::Working);
        signal.open_asks.push(AskFacts {
            kind: AskKind::Permission,
            text: "Run: cargo test".to_string(),
            expires_at_ms: Some(NOW),
        });
        let got = derive_status(&signal, &one(Some(Activity::Working)), NOW);
        assert_eq!(got.status, DerivedStatus::Working);
        assert_ne!(got.needs, Some(Need::Asks));
    }

    #[test]
    fn a_log_only_reader_sees_an_open_card_as_needs_you() {
        // Gate run #4, bug 4: a task whose review could not start read
        // `Working · In review` while its card sat in `/api/state`'s `needs`,
        // so a person reading `shprd status` alone never learned they were
        // needed. An escalation is read exactly as an open ask is.
        let mut signal = task(TaskStatus::Working);
        signal.cards.push(CardFacts {
            body: String::from(
                "C1: the engine's review did not start: no workspace \"shprd · repo\" yet",
            ),
        });
        let got = derive_from_fold(&signal, NOW);
        assert_eq!(got.status, DerivedStatus::NeedsYou);
        assert_eq!(got.needs, Some(Need::Attention));
        assert!(
            got.reason
                .unwrap_or_default()
                .starts_with("C1: the engine's review did not start"),
            "the card's own reason is the row's"
        );

        // A reported task reads it too: nothing moves without the person,
        // whatever its own attempt last said.
        let mut reported = task(TaskStatus::Reported);
        reported.cards.push(CardFacts {
            body: String::from("C1: its `start` step has failed the same way 3 passes in a row"),
        });
        assert_eq!(
            derive_from_fold(&reported, NOW).status,
            DerivedStatus::NeedsYou
        );

        // And a task that is already done says nothing it still needs.
        for status in [
            TaskStatus::Integrated,
            TaskStatus::Dropped,
            TaskStatus::Failed,
        ] {
            let mut done = task(status);
            done.cards.push(CardFacts {
                body: String::from("a stale card"),
            });
            let got = derive_from_fold(&done, NOW);
            if status == TaskStatus::Failed {
                // A failed task is the person's anyway, but for its own reason:
                // the card is not what the row cites.
                assert_ne!(got.reason.as_deref(), Some("a stale card"));
            } else {
                assert_ne!(got.status, DerivedStatus::NeedsYou, "{status:?}");
            }
        }
    }

    #[test]
    fn an_empty_card_lists_reads_as_it_did_before() {
        // Nothing changes for the runs that have no escalation at all.
        let signal = task(TaskStatus::Working);
        assert_eq!(derive_from_fold(&signal, NOW), Derived::working(None));
    }

    #[test]
    fn a_log_only_reader_sees_an_open_ask_as_needs_you() {
        let mut signal = task(TaskStatus::Working);
        signal.open_asks.push(AskFacts {
            kind: AskKind::Question,
            text: "Which database should this use?".to_string(),
            expires_at_ms: None,
        });
        let got = derive_from_fold(&signal, NOW);
        assert_eq!(got.status, DerivedStatus::NeedsYou);
        assert_eq!(got.needs, Some(Need::Asks));
        assert_eq!(
            got.reason.as_deref(),
            Some("Which database should this use?")
        );
    }

    #[test]
    fn a_log_only_reader_never_calls_a_working_task_stopped() {
        // The CLI and the API cannot see the agent terminals; an empty overlay
        // would read "Needs you · Agent stopped" about every working task,
        // which is a claim about a terminal nobody looked at.
        let signal = task(TaskStatus::Working);
        assert_eq!(derive_from_fold(&signal, NOW), Derived::working(None));
        assert_eq!(
            derive_status(&signal, &LiveAgents::default(), NOW).needs,
            Some(Need::Stopped),
            "the rule itself still reads a known-empty overlay as no agent left"
        );
        // Neither does an ask that has already timed out.
        let mut expired = task(TaskStatus::Working);
        expired.open_asks.push(AskFacts {
            kind: AskKind::Permission,
            text: "Run: cargo test".to_string(),
            expires_at_ms: Some(NOW),
        });
        assert_eq!(derive_from_fold(&expired, NOW), Derived::working(None));
    }

    #[test]
    fn a_log_only_reader_reads_the_parked_states_from_the_fold() {
        assert_eq!(
            derive_from_fold(&task(TaskStatus::Queued), NOW).status,
            DerivedStatus::ToDo
        );
        assert_eq!(
            derive_from_fold(&task(TaskStatus::Integrated), NOW).status,
            DerivedStatus::Done
        );
        let mut failed = task(TaskStatus::Failed);
        failed.failure = Some("unwrap used".to_string());
        let got = derive_from_fold(&failed, NOW);
        assert_eq!(got.status, DerivedStatus::NeedsYou);
        assert_eq!(got.needs, Some(Need::Attention));
        assert_eq!(got.reason.as_deref(), Some("unwrap used"));
        // A finished task needs nothing, even if a hook's ask is still open
        // behind it: the live rules park it too, and the news must agree.
        let mut landed = task(TaskStatus::Integrated);
        landed.open_asks.push(AskFacts {
            kind: AskKind::Question,
            text: "Too late?".to_string(),
            expires_at_ms: None,
        });
        assert_eq!(derive_from_fold(&landed, NOW).status, DerivedStatus::Done);
    }
}
