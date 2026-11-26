//! The tick's executor: one pass runs its steps, a failing step is recorded and
//! never stops the others, and two passes never overlap.
//!
//! The facts are scripted here — a fake forge's answers and a fake host's live
//! agents — so nothing touches a network, a terminal or an agent CLI. The start
//! is a closure that appends the `Spawned` event a real spawn would, so the
//! ledger the pass writes is the ledger the CLI writes.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use shprd_core::config::{Config, Land};
use shprd_core::event::{Actor, CheckReport, Event, EventKind, ReportStatus, ReviewSource};
use shprd_core::log::{Clock, EventLog, SystemClock};
use shprd_core::state::RunState;
use shprd_core::tick::PrFacts;
use shprd_git::forge::{FakeForge, Forge, ForgeError, Pr, PrStatus};
use shprd_server::Worker;
use shprd_server::hub::Hub;
use shprd_server::tick::{self, FactsSource, FallbackSource, SharedForge, StepReport};
use shprd_watch::fallback::{AgentFacts, Handover, Role};
use shprd_watch::records::{Cut, ErrorKind};
use tempfile::TempDir;

/// A repository whose log is a fixture, and the states it is advanced through.
struct Fixture {
    /// Keeps the temp directory alive.
    _temp: TempDir,
    /// The repository the run lives in.
    repo: PathBuf,
    /// The home shprd's own files go under.
    home: PathBuf,
}

impl Fixture {
    /// A repository holding `events` as its run log.
    fn new(events: Vec<Event>) -> Fixture {
        let temp = TempDir::new().expect("temp dir");
        let repo = temp.path().join("repo");
        let home = temp.path().join("home");
        std::fs::create_dir_all(repo.join(".shprd")).expect("mkdir .shprd");
        std::fs::create_dir_all(&home).expect("mkdir the home");
        let path = repo.join(".shprd").join("events.jsonl");
        let mut log = EventLog::open(&path).expect("open the log");
        let clock = SystemClock;
        for event in events {
            log.append(event.actor, event.kind, &clock)
                .expect("append the fixture");
        }
        Fixture {
            _temp: temp,
            repo,
            home,
        }
    }

    /// Writes the person's settings file: a named reviewer, as M9.3 stores it.
    fn settings(&self, text: &str) {
        let dir = self.home.join(".config").join("shprd");
        std::fs::create_dir_all(&dir).expect("mkdir the settings dir");
        std::fs::write(dir.join("settings.toml"), text).expect("write the settings");
    }

    /// Every event in the log, oldest first.
    fn events(&self) -> Vec<Event> {
        let path = self.repo.join(".shprd").join("events.jsonl");
        EventLog::read_all(&path).expect("read the log").events
    }

    /// The folded state.
    fn state(&self) -> RunState {
        RunState::fold(&self.events()).expect("the log folds")
    }

    /// A worker over this fixture, with `facts`, `config` and `start`.
    fn worker(
        &self,
        facts: Arc<dyn FactsSource>,
        forge: SharedForge,
        config: Config,
        start: shprd_server::Starter,
    ) -> Worker {
        Worker {
            repo: self.repo.clone(),
            home: self.home.clone(),
            state_dir: shprd_server::state_dir(&shprd_server::state_home(&self.home, None)),
            config,
            facts,
            fallback: None,
            forge,
            local: false,
            start,
            review: None,
            close_reviewer: None,
            switch_reviewers: None,
            close_worker: None,
            actor: Actor::Engine,
            after_write: None,
            hub: Hub::new(),
            news: None,
        }
    }
}

/// A facts source that answers exactly what a test scripted.
#[derive(Default)]
struct Scripted {
    /// What the forge says, by task.
    prs: BTreeMap<String, PrFacts>,
    /// Which tasks have an agent running.
    live: BTreeSet<String>,
}

impl FactsSource for Scripted {
    fn pr(&self, task: &shprd_core::state::Task, _state: &RunState) -> Option<PrFacts> {
        self.prs.get(&task.id).cloned()
    }

    fn live(&self, _state: &RunState) -> BTreeSet<String> {
        self.live.clone()
    }
}

/// A forge that answers nothing: a pass that only starts tasks never asks it.
#[derive(Debug)]
struct NoForge;

impl Forge for NoForge {
    fn find_open(&self, _head: &str) -> Result<Option<Pr>, ForgeError> {
        Ok(None)
    }

    fn create(
        &self,
        _base: &str,
        _head: &str,
        _title: &str,
        _body_file: &Path,
    ) -> Result<Pr, ForgeError> {
        Err(ForgeError::Failed {
            args: vec![String::from("create")],
            code: Some(1),
            stderr: String::from("this test never opens a pull request"),
        })
    }

    fn edit_body(&self, _number: u64, _body_file: &Path) -> Result<(), ForgeError> {
        Ok(())
    }

    fn status(&self, _number: u64) -> Result<PrStatus, ForgeError> {
        Err(ForgeError::Failed {
            args: vec![String::from("status")],
            code: Some(1),
            stderr: String::from("this test never asks"),
        })
    }

    fn merge(&self, _number: u64, _head_sha: &str) -> Result<(), ForgeError> {
        Err(ForgeError::Failed {
            args: vec![String::from("merge")],
            code: Some(1),
            stderr: String::from("this test never merges"),
        })
    }
}

/// An in-memory forge a test scripts, safe to share across threads.
#[derive(Debug, Default)]
struct SharedFake {
    /// The one scripted forge, behind a lock.
    inner: Mutex<FakeForge>,
}

impl SharedFake {
    /// A forge with `prs` open, each at `status`.
    fn new(prs: Vec<(Pr, PrStatus)>) -> Self {
        let forge = FakeForge::new();
        for (pr, status) in prs {
            let number = pr.number;
            forge.add_open(pr);
            forge.set_status(number, status);
        }
        Self {
            inner: Mutex::new(forge),
        }
    }

    /// Every call the forge recorded.
    fn calls(&self) -> Vec<String> {
        match self.inner.lock() {
            Ok(forge) => forge.calls(),
            Err(poisoned) => poisoned.into_inner().calls(),
        }
    }

    /// Runs `f` against the scripted forge.
    fn with<T>(&self, f: impl FnOnce(&FakeForge) -> T) -> T {
        match self.inner.lock() {
            Ok(forge) => f(&forge),
            Err(poisoned) => f(&poisoned.into_inner()),
        }
    }
}

impl Forge for SharedFake {
    fn find_open(&self, head: &str) -> Result<Option<Pr>, ForgeError> {
        self.with(|forge| forge.find_open(head))
    }

    fn create(
        &self,
        base: &str,
        head: &str,
        title: &str,
        body_file: &Path,
    ) -> Result<Pr, ForgeError> {
        self.with(|forge| forge.create(base, head, title, body_file))
    }

    fn edit_body(&self, number: u64, body_file: &Path) -> Result<(), ForgeError> {
        self.with(|forge| forge.edit_body(number, body_file))
    }

    fn status(&self, number: u64) -> Result<PrStatus, ForgeError> {
        self.with(|forge| forge.status(number))
    }

    fn merge(&self, number: u64, head_sha: &str) -> Result<(), ForgeError> {
        self.with(|forge| forge.merge(number, head_sha))
    }

    fn mergeable(&self, number: u64) -> Result<Option<bool>, ForgeError> {
        self.with(|forge| forge.mergeable(number))
    }

    fn close(&self, number: u64, comment: &str) -> Result<(), ForgeError> {
        self.with(|forge| forge.close(number, comment))
    }
}

/// An event with `kind` at position `seq`.
fn ev(seq: u64, kind: EventKind) -> Event {
    Event {
        seq,
        at_ms: seq * 1000,
        actor: Actor::Engine,
        kind,
    }
}

/// The log's first event: run `r1` cut from `main`.
fn started() -> Event {
    ev(
        1,
        EventKind::RunStarted {
            run_id: String::from("r1"),
            repo: String::from("/repo"),
            base_branch: String::from("main"),
            integration_branch: String::from("integration/m8"),
        },
    )
}

/// A task added at `seq`, waiting on nothing.
fn added(seq: u64, id: &str) -> Event {
    ev(
        seq,
        EventKind::TaskAdded {
            task: id.to_string(),
            title: format!("task {id}"),
            scope: vec![format!("task-{id}.txt")],
            acceptance: vec![String::from("it works")],
            tags: Vec::new(),
            skills: Vec::new(),
            deps: Vec::new(),
        },
    )
}

/// Task `id` driven from added to ready to land, with one passing check and an
/// independent, engine-recorded review.
fn ready_task(
    seq: u64,
    id: &str,
    commit: &str,
    worktree: &str,
    branch: &str,
    number: u64,
) -> Vec<Event> {
    vec![
        added(seq, id),
        ev(
            seq + 1,
            EventKind::Spawned {
                task: id.to_string(),
                attempt: 1,
                agent: String::from("claude"),
                model: None,
                worktree: worktree.to_string(),
                branch: branch.to_string(),
            },
        ),
        ev(
            seq + 2,
            EventKind::Reported {
                task: id.to_string(),
                attempt: 1,
                status: ReportStatus::Completed,
                summary: String::from("done"),
                tests: Vec::new(),
                checks: vec![CheckReport {
                    gate: String::from("tests"),
                    command: String::from("cargo test"),
                    exit_code: Some(0),
                    output_tail: String::new(),
                }],
                changed_paths: Vec::new(),
                commit: Some(commit.to_string()),
                skills_used: Vec::new(),
                proofs: Vec::new(),
            },
        ),
        ev(
            seq + 3,
            EventKind::ReviewRequested {
                task: id.to_string(),
                attempt: 1,
                reviewer: String::from("codex"),
                degraded: false,
                note: None,
            },
        ),
        ev(
            seq + 4,
            EventKind::GatePassed {
                task: id.to_string(),
                attempt: 1,
                gate: String::from("review"),
                findings: Vec::new(),
                source: Some(ReviewSource {
                    reviewer: String::from("codex"),
                    findings_sha256: String::from(
                        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
                    ),
                    request_seq: seq + 3,
                }),
            },
        ),
        ev(
            seq + 5,
            EventKind::TaskPrOpened {
                task: id.to_string(),
                number,
                url: format!("https://example.invalid/pull/{number}"),
                head: branch.to_string(),
            },
        ),
    ]
}

/// Task `a` driven from added to ready to land, with one passing check and an
/// independent, engine-recorded review.
fn ready(seq: u64, commit: &str) -> Vec<Event> {
    ready_task(seq, "A", commit, "/wt/A", "shprd/r1/A", 1)
}

/// The config a pass runs under: `merge`, four slots.
fn config(merge: shprd_core::config::MergePolicy) -> Config {
    Config {
        land: Land {
            merge,
            protected: Vec::new(),
        },
        ..Config::default()
    }
}

/// A starter that records the attempt and says so.
fn recording_start(calls: Arc<Mutex<Vec<String>>>) -> shprd_server::Starter {
    Arc::new(move |_repo: &Path, task: &str| {
        calls
            .lock()
            .map_err(|_| String::from("the call log is poisoned"))?
            .push(task.to_string());
        Ok(format!("started {task}"))
    })
}

#[test]
fn a_forge_that_cannot_be_asked_yields_no_action_that_depends_on_it() {
    // The forge fails every call, as one that is down would. A readiness
    // decision that needs its word must not be made.
    let mut log = vec![started()];
    log.extend(ready(2, "abc123"));
    let fixture = Fixture::new(log);
    let forge: SharedForge = Arc::new(NoForge);
    let facts: Arc<dyn FactsSource> = Arc::new(shprd_server::LiveFacts::new(
        Arc::clone(&forge),
        Arc::new(|_state| BTreeSet::new()),
    ));
    let worker = fixture.worker(
        facts,
        forge,
        config(shprd_core::config::MergePolicy::Checks),
        recording_start(Arc::new(Mutex::new(Vec::new()))),
    );
    let report = worker.tick().expect("the pass ran");
    assert!(
        report.steps.is_empty(),
        "nothing may depend on a forge that said nothing: {:?}",
        report.steps
    );
    assert_eq!(
        fixture.state().tasks.get("A").map(|task| task.status),
        Some(shprd_core::state::TaskStatus::Passed),
        "the task was left exactly as it was"
    );
}

#[test]
fn a_pass_publishes_its_writes_on_the_service_hub() {
    // The service's tick and its API share one change counter, so a task the
    // tick started wakes a long poll exactly as one added through the API does.
    let mut log = vec![started()];
    log.extend(ready(2, "abc123"));
    let fixture = Fixture::new(log);
    let forge: SharedForge = Arc::new(SharedFake::new(Vec::new()));
    let mut facts = Scripted::default();
    facts.prs.insert(
        String::from("A"),
        PrFacts {
            open: false,
            merged: true,
            checks: Some(true),
            head: Some(String::from("abc123")),
            number: Some(1),
        },
    );
    let hub = Hub::new();
    let worker = Worker {
        repo: fixture.repo.clone(),
        home: fixture.home.clone(),
        state_dir: shprd_server::state_dir(&shprd_server::state_home(&fixture.home, None)),
        config: config(shprd_core::config::MergePolicy::Ask),
        facts: Arc::new(facts),
        fallback: None,
        forge,
        local: false,
        start: recording_start(Arc::new(Mutex::new(Vec::new()))),
        review: None,
        close_reviewer: None,
        switch_reviewers: None,
        close_worker: None,
        actor: Actor::Engine,
        after_write: None,
        hub: hub.clone(),
        news: None,
    };
    let report = worker.tick().expect("the pass ran");
    let seq = report.last_seq().expect("the pass wrote something");
    assert_eq!(hub.last(), seq, "the write was published");
}

#[test]
fn a_failing_step_is_recorded_and_the_others_still_run() {
    let fixture = Fixture::new(vec![started(), added(2, "A"), added(3, "B")]);
    let forge: SharedForge = Arc::new(NoForge);
    let facts = Arc::new(Scripted::default());
    let start: shprd_server::Starter = Arc::new(|_repo: &Path, task: &str| {
        if task == "A" {
            return Err(String::from("no agent CLI is installed"));
        }
        Ok(format!("started {task}"))
    });
    let worker = fixture.worker(
        facts,
        forge,
        config(shprd_core::config::MergePolicy::Auto),
        start,
    );
    let report = worker.tick().expect("the pass ran");

    assert_eq!(report.steps.len(), 2, "both tasks were attempted");
    let first = report.steps.first().expect("A's step");
    assert_eq!(first.task, "A");
    assert!(!first.ok, "the failure is recorded: {first:?}");
    assert!(first.detail.contains("no agent CLI"), "{first:?}");
    let second = report.steps.get(1).expect("B's step");
    assert_eq!(second.task, "B");
    assert!(second.ok, "B was not held up by A: {second:?}");
    assert!(!report.ok(), "the pass reports the failure it saw");
}

#[test]
fn a_failed_step_is_readable_after_the_pass() {
    // Bug 1 of the M9 gate: a failing step said so only through a
    // `tracing::warn!` that nothing printed, so the person saw a board that did
    // not move and nothing more. The pass now leaves its outcome where
    // `shprd status` and `GET /api/state` read it.
    let fixture = Fixture::new(vec![started(), added(2, "A")]);
    let forge: SharedForge = Arc::new(NoForge);
    let facts = Arc::new(Scripted::default());
    let start: shprd_server::Starter =
        Arc::new(|_repo: &Path, _task: &str| Err(String::from("no agent CLI is installed")));
    let worker = fixture.worker(
        facts,
        forge,
        config(shprd_core::config::MergePolicy::Auto),
        start,
    );
    let report = worker.tick().expect("the pass ran");
    assert!(!report.ok(), "the pass failed a step");

    let dir = shprd_server::state_dir(&shprd_server::state_home(&fixture.home, None));
    let last = shprd_server::LastPass::read(&dir, &fixture.repo).expect("the pass is recorded");
    assert_eq!(last.run.as_deref(), Some("r1"));
    assert_eq!(last.steps, report.steps.len());
    assert_eq!(
        last.failed,
        vec![shprd_server::FailedStep {
            task: String::from("A"),
            step: String::from("start"),
            error: String::from("no agent CLI is installed"),
        }],
        "the step's own error is what the person reads"
    );

    // The next pass that works replaces it: the file is about the last pass,
    // never about the worst one.
    let worker = fixture.worker(
        Arc::new(Scripted::default()),
        Arc::new(NoForge),
        config(shprd_core::config::MergePolicy::Auto),
        recording_start(Arc::new(Mutex::new(Vec::new()))),
    );
    worker.tick().expect("the second pass ran");
    let last = shprd_server::LastPass::read(&dir, &fixture.repo).expect("the pass is recorded");
    assert!(last.failed.is_empty(), "{:?}", last.failed);
}

#[test]
fn a_step_that_fails_the_same_way_three_passes_asks_a_person_once() {
    // Bug 3 of gate run #4: a `start` that failed every pass warned every pass
    // and never reached a person, so the board showed an unchanged row while
    // 99 identical lines went to a terminal nobody read. The third identical
    // failure is a card, recorded once, naming the step and its error.
    let fixture = Fixture::new(vec![started(), added(2, "C2")]);
    let start: shprd_server::Starter =
        Arc::new(|_repo: &Path, _task: &str| Err(String::from("no agent CLI is installed for C2")));
    let worker = fixture.worker(
        Arc::new(Scripted::default()),
        Arc::new(NoForge),
        config(shprd_core::config::MergePolicy::Auto),
        start,
    );

    for pass in 1..=2 {
        let report = worker.tick().expect("the pass ran");
        assert!(!report.ok(), "pass {pass} failed as scripted");
        let state = fixture.state();
        assert!(
            state.escalations.is_empty(),
            "pass {pass} is a warning, not yet a card: {:?}",
            state.escalations
        );
    }

    worker.tick().expect("the third pass ran");
    let state = fixture.state();
    assert_eq!(state.escalations.len(), 1, "{:?}", state.escalations);
    let card = &state.escalations[0];
    assert_eq!(card.task.as_deref(), Some("C2"));
    assert!(
        card.body
            .contains("its `start` step has failed the same way 3 passes in a row"),
        "{}",
        card.body
    );
    assert!(
        card.body.contains("(attempt 0)"),
        "the card is about the attempt the failure happened in, so a fix or a \
         new attempt retires it: {}",
        card.body
    );
    assert!(
        card.body.contains("no agent CLI is installed"),
        "the card names the error: {}",
        card.body
    );
    assert!(
        card.default_action.contains("retries `start`"),
        "the card says what happens once it is fixed: {}",
        card.default_action
    );

    // A fourth pass writes no second card for the same run of failures.
    worker.tick().expect("the fourth pass ran");
    assert_eq!(
        fixture.state().escalations.len(),
        1,
        "one run of failures is one card"
    );

    // And the board reads the card as the person's: the row says Needs you,
    // exactly as `needs` does.
    let now = SystemClock.now_ms();
    let board = shprd_server::dto::SnapshotDto::from_state(&fixture.state(), now);
    assert_eq!(board.needs.len(), 1, "{:?}", board.needs);
    let row = board
        .tasks
        .iter()
        .find(|task| task.id == "C2")
        .expect("C2's row");
    assert_eq!(
        row.derived.status,
        shprd_server::dto::DerivedStatusDto::NeedsYou,
        "{:?}",
        row.derived
    );
}

#[test]
fn a_pass_that_cannot_read_the_run_is_recorded_too() {
    // The one failure that ends a pass before any step runs is the one a
    // person must hear about, because nothing else will move.
    let fixture = Fixture::new(vec![started()]);
    std::fs::write(
        fixture.repo.join(".shprd").join("events.jsonl"),
        "not an event\n",
    )
    .expect("a broken log");

    let worker = fixture.worker(
        Arc::new(Scripted::default()),
        Arc::new(NoForge),
        config(shprd_core::config::MergePolicy::Auto),
        recording_start(Arc::new(Mutex::new(Vec::new()))),
    );
    assert!(worker.tick().is_err(), "a broken log ends the pass");

    let dir = shprd_server::state_dir(&shprd_server::state_home(&fixture.home, None));
    let last = shprd_server::LastPass::read(&dir, &fixture.repo).expect("the failure is recorded");
    let failed = last.failed.first().expect("one failed step");
    assert_eq!(failed.step, "pass");
    assert!(
        !failed.error.is_empty(),
        "the reason the pass could not run is carried"
    );
}

#[test]
fn a_pass_that_starts_a_task_records_it_once() {
    let fixture = Fixture::new(vec![started(), added(2, "A")]);
    let forge: SharedForge = Arc::new(NoForge);
    let facts = Arc::new(Scripted::default());
    let calls = Arc::new(Mutex::new(Vec::new()));
    let worker = fixture.worker(
        facts,
        forge,
        config(shprd_core::config::MergePolicy::Auto),
        recording_start(Arc::clone(&calls)),
    );
    let report = worker.tick().expect("the pass ran");
    assert_eq!(report.steps.len(), 1);
    assert!(report.steps[0].ok);
    assert_eq!(*calls.lock().expect("the call log"), vec!["A".to_string()]);
}

/// A task that spawned and handed in, with one passing check and no review yet.
fn handed_in(seq: u64) -> Vec<Event> {
    handed_in_attempt(seq, 1)
}

/// [`handed_in`] for attempt `attempt`.
fn handed_in_attempt(seq: u64, attempt: u32) -> Vec<Event> {
    vec![
        ev(
            seq,
            EventKind::Spawned {
                task: String::from("A"),
                attempt,
                agent: String::from("claude"),
                model: None,
                worktree: String::from("/wt/A"),
                branch: String::from("shprd/r1/A"),
            },
        ),
        ev(
            seq + 1,
            EventKind::Reported {
                task: String::from("A"),
                attempt,
                status: ReportStatus::Completed,
                summary: String::from("done"),
                tests: Vec::new(),
                checks: vec![CheckReport {
                    gate: String::from("tests"),
                    command: String::from("cargo test"),
                    exit_code: Some(0),
                    output_tail: String::new(),
                }],
                changed_paths: Vec::new(),
                commit: Some(String::from("abc123")),
                skills_used: Vec::new(),
                proofs: Vec::new(),
            },
        ),
    ]
}

#[test]
fn a_hand_in_starts_the_engine_reviewer() {
    // The worker hands in and the pass starts the reviewer, through the same
    // review path the verb uses.
    let mut log = vec![started(), added(2, "A")];
    log.extend(handed_in(3));
    let fixture = Fixture::new(log);
    // A reviewer is configured the M9.3 way: a role that names a program.
    fixture
        .settings("version = 1\n\n[roles.reviewer]\nagent = \"codex\"\nmodel = \"gpt-5-codex\"\n");
    let calls = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&calls);
    let review: shprd_server::Reviewer = Arc::new(move |_repo: &Path, task: &str| {
        recorded
            .lock()
            .map_err(|_| String::from("the call log is poisoned"))?
            .push(task.to_string());
        Ok(format!("started a reviewer on {task}"))
    });
    let worker = Worker {
        repo: fixture.repo.clone(),
        home: fixture.home.clone(),
        state_dir: shprd_server::state_dir(&shprd_server::state_home(&fixture.home, None)),
        config: config(shprd_core::config::MergePolicy::Auto),
        facts: Arc::new(Scripted::default()),
        fallback: None,
        forge: Arc::new(NoForge),
        local: false,
        start: recording_start(Arc::new(Mutex::new(Vec::new()))),
        review: Some(review),
        close_reviewer: None,
        switch_reviewers: None,
        close_worker: None,
        actor: Actor::Engine,
        after_write: None,
        hub: Hub::new(),
        news: None,
    };

    let report = worker.tick().expect("the pass ran");

    assert_eq!(report.steps.len(), 1, "{:?}", report.steps);
    assert!(report.steps[0].ok, "{:?}", report.steps);
    assert_eq!(report.steps[0].action, "review");
    assert_eq!(report.steps[0].task, "A");
    assert_eq!(*calls.lock().expect("the call log"), vec!["A".to_string()]);
}

#[test]
fn a_pass_without_a_reviewer_passes_the_hand_in_on_its_checks() {
    // The review path is wired, but the settings name no reviewer: review is
    // off, so there is nothing to start and the clean hand-in passes on its
    // checks, recorded as a skipped review (never as a review verdict).
    let mut log = vec![started(), added(2, "A")];
    log.extend(handed_in(3));
    let fixture = Fixture::new(log);
    let review: shprd_server::Reviewer =
        Arc::new(|_repo: &Path, task: &str| Ok(format!("started a reviewer on {task}")));
    let worker = Worker {
        repo: fixture.repo.clone(),
        home: fixture.home.clone(),
        state_dir: shprd_server::state_dir(&shprd_server::state_home(&fixture.home, None)),
        config: config(shprd_core::config::MergePolicy::Auto),
        facts: Arc::new(Scripted::default()),
        fallback: None,
        forge: Arc::new(NoForge),
        local: false,
        start: recording_start(Arc::new(Mutex::new(Vec::new()))),
        review: Some(review),
        close_reviewer: None,
        switch_reviewers: None,
        close_worker: None,
        actor: Actor::Engine,
        after_write: None,
        hub: Hub::new(),
        news: None,
    };

    let report = worker.tick().expect("the pass ran");

    let actions: Vec<&str> = report.steps.iter().map(|s| s.action.as_str()).collect();
    assert_eq!(actions, ["skip_review"], "{:?}", report.steps);
    // A second pass has nothing left to do: the skip is recorded once.
    let again = worker.tick().expect("the pass ran");
    assert!(again.steps.is_empty(), "{:?}", again.steps);
}

/// A worker over `fixture` whose review path is `review`.
fn reviewing(fixture: &Fixture, review: shprd_server::Reviewer) -> Worker {
    Worker {
        repo: fixture.repo.clone(),
        home: fixture.home.clone(),
        state_dir: shprd_server::state_dir(&shprd_server::state_home(&fixture.home, None)),
        config: config(shprd_core::config::MergePolicy::Auto),
        facts: Arc::new(Scripted::default()),
        fallback: None,
        forge: Arc::new(NoForge),
        local: false,
        start: recording_start(Arc::new(Mutex::new(Vec::new()))),
        review: Some(review),
        close_reviewer: None,
        switch_reviewers: None,
        close_worker: None,
        actor: Actor::Engine,
        after_write: None,
        hub: Hub::new(),
        news: None,
    }
}

/// Every escalation the fixture's log holds, as (task, body, default action).
fn escalations(fixture: &Fixture) -> Vec<(Option<String>, String, String)> {
    fixture
        .events()
        .into_iter()
        .filter_map(|event| match event.kind {
            EventKind::Escalated {
                task,
                body,
                default_action,
            } => Some((task, body, default_action)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_review_that_fails_after_its_request_is_escalated_once() {
    // The real review records its request before it opens the tab (so `land`
    // can trust it), so a multiplexer that is down leaves a request with no
    // reviewer and no request-less retry. The pass must tell the person.
    let mut log = vec![started(), added(2, "A")];
    log.extend(handed_in(3));
    let fixture = Fixture::new(log);
    fixture
        .settings("version = 1\n\n[roles.reviewer]\nagent = \"codex\"\nmodel = \"gpt-5-codex\"\n");
    let review: shprd_server::Reviewer = Arc::new(move |repo: &Path, task: &str| {
        // What `review()` does before the tab: record who reviews this attempt.
        let path = repo.join(".shprd").join("events.jsonl");
        let mut log = EventLog::open(&path).map_err(|err| err.to_string())?;
        log.append(
            Actor::Engine,
            EventKind::ReviewRequested {
                task: task.to_string(),
                attempt: 1,
                reviewer: String::from("codex"),
                degraded: false,
                note: None,
            },
            &SystemClock,
        )
        .map_err(|err| err.to_string())?;
        Err(String::from("the multiplexer is not running"))
    });
    let worker = reviewing(&fixture, review);

    let first = worker.tick().expect("the pass ran");
    assert_eq!(first.steps.len(), 1, "{:?}", first.steps);
    assert_eq!(first.steps[0].action, "review");
    assert!(!first.steps[0].ok, "{:?}", first.steps);

    let recorded = escalations(&fixture);
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    let (task, body, action) = &recorded[0];
    assert_eq!(task.as_deref(), Some("A"));
    assert!(
        body.starts_with("A: the engine's review did not start: "),
        "{body}"
    );
    assert!(body.contains("the multiplexer is not running"), "{body}");
    assert!(action.contains("--again"), "{action}");

    // The recorded request is what stops the next pass planning it again.
    let second = worker.tick().expect("the second pass ran");
    assert!(second.steps.is_empty(), "{:?}", second.steps);
    assert_eq!(
        escalations(&fixture).len(),
        1,
        "the escalation is said once"
    );
}

#[test]
fn a_review_the_cross_vendor_rule_refuses_is_escalated_once() {
    // A refusal records no request, so every pass plans the step again; the
    // person hears once, not once per pass.
    let mut log = vec![started(), added(2, "A")];
    log.extend(handed_in(3));
    let fixture = Fixture::new(log);
    fixture.settings("version = 1\n\n[roles.reviewer]\nagent = \"pi\"\nmodel = \"deepseek/deepseek-v4.1-flash\"\n");
    let review: shprd_server::Reviewer = Arc::new(move |_repo: &Path, _task: &str| {
        Err(String::from(
            "the reviewer and the worker both run models from the deepseek family",
        ))
    });
    let worker = reviewing(&fixture, review);

    for _ in 0..2 {
        let report = worker.tick().expect("the pass ran");
        assert_eq!(report.steps.len(), 1, "{:?}", report.steps);
        assert!(!report.steps[0].ok, "{:?}", report.steps);
    }

    let escalations = escalations(&fixture);
    assert_eq!(escalations.len(), 1, "{escalations:?}");
    assert!(
        escalations[0]
            .1
            .contains("both run models from the deepseek family"),
        "{escalations:?}"
    );
}

/// Runs git in `dir` and returns its trimmed stdout, asserting success.
fn git_in(dir: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "shprd tests")
        .env("GIT_AUTHOR_EMAIL", "tests@example.invalid")
        .env("GIT_COMMITTER_NAME", "shprd tests")
        .env("GIT_COMMITTER_EMAIL", "tests@example.invalid")
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?} in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Appends one event to `fixture`'s log as `actor`, the way a verb would.
fn append(fixture: &Fixture, actor: Actor, kind: EventKind) {
    let path = fixture.repo.join(".shprd").join("events.jsonl");
    let mut log = EventLog::open(&path).expect("open the log");
    log.append(actor, kind, &SystemClock)
        .expect("the event is legal");
}

/// A starter that records an attempt the way `shprd agent spawn --task` does:
/// a real worktree on the task's branch, and the `Spawned` event.
///
/// The worktree is created once and reused by the attempt after it, because a
/// retry replaces the attempt on the same path and branch and the real spawn's
/// archiving is tested where it lives.
fn real_start(
    repo: PathBuf,
    home: PathBuf,
    worktree: PathBuf,
    branch: String,
) -> shprd_server::Starter {
    Arc::new(move |_repo: &Path, task: &str| {
        if !worktree.is_dir() {
            shprd_git::worktree::create(
                &repo,
                &shprd_git::worktree::Layout::default_for_home(&home),
                "r1",
                task,
                "integration/m8",
            )
            .map_err(|err| err.to_string())?;
        }
        let path = repo.join(".shprd").join("events.jsonl");
        let mut log = EventLog::open(&path).map_err(|err| err.to_string())?;
        let events = EventLog::read_all(&path)
            .map_err(|err| err.to_string())?
            .events;
        let attempt = RunState::fold(&events)
            .map_err(|err| err.to_string())?
            .tasks
            .get(task)
            .map(|found| found.attempts.len() as u32 + 1)
            .unwrap_or(1);
        log.append(
            Actor::Engine,
            EventKind::Spawned {
                task: task.to_string(),
                attempt,
                agent: String::from("claude"),
                model: None,
                worktree: worktree.display().to_string(),
                branch: branch.clone(),
            },
            &SystemClock,
        )
        .map_err(|err| err.to_string())?;
        Ok(format!("started {task} attempt {attempt}"))
    })
}

#[test]
fn a_failed_review_is_sent_back_and_the_second_round_lands() {
    // Bug 2 of the M9 gate: a review that failed with a round left must send the
    // task back by itself, with the reviewer's findings in the new brief, and
    // the attempt after it lands under `auto` with no hand work at all.
    let fixture = Fixture::new(vec![started(), added(2, "A")]);
    // A repository a landing can merge into: no remote, so the run's own
    // integration worktree is where `auto` lands.
    git_in(&fixture.repo, &["init", "-b", "main"]);
    std::fs::write(fixture.repo.join("seed.txt"), "the seed\n").expect("write the seed");
    git_in(&fixture.repo, &["add", "seed.txt"]);
    git_in(&fixture.repo, &["commit", "-m", "seed"]);
    git_in(&fixture.repo, &["branch", "integration/m8"]);

    let home = fixture.home.clone();
    let layout = shprd_git::worktree::Layout::default_for_home(&home);
    let repo_name = fixture
        .repo
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .expect("the repo has a name");
    let worktree = layout.path(&repo_name, "r1", "A");
    let branch = layout.branch_name("r1", "A");
    let start = real_start(
        fixture.repo.clone(),
        home.clone(),
        worktree.clone(),
        branch.clone(),
    );
    let mut worker = fixture.worker(
        Arc::new(Scripted::default()),
        Arc::new(NoForge),
        config(shprd_core::config::MergePolicy::Auto),
        start,
    );
    worker.local = true;

    // Pass 1: the queued task starts, and the worktree is real.
    let first = worker.tick().expect("the first pass ran");
    assert_eq!(first.steps.len(), 1, "{first:?}");
    assert_eq!(first.steps[0].action, "start", "{first:?}");
    assert_eq!(first.steps[0].task, "A");
    assert!(worktree.is_dir(), "{}", worktree.display());

    // The worker commits and hands in; the engine asks for its review, and the
    // reviewer finds something that blocks.
    std::fs::write(worktree.join("task-A.txt"), "one\n").expect("write the work");
    git_in(&worktree, &["add", "task-A.txt"]);
    git_in(&worktree, &["commit", "-m", "the work"]);
    let head = git_in(&worktree, &["rev-parse", "HEAD"]);
    append(
        &fixture,
        Actor::Agent {
            id: String::from("claude"),
        },
        EventKind::Reported {
            task: String::from("A"),
            attempt: 1,
            status: ReportStatus::Completed,
            summary: String::from("done"),
            tests: Vec::new(),
            checks: vec![CheckReport {
                gate: String::from("tests"),
                command: String::from("true"),
                exit_code: Some(0),
                output_tail: String::new(),
            }],
            changed_paths: vec![String::from("task-A.txt")],
            commit: Some(head),
            skills_used: Vec::new(),
            proofs: Vec::new(),
        },
    );
    append(
        &fixture,
        Actor::Engine,
        EventKind::ReviewRequested {
            task: String::from("A"),
            attempt: 1,
            reviewer: String::from("codex"),
            degraded: false,
            note: None,
        },
    );
    write_findings_for_attempt(
        &fixture,
        "A",
        1,
        r#"[{"severity":"critical","title":"the retry loses the batch","detail":"do not drop it"}]"#,
    );

    // Pass 2: the pass decides the review, and one of two rounds is spent.
    let second = worker.tick().expect("the second pass ran");
    assert_eq!(second.steps.len(), 1, "{second:?}");
    assert_eq!(second.steps[0].action, "verdict", "{second:?}");
    assert_eq!(
        fixture.state().tasks.get("A").map(|task| task.status),
        Some(shprd_core::state::TaskStatus::Failed),
        "a blocking finding fails the attempt"
    );

    // Pass 3: the failed review sends the task back by itself.
    let third = worker.tick().expect("the third pass ran");
    assert_eq!(third.steps.len(), 1, "{third:?}");
    assert_eq!(third.steps[0].action, "send_back", "{third:?}");
    assert!(third.steps[0].ok, "{third:?}");
    let state = fixture.state();
    let task = state.tasks.get("A").expect("the task");
    assert_eq!(
        task.attempts.last().map(|attempt| attempt.n),
        Some(2),
        "the send-back started a second attempt"
    );
    let spawned = fixture
        .events()
        .into_iter()
        .rev()
        .find(|event| matches!(event.kind, EventKind::Spawned { .. }))
        .expect("the new attempt is in the log");
    assert_eq!(spawned.actor, Actor::Engine, "the engine records it");

    // The brief the second attempt gets carries the reviewer's findings, because
    // the spawn path renders it from the gate the log holds.
    let brief = shprd_views::brief::task_brief(&shprd_views::brief::BriefInput {
        task,
        run: "r1",
        attempt: 2,
        agent: "claude",
        model: None,
        branch: &branch,
        worktree: &worktree.display().to_string(),
        skills: &[],
        gates: &[],
        archive: None,
        prompt: None,
        ask: shprd_views::brief::AskRoute::Hook,
        proof: true,
        settings: None,
        role: None,
        warnings: &[],
        program_line: None,
    });
    assert!(
        brief.contains("## Why the previous attempt was sent back"),
        "{brief}"
    );
    assert!(
        brief.contains("the retry loses the batch"),
        "the findings reach the new brief: {brief}"
    );

    // The second attempt fixes it and hands in; its review passes.
    std::fs::write(worktree.join("task-A.txt"), "one\ntwo\n").expect("write the fix");
    git_in(&worktree, &["add", "task-A.txt"]);
    git_in(&worktree, &["commit", "-m", "the fix"]);
    let fixed = git_in(&worktree, &["rev-parse", "HEAD"]);
    append(
        &fixture,
        Actor::Agent {
            id: String::from("claude"),
        },
        EventKind::Reported {
            task: String::from("A"),
            attempt: 2,
            status: ReportStatus::Completed,
            summary: String::from("fixed"),
            tests: Vec::new(),
            checks: vec![CheckReport {
                gate: String::from("tests"),
                command: String::from("true"),
                exit_code: Some(0),
                output_tail: String::new(),
            }],
            changed_paths: vec![String::from("task-A.txt")],
            commit: Some(fixed),
            skills_used: Vec::new(),
            proofs: Vec::new(),
        },
    );
    append(
        &fixture,
        Actor::Engine,
        EventKind::ReviewRequested {
            task: String::from("A"),
            attempt: 2,
            reviewer: String::from("codex"),
            degraded: false,
            note: None,
        },
    );
    write_findings_for_attempt(&fixture, "A", 2, "[]");

    let fourth = worker.tick().expect("the fourth pass ran");
    assert_eq!(fourth.steps.len(), 1, "{fourth:?}");
    assert_eq!(fourth.steps[0].action, "verdict", "{fourth:?}");
    assert_eq!(
        fixture.state().tasks.get("A").map(|task| task.status),
        Some(shprd_core::state::TaskStatus::Passed),
        "a clean review passes"
    );

    // The next pass lands it under `auto`, into the run's integration branch.
    let fifth = worker.tick().expect("the fifth pass ran");
    assert_eq!(fifth.steps.len(), 1, "{fifth:?}");
    assert_eq!(fifth.steps[0].action, "land", "{fifth:?}");
    assert!(fifth.steps[0].ok, "{fifth:?}");
    assert_eq!(
        fixture.state().tasks.get("A").map(|task| task.status),
        Some(shprd_core::state::TaskStatus::Integrated),
        "the loop closed"
    );
    let integration = layout.path(&repo_name, "r1", "_int");
    assert_eq!(
        git_in(&integration, &["show", "integration/m8:task-A.txt"]),
        "one\ntwo",
        "the task's work is on the integration branch"
    );

    // Nothing is left to do, and the run is quiet as it should be.
    let sixth = worker.tick().expect("the sixth pass ran");
    assert!(sixth.steps.is_empty(), "{sixth:?}");
}

#[test]
fn a_merge_the_forge_made_is_recorded_and_never_repeated() {
    let mut log = vec![started()];
    log.extend(ready(2, "abc123"));
    let fixture = Fixture::new(log);
    let forge: SharedForge = Arc::new(SharedFake::new(Vec::new()));
    let mut facts = Scripted::default();
    facts.prs.insert(
        String::from("A"),
        PrFacts {
            open: false,
            merged: true,
            checks: Some(true),
            head: Some(String::from("abc123")),
            number: Some(1),
        },
    );
    let calls = Arc::new(Mutex::new(Vec::new()));
    let worker = fixture.worker(
        Arc::new(facts),
        forge,
        config(shprd_core::config::MergePolicy::Ask),
        recording_start(Arc::clone(&calls)),
    );

    let report = worker.tick().expect("the pass ran");
    assert_eq!(report.steps.len(), 1, "{:?}", report.steps);
    let step = &report.steps[0];
    assert_eq!(step.action, "mark_landed");
    assert!(step.ok, "{step:?}");
    assert!(step.seq.is_some(), "the write is named: {step:?}");
    assert_eq!(
        fixture.state().tasks.get("A").map(|task| task.status),
        Some(shprd_core::state::TaskStatus::Integrated)
    );

    // The state after the pass holds the landing, so a second pass has nothing
    // to mark.
    let again = worker.tick().expect("the second pass ran");
    assert!(again.steps.is_empty(), "{:?}", again.steps);
}

#[test]
fn a_landing_goes_through_the_forge_and_the_ledger() {
    let mut log = vec![started()];
    log.extend(ready(2, "abc123"));
    let fixture = Fixture::new(log);
    // The task's attempt worktree is `/wt/A`, which does not exist; the landing
    // therefore fails at the git read, and is recorded as a failed step rather
    // than ending the pass. What it must not do is merge anything.
    let forge = Arc::new(SharedFake::new(vec![(
        Pr {
            number: 1,
            url: String::from("https://example.invalid/pull/1"),
            head: String::from("shprd/r1/A"),
        },
        PrStatus {
            approved: false,
            checks_passed: Some(true),
            merged: false,
            head_sha: String::from("abc123"),
        },
    )]));
    let facts = Arc::new(Scripted::default());
    let worker = fixture.worker(
        facts,
        forge.clone(),
        config(shprd_core::config::MergePolicy::Auto),
        recording_start(Arc::new(Mutex::new(Vec::new()))),
    );
    let report = worker.tick().expect("the pass ran");
    assert_eq!(report.steps.len(), 1, "{:?}", report.steps);
    let step: &StepReport = &report.steps[0];
    assert_eq!(step.action, "land");
    assert!(
        !step.ok,
        "a worktree that is not there is a failure: {step:?}"
    );
    assert!(
        forge.calls().iter().all(|call| !call.starts_with("merge ")),
        "nothing merged: {:?}",
        forge.calls()
    );
    assert_eq!(
        fixture.state().tasks.get("A").map(|task| task.status),
        Some(shprd_core::state::TaskStatus::Passed),
        "a failed landing writes nothing"
    );
}

#[test]
fn a_task_that_already_landed_is_never_landed_again() {
    // The chat-tail shape: the task landed, and every later pass must plan
    // nothing for it, open no pull request and record nothing.
    let mut log = vec![started()];
    log.extend(ready(2, "abc123"));
    let next = log.len() as u64 + 10;
    log.push(ev(
        next,
        EventKind::TaskLanded {
            task: String::from("A"),
            base: String::from("main"),
            head: String::from("abc123"),
            number: Some(1),
            outcome: shprd_core::event::LandOutcome::Merged,
        },
    ));
    let fixture = Fixture::new(log);
    let forge = Arc::new(SharedFake::new(vec![]));
    let worker = fixture.worker(
        Arc::new(Scripted::default()),
        forge.clone(),
        config(shprd_core::config::MergePolicy::Auto),
        recording_start(Arc::new(Mutex::new(Vec::new()))),
    );
    for _ in 0..3 {
        let report = worker.tick().expect("the pass ran");
        assert!(
            report.steps.iter().all(|step| step.action != "land"),
            "{:?}",
            report.steps
        );
    }
    assert!(forge.calls().is_empty(), "{:?}", forge.calls());
    assert_eq!(
        fixture
            .events()
            .iter()
            .filter(|e| matches!(e.kind, EventKind::TaskLanded { .. }))
            .count(),
        1
    );
}

#[test]
fn a_landing_closes_the_landed_tasks_worker_tab_and_no_others() {
    // A landed task's worker tab stayed open with its agent done (the M9 gate's
    // "also noticed"): nothing closed it, the way a verdict closes the reviewer's.
    // The closer is asked for the task that landed and for nobody else — a pass
    // that also starts another task leaves that task's tab alone.
    let mut log = vec![started()];
    log.extend(ready(2, "abc123"));
    log.push(added(7, "B"));
    let fixture = Fixture::new(log);
    let mut facts = Scripted::default();
    // The forge says the pull request is already merged: the pass records the
    // landing it made outside shprd, which is a landing all the same.
    facts.prs.insert(
        String::from("A"),
        PrFacts {
            open: false,
            merged: true,
            checks: Some(true),
            head: Some(String::from("abc123")),
            number: Some(1),
        },
    );
    let closed = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&closed);
    let close_worker: shprd_server::WorkerCloser = Arc::new(move |_repo: &Path, task: &str| {
        recorded
            .lock()
            .map_err(|_| String::from("the call log is poisoned"))?
            .push(task.to_string());
        Ok(())
    });
    let mut worker = fixture.worker(
        Arc::new(facts),
        Arc::new(NoForge),
        config(shprd_core::config::MergePolicy::Auto),
        recording_start(Arc::new(Mutex::new(Vec::new()))),
    );
    worker.close_worker = Some(close_worker);

    let report = worker.tick().expect("the pass ran");

    assert_eq!(
        report
            .steps
            .iter()
            .map(|step| format!("{}:{}", step.action, step.task))
            .collect::<Vec<_>>(),
        vec![String::from("mark_landed:A"), String::from("start:B")],
        "{report:?}"
    );
    assert_eq!(
        fixture.state().tasks.get("A").map(|task| task.status),
        Some(shprd_core::state::TaskStatus::Integrated),
        "A landed"
    );
    let called = closed.lock().expect("the call log");
    assert_eq!(
        *called,
        vec![String::from("A")],
        "only the landed task's tab is closed: B's is still in use"
    );
}

#[test]
fn a_failing_step_does_not_stop_the_loop() {
    // The loop's own property: a step that fails is logged and the next pass
    // runs anyway. Every start here fails, so the only thing that can make the
    // count grow is the loop carrying on past the failure.
    let fixture = Fixture::new(vec![started(), added(2, "A")]);
    let forge: SharedForge = Arc::new(NoForge);
    let facts = Arc::new(Scripted::default());
    let attempts = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&attempts);
    let start: shprd_server::Starter = Arc::new(move |_repo: &Path, _task: &str| {
        seen.fetch_add(1, Ordering::SeqCst);
        Err(String::from("no agent CLI is installed"))
    });
    let worker = Arc::new(fixture.worker(
        facts,
        forge,
        config(shprd_core::config::MergePolicy::Auto),
        start,
    ));
    let runtime = tokio::runtime::Runtime::new().expect("a runtime");
    runtime.block_on(async {
        let stop = async {
            tokio::time::sleep(Duration::from_millis(200)).await;
        };
        tick::run(worker, Duration::from_millis(10), None, stop).await;
    });
    assert!(
        attempts.load(Ordering::SeqCst) > 1,
        "the loop stopped on its first failing pass"
    );
}

#[test]
fn two_passes_never_overlap() {
    // A start that takes a while: without the single-flight permit two passes
    // would be inside it at once, and the counter below would see 2.
    let fixture = Fixture::new(vec![started(), added(2, "A")]);
    let forge: SharedForge = Arc::new(NoForge);
    let facts = Arc::new(Scripted::default());
    let inside = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&inside);
    let high = Arc::clone(&peak);
    let start: shprd_server::Starter = Arc::new(move |_repo: &Path, task: &str| {
        let now = seen.fetch_add(1, Ordering::SeqCst) + 1;
        high.fetch_max(now, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(60));
        seen.fetch_sub(1, Ordering::SeqCst);
        Ok(format!("started {task}"))
    });
    let worker = Arc::new(fixture.worker(
        facts,
        forge,
        config(shprd_core::config::MergePolicy::Auto),
        start,
    ));
    let runtime = tokio::runtime::Runtime::new().expect("a runtime");
    runtime.block_on(async {
        let stop = async {
            tokio::time::sleep(Duration::from_millis(400)).await;
        };
        tick::run(worker, Duration::from_millis(5), None, stop).await;
    });
    assert_eq!(
        peak.load(Ordering::SeqCst),
        1,
        "two passes ran their steps at the same time"
    );
}

#[test]
fn the_running_loop_takes_a_new_tick_interval_without_a_restart() {
    // Bug 4 of the M10 gate at the loop, not just the pass: the loop sleeps what
    // the last pass answered, so a `tasks.tick_secs` written while the service
    // is up changes how soon the next pass comes. The fixture's first passes are
    // a second apart; the settings then name one second, and the count over a
    // fixed wall-clock window has to fall.
    let fixture = Fixture::new(vec![started(), added(2, "A")]);
    let forge: SharedForge = Arc::new(NoForge);
    let attempts = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&attempts);
    let start: shprd_server::Starter = Arc::new(move |_repo: &Path, _task: &str| {
        seen.fetch_add(1, Ordering::SeqCst);
        Err(String::from("no agent CLI is installed"))
    });
    let worker = Arc::new(fixture.worker(
        Arc::new(Scripted::default()),
        forge,
        config(shprd_core::config::MergePolicy::Auto),
        start,
    ));
    // The person's answer, written before the loop starts: one second, not the
    // fifteen the service would have started with.
    fixture.settings("version = 1\n\n[tasks]\ntick_secs = 1\n");
    let runtime = tokio::runtime::Runtime::new().expect("a runtime");
    runtime.block_on(async {
        let stop = async {
            tokio::time::sleep(Duration::from_millis(2_500)).await;
        };
        // Started with a minute between passes: without reading the setting the
        // loop would run once and stop, so a count above one is the proof.
        tick::run(worker, Duration::from_secs(60), None, stop).await;
    });
    assert!(
        attempts.load(Ordering::SeqCst) > 1,
        "the loop kept the interval it started with instead of the setting: {} pass(es)",
        attempts.load(Ordering::SeqCst)
    );
}

/// A fallback host a test scripts: the facts it hands in and the calls it saw.
#[derive(Default)]
struct ScriptedFallback {
    /// The live agents and their cuts.
    agents: Vec<AgentFacts>,
    /// Every nudge the ladder sent.
    nudges: Arc<Mutex<Vec<String>>>,
    /// Every backup the ladder started.
    backups: Arc<Mutex<Vec<String>>>,
    /// Programs installed here.
    installed: BTreeSet<String>,
    /// Programs signed in here.
    signed_in: BTreeSet<String>,
}

impl FallbackSource for ScriptedFallback {
    fn agents(&self, _state: &RunState) -> Vec<AgentFacts> {
        self.agents.clone()
    }

    fn installed(&self) -> BTreeSet<String> {
        self.installed.clone()
    }

    fn signed_in(&self) -> BTreeSet<String> {
        self.signed_in.clone()
    }

    fn nudge(&self, task: &str, agent: &str, text: &str) -> Result<String, String> {
        self.nudges
            .lock()
            .map_err(|_| String::from("the call log is poisoned"))?
            .push(format!("{task}/{agent}: {text}"));
        Ok(format!("told {agent} to carry on"))
    }

    fn backup(
        &self,
        task: &str,
        attempt: u32,
        from: &str,
        to: &str,
        brief: &str,
    ) -> Result<String, String> {
        self.backups
            .lock()
            .map_err(|_| String::from("the call log is poisoned"))?
            .push(format!("{task} a{attempt} {from}->{to}: {brief}"));
        Ok(format!("started {to} on {task}"))
    }
}

/// The log of one task `A` with its first attempt spawned.
fn spawned_task() -> Fixture {
    Fixture::new(vec![
        started(),
        added(2, "A"),
        ev(
            3,
            EventKind::Spawned {
                task: String::from("A"),
                attempt: 1,
                agent: String::from("claude"),
                model: None,
                worktree: String::from("/wt/A"),
                branch: String::from("shprd/r1/A"),
            },
        ),
    ])
}

/// One live worker's facts with a cut `kind` seen `age_ms` ago.
fn cut_facts(kind: ErrorKind, tries: u32, age_ms: u64, role: Role) -> AgentFacts {
    AgentFacts {
        task: String::from("A"),
        attempt: 1,
        agent: String::from("claude"),
        role,
        working: false,
        cut: Some(Cut {
            id: String::from("cut-1"),
            kind,
            message: String::from("the provider said so"),
            at_ms: SystemClock.now_ms().saturating_sub(age_ms),
            resets_at_ms: None,
            tries,
        }),
        handover: Handover {
            title: String::from("task A"),
            where_: String::from("/wt/A"),
            branch: Some(String::from("shprd/r1/A")),
            commits: String::from("abc123 first"),
            changes: String::from(" M src/a.rs"),
            chat_tail: String::from("agent: I am halfway"),
        },
    }
}

/// A pass over one task whose worker's turn a provider cut off.
fn ladder_pass(source: ScriptedFallback) -> (Fixture, Worker, Arc<Mutex<Vec<String>>>) {
    let nudges = Arc::clone(&source.nudges);
    let backups = Arc::clone(&source.backups);
    let fixture = spawned_task();
    let mut config = config(shprd_core::config::MergePolicy::Ask);
    config.roster.workers = vec![String::from("claude"), String::from("codex")];
    let mut worker = fixture.worker(
        Arc::new(Scripted::default()),
        Arc::new(NoForge),
        config,
        recording_start(Arc::new(Mutex::new(Vec::new()))),
    );
    worker.fallback = Some(Arc::new(source));
    let _ = (nudges, backups);
    (fixture, worker, Arc::new(Mutex::new(Vec::new())))
}

#[test]
fn a_cut_is_nudged_once_when_its_wait_is_up() {
    let source = ScriptedFallback {
        agents: vec![cut_facts(ErrorKind::Overloaded, 1, 120_000, Role::Worker)],
        installed: BTreeSet::from([String::from("claude"), String::from("codex")]),
        signed_in: BTreeSet::from([String::from("claude"), String::from("codex")]),
        ..ScriptedFallback::default()
    };
    let nudges = Arc::clone(&source.nudges);
    let (fixture, worker, _) = ladder_pass(source);
    let report = worker.tick().expect("the pass ran");
    let step = report
        .steps
        .iter()
        .find(|step| step.action == "nudge")
        .expect("a nudge");
    assert!(step.ok, "{step:?}");
    assert_eq!(nudges.lock().unwrap().len(), 1);
    assert!(nudges.lock().unwrap()[0].contains("cut off by the model's provider"));
    let events = fixture.events();
    assert!(
        events
            .iter()
            .any(|event| matches!(event.kind, EventKind::Nudged { .. })),
        "the nudge is in the ledger"
    );

    // A second pass reads the ledger and sends nothing more for this cut.
    let report = worker.tick().expect("the second pass ran");
    assert!(
        report.steps.iter().all(|step| step.action != "nudge"),
        "a recorded nudge is not sent twice: {report:?}"
    );
    assert_eq!(nudges.lock().unwrap().len(), 1);
}

#[test]
fn the_worker_roles_fallback_list_is_the_ladder_it_walks() {
    let source = ScriptedFallback {
        agents: vec![cut_facts(ErrorKind::Quota, 1, 0, Role::Worker)],
        installed: BTreeSet::from([
            String::from("claude"),
            String::from("codex"),
            String::from("pi"),
        ]),
        signed_in: BTreeSet::from([
            String::from("claude"),
            String::from("codex"),
            String::from("pi"),
        ]),
        ..ScriptedFallback::default()
    };
    let backups = Arc::clone(&source.backups);
    let (fixture, worker, _) = ladder_pass(source);
    // The person named pi; the configured workers are claude and codex.
    let dir = fixture.home.join(".config").join("shprd");
    std::fs::create_dir_all(&dir).expect("the settings directory");
    std::fs::write(
        dir.join("settings.toml"),
        "[roles.worker]\nfallback = [\"pi\"]\n",
    )
    .expect("the settings file");
    let report = worker.tick().expect("the pass ran");
    assert!(
        report.steps.iter().any(|step| step.action == "backup"),
        "{report:?}"
    );
    assert!(
        backups.lock().unwrap()[0].contains("claude->pi"),
        "the role's list is walked before the configured workers: {:?}",
        backups.lock().unwrap()
    );
}

#[test]
fn a_cut_worker_moves_to_the_next_program_line() {
    let all = || {
        BTreeSet::from([
            String::from("claude"),
            String::from("codex"),
            String::from("pi"),
        ])
    };
    let source = ScriptedFallback {
        agents: vec![cut_facts(ErrorKind::Quota, 1, 0, Role::Worker)],
        installed: all(),
        signed_in: all(),
        ..ScriptedFallback::default()
    };
    let backups = Arc::clone(&source.backups);
    let (fixture, worker, _) = ladder_pass(source);
    // The worker's lines, in order: claude, then pi, then codex. The task runs
    // claude, so it moves to the next line (pi), not to the first installed one.
    let dir = fixture.home.join(".config").join("shprd");
    std::fs::create_dir_all(&dir).expect("the settings directory");
    std::fs::write(
        dir.join("settings.toml"),
        "[roles.worker]\nagent = \"claude\"\nagents = [\"pi\", \"codex\"]\n",
    )
    .expect("the settings file");
    let report = worker.tick().expect("the pass ran");
    assert!(
        report.steps.iter().any(|step| step.action == "backup"),
        "{report:?}"
    );
    assert!(
        backups.lock().unwrap()[0].contains("claude->pi"),
        "the next line takes over: {:?}",
        backups.lock().unwrap()
    );
}

#[test]
fn a_task_is_backed_up_once_and_never_to_its_own_program() {
    let source = ScriptedFallback {
        agents: vec![cut_facts(ErrorKind::Quota, 1, 0, Role::Worker)],
        installed: BTreeSet::from([String::from("claude"), String::from("codex")]),
        signed_in: BTreeSet::from([String::from("claude"), String::from("codex")]),
        ..ScriptedFallback::default()
    };
    let backups = Arc::clone(&source.backups);
    let (fixture, worker, _) = ladder_pass(source);
    let report = worker.tick().expect("the pass ran");
    let step = report
        .steps
        .iter()
        .find(|step| step.action == "backup")
        .expect("a backup");
    assert!(step.ok, "{step:?}");
    // The fallback list starts with the program already running, so codex
    // takes over.
    assert!(backups.lock().unwrap()[0].contains("claude->codex"));
    assert!(backups.lock().unwrap()[0].contains("<shprd-backup"));
    let events = fixture.events();
    let backup = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::Backup {
                from_agent,
                to_agent,
                worktree,
                branch,
                started,
                ..
            } => Some((
                from_agent.clone(),
                to_agent.clone(),
                worktree.clone(),
                branch.clone(),
                *started,
            )),
            _ => None,
        })
        .expect("the backup is in the ledger");
    assert_eq!(
        backup,
        (
            String::from("claude"),
            String::from("codex"),
            String::from("/wt/A"),
            String::from("shprd/r1/A"),
            true
        )
    );

    // One per task: the next pass does not hand over again.
    let report = worker.tick().expect("the second pass ran");
    assert!(
        report.steps.iter().all(|step| step.action != "backup"),
        "a task is backed up once: {report:?}"
    );
    assert_eq!(backups.lock().unwrap().len(), 1);
}

#[test]
fn a_reviewer_is_never_nudged() {
    let source = ScriptedFallback {
        agents: vec![cut_facts(ErrorKind::Overloaded, 1, 120_000, Role::Reviewer)],
        installed: BTreeSet::from([String::from("claude"), String::from("codex")]),
        signed_in: BTreeSet::from([String::from("claude"), String::from("codex")]),
        ..ScriptedFallback::default()
    };
    let nudges = Arc::clone(&source.nudges);
    let (_fixture, worker, _) = ladder_pass(source);
    let report = worker.tick().expect("the pass ran");
    assert!(
        report.steps.iter().all(|step| step.action != "nudge"),
        "{report:?}"
    );
    assert!(nudges.lock().unwrap().is_empty());
}

#[test]
fn a_cut_with_no_backup_asks_the_person_once() {
    let source = ScriptedFallback {
        agents: vec![cut_facts(ErrorKind::Quota, 1, 0, Role::Worker)],
        // The only fallback is the program already running.
        installed: BTreeSet::from([String::from("claude")]),
        signed_in: BTreeSet::from([String::from("claude")]),
        ..ScriptedFallback::default()
    };
    let backups = Arc::clone(&source.backups);
    let (fixture, worker, _) = ladder_pass(source);
    let report = worker.tick().expect("the pass ran");
    let step = report
        .steps
        .iter()
        .find(|step| step.action == "give_up")
        .expect("a give-up");
    assert!(step.ok, "{step:?}");
    assert!(backups.lock().unwrap().is_empty());
    let events = fixture.events();
    assert!(
        events
            .iter()
            .any(|event| matches!(event.kind, EventKind::ProviderGaveUp { .. }))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event.kind, EventKind::Escalated { .. }))
    );

    let report = worker.tick().expect("the second pass ran");
    assert!(
        report.steps.iter().all(|step| step.action != "give_up"),
        "a give-up is recorded once: {report:?}"
    );
}

/// Writes the findings file a reviewer for `task` attempt 1 wrote, backdated so
/// a pass trusts it without waiting out the settle window.
fn write_findings(fixture: &Fixture, task: &str, body: &str) {
    write_findings_for_attempt(fixture, task, 1, body);
}

/// [`write_findings`] for one named attempt.
fn write_findings_for_attempt(fixture: &Fixture, task: &str, attempt: u32, body: &str) {
    let reviews = fixture.home.join(".shprd").join("reviews");
    let path = shprd_core::review::findings_path(&reviews, "r1", task, attempt);
    std::fs::create_dir_all(path.parent().expect("a packet folder")).expect("mkdir the packet");
    std::fs::write(&path, body).expect("write the findings");
    let file = std::fs::File::options()
        .write(true)
        .open(&path)
        .expect("open the findings");
    file.set_modified(std::time::SystemTime::now() - Duration::from_secs(5))
        .expect("backdate the findings");
}

/// A fixture whose one task handed in and whose review was requested.
fn handed_in_and_reviewed() -> Fixture {
    let mut log = vec![started(), added(2, "A")];
    log.extend(handed_in(3));
    log.push(ev(
        5,
        EventKind::ReviewRequested {
            task: String::from("A"),
            attempt: 1,
            reviewer: String::from("codex"),
            degraded: false,
            note: None,
        },
    ));
    Fixture::new(log)
}

#[test]
fn a_reviewers_findings_file_is_decided_once_by_the_pass() {
    // Bug 1 of the M9 gate: the service only ever started reviewers, so a
    // service-only run sat at "In review" for ever. The pass decides the file
    // the reviewer wrote, through the same decision the verdict verb makes.
    let fixture = handed_in_and_reviewed();
    write_findings(&fixture, "A", "[]");
    let worker = reviewing(
        &fixture,
        Arc::new(|_repo: &Path, task: &str| Ok(format!("started a reviewer on {task}"))),
    );

    let first = worker.tick().expect("the pass ran");
    assert_eq!(first.steps.len(), 1, "{:?}", first.steps);
    assert_eq!(first.steps[0].action, "verdict", "{:?}", first.steps);
    assert!(first.steps[0].ok, "{:?}", first.steps);
    assert!(first.steps[0].seq.is_some(), "the write is named");
    let state = fixture.state();
    let task = state.tasks.get("A").expect("the task");
    assert_eq!(
        task.status,
        shprd_core::state::TaskStatus::Passed,
        "a clean review passes"
    );
    let gate = task
        .attempts
        .last()
        .expect("an attempt")
        .gates
        .iter()
        .find(|gate| gate.gate == "review")
        .expect("a review gate");
    assert!(gate.passed);
    let source = gate.source.as_ref().expect("the verdict is proven");
    assert_eq!(source.reviewer, "codex");
    assert!(source.request_seq > 0, "it answers the request");

    // The recorded gate is what stops the next pass deciding it again; what it
    // plans instead is the landing, which is the next step of the loop.
    let second = worker.tick().expect("the second pass ran");
    assert!(
        second.steps.iter().all(|step| step.action != "verdict"),
        "{:?}",
        second.steps
    );
    let gates = fixture
        .events()
        .iter()
        .filter(|event| {
            matches!(
                &event.kind,
                EventKind::GatePassed { task, gate, .. } | EventKind::GateFailed { task, gate, .. }
                    if task == "A" && gate == "review"
            )
        })
        .count();
    assert_eq!(gates, 1, "one verdict, not one per pass");
}

#[test]
fn a_blocking_finding_fails_the_review_and_sends_the_task_back() {
    let fixture = handed_in_and_reviewed();
    write_findings(
        &fixture,
        "A",
        r#"[{"severity":"critical","title":"it deletes the file","detail":"do not"}]"#,
    );
    let worker = reviewing(
        &fixture,
        Arc::new(|_repo: &Path, task: &str| Ok(format!("started a reviewer on {task}"))),
    );

    let report = worker.tick().expect("the pass ran");
    assert_eq!(report.steps.len(), 1, "{report:?}");
    assert_eq!(report.steps[0].action, "verdict");
    assert!(report.steps[0].ok, "{report:?}");
    let state = fixture.state();
    assert_eq!(
        state.tasks.get("A").map(|task| task.status),
        Some(shprd_core::state::TaskStatus::Failed),
        "a blocking finding fails the attempt"
    );
    assert!(
        state.tasks["A"]
            .attempts
            .last()
            .expect("an attempt")
            .gates
            .iter()
            .any(|gate| gate.gate == "review" && !gate.passed),
        "a failed review gate was recorded"
    );
    // Two rounds are allowed, so the first failure sends the task back without
    // asking a person.
    assert!(
        !state
            .escalations
            .iter()
            .any(|escalation| escalation.task.as_deref() == Some("A")),
        "one failed round is not an escalation"
    );
}

/// A review gate failure with one major finding, as the engine records it.
fn review_failed(seq: u64, id: &str, attempt: u32, request_seq: u64) -> Event {
    ev(
        seq,
        EventKind::GateFailed {
            task: id.to_string(),
            attempt,
            gate: String::from("review"),
            findings: vec![shprd_core::event::Finding {
                severity: shprd_core::event::Severity::Major,
                title: String::from("the retry loses the batch"),
                location: Some(String::from("src/calc.py:12")),
                detail: String::from("the second batch is dropped"),
            }],
            source: Some(ReviewSource {
                reviewer: String::from("codex"),
                findings_sha256: String::from(
                    "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
                ),
                request_seq,
            }),
        },
    )
}

/// A task whose review failed on attempt 1 and again on attempt 2: its two
/// rounds are spent, and nothing recorded the card the second failure owes.
fn two_spent_rounds() -> Fixture {
    let mut log = vec![started(), added(2, "A")];
    log.extend(handed_in(3));
    log.push(ev(
        5,
        EventKind::ReviewRequested {
            task: String::from("A"),
            attempt: 1,
            reviewer: String::from("codex"),
            degraded: false,
            note: None,
        },
    ));
    log.push(review_failed(6, "A", 1, 5));
    log.extend(handed_in_attempt(7, 2));
    log.push(ev(
        9,
        EventKind::ReviewRequested {
            task: String::from("A"),
            attempt: 2,
            reviewer: String::from("codex"),
            degraded: false,
            note: None,
        },
    ));
    log.push(review_failed(10, "A", 2, 9));
    Fixture::new(log)
}

#[test]
fn an_exhausted_review_puts_one_card_in_the_state() {
    // Bug 2 of the M9 gate, the second half: a review that spent its rounds must
    // ask the person under the service's own pass, with no coordinator to tell
    // and no orchestrator to type anything.
    let fixture = two_spent_rounds();
    let worker = fixture.worker(
        Arc::new(Scripted::default()),
        Arc::new(NoForge),
        config(shprd_core::config::MergePolicy::Auto),
        recording_start(Arc::new(Mutex::new(Vec::new()))),
    );

    let report = worker.tick().expect("the pass ran");
    assert_eq!(report.steps.len(), 1, "{report:?}");
    assert_eq!(report.steps[0].action, "escalate");
    assert!(report.steps[0].ok, "{report:?}");
    let seq = report.steps[0].seq.expect("the write is named");
    let event = fixture
        .events()
        .into_iter()
        .find(|event| event.seq == seq)
        .expect("the escalation is in the log");
    assert_eq!(event.actor, Actor::Engine, "the engine records the card");

    let state = fixture.state();
    let snapshot = shprd_server::dto::SnapshotDto::from_state(&state, 1_700_000_000_000);
    assert_eq!(snapshot.needs.len(), 1, "{:?}", snapshot.needs);
    let card = &snapshot.needs[0];
    assert_eq!(card.task.as_deref(), Some("A"));
    assert!(
        card.body.contains("the retry loses the batch"),
        "the card says what failed: {}",
        card.body
    );
    for choice in ["retry with another round", "take it as it is", "drop it"] {
        assert!(card.body.contains(choice), "{}", card.body);
    }

    // One card per failure: a second pass says nothing and adds nothing.
    let again = worker.tick().expect("the second pass ran");
    assert!(again.steps.is_empty(), "{again:?}");
    let state = fixture.state();
    let snapshot = shprd_server::dto::SnapshotDto::from_state(&state, 1_700_000_000_000);
    assert_eq!(snapshot.needs.len(), 1, "{:?}", snapshot.needs);
}

#[test]
fn a_retry_answer_makes_the_pass_send_the_task_back() {
    // The card's first choice is answered through the existing API —
    // `POST /api/asks/answer` records this `AskAnswered` — and the pass honours
    // it: one more round, spent by sending the task back.
    let fixture = two_spent_rounds();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&calls);
    let start: shprd_server::Starter = Arc::new(move |repo: &Path, task: &str| {
        seen.lock()
            .map_err(|_| String::from("the call log is poisoned"))?
            .push(task.to_string());
        let path = repo.join(".shprd").join("events.jsonl");
        let mut log = EventLog::open(&path).map_err(|err| err.to_string())?;
        let attempt = RunState::fold(
            &EventLog::read_all(&path)
                .map_err(|err| err.to_string())?
                .events,
        )
        .map_err(|err| err.to_string())?
        .tasks
        .get(task)
        .map(|found| found.attempts.len() as u32 + 1)
        .unwrap_or(1);
        log.append(
            Actor::Engine,
            EventKind::Spawned {
                task: task.to_string(),
                attempt,
                agent: String::from("claude"),
                model: None,
                worktree: String::from("/wt/A"),
                branch: String::from("shprd/r1/A"),
            },
            &SystemClock,
        )
        .map_err(|err| err.to_string())?;
        Ok(format!("started {task} attempt {attempt}"))
    });
    let worker = fixture.worker(
        Arc::new(Scripted::default()),
        Arc::new(NoForge),
        config(shprd_core::config::MergePolicy::Auto),
        start,
    );

    let first = worker.tick().expect("the first pass ran");
    assert_eq!(first.steps[0].action, "escalate", "{first:?}");

    append(
        &fixture,
        Actor::Human,
        EventKind::AskAnswered {
            task: String::from("A"),
            ask: shprd_core::gate::rounds_spent_ask(2),
            answer: String::from(shprd_core::gate::RETRY_CHOICE),
        },
    );

    let second = worker.tick().expect("the second pass ran");
    assert_eq!(second.steps.len(), 1, "{second:?}");
    assert_eq!(second.steps[0].action, "send_back", "{second:?}");
    assert_eq!(*calls.lock().expect("the call log"), vec!["A".to_string()]);

    // The card was about attempt 2, and the task is on attempt 3: the board
    // stops showing it, and the choices with it.
    let state = fixture.state();
    let snapshot = shprd_server::dto::SnapshotDto::from_state(&state, 1_700_000_000_000);
    assert!(snapshot.needs.is_empty(), "{:?}", snapshot.needs);
    assert!(snapshot.asks.is_empty(), "{:?}", snapshot.asks);
}

#[test]
fn a_review_verdict_that_spends_the_rounds_leaves_one_card() {
    // The same card when the verdict is the pass's own decision: `gate::decide`
    // records it, and the pass must not add a second one beside it.
    let fixture = handed_in_and_reviewed();
    write_findings(
        &fixture,
        "A",
        r#"[{"severity":"critical","title":"it deletes the file","detail":"do not"}]"#,
    );
    let worker = reviewing(
        &fixture,
        Arc::new(|_repo: &Path, task: &str| Ok(format!("started a reviewer on {task}"))),
    );

    // The first failing round is a send-back: no card yet.
    let first = worker.tick().expect("the pass ran");
    assert_eq!(first.steps[0].action, "verdict", "{first:?}");
    let state = fixture.state();
    let snapshot = shprd_server::dto::SnapshotDto::from_state(&state, 1_700_000_000_000);
    assert!(
        snapshot.needs.is_empty(),
        "one round of two is not a card: {:?}",
        snapshot.needs
    );

    // The second attempt fails its review as well: the rounds are spent.
    let path = fixture.repo.join(".shprd").join("events.jsonl");
    let mut log = EventLog::open(&path).expect("open the log");
    log.append(
        Actor::Engine,
        EventKind::Spawned {
            task: String::from("A"),
            attempt: 2,
            agent: String::from("claude"),
            model: None,
            worktree: String::from("/wt/A"),
            branch: String::from("shprd/r1/A"),
        },
        &SystemClock,
    )
    .expect("attempt 2 spawns");
    log.append(
        Actor::Agent {
            id: String::from("claude"),
        },
        EventKind::Reported {
            task: String::from("A"),
            attempt: 2,
            status: ReportStatus::Completed,
            summary: String::from("done"),
            tests: Vec::new(),
            checks: vec![CheckReport {
                gate: String::from("tests"),
                command: String::from("cargo test"),
                exit_code: Some(0),
                output_tail: String::new(),
            }],
            changed_paths: Vec::new(),
            commit: Some(String::from("abc123")),
            skills_used: Vec::new(),
            proofs: Vec::new(),
        },
        &SystemClock,
    )
    .expect("attempt 2 hands in");
    log.append(
        Actor::Engine,
        EventKind::ReviewRequested {
            task: String::from("A"),
            attempt: 2,
            reviewer: String::from("codex"),
            degraded: false,
            note: None,
        },
        &SystemClock,
    )
    .expect("attempt 2 is reviewed");
    write_findings_for_attempt(
        &fixture,
        "A",
        2,
        r#"[{"severity":"critical","title":"it deletes the file","detail":"do not"}]"#,
    );

    let second = worker.tick().expect("the second pass ran");
    assert_eq!(second.steps.len(), 1, "{second:?}");
    assert_eq!(second.steps[0].action, "verdict", "{second:?}");
    let state = fixture.state();
    let snapshot = shprd_server::dto::SnapshotDto::from_state(&state, 1_700_000_000_000);
    assert_eq!(
        snapshot.needs.len(),
        1,
        "the last round asks once: {:?}",
        snapshot.needs
    );

    // The next pass makes the card answerable: the choices are an ask a board
    // answers through `POST /api/asks/answer`, and the card itself is not
    // written a second time.
    let third = worker.tick().expect("the third pass ran");
    assert_eq!(third.steps.len(), 1, "{third:?}");
    assert_eq!(third.steps[0].action, "escalate", "{third:?}");
    let state = fixture.state();
    let snapshot = shprd_server::dto::SnapshotDto::from_state(&state, 1_700_000_000_000);
    assert_eq!(snapshot.needs.len(), 1, "one card: {:?}", snapshot.needs);
    assert_eq!(
        snapshot.asks.len(),
        1,
        "one answerable card: {:?}",
        snapshot.asks
    );
    assert_eq!(
        snapshot.asks[0].options,
        vec![
            String::from("retry with another round"),
            String::from("take it as it is"),
            String::from("drop it")
        ],
        "the card offers the person's choices"
    );
    let escalations = fixture
        .events()
        .into_iter()
        .filter(|event| matches!(event.kind, EventKind::Escalated { .. }))
        .count();
    assert_eq!(
        escalations, 1,
        "the card is written once, not once per pass"
    );

    // And then the run is as far as it goes: the decision is the person's.
    let fourth = worker.tick().expect("the fourth pass ran");
    assert!(fourth.steps.is_empty(), "{fourth:?}");
    let state = fixture.state();
    let snapshot = shprd_server::dto::SnapshotDto::from_state(&state, 1_700_000_000_000);
    assert_eq!(snapshot.needs.len(), 1, "{:?}", snapshot.needs);
    assert_eq!(snapshot.asks.len(), 1, "{:?}", snapshot.asks);
}

#[test]
fn an_unreadable_findings_file_is_escalated_once_and_left_to_a_person() {
    let fixture = handed_in_and_reviewed();
    write_findings(
        &fixture,
        "A",
        "this is not the JSON array the packet asks for",
    );
    let worker = reviewing(
        &fixture,
        Arc::new(|_repo: &Path, task: &str| Ok(format!("started a reviewer on {task}"))),
    );

    let first = worker.tick().expect("the pass ran");
    assert_eq!(first.steps.len(), 1, "{first:?}");
    assert_eq!(first.steps[0].action, "verdict");
    assert!(first.steps[0].ok, "the escalation is the step's work");
    let recorded = escalations(&fixture);
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    assert!(
        recorded[0]
            .1
            .starts_with("A: the reviewer's findings could not be read: "),
        "{recorded:?}"
    );
    assert_eq!(
        recorded[0].2,
        shprd_core::review::INVALID_FINDINGS_ACTION,
        "the person is told what to do"
    );

    // The escalation is the record: a person has been told, so the pass stops
    // deciding a file it cannot read.
    let second = worker.tick().expect("the second pass ran");
    assert!(second.steps.is_empty(), "{:?}", second.steps);
    assert_eq!(escalations(&fixture).len(), 1, "said once");
}

#[test]
fn the_merge_policy_is_read_from_the_settings_on_every_pass() {
    // Bug 2 and bug 6 of the M9 gate: `settings set tasks.merge auto` is what
    // the brief prints, and a change to it must apply on the next pass of the
    // running service, not on its next restart.
    let mut log = vec![started()];
    log.extend(ready(2, "abc123"));
    let fixture = Fixture::new(log);
    let forge = Arc::new(SharedFake::new(vec![(
        Pr {
            number: 1,
            url: String::from("https://example.invalid/pull/1"),
            head: String::from("shprd/r1/A"),
        },
        PrStatus {
            approved: false,
            checks_passed: Some(true),
            merged: false,
            head_sha: String::from("abc123"),
        },
    )]));
    // The file layers say `ask`; the settings are the person's newer answer.
    let worker = Fixture::worker(
        &fixture,
        Arc::new(Scripted::default()),
        forge,
        config(shprd_core::config::MergePolicy::Ask),
        recording_start(Arc::new(Mutex::new(Vec::new()))),
    );
    fixture.settings("version = 1\n\n[tasks]\nmerge = \"auto\"\n");

    let auto = worker.tick().expect("the pass ran");
    assert_eq!(auto.steps.len(), 1, "{auto:?}");
    assert_eq!(
        auto.steps[0].action, "land",
        "the setting, not the file layer, decides: {auto:?}"
    );

    // The very next pass of the same worker under `ask` asks instead.
    fixture.settings("version = 1\n\n[tasks]\nmerge = \"ask\"\n");
    let ask = worker.tick().expect("the second pass ran");
    assert_eq!(ask.steps.len(), 1, "{ask:?}");
    assert_eq!(
        ask.steps[0].action, "needs_you",
        "a settings change applies without a restart: {ask:?}"
    );
}

#[test]
fn the_concurrency_cap_is_read_from_the_settings_on_every_pass() {
    let fixture = Fixture::new(vec![started(), added(2, "A"), added(3, "B")]);
    let forge: SharedForge = Arc::new(NoForge);
    let calls = Arc::new(Mutex::new(Vec::new()));
    let worker = Fixture::worker(
        &fixture,
        Arc::new(Scripted::default()),
        forge,
        config(shprd_core::config::MergePolicy::Auto),
        recording_start(Arc::clone(&calls)),
    );
    // One slot: only the first task by id starts. The closure records the call
    // but not the attempt, so both tasks are still queued on the next pass.
    fixture.settings("version = 1\n\n[tasks]\nmax_running = 1\n");
    let first = worker.tick().expect("the pass ran");
    assert_eq!(first.steps.len(), 1, "{first:?}");
    assert_eq!(first.steps[0].task, "A");

    // Two slots apply on the next pass with no restart.
    fixture.settings("version = 1\n\n[tasks]\nmax_running = 2\n");
    let second = worker.tick().expect("the second pass ran");
    assert_eq!(second.steps.len(), 2, "{second:?}");
}

#[test]
fn the_tick_interval_is_read_from_the_settings_on_every_pass() {
    // Bug 4 of the M10 gate: `settings set tasks.tick_secs 10` was written and
    // shown, and the running service kept its 15 s. The settings are re-read per
    // pass and the pass answers the interval it read, so the loop sleeps the new
    // one with no restart.
    let fixture = Fixture::new(vec![started(), added(2, "A")]);
    let forge: SharedForge = Arc::new(NoForge);
    let worker = Fixture::worker(
        &fixture,
        Arc::new(Scripted::default()),
        forge,
        config(shprd_core::config::MergePolicy::Auto),
        recording_start(Arc::new(Mutex::new(Vec::new()))),
    );
    let started_with = Duration::from_secs(20);

    // Nothing names the key: the pass answers none, so the loop keeps the
    // interval it was started with.
    let unset = worker.tick().expect("the pass ran");
    assert_eq!(unset.interval_secs, None, "nobody set it: {unset:?}");
    assert_eq!(unset.next_interval(started_with), started_with);

    // The person's setting is picked up by the same worker's next pass.
    fixture.settings("version = 1\n\n[tasks]\ntick_secs = 10\n");
    let ten = worker.tick().expect("the second pass ran");
    assert_eq!(
        ten.interval_secs,
        Some(10),
        "the setting is read per pass: {ten:?}"
    );
    assert_eq!(ten.next_interval(started_with), Duration::from_secs(10));

    // And a second change is picked up the same way.
    fixture.settings("version = 1\n\n[tasks]\ntick_secs = 3\n");
    let three = worker.tick().expect("the third pass ran");
    assert_eq!(three.next_interval(started_with), Duration::from_secs(3));

    // The older `[tick] interval_secs` spelling reaches the same key through
    // the settings' legacy layer, read from the person's pre-M9 `config.toml`
    // rather than `settings.toml`, so a run that never migrated still moves.
    std::fs::remove_file(
        fixture
            .home
            .join(".config")
            .join("shprd")
            .join("settings.toml"),
    )
    .expect("the settings file goes");
    std::fs::write(
        fixture
            .home
            .join(".config")
            .join("shprd")
            .join("config.toml"),
        "[tick]\ninterval_secs = 7\n",
    )
    .expect("write the old config");
    let seven = worker.tick().expect("the fourth pass ran");
    assert_eq!(
        seven.next_interval(started_with),
        Duration::from_secs(7),
        "the old config key is read too"
    );
}

/// A facts source that lets another decider win the race for the verdict.
///
/// `live` is asked once per pass, after the log was folded and before the plan
/// is made — exactly the window a `shprd watch` runs in. This one records the
/// review's verdict itself, so the pass plans a verdict step from a state that
/// is already stale.
struct Racing {
    /// The repository whose log the competing verdict is appended to.
    repo: PathBuf,
    /// Every call `live` made, so a test can assert the race really happened.
    raced: Arc<Mutex<usize>>,
}

impl FactsSource for Racing {
    fn pr(&self, _task: &shprd_core::state::Task, _state: &RunState) -> Option<PrFacts> {
        None
    }

    fn live(&self, state: &RunState) -> BTreeSet<String> {
        let task = state.tasks.get("A").expect("the fixture's task");
        let request = task
            .attempts
            .last()
            .and_then(|attempt| attempt.reviews.last())
            .expect("the fixture requested a review");
        let path = self.repo.join(".shprd").join("events.jsonl");
        let mut log = EventLog::open(&path).expect("open the log");
        log.append(
            Actor::Engine,
            EventKind::GatePassed {
                task: String::from("A"),
                attempt: 1,
                gate: String::from("review"),
                findings: Vec::new(),
                source: Some(ReviewSource {
                    reviewer: request.reviewer.clone(),
                    findings_sha256: shprd_core::hash::sha256_hex(b"[]"),
                    request_seq: request.seq,
                }),
            },
            &SystemClock,
        )
        .expect("the competing verdict is recorded");
        *self.raced.lock().expect("the race counter") += 1;
        BTreeSet::new()
    }
}

#[test]
fn a_verdict_another_decider_already_recorded_is_not_recorded_twice() {
    // Bug found by the M9 gate's reviewer: a `shprd watch` racing the service
    // could fold one reviewer's packet into two gate results, because both plan
    // from a state that says the review is owed and a second `gate_passed`
    // answering the same request used to be accepted. The log refuses it now,
    // and the pass that loses says the review is already decided.
    let fixture = handed_in_and_reviewed();
    write_findings(&fixture, "A", "[]");
    let raced = Arc::new(Mutex::new(0));
    let facts: Arc<dyn FactsSource> = Arc::new(Racing {
        repo: fixture.repo.clone(),
        raced: Arc::clone(&raced),
    });
    let mut worker = reviewing(
        &fixture,
        Arc::new(|_repo: &Path, task: &str| Ok(format!("started a reviewer on {task}"))),
    );
    worker.facts = facts;

    let report = worker.tick().expect("the pass ran");

    assert_eq!(*raced.lock().expect("the race counter"), 1, "the race ran");
    assert_eq!(report.steps.len(), 1, "{:?}", report.steps);
    assert_eq!(report.steps[0].action, "verdict");
    assert!(
        report.steps[0].ok,
        "losing the race is not a failure: {:?}",
        report.steps
    );
    assert!(
        report.steps[0].detail.contains("already decided"),
        "{:?}",
        report.steps
    );
    let verdicts = fixture
        .events()
        .iter()
        .filter(|event| {
            matches!(
                &event.kind,
                EventKind::GatePassed { task, gate, .. } | EventKind::GateFailed { task, gate, .. }
                    if task == "A" && gate == "review"
            )
        })
        .count();
    assert_eq!(verdicts, 1, "one review, one verdict");
}

/// A task worktree an older build made has no `info/exclude` entries, so the
/// worker's brief would claim a protection that is not there. The pass repairs
/// every live worktree it finds, before it plans anything.
#[test]
fn a_pass_excludes_shprds_state_in_a_worktree_made_before_the_change() {
    use shprd_core::event::ResourceKind;

    // A real repository, because the repair drives git.
    let temp = TempDir::new().expect("temp dir");
    let repo = temp.path().join("repo");
    let home = temp.path().join("home");
    std::fs::create_dir_all(&repo).expect("mkdir the repo");
    std::fs::create_dir_all(&home).expect("mkdir the home");
    let git = |args: &[&str], dir: &Path| {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git runs");
        assert!(output.status.success(), "git {args:?}: {output:?}");
    };
    git(&["init", "-q", "-b", "main"], &repo);
    git(&["config", "user.name", "shprd tests"], &repo);
    git(&["config", "user.email", "tests@example.invalid"], &repo);
    std::fs::write(repo.join("a.txt"), "one\n").expect("a seed file");
    git(&["add", "."], &repo);
    git(&["commit", "-q", "-m", "seed"], &repo);

    // A worktree as git makes one, with no exclusions: what an older shprd left.
    let worktree = home.join(".shprd/wt/repo/r1/T1");
    std::fs::create_dir_all(worktree.parent().expect("a parent")).expect("mkdir the wt root");
    let text = worktree.display().to_string();
    git(
        &["worktree", "add", "-q", "-b", "shprd/r1/T1", &text, "main"],
        &repo,
    );
    let exclude = repo.join(".git/info/exclude");
    std::fs::write(&exclude, "").expect("an empty exclude file");

    // The log names the worktree as live, the way a spawn records one.
    let events = vec![
        started(),
        added(2, "T1"),
        ev(
            3,
            EventKind::Resource {
                action: shprd_core::event::ResourceAction::Created,
                kind: ResourceKind::Worktree,
                name: text.clone(),
                task: Some(String::from("T1")),
            },
        ),
    ];
    let fixture = Fixture {
        _temp: temp,
        repo: repo.clone(),
        home: home.clone(),
    };
    let mut log = EventLog::open(&fixture.repo.join(".shprd/events.jsonl")).expect("open the log");
    let clock = SystemClock;
    for event in events {
        log.append(event.actor, event.kind, &clock)
            .expect("append the fixture");
    }
    let forge: SharedForge = Arc::new(NoForge);
    let worker = fixture.worker(
        Arc::new(Scripted::default()),
        forge,
        Config::default(),
        recording_start(Arc::new(Mutex::new(Vec::new()))),
    );
    worker.tick().expect("the pass ran");

    let text = std::fs::read_to_string(&exclude).expect("the exclude file");
    for pattern in ["/.shprd/", "/.dagr/"] {
        assert!(
            text.lines().any(|line| line.trim() == pattern),
            "{pattern} was not repaired into:\n{text}"
        );
    }
    assert!(
        shprd_git::git::Git::new(&worktree)
            .is_clean()
            .expect("git status"),
        "the repaired worktree still reports shprd's state"
    );
}

/// A task that handed in and whose reviewer was asked, with no verdict since.
fn reviewer_out() -> Fixture {
    let mut log = vec![started(), added(2, "A")];
    log.extend(handed_in(3));
    log.push(ev(
        5,
        EventKind::ReviewRequested {
            task: String::from("A"),
            attempt: 1,
            reviewer: String::from("opencode"),
            degraded: false,
            note: None,
        },
    ));
    Fixture::new(log)
}

#[test]
fn a_review_out_too_long_becomes_one_card_with_a_review_again_choice() {
    let fixture = reviewer_out();
    let worker = reviewing(
        &fixture,
        Arc::new(|_repo: &Path, task: &str| Ok(format!("started a reviewer on {task}"))),
    );
    let now = SystemClock.now_ms();

    // Not yet: the reviewer was asked a moment ago.
    worker.watch_reviews(now);
    assert!(fixture.state().asks.is_empty());

    // Half an hour on, with no verdict, a person is asked — once.
    let later = now + shprd_core::review::STUCK_AFTER_MS + 1;
    worker.watch_reviews(later);
    worker.watch_reviews(later);
    let state = fixture.state();
    assert_eq!(state.asks.len(), 1, "{:?}", state.asks);
    let ask = state.asks.values().next().expect("the card");
    assert_eq!(ask.id, shprd_core::review::stuck_ask(1, 1));
    assert!(ask.text.contains("opencode"), "{}", ask.text);
    assert!(
        ask.options
            .iter()
            .any(|choice| choice == shprd_core::review::REVIEW_AGAIN_CHOICE),
        "{:?}",
        ask.options
    );
    let snapshot = shprd_server::dto::SnapshotDto::from_state(&state, later);
    assert_eq!(snapshot.asks.len(), 1, "the board shows the card");

    // `review again` records a new request; a second silence is a new card.
    append(
        &fixture,
        Actor::Engine,
        EventKind::AskAnswered {
            task: String::from("A"),
            ask: shprd_core::review::stuck_ask(1, 1),
            answer: String::from(shprd_core::review::REVIEW_AGAIN_CHOICE),
        },
    );
    append(
        &fixture,
        Actor::Engine,
        EventKind::ReviewRequested {
            task: String::from("A"),
            attempt: 1,
            reviewer: String::from("opencode"),
            degraded: false,
            note: None,
        },
    );
    worker.watch_reviews(later + shprd_core::review::STUCK_AFTER_MS + 1);
    assert_eq!(fixture.state().asks.len(), 2);
}

#[test]
fn a_review_with_no_reviewer_left_is_one_plain_card_asked_again_once_answered() {
    let fixture = reviewer_out();
    let mut worker = reviewing(
        &fixture,
        Arc::new(|_repo: &Path, task: &str| Ok(format!("started a reviewer on {task}"))),
    );
    worker.switch_reviewers = Some(Arc::new(|_repo: &Path| {
        vec![shprd_core::review::ReviewSwitch::NoneLeft {
            task: String::from("A"),
            attempt: 1,
            why: String::from("codex is out of quota until Dec 18"),
        }]
    }));
    let now = SystemClock.now_ms();

    // At once, not after half an hour, and only once while it waits.
    worker.watch_reviews(now);
    worker.watch_reviews(now);
    let state = fixture.state();
    assert_eq!(state.asks.len(), 1, "{:?}", state.asks);
    let ask = state.asks.values().next().expect("the card");
    assert_eq!(ask.id, shprd_core::review::stuck_ask(1, 1));
    assert!(ask.text.contains("no reviewer can review"), "{}", ask.text);
    assert!(
        ask.text.contains("codex is out of quota until Dec 18"),
        "{}",
        ask.text
    );
    assert!(!ask.text.contains("shprd "), "no commands: {}", ask.text);

    // Answered, and the retry was refused again: the person is asked again.
    append(
        &fixture,
        Actor::Engine,
        EventKind::AskAnswered {
            task: String::from("A"),
            ask: shprd_core::review::stuck_ask(1, 1),
            answer: String::from(shprd_core::review::REVIEW_AGAIN_CHOICE),
        },
    );
    worker.watch_reviews(now);
    assert_eq!(fixture.state().asks.len(), 2);
}

/// A task added with an explicit scope.
fn added_scoped(seq: u64, id: &str, scope: &str) -> Event {
    ev(
        seq,
        EventKind::TaskAdded {
            task: id.to_string(),
            title: format!("task {id}"),
            scope: vec![scope.to_string()],
            acceptance: vec![String::from("it works")],
            tags: Vec::new(),
            skills: Vec::new(),
            deps: Vec::new(),
        },
    )
}

/// `id` spawned at attempt 1, so it is Working.
fn spawned_ev(seq: u64, id: &str) -> Event {
    ev(
        seq,
        EventKind::Spawned {
            task: id.to_string(),
            attempt: 1,
            agent: String::from("claude"),
            model: None,
            worktree: format!("/wt/{id}"),
            branch: format!("shprd/r1/{id}"),
        },
    )
}

#[test]
fn a_task_sharing_a_running_tasks_paths_starts_and_its_old_wait_line_clears() {
    // Declared paths are a hint, not a lock: T4 names the file the running T1
    // names, and starts. The "in use by" line an older build left on its card
    // is cleared on the pass.
    let fixture = Fixture::new(vec![
        started(),
        added_scoped(2, "T1", "calc/ops.py"),
        spawned_ev(3, "T1"),
        added_scoped(4, "T4", "calc/ops.py"),
        added_scoped(5, "T5", "calc"),
        ev(
            6,
            EventKind::TaskStatus {
                task: "T5".to_string(),
                message: "Waiting: calc is in use by T1".to_string(),
                progress: None,
            },
        ),
    ]);
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut cfg = config(shprd_core::config::MergePolicy::Auto);
    // One slot (T1 has no live agent here, so it holds none): T4 takes it, and
    // T5 stays queued at the cap.
    cfg.tick.max_running = 1;
    let worker = fixture.worker(
        Arc::new(Scripted::default()),
        Arc::new(NoForge),
        cfg,
        recording_start(Arc::clone(&calls)),
    );
    let report = worker.tick().expect("the pass ran");
    assert!(report.ok(), "{:?}", report.steps);
    assert_eq!(*calls.lock().expect("calls"), vec![String::from("T4")]);
    let state = fixture.state();
    assert!(state.escalations.is_empty(), "{:?}", state.escalations);
    let status = state.tasks["T5"]
        .last_status
        .as_ref()
        .expect("a status line");
    assert_eq!(status.message, "No longer waiting");
}

#[test]
fn overlapping_tasks_all_start_in_one_pass() {
    let fixture = Fixture::new(vec![
        started(),
        added_scoped(2, "T1", "calc/ops.py"),
        spawned_ev(3, "T1"),
        added_scoped(4, "T4", "calc/ops.py"),
        added_scoped(5, "T5", "README.md"),
        added_scoped(6, "T6", "docs"),
        added_scoped(7, "T7", "README.md"),
    ]);
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut cfg = config(shprd_core::config::MergePolicy::Auto);
    cfg.tick.max_running = 5;
    let worker = fixture.worker(
        Arc::new(Scripted::default()),
        Arc::new(NoForge),
        cfg,
        recording_start(Arc::clone(&calls)),
    );
    let report = worker.tick().expect("the pass ran");
    assert!(report.ok(), "{:?}", report.steps);
    assert_eq!(
        *calls.lock().expect("calls"),
        vec![
            String::from("T4"),
            String::from("T5"),
            String::from("T6"),
            String::from("T7")
        ]
    );
}

/// A repository, an `origin` it pushes to, and two task branches in their own
/// worktrees, as a forge landing sees them: `shprd/r1/A` modifies `proof.log`,
/// which `main` has since deleted (a modify/delete clash), and `shprd/r1/B`
/// adds a file of its own and merges cleanly.
///
/// Returns the worktrees and heads of A and B.
fn clashing_and_clean(fixture: &Fixture) -> [(PathBuf, String); 2] {
    let origin = fixture.home.join("origin.git");
    std::fs::create_dir_all(&origin).expect("mkdir origin");
    git_in(&origin, &["init", "--bare", "-b", "main"]);
    git_in(&fixture.repo, &["init", "-b", "main"]);
    std::fs::write(fixture.repo.join("proof.log"), "numbers\n").expect("write proof");
    git_in(&fixture.repo, &["add", "proof.log"]);
    git_in(&fixture.repo, &["commit", "-m", "seed"]);
    git_in(
        &fixture.repo,
        &["remote", "add", "origin", &origin.display().to_string()],
    );
    git_in(&fixture.repo, &["push", "origin", "main"]);
    let mut out = Vec::new();
    for (id, file, text) in [("A", "proof.log", "changed\n"), ("B", "b.txt", "b\n")] {
        let tree = fixture.home.join(format!("wt-{id}"));
        git_in(
            &fixture.repo,
            &[
                "worktree",
                "add",
                "-b",
                &format!("shprd/r1/{id}"),
                &tree.display().to_string(),
                "main",
            ],
        );
        std::fs::write(tree.join(file), text).expect("write the work");
        git_in(&tree, &["add", file]);
        git_in(&tree, &["commit", "-m", &format!("work of {id}")]);
        let head = git_in(&tree, &["rev-parse", "HEAD"]);
        out.push((tree, head));
    }
    // Work that landed after both were cut: `proof.log` is gone.
    git_in(&fixture.repo, &["rm", "proof.log"]);
    git_in(&fixture.repo, &["commit", "-m", "drop proof.log"]);
    git_in(&fixture.repo, &["push", "origin", "main"]);
    let b = out.pop().expect("b");
    let a = out.pop().expect("a");
    [a, b]
}

/// A forge that holds one open pull request per task, each at its branch tip.
fn forge_with(tasks: &[(&str, u64, &str)]) -> Arc<SharedFake> {
    Arc::new(SharedFake::new(
        tasks
            .iter()
            .map(|(id, number, head)| {
                (
                    Pr {
                        number: *number,
                        url: format!("https://example.invalid/pull/{number}"),
                        head: format!("shprd/r1/{id}"),
                    },
                    PrStatus {
                        approved: false,
                        checks_passed: Some(true),
                        merged: false,
                        head_sha: (*head).to_string(),
                    },
                )
            })
            .collect(),
    ))
}

#[test]
fn a_clash_found_while_landing_sends_the_task_back_and_never_blocks_the_next_task() {
    // The live case: a modify/delete clash, the branch checked out in its
    // worktree, a forge landing. The clash is recorded as the failed landing
    // gate the planner sends back on (with the file named), the stale pull
    // request is closed, and an approved task that merges cleanly lands in the
    // very same pass: one task's failed landing never starves another's.
    let fixture = Fixture::new(vec![started()]);
    let [(tree_a, head_a), (tree_b, head_b)] = clashing_and_clean(&fixture);
    let mut log = Vec::new();
    log.extend(ready_task(
        2,
        "A",
        &head_a,
        &tree_a.display().to_string(),
        "shprd/r1/A",
        1,
    ));
    // Sequence numbers must match the log's own: a review's request is named by
    // its position, and A's six events fill 2..=7.
    log.extend(ready_task(
        8,
        "B",
        &head_b,
        &tree_b.display().to_string(),
        "shprd/r1/B",
        2,
    ));
    for event in log {
        append(&fixture, event.actor, event.kind);
    }
    let forge = forge_with(&[("A", 1, &head_a), ("B", 2, &head_b)]);
    let worker = fixture.worker(
        Arc::new(Scripted::default()),
        forge.clone(),
        config(shprd_core::config::MergePolicy::Auto),
        recording_start(Arc::new(Mutex::new(Vec::new()))),
    );
    let report = worker.tick().expect("the pass ran");
    let land_a = report
        .steps
        .iter()
        .find(|step| step.action == "land" && step.task == "A")
        .expect("A was planned a landing");
    assert!(
        land_a.ok,
        "a clash is an outcome, not a failure: {land_a:?}"
    );
    let first_line = land_a.detail.lines().next().unwrap_or_default();
    assert_eq!(
        first_line,
        "A clashed with work that landed after it; it is being redone on top of the latest (files in conflict: proof.log)",
        "{land_a:?}"
    );
    assert!(
        !land_a.detail.contains("problem on this machine"),
        "{land_a:?}"
    );
    let land_b = report
        .steps
        .iter()
        .find(|step| step.action == "land" && step.task == "B")
        .unwrap_or_else(|| panic!("B was planned a landing: {report:?}"));
    assert!(land_b.ok, "B still lands in the same pass: {land_b:?}");
    let calls = forge.calls();
    assert!(
        calls.iter().any(|call| call.starts_with("close 1 ")),
        "the stale pull request is closed: {calls:?}"
    );
    assert!(
        calls.iter().all(|call| !call.starts_with("merge 1 ")),
        "the clashing task is never merged: {calls:?}"
    );
    let state = fixture.state();
    assert_eq!(
        state.tasks.get("B").map(|task| task.status),
        Some(shprd_core::state::TaskStatus::Integrated),
        "B landed"
    );
    let a = state.tasks.get("A").expect("A");
    assert!(
        shprd_core::rules::landing_conflict(a),
        "the clash is recorded for the planner to send back on: {:?}",
        a.attempts
    );
    // And the next pass sends it back for a fresh attempt.
    let next = worker.tick().expect("the next pass ran");
    assert!(
        next.steps
            .iter()
            .any(|step| step.action == "send_back" && step.task == "A"),
        "A is sent back: {next:?}"
    );
}

#[test]
fn a_landing_that_fails_for_a_machine_reason_records_its_real_cause() {
    // The worktree is not there, so reading the branch tip fails: the person's
    // line names that step, and the pass's record keeps the git command, its
    // exit code and stderr on the lines after.
    let mut log = vec![started()];
    log.extend(ready(2, "abc123"));
    let fixture = Fixture::new(log);
    let forge = forge_with(&[("A", 1, "abc123")]);
    let worker = fixture.worker(
        Arc::new(Scripted::default()),
        forge,
        config(shprd_core::config::MergePolicy::Auto),
        recording_start(Arc::new(Mutex::new(Vec::new()))),
    );
    let report = worker.tick().expect("the pass ran");
    let step = &report.steps[0];
    assert!(!step.ok, "{step:?}");
    let mut lines = step.detail.lines();
    let plain = lines.next().unwrap_or_default();
    assert_eq!(
        plain,
        "Couldn't merge A: it hit a problem on this machine while reading the branch tip, and the details are in the shprd log"
    );
    let cause: Vec<&str> = lines.collect();
    assert!(
        cause.iter().any(|line| line.contains("git")),
        "the cause names the command: {cause:?}"
    );
    let last = shprd_server::pass::LastPass::of(&report, &fixture.repo, 0);
    assert!(last.failed[0].error.contains("reading the branch tip"));
}
