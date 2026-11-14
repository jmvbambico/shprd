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
use shprd_core::log::{EventLog, SystemClock};
use shprd_core::state::RunState;
use shprd_core::tick::PrFacts;
use shprd_git::forge::{FakeForge, Forge, ForgeError, Pr, PrStatus};
use shprd_server::Worker;
use shprd_server::hub::Hub;
use shprd_server::tick::{self, FactsSource, SharedForge, StepReport};
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
            config,
            facts,
            forge,
            start,
            actor: Actor::Engine,
            after_write: None,
            hub: Hub::new(),
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

/// Task `a` driven from added to ready to land, with one passing check and an
/// independent, engine-recorded review.
fn ready(seq: u64, commit: &str) -> Vec<Event> {
    vec![
        added(seq, "A"),
        ev(
            seq + 1,
            EventKind::Spawned {
                task: String::from("A"),
                attempt: 1,
                agent: String::from("claude"),
                model: None,
                worktree: String::from("/wt/A"),
                branch: String::from("shprd/r1/A"),
            },
        ),
        ev(
            seq + 2,
            EventKind::Reported {
                task: String::from("A"),
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
            },
        ),
        ev(
            seq + 3,
            EventKind::ReviewRequested {
                task: String::from("A"),
                attempt: 1,
                reviewer: String::from("codex"),
                degraded: false,
            },
        ),
        ev(
            seq + 4,
            EventKind::GatePassed {
                task: String::from("A"),
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
                task: String::from("A"),
                number: 1,
                url: String::from("https://example.invalid/pull/1"),
                head: String::from("shprd/r1/A"),
            },
        ),
    ]
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
    let facts: Arc<dyn FactsSource> = Arc::new(shprd_server::LiveFacts {
        forge: Arc::clone(&forge),
        live: Arc::new(|_state| BTreeSet::new()),
    });
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
        config: config(shprd_core::config::MergePolicy::Ask),
        facts: Arc::new(facts),
        forge,
        start: recording_start(Arc::new(Mutex::new(Vec::new()))),
        actor: Actor::Engine,
        after_write: None,
        hub: hub.clone(),
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
        tick::run(worker, Duration::from_millis(10), stop).await;
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
        tick::run(worker, Duration::from_millis(5), stop).await;
    });
    assert_eq!(
        peak.load(Ordering::SeqCst),
        1,
        "two passes ran their steps at the same time"
    );
}
