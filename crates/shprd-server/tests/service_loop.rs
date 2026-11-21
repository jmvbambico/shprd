//! The service runs the whole loop on its own.
//!
//! This is the M9 gate's fix F in one test: a repository with no remote, a
//! person's `settings.toml` naming a reviewer and `tasks.merge = "auto"`,
//! **nothing** in `config.toml`, a task that hands in, a scripted reviewer that
//! writes a clean `findings.json`, and successive passes of one long-lived
//! [`Worker`] — the same object `shprd serve` runs — that decide the review and
//! land the task on the run's integration branch. A second settings change turns
//! the policy to `ask` and the very next pass on that same worker asks a person
//! instead of merging, with no restart.
//!
//! The host is a fake (`FactsSource` with no live agents), the forge is one that
//! refuses every call because a local landing has none, and the reviewer is a
//! closure — the same shape the binary wires to `agent review` — that records the
//! review request and writes the findings file a real reviewer would. Everything
//! else is real: the log, the git worktrees and the integration branch.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use shprd_core::event::{Actor, CheckReport, Event, EventKind, ReportStatus};
use shprd_core::log::{EventLog, SystemClock};
use shprd_core::state::{RunState, TaskStatus};
use shprd_core::tick::PrFacts;
use shprd_git::forge::{Forge, ForgeError, Pr, PrStatus};
use shprd_server::hub::Hub;
use shprd_server::tick::{FactsSource, SharedForge};
use shprd_server::{Starter, TickReport, Worker};
use tempfile::TempDir;

/// A repository with no remote, a home, a run, and one task's own worktree.
struct Fixture {
    /// Keeps the temp directory alive.
    _temp: TempDir,
    /// The repository the run works on.
    root: PathBuf,
    /// The home whose `.shprd` holds the log, worktrees, briefs and reviews.
    home: PathBuf,
    /// Every task whose worker tab the pass asked to close, in order.
    closed: Arc<Mutex<Vec<String>>>,
}

impl Fixture {
    /// A repository on `main` with `integration/m9` cut from it, and a run.
    fn new() -> Fixture {
        let temp = TempDir::new().expect("temp dir");
        let root = temp.path().join("repo");
        let home = temp.path().join("home");
        std::fs::create_dir_all(&root).expect("mkdir the repo");
        std::fs::create_dir_all(&home).expect("mkdir the home");
        let fixture = Fixture {
            _temp: temp,
            root,
            home,
            closed: Arc::new(Mutex::new(Vec::new())),
        };
        fixture.git(&["init", "-b", "main"]);
        fixture.git(&["config", "user.name", "shprd tests"]);
        fixture.git(&["config", "user.email", "tests@example.invalid"]);
        fixture.git(&["config", "commit.gpgsign", "false"]);
        std::fs::write(fixture.root.join("a.txt"), "the seed\n").expect("write the seed");
        fixture.git(&["add", "--", "a.txt"]);
        fixture.git(&["commit", "-m", "the seed"]);
        fixture.git(&["branch", "integration/m9"]);
        fixture.append(EventKind::RunStarted {
            run_id: String::from("r1"),
            repo: fixture.root.display().to_string(),
            base_branch: String::from("main"),
            integration_branch: String::from("integration/m9"),
        });
        fixture
    }

    /// Writes the person's `settings.toml`: `merge`, and a reviewer role.
    ///
    /// The merge policy lives here and nowhere else: the acceptance is that a
    /// `settings.toml` alone is enough, with no `config.toml` to keep in step.
    fn settings(&self, merge: &str) {
        let dir = self.home.join(".config").join("shprd");
        std::fs::create_dir_all(&dir).expect("mkdir the settings dir");
        let text = format!(
            "version = 1\n\n[tasks]\nmerge = \"{merge}\"\n\n\
             [roles.reviewer]\nagent = \"codex\"\nmodel = \"gpt-5-codex\"\n"
        );
        std::fs::write(dir.join("settings.toml"), text).expect("write the settings");
    }

    /// Runs git in the repository, asserting success.
    fn git(&self, args: &[&str]) -> String {
        self.git_in(&self.root.clone(), args)
    }

    /// Runs git in `dir`, asserting success, returning its trimmed stdout.
    fn git_in(&self, dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
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

    /// Appends one event to the run log, as the CLI's verbs would.
    fn append(&self, kind: EventKind) {
        let path = self.root.join(".shprd").join("events.jsonl");
        std::fs::create_dir_all(self.root.join(".shprd")).expect("mkdir .shprd");
        let mut log = EventLog::open(&path).expect("open the log");
        log.append(Actor::Engine, kind, &SystemClock)
            .expect("append the event");
    }

    /// Every event in the log, oldest first.
    fn events(&self) -> Vec<Event> {
        let path = self.root.join(".shprd").join("events.jsonl");
        EventLog::read_all(&path).expect("read the log").events
    }

    /// The folded state.
    fn state(&self) -> RunState {
        RunState::fold(&self.events()).expect("the log folds")
    }

    /// Adds task `id` and gives it an attempt in its own worktree, with a commit.
    ///
    /// The worktree is cut from `integration/m9` inside shprd's own layout, so
    /// the landing merges a real branch and never touches the person's checkout.
    fn add_task(&self, id: &str) {
        self.append(EventKind::TaskAdded {
            task: id.to_string(),
            title: format!("task {id}"),
            scope: vec![format!("{id}.txt")],
            acceptance: vec![String::from("it works")],
            tags: Vec::new(),
            skills: Vec::new(),
            deps: Vec::new(),
        });
        let layout = shprd_git::worktree::Layout::default_for_home(&self.home);
        let worktree = layout.path("repo", "r1", id);
        if let Some(parent) = worktree.parent() {
            std::fs::create_dir_all(parent).expect("mkdir the worktree's parent");
        }
        let branch = format!("shprd/r1/{id}");
        self.git(&[
            "worktree",
            "add",
            "-b",
            &branch,
            &worktree.display().to_string(),
            "integration/m9",
        ]);
        self.append(EventKind::Spawned {
            task: id.to_string(),
            attempt: 1,
            agent: String::from("claude"),
            model: None,
            worktree: worktree.display().to_string(),
            branch,
        });
        // The task's work: one file of its own, committed on its branch.
        let name = format!("{id}.txt");
        std::fs::write(worktree.join(&name), format!("{id}'s work\n")).expect("write the work");
        self.git_in(&worktree, &["add", "--", &name]);
        self.git_in(&worktree, &["commit", "-m", "the task's work"]);
        let head = self.git_in(&worktree, &["rev-parse", "HEAD"]);
        self.append(EventKind::Reported {
            task: id.to_string(),
            attempt: 1,
            status: ReportStatus::Completed,
            summary: String::from("done"),
            tests: Vec::new(),
            checks: vec![CheckReport {
                gate: String::from("tests"),
                command: String::from("cargo test"),
                exit_code: Some(0),
                output_tail: String::from("ok"),
            }],
            changed_paths: vec![name],
            commit: Some(head),
            skills_used: Vec::new(),
            proofs: Vec::new(),
        });
    }

    /// Writes the findings file a reviewer for `task` would write, backdated so
    /// a pass trusts it without waiting out the settle window.
    fn write_findings(&self, task: &str, body: &str) {
        let reviews = self.home.join(".shprd").join("reviews");
        let path = shprd_core::review::findings_path(&reviews, "r1", task, 1);
        std::fs::create_dir_all(path.parent().expect("a packet folder")).expect("mkdir the packet");
        std::fs::write(&path, body).expect("write the findings");
        let file = std::fs::File::options()
            .write(true)
            .open(&path)
            .expect("open the findings");
        file.set_modified(SystemTime::now() - Duration::from_secs(5))
            .expect("backdate the findings");
    }

    /// The integration branch's head in the repository.
    fn integration_head(&self) -> String {
        self.git(&["rev-parse", "refs/heads/integration/m9"])
    }

    /// A worker over this fixture: no live agents, no forge, a scripted reviewer.
    fn worker(&self) -> Worker {
        let facts = Arc::new(Scripted::default());
        let forge: SharedForge = Arc::new(NoForge);
        let start: Starter = Arc::new(|_repo: &Path, task: &str| Ok(format!("started {task}")));
        let reviewer_root = self.root.clone();
        let reviewer_home = self.home.clone();
        let review: shprd_server::Reviewer = Arc::new(move |_repo: &Path, task: &str| {
            // What `agent review` records before its tab opens.
            let path = reviewer_root.join(".shprd").join("events.jsonl");
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
            // And what the reviewer itself writes: a clean findings file.
            let reviews = reviewer_home.join(".shprd").join("reviews");
            let findings = shprd_core::review::findings_path(&reviews, "r1", task, 1);
            std::fs::create_dir_all(findings.parent().expect("a packet folder"))
                .map_err(|err| err.to_string())?;
            std::fs::write(&findings, "[]").map_err(|err| err.to_string())?;
            let file = std::fs::File::options()
                .write(true)
                .open(&findings)
                .map_err(|err| err.to_string())?;
            file.set_modified(SystemTime::now() - Duration::from_secs(5))
                .map_err(|err| err.to_string())?;
            Ok(format!("started a reviewer on {task}"))
        });
        // A landing closes the worker tab the task can no longer come back to,
        // through the closure `serve` wires to `spawn::close_worker`.
        let closed = Arc::clone(&self.closed);
        let close_worker: shprd_server::WorkerCloser = Arc::new(move |_repo: &Path, task: &str| {
            closed
                .lock()
                .map_err(|_| String::from("the call log is poisoned"))?
                .push(task.to_string());
            Ok(())
        });
        Worker {
            repo: self.root.clone(),
            home: self.home.clone(),
            state_dir: shprd_server::state_dir(&shprd_server::state_home(&self.home, None)),
            // The file layers say nothing about a merge policy: the settings are
            // the only place `auto` is written.
            config: shprd_core::config::Config::default(),
            facts,
            fallback: None,
            forge,
            // No remote: a landing goes to `integration/m9` in shprd's own
            // integration worktree.
            local: true,
            start,
            review: Some(review),
            close_reviewer: None,
            close_worker: Some(close_worker),
            actor: Actor::Engine,
            after_write: None,
            hub: Hub::new(),
            news: None,
        }
    }

    /// Every task whose worker tab the passes asked to close, in order.
    fn closed_workers(&self) -> Vec<String> {
        self.closed
            .lock()
            .map(|closed| closed.clone())
            .unwrap_or_else(|poisoned| poisoned.into_inner().clone())
    }
}

/// A facts source that says nothing: no live agents and no pull requests.
#[derive(Default)]
struct Scripted {
    /// What the forge says, by task (nothing here: a local run has no forge).
    prs: BTreeMap<String, PrFacts>,
}

impl FactsSource for Scripted {
    fn pr(&self, task: &shprd_core::state::Task, _state: &RunState) -> Option<PrFacts> {
        self.prs.get(&task.id).cloned()
    }

    fn live(&self, _state: &RunState) -> BTreeSet<String> {
        BTreeSet::new()
    }
}

/// A forge a local run never asks, and that fails loudly if one does.
#[derive(Debug)]
struct NoForge;

impl Forge for NoForge {
    fn find_open(&self, _head: &str) -> Result<Option<Pr>, ForgeError> {
        Err(unsupported())
    }

    fn create(
        &self,
        _base: &str,
        _head: &str,
        _title: &str,
        _body_file: &Path,
    ) -> Result<Pr, ForgeError> {
        Err(unsupported())
    }

    fn edit_body(&self, _number: u64, _body_file: &Path) -> Result<(), ForgeError> {
        Err(unsupported())
    }

    fn status(&self, _number: u64) -> Result<PrStatus, ForgeError> {
        Err(unsupported())
    }

    fn merge(&self, _number: u64, _head_sha: &str) -> Result<(), ForgeError> {
        Err(unsupported())
    }
}

/// The error a local run's forge would see if a pass ever asked it anything.
fn unsupported() -> ForgeError {
    ForgeError::Failed {
        args: vec![String::from("gh")],
        code: Some(1),
        stderr: String::from("this repository has no remote, so it has no forge"),
    }
}

/// The actions a pass reported, as `action:task` pairs.
fn actions(report: &TickReport) -> Vec<String> {
    report
        .steps
        .iter()
        .map(|step| format!("{}:{}", step.action, step.task))
        .collect()
}

#[test]
fn successive_passes_decide_the_review_and_land_the_task_on_their_own() {
    let fixture = Fixture::new();
    fixture.settings("auto");
    fixture.add_task("T1");
    let worker = fixture.worker();
    let seed = fixture.integration_head();
    let main_before = fixture.git(&["rev-parse", "refs/heads/main"]);

    // Pass 1: the hand-in is reviewed. The scripted reviewer records its request
    // and writes a clean findings file.
    let first = worker.tick().expect("the first pass ran");
    assert_eq!(actions(&first), vec!["review:T1".to_string()], "{first:?}");
    assert!(first.steps[0].ok, "{first:?}");

    // Pass 2: the verdict is recorded from the file, with no other call.
    let second = worker.tick().expect("the second pass ran");
    assert_eq!(
        actions(&second),
        vec!["verdict:T1".to_string()],
        "{second:?}"
    );
    assert!(second.steps[0].ok, "{second:?}");
    let state = fixture.state();
    let reviewed = state.tasks.get("T1").expect("the task");
    assert_eq!(reviewed.status, TaskStatus::Passed, "the review passed");
    assert!(
        reviewed
            .attempts
            .last()
            .expect("an attempt")
            .gates
            .iter()
            .any(|gate| gate.gate == "review" && gate.passed),
        "a review gate was recorded"
    );

    // Pass 3: `tasks.merge = "auto"` from the settings alone lands it on the
    // run's integration branch, in shprd's own worktree.
    let third = worker.tick().expect("the third pass ran");
    assert_eq!(actions(&third), vec!["land:T1".to_string()], "{third:?}");
    assert!(third.steps[0].ok, "{third:?}");
    let head = fixture.integration_head();
    assert_ne!(head, seed, "the integration branch moved");
    assert_eq!(
        fixture.git(&["show", &format!("{head}:T1.txt")]),
        "T1's work",
        "the task's work is on the integration branch"
    );
    assert_eq!(
        fixture.state().tasks.get("T1").map(|task| task.status),
        Some(TaskStatus::Integrated)
    );
    assert_eq!(
        fixture.git(&["rev-parse", "refs/heads/main"]),
        main_before,
        "the person's branch never moved"
    );

    // Pass 4: nothing is left to do, and the pass says so.
    let fourth = worker.tick().expect("the fourth pass ran");
    assert!(fourth.steps.is_empty(), "{fourth:?}");

    // The landing closed the worker's tab, and only that: a tab of a task that
    // did not land is still open.
    assert_eq!(
        fixture.closed_workers(),
        vec![String::from("T1")],
        "the landing closed T1's worker tab and nothing else"
    );

    // The whole loop is in the ledger: one request, one pass, one landing.
    let passed = fixture
        .events()
        .iter()
        .filter(|event| {
            matches!(
                &event.kind,
                EventKind::GatePassed { task, gate, .. } if task == "T1" && gate == "review"
            )
        })
        .count();
    assert_eq!(passed, 1, "the verdict was recorded once");
    let landed: Vec<Event> = fixture
        .events()
        .into_iter()
        .filter(|event| matches!(&event.kind, EventKind::TaskLanded { task, .. } if task == "T1"))
        .collect();
    assert_eq!(landed.len(), 1, "the landing was recorded once");
    match &landed[0].kind {
        EventKind::TaskLanded { base, outcome, .. } => {
            assert_eq!(base, "integration/m9", "the branch the landing wrote");
            assert_eq!(*outcome, shprd_core::event::LandOutcome::Merged);
        }
        other => panic!("expected a landing, got {other:?}"),
    }

    // A second settings change applies to the very next pass of the same
    // worker: `ask` is a question, not a merge.
    fixture.settings("ask");
    fixture.add_task("T2");
    // T2 is already reviewed: the request and the verdict are in the log, so the
    // pass that matters is the landing one.
    fixture.append(EventKind::ReviewRequested {
        task: String::from("T2"),
        attempt: 1,
        reviewer: String::from("codex"),
        degraded: false,
    });
    fixture.write_findings("T2", "[]");
    let reviewed = worker.tick().expect("the verdict pass ran");
    assert_eq!(
        actions(&reviewed),
        vec!["verdict:T2".to_string()],
        "{reviewed:?}"
    );
    let before = fixture.integration_head();
    let asked = worker
        .tick()
        .expect("the pass after the settings change ran");
    assert_eq!(
        actions(&asked),
        vec!["needs_you:T2".to_string()],
        "{asked:?}"
    );
    assert!(
        asked.steps[0]
            .detail
            .contains("T2 is ready to land: Review and Accept it in the Tasks pane"),
        "the person is told what lands it (the Accept button, no command): {asked:?}"
    );
    assert_eq!(
        fixture.integration_head(),
        before,
        "an ask never lands anything"
    );
    assert_eq!(
        fixture.state().tasks.get("T2").map(|task| task.status),
        Some(TaskStatus::Passed),
        "the task waits for its person"
    );
    // A task that was only asked about never lands, so its tab is not closed:
    // the person may still need the worker's output to decide.
    assert_eq!(
        fixture.closed_workers(),
        vec![String::from("T1")],
        "an ask closes nothing"
    );
}
