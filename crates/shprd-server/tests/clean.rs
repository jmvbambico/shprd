//! The engine's own cleanup of finished tasks: a pass that finds a landed or
//! dropped task with nothing running removes its worktree and branch, closes
//! what is left of its windows, and never touches a task that is still working.
//! Real git, a scripted log, no network and no terminal.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use shprd_core::ask::AskKind;
use shprd_core::config::Config;
use shprd_core::event::{Actor, Event, EventKind, LandOutcome, ResourceAction, ResourceKind};
use shprd_core::log::{EventLog, SystemClock};
use shprd_core::state::{RunState, Task, TaskStatus};
use shprd_core::tick::PrFacts;
use shprd_git::forge::{Forge, ForgeError, Pr, PrStatus};
use shprd_git::worktree::Layout;
use shprd_server::Worker;
use shprd_server::hub::Hub;
use shprd_server::tick::{FactsSource, SharedForge};
use tempfile::TempDir;

/// A forge that answers nothing: a cleanup pass never asks it.
#[derive(Debug)]
struct NoForge;

impl Forge for NoForge {
    fn find_open(&self, _head: &str) -> Result<Option<Pr>, ForgeError> {
        Ok(None)
    }
    fn create(&self, _: &str, _: &str, _: &str, _: &Path) -> Result<Pr, ForgeError> {
        Err(ForgeError::Failed {
            args: vec![],
            code: Some(1),
            stderr: String::new(),
        })
    }
    fn edit_body(&self, _: u64, _: &Path) -> Result<(), ForgeError> {
        Ok(())
    }
    fn status(&self, _: u64) -> Result<PrStatus, ForgeError> {
        Err(ForgeError::Failed {
            args: vec![],
            code: Some(1),
            stderr: String::new(),
        })
    }
    fn merge(&self, _: u64, _: &str) -> Result<(), ForgeError> {
        Err(ForgeError::Failed {
            args: vec![],
            code: Some(1),
            stderr: String::new(),
        })
    }
}

/// Which tasks the host says have a window open; a test changes it between passes.
#[derive(Default)]
struct Windows {
    live: Mutex<BTreeSet<String>>,
}

impl FactsSource for Windows {
    fn pr(&self, _task: &Task, _state: &RunState) -> Option<PrFacts> {
        None
    }
    fn live(&self, _state: &RunState) -> BTreeSet<String> {
        self.live.lock().unwrap().clone()
    }
}

/// A real repository with run `r1` in its log and shprd's home beside it.
struct Fixture {
    _temp: TempDir,
    repo: PathBuf,
    home: PathBuf,
    windows: Arc<Windows>,
    closed: Arc<AtomicUsize>,
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@t.invalid"])
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

impl Fixture {
    fn new() -> Fixture {
        let temp = TempDir::new().expect("temp dir");
        // `canonicalize`: macOS's /var is a link and git reports the real path.
        let base = temp.path().canonicalize().expect("canonical temp");
        let repo = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(repo.join(".shprd")).expect("mkdir");
        std::fs::create_dir_all(&home).expect("mkdir home");
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("a.txt"), "a").expect("write");
        git(&repo, &["add", "a.txt"]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        let fixture = Fixture {
            _temp: temp,
            repo,
            home,
            windows: Arc::new(Windows::default()),
            closed: Arc::new(AtomicUsize::new(0)),
        };
        fixture.append(
            Actor::Engine,
            EventKind::RunStarted {
                run_id: "r1".into(),
                repo: fixture.repo.display().to_string(),
                base_branch: "main".into(),
                integration_branch: "integration/m1".into(),
            },
        );
        fixture
    }

    fn log_path(&self) -> PathBuf {
        self.repo.join(".shprd").join("events.jsonl")
    }

    fn append(&self, actor: Actor, kind: EventKind) {
        let mut log = EventLog::open(&self.log_path()).expect("open the log");
        log.append(actor, kind, &SystemClock).expect("append");
    }

    fn events(&self) -> Vec<Event> {
        EventLog::read_all(&self.log_path())
            .expect("read the log")
            .events
    }

    fn state(&self) -> RunState {
        RunState::fold(&self.events()).expect("the log folds")
    }

    fn settings(&self, text: &str) {
        let dir = self.home.join(".config").join("shprd");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("settings.toml"), text).expect("write");
    }

    /// Adds task `id` with a real worktree (made dirty, with build output) and
    /// branch, recorded in the ledger the way a spawn records them.
    fn task(&self, id: &str) -> (PathBuf, String) {
        let layout = Layout::default_for_home(&self.home);
        let path = layout.path("repo", "r1", id);
        let branch = format!("shprd/r1/{id}");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        git(
            &self.repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                &branch,
                &path.display().to_string(),
            ],
        );
        // Untracked proof and build output: the very thing that made a removal
        // refuse before.
        std::fs::write(path.join("proof.txt"), "untracked").expect("write");
        std::fs::create_dir_all(path.join("target/debug")).expect("mkdir");
        std::fs::write(path.join("target/debug/out"), "bin").expect("write");
        self.append(
            Actor::Engine,
            EventKind::TaskAdded {
                task: id.into(),
                title: id.into(),
                scope: vec!["a.txt".into()],
                acceptance: vec!["works".into()],
                tags: vec![],
                skills: vec![],
                deps: vec![],
            },
        );
        self.append(
            Actor::Engine,
            EventKind::Spawned {
                task: id.into(),
                attempt: 1,
                agent: "claude".into(),
                model: None,
                worktree: path.display().to_string(),
                branch: branch.clone(),
            },
        );
        for (kind, name) in [
            (ResourceKind::Worktree, path.display().to_string()),
            (ResourceKind::Branch, branch.clone()),
        ] {
            self.append(
                Actor::Engine,
                EventKind::Resource {
                    action: ResourceAction::Created,
                    kind,
                    name,
                    task: Some(id.into()),
                },
            );
        }
        (path, branch)
    }

    fn land(&self, id: &str) {
        self.append(
            Actor::Engine,
            EventKind::TaskLanded {
                task: id.into(),
                base: "main".into(),
                head: "abc".into(),
                number: Some(1),
                outcome: LandOutcome::Merged,
            },
        );
    }

    fn drop_task(&self, id: &str) {
        self.append(
            Actor::Human,
            EventKind::TaskDropped {
                task: id.into(),
                reason: "changed my mind".into(),
            },
        );
    }

    fn worker(&self) -> Worker {
        let closed = Arc::clone(&self.closed);
        let forge: SharedForge = Arc::new(NoForge);
        let start: shprd_server::Starter = Arc::new(|_: &Path, _: &str| Ok(String::new()));
        let close: shprd_server::WorkerCloser = Arc::new(move |_: &Path, _: &str| {
            closed.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        Worker {
            repo: self.repo.clone(),
            home: self.home.clone(),
            state_dir: shprd_server::state_dir(&shprd_server::state_home(&self.home, None)),
            config: Config::default(),
            facts: self.windows.clone(),
            fallback: None,
            forge,
            local: false,
            start,
            review: None,
            close_reviewer: None,
            switch_reviewers: None,
            close_worker: Some(close),
            actor: Actor::Engine,
            after_write: None,
            hub: Hub::new(),
            news: None,
        }
    }

    fn pass(&self) {
        self.worker().tick().expect("the pass runs");
    }

    fn branch_exists(&self, branch: &str) -> bool {
        let out = Command::new("git")
            .current_dir(&self.repo)
            .args(["branch", "--list", branch])
            .output()
            .expect("git");
        !String::from_utf8_lossy(&out.stdout).trim().is_empty()
    }

    fn archive_refs(&self) -> String {
        let out = Command::new("git")
            .current_dir(&self.repo)
            .args(["for-each-ref", "refs/shprd/archive"])
            .output()
            .expect("git");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn cards(&self) -> Vec<String> {
        self.state()
            .escalations
            .into_iter()
            .map(|escalation| escalation.body)
            .collect()
    }
}

#[test]
fn a_landed_task_is_cleaned_once_its_windows_have_closed() {
    let fixture = Fixture::new();
    let (path, branch) = fixture.task("T1");
    fixture.land("T1");
    assert_eq!(fixture.state().tasks["T1"].status, TaskStatus::Integrated);

    // A window is still open: the pass closes it and leaves the files alone.
    fixture.windows.live.lock().unwrap().insert("T1".into());
    fixture.pass();
    assert_eq!(
        fixture.closed.load(Ordering::SeqCst),
        1,
        "the window is closed"
    );
    assert!(path.exists(), "nothing is removed under an open window");

    // Gone: the next pass removes the worktree (dirty, with its build output),
    // archives and deletes the branch.
    fixture.windows.live.lock().unwrap().clear();
    fixture.pass();
    assert!(
        !path.exists(),
        "the worktree is gone: {:?}",
        shprd_server::clean::CleanFailures::read(
            &shprd_server::state_dir(&shprd_server::state_home(&fixture.home, None)),
            &fixture.repo
        )
    );
    assert!(!fixture.branch_exists(&branch), "the branch is deleted");
    assert!(
        fixture.archive_refs().contains("refs/shprd/archive/r1/T1"),
        "the branch was archived first: {}",
        fixture.archive_refs()
    );
    let removed: Vec<_> = fixture
        .events()
        .into_iter()
        .filter(|event| {
            matches!(
                event.kind,
                EventKind::Resource {
                    action: ResourceAction::Removed,
                    ..
                }
            )
        })
        .collect();
    assert_eq!(removed.len(), 2, "the removals are in the ledger");
    assert!(fixture.cards().is_empty(), "a clean pass raises no card");
}

#[test]
fn a_dropped_task_is_cleaned_too() {
    let fixture = Fixture::new();
    let (path, branch) = fixture.task("T1");
    fixture.drop_task("T1");
    fixture.pass();
    assert!(!path.exists());
    assert!(!fixture.branch_exists(&branch));
}

#[test]
fn a_working_task_and_one_with_an_open_card_are_never_touched() {
    let fixture = Fixture::new();
    let (working, working_branch) = fixture.task("T1");
    let (asked, asked_branch) = fixture.task("T2");
    fixture.drop_task("T2");
    fixture.append(
        Actor::Engine,
        EventKind::AskRaised {
            task: "T2".into(),
            ask: "q1".into(),
            ask_kind: AskKind::Question,
            text: "keep it?".into(),
            options: vec![],
            created_at_ms: u64::MAX / 4,
            expires_at_ms: None,
        },
    );
    for _ in 0..3 {
        fixture.pass();
    }
    assert!(working.exists() && fixture.branch_exists(&working_branch));
    assert!(asked.exists() && fixture.branch_exists(&asked_branch));
    assert_eq!(fixture.closed.load(Ordering::SeqCst), 0);
}

#[test]
fn a_failed_removal_is_retried_and_then_carded_once() {
    let fixture = Fixture::new();
    let (path, _branch) = fixture.task("T1");
    fixture.drop_task("T1");
    // A folder nothing can be deleted from: git and the filesystem both refuse.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o555)).expect("chmod");

    fixture.pass();
    fixture.pass();
    assert!(fixture.cards().is_empty(), "two failures are only retried");
    fixture.pass();
    let cards = fixture.cards();
    assert_eq!(
        cards.len(),
        1,
        "the third failure is one card: {cards:?} {:?}",
        shprd_server::clean::CleanFailures::read(
            &shprd_server::state_dir(&shprd_server::state_home(&fixture.home, None)),
            &fixture.repo
        )
    );
    assert!(
        cards[0].contains(&path.display().to_string()),
        "the card names the path: {}",
        cards[0]
    );
    assert!(cards[0].contains("Git said"), "{}", cards[0]);
    // A run-wide card, so it is the coordinator's and no task row reads For you.
    assert!(
        fixture
            .state()
            .escalations
            .iter()
            .all(|escalation| escalation.task.is_none())
    );
    fixture.pass();
    assert_eq!(fixture.cards().len(), 1, "it is not raised again");
    assert!(path.exists(), "still there, and still tried");

    // Once the path is free the retry succeeds.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    fixture.pass();
    assert!(!path.exists(), "the retry removed it");
}

#[test]
fn with_auto_clean_off_nothing_is_touched() {
    let fixture = Fixture::new();
    fixture.settings("version = 1\n[tasks]\nauto_clean = false\n");
    let (path, branch) = fixture.task("T1");
    fixture.land("T1");
    fixture.windows.live.lock().unwrap().insert("T2".into());
    fixture.pass();
    fixture.pass();
    assert!(path.exists() && fixture.branch_exists(&branch));
    assert_eq!(fixture.closed.load(Ordering::SeqCst), 0);
}
