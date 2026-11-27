//! News to the coordinator: what a pass tells a space's coordinator, and when.
//!
//! The rules — which change is worth a line, the settle window, one line per
//! change, one message per space, and the re-check before sending — are
//! [`shprd_views::news`]'s, and pure. This module is the part with effects: it
//! keeps the news' memory on disk, so a change is told once across passes and
//! restarts, and it types a batch into the coordinator's pane through the mux
//! seam ([`Coordinator`]).
//!
//! Ported from Shepherdr.
//!
//! - Source: <https://github.com/eliasstravik/shepherdr>
//! - Path: `mods/tasks/bin/herdr_task/news.py` (`news_pass()`, `news_send()`,
//!   and the `.news.json` memory), as the M7a study describes it in §8.3
//! - Commit: `a3f327a` (tag `v3.1.34`)
//! - License: MIT — Copyright (c) 2026 Elias Stravik
//!
//! Shepherdr writes its coordinator's news into the Assistant's terminal through
//! `hc send`, one message per space per pass, with `.news.json` holding what
//! was told. shprd does the same through the multiplexer seam, into the pane its
//! [`Coordinator`] names: the pane the run recorded when it launched its own
//! coordinator, or — in the M10 flow, where a person starts the coordinator
//! themselves — the repository space's coordinator tab, found by its label. A
//! pane shprd cannot name is never typed into, so the lines wait for a later
//! pass.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use shprd_core::event::{Event, FallbackAction};
use shprd_core::log::Clock;
use shprd_core::state::{RunState, Task, TaskStatus};
use shprd_views::derived::{Derived, TaskSignal};
use shprd_views::news::{Mark, News, Reading, Snapshot};

/// The multiplexer seam news is delivered through.
///
/// It is a trait rather than the [`shprd_mux::Mux`] itself for the same reason
/// the tick's facts are: a test drives the whole pass with a fake that records
/// what was typed, and the binary wires the real host.
///
/// [`shprd_mux::Mux`]: https://docs.rs/shprd-mux
pub trait Coordinator: Send + Sync {
    /// The pane this run's news may be typed into, when one is reachable.
    ///
    /// The default is the pane the run recorded when it launched its own
    /// coordinator ([`OrchestratorLaunched`]). The binary's adapter answers
    /// more than that: when a person starts the coordinator themselves in the
    /// repository's own space, it finds that space by its label through the
    /// host and uses the pane `shprd serve` recorded there. `None` means nobody
    /// can be told right now, so the lines stay queued for a later pass.
    ///
    /// It is asked once on a pass that has something to tell, before anything
    /// is typed: a coordinator that was closed since the last pass is found
    /// again, or the news waits.
    ///
    /// [`OrchestratorLaunched`]: shprd_core::event::EventKind::OrchestratorLaunched
    fn pane(&self, state: &RunState) -> Option<String> {
        state
            .orchestrator
            .as_ref()
            .map(|orchestrator| orchestrator.pane.clone())
    }

    /// Type `text` into `pane` as one turn of input, exactly as
    /// [`Mux::prompt`](https://docs.rs/shprd-mux) does for an agent.
    ///
    /// `Err` is a host that could not be written to; the line is kept and tried
    /// again on the next pass.
    fn tell(&self, pane: &str, text: &str) -> Result<(), String>;
}

/// Everything a pass's news needs besides the run itself.
pub struct NewsDesk {
    /// The seam to the coordinator's pane.
    pub coordinator: Arc<dyn Coordinator>,
    /// How long a change must hold before it is told, ms
    /// (`tasks.news_settle_secs`).
    pub settle_ms: u64,
    /// The clock the settle window and the stall rule read.
    pub clock: Arc<dyn Clock + Send + Sync>,
}

impl std::fmt::Debug for NewsDesk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewsDesk")
            .field("settle_ms", &self.settle_ms)
            .finish_non_exhaustive()
    }
}

/// What one pass told a coordinator, for the tick's report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewsSent {
    /// The space whose coordinator was told.
    pub space: String,
    /// The lines it was told, oldest first.
    pub lines: Vec<String>,
}

/// Where a repository keeps what the news already told, beside its log.
pub fn memory_path(repo: &Path) -> PathBuf {
    repo.join(".shprd").join("news.json")
}

/// Runs one news pass: notes how every task reads, tells what settled, and
/// keeps the memory.
///
/// Delivery is one message per space per pass, into the pane the pass's
/// [`Coordinator::pane`] names. A run with no reachable coordinator has nobody
/// to tell, so the lines stay queued (and are dropped after six hours, as
/// Shepherdr drops them) rather than going into a pane shprd cannot name.
///
/// The memory is held under its own file lock for the whole pass, as Shepherdr
/// runs its news inside the tick lock: two processes on one repository — a
/// `shprd serve` and a hand-run `shprd tick` — must not both read it, tell the
/// same batch, and write back their own view of it.
pub fn tell(desk: &NewsDesk, repo: &Path, state: &RunState, snapshot: &Snapshot) -> Vec<NewsSent> {
    let now_ms = desk.clock.now_ms();
    let path = memory_path(repo);
    let _lock = match lock_memory(&path) {
        Ok(lock) => Some(lock),
        // A memory that cannot be locked is not a reason to keep the news from
        // its coordinator: the pass carries on, unlocked, and says so.
        Err(err) => {
            tracing::warn!(%err, "the news' memory could not be locked");
            None
        }
    };
    let mut news = read(&path);
    // What a previous run said about a reused task id says nothing about this
    // run's tasks.
    let run = state.run.as_ref().map(|run| run.run_id.as_str());
    let started_over = news.note_run(run.unwrap_or_default(), state.last_seq);
    let before = news.clone();
    news.observe(now_ms, snapshot);
    let batches = news.settled(now_ms, desk.settle_ms, snapshot);
    let mut sent = Vec::new();
    // Only a pass with something to tell asks the host where the coordinator
    // is: an idle pass should not make host calls for news that does not exist.
    if !batches.is_empty()
        && let Some(pane) = desk.coordinator.pane(state)
    {
        for batch in batches {
            match desk.coordinator.tell(&pane, &batch.text()) {
                Ok(()) => {
                    let report = NewsSent {
                        space: batch.space.clone(),
                        lines: batch.items.iter().map(|item| item.text.clone()).collect(),
                    };
                    news.told(&batch);
                    sent.push(report);
                }
                // A host that refuses the write is not a reason to forget the
                // news: it is kept, and tried again on the next pass.
                Err(reason) => {
                    tracing::warn!(space = %batch.space, %reason, "news could not be told");
                }
            }
        }
    }
    // Written even when nothing could be told: what was seen must not come back
    // as new, and a line still waiting must still be there next pass.
    if (started_over || news != before)
        && let Err(err) = write(&path, &news)
    {
        tracing::warn!(%err, "could not keep the news' memory");
    }
    sent
}

/// Takes the news memory's exclusive lock, creating its lock file.
///
/// It is an advisory `flock` beside the memory, so the OS drops it when the
/// process exits, however it exits (`shprd`'s run lock takes its own the same
/// way).
fn lock_memory(path: &Path) -> Result<std::fs::File, String> {
    let dir = path
        .parent()
        .ok_or_else(|| format!("{} has no directory", path.display()))?;
    std::fs::create_dir_all(dir).map_err(|err| format!("creating {}: {err}", dir.display()))?;
    let lock = path.with_extension("json.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock)
        .map_err(|err| format!("opening {}: {err}", lock.display()))?;
    file.lock()
        .map_err(|err| format!("locking {}: {err}", lock.display()))?;
    Ok(file)
}

/// How every task reads as news, from the fold and the facts it keeps.
///
/// `rows` is what the status rules derived for each task, with the facts they
/// derived it from; `events` is the run's log; `live` names the tasks whose
/// agent the host reports running right now, and `now_ms` is the pass's clock.
///
/// An agent that is at work is something happening, so a task with one is never
/// quiet: a worker running a long build or test command appends no event for
/// half an hour and is not stalled. Shepherdr's `activity()` counts the agent's
/// own status timestamp and its session log's mtime for the same reason
/// (`news.py:41-53`); the tick has the host's word, which is that the agent is
/// still there.
pub fn readings(
    state: &RunState,
    repo: &Path,
    rows: &BTreeMap<String, (Derived, TaskSignal)>,
    events: &[Event],
    live: &BTreeSet<String>,
    now_ms: u64,
) -> Snapshot {
    let space = space_of(state, repo);
    let activity = activity(events);
    let mut snapshot = Snapshot::new();
    for (id, task) in &state.tasks {
        let Some((row, signal)) = rows.get(id) else {
            continue;
        };
        let mut last = activity.get(id).copied().unwrap_or(0);
        if live.contains(id) {
            last = last.max(now_ms);
        }
        let mut one = reading(&space, task, row, signal, last);
        one.stop_confirmed = signal.stopped || exit_recorded(events, id);
        snapshot.insert(id.clone(), one);
    }
    snapshot
}

use shprd_watch::machine::EXIT_REASON;

/// Whether the newest thing the log says about `task` is its worker's process
/// exit: a stop the watch saw, not one the news only inferred from a terminal
/// the host did not list. Anything the task did after (a status line, a
/// hand-in, a new spawn) means it was not the end.
fn exit_recorded(events: &[Event], task: &str) -> bool {
    events
        .iter()
        .rev()
        .find(|event| event.kind.task() == Some(task))
        .is_some_and(|event| {
            matches!(
                &event.kind,
                shprd_core::event::EventKind::HandedOff { reason, .. }
                    if reason.as_deref() == Some(EXIT_REASON)
            )
        })
}

/// One task as news reads it.
fn reading(
    space: &str,
    task: &Task,
    row: &Derived,
    signal: &TaskSignal,
    last_activity_ms: u64,
) -> Reading {
    Reading {
        space: space.to_owned(),
        title: task.title.clone(),
        status: row.status,
        need: row.needs,
        reason: row.reason.clone(),
        summary: hand_in(task),
        failed: signal.failure.clone(),
        outcome: outcome(task),
        review_failed: review_failed(task),
        backup: backup(task),
        last_activity_ms,
        attempt: task.latest_attempt().map(|attempt| attempt.n),
        stop_confirmed: false,
    }
}

/// The space a pass serves: the repository, by the directory name that also
/// names its workspace.
fn space_of(state: &RunState, repo: &Path) -> String {
    let run_repo = state
        .run
        .as_ref()
        .map(|run| PathBuf::from(&run.repo))
        .unwrap_or_else(|| repo.to_path_buf());
    match run_repo.file_name() {
        Some(name) => name.to_string_lossy().into_owned(),
        None => run_repo.display().to_string(),
    }
}

/// When each task's newest attempt was spawned, ms, from the log.
///
/// The status rules read it as the start grace (`STARTING_MS`): an agent that
/// was started a moment ago is at work even before the host has noticed its
/// terminal, which is what keeps a task that just started from being told as
/// one whose agent stopped.
pub fn started_at(events: &[Event]) -> BTreeMap<String, u64> {
    let mut at: BTreeMap<String, u64> = BTreeMap::new();
    for event in events {
        if let shprd_core::event::EventKind::Spawned { task, .. } = &event.kind {
            let seen = at.entry(task.clone()).or_insert(event.at_ms);
            *seen = (*seen).max(event.at_ms);
        }
    }
    at
}

/// When the log last said anything about each task, ms.
///
/// Shepherdr reads a task's agent status, its own status line and its session
/// log's mtime; shprd's fold keeps none of those (and the service does not read
/// transcripts), so the newest event that names the task is the log's half of
/// "something happened". [`readings`] adds the other half the pass already
/// knows: an agent the host reports running is at work now.
pub fn activity(events: &[Event]) -> BTreeMap<String, u64> {
    let mut at: BTreeMap<String, u64> = BTreeMap::new();
    for event in events {
        let Some(task) = event.kind.task().filter(|task| !task.is_empty()) else {
            continue;
        };
        let seen = at.entry(task.to_owned()).or_insert(event.at_ms);
        *seen = (*seen).max(event.at_ms);
    }
    at
}

/// What its agent said it did, for a hand-in (`news.py`'s ready line).
fn hand_in(task: &Task) -> Option<String> {
    let summary = task
        .latest_attempt()
        .and_then(|attempt| attempt.report.as_ref())
        .map(|report| report.summary.trim().to_owned())?;
    (!summary.is_empty()).then_some(summary)
}

/// How a done task ended (Shepherdr's "merged", "nothing to merge" or
/// "accepted"; shprd drops a task instead of accepting it).
fn outcome(task: &Task) -> Option<String> {
    match task.status {
        TaskStatus::Dropped => Some(String::from("dropped")),
        TaskStatus::Integrated if task.pr.as_ref().is_some_and(|pr| pr.merged) => {
            Some(String::from("merged"))
        }
        TaskStatus::Integrated if task.nothing_to_merge.is_some() => {
            Some(String::from("nothing to merge"))
        }
        _ => None,
    }
}

/// The newest review round of the attempt in hand, when it failed.
///
/// A review verdict is a gate with a [`ReviewSource`], so the newest of those
/// is the review's latest word: passed, or failed with its first finding. Only
/// the latest attempt's verdicts count — a new attempt is the worker's answer to
/// an old review.
///
/// [`ReviewSource`]: shprd_core::event::ReviewSource
fn review_failed(task: &Task) -> Option<Mark> {
    // Only the attempt in hand: a review of an older attempt says nothing about
    // this one, and a new attempt is the worker's answer to it.
    let attempt = task.latest_attempt()?;
    let gate = attempt
        .gates
        .iter()
        .rev()
        .find(|gate| gate.source.is_some())?;
    if gate.passed {
        return None;
    }
    let words = match gate.findings.first() {
        Some(finding) => finding.title.clone(),
        None => format!("{} failed", gate.gate),
    };
    Some(Mark {
        id: gate.seq.to_string(),
        words,
    })
}

/// The backup program that took over, when one did.
fn backup(task: &Task) -> Option<Mark> {
    let record =
        task.fallbacks.iter().rev().find(|record| {
            record.action == FallbackAction::Backup && record.started == Some(true)
        })?;
    let to = record.to_agent.as_deref().unwrap_or("a backup");
    Some(Mark {
        id: record.seq.to_string(),
        words: format!(
            "{to} took over from {}: {}",
            record.agent,
            record.reason.trim()
        ),
    })
}

/// The news' memory, as this machine last wrote it.
fn read(path: &Path) -> News {
    match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(news) => news,
            Err(err) => {
                tracing::warn!(%err, path = %path.display(), "the news' memory does not parse: starting fresh");
                News::new()
            }
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => News::new(),
        Err(err) => {
            tracing::warn!(%err, path = %path.display(), "the news' memory cannot be read: starting fresh");
            News::new()
        }
    }
}

/// Writes the news' memory, atomically, so a crash cannot leave half of it.
fn write(path: &Path, news: &News) -> Result<(), String> {
    let dir = path
        .parent()
        .ok_or_else(|| format!("{} has no directory", path.display()))?;
    std::fs::create_dir_all(dir).map_err(|err| format!("creating {}: {err}", dir.display()))?;
    let body = serde_json::to_vec(news).map_err(|err| format!("encoding the news: {err}"))?;
    let temp = path.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&temp, body).map_err(|err| format!("writing {}: {err}", temp.display()))?;
    std::fs::rename(&temp, path)
        .map_err(|err| format!("moving {} to {}: {err}", temp.display(), path.display()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use shprd_core::event::{
        Actor, CheckReport, EventKind, Finding, LandOutcome, ProviderClass, ReportStatus,
        ReviewSource, Severity,
    };
    use shprd_core::log::SystemClock;
    use shprd_core::state::RunState;
    use shprd_views::news::QUIET_MS;

    /// The moment the fixtures start at.
    const NOW: u64 = 1_700_000_000_000;

    /// A log of one event naming a task at a given moment.
    fn event(seq: u64, at_ms: u64, task: &str) -> Event {
        Event {
            seq,
            at_ms,
            actor: Actor::Engine,
            kind: shprd_core::event::EventKind::TaskStatus {
                task: task.to_owned(),
                message: String::from("working"),
                progress: None,
            },
        }
    }

    /// The log `kinds` makes, one event a second apart from [`NOW`].
    fn kinds_events(kinds: Vec<EventKind>) -> Vec<Event> {
        kinds
            .into_iter()
            .enumerate()
            .map(|(i, kind)| Event {
                seq: i as u64 + 1,
                at_ms: NOW + i as u64 * 1_000,
                actor: Actor::Engine,
                kind,
            })
            .collect()
    }

    /// The state `kinds` folds to, as a run's log would leave it.
    fn folded(kinds: Vec<EventKind>) -> RunState {
        RunState::fold(&kinds_events(kinds)).expect("the fixture log folds")
    }

    /// A run on `/repo/shprd`, with nothing else.
    fn run() -> Vec<EventKind> {
        vec![EventKind::RunStarted {
            run_id: String::from("r1"),
            repo: String::from("/repo/shprd"),
            base_branch: String::from("main"),
            integration_branch: String::from("integration/m9"),
        }]
    }

    /// Task `A` added to the plan.
    fn added() -> EventKind {
        EventKind::TaskAdded {
            task: String::from("A"),
            title: String::from("task A"),
            scope: vec![String::from("a.txt")],
            acceptance: vec![String::from("it works")],
            tags: Vec::new(),
            skills: Vec::new(),
            deps: Vec::new(),
        }
    }

    /// Task `A`'s attempt `n` spawned.
    fn spawned(n: u32) -> EventKind {
        EventKind::Spawned {
            task: String::from("A"),
            attempt: n,
            agent: String::from("claude"),
            model: None,
            worktree: String::from("/wt/A"),
            branch: String::from("shprd/r1/A"),
        }
    }

    /// Task `A` reported a completed hand-in, with its checks green.
    fn reported() -> EventKind {
        EventKind::Reported {
            task: String::from("A"),
            attempt: 1,
            status: ReportStatus::Completed,
            summary: String::from("the news module is pure"),
            tests: Vec::new(),
            checks: vec![CheckReport {
                gate: String::from("tests"),
                command: String::from("cargo test"),
                exit_code: Some(0),
                output_tail: String::new(),
                pre_existing: None,
            }],
            changed_paths: Vec::new(),
            commit: Some(String::from("abc123")),
            skills_used: Vec::new(),
            proofs: Vec::new(),
        }
    }

    /// Task `A`'s review was handed to `codex`.
    fn review_asked() -> EventKind {
        EventKind::ReviewRequested {
            task: String::from("A"),
            attempt: 1,
            reviewer: String::from("codex"),
            degraded: false,
            note: None,
        }
    }

    /// The reviewer's verdict on `A`: `passed`, with `finding` when it did not.
    fn verdict(passed: bool, finding: Option<&str>) -> EventKind {
        let findings: Vec<Finding> = finding
            .map(|title| Finding {
                severity: Severity::Major,
                title: title.to_owned(),
                location: None,
                detail: String::from("detail"),
            })
            .into_iter()
            .collect();
        let source = Some(ReviewSource {
            reviewer: String::from("codex"),
            findings_sha256: String::from("bc"),
            request_seq: 5,
        });
        match passed {
            true => EventKind::GatePassed {
                task: String::from("A"),
                attempt: 1,
                gate: String::from("review"),
                findings,
                source,
            },
            false => EventKind::GateFailed {
                task: String::from("A"),
                attempt: 1,
                gate: String::from("review"),
                findings,
                source,
            },
        }
    }

    #[test]
    fn the_attempt_in_hand_decides_whether_a_review_failed() {
        let state = folded(
            {
                let mut kinds = run();
                kinds.extend([added(), spawned(1), reported(), review_asked()]);
                kinds
            }
            .into_iter()
            .chain([verdict(false, Some("the tests stub the clock"))])
            .collect(),
        );
        let task = state.tasks.get("A").expect("task A folded");
        let failed = review_failed(task).expect("its review failed");
        assert_eq!(failed.words, "the tests stub the clock");
        assert_eq!(
            failed.id, "6",
            "the mark is the verdict's own log line, so it is told once"
        );

        // A new attempt is the worker's answer to that review: it says nothing
        // about the attempt in hand.
        let events = {
            let mut kinds = run();
            kinds.extend([
                added(),
                spawned(1),
                reported(),
                review_asked(),
                verdict(false, Some("the tests stub the clock")),
                spawned(2),
            ]);
            kinds
        };
        let state = folded(events);
        let task = state.tasks.get("A").expect("task A folded");
        assert_eq!(review_failed(task), None);
    }

    #[test]
    fn a_review_that_passed_is_not_a_failure() {
        let events = {
            let mut kinds = run();
            kinds.extend([
                added(),
                spawned(1),
                reported(),
                review_asked(),
                verdict(true, None),
            ]);
            kinds
        };
        let state = folded(events);
        let task = state.tasks.get("A").expect("task A folded");
        assert_eq!(review_failed(task), None);
    }

    /// The log of a task that landed through its own pull request `number`.
    fn landed(number: u64) -> Vec<EventKind> {
        let mut kinds = run();
        kinds.extend([
            added(),
            spawned(1),
            reported(),
            review_asked(),
            verdict(true, None),
            EventKind::TaskPrOpened {
                task: String::from("A"),
                number,
                url: format!("https://example.invalid/pull/{number}"),
                head: String::from("shprd/r1/A"),
            },
            EventKind::TaskLanded {
                task: String::from("A"),
                base: String::from("integration/m9"),
                head: String::from("abc123"),
                number: Some(number),
                outcome: LandOutcome::Merged,
            },
        ]);
        kinds
    }

    #[test]
    fn a_done_task_says_how_it_ended() {
        let state = folded(landed(7));
        let task = state.tasks.get("A").expect("task A folded");
        assert_eq!(outcome(task).as_deref(), Some("merged"));

        let events = {
            let mut kinds = run();
            kinds.extend([
                added(),
                EventKind::TaskDropped {
                    task: String::from("A"),
                    reason: String::from("not needed"),
                },
            ]);
            kinds
        };
        let state = folded(events);
        let task = state.tasks.get("A").expect("task A folded");
        assert_eq!(outcome(task).as_deref(), Some("dropped"));
    }

    #[test]
    fn a_backup_that_started_is_the_one_told() {
        let events = {
            let mut kinds = run();
            kinds.extend([
                added(),
                spawned(1),
                EventKind::Backup {
                    task: String::from("A"),
                    attempt: 1,
                    from_agent: String::from("claude"),
                    to_agent: String::from("pi"),
                    cut: String::from("cut-1"),
                    class: ProviderClass::Quota,
                    reason: String::from("out of allowance"),
                    worktree: String::from("/wt/A"),
                    branch: String::from("shprd/r1/A"),
                    started: true,
                },
            ]);
            kinds
        };
        let state = folded(events);
        let task = state.tasks.get("A").expect("task A folded");
        let backup = backup(task).expect("its backup took over");
        assert_eq!(backup.words, "pi took over from claude: out of allowance");
        assert_eq!(backup.id, "4", "the mark is the backup's own log line");
    }

    #[test]
    fn an_agent_at_work_is_something_happening() {
        let kinds = {
            let mut kinds = run();
            kinds.extend([added(), spawned(1)]);
            kinds
        };
        let events = kinds_events(kinds);
        let state = RunState::fold(&events).expect("the fixture log folds");
        let task = state.tasks.get("A").expect("task A folded");
        let rows = BTreeMap::from([(
            String::from("A"),
            (
                Derived {
                    status: shprd_views::derived::DerivedStatus::Working,
                    needs: None,
                    reason: None,
                },
                TaskSignal::from_state(&state, "A").expect("task A's signal"),
            ),
        )]);
        // Half an hour after the last event: a worker running a long build.
        let now = NOW + QUIET_MS + 600_000;
        let space = Path::new("/repo/shprd");

        // The host reports its agent running, so nothing is quiet.
        let live = BTreeSet::from([String::from("A")]);
        let snapshot = readings(&state, space, &rows, &events, &live, now);
        assert_eq!(snapshot["A"].last_activity_ms, now);
        assert_eq!(shprd_views::news::stall_of(&snapshot["A"], now), None);
        assert_eq!(space_of(&state, space), "shprd");
        assert_eq!(task.title, "task A");

        // With no agent on it, the quiet rule still applies.
        let snapshot = readings(&state, space, &rows, &events, &BTreeSet::new(), now);
        assert_eq!(snapshot["A"].last_activity_ms, NOW + 2_000);
        assert!(
            shprd_views::news::stall_of(&snapshot["A"], now).is_some(),
            "a working task with nothing at all happening has stalled"
        );
    }

    #[test]
    fn two_passes_cannot_hold_the_memory_at_once() {
        let temp = tempfile::TempDir::new().expect("a temp dir");
        let path = temp.path().join("news.json");
        let held = lock_memory(&path).expect("the first pass takes the lock");
        assert!(path.with_extension("json.lock").exists(), "the lock file");
        let second = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path.with_extension("json.lock"))
            .expect("the lock file opens");
        assert!(
            second.try_lock().is_err(),
            "a second process's pass waits rather than racing"
        );
        drop(held);
        assert!(
            second.try_lock().is_ok(),
            "and takes it once the first is done"
        );
    }

    #[test]
    fn a_hand_in_carries_its_agents_words() {
        let events = {
            let mut kinds = run();
            kinds.extend([added(), spawned(1), reported()]);
            kinds
        };
        let state = folded(events);
        let task = state.tasks.get("A").expect("task A folded");
        assert_eq!(hand_in(task).as_deref(), Some("the news module is pure"));
    }

    #[test]
    fn the_space_is_the_repositorys_directory_name() {
        let state = folded(run());
        assert_eq!(space_of(&state, Path::new("/elsewhere/shprd")), "shprd");
    }

    #[test]
    fn activity_is_the_newest_event_about_each_task() {
        let events = vec![
            event(1, 1_000, "T1"),
            event(2, 2_000, "T1"),
            event(3, 1_500, "T2"),
        ];
        let at = activity(&events);
        assert_eq!(at.get("T1"), Some(&2_000));
        assert_eq!(at.get("T2"), Some(&1_500));
    }

    #[test]
    fn a_spawn_time_is_read_from_the_log() {
        let events = {
            let mut kinds = run();
            kinds.extend([added(), spawned(1), spawned(2)]);
            kinds
        };
        let events: Vec<Event> = kinds_events(events);
        let at = started_at(&events);
        assert_eq!(
            at.get("A"),
            Some(&(NOW + 3_000)),
            "the newest attempt's spawn is what the grace counts from"
        );
    }

    #[test]
    fn a_run_without_a_coordinator_tells_nothing() {
        let temp = tempfile::TempDir::new().expect("a temp dir");
        let desk = NewsDesk {
            coordinator: Arc::new(Silent),
            settle_ms: 0,
            clock: Arc::new(SystemClock),
        };
        let sent = tell(&desk, temp.path(), &RunState::default(), &Snapshot::new());
        assert!(sent.is_empty());
    }

    /// A coordinator that records nothing and refuses nothing.
    struct Silent;

    impl Coordinator for Silent {
        fn tell(&self, _pane: &str, _text: &str) -> Result<(), String> {
            Ok(())
        }
    }

    /// The reason the news recognises is the one constant the watch records, so
    /// the two cannot drift apart.
    #[test]
    fn the_news_and_the_watch_share_one_exit_reason() {
        assert_eq!(EXIT_REASON, shprd_watch::machine::EXIT_REASON);
    }

    /// A recorded process exit is a confirmed stop; a later status line from
    /// the same task is proof of life that takes the confirmation back.
    #[test]
    fn only_a_recorded_exit_confirms_a_stop() {
        let exit = EventKind::HandedOff {
            task: String::from("A"),
            from_agent: String::from("claude"),
            to_agent: String::from("claude"),
            checkpoint: String::new(),
            reason: Some(String::from(EXIT_REASON)),
        };
        let line = EventKind::TaskStatus {
            task: String::from("A"),
            message: String::from("still here"),
            progress: None,
        };
        let mut kinds = run();
        kinds.push(added());
        kinds.push(spawned(1));
        assert!(!exit_recorded(&kinds_events(kinds.clone()), "A"));
        kinds.push(exit);
        assert!(exit_recorded(&kinds_events(kinds.clone()), "A"));
        kinds.push(line);
        assert!(!exit_recorded(&kinds_events(kinds), "A"));
    }
}
