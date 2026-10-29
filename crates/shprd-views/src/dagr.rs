//! Projector: run state to a dagr v3 `run.json`.
//!
//! [`project`] is a pure function of a [`RunState`], its event log and a clock
//! reading, so equal inputs always produce an identical document. Field names
//! and enum spellings follow dagr's schema v3 field reference; a document is
//! well formed when `dagr check --strict --json` prints `[]`.
//!
//! Where shprd's run state carries less than dagr's schema expects, this
//! projector states the gap rather than inventing detail:
//!
//! - shprd records no orchestrator pane or agent, so `run.orchestrator` (which
//!   only enables dagr's message view) is omitted.
//! - Attempt times come from the event log alone: [`EventKind::Spawned`]
//!   starts an attempt, its report or gate outcome ends it. An attempt whose
//!   start or settle event is absent is refused rather than stamped with
//!   `generated_at`.
//! - A live attempt carries `locator.agent` (shprd knows which agent it
//!   spawned, not which pane it landed in, so `locator.pane` is omitted) and
//!   `liveness.queued_input: 0` (shprd has no way to feed input to an agent
//!   mid-attempt, so nothing can be queued behind the prompt).
//! - A report or a gate settles an attempt, so a task dagr draws as `review`
//!   has a `done` attempt: dagr's attempt states have no `review`.
//!
//! - A state whose run has not started has no run identity to state, so
//!   [`project`] returns [`ViewError::NoRun`] instead of inventing one.
//!
//! This projection is what `dagr view` draws; it never mutates run state.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};
use shprd_core::event::{Event, EventKind};
use shprd_core::state::{Attempt, RunState, Task, TaskStatus};

/// dagr schema version this projector writes.
const DAGR_VERSION: u32 = 3;

/// How many of the most recent events are projected.
const MAX_EVENTS: usize = 50;

/// Errors from [`project`] and [`write_atomic`].
#[derive(Debug, thiserror::Error)]
pub enum ViewError {
    /// The run has not started, so there is nothing to project and no run
    /// identity to state.
    #[error("the run has not started")]
    NoRun,
    /// Creating the directory, writing the temporary document or moving it
    /// into place failed.
    #[error("writing {path}: {source}")]
    Io {
        /// The path being written.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The event log does not record a fact the dagr contract requires, so the
    /// projection refuses rather than invent or borrow a value.
    #[error("the event log does not record {what}")]
    MissingFact {
        /// The missing fact, named as the log would carry it.
        what: String,
    },
    /// `dagr check --strict` reported findings for the document.
    #[error("dagr check rejected the document: {output}")]
    Check {
        /// The checker's findings, verbatim as JSON.
        output: String,
    },
}

/// Times observed for one attempt, in milliseconds since the Unix epoch.
#[derive(Debug, Default, Clone, Copy)]
struct Times {
    /// When the attempt was spawned.
    start: Option<u64>,
    /// When the attempt reported or was gated.
    end: Option<u64>,
}

/// Projects run state as a dagr v3 run document.
///
/// `generated_at_ms` (milliseconds since the Unix epoch) becomes
/// `generated_at`, dagr's staleness anchor. `events` supplies the run start
/// time, per-attempt start and end times, and the document's `events[]`.
/// Tasks are emitted in id order, so the document is deterministic.
///
/// The run's title is `<repo basename> · <run_id>`: both are recorded facts.
/// `started_at` is the `RunStarted` event's time. No other event stands in for
/// it: an excerpt without `RunStarted` is refused, because a projector that
/// guessed a start time would state a fact shprd never recorded.
///
/// # Errors
///
/// [`ViewError::NoRun`] when the run has not started, and
/// [`ViewError::MissingFact`] when `events` does not record a time the
/// document needs.
pub fn project(
    state: &RunState,
    events: &[Event],
    generated_at_ms: u64,
) -> Result<Value, ViewError> {
    let run = state.run.as_ref().ok_or(ViewError::NoRun)?;
    let generated_at = rfc3339_utc(generated_at_ms);
    let started_at = events
        .iter()
        .find(|event| matches!(event.kind, EventKind::RunStarted { .. }))
        .map(|event| event.at_ms)
        .ok_or_else(|| ViewError::MissingFact {
            what: "a RunStarted event".to_string(),
        })?;
    let run_id = run.run_id.clone();
    let title = format!("{} · {run_id}", repo_basename(&run.repo));
    let times = times_by_attempt(events);

    let mut tasks: Vec<Value> = state
        .tasks
        .values()
        .map(|task| task_value(task, &times))
        .collect::<Result<_, _>>()?;
    tasks.sort_by(|left, right| id_of(left).cmp(id_of(right)));

    Ok(json!({
        "dagr": DAGR_VERSION,
        "run": {
            "id": run_id,
            "title": title,
            "started_at": rfc3339_utc(started_at),
        },
        "generated_at": generated_at,
        "projects": project_values(state),
        "tasks": tasks,
        "events": event_values(events),
    }))
}

/// The last path component of a repository path, or the path itself when it
/// has no components.
fn repo_basename(repo: &str) -> &str {
    Path::new(repo)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(repo)
}

/// The `id` of a projected JSON object, or `""` when it has none.
fn id_of(value: &Value) -> &str {
    value.get("id").and_then(Value::as_str).unwrap_or("")
}

/// Start and end times per `(task, attempt)`, read off the event log.
fn times_by_attempt(events: &[Event]) -> BTreeMap<(&str, u32), Times> {
    let mut times: BTreeMap<(&str, u32), Times> = BTreeMap::new();
    for event in events {
        match &event.kind {
            EventKind::Spawned { task, attempt, .. } => {
                times.entry((task.as_str(), *attempt)).or_default().start = Some(event.at_ms);
            }
            EventKind::Reported { task, attempt, .. }
            | EventKind::GatePassed { task, attempt, .. }
            | EventKind::GateFailed { task, attempt, .. } => {
                times.entry((task.as_str(), *attempt)).or_default().end = Some(event.at_ms);
            }
            _ => {}
        }
    }
    times
}

/// One dagr project per distinct top-level path component of the task scopes.
fn project_values(state: &RunState) -> Vec<Value> {
    let mut ids: BTreeSet<&str> = BTreeSet::new();
    for task in state.tasks.values() {
        for path in &task.scope {
            if let Some(project) = project_of(path) {
                ids.insert(project);
            }
        }
        if let Some(project) = task_project(task) {
            ids.insert(project);
        }
    }
    ids.into_iter()
        .map(|id| json!({ "id": id, "title": id }))
        .collect()
}

/// The project a task is drawn under: the top-level component of its first
/// scope entry. `None` means the run root.
fn task_project(task: &Task) -> Option<&str> {
    task.scope.first().and_then(|path| project_of(path))
}

/// The top-level path component of a repo-relative path.
fn project_of(path: &str) -> Option<&str> {
    path.split('/').find(|part| !part.is_empty())
}

/// Projects one task.
fn task_value(task: &Task, times: &BTreeMap<(&str, u32), Times>) -> Result<Value, ViewError> {
    let latest = task.attempts.len().checked_sub(1);
    let attempts: Vec<Value> = task
        .attempts
        .iter()
        .enumerate()
        .map(|(index, attempt)| attempt_value(task, attempt, Some(index) == latest, times))
        .collect::<Result<_, _>>()?;

    let (state, unblock, note) = task_state(task);

    let mut object = Map::new();
    object.insert("id".to_string(), json!(task.id));
    object.insert("title".to_string(), json!(task.title));
    object.insert("kind".to_string(), json!(task_kind(task)));
    if let Some(project) = task_project(task) {
        object.insert("project".to_string(), json!(project));
    }
    object.insert("state".to_string(), json!(state));
    object.insert("deps".to_string(), json!(task.deps));
    object.insert("criteria".to_string(), json!(task.acceptance.join("; ")));
    if let Some(owner) = task.latest_attempt().map(|attempt| attempt.agent.clone()) {
        object.insert("owner".to_string(), json!(owner));
    }
    if let Some(unblock) = unblock {
        object.insert("unblock".to_string(), json!(unblock));
    }
    if let Some(note) = note {
        object.insert("note".to_string(), json!(note));
    }
    object.insert("attempts".to_string(), Value::Array(attempts));
    Ok(Value::Object(object))
}

/// The task's dagr kind, derived from its tags.
fn task_kind(task: &Task) -> &'static str {
    if task.tags.iter().any(|tag| tag == "review") {
        "review"
    } else if task.tags.iter().any(|tag| tag == "docs") {
        "docs"
    } else {
        "impl"
    }
}

/// The task's dagr state, plus the blocked owner and note dagr wants with it.
fn task_state(task: &Task) -> (&'static str, Option<&'static str>, Option<String>) {
    match task.status {
        TaskStatus::Queued => ("queued", None, None),
        TaskStatus::Working => ("working", None, None),
        TaskStatus::Reported => ("review", None, None),
        TaskStatus::Passed => (
            "review",
            None,
            Some("passed, awaiting integration".to_string()),
        ),
        TaskStatus::Failed => ("blocked", Some("orchestrator"), Some(failed_note(task))),
        TaskStatus::Integrated => ("done", None, None),
    }
}

/// Why a failed task is blocked: the first failed gate's first finding, or
/// "gate failed" when the gate gave no findings.
fn failed_note(task: &Task) -> String {
    task.latest_attempt()
        .and_then(|attempt| attempt.gates.iter().find(|gate| !gate.passed))
        .and_then(|gate| gate.findings.first().map(|finding| finding.title.clone()))
        .unwrap_or_else(|| "gate failed".to_string())
}

/// Projects one attempt.
///
/// Both times are recorded facts: `started_at` is the attempt's `Spawned`
/// event and `ended_at` its settling event. When either is missing from the
/// log the projection refuses; the document's `generated_at` is never
/// borrowed as an attempt time.
///
/// # Errors
///
/// [`ViewError::MissingFact`] when the log has no `Spawned` event for the
/// attempt, or no settling event although the attempt is settled.
fn attempt_value(
    task: &Task,
    attempt: &Attempt,
    latest: bool,
    times: &BTreeMap<(&str, u32), Times>,
) -> Result<Value, ViewError> {
    let state = attempt_state(task, attempt, latest);
    let terminal = matches!(state, "done" | "failed");

    let mut object = Map::new();
    object.insert(
        "id".to_string(),
        json!(format!("{}·a{}", task.id, attempt.n)),
    );
    object.insert("n".to_string(), json!(attempt.n));
    object.insert(
        "cause".to_string(),
        json!({ "type": if attempt.n <= 1 { "initial" } else { "sent_back" } }),
    );
    object.insert("actor".to_string(), json!(attempt.agent));
    if let Some(model) = &attempt.model {
        object.insert("model".to_string(), json!(model));
    }
    object.insert("state".to_string(), json!(state));

    let start = time_ms(times, &task.id, attempt.n, |times| times.start)
        .ok_or_else(|| missing_fact("a Spawned event", task, attempt))?;
    object.insert("started_at".to_string(), json!(rfc3339_utc(start)));
    if state == "working" {
        object.insert("locator".to_string(), json!({ "agent": attempt.agent }));
        object.insert("liveness".to_string(), json!({ "queued_input": 0 }));
    }
    if terminal {
        let end = time_ms(times, &task.id, attempt.n, |times| times.end).ok_or_else(|| {
            missing_fact("a Reported, GatePassed or GateFailed event", task, attempt)
        })?;
        object.insert("ended_at".to_string(), json!(rfc3339_utc(end)));
        object.insert(
            "outcome".to_string(),
            json!({ "result": state, "evidence": evidence(attempt) }),
        );
    }
    Ok(Value::Object(object))
}

/// A [`ViewError::MissingFact`] naming `fact` for `task`'s attempt `attempt`.
fn missing_fact(fact: &str, task: &Task, attempt: &Attempt) -> ViewError {
    ViewError::MissingFact {
        what: format!("{fact} for task {} attempt {}", task.id, attempt.n),
    }
}

/// The time recorded for an attempt under `pick`, in ms since the epoch.
fn time_ms(
    times: &BTreeMap<(&str, u32), Times>,
    task: &str,
    n: u32,
    pick: fn(&Times) -> Option<u64>,
) -> Option<u64> {
    times.get(&(task, n)).and_then(pick)
}

/// The attempt's dagr state.
///
/// A reported or gate-passed attempt has finished its work, so it reads `done`
/// even while dagr draws its task as `review`; only a live attempt reads
/// `working`. Earlier attempts read `failed` when a gate failed or the report
/// was not completed, and `done` otherwise.
fn attempt_state(task: &Task, attempt: &Attempt, latest: bool) -> &'static str {
    let failed = attempt.gates.iter().any(|gate| !gate.passed)
        || attempt
            .report
            .as_ref()
            .is_some_and(|report| report.status != shprd_core::event::ReportStatus::Completed);
    if !latest {
        return if failed { "failed" } else { "done" };
    }
    match task.status {
        TaskStatus::Queued => "queued",
        TaskStatus::Working => {
            if failed {
                "failed"
            } else {
                "working"
            }
        }
        TaskStatus::Reported | TaskStatus::Passed => {
            if failed {
                "failed"
            } else {
                "done"
            }
        }
        TaskStatus::Failed => "failed",
        TaskStatus::Integrated => "done",
    }
}

/// How strongly the engine can vouch for an attempt's outcome.
fn evidence(attempt: &Attempt) -> &'static str {
    if attempt.gates.iter().any(|gate| gate.passed) {
        "verified"
    } else if attempt.report.is_some() {
        "reported"
    } else {
        "asserted"
    }
}

/// The dagr event type an shprd event maps to.
///
/// dagr's set is closed (`attempt_started|attempt_settled|promoted|directive|
/// message_resolved|note`), so only spawns and gate outcomes get a specific
/// type; everything else is a `note` whose detail names the shprd kind.
fn dagr_event_type(kind: &EventKind) -> &'static str {
    match kind {
        EventKind::Spawned { .. } => "attempt_started",
        EventKind::GatePassed { .. } | EventKind::GateFailed { .. } => "attempt_settled",
        _ => "note",
    }
}

/// A short name for an event kind, used as the dagr event's detail.
fn event_label(kind: &EventKind) -> &'static str {
    match kind {
        EventKind::RunStarted { .. } => "run_started",
        EventKind::TaskAdded { .. } => "task_added",
        EventKind::Spawned { .. } => "spawned",
        EventKind::Reported { .. } => "reported",
        EventKind::GatePassed { .. } => "gate_passed",
        EventKind::GateFailed { .. } => "gate_failed",
        EventKind::Integrated { .. } => "integrated",
        EventKind::ReviewRequested { .. } => "review_requested",
        EventKind::Dry { .. } => "dry",
        EventKind::Available { .. } => "available",
        EventKind::HandedOff { .. } => "handed_off",
        EventKind::Escalated { .. } => "escalated",
        EventKind::Override { .. } => "override",
        EventKind::Resource { .. } => "resource",
        EventKind::FullGate { .. } => "full_gate",
        EventKind::PrOpened { .. } => "pr_opened",
        EventKind::PrMerged { .. } => "pr_merged",
        EventKind::Approved { .. } => "approved",
        EventKind::OrchestratorLaunched { .. } => "orchestrator_launched",
        EventKind::WatchLaunched { .. } => "watch_launched",
        EventKind::OrchestratorNotified { .. } => "orchestrator_notified",
        EventKind::RunFinished { .. } => "run_finished",
    }
}

/// The task an event concerns, when it concerns one.
fn event_task(kind: &EventKind) -> Option<&str> {
    match kind {
        EventKind::TaskAdded { task, .. }
        | EventKind::Spawned { task, .. }
        | EventKind::Reported { task, .. }
        | EventKind::GatePassed { task, .. }
        | EventKind::GateFailed { task, .. }
        | EventKind::Integrated { task, .. }
        | EventKind::ReviewRequested { task, .. }
        | EventKind::HandedOff { task, .. } => Some(task),
        EventKind::Escalated { task, .. } => task.as_deref(),
        _ => None,
    }
}

/// The attempt an event concerns, as a dagr attempt id (`T·aN`).
///
/// dagr requires an attempt reference on `attempt_started` and
/// `attempt_settled` events, which is why only those two carry one.
fn event_attempt(kind: &EventKind) -> Option<String> {
    let (task, attempt) = match kind {
        EventKind::Spawned { task, attempt, .. }
        | EventKind::GatePassed { task, attempt, .. }
        | EventKind::GateFailed { task, attempt, .. } => (task, attempt),
        _ => return None,
    };
    Some(format!("{task}·a{attempt}"))
}

/// The last [`MAX_EVENTS`] events, projected in log order.
fn event_values(events: &[Event]) -> Vec<Value> {
    let start = events.len().saturating_sub(MAX_EVENTS);
    events[start..]
        .iter()
        .map(|event| {
            let mut object = Map::new();
            object.insert("at".to_string(), json!(rfc3339_utc(event.at_ms)));
            object.insert("type".to_string(), json!(dagr_event_type(&event.kind)));
            if let Some(task) = event_task(&event.kind) {
                object.insert("task".to_string(), json!(task));
            }
            if let Some(attempt) = event_attempt(&event.kind) {
                object.insert("attempt".to_string(), json!(attempt));
            }
            object.insert("detail".to_string(), json!(event_label(&event.kind)));
            Value::Object(object)
        })
        .collect()
}

/// Formats milliseconds since the Unix epoch as RFC 3339 UTC (`…Z`).
fn rfc3339_utc(ms: u64) -> String {
    let seconds = ms / 1000;
    let days = (seconds / 86_400) as i64;
    let rest = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = (rest / 3600, (rest % 3600) / 60, rest % 60);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Days since the Unix epoch to a proleptic Gregorian `(year, month, day)`.
///
/// Howard Hinnant's `civil_from_days`: integer arithmetic only, no calendar
/// crate, valid for every day this projection can produce.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_position = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_position + 2) / 5 + 1) as u32;
    let month = if month_position < 10 {
        month_position + 3
    } else {
        month_position - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// Writes `doc` to `path` atomically, optionally gating on `dagr check`.
///
/// The document is written as pretty JSON to a temporary file in the same
/// directory (created if missing): `.<file name>.<pid>.<nanos>.tmp`, unique per
/// write, so concurrent writers — parallel agents each syncing their own run
/// through this crate — never share one and never leave a half-written
/// document behind. When `checker` is given it is run as
/// `<checker> check <temporary file> --strict --json`, and the document
/// replaces `path` only if the checker exits 0 with no findings. Every failure
/// after the temporary file exists — a rejected document, a failed rename —
/// removes exactly that file, so a reader never sees a partial or unchecked
/// `run.json` and no temporary file outlives a failed write.
///
/// # Errors
///
/// [`ViewError::Io`] when the directory, the temporary file or the rename
/// fails, and [`ViewError::Check`] when the checker rejects the document or
/// cannot be run.
pub fn write_atomic(doc: &Value, path: &Path, checker: Option<&Path>) -> Result<(), ViewError> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|source| ViewError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let tmp = TempFile::new(path);
    let text = serde_json::to_string_pretty(doc).map_err(|source| ViewError::Io {
        path: tmp.path().to_path_buf(),
        source: std::io::Error::other(source),
    })?;
    fs::write(tmp.path(), text.as_bytes()).map_err(|source| ViewError::Io {
        path: tmp.path().to_path_buf(),
        source,
    })?;

    if let Some(checker) = checker {
        check(checker, tmp.path())?;
    }

    fs::rename(tmp.path(), path).map_err(|source| ViewError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// The temporary file a write goes through, deleted on drop.
///
/// Renaming it into place makes the drop a no-op: the path is already gone.
struct TempFile {
    /// The unique temporary path this write owns.
    path: PathBuf,
}

impl TempFile {
    /// The temporary path for `path`, which is not created yet.
    fn new(path: &Path) -> TempFile {
        TempFile {
            path: temp_path(path),
        }
    }

    /// The temporary path.
    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// `.<file name>.<pid>.<nanos since epoch>.tmp`, in `path`'s directory so the
/// rename stays within one filesystem.
///
/// The name is unique per write: the process id separates concurrent shprd
/// processes, and [`unique_nanos`] makes the timestamp component strictly
/// increase within one process, so two writers in the same process never share
/// a temporary file even when they read the same clock tick.
fn temp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = format!(".{name}.{}.{}.tmp", std::process::id(), unique_nanos());
    match path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        Some(parent) => parent.join(tmp),
        None => PathBuf::from(tmp),
    }
}

/// The largest nanosecond timestamp this process has handed out so far.
static LAST_NANOS: AtomicU64 = AtomicU64::new(0);

/// Nanoseconds since the Unix epoch, at least one more than the previous call's
/// value within this process.
fn unique_nanos() -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or(0);
    let mut previous = LAST_NANOS.load(Ordering::Relaxed);
    loop {
        let candidate = now.max(previous.saturating_add(1));
        match LAST_NANOS.compare_exchange_weak(
            previous,
            candidate,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return candidate,
            Err(actual) => previous = actual,
        }
    }
}

/// Runs `<checker> check <tmp> --strict --json` and requires no findings.
fn check(checker: &Path, tmp: &Path) -> Result<(), ViewError> {
    let output = Command::new(checker)
        .arg("check")
        .arg(tmp)
        .arg("--strict")
        .arg("--json")
        .output()
        .map_err(|source| ViewError::Io {
            path: checker.to_path_buf(),
            source,
        })?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if output.status.success() && is_empty_findings(&text) {
        return Ok(());
    }
    let reported = if text.is_empty() {
        String::from_utf8_lossy(&output.stderr).trim().to_string()
    } else {
        text
    };
    Err(ViewError::Check { output: reported })
}

/// Whether the checker's output is the empty findings list.
fn is_empty_findings(text: &str) -> bool {
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Array(findings)) => findings.is_empty(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shprd_core::event::{Actor, Event, EventKind, Finding, ReportStatus, Severity};
    use std::path::PathBuf;

    /// 2025-03-01T00:00:00Z, the run's start time.
    const T0: u64 = 1_740_787_200_000;
    /// A minute, for spacing events out.
    const MINUTE: u64 = 60_000;

    fn event(seq: u64, at_ms: u64, kind: EventKind) -> Event {
        Event {
            seq,
            at_ms,
            actor: Actor::Engine,
            kind,
        }
    }

    fn task_added(task: &str, title: &str, scope: &[&str], tags: &[&str]) -> EventKind {
        EventKind::TaskAdded {
            task: task.to_string(),
            title: title.to_string(),
            scope: scope.iter().map(|path| path.to_string()).collect(),
            acceptance: vec!["it works".to_string()],
            tags: tags.iter().map(|tag| tag.to_string()).collect(),
            skills: Vec::new(),
            deps: Vec::new(),
        }
    }

    fn spawned(task: &str, attempt: u32, agent: &str) -> EventKind {
        EventKind::Spawned {
            task: task.to_string(),
            attempt,
            agent: agent.to_string(),
            model: Some(format!("{agent}-model")),
            worktree: format!("/wt/{task}"),
            branch: format!("feature/{task}"),
        }
    }

    fn reported(task: &str, attempt: u32, status: ReportStatus, path: &str) -> EventKind {
        EventKind::Reported {
            task: task.to_string(),
            attempt,
            status,
            summary: "done".to_string(),
            tests: Vec::new(),
            changed_paths: vec![path.to_string()],
            commit: None,
            skills_used: Vec::new(),
        }
    }

    fn gate(task: &str, attempt: u32, passed: bool, findings: Vec<Finding>) -> EventKind {
        let gate = "full".to_string();
        if passed {
            EventKind::GatePassed {
                task: task.to_string(),
                attempt,
                gate,
                findings,
                source: None,
            }
        } else {
            EventKind::GateFailed {
                task: task.to_string(),
                attempt,
                gate,
                findings,
                source: None,
            }
        }
    }

    /// A run where one task integrates and the other fails its gate.
    fn scenario() -> Vec<Event> {
        let extra = Actor::Agent {
            id: "w1".to_string(),
        };
        vec![
            event(
                1,
                T0,
                EventKind::RunStarted {
                    run_id: "run-1".to_string(),
                    repo: "/repo".to_string(),
                    base_branch: "main".to_string(),
                    integration_branch: "integration/m1".to_string(),
                },
            ),
            event(
                2,
                T0 + MINUTE,
                task_added("a", "core work", &["crates/x"], &[]),
            ),
            event(
                3,
                T0 + MINUTE,
                task_added("b", "docs work", &["docs/y"], &["docs"]),
            ),
            event(4, T0 + 2 * MINUTE, spawned("a", 1, "w1")),
            event(5, T0 + 2 * MINUTE, spawned("b", 1, "w2")),
            Event {
                actor: extra,
                ..event(
                    6,
                    T0 + 3 * MINUTE,
                    reported("a", 1, ReportStatus::Completed, "crates/x/src/lib.rs"),
                )
            },
            event(
                7,
                T0 + 3 * MINUTE,
                reported("b", 1, ReportStatus::Completed, "docs/y/README.md"),
            ),
            event(8, T0 + 4 * MINUTE, gate("a", 1, true, Vec::new())),
            event(
                9,
                T0 + 5 * MINUTE,
                EventKind::Integrated {
                    task: "a".to_string(),
                    commits: vec!["abc123".to_string()],
                },
            ),
            event(
                10,
                T0 + 6 * MINUTE,
                gate(
                    "b",
                    1,
                    false,
                    vec![Finding {
                        severity: Severity::Major,
                        title: "clippy warning".to_string(),
                        location: Some("docs/y/README.md:1".to_string()),
                        detail: "unwrap used".to_string(),
                    }],
                ),
            ),
        ]
    }

    /// A run with one task an agent is still working on.
    fn working_events() -> Vec<Event> {
        vec![
            event(
                1,
                T0,
                EventKind::RunStarted {
                    run_id: "run-3".to_string(),
                    repo: "/repo".to_string(),
                    base_branch: "main".to_string(),
                    integration_branch: "integration/m1".to_string(),
                },
            ),
            event(
                2,
                T0 + MINUTE,
                task_added("d", "live work", &["crates/w"], &["impl"]),
            ),
            event(3, T0 + 2 * MINUTE, spawned("d", 1, "w4")),
        ]
    }

    /// A run whose single task passed its gate and awaits integration.
    fn passed_events() -> Vec<Event> {
        vec![
            event(
                1,
                T0,
                EventKind::RunStarted {
                    run_id: "run-2".to_string(),
                    repo: "/repo".to_string(),
                    base_branch: "main".to_string(),
                    integration_branch: "integration/m1".to_string(),
                },
            ),
            event(
                2,
                T0 + MINUTE,
                task_added("c", "review the core", &["crates/z"], &["review"]),
            ),
            event(3, T0 + 2 * MINUTE, spawned("c", 1, "w3")),
            event(
                4,
                T0 + 3 * MINUTE,
                reported("c", 1, ReportStatus::Completed, "crates/z/src/lib.rs"),
            ),
            event(5, T0 + 4 * MINUTE, gate("c", 1, true, Vec::new())),
        ]
    }

    fn state(events: &[Event]) -> RunState {
        match RunState::fold(events) {
            Ok(state) => state,
            Err(err) => panic!("fold failed: {err}"),
        }
    }

    /// Projects, panicking on an error: every scenario here has a started run.
    fn project_doc(state: &RunState, events: &[Event], generated_at_ms: u64) -> Value {
        match project(state, events, generated_at_ms) {
            Ok(doc) => doc,
            Err(err) => panic!("projection failed: {err}"),
        }
    }

    fn projected() -> Value {
        let events = scenario();
        project_doc(&state(&events), &events, T0 + 10 * MINUTE)
    }

    #[test]
    fn refuses_to_project_a_run_that_never_started() {
        let state = RunState::default();
        match project(&state, &[], T0 + MINUTE) {
            Err(ViewError::NoRun) => {}
            other => panic!("expected NoRun, got {other:?}"),
        }
    }

    #[test]
    fn refuses_to_project_a_log_without_a_run_started_event() {
        let events = scenario();
        let state = state(&events);
        let excerpt: Vec<Event> = events
            .into_iter()
            .filter(|event| !matches!(event.kind, EventKind::RunStarted { .. }))
            .collect();
        match project(&state, &excerpt, T0 + 10 * MINUTE) {
            Err(ViewError::MissingFact { what }) => {
                assert!(what.contains("RunStarted"), "{what}")
            }
            other => panic!("expected MissingFact, got {other:?}"),
        }
    }

    /// `events` minus the events of task `a` attempt 1 for which `keep` is
    /// false, leaving every other event in place.
    fn without(events: Vec<Event>, keep: fn(&EventKind) -> bool) -> Vec<Event> {
        events
            .into_iter()
            .filter(|event| !is_task_attempt(&event.kind, "a", 1) || keep(&event.kind))
            .collect()
    }

    fn is_task_attempt(kind: &EventKind, want_task: &str, want_attempt: u32) -> bool {
        let named = |task: &String, attempt: &u32| task == want_task && *attempt == want_attempt;
        match kind {
            EventKind::Spawned { task, attempt, .. }
            | EventKind::Reported { task, attempt, .. }
            | EventKind::GatePassed { task, attempt, .. }
            | EventKind::GateFailed { task, attempt, .. } => named(task, attempt),
            _ => false,
        }
    }

    #[test]
    fn refuses_to_project_an_attempt_without_a_spawn_event() {
        let events = scenario();
        let state = state(&events);
        let excerpt = without(events, |kind| !matches!(kind, EventKind::Spawned { .. }));
        match project(&state, &excerpt, T0 + 10 * MINUTE) {
            Err(ViewError::MissingFact { what }) => {
                assert!(what.contains("Spawned"), "{what}");
                assert!(what.contains('a') && what.contains('1'), "{what}");
            }
            other => panic!("expected MissingFact, got {other:?}"),
        }
    }

    #[test]
    fn refuses_to_project_a_settled_attempt_without_a_settling_event() {
        let events = scenario();
        let state = state(&events);
        let excerpt = without(events, |kind| {
            !matches!(
                kind,
                EventKind::Reported { .. }
                    | EventKind::GatePassed { .. }
                    | EventKind::GateFailed { .. }
            )
        });
        match project(&state, &excerpt, T0 + 10 * MINUTE) {
            Err(ViewError::MissingFact { what }) => {
                assert!(what.contains("Reported"), "{what}");
                assert!(what.contains('a') && what.contains('1'), "{what}");
            }
            other => panic!("expected MissingFact, got {other:?}"),
        }
    }

    fn task<'a>(doc: &'a Value, id: &str) -> &'a Value {
        match doc["tasks"]
            .as_array()
            .and_then(|tasks| tasks.iter().find(|task| task["id"] == id))
        {
            Some(task) => task,
            None => panic!("no task {id} in {doc}"),
        }
    }

    #[test]
    fn writes_schema_version_and_run_identity() {
        let doc = projected();
        assert_eq!(doc["dagr"], 3);
        assert_eq!(doc["run"]["id"], "run-1");
        assert_eq!(doc["run"]["title"], "repo · run-1");
        assert_eq!(doc["run"]["started_at"], "2025-03-01T00:00:00Z");
        assert_eq!(doc["generated_at"], "2025-03-01T00:10:00Z");
    }

    #[test]
    fn projects_one_project_per_top_level_scope_component() {
        let doc = projected();
        let ids: Vec<&str> = doc["projects"]
            .as_array()
            .map(|projects| projects.iter().filter_map(|p| p["id"].as_str()).collect())
            .unwrap_or_default();
        assert_eq!(ids, vec!["crates", "docs"]);
        assert_eq!(task(&doc, "a")["project"], "crates");
        assert_eq!(task(&doc, "b")["project"], "docs");
    }

    #[test]
    fn maps_task_states_and_kinds() {
        let doc = projected();
        let a = task(&doc, "a");
        assert_eq!(a["state"], "done");
        assert_eq!(a["kind"], "impl");
        assert_eq!(a["owner"], "w1");
        assert_eq!(a["criteria"], "it works");

        let b = task(&doc, "b");
        assert_eq!(b["state"], "blocked");
        assert_eq!(b["kind"], "docs");
        assert_eq!(b["unblock"], "orchestrator");
        assert_eq!(b["note"], "clippy warning");
        assert_eq!(b["owner"], "w2");
    }

    #[test]
    fn maps_attempts_with_cause_state_and_evidence() {
        let doc = projected();
        let a1 = &task(&doc, "a")["attempts"][0];
        assert_eq!(a1["id"], "a·a1");
        assert_eq!(a1["n"], 1);
        assert_eq!(a1["cause"]["type"], "initial");
        assert_eq!(a1["actor"], "w1");
        assert_eq!(a1["model"], "w1-model");
        assert_eq!(a1["state"], "done");
        assert_eq!(a1["outcome"]["result"], "done");
        assert_eq!(a1["outcome"]["evidence"], "verified");
        assert_eq!(a1["started_at"], "2025-03-01T00:02:00Z");
        assert_eq!(a1["ended_at"], "2025-03-01T00:04:00Z");

        let b1 = &task(&doc, "b")["attempts"][0];
        assert_eq!(b1["state"], "failed");
        assert_eq!(b1["outcome"]["result"], "failed");
        assert_eq!(b1["outcome"]["evidence"], "reported");
    }

    #[test]
    fn projects_events_in_log_order() {
        let doc = projected();
        let events = match doc["events"].as_array() {
            Some(events) => events,
            None => panic!("no events array"),
        };
        assert_eq!(events.len(), 10);
        assert_eq!(events[0]["type"], "note");
        assert_eq!(events[0]["detail"], "run_started");
        assert_eq!(events[3]["type"], "attempt_started");
        assert_eq!(events[3]["attempt"], "a·a1");
        assert_eq!(events[7]["type"], "attempt_settled");
        assert_eq!(events[7]["task"], "a");
        assert_eq!(events[7]["attempt"], "a·a1");
        assert_eq!(events[9]["detail"], "gate_failed");
    }

    #[test]
    fn pull_request_events_project_as_notes() {
        let opened = EventKind::PrOpened {
            number: 7,
            url: "https://example.invalid/pr/7".to_string(),
            head: "shprd/r1/int".to_string(),
        };
        let merged = EventKind::PrMerged {
            number: 7,
            head: "abc1234".to_string(),
        };
        assert_eq!(dagr_event_type(&opened), "note");
        assert_eq!(dagr_event_type(&merged), "note");
        assert_eq!(event_label(&opened), "pr_opened");
        assert_eq!(event_label(&merged), "pr_merged");
    }

    #[test]
    fn truncates_events_to_the_last_fifty() {
        let mut events = scenario();
        let state = state(&events);
        for seq in 11..=70 {
            events.push(event(
                seq,
                T0 + seq * MINUTE,
                EventKind::Dry {
                    agent: format!("w{seq}"),
                    reason: "quota".to_string(),
                    reset_at_ms: None,
                },
            ));
        }
        let doc = project_doc(&state, &events, T0 + 100 * MINUTE);
        let projected = doc["events"].as_array().map(Vec::len).unwrap_or_default();
        assert_eq!(projected, MAX_EVENTS);
    }

    #[test]
    fn is_deterministic() {
        let events = scenario();
        let state = state(&events);
        assert_eq!(
            project_doc(&state, &events, T0),
            project_doc(&state, &events, T0)
        );
    }

    #[test]
    fn formats_known_instants() {
        assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_utc(T0), "2025-03-01T00:00:00Z");
        assert_eq!(rfc3339_utc(1_709_210_096_000), "2024-02-29T12:34:56Z");
    }

    #[test]
    fn maps_a_passed_task_to_review_awaiting_integration() {
        let events = passed_events();
        let doc = project_doc(&state(&events), &events, T0 + 5 * MINUTE);
        let c = task(&doc, "c");
        assert_eq!(c["state"], "review");
        assert_eq!(c["kind"], "review");
        assert_eq!(c["note"], "passed, awaiting integration");
        let c1 = &c["attempts"][0];
        assert_eq!(c1["state"], "done");
        assert_eq!(c1["outcome"]["evidence"], "verified");
    }

    #[test]
    fn maps_a_live_attempt_with_locator_and_liveness() {
        let events = working_events();
        let doc = project_doc(&state(&events), &events, T0 + 3 * MINUTE);
        let d = task(&doc, "d");
        assert_eq!(d["state"], "working");
        let d1 = &d["attempts"][0];
        assert_eq!(d1["state"], "working");
        assert_eq!(d1["locator"]["agent"], "w4");
        assert_eq!(d1["liveness"]["queued_input"], 0);
        assert!(d1.get("outcome").is_none(), "a live attempt has no outcome");
    }

    #[test]
    fn writes_atomically_without_a_checker() {
        let dir = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(err) => panic!("tempdir: {err}"),
        };
        let path = dir.path().join("views/run.json");
        assert!(write_atomic(&projected(), &path, None).is_ok());
        assert!(path.exists());
        assert!(
            tmp_files(dir.path()).is_empty(),
            "{:?}",
            tmp_files(dir.path())
        );
        let text = fs::read_to_string(&path).unwrap_or_default();
        assert!(text.contains("\"dagr\": 3"), "{text}");
    }

    /// Every leftover temporary file in `dir`.
    fn tmp_files(dir: &Path) -> Vec<PathBuf> {
        match fs::read_dir(dir) {
            Ok(entries) => entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("tmp"))
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    #[test]
    fn concurrent_writes_leave_one_parsable_document_and_no_temporary_file() {
        let dir = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(err) => panic!("tempdir: {err}"),
        };
        let path = dir.path().join("run.json");
        let doc = projected();
        let workers: Vec<std::thread::JoinHandle<()>> = (0..2)
            .map(|_| {
                let path = path.clone();
                let doc = doc.clone();
                std::thread::spawn(move || {
                    for round in 0..50 {
                        assert!(
                            write_atomic(&doc, &path, None).is_ok(),
                            "write {round} failed"
                        );
                    }
                })
            })
            .collect();
        for worker in workers {
            assert!(worker.join().is_ok());
        }
        let text = fs::read_to_string(&path).unwrap_or_default();
        assert!(
            serde_json::from_str::<Value>(&text).is_ok(),
            "run.json must parse: {text}"
        );
        assert!(
            tmp_files(dir.path()).is_empty(),
            "{:?}",
            tmp_files(dir.path())
        );
    }

    #[test]
    fn a_failed_rename_leaves_no_temporary_file() {
        let dir = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(err) => panic!("tempdir: {err}"),
        };
        let path = dir.path().join("run.json");
        assert!(fs::create_dir_all(path.join("inside")).is_ok());
        assert!(fs::write(path.join("inside").join("keep"), b"x").is_ok());
        match write_atomic(&projected(), &path, None) {
            Err(ViewError::Io { .. }) => {}
            other => panic!("expected the rename to fail, got {other:?}"),
        }
        assert!(
            tmp_files(dir.path()).is_empty(),
            "a failed write must not leave {:?}",
            tmp_files(dir.path())
        );
    }

    /// A `dagr` stand-in that always reports one finding.
    #[cfg(unix)]
    fn failing_checker(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("fake-dagr");
        let script = "#!/bin/sh\necho '[{\"code\":\"E001\",\"level\":\"error\"}]'\nexit 1\n";
        assert!(fs::write(&path, script).is_ok());
        assert!(fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).is_ok());
        path
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_document_the_checker_refuses() {
        let dir = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(err) => panic!("tempdir: {err}"),
        };
        let path = dir.path().join("run.json");
        let checker = failing_checker(dir.path());
        match write_atomic(&projected(), &path, Some(&checker)) {
            Err(ViewError::Check { output }) => assert!(output.contains("E001"), "{output}"),
            other => panic!("expected a check failure, got {other:?}"),
        }
        assert!(!path.exists(), "a rejected document must not be written");
        assert!(
            tmp_files(dir.path()).is_empty(),
            "the temporary file is removed: {:?}",
            tmp_files(dir.path())
        );
    }

    /// The real thing, when a dagr binary is around: the projection must be a
    /// contract-valid document. Skipped (not failed) when dagr is absent.
    #[test]
    fn validates_with_real_dagr() {
        let bin = std::env::var_os("SHPRD_DAGR_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(
                    "/Users/cryogenix/scratch/mh-verify/repos/herdr-dagr/target/release/dagr",
                )
            });
        if !bin.exists() {
            eprintln!("skipping: no dagr binary at {}", bin.display());
            return;
        }
        let dir = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(err) => panic!("tempdir: {err}"),
        };
        let passed_events = passed_events();
        let working_events = working_events();
        let documents = [
            projected(),
            project_doc(&state(&passed_events), &passed_events, T0 + 5 * MINUTE),
            project_doc(&state(&working_events), &working_events, T0 + 3 * MINUTE),
        ];
        for (index, doc) in documents.iter().enumerate() {
            let path = dir.path().join(format!("run-{index}.json"));
            match write_atomic(doc, &path, Some(&bin)) {
                Ok(()) => {}
                Err(err) => panic!("dagr rejected projection {index}: {err}"),
            }
            assert!(path.exists());
            let text = fs::read_to_string(&path).unwrap_or_default();
            assert!(text.starts_with('{'), "{text}");
        }
    }
}
