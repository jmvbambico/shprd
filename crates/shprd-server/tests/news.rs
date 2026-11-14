//! The news a pass tells the coordinator: one message per space, re-checked,
//! and never twice.
//!
//! The delivery is a fake here — it records the pane and the text it was given
//! and types it nowhere — so the whole pass runs with no multiplexer, no agent
//! and no model. The rules themselves (which change is worth a line, the settle
//! window, flapping) are tested next to them in `shprd-views`; these are the
//! service's half: the state it folds, the pane it writes to, and the memory it
//! keeps on disk.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use shprd_core::event::{Actor, Event, EventKind};
use shprd_core::log::{Clock, EventLog, SystemClock};
use shprd_core::state::RunState;
use shprd_core::tick::PrFacts;
use shprd_git::forge::Forge;
use shprd_server::hub::Hub;
use shprd_server::news::memory_path;
use shprd_server::tick::FactsSource;
use shprd_server::{Coordinator, NewsDesk, Worker};
use tempfile::TempDir;

/// A repository whose log is a fixture and whose state a pass advances.
struct Fixture {
    /// Keeps the temp directory alive.
    _temp: TempDir,
    /// The repository the run lives in.
    repo: PathBuf,
    /// The home shprd's own files go under.
    home: PathBuf,
}

impl Fixture {
    /// An empty repository with a home, ready for a log.
    fn new() -> Fixture {
        let temp = TempDir::new().expect("a temp dir");
        let repo = temp.path().join("shprd");
        let home = temp.path().join("home");
        std::fs::create_dir_all(repo.join(".shprd")).expect("mkdir .shprd");
        std::fs::create_dir_all(&home).expect("mkdir the home");
        Fixture {
            _temp: temp,
            repo,
            home,
        }
    }

    /// Appends one event to the log, as a verb would.
    fn append(&self, kind: EventKind) -> Event {
        let path = self.repo.join(".shprd").join("events.jsonl");
        let mut log = EventLog::open(&path).expect("open the log");
        log.append(Actor::Engine, kind, &SystemClock)
            .expect("append the fixture")
    }

    /// Starts run `r1` and adds the given tasks, ready to be started.
    fn plan(&self, tasks: &[&str]) {
        self.start_run("r1", tasks);
    }

    /// Starts a run with the given id and tasks, as the first events of a log.
    fn start_run(&self, run_id: &str, tasks: &[&str]) {
        self.append(run_started(run_id));
        for task in tasks {
            self.append(task_added(task));
        }
    }

    /// Throws the log away and starts `run_id` in its place, as a repository
    /// reset for a new run does.
    fn reset_log(&self, run_id: &str, tasks: &[&str]) {
        let _ = std::fs::remove_file(self.repo.join(".shprd").join("events.jsonl"));
        self.start_run(run_id, tasks);
    }

    /// Records that the coordinator runs in `pane`, as `up` does.
    fn coordinator(&self, pane: &str) {
        self.append(EventKind::OrchestratorLaunched {
            title: String::from("orchestrator"),
            pane: pane.to_string(),
            process_group: 1,
        });
    }

    /// Records that a backup took over `task`, as the fallback ladder does.
    fn backup(&self, task: &str) {
        self.append(EventKind::Backup {
            task: task.to_string(),
            attempt: 1,
            from_agent: String::from("claude"),
            to_agent: String::from("pi"),
            cut: String::from("cut-1"),
            class: shprd_core::event::ProviderClass::Quota,
            reason: String::from("out of allowance"),
            worktree: format!("/wt/{task}"),
            branch: format!("shprd/r1/{task}"),
            started: true,
        });
    }

    /// Records that `task`'s first attempt was spawned, as a real start does.
    fn spawn(&self, task: &str) {
        self.append(EventKind::Spawned {
            task: task.to_string(),
            attempt: 1,
            agent: String::from("claude"),
            model: None,
            worktree: format!("/wt/{task}"),
            branch: format!("shprd/r1/{task}"),
        });
    }

    /// A worker over this repository, with the given facts and news desk.
    fn worker(&self, live: &[&str], news: Option<NewsDesk>) -> Worker {
        let live: std::collections::BTreeSet<String> =
            live.iter().map(|task| (*task).to_owned()).collect();
        // A start records nothing here: the tests spawn the attempt explicitly,
        // so the first pass of a fresh run keeps its old news quiet, as
        // Shepherdr's first look does.
        let start: shprd_server::Starter =
            Arc::new(|_repo: &Path, task: &str| Ok(format!("started {task}")));
        Worker {
            repo: self.repo.clone(),
            home: self.home.clone(),
            config: shprd_core::config::Config::default(),
            facts: Arc::new(Scripted { live }),
            fallback: None,
            forge: Arc::new(NoForge),
            start,
            actor: Actor::Engine,
            after_write: None,
            hub: Hub::new(),
            news,
        }
    }

    /// One pass, asserting it ran.
    fn pass(&self, worker: &Worker) {
        worker.tick().expect("the pass ran");
    }
}

/// A facts source with no forge answers and the given live tasks.
struct Scripted {
    /// Which tasks have an agent running.
    live: std::collections::BTreeSet<String>,
}

impl FactsSource for Scripted {
    fn pr(&self, _task: &shprd_core::state::Task, _state: &RunState) -> Option<PrFacts> {
        None
    }

    fn live(&self, _state: &RunState) -> std::collections::BTreeSet<String> {
        self.live.clone()
    }
}

/// A forge nobody asks.
struct NoForge;

impl Forge for NoForge {
    fn find_open(
        &self,
        _head: &str,
    ) -> Result<Option<shprd_git::forge::Pr>, shprd_git::forge::ForgeError> {
        Ok(None)
    }

    fn create(
        &self,
        _base: &str,
        _head: &str,
        _title: &str,
        _body_file: &Path,
    ) -> Result<shprd_git::forge::Pr, shprd_git::forge::ForgeError> {
        panic!("a news pass never opens a pull request")
    }

    fn edit_body(
        &self,
        _number: u64,
        _body_file: &Path,
    ) -> Result<(), shprd_git::forge::ForgeError> {
        panic!("a news pass never edits a pull request")
    }

    fn status(
        &self,
        _number: u64,
    ) -> Result<shprd_git::forge::PrStatus, shprd_git::forge::ForgeError> {
        panic!("a news pass never asks about a pull request")
    }

    fn merge(&self, _number: u64, _head_sha: &str) -> Result<(), shprd_git::forge::ForgeError> {
        panic!("a news pass never merges")
    }
}

/// A coordinator that records what it is told and types it nowhere.
#[derive(Default)]
struct FakeHost {
    /// Every message, in order: the pane and the text.
    told: Mutex<Vec<(String, String)>>,
    /// A host that refuses every message, as an unreachable pane would.
    refuse: bool,
}

impl FakeHost {
    /// A host that takes what it is given.
    fn new() -> Arc<FakeHost> {
        Arc::new(FakeHost::default())
    }

    /// A host that refuses every message.
    fn refusing() -> Arc<FakeHost> {
        Arc::new(FakeHost {
            refuse: true,
            ..FakeHost::default()
        })
    }

    /// Everything it was told, oldest first.
    fn told(&self) -> Vec<(String, String)> {
        self.told
            .lock()
            .expect("the host's log is not poisoned")
            .clone()
    }
}

impl Coordinator for FakeHost {
    fn tell(&self, pane: &str, text: &str) -> Result<(), String> {
        if self.refuse {
            return Err(String::from("the pane is gone"));
        }
        self.told
            .lock()
            .map_err(|_| String::from("the host's log is poisoned"))?
            .push((pane.to_owned(), text.to_owned()));
        Ok(())
    }
}

/// A clock a test moves by hand, so the settle window is not a race.
struct HandClock(AtomicU64);

impl HandClock {
    /// A clock that starts at `at`.
    fn at(at: u64) -> Arc<HandClock> {
        Arc::new(HandClock(AtomicU64::new(at)))
    }

    /// Moves it to `at`.
    fn set(&self, at: u64) {
        self.0.store(at, Ordering::SeqCst);
    }
}

impl Clock for HandClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// The moment the tests start at.
const NOW: u64 = 1_700_000_000_000;

/// A news desk over `host`, settling after `settle_ms` on `clock`.
fn desk(host: Arc<FakeHost>, clock: Arc<HandClock>, settle_ms: u64) -> NewsDesk {
    NewsDesk {
        coordinator: host,
        settle_ms,
        clock,
    }
}

/// A run's first event.
fn run_started(run_id: &str) -> EventKind {
    EventKind::RunStarted {
        run_id: run_id.to_string(),
        repo: String::from("/repo/shprd"),
        base_branch: String::from("main"),
        integration_branch: String::from("integration/m9"),
    }
}

/// A task added, waiting on nothing.
fn task_added(task: &str) -> EventKind {
    EventKind::TaskAdded {
        task: task.to_string(),
        title: format!("task {task}"),
        scope: vec![format!("{task}.txt")],
        acceptance: vec![String::from("it works")],
        tags: Vec::new(),
        skills: Vec::new(),
        deps: Vec::new(),
    }
}

/// The whole point: two tasks start, and the coordinator is told once, in one
/// message, with both lines.
#[test]
fn a_pass_tells_the_coordinator_once_with_the_lines_of_one_moment() {
    let fixture = Fixture::new();
    fixture.plan(&["A", "B"]);
    fixture.coordinator("pane-1");
    let host = FakeHost::new();
    let clock = HandClock::at(NOW);
    let worker = fixture.worker(&["A", "B"], Some(desk(host.clone(), clock.clone(), 0)));

    // The first pass starts the tasks and only notes how they read: old news
    // is not news.
    fixture.pass(&worker);
    assert!(
        host.told().is_empty(),
        "the first look told old news: {:?}",
        host.told()
    );
    fixture.spawn("A");
    fixture.spawn("B");
    clock.set(NOW + 1_000);

    let report = worker.tick().expect("the pass ran");
    let told = host.told();
    assert_eq!(told.len(), 1, "exactly one message: {told:?}");
    let (pane, text) = &told[0];
    assert_eq!(pane, "pane-1", "the pane the run recorded");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "both tasks in one message: {text:?}");
    assert!(
        text.contains("A started: task A") && text.contains("B started: task B"),
        "each line says what happened: {text:?}"
    );
    assert_eq!(
        report.news.len(),
        1,
        "the report says who was told: {:?}",
        report.news
    );
    assert_eq!(report.news[0].lines.len(), 2);
}

/// The same state a second and a third time tells nothing again, and a
/// restarted service reads the memory rather than the tasks as new.
#[test]
fn a_change_is_told_once_across_passes_and_restarts() {
    let fixture = Fixture::new();
    fixture.plan(&["A", "B"]);
    fixture.coordinator("pane-1");
    let host = FakeHost::new();
    let clock = HandClock::at(NOW);
    let worker = fixture.worker(&["A", "B"], Some(desk(host.clone(), clock.clone(), 0)));

    fixture.pass(&worker);
    fixture.spawn("A");
    fixture.spawn("B");
    clock.set(NOW + 1_000);
    worker.tick().expect("the pass that starts them ran");
    assert_eq!(host.told().len(), 1, "the change tells it once");

    // Two more passes of the same state, and then a fresh worker over the same
    // repository as a restarted service would build.
    clock.set(NOW + 20_000);
    worker.tick().expect("the second pass ran");
    clock.set(NOW + 40_000);
    worker.tick().expect("the third pass ran");
    assert_eq!(host.told().len(), 1, "a pass repeats nothing");

    let restarted_host = FakeHost::new();
    let restarted = fixture.worker(
        &["A", "B"],
        Some(desk(restarted_host.clone(), HandClock::at(NOW + 60_000), 0)),
    );
    restarted.tick().expect("the restarted pass ran");
    assert!(
        restarted_host.told().is_empty(),
        "a restart told the old news again: {:?}",
        restarted_host.told()
    );
    assert!(
        memory_path(&fixture.repo).exists(),
        "the news' memory is kept in the repository's .shprd"
    );
}

/// A change that has not held for the settle window waits for the next pass.
#[test]
fn a_change_waits_for_the_settle_window() {
    let fixture = Fixture::new();
    fixture.plan(&["A"]);
    fixture.coordinator("pane-1");
    let host = FakeHost::new();
    let clock = HandClock::at(NOW);
    // The first pass starts A and only notes how it reads; the change comes
    // after it, and with a five-second window it is too fresh to tell.
    let worker = fixture.worker(&["A"], Some(desk(host.clone(), clock.clone(), 5_000)));

    fixture.pass(&worker);
    fixture.spawn("A");
    clock.set(NOW + 1_000);
    worker.tick().expect("the pass that made the news ran");
    assert!(host.told().is_empty(), "told before the window was over");

    clock.set(NOW + 5_000);
    worker.tick().expect("a pass inside the window ran");
    assert!(host.told().is_empty(), "four seconds is not five");

    clock.set(NOW + 7_000);
    worker.tick().expect("a pass after the window ran");
    assert_eq!(host.told().len(), 1, "the window is over, so it is told");
}

/// A coordinated run with no coordinator pane keeps its news for a later pass.
#[test]
fn news_with_nobody_to_tell_is_kept_until_there_is_a_pane() {
    let fixture = Fixture::new();
    fixture.plan(&["A"]);
    let host = FakeHost::new();
    let clock = HandClock::at(NOW);
    let worker = fixture.worker(&["A"], Some(desk(host.clone(), clock.clone(), 0)));

    fixture.pass(&worker);
    fixture.spawn("A");
    clock.set(NOW + 1_000);
    worker.tick().expect("the pass with no coordinator ran");
    assert!(host.told().is_empty(), "there was nobody to tell");

    // `up` starts the coordinator afterwards.
    fixture.coordinator("pane-1");
    clock.set(NOW + 10_000);
    worker.tick().expect("the pass with a coordinator ran");
    let told = host.told();
    assert_eq!(told.len(), 1, "the kept line went once: {told:?}");
    assert!(told[0].1.contains("A started"), "and it is the old line");
}

/// A refused write keeps the line: the next pass tries again.
#[test]
fn a_refused_delivery_is_tried_again() {
    let fixture = Fixture::new();
    fixture.plan(&["A"]);
    fixture.coordinator("pane-1");
    let refusing = FakeHost::refusing();
    let clock = HandClock::at(NOW);
    let worker = fixture.worker(&["A"], Some(desk(refusing, clock.clone(), 0)));

    fixture.pass(&worker);
    fixture.spawn("A");
    clock.set(NOW + 1_000);
    let report = worker.tick().expect("the pass ran with a refusing host");
    assert!(report.news.is_empty(), "nothing was told");

    let host = FakeHost::new();
    let retrying = fixture.worker(&["A"], Some(desk(host.clone(), clock.clone(), 0)));
    clock.set(NOW + 2_000);
    retrying.tick().expect("the next pass ran");
    let told = host.told();
    assert_eq!(told.len(), 1, "the kept line went on the next pass");
    assert!(told[0].1.contains("A started"));
}

/// A task just started is news as started, not as an agent that stopped: the
/// host has not seen the new terminal yet, and the start grace says so.
#[test]
fn a_task_just_spawned_is_not_told_as_failed_before_the_host_sees_it() {
    let fixture = Fixture::new();
    fixture.plan(&["A"]);
    fixture.coordinator("pane-1");
    let host = FakeHost::new();
    // The real clock: the spawn the test records is a moment ago, as a real
    // pass's is.
    let worker = fixture.worker(
        &[],
        Some(NewsDesk {
            coordinator: host.clone(),
            settle_ms: 0,
            clock: Arc::new(SystemClock),
        }),
    );

    fixture.pass(&worker);
    fixture.spawn("A");
    worker.tick().expect("the pass after the spawn ran");
    let told = host.told();
    assert_eq!(told.len(), 1, "one line: {told:?}");
    assert!(
        told[0].1.contains("A started"),
        "a just-started task is news as started: {:?}",
        told[0].1
    );
}

/// A worker that has appended no event for half an hour is not stalled: its
/// agent is at work, and the host says so.
#[test]
fn a_live_agent_with_nothing_in_the_log_is_not_told_as_stalled() {
    let fixture = Fixture::new();
    fixture.plan(&["A"]);
    fixture.coordinator("pane-1");
    let host = FakeHost::new();
    // The clock is half an hour past the log, as if the task had been working
    // through a long build since its spawn.
    let clock = HandClock::at(SystemClock.now_ms() + shprd_views::news::QUIET_MS + 600_000);
    let worker = fixture.worker(&["A"], Some(desk(host.clone(), clock.clone(), 0)));

    fixture.pass(&worker);
    fixture.spawn("A");
    worker.tick().expect("the pass half an hour later ran");
    let told = host.told();
    assert_eq!(told.len(), 1, "one message: {told:?}");
    let lines: Vec<&str> = told[0].1.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "the start is the only news, not a stall: {lines:?}"
    );
    assert!(lines[0].contains("A started"), "{:?}", lines[0]);
    assert!(
        !told[0].1.contains("went quiet"),
        "a working agent is not stalled: {:?}",
        told[0].1
    );
}

/// A memory left by another run does not suppress this run's news: the ids and
/// the log's sequence numbers are reused.
#[test]
fn a_memory_of_another_run_does_not_suppress_the_new_ones_news() {
    let fixture = Fixture::new();
    fixture.plan(&["A"]);
    fixture.coordinator("pane-1");
    let host = FakeHost::new();
    let clock = HandClock::at(NOW);
    let worker = fixture.worker(&["A"], Some(desk(host.clone(), clock.clone(), 0)));

    // The first run: A starts and a backup takes over, both told.
    fixture.pass(&worker);
    fixture.spawn("A");
    fixture.backup("A");
    clock.set(NOW + 1_000);
    worker.tick().expect("the first run's pass ran");
    assert_eq!(host.told().len(), 1, "the first run's message");
    assert!(host.told()[0].1.contains("A backup"));

    // The repository is reset and run `r2` takes over the same log file, with
    // the same sequence numbers.
    fixture.reset_log("r2", &["A"]);
    fixture.coordinator("pane-1");
    clock.set(NOW + 20_000);
    worker.tick().expect("the new run's first pass ran");
    assert_eq!(host.told().len(), 1, "the new run's first look is not news");
    fixture.spawn("A");
    fixture.backup("A");
    clock.set(NOW + 40_000);
    worker.tick().expect("the new run's pass ran");
    let told = host.told();
    assert_eq!(
        told.len(),
        2,
        "the new run's backup is news again: {told:?}"
    );
    assert!(
        told[1].1.contains("A backup"),
        "the same sequence number in another run is another backup: {:?}",
        told[1].1
    );
}

/// A pass with no news desk never writes to a coordinator.
#[test]
fn a_pass_without_a_news_desk_tells_nobody() {
    let fixture = Fixture::new();
    fixture.plan(&["A"]);
    fixture.coordinator("pane-1");
    let worker = fixture.worker(&["A"], None);
    let report = worker.tick().expect("the pass ran");
    assert!(report.news.is_empty());
    assert!(
        !memory_path(&fixture.repo).exists(),
        "nothing kept when there is no news"
    );
}
