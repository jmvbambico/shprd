//! End-to-end tests for the agent-callable task verbs: the status line a worker
//! records, and the hand-in a worker submits with its checks.
//!
//! They drive the real binary against a throwaway repo root and read the log the
//! commands produce, because what matters is what reaches the ledger: the line
//! is capped, several writers never interleave, and a hand-in without passing
//! checks is refused rather than stored.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;

use serde_json::Value;
use tempfile::TempDir;

/// The binary under test, built by cargo for this integration test.
const BIN: &str = env!("CARGO_BIN_EXE_shprd");

/// The cap a status line is stored under, from `shprd-core`.
const STATUS_MAX: usize = 40;

/// A throwaway repo root that acts as `--repo`.
fn repo() -> TempDir {
    match TempDir::new() {
        Ok(dir) => dir,
        Err(err) => panic!("tempdir failed: {err}"),
    }
}

/// Runs the binary with `--repo <root>`, `--no-dagr` and the given arguments.
fn run(root: &Path, args: &[&str]) -> Output {
    let mut all: Vec<String> = vec![
        "--repo".into(),
        root.display().to_string(),
        "--no-dagr".into(),
    ];
    all.extend(args.iter().map(|arg| (*arg).to_string()));
    match Command::new(BIN)
        .args(&all)
        .env_remove("SHPRD_ACTOR")
        .output()
    {
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

/// Runs the binary and asserts it exited 2 with a `refused:` line, returning
/// stderr.
fn refused(root: &Path, args: &[&str]) -> String {
    let output = run(root, args);
    assert_eq!(
        output.status.code(),
        Some(2),
        "expected exit 2 for {args:?}\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(stderr.contains("refused:"), "{stderr:?}");
    stderr
}

/// The log file a run root should hold.
fn log_path(root: &Path) -> PathBuf {
    root.join(".shprd").join("events.jsonl")
}

/// Every event in the log, in order.
fn events(root: &Path) -> Vec<Value> {
    let body = match std::fs::read_to_string(log_path(root)) {
        Ok(body) => body,
        Err(err) => panic!("read {}: {err}", log_path(root).display()),
    };
    body.lines()
        .map(|line| match serde_json::from_str::<Value>(line) {
            Ok(value) => value,
            Err(err) => panic!("bad event line {line:?}: {err}"),
        })
        .collect()
}

/// The last event the log holds.
fn last_event(root: &Path) -> Value {
    match events(root).pop() {
        Some(event) => event,
        None => panic!("the log is empty"),
    }
}

/// Starts run `t1` and adds and spawns task `T1`.
fn working_task(root: &Path) {
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
    ok(
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
    );
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
            "feature/x",
        ],
    );
}

/// The `agent task report` arguments for a completed hand-in of `T1`.
fn hand_in_args(checks: &[&str]) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "agent".into(),
        "task".into(),
        "report".into(),
        "--task".into(),
        "T1".into(),
        "--attempt".into(),
        "1".into(),
        "--status".into(),
        "completed".into(),
        "--summary".into(),
        "did it".into(),
        "--changed".into(),
        "crates/shprd/src/main.rs".into(),
    ];
    for check in checks {
        args.push("--check".into());
        args.push((*check).to_string());
    }
    args
}

/// Runs the hand-in with `checks` as its `--check` values.
fn hand_in(root: &Path, checks: &[&str]) -> Output {
    let args = hand_in_args(checks);
    let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
    run(root, &borrowed)
}

#[test]
fn a_task_status_records_a_capped_one_line_message() {
    let dir = repo();
    let root = dir.path();
    working_task(root);

    let pasted = "I read the event log and changed the fold, then I ran every gate and they all passed\nand here is a second line nobody asked for";
    ok(
        root,
        &["task", "status", "T1", "--msg", pasted, "--progress", "40"],
    );

    let event = last_event(root);
    assert_eq!(
        event.get("kind").and_then(Value::as_str),
        Some("task_status"),
        "{event}"
    );
    assert_eq!(event.get("task").and_then(Value::as_str), Some("T1"));
    assert_eq!(event.get("progress").and_then(Value::as_u64), Some(40));
    let message = match event.get("message").and_then(Value::as_str) {
        Some(message) => message,
        None => panic!("the status carried no message: {event}"),
    };
    assert!(
        message.chars().count() <= STATUS_MAX,
        "a pasted message must be capped: {message:?}"
    );
    assert!(!message.contains('\n'), "a status is one line: {message:?}");
    assert!(
        message.ends_with('\u{2026}'),
        "a capped status says so: {message:?}"
    );

    // The event is the actor's own: an agent's shell records it as that agent.
    let output = Command::new(BIN)
        .args([
            "--repo",
            &root.display().to_string(),
            "--no-dagr",
            "task",
            "status",
            "T1",
            "--msg",
            "running clippy",
        ])
        .env_remove("SHPRD_ACTOR")
        .env("SHPRD_ACTOR", "agent:codex")
        .output();
    let output = match output {
        Ok(output) => output,
        Err(err) => panic!("running {BIN} failed: {err}"),
    };
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let event = last_event(root);
    assert_eq!(
        event.get("actor").and_then(|actor| actor.get("id")),
        Some(&Value::String("codex".to_string())),
        "{event}"
    );
}

#[test]
fn task_status_is_safe_from_parallel_agents() {
    let dir = repo();
    let root = dir.path();
    working_task(root);
    let before = events(root).len();

    let root_path = root.to_path_buf();
    let mut handles = Vec::new();
    for index in 0..8 {
        let root = root_path.clone();
        handles.push(thread::spawn(move || {
            let message = format!("writer {index}");
            let output = Command::new(BIN)
                .args([
                    "--repo",
                    &root.display().to_string(),
                    "--no-dagr",
                    "task",
                    "status",
                    "T1",
                    "--msg",
                    &message,
                ])
                .env_remove("SHPRD_ACTOR")
                .output();
            match output {
                Ok(output) => {
                    assert!(
                        output.status.success(),
                        "writer {index} failed: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
                Err(err) => panic!("running {BIN} failed: {err}"),
            }
        }));
    }
    for handle in handles {
        handle
            .join()
            .unwrap_or_else(|err| panic!("a writer panicked: {err:?}"));
    }

    let all = events(root);
    assert_eq!(
        all.len(),
        before + 8,
        "every parallel write must reach the log exactly once"
    );
    for (index, event) in all.iter().enumerate() {
        assert_eq!(
            event.get("seq").and_then(Value::as_u64),
            Some(index as u64 + 1),
            "sequence numbers must stay contiguous: {event}"
        );
    }
    let messages: Vec<&str> = all[before..]
        .iter()
        .filter_map(|event| event.get("message").and_then(Value::as_str))
        .collect();
    assert_eq!(messages.len(), 8, "no status line was lost: {all:?}");
    for index in 0..8 {
        assert!(
            messages.contains(&format!("writer {index}").as_str()),
            "writer {index}'s line is missing: {messages:?}"
        );
    }
}

#[test]
fn a_hand_in_without_checks_is_refused() {
    let dir = repo();
    let root = dir.path();
    working_task(root);
    let before = last_event(root);

    let stderr = refused(
        root,
        &[
            "agent",
            "task",
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
    );
    assert!(
        stderr.contains("handed in without its checks"),
        "the refusal says what is missing: {stderr}"
    );
    assert!(
        stderr.contains("--check"),
        "the refusal says how to fix it: {stderr}"
    );
    assert_eq!(last_event(root), before, "nothing may be logged");
}

#[test]
fn a_hand_in_whose_check_failed_is_refused() {
    let dir = repo();
    let root = dir.path();
    working_task(root);
    let before = last_event(root);

    let output = hand_in(
        root,
        &[
            "format|cargo fmt --all --check|0|ok",
            "tests|cargo test --workspace|101|test result: FAILED",
        ],
    );
    assert_eq!(
        output.status.code(),
        Some(2),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(stderr.contains("refused:"), "{stderr}");
    assert!(
        stderr.contains("tests") && stderr.contains("101"),
        "the refusal names the gate and its exit code: {stderr}"
    );
    assert_eq!(last_event(root), before, "nothing may be logged");
}

#[test]
fn a_hand_in_records_every_check_it_ran() {
    let dir = repo();
    let root = dir.path();
    working_task(root);

    ok(
        root,
        &[
            "agent",
            "task",
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
            "crates/shprd/src/main.rs",
            "--check",
            "format|cargo fmt --all --check|0|",
            "--check",
            "lint|cargo clippy --workspace --all-targets -- -D warnings|0|Finished dev profile",
            "--check",
            "tests|cargo test --workspace|0|test result: ok. 42 passed",
        ],
    );

    let event = last_event(root);
    assert_eq!(
        event.get("kind").and_then(Value::as_str),
        Some("reported"),
        "{event}"
    );
    let checks = match event.get("checks").and_then(Value::as_array) {
        Some(checks) => checks,
        None => panic!("the hand-in stored no checks: {event}"),
    };
    assert_eq!(checks.len(), 3, "{checks:?}");
    assert_eq!(
        checks[1].get("gate").and_then(Value::as_str),
        Some("lint"),
        "{checks:?}"
    );
    assert_eq!(
        checks[1].get("command").and_then(Value::as_str),
        Some("cargo clippy --workspace --all-targets -- -D warnings")
    );
    assert_eq!(checks[1].get("exit_code").and_then(Value::as_i64), Some(0));
    assert_eq!(
        checks[2].get("output_tail").and_then(Value::as_str),
        Some("test result: ok. 42 passed")
    );
}

#[test]
fn the_legacy_report_spelling_enforces_the_same_hand_in() {
    let dir = repo();
    let root = dir.path();
    working_task(root);
    let output = run(
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
    );
    assert_eq!(
        output.status.code(),
        Some(2),
        "the old spelling must not be a way around the checks: {}",
        String::from_utf8_lossy(&output.stderr)
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
            "did it",
            "--check",
            "tests|cargo test|0",
        ],
    );
}

#[test]
fn task_status_refuses_a_bad_line_or_a_bad_progress() {
    let dir = repo();
    let root = dir.path();
    working_task(root);
    let before = last_event(root);

    // An empty line says nothing, so it is refused rather than stored.
    refused(root, &["task", "status", "T1", "--msg", "   "]);
    // Progress outside 0..100 is a usage error: clap refuses it before a write.
    let output = run(
        root,
        &["task", "status", "T1", "--msg", "done", "--progress", "101"],
    );
    assert_eq!(
        output.status.code(),
        Some(2),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // A task the run does not hold cannot take a status line.
    refused(root, &["task", "status", "T9", "--msg", "here"]);
    assert_eq!(last_event(root), before, "nothing may be logged");
}
