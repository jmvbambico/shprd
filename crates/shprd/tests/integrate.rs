//! End-to-end tests for `shprd agent integrate`, against a real repository.
//!
//! No multiplexer is involved: the attempt is recorded with `spawn
//! --record-only`, so what is under test is the git work and the ledger.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

/// The binary under test, built by cargo for this integration test.
const BIN: &str = env!("CARGO_BIN_EXE_shprd");

/// The branch shprd creates for a run's integration work.
const INT_BRANCH: &str = "shprd/r1/int";

/// A repository, a run home, and the temp directory that keeps both alive.
struct Fixture {
    /// Keeps the temp directory alive.
    _temp: TempDir,
    /// The repository the run works on.
    root: PathBuf,
    /// The home whose `.shprd` holds worktrees and locks.
    home: PathBuf,
}

/// Creates a directory, panicking on failure.
fn mkdir(path: &Path) {
    if let Err(err) = std::fs::create_dir_all(path) {
        panic!("mkdir {}: {err}", path.display());
    }
}

/// Writes a file, panicking on failure.
fn write(path: &Path, body: &str) {
    if let Err(err) = std::fs::write(path, body) {
        panic!("write {}: {err}", path.display());
    }
}

impl Fixture {
    /// A repository with `main`, a seed commit, and one file to fight over.
    fn new() -> Fixture {
        let temp = match TempDir::new() {
            Ok(temp) => temp,
            Err(err) => panic!("tempdir: {err}"),
        };
        let root = temp.path().join("repo");
        let home = temp.path().join("home");
        mkdir(&root);
        mkdir(&home);
        let fixture = Fixture {
            _temp: temp,
            root,
            home,
        };
        fixture.git(&["init", "-b", "main"]);
        fixture.git(&["config", "user.name", "shprd tests"]);
        fixture.git(&["config", "user.email", "tests@example.invalid"]);
        fixture.git(&["config", "commit.gpgsign", "false"]);
        write(&fixture.root.join("a.txt"), "one\n");
        write(&fixture.root.join("b.txt"), "one\n");
        fixture.git(&["add", "--", "a.txt", "b.txt"]);
        fixture.git(&["commit", "-m", "seed"]);
        fixture
    }

    /// Runs git in the repository, asserting success, and returns its output.
    fn git(&self, args: &[&str]) -> String {
        let output = match Command::new("git")
            .args(args)
            .current_dir(&self.root)
            .output()
        {
            Ok(output) => output,
            Err(err) => panic!("git {args:?}: {err}"),
        };
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    /// A command that runs the binary against this fixture, unstarted.
    fn command(&self, args: &[&str]) -> Command {
        let mut all: Vec<String> = vec![
            "--repo".to_string(),
            self.root.display().to_string(),
            "--home".to_string(),
            self.home.display().to_string(),
            "--no-dagr".to_string(),
        ];
        all.extend(args.iter().map(|arg| (*arg).to_string()));
        let mut command = Command::new(BIN);
        command.args(&all);
        command
    }

    /// Runs the binary against this fixture.
    fn run(&self, args: &[&str]) -> Output {
        match self.command(args).output() {
            Ok(output) => output,
            Err(err) => panic!("running {BIN}: {err}"),
        }
    }

    /// Runs the binary and asserts it succeeded.
    fn ok(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "{args:?} failed ({:?}): {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).to_string()
    }

    /// Starts run `r1`, whose integration work lands on `main`'s tip.
    fn start(&self) {
        self.ok(&[
            "run",
            "start",
            "--run-id",
            "r1",
            "--base",
            "main",
            "--integration",
            "main",
        ]);
    }

    /// The folded state as JSON.
    fn state(&self) -> serde_json::Value {
        let text = self.ok(&["state", "--json"]);
        match serde_json::from_str(&text) {
            Ok(value) => value,
            Err(err) => panic!("state was not JSON ({err}): {text}"),
        }
    }

    /// The status string of one task in the folded state.
    fn status(&self, id: &str) -> String {
        let state = self.state();
        let tasks = match state.get("tasks").and_then(|tasks| tasks.as_array()) {
            Some(tasks) => tasks.clone(),
            None => panic!("no tasks in {state}"),
        };
        for task in tasks {
            if task.get("id").and_then(|id| id.as_str()) == Some(id) {
                return task
                    .get("status")
                    .and_then(|status| status.as_str())
                    .unwrap_or("<missing>")
                    .to_string();
            }
        }
        panic!("task {id} missing from {state}");
    }

    /// Commits one rewrite of `file` on `branch`, cut from `main`.
    ///
    /// A `branch` of `main` commits straight onto it, which is how a test makes
    /// the integration branch disagree with a task.
    fn commit_on(&self, branch: &str, file: &str, body: &str, message: &str) {
        if branch == "main" {
            write(&self.root.join(file), body);
            self.git(&["add", "--", file]);
            self.git(&["commit", "-m", message]);
            return;
        }
        self.git(&["checkout", "-b", branch, "main"]);
        write(&self.root.join(file), body);
        self.git(&["add", "--", file]);
        self.git(&["commit", "-m", message]);
        self.git(&["checkout", "main"]);
    }

    /// Drives task `id` from added to passed, recording `branch` as its own.
    fn passed_task(&self, id: &str, branch: &str, scope: &str) {
        self.ok(&[
            "agent",
            "task",
            "add",
            "--id",
            id,
            "--title",
            "a task",
            "--scope",
            scope,
            "--acceptance",
            "it works",
        ]);
        self.ok(&[
            "agent",
            "spawn",
            "--record-only",
            "--task",
            id,
            "--attempt",
            "1",
            "--agent",
            "w1",
            "--worktree",
            "/tmp/wt",
            "--branch",
            branch,
        ]);
        self.ok(&[
            "agent",
            "report",
            "--task",
            id,
            "--attempt",
            "1",
            "--status",
            "completed",
            "--summary",
            "done",
            "--changed",
            scope,
        ]);
        self.ok(&[
            "agent",
            "gate",
            "pass",
            "--task",
            id,
            "--attempt",
            "1",
            "--gate",
            "lint",
        ]);
    }

    /// The run's event log as text.
    fn log_text(&self) -> String {
        let path = self.root.join(".shprd").join("events.jsonl");
        match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) => panic!("read {}: {err}", path.display()),
        }
    }
}

#[test]
fn a_passed_task_lands_on_the_integration_branch() {
    let fixture = Fixture::new();
    fixture.start();
    fixture.commit_on("feature/t1", "a.txt", "feature\n", "the task's work");
    fixture.passed_task("T1", "feature/t1", "a.txt");

    let printed = fixture.ok(&["agent", "integrate", "--task", "T1"]);
    assert!(
        printed.contains("integrated T1: 1 commit(s), head "),
        "{printed}"
    );

    assert_eq!(
        fixture.git(&["log", "-1", "--format=%s", INT_BRANCH]),
        "the task's work",
        "the pick did not land on the integration branch"
    );
    assert_eq!(fixture.status("T1"), "integrated");
    assert!(fixture.log_text().contains("\"kind\":\"integrated\""));
}

/// Runs git in `dir`, asserting success, and returns its trimmed output.
fn git_at(dir: &Path, args: &[&str]) -> String {
    let output = match Command::new("git").args(args).current_dir(dir).output() {
        Ok(output) => output,
        Err(err) => panic!("git {args:?}: {err}"),
    };
    assert!(
        output.status.success(),
        "git {args:?} in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[test]
fn a_conflicting_pick_sends_the_task_back() {
    let fixture = Fixture::new();
    fixture.start();
    fixture.commit_on("feature/t1", "a.txt", "feature\n", "the task's work");
    fixture.commit_on("main", "a.txt", "main moved on\n", "main moved on");
    fixture.passed_task("T1", "feature/t1", "a.txt");
    let main_before = fixture.git(&["rev-parse", "main"]);

    let output = fixture.run(&["agent", "integrate", "--task", "T1"]);
    assert_eq!(
        output.status.code(),
        Some(2),
        "expected exit 2, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("conflict"));
    assert_eq!(fixture.status("T1"), "failed", "the task was not sent back");

    let log = fixture.log_text();
    assert!(log.contains("\"gate\":\"integrate\""), "{log}");
    assert!(
        log.contains("conflicts with the integration branch"),
        "{log}"
    );

    // The integration worktree is left where it was, with nothing half-picked.
    let worktree = fixture
        .home
        .join(".shprd")
        .join("wt")
        .join("repo")
        .join("r1")
        .join("_int");
    assert_eq!(git_at(&worktree, &["status", "--porcelain"]), "");
    assert_eq!(git_at(&worktree, &["rev-parse", "HEAD"]), main_before);
}

#[test]
fn two_integrates_on_one_run_take_turns() {
    let fixture = Fixture::new();
    fixture.start();
    fixture.commit_on("feature/t1", "a.txt", "first\n", "first work");
    fixture.passed_task("T1", "feature/t1", "a.txt");
    fixture.commit_on("feature/t2", "b.txt", "second\n", "second work");
    fixture.passed_task("T2", "feature/t2", "b.txt");

    let mut first = match fixture
        .command(&["agent", "integrate", "--task", "T1"])
        .spawn()
    {
        Ok(child) => child,
        Err(err) => panic!("spawn: {err}"),
    };
    let mut second = match fixture
        .command(&["agent", "integrate", "--task", "T2"])
        .spawn()
    {
        Ok(child) => child,
        Err(err) => panic!("spawn: {err}"),
    };
    let first = match first.wait() {
        Ok(status) => status,
        Err(err) => panic!("wait: {err}"),
    };
    let second = match second.wait() {
        Ok(status) => status,
        Err(err) => panic!("wait: {err}"),
    };
    assert!(first.success(), "the first integrate failed");
    assert!(second.success(), "the second integrate failed");
    assert_eq!(fixture.status("T1"), "integrated");
    assert_eq!(fixture.status("T2"), "integrated");

    let log = fixture.git(&["log", "--format=%s", INT_BRANCH]);
    assert!(log.contains("first work"), "{log}");
    assert!(log.contains("second work"), "{log}");
}
