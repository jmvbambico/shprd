//! The data-transfer objects the API answers with, and the TypeScript they
//! generate.
//!
//! Everything a client reads or writes goes through one of these types. They
//! are plain JSON both ways, derived from the fold by [`crate::snapshot`], and
//! every one of them is exported to `web/src/gen/` as a TypeScript type (see
//! [`crate::bindings`]) so the web app and a Herdr mod never hand-write the
//! shapes.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use shprd_core::ask::AskKind;
use shprd_core::state::{RunState, TaskStatus};
use ts_rs::TS;

use crate::snapshot;

/// Where a task sits in the run, as the web reads it.
///
/// It is the fold's [`TaskStatus`] with `Serialize` and a TypeScript type on
/// it; nothing derives it here, so the board and the CLI never disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "TaskStatusDto.ts")]
pub enum TaskStatusDto {
    /// Added to the plan, no agent has run it.
    Queued,
    /// An agent is working on it.
    Working,
    /// The agent reported a completed outcome for the current attempt.
    Reported,
    /// Its gates passed.
    Passed,
    /// It failed, or an attempt reported blocked or failed.
    Failed,
    /// Its work landed on the integration branch.
    Integrated,
    /// A human decided the task will not be done.
    Dropped,
}

impl From<TaskStatus> for TaskStatusDto {
    fn from(status: TaskStatus) -> Self {
        match status {
            TaskStatus::Queued => Self::Queued,
            TaskStatus::Working => Self::Working,
            TaskStatus::Reported => Self::Reported,
            TaskStatus::Passed => Self::Passed,
            TaskStatus::Failed => Self::Failed,
            TaskStatus::Integrated => Self::Integrated,
            TaskStatus::Dropped => Self::Dropped,
        }
    }
}

/// The eight words a board row can show, from M8.5's status rules.
///
/// It is [`shprd_views::derived::DerivedStatus`] with a TypeScript type on it:
/// the service derives it from the fold and hands it to the client beside the
/// stored [`TaskStatusDto`], so a card and the CLI's `status` never disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "DerivedStatusDto.ts")]
pub enum DerivedStatusDto {
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
    /// Its agent is at rest and said ready; what is left is the person's.
    Ready,
    /// Its merge is under way, and nothing about it is the person's.
    Merging,
    /// Finished, or it never will be.
    Done,
}

impl From<shprd_views::derived::DerivedStatus> for DerivedStatusDto {
    fn from(status: shprd_views::derived::DerivedStatus) -> Self {
        use shprd_views::derived::DerivedStatus;
        match status {
            DerivedStatus::ToDo => Self::ToDo,
            DerivedStatus::Scheduled => Self::Scheduled,
            DerivedStatus::Queued => Self::Queued,
            DerivedStatus::Working => Self::Working,
            DerivedStatus::NeedsYou => Self::NeedsYou,
            DerivedStatus::Ready => Self::Ready,
            DerivedStatus::Merging => Self::Merging,
            DerivedStatus::Done => Self::Done,
        }
    }
}

/// Why a task reads Needs you.
///
/// It is [`shprd_views::derived::Need`] with a TypeScript type on it: an ask,
/// something shprd found, an agent that went quiet, no agent left, or a
/// provider cut whose retries are used up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "NeedKindDto.ts")]
pub enum NeedKindDto {
    /// Something shprd found that only the person can settle.
    Attention,
    /// The agent asks something: a question, a permission, a plan, a folder to
    /// trust.
    Asks,
    /// The agent went quiet without saying ready.
    Reply,
    /// No agent is left on it.
    Stopped,
    /// Its provider cut it off and the retries are used up.
    Provider,
}

impl From<shprd_views::derived::Need> for NeedKindDto {
    fn from(need: shprd_views::derived::Need) -> Self {
        use shprd_views::derived::Need;
        match need {
            Need::Attention => Self::Attention,
            Need::Asks => Self::Asks,
            Need::Reply => Self::Reply,
            Need::Stopped => Self::Stopped,
            Need::Provider => Self::Provider,
        }
    }
}

/// What an agent reported about an attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "ReportStatusDto.ts")]
pub enum ReportStatusDto {
    /// The attempt finished its scope and is ready to integrate.
    Completed,
    /// The attempt stopped on something it could not resolve.
    Blocked,
    /// The attempt failed.
    Failed,
}

impl From<shprd_core::event::ReportStatus> for ReportStatusDto {
    fn from(status: shprd_core::event::ReportStatus) -> Self {
        match status {
            shprd_core::event::ReportStatus::Completed => Self::Completed,
            shprd_core::event::ReportStatus::Blocked => Self::Blocked,
            shprd_core::event::ReportStatus::Failed => Self::Failed,
        }
    }
}

/// How bad a gate finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "SeverityDto.ts")]
pub enum SeverityDto {
    /// Must be fixed before integration.
    Critical,
    /// Should be fixed before integration.
    Major,
    /// Worth fixing, not blocking.
    Minor,
    /// Cosmetic.
    Trivial,
}

impl From<shprd_core::event::Severity> for SeverityDto {
    fn from(severity: shprd_core::event::Severity) -> Self {
        match severity {
            shprd_core::event::Severity::Critical => Self::Critical,
            shprd_core::event::Severity::Major => Self::Major,
            shprd_core::event::Severity::Minor => Self::Minor,
            shprd_core::event::Severity::Trivial => Self::Trivial,
        }
    }
}

/// The run's identity, as the header of a board shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "RunDto.ts")]
pub struct RunDto {
    /// Run identifier.
    pub id: String,
    /// Repository the run works on.
    pub repo: String,
    /// Branch the run was cut from.
    pub base_branch: String,
    /// Branch landed work is integrated on.
    pub integration_branch: String,
}

/// One step of the last advance pass that failed.
///
/// It is the smallest thing that lets a board say what went wrong instead of
/// showing an empty board: which task, which step, and the error the step was
/// given.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "FailedStepDto.ts")]
pub struct FailedStepDto {
    /// The task the step was about; empty when it was the pass's own failure.
    pub task: String,
    /// What the step was, e.g. `start`, `review`, `land`, or `pass`.
    pub step: String,
    /// Why it failed, one line.
    pub error: String,
}

/// What the last advance pass did, as a board reads it.
///
/// A pass that fails a step goes on with the others and leaves the run where it
/// was for that task, so the failure would otherwise be invisible: it is in the
/// service's own output at best, and `/api/state` shows the tasks as they were.
/// This is that pass, mirrored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "LastTickDto.ts")]
pub struct LastTickDto {
    /// When the pass finished, ms since the Unix epoch.
    pub at_ms: u64,
    /// How many steps it ran, the failed ones included.
    pub steps: usize,
    /// Every step that failed, in plan order.
    pub failed: Vec<FailedStepDto>,
}

impl From<&crate::pass::LastPass> for LastTickDto {
    fn from(last: &crate::pass::LastPass) -> Self {
        Self {
            at_ms: last.at_ms,
            steps: last.steps,
            failed: last
                .failed
                .iter()
                .map(|step| FailedStepDto {
                    task: step.task.clone(),
                    step: step.step.clone(),
                    error: step.error.clone(),
                })
                .collect(),
        }
    }
}

/// One task of the run plan, with its progress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "TaskDto.ts")]
pub struct TaskDto {
    /// Task id.
    pub id: String,
    /// Human-readable title.
    pub title: String,
    /// Where the task is in its lifecycle, as the log stores it.
    pub status: TaskStatusDto,
    /// The word a person sees for it, from M8.5's status rules, with why it
    /// needs them when it does.
    pub derived: TaskDerivedDto,
    /// Repo-relative paths the task may change.
    pub scope: Vec<String>,
    /// What must be true when the task is done.
    pub acceptance: Vec<String>,
    /// Free-form tags.
    pub tags: Vec<String>,
    /// Skills the agent is told to load.
    pub skills: Vec<String>,
    /// Tasks that must be integrated first.
    pub deps: Vec<String>,
    /// Why a human dropped it, once one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub dropped_reason: Option<String>,
    /// The last progress line an agent recorded, with how far it said it had
    /// got.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub last_status: Option<TaskProgressDto>,
    /// Attempts made so far, in order.
    pub attempts: Vec<AttemptDto>,
    /// The task's own pull request, once a landing opened one.
    ///
    /// `null` is the normal answer for a repository with no forge: a local
    /// landing merges the branch into its base and opens nothing, so a card
    /// that drew a pull request row for it would be inventing one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pr: Option<TaskPrDto>,
}

/// Whether a task's pull request has been merged.
///
/// It is the fold's [`shprd_core::state::PrInfo::merged`] as a word: a board row
/// has room for two states and a person decides by them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "PrStateDto.ts")]
pub enum PrStateDto {
    /// The forge has not merged it, and landing still waits on it.
    Open,
    /// It was merged: the task's work is in its base branch.
    Merged,
}

/// A task's own pull request, as its card reads it.
///
/// The per-task workflow gives every task its own pull request into the run's
/// base branch, so this is where a person goes to read, approve or merge the
/// work. A task that landed locally has none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "TaskPrDto.ts")]
pub struct TaskPrDto {
    /// The forge's number for it, as a person writes it (`#12`).
    pub number: u64,
    /// The URL to open: the forge's page for it.
    pub url: String,
    /// The branch it proposes to merge.
    pub head: String,
    /// Whether it is open or merged.
    pub state: PrStateDto,
    /// Whether a human approved it in shprd, at the head the log recorded.
    pub approved: bool,
}

/// One task as a person sees it: its derived word, and why it needs them.
///
/// The service derives it from the fold (M8.5's [`shprd_views::derived`]
/// rules), an open ask included, so a card shows **Needs you** while an agent
/// waits. It reads the log alone: a task whose agent went quiet without saying
/// ready reads as Working here rather than claiming an agent stopped, since the
/// snapshot handler has no terminal to look at (the service's own tick does).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "TaskDerivedDto.ts")]
pub struct TaskDerivedDto {
    /// The eight-word status a row shows.
    pub status: DerivedStatusDto,
    /// Why it needs the person, when it does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub need: Option<NeedKindDto>,
    /// The row's second line, at most 40 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reason: Option<String>,
}

/// The last progress line an agent recorded for a task.
///
/// It is what a board row shows under the task's name — "halfway, 50%" — and it
/// never moves the task's derived status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "TaskProgressDto.ts")]
pub struct TaskProgressDto {
    /// The short line the agent reported, trimmed and capped.
    pub message: String,
    /// How far along it said the task was, from 0 to 100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub progress: Option<u8>,
    /// The log sequence that recorded it.
    pub seq: u64,
}

/// One attempt at a task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "AttemptDto.ts")]
pub struct AttemptDto {
    /// Attempt number, starting at 1.
    pub n: u32,
    /// The agent that ran it.
    pub agent: String,
    /// Model the agent ran on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub model: Option<String>,
    /// Worktree the agent worked in.
    pub worktree: String,
    /// Branch the agent committed on.
    pub branch: String,
    /// The agent's report, once it has reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub report: Option<ReportDto>,
    /// Gates run against this attempt, in order.
    pub gates: Vec<GateDto>,
    /// Reviews dispatched for this attempt, in order.
    pub reviews: Vec<ReviewDto>,
}

/// What an agent reported about an attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "ReportDto.ts")]
pub struct ReportDto {
    /// Outcome of the attempt.
    pub status: ReportStatusDto,
    /// What the agent says it did.
    pub summary: String,
    /// Test commands the agent ran, with their result.
    pub tests: Vec<String>,
    /// The gates the agent ran for the hand-in, with their result.
    ///
    /// This is what the accept card's Checks section reads: the commands the
    /// worker really ran and what each said, which is the half of a hand-in a
    /// person can check for themselves. Empty for a report recorded before
    /// hand-ins carried checks.
    pub checks: Vec<CheckDto>,
    /// Normalized paths the agent changed.
    pub changed_paths: Vec<String>,
    /// Commit the agent made, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub commit: Option<String>,
    /// Skills the agent used.
    pub skills_used: Vec<String>,
}

/// One gate the agent ran for its hand-in.
///
/// It is the worker's own record of a check — the gate's name, the command, its
/// exit code and the tail of what it printed — so a person reading the accept
/// card sees what ran rather than a claim that it passed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "CheckDto.ts")]
pub struct CheckDto {
    /// The gate's name, e.g. `tests` or `lint`.
    pub gate: String,
    /// The command, as the worker ran it.
    pub command: String,
    /// Its exit code, or `null` when a timeout killed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub exit_code: Option<i32>,
    /// The last lines it printed, capped.
    pub output_tail: String,
}

/// The outcome of one gate against one attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "GateDto.ts")]
pub struct GateDto {
    /// Gate name.
    pub gate: String,
    /// Whether it passed.
    pub passed: bool,
    /// What it found.
    pub findings: Vec<FindingDto>,
    /// True when the gate carried a reviewer's provenance.
    ///
    /// A review gate without it was recorded by something other than the
    /// reviewer's packet, which is not an independent review.
    pub proven: bool,
}

/// One finding a gate reported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "FindingDto.ts")]
pub struct FindingDto {
    /// How bad it is.
    pub severity: SeverityDto,
    /// One-line title.
    pub title: String,
    /// Where it is, e.g. `crates/shprd-core/src/log.rs:42`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub location: Option<String>,
    /// The full detail, including what to do about it.
    pub detail: String,
}

/// A review handed to a reviewer for an attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "ReviewDto.ts")]
pub struct ReviewDto {
    /// The agent string the review was handed to.
    pub reviewer: String,
    /// True when the reviewer shares the author's vendor.
    pub degraded: bool,
}

/// What the run knows about one agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "AgentDto.ts")]
pub struct AgentDto {
    /// The agent's id.
    pub id: String,
    /// Whether the agent is on cooldown.
    pub dry: bool,
    /// Why it is dry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub dry_reason: Option<String>,
    /// When it is expected back, ms since the Unix epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub dry_until_ms: Option<u64>,
}

/// What an agent is waiting for, as a card shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "AskKindDto.ts")]
pub enum AskKindDto {
    /// The agent asks a question and waits for an answer.
    Question,
    /// The agent asks permission to run a tool.
    Permission,
    /// The agent offers a plan and waits for approval.
    Plan,
}

impl From<AskKind> for AskKindDto {
    fn from(kind: AskKind) -> Self {
        match kind {
            AskKind::Question => Self::Question,
            AskKind::Permission => Self::Permission,
            AskKind::Plan => Self::Plan,
        }
    }
}

/// One ask still waiting for the person.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "AskDto.ts")]
pub struct AskDto {
    /// Its id, unique inside the task.
    pub id: String,
    /// The task it belongs to.
    pub task: String,
    /// Question, permission or plan.
    pub kind: AskKindDto,
    /// What the person is asked.
    pub text: String,
    /// The choices a card offers, when it has any.
    pub options: Vec<String>,
    /// When it was raised, ms since the Unix epoch.
    pub created_at_ms: u64,
    /// When it stops being open, ms since the Unix epoch, when it does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub expires_at_ms: Option<u64>,
}

/// What `GET /api/asks` answers with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "AskListDto.ts")]
pub struct AskListDto {
    /// Every open ask, oldest first.
    pub asks: Vec<AskDto>,
}

/// The body of `POST /api/asks/answer`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "AnswerAsk.ts")]
pub struct AnswerAsk {
    /// The task the ask belongs to.
    pub task: String,
    /// The ask to answer.
    pub ask: String,
    /// What the person answered: `allow`/`deny`, `approve`/`reject`, or their
    /// own words for a question.
    pub answer: String,
}

/// What answering an ask answers with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "AskReply.ts")]
pub struct AskReply {
    /// True; a refusal answers with [`ErrorBody`] and a failure status.
    pub ok: bool,
    /// The ask as it now stands, with its answer.
    pub ask: AskDto,
}

/// Something waiting on a human.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "NeedDto.ts")]
pub struct NeedDto {
    /// The task it concerns, when it is not run-wide.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub task: Option<String>,
    /// What needs deciding.
    pub body: String,
    /// The action taken if nobody answers.
    pub default_action: String,
    /// The log sequence that raised it.
    pub seq: u64,
}

/// The whole board: one snapshot, rebuilt from the fold on every request.
///
/// It is the small part of the run a UI draws — the run's identity, every task
/// with its attempts, the agents' dry marks, and what is waiting on a human. A
/// client that needs the whole log asks [`ChangeDto`] instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "SnapshotDto.ts")]
pub struct SnapshotDto {
    /// The log sequence this snapshot was folded up to.
    pub seq: u64,
    /// The run's identity, once it has started; `null` before `shprd run start`.
    #[serde(default)]
    pub run: Option<RunDto>,
    /// What the last advance pass did, once one has run against this
    /// repository. A pass that failed a step is what a board shows as
    /// "something went wrong", rather than an unchanged board.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub last_tick: Option<LastTickDto>,
    /// What the person can do about a board that cannot advance on its own —
    /// no run started, for one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub hint: Option<String>,
    /// Every task, in id order.
    pub tasks: Vec<TaskDto>,
    /// Every agent the run has seen.
    pub agents: Vec<AgentDto>,
    /// Everything waiting on a human, oldest first.
    pub needs: Vec<NeedDto>,
    /// Every ask still waiting for the person, oldest first.
    pub asks: Vec<AskDto>,
    /// Whether the run has finished.
    pub finished: bool,
}

impl SnapshotDto {
    /// Folds `state` into the snapshot a board draws, as of `now_ms`.
    pub fn from_state(state: &RunState, now_ms: u64) -> Self {
        snapshot::snapshot(state, now_ms)
    }
}

/// What a client learns from `GET /api/health`, before it authenticates.
///
/// `id` is the handshake: a client that read `service.json`'s `port` checks it
/// here before it sends the token, so it never sends a secret to whatever else
/// happens to hold that port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "HealthDto.ts")]
pub struct HealthDto {
    /// Always true; a failing service does not answer at all.
    pub ok: bool,
    /// The service's identity, as `service.json` records it.
    pub id: String,
    /// The service's version.
    pub version: String,
}

/// The body of `POST /api/tasks`.
///
/// It is the person's own `shprd agent task add`, minus the id: a form on a
/// board asks for a title, a scope and an acceptance line, and the service names
/// the task from the title (see [`crate::tasks::task_id`]). A caller that does
/// have an id — a script porting a plan — sends it and the service keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "NewTaskDto.ts")]
pub struct NewTaskDto {
    /// The id to store it under, when the caller has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub id: Option<String>,
    /// One-line title.
    pub title: String,
    /// Repo-relative paths the task may change.
    #[serde(default)]
    pub scope: Vec<String>,
    /// What must be true when the task is done.
    #[serde(default)]
    pub acceptance: Vec<String>,
    /// Free-form tags.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Skills the agent is told to load.
    #[serde(default)]
    pub skills: Vec<String>,
    /// Tasks that must be integrated first.
    #[serde(default)]
    pub deps: Vec<String>,
}

/// One file a task's branch changed, with its line counts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "FileChangeDto.ts")]
pub struct FileChangeDto {
    /// The path, repo-relative.
    pub path: String,
    /// Lines added.
    pub added: u64,
    /// Lines deleted.
    pub deleted: u64,
    /// True when git counted no lines: the file is binary.
    pub binary: bool,
}

/// What `GET /api/tasks/{id}/changes` answers with.
///
/// It is the task's branch against its base, read from the merge base, so the
/// numbers are the work the task did rather than everything its base moved
/// since. The accept card draws it as a count line and a list of paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "TaskChangesDto.ts")]
pub struct TaskChangesDto {
    /// The task the changes belong to.
    pub task: String,
    /// The branch its work lands on, as the run records it.
    pub base: String,
    /// The branch the task's attempt committed on.
    pub branch: String,
    /// The commit both branches meet at: what the diff is taken from.
    pub merge_base: String,
    /// Every changed file, in git's order.
    pub files: Vec<FileChangeDto>,
    /// Lines added across every file.
    pub added: u64,
    /// Lines deleted across every file.
    pub deleted: u64,
}

/// One event's footprint, as change notifications carry it.
///
/// The change channel carries only what tells a client whether it must refetch
/// the snapshot: the position, the kind, and the task if the event named one.
/// A client that needs the payload reads the snapshot or the log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "EventNoticeDto.ts")]
pub struct EventNoticeDto {
    /// The event's position in the log.
    pub seq: u64,
    /// When it was appended, ms since the Unix epoch.
    pub at_ms: u64,
    /// The event's kind, e.g. `task_added`.
    pub kind: String,
    /// The task it named, when it named one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub task: Option<String>,
}

/// What `GET /api/changes` and the WebSocket answer with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "ChangeDto.ts")]
pub struct ChangeDto {
    /// The log's last sequence after the change.
    pub seq: u64,
    /// Every event after the sequence the client asked from.
    pub events: Vec<EventNoticeDto>,
}

/// One action a client may post to `POST /api/action`.
///
/// It is one closed union: a kind the service does not know is refused, so a
/// client can never reach past the verbs the CLI already has. The handler does
/// not implement any of them itself — `actions` does, against the same fold and
/// log the CLI writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export_to = "Action.ts")]
pub enum Action {
    /// Add a task to the run plan, as `shprd agent task add` does.
    ///
    /// Spelled `task.add`; [`Action::from_value`] accepts `task_add` too,
    /// because a JSON client and the Rust type would otherwise disagree about
    /// one separator.
    #[serde(rename = "task.add")]
    TaskAdd {
        /// Task id, e.g. `M1.6`.
        task: String,
        /// One-line title.
        title: String,
        /// Repo-relative path the task may change.
        #[serde(default)]
        scope: Vec<String>,
        /// What must be true when the task is done.
        #[serde(default)]
        acceptance: Vec<String>,
        /// Free-form tag.
        #[serde(default)]
        tags: Vec<String>,
        /// Skill the agent must load.
        #[serde(default)]
        skills: Vec<String>,
        /// Task that must land first.
        #[serde(default)]
        deps: Vec<String>,
    },
    /// Record a short progress line for a task, as `shprd task status` does.
    ///
    /// It appends a `TaskStatus` event under the log's lock — several workers
    /// may record one at a time — and the line is trimmed and capped by the
    /// writer, so a pasted agent message never becomes an event. Spelled
    /// `task.status`, with `task_status` accepted.
    #[serde(rename = "task.status")]
    TaskStatus {
        /// The task.
        task: String,
        /// One short line saying where the work stands; capped at 40
        /// characters.
        msg: String,
        /// How far along the task is, from 0 to 100.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        progress: Option<u8>,
    },
    /// Start a task's first attempt, as `shprd agent spawn --task` does.
    ///
    /// The spawn belongs to the binary — it writes a brief, makes a worktree
    /// and opens a tab — so the service calls that same path. The transition
    /// rules decide whether a start is allowed: only a queued or failed task
    /// with its dependencies landed may have one.
    #[serde(rename = "task.start")]
    TaskStart {
        /// The task to start.
        task: String,
    },
    /// Start another attempt at a task, as a send-back does.
    ///
    /// It is the same spawn path a start uses, so the new attempt's brief
    /// carries the failed review's findings exactly as the engine's own
    /// send-back does. The rules refuse it while the task is not in a state
    /// where a new attempt is the answer.
    #[serde(rename = "task.retry")]
    TaskRetry {
        /// The task to try again.
        task: String,
    },
    /// Stop the agent working on a task, by closing its space on the host.
    ///
    /// The space is closed first — a host that refuses refuses the whole stop,
    /// and nothing is written — and then the attempt is recorded as stopped. The
    /// task reads *Needs you · Agent stopped* and `task.retry` starts a new
    /// attempt, which is Shepherdr's rule for a task whose agent went away (M7a
    /// §5). A stop of a task nobody started, or a second stop of the same
    /// attempt, is refused with its reason. A service started without a host
    /// refuses it.
    #[serde(rename = "task.stop")]
    TaskStop {
        /// The task whose agent should stop.
        task: String,
    },
    /// Drop a task, as `shprd drop` does.
    ///
    /// It is the person's decision that the task will not be done, and it is
    /// what unblocks a run held by work nobody will finish. The reason is
    /// required: it is what the run's pull request lists under `## Dropped`.
    /// Nothing is deleted — the branch stays — and the rules refuse a task that
    /// already landed or was already dropped.
    #[serde(rename = "task.drop")]
    TaskDrop {
        /// The task to drop.
        task: String,
        /// Why the person decided against it.
        reason: String,
    },
    /// Accept a task's work at the head its branch is at, as `shprd approve`
    /// does for a task.
    ///
    /// A solo maintainer cannot approve their own pull request on a forge, so
    /// the approval is recorded in the log, pinned to the commit it was given
    /// for. It only means something where `tasks.merge` is `ask`: with `checks`
    /// or `auto` the task lands without an approval, and the service refuses
    /// this action rather than recording a decision nothing waits on.
    #[serde(rename = "task.accept")]
    TaskAccept {
        /// The task to accept.
        task: String,
    },
}

impl Action {
    /// Every kind this envelope accepts, so a client that names another one is
    /// told which ones exist instead of getting a parse error.
    ///
    /// Keep it beside the enum: `every_action_kind_is_listed` fails when a
    /// variant is added without its kind.
    pub const KINDS: &'static [&'static str] = &[
        "task.add",
        "task_add",
        "task.status",
        "task_status",
        "task.start",
        "task_start",
        "task.retry",
        "task_retry",
        "task.stop",
        "task_stop",
        "task.drop",
        "task_drop",
        "task.accept",
        "task_accept",
    ];

    /// The action's kind, as the envelope spells it.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::TaskAdd { .. } => "task.add",
            Self::TaskStatus { .. } => "task.status",
            Self::TaskStart { .. } => "task.start",
            Self::TaskRetry { .. } => "task.retry",
            Self::TaskStop { .. } => "task.stop",
            Self::TaskDrop { .. } => "task.drop",
            Self::TaskAccept { .. } => "task.accept",
        }
    }

    /// Parses one envelope, accepting the snake_case spelling of its kind too.
    ///
    /// The brief spells the actions `task.add` and `task.status`; a Rust client
    /// shaped by the type's variants writes `task_add`. Both are the same
    /// action, and serde has no alias that its own derive and ts-rs both read
    /// without warning, so the one substitution lives here.
    pub fn from_value(mut value: serde_json::Value) -> Result<Self, serde_json::Error> {
        if let Some(kind) = value.get("kind").and_then(serde_json::Value::as_str) {
            let canonical = match kind {
                "task_add" => Some("task.add"),
                "task_status" => Some("task.status"),
                "task_start" => Some("task.start"),
                "task_retry" => Some("task.retry"),
                "task_stop" => Some("task.stop"),
                "task_drop" => Some("task.drop"),
                "task_accept" => Some("task.accept"),
                _ => None,
            };
            if let (Some(canonical), Some(object)) = (canonical, value.as_object_mut()) {
                object.insert(
                    "kind".to_string(),
                    serde_json::Value::String(canonical.to_string()),
                );
            }
        }
        serde_json::from_value(value)
    }
}

/// What a successful action answers with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "ActionReply.ts")]
pub struct ActionReply {
    /// True; a refused action answers with [`ErrorBody`] and a failure status.
    pub ok: bool,
    /// The log's sequence after the action.
    pub seq: u64,
    /// The task the action was about, folded as it is now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub task: Option<TaskDto>,
}

/// Why a request was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "ErrorCode.ts")]
pub enum ErrorCode {
    /// The token was missing or wrong.
    Unauthorized,
    /// The request came from another origin.
    CrossOrigin,
    /// A write did not carry `x-shprd: 1`.
    MissingHeader,
    /// The body was not a well-formed request.
    BadRequest,
    /// The action's kind is not one this service knows.
    UnknownAction,
    /// The task the request named does not exist.
    UnknownTask,
    /// The path is not one this service serves.
    NotFound,
    /// A transition rule refused the action; nothing was written.
    Refused,
    /// The event log could not be read or written.
    Ledger,
    /// Anything else.
    Internal,
}

/// The body of every refusal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "ErrorBody.ts")]
pub struct ErrorBody {
    /// Why the request was refused.
    pub code: ErrorCode,
    /// One line a human can act on.
    pub message: String,
}

/// One message the WebSocket pushes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export_to = "WsMessage.ts")]
pub enum WsMessage {
    /// The first message on every socket: who this is, and where the log is.
    Hello {
        /// The service's identity.
        id: String,
        /// The service's version.
        version: String,
        /// The log's last sequence at connect time.
        seq: u64,
    },
    /// New events were appended since the last message.
    Change {
        /// The change itself, the same shape `GET /api/changes` answers with.
        change: ChangeDto,
    },
    /// An answer to a client's `{"type":"ping"}`.
    Pong {
        /// The value the ping carried, echoed back.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        at: Option<i64>,
    },
}

/// One message a client may send on the WebSocket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export_to = "ClientMessage.ts")]
pub enum ClientMessage {
    /// Ask for a [`WsMessage::Pong`]; also a liveness check.
    Ping {
        /// A value to echo back.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        at: Option<i64>,
    },
}

/// The layer one settings value came from, nearest first.
///
/// The same names `shprd settings show` prints: `task`, `repo_entry`, `you`,
/// `legacy`, `repo_file` and `built_in`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "SettingsSourceDto.ts")]
pub enum SettingsSourceDto {
    /// The task's own record.
    Task,
    /// The person's entry for this repository.
    RepoEntry,
    /// The person's own defaults.
    You,
    /// The keys the person's pre-M9 `config.toml` still sets.
    Legacy,
    /// The repository's committed `.shprd/settings.toml`.
    RepoFile,
    /// The values shprd ships.
    BuiltIn,
}

/// One settings key, its value and the layer it came from.
///
/// `value` is a string, a number, a boolean or a list of strings, as the key
/// takes; both it and `source` are `null` when nothing sets the key, because a
/// settings page has to draw an unset field too.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "SettingsValueDto.ts")]
pub struct SettingsValueDto {
    /// The key's canonical dotted name, e.g. `roles.worker.agent`.
    pub key: String,
    /// The value, or `null` when nothing sets it.
    #[ts(type = "string | number | boolean | Array<string> | null")]
    pub value: Option<Value>,
    /// The layer that set it, or `null`.
    pub source: Option<SettingsSourceDto>,
}

/// Every setting the service resolved, with where each value came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "SettingsDto.ts")]
pub struct SettingsDto {
    /// The name this repository's entry is keyed by.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub repo: Option<String>,
    /// Every key, in the order a settings page draws them.
    pub values: Vec<SettingsValueDto>,
    /// What is wrong with the resolved roles, if anything.
    ///
    /// Reading never refuses: a pair of settings that would not work is
    /// reported here so the page can show it. A write of one is refused.
    pub problems: Vec<String>,
    /// Why the roles could not be checked at all, when they could not.
    ///
    /// An empty `problems` list and an unchecked one must not look alike: a
    /// service with no harness data directory cannot tell whether a reviewer is
    /// another vendor, so it says so here rather than showing a clean bill of
    /// health.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub not_checked: Option<String>,
}

/// One write to the settings.
///
/// A body without `value` removes the key, so it inherits again. `for_repo`
/// writes this repository's entry instead of the person's own defaults. A
/// repository's committed file is never written through the API: changing it is
/// a commit in that repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "SettingsPutDto.ts")]
pub struct SettingsPutDto {
    /// The key to write, e.g. `tasks.merge`.
    pub key: String,
    /// The value to store, or absent to remove the key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[ts(type = "string | number | boolean | Array<string>")]
    pub value: Option<Value>,
    /// Write this repository's entry instead of the person's own defaults.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub for_repo: bool,
}

/// One prompt, as the file on this machine holds it.
///
/// `state` says whether the file is exactly the words this build ships
/// (`default`) or the person's own (`edited`); `path` is the file an editor
/// opens, and `text` is what an agent is given.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "PromptDto.ts")]
pub struct PromptDto {
    /// The id the prompt is stored under.
    pub id: String,
    /// The file it lives in.
    pub path: String,
    /// Its words.
    pub text: String,
    /// Whether the file is the shipped default or the person's own words.
    pub state: PromptStateDto,
}

/// Whether one prompt file is the shipped default or the person's own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "PromptStateDto.ts")]
pub enum PromptStateDto {
    /// The file holds exactly the words this build ships.
    Default,
    /// The file holds the person's own words, or an older build's untracked
    /// default.
    Edited,
}

/// One prompt write: the words to store over the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "PromptPutDto.ts")]
pub struct PromptPutDto {
    /// The role's standing words, exactly as they should be stored.
    pub text: String,
}

/// One prompt in the templates page's list: its identity, its file and whether
/// it is the person's own.
///
/// The words themselves are not in the list — four prompts' standing text is a
/// lot to send for a page that shows one at a time — so a client reads the one
/// it opens with [`PromptDto`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "PromptSummaryDto.ts")]
pub struct PromptSummaryDto {
    /// The id it is stored under.
    pub id: String,
    /// One line about the role, when this build ships this prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub about: Option<String>,
    /// The file it lives in.
    pub path: String,
    /// Whether the file is the shipped default or the person's own words, when
    /// it could be read at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub state: Option<PromptStateDto>,
    /// Whether this build ships a prompt under this id.
    pub shipped: bool,
    /// Why the file cannot be used, when it cannot: a person's own file is never
    /// dropped from the list without a word about why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub problem: Option<String>,
}

/// What `GET /api/prompts` answers with: every prompt this machine has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "PromptListDto.ts")]
pub struct PromptListDto {
    /// The prompts, the shipped ones first, then the person's own by id.
    pub prompts: Vec<PromptSummaryDto>,
}

/// One program a role may run, with what its catalog and harness table say it
/// takes.
///
/// It is the picker's data, served by the service for the same reason the
/// settings are: the mod has no data directory of its own, and a picker that
/// guessed at a program's models would offer ones it cannot run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "ProgramDto.ts")]
pub struct ProgramDto {
    /// The harness id, e.g. `claude`.
    pub id: String,
    /// The words a person sees, e.g. Claude Code.
    pub name: String,
    /// The vendor that ships the CLI, e.g. `anthropic`.
    pub vendor: String,
    /// Whether the CLI runs other vendors' models, so a model names its own.
    pub multi_model: bool,
    /// Whether a model must name its provider too, as `pi` requires.
    pub needs_provider: bool,
    /// The models its catalog lists, in the catalog's order.
    pub models: Vec<ModelChoiceDto>,
    /// Where the models came from, so a picker can say why a list is short.
    pub models_source: ModelsSourceDto,
    /// Why there are no models, when there are none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub models_note: Option<String>,
    /// The effort levels the program takes overall, lowest first.
    pub efforts: Vec<String>,
    /// Whether the program has a fast (higher-priority) mode.
    pub fast: bool,
    /// The access modes it offers, in the order its harness table lists them.
    pub permissions: Vec<PermissionDto>,
    /// The access mode a role gets when it names none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub default_permission: Option<String>,
}

/// One model a program's catalog lists, with the levels it takes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "ModelChoiceDto.ts")]
pub struct ModelChoiceDto {
    /// The name the program's model flag takes.
    pub id: String,
    /// The words a person sees; the id when the catalog names no other.
    pub name: String,
    /// The effort levels this model takes, lowest first.
    pub efforts: Vec<String>,
    /// The level it starts at when a role names none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub default_effort: Option<String>,
    /// Whether the model has a speed tier of its own.
    pub fast: bool,
}

/// One access mode a program offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "PermissionDto.ts")]
pub struct PermissionDto {
    /// The program's own word for the mode, e.g. `auto`.
    pub id: String,
    /// The words a person sees, e.g. `Auto`.
    pub label: String,
}

/// Where a program's model list came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "ModelsSourceDto.ts")]
pub enum ModelsSourceDto {
    /// A static list in the shipped data, e.g. `data/models/claude.toml`.
    Static,
    /// The program's own list command, read for this answer.
    Command,
    /// The program names no model list at all.
    None,
}

/// What `GET /api/roster` answers with: the programs a role may run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "RosterDto.ts")]
pub struct RosterDto {
    /// One entry per `data/harness/*.toml`.
    pub programs: Vec<ProgramDto>,
    /// Why there are no programs, when there are none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub note: Option<String>,
}

/// What one write did, and the settings as they now resolve.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "SettingsWriteDto.ts")]
pub struct SettingsWriteDto {
    /// What happened to the key.
    pub outcome: SettingsOutcomeDto,
    /// The settings, resolved after the write.
    pub settings: SettingsDto,
}

/// One message of a space's conversation, as a panel draws it.
///
/// It is [`shprd_agents::transcript::Message`] with a JSON spelling: the role is
/// the word `user` or `assistant`, and each tool the agent ran is beside its
/// words rather than a row of its own, because a person reads what the agent
/// said and only glances at what it ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "ChatMessageDto.ts")]
pub struct ChatMessageDto {
    /// Who said it: `user` or `assistant`.
    pub role: String,
    /// The words, with a program's own markup removed. Empty for a message that
    /// is tool calls only, which is what Codex records for a call.
    pub text: String,
    /// The tools the agent ran as part of this reply, in order.
    pub tools: Vec<ChatToolDto>,
    /// When the program recorded it, in ms since the epoch, when it did.
    pub at_ms: Option<i64>,
}

/// One tool call, summarised to what a chat row shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "ChatToolDto.ts")]
pub struct ChatToolDto {
    /// The program's own name for the tool, e.g. `Bash`.
    pub name: String,
    /// One line of what it ran or read, already cut short.
    pub summary: String,
}

/// One page of a space's conversation, and where to ask from next.
///
/// `cursor` is opaque to a client: it names the session and how far into it the
/// page reached, so a client that polls with the last cursor it was given sees
/// each message once. A cursor from another session (the agent started a new
/// one) reads that session from its start, which is what a panel wants to show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "ChatDto.ts")]
pub struct ChatDto {
    /// The space asked about: `coordinator`, or a task's id.
    pub space: String,
    /// The program whose record was read, when the service knows it.
    pub program: Option<String>,
    /// The session that was read, when there was one.
    pub session: Option<String>,
    /// The messages after the client's cursor, oldest first.
    pub messages: Vec<ChatMessageDto>,
    /// Where to ask from next.
    pub cursor: String,
    /// Whether the record has more to give: ask again at once with `cursor`.
    pub more: bool,
    /// Why the page is empty, in words a panel shows, when it is.
    pub note: Option<String>,
}

/// One chat line to type into a space's agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "ChatSendDto.ts")]
pub struct ChatSendDto {
    /// The space to type into: `coordinator`, or a task's id.
    pub space: String,
    /// The words to send, as one turn of the person's input.
    pub text: String,
}

/// What one send did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "ChatSendReply.ts")]
pub struct ChatSendReply {
    /// The space the line went to.
    pub space: String,
    /// True when the line was typed into the agent's pane. A refusal is an
    /// error answer instead, so a `true` here means the host took it.
    pub sent: bool,
}

/// What one write did to the key it named.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "SettingsOutcomeDto.ts")]
pub enum SettingsOutcomeDto {
    /// The value is now in the scope's layer.
    Stored,
    /// The value equals what the scope would inherit, so nothing was stored.
    EqualToInherited,
    /// The key was removed from the scope's layer.
    Removed,
    /// The key was not in the scope's layer, so there was nothing to remove.
    WasUnset,
}

impl From<shprd_core::settings::Source> for SettingsSourceDto {
    fn from(source: shprd_core::settings::Source) -> Self {
        match source {
            shprd_core::settings::Source::Task => Self::Task,
            shprd_core::settings::Source::RepoEntry => Self::RepoEntry,
            shprd_core::settings::Source::You => Self::You,
            shprd_core::settings::Source::Legacy => Self::Legacy,
            shprd_core::settings::Source::RepoFile => Self::RepoFile,
            shprd_core::settings::Source::BuiltIn => Self::BuiltIn,
        }
    }
}

impl From<shprd_core::settings::WriteOutcome> for SettingsOutcomeDto {
    fn from(outcome: shprd_core::settings::WriteOutcome) -> Self {
        match outcome {
            shprd_core::settings::WriteOutcome::Stored => Self::Stored,
            shprd_core::settings::WriteOutcome::EqualToInherited => Self::EqualToInherited,
            shprd_core::settings::WriteOutcome::Removed => Self::Removed,
            shprd_core::settings::WriteOutcome::WasUnset => Self::WasUnset,
        }
    }
}

impl SettingsDto {
    /// Project resolved settings for the API.
    pub fn from_resolved(
        resolved: &shprd_core::settings::Resolved,
        repo: Option<&str>,
        problems: Vec<String>,
        not_checked: Option<String>,
    ) -> Self {
        Self {
            repo: repo.map(str::to_owned),
            values: resolved
                .rows()
                .into_iter()
                .map(|row| SettingsValueDto {
                    key: row.key,
                    value: row.value.as_ref().map(shprd_core::settings::Value::to_json),
                    source: row.source.map(SettingsSourceDto::from),
                })
                .collect(),
            problems,
            not_checked,
        }
    }
}

/// Every type the API exports, as `(file name, file contents)`.
///
/// Order does not matter: the generator writes one file per type and takes each
/// type's own imports from its declaration.
pub(crate) fn exports() -> Result<Vec<(String, String)>, ts_rs::ExportError> {
    fn one<T: TS + 'static>() -> Result<(String, String), ts_rs::ExportError> {
        let cfg = crate::bindings::config();
        let name = match T::output_path().and_then(|path| path.to_str().map(str::to_owned)) {
            Some(name) => name,
            None => format!("{}.ts", T::ident(&cfg)),
        };
        Ok((name, T::export_to_string(&cfg)?))
    }
    Ok(vec![
        one::<TaskStatusDto>()?,
        one::<DerivedStatusDto>()?,
        one::<NeedKindDto>()?,
        one::<TaskDerivedDto>()?,
        one::<ReportStatusDto>()?,
        one::<SeverityDto>()?,
        one::<RunDto>()?,
        one::<FailedStepDto>()?,
        one::<LastTickDto>()?,
        one::<TaskDto>()?,
        one::<TaskPrDto>()?,
        one::<PrStateDto>()?,
        one::<NewTaskDto>()?,
        one::<FileChangeDto>()?,
        one::<TaskChangesDto>()?,
        one::<TaskProgressDto>()?,
        one::<AttemptDto>()?,
        one::<ReportDto>()?,
        one::<CheckDto>()?,
        one::<GateDto>()?,
        one::<FindingDto>()?,
        one::<ReviewDto>()?,
        one::<AgentDto>()?,
        one::<NeedDto>()?,
        one::<AskKindDto>()?,
        one::<AskDto>()?,
        one::<AskListDto>()?,
        one::<AnswerAsk>()?,
        one::<AskReply>()?,
        one::<ChatMessageDto>()?,
        one::<ChatToolDto>()?,
        one::<ChatDto>()?,
        one::<ChatSendDto>()?,
        one::<ChatSendReply>()?,
        one::<SnapshotDto>()?,
        one::<HealthDto>()?,
        one::<EventNoticeDto>()?,
        one::<ChangeDto>()?,
        one::<Action>()?,
        one::<ActionReply>()?,
        one::<ErrorCode>()?,
        one::<ErrorBody>()?,
        one::<WsMessage>()?,
        one::<ClientMessage>()?,
        one::<SettingsSourceDto>()?,
        one::<SettingsValueDto>()?,
        one::<SettingsDto>()?,
        one::<SettingsPutDto>()?,
        one::<SettingsWriteDto>()?,
        one::<PromptDto>()?,
        one::<PromptStateDto>()?,
        one::<PromptPutDto>()?,
        one::<PromptSummaryDto>()?,
        one::<PromptListDto>()?,
        one::<SettingsOutcomeDto>()?,
        one::<RosterDto>()?,
        one::<ProgramDto>()?,
        one::<ModelChoiceDto>()?,
        one::<PermissionDto>()?,
        one::<ModelsSourceDto>()?,
        one::<GraphDto>()?,
        one::<DagrDto>()?,
        one::<DagrOriginDto>()?,
    ])
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn task_add() -> Action {
        Action::TaskAdd {
            task: "T1".to_string(),
            title: "t".to_string(),
            scope: vec!["crates/a".to_string()],
            acceptance: vec!["works".to_string()],
            tags: Vec::new(),
            skills: Vec::new(),
            deps: Vec::new(),
        }
    }

    #[test]
    fn both_spellings_of_a_kind_are_the_same_action() {
        let dotted = serde_json::json!({
            "kind": "task.add", "task": "T1", "title": "t",
            "scope": ["crates/a"], "acceptance": ["works"],
        });
        let snake = serde_json::json!({
            "kind": "task_add", "task": "T1", "title": "t",
            "scope": ["crates/a"], "acceptance": ["works"],
        });
        assert_eq!(Action::from_value(dotted).expect("dotted"), task_add());
        assert_eq!(Action::from_value(snake).expect("snake"), task_add());
        assert_eq!(task_add().kind(), "task.add");
    }

    #[test]
    fn every_action_kind_is_listed() {
        for action in [
            task_add(),
            Action::TaskStatus {
                task: "T1".to_string(),
                msg: "halfway".to_string(),
                progress: Some(50),
            },
            Action::TaskStart {
                task: "T1".to_string(),
            },
            Action::TaskRetry {
                task: "T1".to_string(),
            },
            Action::TaskStop {
                task: "T1".to_string(),
            },
            Action::TaskDrop {
                task: "T1".to_string(),
                reason: "not needed".to_string(),
            },
            Action::TaskAccept {
                task: "T1".to_string(),
            },
        ] {
            assert!(
                Action::KINDS.contains(&action.kind()),
                "{} is a kind nobody lists",
                action.kind()
            );
            // And the snake spelling of it round-trips too.
            let snake = action.kind().replace('.', "_");
            assert!(Action::KINDS.contains(&snake.as_str()), "{snake}");
        }
    }

    #[test]
    fn the_persons_task_verbs_parse() {
        let drop = Action::from_value(serde_json::json!({
            "kind": "task_drop", "task": "T1", "reason": "not needed",
        }))
        .expect("a drop");
        assert_eq!(
            drop,
            Action::TaskDrop {
                task: "T1".to_string(),
                reason: "not needed".to_string(),
            }
        );
        assert_eq!(drop.kind(), "task.drop");
        let accept = Action::from_value(serde_json::json!({
            "kind": "task.accept", "task": "T1",
        }))
        .expect("an accept");
        assert_eq!(accept.kind(), "task.accept");
        let start = Action::from_value(serde_json::json!({
            "kind": "task.start", "task": "T1",
        }))
        .expect("a start");
        assert_eq!(start.kind(), "task.start");
    }

    #[test]
    fn a_task_status_parses_with_and_without_progress() {
        let with = Action::from_value(serde_json::json!({
            "kind": "task_status", "task": "T1", "msg": "halfway", "progress": 50,
        }))
        .expect("a status with progress");
        assert_eq!(
            with,
            Action::TaskStatus {
                task: "T1".to_string(),
                msg: "halfway".to_string(),
                progress: Some(50),
            }
        );
        let without = Action::from_value(serde_json::json!({
            "kind": "task.status", "task": "T1", "msg": "halfway",
        }))
        .expect("a status without progress");
        assert_eq!(
            without,
            Action::TaskStatus {
                task: "T1".to_string(),
                msg: "halfway".to_string(),
                progress: None,
            }
        );
    }

    #[test]
    fn an_empty_scope_and_acceptance_default_to_nothing() {
        let action = Action::from_value(serde_json::json!({
            "kind": "task.add", "task": "T1", "title": "t",
        }))
        .expect("the optional lists default");
        match action {
            Action::TaskAdd {
                scope, acceptance, ..
            } => {
                assert!(scope.is_empty());
                assert!(acceptance.is_empty());
            }
            other => panic!("expected a task.add, got {other:?}"),
        }
    }
}

/// What a graph block needs to draw the run, from `GET /api/graph`.
///
/// The block itself is a terminal running dagr (`dagr view <run_json>`), so this
/// answer carries the command to run and where the program came from, not the
/// document: dagr reads the file and follows it. `message` is set only when
/// there is no program, and it names the setting a person has to fix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "GraphDto.ts")]
pub struct GraphDto {
    /// The repository the service serves.
    pub repo: String,
    /// Where the projection lives: `<repo>/.dagr/run.json`.
    pub run_json: String,
    /// The run's identity, or `null` before `shprd run start`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub run: Option<String>,
    /// The program that draws it, or `null` when there is none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub dagr: Option<DagrDto>,
    /// The command the block runs, `[dagr, view, run_json]`; empty without a
    /// program.
    pub argv: Vec<String>,
    /// The settings key that names the program, so a message can say which key
    /// to change instead of "configure it somewhere".
    pub setting: String,
    /// Why there is no program, in one line, when there is none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub message: Option<String>,
}

/// The program that draws the graph, and the probe that found it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "DagrDto.ts")]
pub struct DagrDto {
    /// The executable to run.
    pub path: String,
    /// Which probe named it: the setting, `PATH` or the herdr plugin folder.
    pub origin: DagrOriginDto,
}

/// Where the graph program was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "DagrOriginDto.ts")]
pub enum DagrOriginDto {
    /// `$SHPRD_DAGR_BIN`.
    Env,
    /// The `graph.dagr` setting.
    Setting,
    /// A `dagr` on `PATH`.
    Path,
    /// The `herdr-dagr` herdr plugin's checkout.
    Plugin,
}

impl From<crate::graph::Origin> for DagrOriginDto {
    fn from(origin: crate::graph::Origin) -> Self {
        match origin {
            crate::graph::Origin::Env => Self::Env,
            crate::graph::Origin::Setting => Self::Setting,
            crate::graph::Origin::Path => Self::Path,
            crate::graph::Origin::Plugin => Self::Plugin,
        }
    }
}

impl GraphDto {
    /// The answer for `repo`, whose run is `run` (when one has started), with
    /// what the binary found when it looked for dagr.
    pub fn new(
        repo: &std::path::Path,
        run: Option<&str>,
        dagr: Option<&crate::graph::Dagr>,
    ) -> Self {
        let run_json = crate::graph::run_json(repo);
        let (details, argv, message) = match dagr {
            Some(crate::graph::Dagr::Found(located)) => (
                Some(DagrDto {
                    path: located.path.display().to_string(),
                    origin: DagrOriginDto::from(located.origin),
                }),
                located.command(&run_json),
                None,
            ),
            Some(crate::graph::Dagr::Missing(missing)) => {
                (None, Vec::new(), Some(missing.message()))
            }
            // A service nobody told reads nothing of the machine, so it says so
            // rather than pretending the machine has no dagr.
            None => (
                None,
                Vec::new(),
                Some(format!(
                    "this service was not told where dagr is: set `{}` and restart it",
                    crate::graph::SETTING
                )),
            ),
        };
        Self {
            repo: repo.display().to_string(),
            run_json: run_json.display().to_string(),
            run: run.map(str::to_owned),
            dagr: details,
            argv,
            setting: crate::graph::SETTING.to_owned(),
            message,
        }
    }
}
