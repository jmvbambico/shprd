//! End-to-end tests for the writing verbs: they drive the real binary against
//! a throwaway repo root and read the log the commands produce.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
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
            "--task",
            "T1",
            "--attempt",
            "1",
            "--agent",
            "w1",
            "--model",
            "m",
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
        &["agent", "integrate", "--task", "T1", "--commit", "abc123"],
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
        &["agent", "integrate", "--task", "T1", "--commit", "abc123"],
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
