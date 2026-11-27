//! Event types: the durable vocabulary of the append-only run log.
//!
//! Every state change in a run is one [`Event`]. Current state is a fold over
//! the log, and every view is rendered from the log, so these types are the
//! only way anything about a run is recorded.

use serde::{Deserialize, Serialize};

use crate::ask::{AskId, AskKind};
use crate::baseline::BaselineEntry;

/// Identifies a task inside a run.
pub type TaskId = String;

/// The next free short task id, `T<n>`, for a log that holds `events`.
///
/// It is one more than the highest `T<n>` any `TaskAdded` event already names,
/// and is bumped past any id in use, so a long id a person gave never blocks
/// it. Ids that are not `T<digits>` are ignored when counting.
pub fn next_task_id(events: &[Event]) -> TaskId {
    let used: std::collections::HashSet<&str> = events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::TaskAdded { task, .. } => Some(task.as_str()),
            _ => None,
        })
        .collect();
    let mut n = used
        .iter()
        .filter_map(|id| id.strip_prefix('T')?.parse::<u64>().ok())
        .max()
        .map_or(1, |highest| highest.saturating_add(1));
    // Past u64::MAX there is no next number: fall back to the first free one
    // below, and if every number is taken stop rather than loop forever.
    while used.contains(format!("T{n}").as_str()) {
        n = match n.checked_add(1) {
            Some(next) => next,
            None => (1..u64::MAX)
                .find(|k| !used.contains(format!("T{k}").as_str()))
                .unwrap_or(1),
        };
    }
    format!("T{n}")
}

/// How an attempt is named for a person: `<id>·a<n>`, e.g. `T3·a2`.
pub fn attempt_label(task: &str, attempt: u32) -> String {
    format!("{task}·a{attempt}")
}

/// Identifies an agent (a worker slot in the roster).
pub type AgentId = String;

/// Who caused an event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Actor {
    /// The engine itself: spawning, gating, integrating.
    Engine,
    /// The orchestrator process driving the run.
    Orchestrator,
    /// A specific agent.
    Agent {
        /// The agent's id.
        id: AgentId,
    },
    /// A human operator.
    Human,
}

/// One line of the append-only run log.
///
/// `seq` starts at 1 and increases by exactly one per event. `at_ms` is the
/// wall-clock time the event was appended, in milliseconds since the Unix
/// epoch. The variant payload is flattened into the same JSON object as
/// `seq`, `at_ms` and `actor`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// Monotonic position of this event in the log, starting at 1.
    pub seq: u64,
    /// Wall-clock time the event was appended, ms since the Unix epoch.
    pub at_ms: u64,
    /// Who caused the event.
    pub actor: Actor,
    /// What happened.
    #[serde(flatten)]
    pub kind: EventKind,
}

/// Outcome of a single task attempt, as reported by the agent that ran it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportStatus {
    /// The attempt finished its scope and is ready to integrate.
    Completed,
    /// The attempt stopped on something it could not resolve.
    Blocked,
    /// The attempt failed.
    Failed,
}

/// How many characters a task's status line may carry.
///
/// A status is a progress line, not a message: the cap is what keeps a pasted
/// agent message out of the log.
pub const TASK_STATUS_MAX_CHARS: usize = 40;

/// The longest status line the engine itself writes: it says why a step is
/// waiting, which needs room for a path and a task id.
pub const ENGINE_STATUS_MAX_CHARS: usize = 120;

/// A one-line task status, trimmed and capped for the log.
///
/// Whitespace runs collapse to one space, so a pasted block of text becomes a
/// single line, and anything past [`TASK_STATUS_MAX_CHARS`] is cut with an
/// ellipsis rather than stored whole.
pub fn status_line(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= TASK_STATUS_MAX_CHARS {
        return collapsed;
    }
    let mut capped: String = collapsed.chars().take(TASK_STATUS_MAX_CHARS - 1).collect();
    capped.push('\u{2026}');
    capped
}

/// What kind of provider trouble cut a turn off, as the fallback ladder reads
/// it.
///
/// It mirrors `shprd_watch::records::ErrorKind`; the tick maps one onto the
/// other because `shprd-core` sits below the watchdog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderClass {
    /// A rate limit: waiting and trying again is the fix.
    RateLimit,
    /// The account is out of allowance: a quota, credit or billing limit.
    Quota,
    /// The credentials were rejected: the login is gone.
    Auth,
    /// The provider is down or overloaded.
    Overloaded,
    /// Anything else the reader could not classify.
    Other,
}

/// What the fallback ladder did about one provider cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FallbackAction {
    /// A carry-on message was sent.
    Nudged,
    /// The task was handed to another program, once.
    Backup,
    /// The ladder ended and a person was asked.
    GaveUp,
}

/// How many lines of a hand-in check's output the event keeps.
pub const CHECK_OUTPUT_LINES: usize = 20;

/// How many characters of a hand-in check's output the event keeps.
///
/// A failing build can print megabytes on one line, and the log is folded on
/// every command, so the tail is capped by characters as well as by lines.
pub const CHECK_OUTPUT_TAIL_CHARS: usize = 2000;

/// The last few lines of `text`, capped for the log.
///
/// At most [`CHECK_OUTPUT_LINES`] lines and [`CHECK_OUTPUT_TAIL_CHARS`]
/// characters are kept, counted from the end so the failure itself survives.
pub fn output_tail(text: &str) -> String {
    let mut lines: Vec<&str> = text.trim_end().lines().collect();
    if lines.len() > CHECK_OUTPUT_LINES {
        lines.drain(..lines.len() - CHECK_OUTPUT_LINES);
    }
    let mut tail = lines.join("\n");
    while tail.chars().count() > CHECK_OUTPUT_TAIL_CHARS {
        match tail.split_once('\n') {
            Some((_, rest)) => tail = rest.to_string(),
            None => {
                let skip = tail.chars().count() - CHECK_OUTPUT_TAIL_CHARS;
                tail = tail.chars().skip(skip).collect();
            }
        }
    }
    tail
}

/// One gate a worker ran before handing its work in.
///
/// The hand-in is the worker's claim that the work is tested: this records what
/// it actually ran, so the engine and the reviewer can check the claim instead
/// of trusting the summary. A hand-in that names no check is refused, and so is
/// one whose check did not pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckReport {
    /// Gate name, e.g. `format`, `lint`, `tests`.
    pub gate: String,
    /// The command, as the worker ran it.
    pub command: String,
    /// Its exit code, or `None` when a timeout killed it.
    pub exit_code: Option<i32>,
    /// The last lines it printed, capped at [`CHECK_OUTPUT_LINES`].
    #[serde(default)]
    pub output_tail: String,
    /// Set by the engine when the check failed no worse than the gate did on
    /// the base: `pre-existing on base (<sha>, N errors on base; exit E)`. The
    /// exit code above stays the real one; the rules, not this note, decide a
    /// hand-in may carry the failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_existing: Option<String>,
}

impl CheckReport {
    /// A check that passed, for a caller that only needs the shape.
    ///
    /// The hand-in verbs build one from what the worker actually ran; this is
    /// for fixtures and for the engine recording a check it ran itself.
    pub fn passed(gate: &str, command: &str) -> Self {
        Self {
            gate: gate.to_string(),
            command: command.to_string(),
            exit_code: Some(0),
            output_tail: String::new(),
            pre_existing: None,
        }
    }
}

/// How bad a gate finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Must be fixed before integration.
    Critical,
    /// Should be fixed before integration.
    Major,
    /// Worth fixing, not blocking.
    Minor,
    /// Cosmetic.
    Trivial,
}

/// One finding reported by a gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    /// How bad it is.
    pub severity: Severity,
    /// One-line title.
    pub title: String,
    /// Where it is, e.g. `crates/shprd-core/src/log.rs:42`.
    pub location: Option<String>,
    /// The full detail, including what to do about it.
    pub detail: String,
}

/// One command of the run-wide gate, and how it went.
///
/// The step is recorded whole so a reviewer can see what ran and why it failed
/// without the machine that ran it: the command string, its exit code (`None`
/// when a timeout killed it) and the tail of what it printed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GateStep {
    /// The command, as the config spelled it.
    pub command: String,
    /// Its exit code, or `None` when a timeout killed it.
    pub exit_code: Option<i32>,
    /// How long it took, in milliseconds.
    pub duration_ms: u64,
    /// The last lines it printed, capped at [`GATE_OUTPUT_TAIL_CHARS`].
    pub output_tail: String,
}

/// How many characters of a gate step's output the event keeps.
///
/// A failing build can print megabytes and the log is folded on every command,
/// so the tail is capped rather than kept whole.
pub const GATE_OUTPUT_TAIL_CHARS: usize = 4000;

/// What happened to a resource shprd owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceAction {
    /// shprd created it.
    Created,
    /// shprd kept it but took it out of service.
    Archived,
    /// shprd deleted it.
    Removed,
}

/// A kind of resource shprd creates and is therefore allowed to clean up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    /// A git worktree.
    Worktree,
    /// A local branch.
    Branch,
    /// A branch on a remote.
    RemoteBranch,
    /// An archive ref holding a run's history.
    ArchiveRef,
    /// An exported bundle.
    Bundle,
    /// A run's review packet folder, `<home>/.shprd/reviews/<run>`.
    ///
    /// It is shprd's own directory, named by the layout and a validated run id
    /// rather than by anything a repository holds, and the janitor copies it
    /// aside before it deletes it. Unlike every other kind, a removal may name
    /// a folder no earlier record ever held: shprd wrote packets there before it
    /// kept this ledger, and a finished run can record no creation. The plan is
    /// what proves the folder is shprd's own; the removal is its audit trail.
    ReviewPackets,
}

/// Where a review verdict came from: the reviewer, and the findings it decided.
///
/// A verdict is only a verdict when something proves a reviewer wrote it, so the
/// gate result carries the reviewer the dispatch named and the SHA-256 of the
/// exact findings bytes the engine read. A caller that records a review gate
/// without this is refused, and a verdict recorded before this field existed
/// folds with `None` and reads as unproven.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewSource {
    /// The agent the review was dispatched to, as [`EventKind::ReviewRequested`]
    /// recorded it.
    pub reviewer: AgentId,
    /// SHA-256 of the findings file's bytes, as lowercase hex.
    pub findings_sha256: String,
    /// The `seq` of the [`EventKind::ReviewRequested`] this verdict answers.
    ///
    /// A restarted review records a fresh request for the same attempt, and it
    /// may name the same reviewer, so who reviewed does not tell the two
    /// requests apart. The request's position does: a verdict written for a
    /// request that has since been superseded is refused instead of being read
    /// as the current review's own.
    ///
    /// Defaulted to `0` so a log written before this field existed still
    /// folds; no request has that seq, so a verdict recorded now names one.
    #[serde(default)]
    pub request_seq: u64,
}

/// How landing one task ended.
///
/// A task lands through its own pull request, and the outcome says what the
/// branch and its base had left to do. Every outcome is recorded once, so a
/// second landing reads what already happened instead of merging twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LandOutcome {
    /// Its pull request was merged by this landing.
    Merged,
    /// Its pull request was already merged when this landing looked.
    AlreadyMerged,
    /// Its own work is in the base branch as different commits: a squash merge,
    /// or a merge made by hand.
    SquashMerged,
    /// Its branch held no work of its own beyond the base branch.
    NothingToMerge,
}

impl LandOutcome {
    /// True when this outcome means the task's work is in its base branch.
    ///
    /// `NothingToMerge` is the one that is not: the branch had nothing to add,
    /// so nothing of it landed.
    pub fn is_landed(self) -> bool {
        !matches!(self, LandOutcome::NothingToMerge)
    }
}

/// What happened. Serialized with a `kind` tag alongside the [`Event`] fields.
///
/// Unknown-field rejection is not enabled here: `deny_unknown_fields` cannot
/// be combined with the internally tagged representation used by this enum
/// (nor with the flattened field on [`Event`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventKind {
    /// A run was started.
    RunStarted {
        /// Run identifier.
        run_id: String,
        /// Repository the run works on.
        repo: String,
        /// Branch the run was cut from.
        base_branch: String,
        /// Branch landed work is integrated on.
        integration_branch: String,
    },
    /// A task was added to the run's plan.
    TaskAdded {
        /// The task.
        task: TaskId,
        /// Human-readable title.
        title: String,
        /// What the task must change.
        scope: Vec<String>,
        /// What must be true when it is done.
        acceptance: Vec<String>,
        /// Free-form tags.
        tags: Vec<String>,
        /// Skills the agent is told to load.
        skills: Vec<String>,
        /// Tasks that must land before this one.
        deps: Vec<TaskId>,
    },
    /// An agent was spawned for a task attempt.
    Spawned {
        /// The task.
        task: TaskId,
        /// Attempt number, starting at 1.
        attempt: u32,
        /// The agent that was spawned.
        agent: AgentId,
        /// Model the agent runs on.
        model: Option<String>,
        /// Worktree the agent works in.
        worktree: String,
        /// Branch the agent commits on.
        branch: String,
    },
    /// An agent reported the outcome of an attempt.
    Reported {
        /// The task.
        task: TaskId,
        /// Attempt number.
        attempt: u32,
        /// Outcome.
        status: ReportStatus,
        /// What the agent says it did.
        summary: String,
        /// Test commands the agent ran, with their result.
        tests: Vec<String>,
        /// The gates the agent ran for the hand-in, with their result.
        ///
        /// Defaulted so a log written before hand-ins carried checks still
        /// folds; the fold does not re-judge history.
        #[serde(default)]
        checks: Vec<CheckReport>,
        /// Paths the agent changed.
        changed_paths: Vec<String>,
        /// Commit the agent made, if any.
        commit: Option<String>,
        /// Skills the agent used.
        skills_used: Vec<String>,
        /// Where this attempt left its proof, when the worker named one.
        ///
        /// A proof is the artifact a person can look at — a screenshot, a
        /// command's real output, a test run — and it belongs beside the
        /// summary, not inside it: the summary is one plain line for the
        /// person, and a path there would be jargon.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        proofs: Vec<String>,
    },
    /// An agent recorded where it is on a task.
    ///
    /// This is a progress line, not a report: it never moves the task's status
    /// and it is capped at [`TASK_STATUS_MAX_CHARS`], so a stream of them costs
    /// the log a short line each.
    TaskStatus {
        /// The task.
        task: TaskId,
        /// A short progress line, trimmed and capped by the writer.
        message: String,
        /// How far the agent says it has got, from 0 to 100.
        progress: Option<u8>,
    },
    /// An agent raised a question, permission or plan and waits for an answer.
    ///
    /// The ask itself lives in the file queue beside the log
    /// ([`crate::ask::AskQueue`]); this event is the durable record the fold
    /// and every view read, so a card and the board cannot disagree about
    /// whether something still needs the person.
    AskRaised {
        /// The task it belongs to.
        task: TaskId,
        /// Its id, unique inside the task.
        ask: AskId,
        /// Question, permission or plan.
        ///
        /// Spelled `ask_kind` because `kind` is the event tag itself.
        ask_kind: AskKind,
        /// What the person is asked.
        text: String,
        /// The choices a card offers, when it has any.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        options: Vec<String>,
        /// When it was raised, ms since the Unix epoch.
        created_at_ms: u64,
        /// When it stops being open, ms since the Unix epoch, when it does.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expires_at_ms: Option<u64>,
    },
    /// A raised ask was answered; the answer is what the waiting hook prints.
    ///
    /// An answer to an ask the log does not know, or to one already answered,
    /// is refused by the rules ([`crate::rules::RuleError::UnknownAsk`],
    /// [`crate::rules::RuleError::AskAlreadyAnswered`]).
    AskAnswered {
        /// The task it belongs to.
        task: TaskId,
        /// The ask.
        ask: AskId,
        /// What the person answered.
        answer: String,
    },
    /// A gate passed for an attempt.
    GatePassed {
        /// The task.
        task: TaskId,
        /// Attempt number.
        attempt: u32,
        /// Gate name.
        gate: String,
        /// Findings the gate raised, if any.
        findings: Vec<Finding>,
        /// Provenance, for a review gate only.
        ///
        /// Defaulted so a log written before provenance existed still folds.
        #[serde(default)]
        source: Option<ReviewSource>,
    },
    /// A gate failed for an attempt.
    GateFailed {
        /// The task.
        task: TaskId,
        /// Attempt number.
        attempt: u32,
        /// Gate name.
        gate: String,
        /// Why it failed.
        findings: Vec<Finding>,
        /// Provenance, for a review gate only.
        ///
        /// Defaulted so a log written before provenance existed still folds.
        #[serde(default)]
        source: Option<ReviewSource>,
    },
    /// A task's work was integrated.
    Integrated {
        /// The task.
        task: TaskId,
        /// Commits that were landed.
        commits: Vec<String>,
    },
    /// A human decided a task will not be done.
    ///
    /// The other way a task leaves the run's work: `Integrated` puts its
    /// commits on the integration branch, and this says the commits are never
    /// coming. It is the human's decision alone, because a task nobody does is
    /// work no agent may decide to skip, and it is terminal: the worktree and
    /// the branch are kept, so whatever the task did is still reachable.
    TaskDropped {
        /// The task.
        task: TaskId,
        /// Why the human dropped it, in their words. It is what the run's pull
        /// request lists, so a reader sees what was left out and why.
        reason: String,
    },
    /// A person widened a task's scope.
    ///
    /// The scope is what a hand-in may change, so widening it is the person's
    /// own call (a worker that could widen its own scope would have none). It
    /// is what a refused hand-in's card offers: the paths named are added to the
    /// task's scope, and the worker hands in again.
    ScopeWidened {
        /// The task.
        task: TaskId,
        /// The paths added, as the person gave them.
        paths: Vec<String>,
    },
    /// The agent working on a task was stopped, so the attempt has no agent.
    ///
    /// It is the person's own verb (`task.stop` on the API), and the fact it
    /// records is about the attempt's terminal rather than about the work: the
    /// space was closed, so nobody is on the task any more. Shepherdr's rule is
    /// that such a task reads *Needs you · Agent stopped* instead of staying
    /// Working for ever (M7a §5), so the fold marks that attempt stopped and
    /// fails it: a stopped attempt is one a retry may replace, which is what
    /// keeps the task reachable rather than wedged.
    TaskStopped {
        /// The task.
        task: TaskId,
        /// The attempt whose agent was stopped.
        attempt: u32,
    },
    /// A person moved a handed-in task back to Working, for the same attempt.
    ///
    /// The recovery for a task stuck at Reported or Passed (a review that
    /// cannot start, a scope fix the worker has to hand in again): the attempt,
    /// its worktree and its branch stay, and the attempt's report, review
    /// requests and gate results are cleared so the worker can hand in again
    /// and be reviewed afresh. The earlier ones stay in the log. It is a human's
    /// verb alone.
    TaskReopened {
        /// The task.
        task: TaskId,
        /// The attempt that is open again.
        attempt: u32,
    },
    /// A review was handed to a reviewer for an attempt.
    ///
    /// `degraded` is the fact a landing decision needs: it records that the
    /// reviewer shares the author's vendor, so no independent review happened.
    /// Recording it here keeps `shprd land` from guessing it out of prose.
    ReviewRequested {
        /// The task.
        task: TaskId,
        /// Attempt number.
        attempt: u32,
        /// The agent string the review runs on.
        reviewer: String,
        /// True when the reviewer shares the author's vendor.
        degraded: bool,
        /// One plain line when this review is not the first choice, for the
        /// task's card: "codex is out of quota until Dec 18; GLM (pi) reviews
        /// instead". Empty for an ordinary review.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    /// Review is off for this repository, so a hand-in that passed its own
    /// checks passes on them alone.
    ///
    /// The engine records it, once, for an attempt that no review was ever
    /// requested for while the repository's review is switched off
    /// (`tasks.review_rounds = 0`, or no reviewer). It is the one way to
    /// `Passed` that has no review verdict, and being an event it is on the
    /// log: a review that merely failed to start never records it.
    ReviewSkipped {
        /// The task.
        task: TaskId,
        /// Attempt number.
        attempt: u32,
    },
    /// An agent was put on cooldown (rate limit, quota, auth).
    Dry {
        /// The agent.
        agent: AgentId,
        /// Why it is dry.
        reason: String,
        /// When it is expected back, ms since the Unix epoch.
        reset_at_ms: Option<u64>,
    },
    /// A dry agent was re-admitted (its cooldown was cleared).
    Available {
        /// The agent.
        agent: AgentId,
        /// Why it is usable again.
        reason: String,
    },
    /// A task was handed from one agent to another.
    HandedOff {
        /// The task.
        task: TaskId,
        /// The agent handing it over.
        from_agent: AgentId,
        /// The agent taking it over.
        to_agent: AgentId,
        /// The checkpoint the new agent should resume from.
        checkpoint: String,
        /// Why it was handed over, in the words of the verdict that caused it.
        ///
        /// A worker process exiting hands the attempt on without drying the
        /// agent, so the reason is the only record that the attempt's worker
        /// was lost. Defaulted so a log written before it existed still folds.
        #[serde(default)]
        reason: Option<String>,
    },
    /// Something needs a human decision.
    Escalated {
        /// The task it concerns, when it is not run-wide.
        task: Option<TaskId>,
        /// What needs deciding.
        body: String,
        /// The action taken if nobody answers.
        default_action: String,
    },
    /// A person dismissed a card: the escalation recorded at `seq` is no longer
    /// in front of them.
    ///
    /// A human writes it, or the engine when the cause of a card it raised is
    /// gone; only for the sequence number of an `Escalated` event the run holds. The card reads as history from then on;
    /// a log written before this event existed folds unchanged.
    NeedDismissed {
        /// Sequence number of the `Escalated` event whose card is dismissed
        /// (not called `seq`: the event's own `seq` shares its object).
        need: u64,
    },
    /// A human overrode a rule.
    Override {
        /// The task, or run-wide rule when empty.
        task: TaskId,
        /// The rule that was overridden.
        rule: String,
        /// Why it was overridden.
        reason: String,
    },
    /// A resource was created, archived or removed.
    Resource {
        /// What happened to it.
        action: ResourceAction,
        /// What sort of resource it is.
        ///
        /// Serialized as `resource`, because `kind` is taken by the tag that
        /// names the event variant.
        #[serde(rename = "resource")]
        kind: ResourceKind,
        /// Its name: a worktree path, a branch name, an archive ref.
        name: String,
        /// The task it belongs to, when it belongs to one.
        task: Option<TaskId>,
    },
    /// The run-wide gate ran over the integration branch.
    ///
    /// One event per `shprd gate` run, holding every step it reached: the
    /// whole verdict is one log line, so a reader never has to join steps.
    FullGate {
        /// The integration branch's head the gate ran on.
        head: String,
        /// True when every step it ran passed.
        passed: bool,
        /// The steps it ran, in order.
        steps: Vec<GateStep>,
    },
    /// A pull request was opened, or its body refreshed, for the run's
    /// integration branch.
    PrOpened {
        /// The forge's number for the pull request.
        number: u64,
        /// The URL a human opens to read, approve or merge it.
        url: String,
        /// The branch the pull request proposes to merge.
        head: String,
    },
    /// The run's pull request was merged.
    PrMerged {
        /// The forge's number for the pull request.
        number: u64,
        /// The head that was merged.
        head: String,
    },
    /// A human approved the run's pull request in shprd itself.
    ///
    /// A solo maintainer cannot approve their own pull request on GitHub, so
    /// the approval a merge policy waits for can also be recorded here. It
    /// names the head it was given for: an approval of an older head says
    /// nothing about the commits now on top of it.
    Approved {
        /// The forge's number for the pull request.
        number: u64,
        /// The integration head the human approved.
        head_sha: String,
    },
    /// A task's own pull request was opened from its branch, or its body
    /// refreshed.
    ///
    /// The per-task workflow gives every task its own pull request into the
    /// run's base branch, so the task's log holds what the run's pull request
    /// used to: the forge's number, its URL and the head it proposes.
    TaskPrOpened {
        /// The task.
        task: TaskId,
        /// The forge's number for the pull request.
        number: u64,
        /// The URL a human opens to read, approve or merge it.
        url: String,
        /// The branch the pull request proposes to merge.
        head: String,
    },
    /// A human approved a task's pull request in shprd itself.
    ///
    /// Like [`EventKind::Approved`], for the task's own pull request: the
    /// approval names the head it was given for, and the `ask` policy honours
    /// it only while that is still the branch's tip.
    TaskApproved {
        /// The task.
        task: TaskId,
        /// The forge's number for the pull request, or `0` when the task lands
        /// locally and has no pull request.
        number: u64,
        /// The commit the human approved.
        head_sha: String,
    },
    /// Landing one task ended: its work is in its base branch, or it had
    /// nothing to add.
    ///
    /// Recorded once per outcome, so a re-run of `shprd land` reads the fact
    /// instead of opening a second pull request or merging again.
    TaskLanded {
        /// The task.
        task: TaskId,
        /// The base branch its work went into, or would have gone into.
        base: String,
        /// The branch tip the landing looked at.
        head: String,
        /// The pull request that carried it, when one did.
        number: Option<u64>,
        /// What happened to it.
        outcome: LandOutcome,
    },
    /// shprd started the orchestrator in a pane.
    ///
    /// The process group is the orchestrator's identity for the rest of the
    /// run: a host that restores the tab and relaunches the CLI itself gives
    /// the pane a different group, which is how `up` tells a session it
    /// started from one it did not.
    OrchestratorLaunched {
        /// The title of the orchestrator's tab.
        title: String,
        /// The pane the orchestrator runs in.
        pane: String,
        /// The pane's foreground process group at launch.
        process_group: u32,
    },
    /// shprd started the watchdog in a pane.
    ///
    /// The watchdog is the process that turns a reviewer's findings into a
    /// verdict and hands off a dry worker, so `up` starts it the way it starts
    /// the orchestrator: the pane and its process group are what a later `up`
    /// checks before it trusts the pane, and what run end closes.
    WatchLaunched {
        /// The pane the watchdog runs in.
        pane: String,
        /// The pane's foreground process group at launch.
        ///
        /// `None` is a host that cannot tell. It is not evidence against the
        /// pane, so a later `up` leaves it alone rather than splitting a second
        /// watchdog off the chat tab.
        process_group: Option<u32>,
    },
    /// The watchdog told the orchestrator about the log up to this point.
    ///
    /// The orchestrator is an interactive CLI: it only acts when it is given
    /// input, so the watchdog prompts its pane. Two workers can report and the
    /// orchestrator sits idle until a human notices, unless something tells it.
    /// This event is that delivery's watermark: it names the last sequence the
    /// orchestrator was told about, so a restarted watchdog neither replays old
    /// notices nor drops new ones.
    OrchestratorNotified {
        /// The last log sequence the orchestrator was told about.
        up_to_seq: u64,
    },
    /// The base branch was merged into the run's integration branch.
    ///
    /// Recorded by `shprd sync` when the base moved under the run — a pull
    /// request merged meanwhile — so the integration branch is not left behind
    /// the branch it proposes to merge into. The merge is always a merge
    /// commit, never a rebase, so a task's commits keep the ids the pull
    /// request body lists.
    ///
    /// Both ends are recorded because the run-wide gate is keyed to the
    /// integration head: a sync moves that head, so a gate that ran before it
    /// no longer describes the commits that would land.
    BaseSynced {
        /// The base commit that was merged in.
        base_sha: String,
        /// The integration branch's head after the merge.
        int_sha: String,
    },
    /// Each worker gate was run on the base: how it fared, with a fingerprint
    /// of any failure.
    ///
    /// Recorded when a run starts and again on every sync that moves the base.
    /// A hand-in whose gate fails no worse than its baseline is accepted as
    /// pre-existing; only a regression refuses it.
    GateBaselined {
        /// The base commit the gates ran on.
        base_sha: String,
        /// One entry per worker gate command.
        entries: Vec<BaselineEntry>,
    },
    /// A provider cut a turn off and the fallback ladder sent the agent a
    /// carry-on message.
    ///
    /// The event is the ladder's own clock: it names the cut it answers, so a
    /// later pass neither sends the same nudge twice nor restarts the waits.
    Nudged {
        /// The task.
        task: TaskId,
        /// Attempt number.
        attempt: u32,
        /// The program that was cut off.
        agent: AgentId,
        /// A stable identity for the cut, from the CLI's own record.
        cut: String,
        /// What kind of provider trouble it was.
        class: ProviderClass,
        /// Why, in a few words.
        reason: String,
        /// Which try this nudge is.
        tries: u32,
    },
    /// The fallback ladder handed a task to another program, once.
    ///
    /// `started` is false when the handover was attempted and could not be
    /// made: it is still recorded, because Shepherdr's rule is one try per
    /// task and the person decides after a failed one. A backup is never
    /// recorded twice for one task.
    Backup {
        /// The task.
        task: TaskId,
        /// The attempt that continues.
        attempt: u32,
        /// The program handing the task over.
        from_agent: AgentId,
        /// The program taking it over.
        to_agent: AgentId,
        /// The cut that ended the ladder.
        cut: String,
        /// What kind of provider trouble ended it.
        class: ProviderClass,
        /// Why the handover happened.
        reason: String,
        /// The worktree the newcomer continues in.
        worktree: String,
        /// The branch the newcomer commits on.
        branch: String,
        /// Whether the backup actually started.
        started: bool,
    },
    /// The fallback ladder ended without a backup: a person has to decide.
    ProviderGaveUp {
        /// The task.
        task: TaskId,
        /// Attempt number.
        attempt: u32,
        /// The program that cannot go on.
        agent: AgentId,
        /// The cut that ended the ladder.
        cut: String,
        /// What kind of provider trouble it was.
        class: ProviderClass,
        /// Why it needs the person, in one line.
        reason: String,
        /// Which try ended the ladder.
        tries: u32,
    },
    /// The run finished.
    RunFinished {
        /// The tasks the run ended with unfinished, when a human abandoned
        /// them.
        ///
        /// Empty for a run that ended with its work landed. Non-empty is the
        /// one explicit choice to end a run anyway (`--abandon`): the tasks
        /// keep the branches their work sits on, and `up` says so when it
        /// archives the run. Defaulted, so a log written before the field
        /// existed still folds.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        abandoned: Vec<TaskId>,
    },
}

impl EventKind {
    /// The task this event is about, when it is about one.
    ///
    /// The log is how a reader follows one task: which events name it is the
    /// same question every reader asks, so it is answered once here rather
    /// than matched again in each view. Run-wide events (`RunStarted`,
    /// `FullGate`, `BaseSynced`, `GateBaselined`, the two launcher events) name none, and
    /// `Escalated` names one only when it is not run-wide.
    pub fn task(&self) -> Option<&str> {
        match self {
            Self::TaskAdded { task, .. }
            | Self::Spawned { task, .. }
            | Self::Reported { task, .. }
            | Self::TaskStatus { task, .. }
            | Self::AskRaised { task, .. }
            | Self::AskAnswered { task, .. }
            | Self::GatePassed { task, .. }
            | Self::GateFailed { task, .. }
            | Self::Integrated { task, .. }
            | Self::TaskDropped { task, .. }
            | Self::ScopeWidened { task, .. }
            | Self::TaskStopped { task, .. }
            | Self::TaskReopened { task, .. }
            | Self::ReviewRequested { task, .. }
            | Self::ReviewSkipped { task, .. }
            | Self::HandedOff { task, .. }
            | Self::Override { task, .. }
            | Self::TaskPrOpened { task, .. }
            | Self::TaskApproved { task, .. }
            | Self::TaskLanded { task, .. }
            | Self::Nudged { task, .. }
            | Self::Backup { task, .. }
            | Self::ProviderGaveUp { task, .. } => Some(task),
            Self::Escalated { task, .. } => task.as_deref(),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn added(seq: u64, id: &str) -> Event {
        Event {
            seq,
            at_ms: seq,
            actor: Actor::Human,
            kind: EventKind::TaskAdded {
                task: id.to_string(),
                title: "t".to_string(),
                scope: Vec::new(),
                acceptance: vec!["ok".to_string()],
                tags: Vec::new(),
                skills: Vec::new(),
                deps: Vec::new(),
            },
        }
    }

    #[test]
    fn the_allocator_ends_at_the_u64_limit() {
        let log = [added(1, "T18446744073709551615")];
        assert_eq!(next_task_id(&log), "T1");
    }

    #[test]
    fn the_next_task_id_is_one_past_the_highest_short_id() {
        assert_eq!(next_task_id(&[]), "T1");
        let log = [added(1, "T1"), added(2, "long-slug"), added(3, "T7")];
        assert_eq!(next_task_id(&log), "T8");
        // A long id never blocks, and an id in use is skipped.
        assert_eq!(next_task_id(&[added(1, "M1.6")]), "T1");
    }

    #[test]
    fn an_attempt_reads_as_the_task_id_and_a_number() {
        assert_eq!(attempt_label("T3", 2), "T3·a2");
        assert_eq!(attempt_label("long-slug", 1), "long-slug·a1");
    }

    fn to_json<T: Serialize>(value: &T) -> String {
        match serde_json::to_string(value) {
            Ok(text) => text,
            Err(err) => panic!("serialize failed: {err}"),
        }
    }

    fn from_json<T: serde::de::DeserializeOwned>(text: &str) -> T {
        match serde_json::from_str(text) {
            Ok(value) => value,
            Err(err) => panic!("deserialize failed: {err}\n{text}"),
        }
    }

    fn finding() -> Finding {
        Finding {
            severity: Severity::Major,
            title: "clippy warning".to_string(),
            location: Some("crates/shprd-core/src/log.rs:42".to_string()),
            detail: "unwrap used".to_string(),
        }
    }

    fn check() -> CheckReport {
        CheckReport {
            gate: "tests".to_string(),
            command: "cargo test --workspace".to_string(),
            exit_code: Some(0),
            output_tail: "test result: ok. 42 passed".to_string(),
            pre_existing: None,
        }
    }

    /// Every kind, with non-trivial field values.
    fn every_kind() -> Vec<EventKind> {
        vec![
            EventKind::RunStarted {
                run_id: "run-1".to_string(),
                repo: "/tmp/repo".to_string(),
                base_branch: "main".to_string(),
                integration_branch: "integration/m1".to_string(),
            },
            EventKind::TaskAdded {
                task: "M1.1".to_string(),
                title: "event log".to_string(),
                scope: vec!["crates/shprd-core".to_string()],
                acceptance: vec!["round trip".to_string()],
                tags: vec!["core".to_string()],
                skills: vec!["tdd".to_string()],
                deps: vec!["M0.2".to_string()],
            },
            EventKind::Spawned {
                task: "M1.1".to_string(),
                attempt: 1,
                agent: "claude".to_string(),
                model: Some("opus".to_string()),
                worktree: "/tmp/wt".to_string(),
                branch: "feature/m1-events".to_string(),
            },
            EventKind::Reported {
                task: "M1.1".to_string(),
                attempt: 1,
                status: ReportStatus::Completed,
                summary: "done".to_string(),
                tests: vec!["cargo test".to_string()],
                checks: vec![check()],
                changed_paths: vec!["crates/shprd-core/src/event.rs".to_string()],
                commit: Some("abc123".to_string()),
                skills_used: vec!["tdd".to_string()],
                proofs: Vec::new(),
            },
            EventKind::TaskStatus {
                task: "M1.1".to_string(),
                message: "writing the fold".to_string(),
                progress: Some(40),
            },
            EventKind::GatePassed {
                task: "M1.1".to_string(),
                attempt: 1,
                gate: "review".to_string(),
                findings: vec![],
                source: Some(ReviewSource {
                    reviewer: "codex/m1".to_string(),
                    findings_sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb41"
                        .to_string(),
                    request_seq: 5,
                }),
            },
            EventKind::GateFailed {
                task: "M1.1".to_string(),
                attempt: 2,
                gate: "tests".to_string(),
                findings: vec![finding()],
                source: None,
            },
            EventKind::Integrated {
                task: "M1.1".to_string(),
                commits: vec!["abc123".to_string(), "def456".to_string()],
            },
            EventKind::TaskDropped {
                task: "M1.2".to_string(),
                reason: "not needed after all".to_string(),
            },
            EventKind::ScopeWidened {
                task: "M1.3".to_string(),
                paths: vec!["tests".to_string()],
            },
            EventKind::TaskStopped {
                task: "M1.3".to_string(),
                attempt: 1,
            },
            EventKind::TaskReopened {
                task: "M1.3".to_string(),
                attempt: 1,
            },
            EventKind::ReviewSkipped {
                task: "M1.1".to_string(),
                attempt: 1,
            },
            EventKind::ReviewRequested {
                task: "M1.1".to_string(),
                attempt: 1,
                reviewer: "codex".to_string(),
                degraded: true,
                note: None,
            },
            EventKind::Dry {
                agent: "codex".to_string(),
                reason: "rate limited".to_string(),
                reset_at_ms: Some(1_700_000_000_000),
            },
            EventKind::HandedOff {
                task: "M1.1".to_string(),
                from_agent: "claude".to_string(),
                to_agent: "codex".to_string(),
                checkpoint: "after log.rs".to_string(),
                reason: Some("agent process exited".to_string()),
            },
            EventKind::Escalated {
                task: None,
                body: "two gates disagree".to_string(),
                default_action: "stop the run".to_string(),
            },
            EventKind::NeedDismissed { need: 7 },
            EventKind::Override {
                task: "M1.1".to_string(),
                rule: "no_main_push".to_string(),
                reason: "release branch".to_string(),
            },
            EventKind::Resource {
                action: ResourceAction::Created,
                kind: ResourceKind::Worktree,
                name: "/tmp/wt/M1.1".to_string(),
                task: Some("M1.1".to_string()),
            },
            EventKind::FullGate {
                head: "abc1234".to_string(),
                passed: false,
                steps: vec![
                    GateStep {
                        command: "cargo test --workspace".to_string(),
                        exit_code: Some(101),
                        duration_ms: 1200,
                        output_tail: "test result: FAILED".to_string(),
                    },
                    GateStep {
                        command: "cargo fmt --all --check".to_string(),
                        exit_code: None,
                        duration_ms: 30_000,
                        output_tail: "timed out".to_string(),
                    },
                ],
            },
            EventKind::PrOpened {
                number: 7,
                url: "https://example.invalid/pr/7".to_string(),
                head: "shprd/r1/int".to_string(),
            },
            EventKind::PrMerged {
                number: 7,
                head: "abc1234".to_string(),
            },
            EventKind::Approved {
                number: 7,
                head_sha: "abc1234".to_string(),
            },
            EventKind::OrchestratorLaunched {
                title: "★ claude·orchestrator".to_string(),
                pane: "w1:p2".to_string(),
                process_group: 88158,
            },
            EventKind::OrchestratorNotified { up_to_seq: 42 },
            EventKind::BaseSynced {
                base_sha: "abc1234".to_string(),
                int_sha: "def5678".to_string(),
            },
            EventKind::GateBaselined {
                base_sha: "abc1234".to_string(),
                entries: vec![BaselineEntry {
                    command: "npm run lint".to_string(),
                    passed: false,
                    exit_code: Some(1),
                    fingerprint: Default::default(),
                    base_sha: "abc1234".to_string(),
                }],
            },
            EventKind::Nudged {
                task: "M1.1".to_string(),
                attempt: 1,
                agent: "claude".to_string(),
                cut: "cut-1".to_string(),
                class: ProviderClass::Overloaded,
                reason: "Provider overloaded".to_string(),
                tries: 1,
            },
            EventKind::Backup {
                task: "M1.1".to_string(),
                attempt: 1,
                from_agent: "claude".to_string(),
                to_agent: "codex".to_string(),
                cut: "cut-5".to_string(),
                class: ProviderClass::RateLimit,
                reason: "Rate limited, still after 5 tries".to_string(),
                worktree: "/tmp/wt".to_string(),
                branch: "feature/m1-events".to_string(),
                started: true,
            },
            EventKind::ProviderGaveUp {
                task: "M1.1".to_string(),
                attempt: 1,
                agent: "codex".to_string(),
                cut: "cut-6".to_string(),
                class: ProviderClass::Other,
                reason: "Provider error".to_string(),
                tries: 1,
            },
            EventKind::RunFinished {
                abandoned: Vec::new(),
            },
        ]
    }

    #[test]
    fn every_kind_round_trips() {
        let kinds = every_kind();
        assert_eq!(kinds.len(), 32, "one case per EventKind variant");

        for (index, kind) in kinds.into_iter().enumerate() {
            let event = Event {
                seq: index as u64 + 1,
                at_ms: 1_700_000_000_000 + index as u64,
                actor: Actor::Engine,
                kind,
            };
            let text = to_json(&event);
            assert!(
                !text.contains('\n'),
                "an event must be a single JSON line: {text}"
            );
            let back: Event = from_json(&text);
            assert_eq!(event, back, "round trip changed the event: {text}");
        }
    }

    #[test]
    fn a_gate_recorded_before_provenance_still_parses() {
        // Old logs predate `source`: the field defaults to None, and a review
        // gate from then is an unproven verdict, not a parse failure.
        let text = r#"{"seq":7,"at_ms":5,"actor":{"type":"engine"},"kind":"gate_passed","task":"M1.1","attempt":1,"gate":"review","findings":[]}"#;
        let event: Event = from_json(text);
        match &event.kind {
            EventKind::GatePassed { source, gate, .. } => {
                assert_eq!(gate, "review");
                assert_eq!(*source, None);
            }
            other => panic!("expected a passed gate, got {other:?}"),
        }
    }

    #[test]
    fn an_event_names_the_task_it_is_about() {
        let spawned = EventKind::Spawned {
            task: "T1".to_string(),
            attempt: 1,
            agent: "claude".to_string(),
            model: None,
            worktree: "/wt".to_string(),
            branch: "b".to_string(),
        };
        assert_eq!(spawned.task(), Some("T1"));
        let escalated = EventKind::Escalated {
            task: Some("T2".to_string()),
            body: "check it".to_string(),
            default_action: "stop".to_string(),
        };
        assert_eq!(escalated.task(), Some("T2"));
        let run_wide = EventKind::Escalated {
            task: None,
            body: "check it".to_string(),
            default_action: "stop".to_string(),
        };
        assert_eq!(run_wide.task(), None);
        let start = EventKind::RunStarted {
            run_id: "r1".to_string(),
            repo: "/repo".to_string(),
            base_branch: "main".to_string(),
            integration_branch: "integration/m1".to_string(),
        };
        assert_eq!(start.task(), None, "a run-wide event is about no task");
    }

    #[test]
    fn kind_is_flattened_into_the_event_object() {
        let event = Event {
            seq: 1,
            at_ms: 5,
            actor: Actor::Agent {
                id: "claude".to_string(),
            },
            kind: EventKind::RunFinished {
                abandoned: Vec::new(),
            },
        };
        let text = to_json(&event);
        assert!(text.contains("\"kind\":\"run_finished\""), "{text}");
        assert!(
            text.contains("\"actor\":{\"type\":\"agent\",\"id\":"),
            "{text}"
        );
        assert!(text.contains("\"seq\":1"), "{text}");
        assert!(text.contains("\"at_ms\":5"), "{text}");
        assert!(
            !text.contains("\"kind\":{"),
            "kind must not be nested: {text}"
        );
    }

    #[test]
    fn actor_variants_round_trip() {
        let actors = [
            Actor::Engine,
            Actor::Orchestrator,
            Actor::Agent {
                id: "claude".to_string(),
            },
            Actor::Human,
        ];
        for actor in actors {
            let text = to_json(&actor);
            let back: Actor = from_json(&text);
            assert_eq!(actor, back, "{text}");
        }
        assert_eq!(to_json(&Actor::Engine), "{\"type\":\"engine\"}");
    }

    #[test]
    fn report_status_and_severity_are_snake_case() {
        assert_eq!(to_json(&ReportStatus::Completed), "\"completed\"");
        assert_eq!(to_json(&ReportStatus::Blocked), "\"blocked\"");
        assert_eq!(to_json(&ReportStatus::Failed), "\"failed\"");
        assert_eq!(to_json(&Severity::Critical), "\"critical\"");
        assert_eq!(to_json(&Severity::Major), "\"major\"");
        assert_eq!(to_json(&Severity::Minor), "\"minor\"");
        assert_eq!(to_json(&Severity::Trivial), "\"trivial\"");
    }

    #[test]
    fn a_report_recorded_before_hand_ins_carried_checks_still_parses() {
        // Old logs predate `checks`: the field defaults to empty rather than
        // failing to parse, and the fold does not re-judge it.
        let text = r#"{"seq":6,"at_ms":5,"actor":{"type":"agent","id":"w1"},"kind":"reported","task":"M1.1","attempt":1,"status":"completed","summary":"done","tests":[],"changed_paths":[],"commit":null,"skills_used":[]}"#;
        let event: Event = from_json(text);
        match &event.kind {
            EventKind::Reported { checks, .. } => assert!(checks.is_empty()),
            other => panic!("expected a report, got {other:?}"),
        }
    }

    #[test]
    fn a_status_line_collapses_whitespace_and_is_capped() {
        assert_eq!(status_line("  writing   the fold \n"), "writing the fold");
        let pasted = "a whole paragraph the agent pasted instead of a status line";
        let capped = status_line(pasted);
        assert_eq!(capped.chars().count(), TASK_STATUS_MAX_CHARS);
        assert!(capped.ends_with('\u{2026}'), "{capped:?}");
    }

    #[test]
    fn an_output_tail_keeps_the_last_lines_and_caps_characters() {
        let mut text = String::new();
        for line in 1..=30 {
            text.push_str(&format!("line {line}\n"));
        }
        let tail = output_tail(&text);
        assert_eq!(tail.lines().count(), CHECK_OUTPUT_LINES);
        assert!(tail.starts_with("line 11"), "{tail:?}");
        assert!(tail.ends_with("line 30"), "{tail:?}");

        let long = "x".repeat(CHECK_OUTPUT_TAIL_CHARS + 500);
        assert_eq!(output_tail(&long).chars().count(), CHECK_OUTPUT_TAIL_CHARS);
        assert!(output_tail("").is_empty());
    }

    #[test]
    fn finding_rejects_unknown_fields() {
        let text = "{\"severity\":\"minor\",\"title\":\"t\",\"location\":null,\
                    \"detail\":\"d\",\"extra\":1}";
        assert!(serde_json::from_str::<Finding>(text).is_err());
    }
}
