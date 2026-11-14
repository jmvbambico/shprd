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
            config,
            facts,
            fallback: None,
            forge,
            local: false,
            start,
            review: None,
            close_reviewer: None,
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
        fallback: None,
        forge,
        local: false,
        start: recording_start(Arc::new(Mutex::new(Vec::new()))),
        review: None,
        close_reviewer: None,
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
    vec![
        ev(
            seq,
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
            seq + 1,
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
                commit: Some(String::from("abc123")),
                skills_used: Vec::new(),
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
        config: config(shprd_core::config::MergePolicy::Auto),
        facts: Arc::new(Scripted::default()),
        fallback: None,
        forge: Arc::new(NoForge),
        local: false,
        start: recording_start(Arc::new(Mutex::new(Vec::new()))),
        review: Some(review),
        close_reviewer: None,
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
fn a_pass_without_a_reviewer_leaves_the_hand_in_alone() {
    // The review path is wired, but the settings name no reviewer, so there is
    // nothing to start and the pass plans no review step.
    let mut log = vec![started(), added(2, "A")];
    log.extend(handed_in(3));
    let fixture = Fixture::new(log);
    let review: shprd_server::Reviewer =
        Arc::new(|_repo: &Path, task: &str| Ok(format!("started a reviewer on {task}")));
    let worker = Worker {
        repo: fixture.repo.clone(),
        home: fixture.home.clone(),
        config: config(shprd_core::config::MergePolicy::Auto),
        facts: Arc::new(Scripted::default()),
        fallback: None,
        forge: Arc::new(NoForge),
        local: false,
        start: recording_start(Arc::new(Mutex::new(Vec::new()))),
        review: Some(review),
        close_reviewer: None,
        actor: Actor::Engine,
        after_write: None,
        hub: Hub::new(),
        news: None,
    };

    let report = worker.tick().expect("the pass ran");

    assert!(report.steps.is_empty(), "{:?}", report.steps);
}

/// A worker over `fixture` whose review path is `review`.
fn reviewing(fixture: &Fixture, review: shprd_server::Reviewer) -> Worker {
    Worker {
        repo: fixture.repo.clone(),
        home: fixture.home.clone(),
        config: config(shprd_core::config::MergePolicy::Auto),
        facts: Arc::new(Scripted::default()),
        fallback: None,
        forge: Arc::new(NoForge),
        local: false,
        start: recording_start(Arc::new(Mutex::new(Vec::new()))),
        review: Some(review),
        close_reviewer: None,
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
    let reviews = fixture.home.join(".shprd").join("reviews");
    let path = shprd_core::review::findings_path(&reviews, "r1", task, 1);
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
