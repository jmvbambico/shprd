//! End-to-end tests for the writing verbs: they drive the real binary against
//! a throwaway repo root and read the log the commands produce.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use shprd_core::event::{Actor, Event, EventKind, ResourceAction, ResourceKind};
use tempfile::TempDir;

/// The binary under test, built by cargo for this integration test.
const BIN: &str = env!("CARGO_BIN_EXE_shprd");

/// A throwaway repo root that acts as `--repo`.
fn repo() -> TempDir {
    match TempDir::new() {
        Ok(dir) => dir,
        Err(err) => panic!("tempdir failed: {err}"),
    }
}

/// Runs the binary with `--repo <root>` and the given arguments.
fn run(root: &Path, args: &[&str]) -> Output {
    let mut all: Vec<String> = vec!["--repo".into(), root.display().to_string()];
    all.extend(args.iter().map(|arg| (*arg).to_string()));
    match Command::new(BIN).args(&all).output() {
        Ok(output) => output,
        Err(err) => panic!("running {BIN} failed: {err}"),
    }
}

/// Runs the binary and asserts it succeeded, returning stdout.
fn ok(root: &Path, args: &[&str]) -> String {
    let output = run(root, args);
    assert!(
        output.status.success(),
        "expected success for {args:?}, got {:?}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

/// Runs the binary and asserts it exited 2 with a `refused:` line.
fn refused(root: &Path, args: &[&str]) -> String {
    let output = run(root, args);
    assert_eq!(
        output.status.code(),
        Some(2),
        "expected exit 2 for {args:?}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        stderr.contains("refused:"),
        "expected a refused: line for {args:?}, got {stderr:?}"
    );
    stderr
}

/// Starts a run in `root`.
fn start(root: &Path) {
    ok(
        root,
        &[
            "run",
            "start",
            "--run-id",
            "t1",
            "--base",
            "main",
            "--integration",
            "integration/m1",
        ],
    );
}

/// Adds a task with one scope path and one acceptance line.
fn add_task(root: &Path, id: &str, scope: &str) {
    ok(
        root,
        &[
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
        ],
    );
}

/// Spawns an attempt for `id`.
fn spawn(root: &Path, id: &str) {
    ok(
        root,
        &[
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
            "feature/x",
        ],
    );
}

/// The folded state as JSON.
fn state(root: &Path) -> Value {
    let text = ok(root, &["state", "--json"]);
    match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(err) => panic!("state was not JSON ({err}): {text}"),
    }
}

/// `state --json` path for a task's status string.
fn status_of<'a>(state: &'a Value, id: &str) -> &'a str {
    let tasks = match state.get("tasks").and_then(Value::as_array) {
        Some(tasks) => tasks,
        None => panic!("no tasks array in {state}"),
    };
    for task in tasks {
        if task.get("id").and_then(Value::as_str) == Some(id) {
            return task
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("<missing>");
        }
    }
    panic!("task {id} missing from {state}");
}

/// The log file a run root should hold.
fn log_path(root: &Path) -> PathBuf {
    root.join(".shprd").join("events.jsonl")
}

#[test]
fn happy_path_reaches_integrated() {
    let dir = repo();
    let root = dir.path();
    start(root);
    add_task(root, "T1", "crates/shprd");

    let spawned = run(
        root,
        &[
            "agent",
            "spawn",
            "--record-only",
            "--task",
            "T1",
            "--attempt",
            "1",
            "--agent",
            "w1/claude-opus-5-5",
            "--worktree",
            "/tmp/wt",
            "--branch",
            "feature/x",
            "--json",
        ],
    );
    assert!(spawned.status.success());
    let event: Value = serde_json::from_slice(&spawned.stdout).unwrap_or(Value::Null);
    assert_eq!(event.get("kind").and_then(Value::as_str), Some("spawned"));
    assert!(event.get("seq").is_some(), "event carried no seq: {event}");

    ok(
        root,
        &[
            "agent",
            "report",
            "--task",
            "T1",
            "--attempt",
            "1",
            "--status",
            "completed",
            "--summary",
            "did it",
            "--test",
            "cargo test",
            "--changed",
            "crates/shprd/src/main.rs",
            "--commit",
            "abc123",
            "--skill-used",
            "tdd",
        ],
    );
    ok(
        root,
        &[
            "agent",
            "gate",
            "pass",
            "--task",
            "T1",
            "--attempt",
            "1",
            "--gate",
            "lint",
        ],
    );
    ok(
        root,
        &["agent", "integrate", "--task", "T1", "--record-only"],
    );

    let state = state(root);
    assert_eq!(status_of(&state, "T1"), "integrated");
    assert_eq!(
        state.get("run").and_then(|run| run.get("run_id")),
        Some(&Value::String("t1".into()))
    );
    assert!(log_path(root).exists(), "no log file was written");
}

#[test]
fn overlapping_scope_is_refused() {
    let dir = repo();
    let root = dir.path();
    start(root);
    add_task(root, "T1", "crates/shprd");
    add_task(root, "T2", "crates/shprd/src");
    spawn(root, "T1");

    let stderr = refused(
        root,
        &[
            "agent",
            "spawn",
            "--record-only",
            "--task",
            "T2",
            "--attempt",
            "1",
            "--agent",
            "w2",
            "--worktree",
            "/tmp/wt2",
            "--branch",
            "feature/y",
        ],
    );
    assert!(stderr.contains("T2"), "refusal named no task: {stderr}");
    assert_eq!(status_of(&state(root), "T2"), "queued");
}

#[test]
fn changed_path_outside_scope_is_refused() {
    let dir = repo();
    let root = dir.path();
    start(root);
    add_task(root, "T1", "crates/shprd");
    spawn(root, "T1");

    let stderr = refused(
        root,
        &[
            "agent",
            "report",
            "--task",
            "T1",
            "--attempt",
            "1",
            "--status",
            "completed",
            "--summary",
            "did it",
            "--changed",
            "docs/plan.md",
        ],
    );
    assert!(stderr.contains("docs/plan.md"), "refusal was {:?}", stderr);
    assert_eq!(status_of(&state(root), "T1"), "working");
}

#[test]
fn verbs_before_run_start_are_refused() {
    let dir = repo();
    let root = dir.path();
    refused(
        root,
        &[
            "agent",
            "task",
            "add",
            "--id",
            "T1",
            "--title",
            "t",
            "--scope",
            "crates/shprd",
            "--acceptance",
            "ok",
        ],
    );
    refused(
        root,
        &["agent", "escalate", "--body", "b", "--default", "d"],
    );
    refused(root, &["run", "finish"]);
    assert!(
        !log_path(root).exists(),
        "a refused verb wrote to the log anyway"
    );
}

/// Runs the binary with `--repo <root>`, `PATH` set to `bin_dir` and
/// `SHPRD_DAGR_BIN` unset, so no dagr checker can be found.
fn run_with_path(root: &Path, args: &[&str], bin_dir: &Path) -> Output {
    let mut all: Vec<String> = vec!["--repo".into(), root.display().to_string()];
    all.extend(args.iter().map(|arg| (*arg).to_string()));
    match Command::new(BIN)
        .args(&all)
        .env("PATH", bin_dir)
        .env_remove("SHPRD_DAGR_BIN")
        .output()
    {
        Ok(output) => output,
        Err(err) => panic!("running {BIN} failed: {err}"),
    }
}

/// [`run_with_path`], asserting success and returning stdout.
fn ok_with_path(root: &Path, args: &[&str], bin_dir: &Path) -> String {
    let output = run_with_path(root, args, bin_dir);
    assert!(
        output.status.success(),
        "expected success for {args:?}, got {:?}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

#[test]
fn a_scripted_run_writes_the_document_and_the_views() {
    let dir = repo();
    let root = dir.path();
    // An empty directory stands in for PATH: no dagr can be found there.
    let empty = repo();
    let bin_dir = empty.path();

    ok_with_path(
        root,
        &[
            "run",
            "start",
            "--run-id",
            "t1",
            "--base",
            "main",
            "--integration",
            "integration/m1",
        ],
        bin_dir,
    );
    ok_with_path(
        root,
        &[
            "agent",
            "task",
            "add",
            "--id",
            "T1",
            "--title",
            "a task",
            "--scope",
            "crates/shprd",
            "--acceptance",
            "it works",
        ],
        bin_dir,
    );
    ok_with_path(
        root,
        &[
            "agent",
            "spawn",
            "--record-only",
            "--task",
            "T1",
            "--attempt",
            "1",
            "--agent",
            "w1",
            "--worktree",
            "/tmp/wt",
            "--branch",
            "feature/x",
        ],
        bin_dir,
    );
    ok_with_path(
        root,
        &[
            "agent",
            "report",
            "--task",
            "T1",
            "--attempt",
            "1",
            "--status",
            "completed",
            "--summary",
            "did it",
        ],
        bin_dir,
    );
    ok_with_path(
        root,
        &[
            "agent",
            "gate",
            "pass",
            "--task",
            "T1",
            "--attempt",
            "1",
            "--gate",
            "lint",
        ],
        bin_dir,
    );
    ok_with_path(
        root,
        &["agent", "integrate", "--task", "T1", "--record-only"],
        bin_dir,
    );
    ok_with_path(
        root,
        &[
            "agent",
            "override",
            "--rule",
            "no_main_push",
            "--reason",
            "release branch",
        ],
        bin_dir,
    );

    let doc_path = root.join(".dagr").join("run.json");
    let text = match std::fs::read_to_string(&doc_path) {
        Ok(text) => text,
        Err(err) => panic!("{} was not written: {err}", doc_path.display()),
    };
    let doc: Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(err) => panic!("run.json is not JSON ({err}): {text}"),
    };
    assert_eq!(doc.get("dagr").and_then(Value::as_u64), Some(3), "{text}");

    let brief = ok_with_path(root, &["brief"], bin_dir);
    assert!(brief.starts_with("run "), "brief was {brief:?}");

    let status = ok_with_path(root, &["status"], bin_dir);
    assert!(status.contains("T1"), "status was {status:?}");
    assert!(status.contains('✓'), "status was {status:?}");

    let body = ok_with_path(root, &["pr-body"], bin_dir);
    assert!(body.contains("## Tasks"), "pr-body was {body:?}");
    assert!(
        body.contains("- run-wide `no_main_push`: release branch"),
        "a run-wide override must reach the PR body: {body:?}"
    );
}

#[test]
fn bare_shprd_prints_the_status_line() {
    let dir = repo();
    let root = dir.path();
    let empty = repo();
    let bin_dir = empty.path();
    ok_with_path(
        root,
        &[
            "run",
            "start",
            "--run-id",
            "t1",
            "--base",
            "main",
            "--integration",
            "integration/m1",
        ],
        bin_dir,
    );
    ok_with_path(
        root,
        &[
            "agent",
            "task",
            "add",
            "--id",
            "T1",
            "--title",
            "a task",
            "--scope",
            "crates/shprd",
            "--acceptance",
            "it works",
        ],
        bin_dir,
    );
    let bare = ok_with_path(root, &[], bin_dir);
    let status = ok_with_path(root, &["status"], bin_dir);
    assert_eq!(bare, status, "bare shprd must equal shprd status");
}

#[test]
fn dagr_sync_checks_the_document_when_a_checker_is_given() {
    let checker =
        Path::new("/Users/cryogenix/scratch/mh-verify/repos/herdr-dagr/target/release/dagr");
    if !checker.exists() {
        eprintln!("skipping: no dagr at {}", checker.display());
        return;
    }
    let dir = repo();
    let root = dir.path();
    start(root);
    add_task(root, "T1", "crates/shprd");
    let output = Command::new(BIN)
        .args(["--repo", &root.display().to_string(), "dagr", "sync"])
        .env("SHPRD_DAGR_BIN", checker)
        .output();
    let output = match output {
        Ok(output) => output,
        Err(err) => panic!("running {BIN} failed: {err}"),
    };
    assert_eq!(
        output.status.code(),
        Some(0),
        "dagr sync failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(root.join(".dagr").join("run.json").exists());
}

#[test]
fn a_failing_sync_does_not_undo_a_write() {
    let dir = repo();
    let root = dir.path();
    // A checker that always refuses: `/bin/false`.
    let output = Command::new(BIN)
        .args([
            "--repo",
            &root.display().to_string(),
            "run",
            "start",
            "--run-id",
            "t1",
            "--base",
            "main",
            "--integration",
            "integration/m1",
        ])
        .env("SHPRD_DAGR_BIN", "/bin/false")
        .output();
    let output = match output {
        Ok(output) => output,
        Err(err) => panic!("running {BIN} failed: {err}"),
    };
    assert_eq!(
        output.status.code(),
        Some(0),
        "the write must still succeed"
    );
    assert!(log_path(root).exists(), "the event must still be written");
}

#[test]
fn dagr_sync_before_run_start_says_there_is_nothing_to_project() {
    let dir = repo();
    let root = dir.path();
    let output = run(root, &["dagr", "sync"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "a run that has not started is not an error\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        stderr.contains("no run yet; nothing to project"),
        "{stderr}"
    );
    assert!(
        !root.join(".dagr").join("run.json").exists(),
        "nothing may be projected before the run starts"
    );
}

#[test]
fn no_dagr_flag_skips_the_sync() {
    let dir = repo();
    let root = dir.path();
    ok(
        root,
        &[
            "--no-dagr",
            "run",
            "start",
            "--run-id",
            "t1",
            "--base",
            "main",
            "--integration",
            "integration/m1",
        ],
    );
    assert!(
        !root.join(".dagr").join("run.json").exists(),
        "--no-dagr must not write the document"
    );
}

/// A throwaway repository, for verbs that drive real git.
struct GitRepo {
    /// Keeps the temp directory alive.
    _temp: TempDir,
    /// The repository itself.
    path: PathBuf,
}

impl GitRepo {
    /// Initializes a repository with one commit on `main`.
    fn new() -> GitRepo {
        let temp = match TempDir::new() {
            Ok(temp) => temp,
            Err(err) => panic!("tempdir failed: {err}"),
        };
        let path = temp.path().join("repo");
        if let Err(err) = std::fs::create_dir(&path) {
            panic!("mkdir {}: {err}", path.display());
        }
        let repo = GitRepo { _temp: temp, path };
        repo.git(&["init", "-b", "main"]);
        repo.git(&["config", "user.name", "shprd tests"]);
        repo.git(&["config", "user.email", "tests@example.invalid"]);
        repo.git(&["config", "commit.gpgsign", "false"]);
        if let Err(err) = std::fs::write(repo.path.join("seed.txt"), "seed\n") {
            panic!("write seed: {err}");
        }
        repo.git(&["add", "--", "seed.txt"]);
        repo.git(&["commit", "-m", "seed"]);
        repo
    }

    /// Runs git here, returning its trimmed stdout.
    fn git(&self, args: &[&str]) -> String {
        let output = match Command::new("git")
            .args(args)
            .current_dir(&self.path)
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
        String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_string()
    }

    /// True when `rev` resolves.
    fn has_ref(&self, rev: &str) -> bool {
        match Command::new("git")
            .args(["rev-parse", "--verify", "--quiet", rev])
            .current_dir(&self.path)
            .output()
        {
            Ok(output) => output.status.success(),
            Err(err) => panic!("git rev-parse {rev}: {err}"),
        }
    }
}

/// Runs the binary with `--repo`, `--no-dagr`, `--home` and `args`.
fn run_with_home(root: &Path, home: &Path, args: &[&str]) -> Output {
    let mut all: Vec<String> = vec![
        "--repo".into(),
        root.display().to_string(),
        "--no-dagr".into(),
        "--home".into(),
        home.display().to_string(),
    ];
    all.extend(args.iter().map(|arg| (*arg).to_string()));
    match Command::new(BIN).args(&all).output() {
        Ok(output) => output,
        Err(err) => panic!("running {BIN} failed: {err}"),
    }
}

/// Runs the binary and asserts it succeeded, returning stdout.
fn ok_with_home(root: &Path, home: &Path, args: &[&str]) -> String {
    let output = run_with_home(root, home, args);
    assert!(
        output.status.success(),
        "expected success for {args:?}, got {:?}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

/// Appends `kinds` to the run log, continuing its sequence numbers.
fn append_events(root: &Path, kinds: Vec<EventKind>) {
    append_events_at(root, kinds, 1_800_000_000_000);
}

/// Appends `kinds` to the run log with `at_ms` as the first one's timestamp.
fn append_events_at(root: &Path, kinds: Vec<EventKind>, at_ms: u64) {
    let path = root.join(".shprd").join("events.jsonl");
    let existing = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) => panic!("read {}: {err}", path.display()),
    };
    let mut body = existing;
    let mut seq = body.lines().count() as u64;
    for kind in kinds {
        seq += 1;
        let event = Event {
            seq,
            at_ms: at_ms.saturating_add(seq),
            actor: Actor::Engine,
            kind,
        };
        let line = match serde_json::to_string(&event) {
            Ok(line) => line,
            Err(err) => panic!("serialize event: {err}"),
        };
        body.push_str(&line);
        body.push('\n');
    }
    if let Err(err) = std::fs::write(&path, body) {
        panic!("write {}: {err}", path.display());
    }
}

/// How many `resource` events the log holds.
fn resource_events(root: &Path) -> usize {
    let path = root.join(".shprd").join("events.jsonl");
    match std::fs::read_to_string(&path) {
        Ok(text) => text
            .lines()
            .filter(|line| line.contains("\"kind\":\"resource\""))
            .count(),
        Err(err) => panic!("read {}: {err}", path.display()),
    }
}

/// Drives one task of run `r1` from planned to integrated, against a real repo.
fn integrated_task(root: &Path, branch: &str) {
    ok(
        root,
        &[
            "run",
            "start",
            "--run-id",
            "r1",
            "--base",
            "main",
            "--integration",
            "integration/m1",
        ],
    );
    add_task(root, "T1", "crates/x");
    ok(
        root,
        &[
            "agent",
            "spawn",
            "--record-only",
            "--task",
            "T1",
            "--attempt",
            "1",
            "--agent",
            "w1",
            "--worktree",
            "/tmp/wt",
            "--branch",
            branch,
        ],
    );
    ok(
        root,
        &[
            "agent",
            "report",
            "--task",
            "T1",
            "--attempt",
            "1",
            "--status",
            "completed",
            "--summary",
            "done",
            "--changed",
            "crates/x/a.rs",
        ],
    );
    ok(
        root,
        &[
            "agent",
            "gate",
            "pass",
            "--task",
            "T1",
            "--attempt",
            "1",
            "--gate",
            "lint",
        ],
    );
    ok(
        root,
        &["agent", "integrate", "--task", "T1", "--record-only"],
    );
}

#[test]
fn janitor_dry_run_changes_nothing_and_apply_appends_resource_events() {
    let git = GitRepo::new();
    let root = git.path.clone();
    let home = match TempDir::new() {
        Ok(dir) => dir,
        Err(err) => panic!("tempdir failed: {err}"),
    };
    let branch = "shprd/r1/T1";
    integrated_task(&root, branch);
    let tip = git.git(&["rev-parse", "main"]);
    git.git(&["branch", branch, "main"]);
    append_events(
        &root,
        vec![EventKind::Resource {
            action: ResourceAction::Created,
            kind: ResourceKind::Branch,
            name: branch.to_string(),
            task: Some("T1".to_string()),
        }],
    );
    let before = resource_events(&root);

    let plan = ok_with_home(&root, home.path(), &["janitor"]);
    assert!(plan.contains("would archive"), "{plan}");
    assert!(plan.contains(branch), "{plan}");
    assert!(git.has_ref(branch), "a dry run must not delete the branch");
    assert!(
        !git.has_ref("refs/shprd/archive/r1/T1"),
        "a dry run must not create the archive ref"
    );
    assert_eq!(resource_events(&root), before, "a dry run writes nothing");

    let applied = ok_with_home(&root, home.path(), &["janitor", "--apply"]);
    assert!(applied.contains("ok "), "{applied}");
    assert!(!git.has_ref(branch), "the branch must be gone");
    assert_eq!(
        git.git(&["rev-parse", "refs/shprd/archive/r1/T1"]),
        tip,
        "the archive ref must hold the branch tip"
    );
    assert_eq!(
        resource_events(&root),
        before + 3,
        "the archive ref, the archived branch and the deleted branch"
    );

    let restored = ok_with_home(&root, home.path(), &["restore", "T1"]);
    assert!(restored.contains("ok resource"), "{restored}");
    assert_eq!(
        git.git(&["rev-parse", branch]),
        tip,
        "restore must bring the branch back at the same commit"
    );
    assert_eq!(resource_events(&root), before + 4, "restore is recorded");
}

#[test]
fn restore_refuses_a_finished_run_before_creating_the_branch() {
    let git = GitRepo::new();
    let root = git.path.clone();
    let home = match TempDir::new() {
        Ok(dir) => dir,
        Err(err) => panic!("tempdir failed: {err}"),
    };
    let branch = "shprd/r1/T1";
    integrated_task(&root, branch);
    let tip = git.git(&["rev-parse", "main"]);
    git.git(&["update-ref", "refs/shprd/archive/r1/T1", &tip]);
    // The task landed but no pull request merged, so the run only ends when a
    // human says to end it anyway.
    ok(&root, &["run", "finish", "--abandon"]);

    let output = run_with_home(&root, home.path(), &["restore", "T1"]);
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert_eq!(output.status.code(), Some(2), "stderr: {stderr}");
    assert!(stderr.contains("refused"), "stderr: {stderr}");
    assert!(
        !git.has_ref(branch),
        "a refused restore must not leave the branch behind"
    );
}

#[test]
fn janitor_leaves_a_branch_outside_the_namespace_alone() {
    let git = GitRepo::new();
    let root = git.path.clone();
    let home = match TempDir::new() {
        Ok(dir) => dir,
        Err(err) => panic!("tempdir failed: {err}"),
    };
    integrated_task(&root, "feature/x");
    git.git(&["branch", "feature/x", "main"]);
    append_events(
        &root,
        vec![EventKind::Resource {
            action: ResourceAction::Created,
            kind: ResourceKind::Branch,
            name: "feature/x".to_string(),
            task: Some("T1".to_string()),
        }],
    );

    let plan = ok_with_home(&root, home.path(), &["janitor"]);
    assert!(plan.contains("nothing to clean up"), "{plan}");
    ok_with_home(&root, home.path(), &["janitor", "--apply"]);
    assert!(
        git.has_ref("feature/x"),
        "a branch shprd did not create must survive"
    );
}

#[test]
fn janitor_without_a_run_is_an_error() {
    let git = GitRepo::new();
    let root = git.path.clone();
    let home = match TempDir::new() {
        Ok(dir) => dir,
        Err(err) => panic!("tempdir failed: {err}"),
    };
    let output = run_with_home(&root, home.path(), &["janitor"]);
    assert_eq!(output.status.code(), Some(1), "no run is not a refusal");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no run"), "{stderr}");
}

/// Milliseconds since the Unix epoch.
fn now_ms() -> u64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(elapsed) => u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
        Err(err) => panic!("system clock before the epoch: {err}"),
    }
}

/// Writes `<root>/.shprd/config.toml`.
fn write_repo_config(root: &Path, body: &str) {
    let path = root.join(".shprd").join("config.toml");
    if let Some(parent) = path.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        panic!("mkdir {}: {err}", parent.display());
    }
    if let Err(err) = std::fs::write(&path, body) {
        panic!("write {}: {err}", path.display());
    }
}

#[test]
fn janitor_takes_the_archive_window_from_the_repo_config() {
    let git = GitRepo::new();
    let root = git.path.clone();
    let home = match TempDir::new() {
        Ok(dir) => dir,
        Err(err) => panic!("tempdir failed: {err}"),
    };
    ok(
        &root,
        &[
            "run",
            "start",
            "--run-id",
            "r1",
            "--base",
            "main",
            "--integration",
            "integration/m1",
        ],
    );
    add_task(&root, "T2", "crates/x");
    let ref_name = "refs/shprd/archive/r1/T2";
    append_events_at(
        &root,
        vec![EventKind::Resource {
            action: ResourceAction::Created,
            kind: ResourceKind::ArchiveRef,
            name: ref_name.to_string(),
            task: Some("T2".to_string()),
        }],
        now_ms() - 5 * 86_400_000,
    );

    let unconfigured = ok_with_home(&root, home.path(), &["janitor"]);
    assert!(
        unconfigured.contains("nothing to clean up"),
        "the 14-day default keeps a five-day-old archive: {unconfigured}"
    );

    write_repo_config(&root, "[janitor]\narchive_days = 3\n");
    let configured = ok_with_home(&root, home.path(), &["janitor"]);
    assert!(
        configured.contains("would expire archive"),
        "a three-day window expires it: {configured}"
    );
    assert!(configured.contains(ref_name), "{configured}");

    let overridden = ok_with_home(&root, home.path(), &["janitor", "--archive-days", "30"]);
    assert!(
        overridden.contains("nothing to clean up"),
        "the flag still wins over the config: {overridden}"
    );
}

#[test]
fn janitor_takes_the_namespace_from_the_repo_config() {
    let git = GitRepo::new();
    let root = git.path.clone();
    let home = match TempDir::new() {
        Ok(dir) => dir,
        Err(err) => panic!("tempdir failed: {err}"),
    };
    let branch = "shprdtest/r1/T1";
    integrated_task(&root, branch);
    git.git(&["branch", branch, "main"]);
    append_events(
        &root,
        vec![EventKind::Resource {
            action: ResourceAction::Created,
            kind: ResourceKind::Branch,
            name: branch.to_string(),
            task: Some("T1".to_string()),
        }],
    );

    let unconfigured = ok_with_home(&root, home.path(), &["janitor"]);
    assert!(
        unconfigured.contains("nothing to clean up"),
        "the default namespace does not own {branch}: {unconfigured}"
    );

    write_repo_config(&root, "[janitor]\nnamespace = \"shprdtest/\"\n");
    let configured = ok_with_home(&root, home.path(), &["janitor"]);
    assert!(
        configured.contains("would archive"),
        "the configured namespace owns {branch}: {configured}"
    );
}

#[test]
fn handoff_needs_a_task_and_a_reason() {
    let root = repo();
    let root = root.path();
    start(root);
    add_task(root, "T1", "crates/shprd");
    let help = ok(root, &["agent", "handoff", "--help"]);
    assert!(help.contains("--task"), "{help}");
    assert!(help.contains("--reason"), "{help}");
    assert!(help.contains("--to"), "{help}");
    let missing = run(root, &["agent", "handoff", "--task", "T1"]);
    assert_eq!(
        missing.status.code(),
        Some(2),
        "a hand-off without a reason must be a usage error: {}",
        String::from_utf8_lossy(&missing.stderr)
    );
}

/// The last event the log holds, as JSON.
fn last_event(root: &Path) -> Value {
    let path = log_path(root);
    let body = match std::fs::read_to_string(&path) {
        Ok(body) => body,
        Err(err) => panic!("read {}: {err}", path.display()),
    };
    let line = match body.lines().last() {
        Some(line) => line,
        None => panic!("the log is empty"),
    };
    match serde_json::from_str(line) {
        Ok(value) => value,
        Err(err) => panic!("bad event line {line:?}: {err}"),
    }
}

/// Records a dry mark for `agent`, the way the watchdog does when its process
/// exits and no reset time is known.
fn mark_dry(root: &Path, agent: &str) {
    append_events(
        root,
        vec![EventKind::Dry {
            agent: agent.to_string(),
            reason: "agent process exited".to_string(),
            reset_at_ms: None,
        }],
    );
}

#[test]
fn a_human_re_admits_a_dry_agent_and_the_run_may_use_it_again() {
    let dir = repo();
    let root = dir.path();
    start(root);
    add_task(root, "T1", "crates/shprd");
    mark_dry(root, "w1");

    // The dry mark holds the run back, and the status says how to lift it.
    let spawn = [
        "agent",
        "spawn",
        "--record-only",
        "--task",
        "T1",
        "--attempt",
        "1",
        "--agent",
        "w1",
        "--worktree",
        "/tmp/wt",
        "--branch",
        "feature/x",
    ];
    let message = refused(root, &spawn);
    assert!(message.contains("is dry"), "{message}");
    let status = ok(root, &["status"]);
    assert!(
        status.contains("w1 dry until unknown (agent process exited) — shprd readmit w1"),
        "the status must say how to re-admit: {status}"
    );

    // A roster spelling names the same agent the dry mark stores.
    let printed = ok(root, &["readmit", "w1/some-model"]);
    assert!(printed.contains("re-admitted w1"), "{printed}");
    let event = last_event(root);
    assert_eq!(event.get("kind").and_then(Value::as_str), Some("available"));
    assert_eq!(event.get("agent").and_then(Value::as_str), Some("w1"));
    assert_eq!(
        event.get("reason").and_then(Value::as_str),
        Some("re-admitted by a human"),
        "the default reason says a human did it: {event}"
    );
    assert_eq!(
        event.get("actor").and_then(|actor| actor.get("type")),
        Some(&Value::String("human".to_string())),
        "the log says a human re-admitted it: {event}"
    );

    // And with the mark cleared, the attempt spawns.
    ok(root, &spawn);
}

#[test]
fn readmit_takes_a_reason_and_refuses_what_is_not_dry() {
    let dir = repo();
    let root = dir.path();
    start(root);
    add_task(root, "T1", "crates/shprd");
    mark_dry(root, "w1");
    ok(root, &["readmit", "w1", "--reason", "quota topped up"]);
    assert_eq!(
        last_event(root).get("reason").and_then(Value::as_str),
        Some("quota topped up")
    );

    let message = refused(root, &["readmit", "w1"]);
    assert!(message.contains("is not dry"), "{message}");

    // An agent the log never mentions is not one this run holds.
    let output = run(root, &["readmit", "pi"]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(stderr.contains("is not known to this run"), "{stderr}");
}

#[test]
fn an_orchestrator_may_not_re_admit_an_agent() {
    let dir = repo();
    let root = dir.path();
    start(root);
    add_task(root, "T1", "crates/shprd");
    mark_dry(root, "w1");
    let before = last_event(root);
    let output = match Command::new(BIN)
        .args([
            "--repo",
            &root.display().to_string(),
            "readmit",
            "w1",
            "--json",
        ])
        .env("SHPRD_ACTOR", "orchestrator")
        .output()
    {
        Ok(output) => output,
        Err(err) => panic!("running the binary: {err}"),
    };
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(stderr.contains("only a human"), "{stderr}");
    assert_eq!(last_event(root), before, "nothing was logged");
}
