//! A worker's own verbs, run from the worktree it was launched in.
//!
//! A worker starts in a linked git worktree, with `$SHPRD_REPO` naming that
//! worktree and its working directory inside it, and runs `shprd` itself for
//! everything the brief asks of it: `task status`, the hand-in, the ask hook.
//! The run's log and config live in the repository the worktree was cut from,
//! so each of these tests drives the real binary from inside a worktree, the way
//! a worker does, and checks that the run it reaches is the one the service
//! serves.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

mod support;

use support::isolate;

/// The binary under test, built by cargo for this integration test.
const BIN: &str = env!("CARGO_BIN_EXE_shprd");

/// A repository, a linked worktree of it, and a home.
struct Fixture {
    /// Keeps the temp directory alive.
    _temp: TempDir,
    /// The repository the run works on: the main worktree.
    root: PathBuf,
    /// The home whose `.shprd` holds shprd's own state.
    home: PathBuf,
    /// The linked worktree a worker would be launched in.
    worktree: PathBuf,
}

impl Fixture {
    /// A repository with `main`, a seed commit and one linked worktree.
    fn new() -> Option<Fixture> {
        if !git_available() {
            eprintln!("skipped: git is not on PATH");
            return None;
        }
        let temp = match TempDir::new() {
            Ok(temp) => temp,
            Err(err) => panic!("tempdir: {err}"),
        };
        let root = temp.path().join("repo");
        let home = temp.path().join("home");
        let worktree = temp.path().join("wt").join("T1");
        mkdir(&root);
        mkdir(&home);
        let fixture = Fixture {
            _temp: temp,
            root,
            home,
            worktree,
        };
        fixture.git(&fixture.root, &["init", "-b", "main"]);
        fixture.git(&fixture.root, &["config", "user.name", "shprd tests"]);
        fixture.git(
            &fixture.root,
            &["config", "user.email", "tests@example.invalid"],
        );
        fixture.git(&fixture.root, &["config", "commit.gpgsign", "false"]);
        write(
            &fixture.root.join("calc.py"),
            "def add(a, b):\n    return a + b\n",
        );
        fixture.git(&fixture.root, &["add", "--", "calc.py"]);
        fixture.git(&fixture.root, &["commit", "-m", "seed"]);
        let path = fixture.worktree.display().to_string();
        fixture.git(
            &fixture.root,
            &["worktree", "add", &path, "-b", "shprd/r1/T1"],
        );
        Some(fixture)
    }

    /// Runs git in `dir`, asserting success, and returns its stdout.
    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let output = match Command::new("git").args(args).current_dir(dir).output() {
            Ok(output) => output,
            Err(err) => panic!("git {args:?}: {err}"),
        };
        assert!(
            output.status.success(),
            "git {args:?} in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    /// A command run in the repository root: the service's own spelling.
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
        isolate(&mut command, &self.home);
        command
            .args(&all)
            .env("SHPRD_HOME", &self.home)
            // The fixture's own XDG directories: no verb here may write into
            // the machine's real config, state or data home.
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("XDG_STATE_HOME", self.home.join(".local").join("state"))
            .env("XDG_DATA_HOME", self.home.join(".local").join("share"))
            .env_remove("SHPRD_REPO")
            .env_remove("SHPRD_ACTOR");
        command
    }

    /// A command run from inside the worker's worktree, with the environment a
    /// launch gives it, and no `--repo`.
    fn worker_command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(BIN);
        isolate(&mut command, &self.home);
        command
            .args(["--no-dagr"])
            .args(args)
            .current_dir(&self.worktree)
            .env("SHPRD_HOME", &self.home)
            .env("SHPRD_REPO", &self.worktree)
            .env("SHPRD_ACTOR", "agent:pi")
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("XDG_STATE_HOME", self.home.join(".local").join("state"))
            .env("XDG_DATA_HOME", self.home.join(".local").join("share"))
            .env_remove("SHPRD_DATA_DIR");
        command
    }

    /// Runs the binary against the repository and asserts it succeeded.
    fn ok(&self, args: &[&str]) -> String {
        let output = self.command(args).output();
        succeeded(args, output)
    }

    /// Runs a verb from inside the worktree and asserts it succeeded.
    fn worker_ok(&self, args: &[&str]) -> String {
        let output = self.worker_command(args).output();
        succeeded(args, output)
    }

    /// Starts run `r1` and adds task `T1` to it, from the repository root.
    fn started_run(&self) {
        self.ok(&[
            "run",
            "start",
            "--run-id",
            "r1",
            "--base",
            "main",
            "--integration",
            "integration/m1",
        ]);
        self.ok(&[
            "agent",
            "task",
            "add",
            "--id",
            "T1",
            "--title",
            "add a sum helper",
            "--scope",
            "calc.py",
            "--acceptance",
            "add(a, b) returns a + b",
        ]);
    }

    /// Every event of the run's log, in order.
    fn events(&self) -> Vec<Value> {
        let path = self.root.join(".shprd").join("events.jsonl");
        let body = match std::fs::read_to_string(&path) {
            Ok(body) => body,
            Err(err) => panic!("read {}: {err}", path.display()),
        };
        body.lines()
            .map(|line| match serde_json::from_str::<Value>(line) {
                Ok(value) => value,
                Err(err) => panic!("bad event line {line:?}: {err}"),
            })
            .collect()
    }
}

/// Runs a command and asserts it succeeded, returning stdout.
fn succeeded(args: &[&str], output: std::io::Result<Output>) -> String {
    let output = match output {
        Ok(output) => output,
        Err(err) => panic!("running {BIN} {args:?}: {err}"),
    };
    assert!(
        output.status.success(),
        "{args:?} failed ({:?})\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

/// Whether git is on this machine; without it a worktree cannot exist.
fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
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

#[test]
fn a_worker_verb_with_shprd_repo_set_to_its_worktree_finds_the_run() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    fixture.started_run();

    let stdout = fixture.worker_ok(&[
        "task",
        "status",
        "T1",
        "--msg",
        "adding sum() with a unit test",
        "--progress",
        "20",
    ]);
    assert!(stdout.contains("task_status"), "{stdout}");

    // The event went to the run the service reads, not to a log of the
    // worktree's own: a worktree holds no `.shprd`, and the last event of the
    // repository's log is the worker's.
    assert!(
        !fixture.worktree.join(".shprd").exists(),
        "the worktree grew a log of its own"
    );
    let last = fixture
        .events()
        .pop()
        .unwrap_or_else(|| panic!("the run's log is empty"));
    assert_eq!(
        last.get("kind").and_then(Value::as_str),
        Some("task_status"),
        "{last}"
    );
    assert_eq!(last.get("task").and_then(Value::as_str), Some("T1"));
    assert_eq!(
        last.get("actor")
            .and_then(|actor| actor.get("id"))
            .and_then(Value::as_str),
        Some("pi"),
        "{last}"
    );
}

#[test]
fn a_worker_verb_finds_the_run_from_the_worktree_alone() {
    // The same walk a worker's `shprd` takes with no `$SHPRD_REPO` at all: the
    // working directory is the worktree, and git names the worktree, not the
    // run.
    let Some(fixture) = Fixture::new() else {
        return;
    };
    fixture.started_run();

    let mut command =
        fixture.worker_command(&["task", "status", "T1", "--msg", "reading the brief"]);
    command.env_remove("SHPRD_REPO");
    let output = command.output();
    succeeded(&["task", "status", "T1"], output);

    let last = fixture
        .events()
        .pop()
        .unwrap_or_else(|| panic!("the run's log is empty"));
    assert_eq!(
        last.get("task").and_then(Value::as_str),
        Some("T1"),
        "{last}"
    );
}

#[test]
fn a_worker_reads_the_run_status_from_its_worktree() {
    let Some(fixture) = Fixture::new() else {
        return;
    };
    fixture.started_run();

    let stdout = fixture.worker_ok(&["status"]);
    // `shprd status` prints the derived word (M8.5's rules) for each task, so a
    // task nothing has started on reads "To do" rather than its stored
    // "queued". The test is that the worktree reached the run's log, so the
    // line has to name the run's task and its title.
    assert!(
        stdout.contains("○ T1 To do · add a sum helper"),
        "the worktree's status is the run's: {stdout}"
    );
}

#[test]
fn the_repository_root_still_works_the_way_it_did() {
    // Backward compatibility: `--repo` (and `$SHPRD_REPO`) naming the main
    // worktree is unchanged, from anywhere.
    let Some(fixture) = Fixture::new() else {
        return;
    };
    fixture.started_run();

    let output = fixture
        .command(&["task", "status", "T1", "--msg", "from the root"])
        .current_dir(fixture._temp.path())
        .output();
    succeeded(&["task", "status", "T1"], output);

    let mut named = Command::new(BIN);
    isolate(&mut named, &fixture.home);
    let output = named
        .args(["--no-dagr", "task", "status", "T1", "--msg", "by variable"])
        .current_dir(fixture._temp.path())
        .env("SHPRD_HOME", &fixture.home)
        .env("SHPRD_REPO", &fixture.root)
        .env("XDG_CONFIG_HOME", fixture.home.join(".config"))
        .env("XDG_STATE_HOME", fixture.home.join(".local").join("state"))
        .env("XDG_DATA_HOME", fixture.home.join(".local").join("share"))
        .env_remove("SHPRD_ACTOR")
        .output();
    succeeded(&["task", "status", "T1"], output);

    assert_eq!(
        fixture
            .events()
            .last()
            .and_then(|event| event.get("kind"))
            .and_then(Value::as_str),
        Some("task_status")
    );
}
