//! End-to-end tests for the writing verbs: they drive the real binary against
//! a throwaway repo root and read the log the commands produce.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use shprd_core::event::{Actor, Event, EventKind, ResourceAction, ResourceKind};
use tempfile::TempDir;

mod support;

use support::isolate;

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
    let mut command = Command::new(BIN);
    isolate(&mut command, root);
    match command.args(&all).env_remove("SHPRD_ACTOR").output() {
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
            "--check",
            "tests|cargo test|0",
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
    support::pass_review(root, "T1");
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

/// A proof is recorded beside the summary, not inside it: the hand-in event
/// carries the paths, and the summary stays the one plain line the person reads.
#[test]
fn a_hand_in_records_its_proofs_apart_from_the_summary() {
    let dir = repo();
    let root = dir.path();
    start(root);
    add_task(root, "T1", "crates/shprd");
    spawn(root, "T1");

    let printed = ok(
        root,
        &[
            "agent",
            "report",
            "--check",
            "tests|cargo test|0",
            "--task",
            "T1",
            "--attempt",
            "1",
            "--status",
            "completed",
            "--summary",
            "the menu opens",
            "--changed",
            "crates/shprd/src/main.rs",
            "--proof",
            "proof/menu.png",
            "--proof",
            "proof/tests.txt",
            "--json",
        ],
    );
    let event: Value = serde_json::from_str(&printed).unwrap_or(Value::Null);
    assert_eq!(event.get("kind").and_then(Value::as_str), Some("reported"));
    assert_eq!(
        event.get("summary").and_then(Value::as_str),
        Some("the menu opens"),
        "the summary is unchanged: {event}"
    );
    assert_eq!(
        event.get("proofs"),
        Some(&serde_json::json!(["proof/menu.png", "proof/tests.txt"])),
        "both proofs are recorded: {event}"
    );
}

/// A proof outside the task's worktree is refused: the reviewer reads the
/// packet from outside the worktree, so a path there is one it cannot open
/// (item 6 of the M10.0 follow-ups).
#[test]
fn a_proof_outside_the_worktree_is_refused() {
    let dir = repo();
    let root = dir.path();
    let outside = match TempDir::new() {
        Ok(dir) => dir,
        Err(err) => panic!("tempdir failed: {err}"),
    };
    start(root);
    add_task(root, "T1", "crates/shprd");
    spawn(root, "T1");

    let output = run(
        root,
        &[
            "agent",
            "report",
            "--check",
            "tests|cargo test|0",
            "--task",
            "T1",
            "--attempt",
            "1",
            "--status",
            "completed",
            "--summary",
            "the menu opens",
            "--proof",
            &outside.path().join("tests.txt").display().to_string(),
        ],
    );
    assert_eq!(
        output.status.code(),
        Some(2),
        "a proof the reviewer cannot open is refused: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        stderr.contains("not inside the task's worktree"),
        "the refusal says why: {stderr}"
    );
    assert!(
        stderr.contains(root.display().to_string().as_str()),
        "it names the worktree the proof belongs in: {stderr}"
    );
    assert!(
        stderr.contains("--proof .shprd/proof/tests.txt"),
        "and shows the shape that works: {stderr}"
    );
    // Nothing was written: the log has the spawn and nothing after it.
    let log = match std::fs::read_to_string(log_path(root)) {
        Ok(text) => text,
        Err(err) => panic!("read the log: {err}"),
    };
    assert!(
        !log.contains("\"kind\":\"reported\""),
        "a refused hand-in records nothing: {log}"
    );
}

/// A proof inside the worktree is recorded, including one written with a `..`
/// that stays inside it.
#[test]
fn a_proof_inside_the_worktree_is_recorded() {
    let dir = repo();
    let root = dir.path();
    start(root);
    add_task(root, "T1", "crates/shprd");
    spawn(root, "T1");

    let printed = ok(
        root,
        &[
            "agent",
            "report",
            "--check",
            "tests|cargo test|0",
            "--task",
            "T1",
            "--attempt",
            "1",
            "--status",
            "completed",
            "--summary",
            "the menu opens",
            "--proof",
            "proof/tests.txt",
            "--proof",
            "./proof/../proof/menu.png",
            "--json",
        ],
    );
    let event: Value = serde_json::from_str(&printed).unwrap_or(Value::Null);
    assert_eq!(
        event.get("proofs"),
        Some(&serde_json::json!([
            "proof/tests.txt",
            "./proof/../proof/menu.png"
        ])),
        "the paths are recorded as the worker wrote them: {event}"
    );
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
            "--check",
            "tests|cargo test|0",
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

/// The run log's text.
fn read_log(root: &Path) -> String {
    match std::fs::read_to_string(log_path(root)) {
        Ok(text) => text,
        Err(err) => panic!("reading the log failed: {err}"),
    }
}

#[test]
fn a_refused_hand_in_leaves_a_card_naming_the_reason() {
    let dir = repo();
    let root = dir.path();
    start(root);
    add_task(root, "T1", "crates/shprd");
    spawn(root, "T1");
    let report = [
        "agent",
        "report",
        "--check",
        "tests|cargo test|0",
        "--task",
        "T1",
        "--attempt",
        "1",
        "--status",
        "completed",
        "--summary",
        "did it",
        "--changed",
        "tests/test_ops.py",
    ];
    refused(root, &report);
    let log = read_log(root);
    let raised: Vec<&str> = log
        .lines()
        .filter(|line| line.contains("\"ask_raised\""))
        .collect();
    assert_eq!(raised.len(), 1, "one card: {log}");
    assert!(raised[0].contains("hand-in-refused-1-1"), "{}", raised[0]);
    assert!(
        raised[0].contains("tests/test_ops.py"),
        "names the path: {}",
        raised[0]
    );
    assert!(
        raised[0].contains("widen the scope to tests/test_ops.py"),
        "offers the widening: {}",
        raised[0]
    );
    assert!(raised[0].contains("send it back"), "{}", raised[0]);
    // The worker retries the same hand-in: still one open card.
    refused(root, &report);
    let log = read_log(root);
    assert_eq!(log.matches("\"ask_raised\"").count(), 1, "{log}");
}

#[test]
fn verbs_before_run_start_are_refused() {
    let dir = repo();
    let root = dir.path();
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
    let mut command = Command::new(BIN);
    isolate(&mut command, root);
    match command
        .args(&all)
        .env_remove("SHPRD_ACTOR")
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
            "--check",
            "tests|cargo test|0",
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
    support::pass_review(root, "T1");
    ok_with_path(
        root,
        &["agent", "integrate", "--task", "T1", "--record-only"],
        bin_dir,
    );
    ok_with_path(
        root,
        &[
            "override",
            "--task",
            "T1",
            "--rule",
            "integrate_after_failed_gate",
            "--reason",
            "release branch",
            "--yes",
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
    assert!(
        status.contains("Done (landed)"),
        "a landed task says so: {status:?}"
    );

    let body = ok_with_path(root, &["pr-body"], bin_dir);
    assert!(body.contains("## Tasks"), "pr-body was {body:?}");
    assert!(
        body.contains("- T1 `integrate_after_failed_gate`: release branch"),
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
    let mut command = Command::new(BIN);
    isolate(&mut command, root);
    let output = command
        .args(["--repo", &root.display().to_string(), "dagr", "sync"])
        .env_remove("SHPRD_ACTOR")
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
    let mut command = Command::new(BIN);
    isolate(&mut command, root);
    let output = command
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
        .env_remove("SHPRD_ACTOR")
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

/// A stub checker that records the arguments it was run with, then passes.
///
/// It stands in for `dagr check`: the projection is only proof of anything if a
/// program really ran on it, so the stub leaves a file naming the document it
/// was asked about.
#[cfg(unix)]
fn stub_checker(dir: &Path) -> (PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let script = dir.join("stub-dagr");
    let record = dir.join("called.txt");
    if let Err(err) = std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" > '{}'\nprintf '[]\\n'\nexit 0\n",
            record.display()
        ),
    ) {
        panic!("writing the stub failed: {err}");
    }
    if let Err(err) = std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)) {
        panic!("making the stub executable failed: {err}");
    }
    (script, record)
}

/// A temp home holding `settings.toml` with `text`.
fn home_with_settings(text: &str) -> TempDir {
    let home = match TempDir::new() {
        Ok(home) => home,
        Err(err) => panic!("temp home failed: {err}"),
    };
    let dir = home.path().join(".config").join("shprd");
    if let Err(err) = std::fs::create_dir_all(&dir) {
        panic!("creating the config folder failed: {err}");
    }
    if let Err(err) = std::fs::write(dir.join("settings.toml"), text) {
        panic!("writing the settings failed: {err}");
    }
    home
}

#[test]
#[cfg(unix)]
fn the_graph_dagr_setting_is_what_validates_the_projection() {
    // M10.7's first rule: a person who names their dagr in `graph.dagr` gets
    // their projections checked by it, and the value is read where the wiring
    // reads every other file — from their home, not from a variable.
    let dir = repo();
    let root = dir.path();
    let stubs = match TempDir::new() {
        Ok(stubs) => stubs,
        Err(err) => panic!("temp folder failed: {err}"),
    };
    let (checker, record) = stub_checker(stubs.path());
    let home = home_with_settings(&format!(
        "[graph]\ndagr = {:?}\n",
        checker.display().to_string()
    ));

    let mut command = Command::new(BIN);
    isolate(&mut command, root);
    let output = command
        .args([
            "--repo",
            &root.display().to_string(),
            "--home",
            &home.path().display().to_string(),
            "run",
            "start",
            "--run-id",
            "t1",
            "--base",
            "main",
            "--integration",
            "integration/m1",
        ])
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .env_remove("SHPRD_ACTOR")
        .env_remove("SHPRD_DAGR_BIN")
        .output();
    let output = match output {
        Ok(output) => output,
        Err(err) => panic!("running {BIN} failed: {err}"),
    };
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        !stderr.contains("wrote unchecked"),
        "the document must be checked when the setting names a dagr: {stderr}"
    );
    let called = match std::fs::read_to_string(&record) {
        Ok(text) => text,
        Err(err) => panic!("the stub was never run: {err}"),
    };
    assert!(
        called.contains("check"),
        "the stub is run as a checker: {called}"
    );
    assert!(
        called.contains(".dagr/run.json.tmp") || called.contains(".tmp"),
        "it is run on the temporary document, before the rename: {called}"
    );
    assert!(
        root.join(".dagr").join("run.json").exists(),
        "the document is written"
    );
}

#[test]
#[cfg(unix)]
fn without_dagr_the_note_names_the_setting_to_fix() {
    // No `graph.dagr`, no `dagr` on PATH and no herdr plugin in this home: the
    // write still happens, and the one line a person sees names the key.
    let dir = repo();
    let root = dir.path();
    let home = match TempDir::new() {
        Ok(home) => home,
        Err(err) => panic!("temp home failed: {err}"),
    };
    let empty = home.path().join("bin");
    if let Err(err) = std::fs::create_dir_all(&empty) {
        panic!("creating the bin folder failed: {err}");
    }
    let mut command = Command::new(BIN);
    isolate(&mut command, root);
    let output = command
        .args([
            "--repo",
            &root.display().to_string(),
            "--home",
            &home.path().display().to_string(),
            "run",
            "start",
            "--run-id",
            "t1",
            "--base",
            "main",
            "--integration",
            "integration/m1",
        ])
        .env("PATH", &empty)
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .env_remove("SHPRD_ACTOR")
        .env_remove("SHPRD_DAGR_BIN")
        .output();
    let output = match output {
        Ok(output) => output,
        Err(err) => panic!("running {BIN} failed: {err}"),
    };
    assert_eq!(output.status.code(), Some(0), "the write still succeeds");
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(stderr.contains("wrote unchecked"), "{stderr}");
    assert!(stderr.contains("graph.dagr"), "the key is named: {stderr}");
    assert!(
        root.join(".dagr").join("run.json").exists(),
        "a machine without dagr still leaves a document"
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
    let mut command = Command::new(BIN);
    isolate(&mut command, home);
    match command.args(&all).env_remove("SHPRD_ACTOR").output() {
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
            "--check",
            "tests|cargo test|0",
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
    support::pass_review(root, "T1");
    ok(
        root,
        &["agent", "integrate", "--task", "T1", "--record-only"],
    );
}

/// `run start` makes the branch it integrates on, from `--base`, when it is not
/// there: a run whose integration branch does not exist cannot move, and every
/// start fails on it for ever (bug 2 of gate run #4).
#[test]
fn run_start_creates_the_integration_branch_from_the_base() {
    let git = GitRepo::new();
    let root = git.path.clone();
    let base = git.git(&["rev-parse", "main"]);
    assert!(
        !git.has_ref("integration/m9"),
        "the fixture starts without the branch"
    );

    let printed = ok(
        &root,
        &[
            "run",
            "start",
            "--run-id",
            "m9",
            "--base",
            "main",
            "--integration",
            "integration/m9",
            "--json",
        ],
    );
    let event: Value = serde_json::from_str(&printed).unwrap_or(Value::Null);
    assert_eq!(
        event.get("kind").and_then(Value::as_str),
        Some("run_started")
    );

    assert!(
        git.has_ref("integration/m9"),
        "the integration branch was made"
    );
    assert_eq!(
        git.git(&["rev-parse", "integration/m9"]),
        base,
        "it is cut from the base branch"
    );

    // And the record says shprd made it, so the janitor may clean it up.
    let log = match std::fs::read_to_string(log_path(&root)) {
        Ok(text) => text,
        Err(err) => panic!("read the log: {err}"),
    };
    assert_eq!(
        resource_events(&root),
        1,
        "one branch was created and recorded"
    );
    assert!(
        log.lines()
            .any(|line| line.contains("\"kind\":\"resource\"")
                && line.contains("\"resource\":\"branch\"")
                && line.contains("integration/m9")
                && line.contains("\"created\"")),
        "the resource line names the branch it made: {log}"
    );
}

/// A base branch that is not there either is the person's mistake, and the run
/// must not start half made.
#[test]
fn run_start_refuses_a_missing_base_and_writes_nothing() {
    let git = GitRepo::new();
    let root = git.path.clone();
    let output = run(
        &root,
        &[
            "run",
            "start",
            "--run-id",
            "m9",
            "--base",
            "release/9",
            "--integration",
            "integration/m9",
        ],
    );
    assert_eq!(
        output.status.code(),
        Some(2),
        "a missing base is the person's mistake: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(stderr.contains("release/9"), "{stderr}");
    assert!(
        stderr.contains("integration/m9"),
        "the branch it could not cut is named: {stderr}"
    );
    assert!(!git.has_ref("integration/m9"), "nothing was created");
    assert!(
        !root.join(".shprd").join("events.jsonl").exists(),
        "a refused start writes no log"
    );
}

/// A run started over a branch that is already there keeps it, and creates
/// nothing: an existing branch may hold someone's commits.
#[test]
fn run_start_leaves_an_existing_integration_branch_alone() {
    let git = GitRepo::new();
    let root = git.path.clone();
    git.git(&["branch", "integration/m9", "main"]);
    git.git(&["checkout", "integration/m9"]);
    if let Err(err) = std::fs::write(root.join("work.txt"), "landed work\n") {
        panic!("write work.txt: {err}");
    }
    git.git(&["add", "--", "work.txt"]);
    git.git(&["commit", "-m", "work already integrated"]);
    let tip = git.git(&["rev-parse", "integration/m9"]);
    git.git(&["checkout", "main"]);

    ok(
        &root,
        &[
            "run",
            "start",
            "--run-id",
            "m9",
            "--base",
            "main",
            "--integration",
            "integration/m9",
        ],
    );

    assert_eq!(
        git.git(&["rev-parse", "integration/m9"]),
        tip,
        "the existing branch and its commits are kept"
    );
    assert_eq!(
        resource_events(&root),
        0,
        "an existing branch is not recorded as shprd's own"
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
        !git.has_ref("refs/shprd/archive/r1/T1-a1"),
        "a dry run must not create the archive ref"
    );
    assert_eq!(resource_events(&root), before, "a dry run writes nothing");

    let applied = ok_with_home(&root, home.path(), &["janitor", "--apply"]);
    assert!(applied.contains("ok "), "{applied}");
    assert!(!git.has_ref(branch), "the branch must be gone");
    assert_eq!(
        git.git(&["rev-parse", "refs/shprd/archive/r1/T1-a1"]),
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
fn restore_brings_back_the_newest_attempt_archive() {
    let git = GitRepo::new();
    let root = git.path.clone();
    let home = match TempDir::new() {
        Ok(dir) => dir,
        Err(err) => panic!("tempdir failed: {err}"),
    };
    let branch = "shprd/r1/T1";
    integrated_task(&root, branch);
    let older = git.git(&["rev-parse", "main"]);
    git.git(&["commit", "--allow-empty", "-q", "-m", "attempt two"]);
    let newer = git.git(&["rev-parse", "HEAD"]);
    git.git(&["update-ref", "refs/shprd/archive/r1/T1-a1", &older]);
    git.git(&["update-ref", "refs/shprd/archive/r1/T1-a2", &newer]);

    let restored = ok_with_home(&root, home.path(), &["restore", "T1"]);
    assert!(restored.contains("ok resource"), "{restored}");
    assert_eq!(
        git.git(&["rev-parse", branch]),
        newer,
        "restore brings back the newest attempt, not the first"
    );
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
    // T1 landed but a second task never ran, so the run only ends when a human
    // says to end it anyway, naming the work it leaves behind.
    add_task(&root, "T2", "crates/y");
    ok(&root, &["run", "finish", "--abandon", "--yes"]);

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
fn abandoning_refuses_without_a_terminal_and_logs_nothing() {
    let dir = repo();
    let root = dir.path();
    start(root);
    add_task(root, "T2", "crates/y");
    // An agent's shell tool has no TTY, so `--abandon` without the debug
    // `--yes` has nobody to ask and must not guess: nothing is written, so the
    // run is still in flight.
    let before = last_event(root);
    let output = run(root, &["run", "finish", "--abandon"]);
    assert_eq!(
        output.status.code(),
        Some(2),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let message = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(message.contains("stdin is not a terminal"), "{message}");
    assert!(
        message.contains("shprd run finish --abandon"),
        "the refusal names the command to run: {message}"
    );
    assert_eq!(last_event(root), before, "nothing was logged");
    assert!(!state(root)["finished"].as_bool().unwrap_or(false));
}

#[test]
fn abandoning_is_refused_for_an_agent_actor_even_with_yes() {
    let dir = repo();
    let root = dir.path();
    start(root);
    add_task(root, "T2", "crates/y");
    let before = last_event(root);
    let mut command = Command::new(BIN);
    isolate(&mut command, root);
    let output = match command
        .args([
            "--repo",
            &root.display().to_string(),
            "run",
            "finish",
            "--abandon",
            "--yes",
        ])
        .env_remove("SHPRD_ACTOR")
        .env("SHPRD_ACTOR", "agent:codex")
        .output()
    {
        Ok(output) => output,
        Err(err) => panic!("running the binary: {err}"),
    };
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(stderr.contains("only a human abandons a run"), "{stderr}");
    assert_eq!(last_event(root), before, "nothing was logged");
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
    let mut command = Command::new(BIN);
    isolate(&mut command, root);
    let output = match command
        .args([
            "--repo",
            &root.display().to_string(),
            "readmit",
            "w1",
            "--json",
        ])
        .env_remove("SHPRD_ACTOR")
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

/// The rule name the run honours, as the override verbs spell it.
const OVERRIDE_RULE: &str = "integrate_after_failed_gate";

/// A run with one task that failed `clippy` and then passed it: the task is
/// `Passed` with a failed gate behind it, so only an override lets it land.
fn run_that_needs_an_override(root: &Path) {
    start(root);
    add_task(root, "T1", "crates/shprd");
    spawn(root, "T1");
    ok(
        root,
        &[
            "agent",
            "report",
            "--check",
            "tests|cargo test|0",
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
    support::pass_review_after_failed_gate(root, "T1", "clippy");
    let message = refused(
        root,
        &["agent", "integrate", "--task", "T1", "--record-only"],
    );
    assert!(
        message.contains("clippy"),
        "the refusal names the gate: {message}"
    );
}

/// Runs the binary with `SHPRD_ACTOR` naming who acts.
fn run_as(root: &Path, actor: &str, args: &[&str]) -> Output {
    let mut all = vec![
        "--repo".to_string(),
        root.display().to_string(),
        "--json".to_string(),
    ];
    all.extend(args.iter().map(|arg| (*arg).to_string()));
    let mut command = Command::new(BIN);
    isolate(&mut command, root);
    match command
        .args(&all)
        .env_remove("SHPRD_ACTOR")
        .env("SHPRD_ACTOR", actor)
        .output()
    {
        Ok(output) => output,
        Err(err) => panic!("running the binary: {err}"),
    }
}

/// Runs the binary, asserts it exited 2, and returns stderr.
///
/// A human verb refuses with [`shprd_core::rules::RuleError`] nowhere in the
/// message, so unlike [`refused`] this asks only for the exit code.
fn refused_verb(root: &Path, args: &[&str]) -> String {
    let output = run(root, args);
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert_eq!(output.status.code(), Some(2), "{args:?}: {stderr}");
    stderr
}

/// Every override the log holds.
fn override_events(root: &Path) -> Vec<Value> {
    let body = match std::fs::read_to_string(log_path(root)) {
        Ok(body) => body,
        Err(err) => panic!("reading the log: {err}"),
    };
    body.lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|event| event.get("kind").and_then(Value::as_str) == Some("override"))
        .collect()
}

#[test]
fn override_without_a_terminal_and_without_yes_is_refused_and_logs_nothing() {
    let dir = repo();
    let root = dir.path();
    run_that_needs_an_override(root);
    let before = last_event(root);
    // An agent's shell tool has no TTY, so the question has nobody to ask,
    // and a guess that went through would be exactly the hole this closes.
    let message = refused_verb(
        root,
        &[
            "override",
            "--task",
            "T1",
            "--rule",
            OVERRIDE_RULE,
            "--reason",
            "the finding is cosmetic",
        ],
    );
    assert!(message.contains("stdin is not a terminal"), "{message}");
    assert!(message.contains("shprd override"), "{message}");
    assert_eq!(last_event(root), before, "nothing was logged");
    assert!(override_events(root).is_empty());
    // And the work still cannot land, so nothing was set aside either.
    refused(
        root,
        &["agent", "integrate", "--task", "T1", "--record-only"],
    );
}

#[test]
fn an_orchestrator_override_is_refused_even_with_yes() {
    let dir = repo();
    let root = dir.path();
    run_that_needs_an_override(root);
    let before = last_event(root);
    let output = run_as(
        root,
        "orchestrator",
        &[
            "override",
            "--task",
            "T1",
            "--rule",
            OVERRIDE_RULE,
            "--reason",
            "shipping it",
            "--yes",
        ],
    );
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        stderr.contains("only a human overrides a rule"),
        "the refusal says who may: {stderr}"
    );
    assert_eq!(last_event(root), before, "nothing was logged");
    assert!(override_events(root).is_empty());
}

#[test]
fn an_agent_override_is_refused_even_with_yes() {
    let dir = repo();
    let root = dir.path();
    run_that_needs_an_override(root);
    let before = last_event(root);
    let output = run_as(
        root,
        "agent:codex",
        &[
            "override",
            "--task",
            "T1",
            "--rule",
            OVERRIDE_RULE,
            "--reason",
            "shipping it",
            "--yes",
        ],
    );
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(stderr.contains("only a human overrides a rule"), "{stderr}");
    assert_eq!(last_event(root), before, "nothing was logged");
}

#[test]
fn an_override_of_a_rule_nothing_reads_is_refused() {
    let dir = repo();
    let root = dir.path();
    run_that_needs_an_override(root);
    let before = last_event(root);
    let message = refused_verb(
        root,
        &[
            "override",
            "--task",
            "T1",
            "--rule",
            "integrate_after_failed_gates",
            "--reason",
            "a typo",
            "--yes",
        ],
    );
    assert!(
        message.contains("is not one") && message.contains("integrate_after_failed_gate"),
        "the refusal names the rule and the rules there are: {message}"
    );
    assert_eq!(last_event(root), before, "nothing was logged");
}

#[test]
fn a_human_override_with_yes_is_recorded_and_the_task_then_integrates() {
    let dir = repo();
    let root = dir.path();
    run_that_needs_an_override(root);
    let printed = ok(
        root,
        &[
            "override",
            "--task",
            "T1",
            "--rule",
            OVERRIDE_RULE,
            "--reason",
            "the finding is cosmetic",
            "--yes",
        ],
    );
    assert!(
        printed.contains("overrode integrate_after_failed_gate for task T1"),
        "{printed}"
    );
    let recorded = override_events(root);
    assert_eq!(recorded.len(), 1, "the override is in the log");
    assert_eq!(
        recorded[0].get("rule").and_then(Value::as_str),
        Some(OVERRIDE_RULE)
    );
    assert_eq!(
        recorded[0].get("actor").and_then(|actor| actor.get("type")),
        Some(&Value::String("human".to_string())),
        "the log says a human took the decision"
    );

    ok(
        root,
        &["agent", "integrate", "--task", "T1", "--record-only"],
    );
    assert_eq!(status_of(&state(root), "T1"), "integrated");
}

#[test]
fn a_drop_that_cannot_find_the_home_is_refused_and_logs_nothing() {
    // No home means the briefs folder cannot be found, which is not the same as
    // there being no tab records in it: the task may have an agent working in a
    // tab this drop cannot look for, so the verb refuses instead of dropping a
    // task and leaving the agent behind.
    let dir = repo();
    let root = dir.path();
    start(root);
    add_task(root, "T1", "crates/shprd");
    let log = log_path(root);
    let before = match std::fs::read_to_string(&log) {
        Ok(text) => text,
        Err(err) => panic!("read {}: {err}", log.display()),
    };

    let args: Vec<String> = vec![
        "--repo".to_string(),
        root.display().to_string(),
        "drop".to_string(),
        "T1".to_string(),
        "--reason".to_string(),
        "not needed after all".to_string(),
        "--yes".to_string(),
    ];
    let mut command = Command::new(BIN);
    isolate(&mut command, root);
    let output = match command
        .args(&args)
        .env_remove("SHPRD_ACTOR")
        .env_remove("SHPRD_HOME")
        .env_remove("HOME")
        .output()
    {
        Ok(output) => output,
        Err(err) => panic!("running {BIN} failed: {err}"),
    };

    assert_eq!(
        output.status.code(),
        Some(2),
        "a drop with no home is refused\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        stderr.contains("home") && stderr.contains("cannot be checked"),
        "the refusal says why: {stderr:?}"
    );
    let after = match std::fs::read_to_string(&log) {
        Ok(text) => text,
        Err(err) => panic!("read {}: {err}", log.display()),
    };
    assert_eq!(after, before, "nothing was logged");
    assert_eq!(
        status_of(&state(root), "T1"),
        "queued",
        "the task is still in the run"
    );
}

/// SHPR-70: the coordinator adds tasks with this verb, and nobody has typed
/// `shprd run start`: the first task in a repository with a commit starts the run
/// itself, on the checked-out branch.
#[test]
fn the_first_task_in_a_fresh_repo_starts_a_run() {
    let root = repo();
    for args in [
        vec!["init", "--quiet", "-b", "main"],
        vec!["config", "user.name", "t"],
        vec!["config", "user.email", "t@example.invalid"],
        vec!["config", "commit.gpgsign", "false"],
        vec!["commit", "--quiet", "--allow-empty", "-m", "seed"],
    ] {
        let status = Command::new("git")
            .arg("-C")
            .arg(root.path())
            .args(&args)
            .status();
        assert!(status.is_ok_and(|status| status.success()), "git {args:?}");
    }
    ok(
        root.path(),
        &[
            "agent",
            "task",
            "add",
            "--id",
            "T1",
            "--title",
            "hello",
            "--scope",
            "hello.py",
            "--acceptance",
            "it prints hi",
        ],
    );
    let state = ok(root.path(), &["state", "--json"]);
    let Ok(value) = serde_json::from_str::<Value>(&state) else {
        panic!("not json: {state}");
    };
    assert_eq!(value["run"]["base_branch"], "main", "{state}");
    assert!(
        value["run"]["integration_branch"]
            .as_str()
            .is_some_and(|name| name.starts_with("integration/r")),
        "{state}"
    );
    assert!(value["tasks"].to_string().contains("T1"), "{state}");
}

/// Runs the binary with `--as` and `$SHPRD_ACTOR` set as given.
fn run_with_actor(root: &Path, env_actor: &str, as_flag: &str, args: &[&str]) -> Output {
    let mut all = vec![
        "--repo".to_string(),
        root.display().to_string(),
        "--as".to_string(),
        as_flag.to_string(),
    ];
    all.extend(args.iter().map(|arg| (*arg).to_string()));
    let mut command = Command::new(BIN);
    isolate(&mut command, root);
    match command
        .args(&all)
        .env_remove("SHPRD_ACTOR")
        .env("SHPRD_ACTOR", env_actor)
        .output()
    {
        Ok(output) => output,
        Err(err) => panic!("running the binary: {err}"),
    }
}

/// Every human verb, as the argument list that reaches its actor check.
fn human_verbs() -> Vec<(&'static str, Vec<&'static str>)> {
    vec![
        (
            "drop",
            vec!["drop", "T1", "--reason", "not needed", "--yes"],
        ),
        (
            "override",
            vec![
                "override",
                "--task",
                "T1",
                "--rule",
                OVERRIDE_RULE,
                "--reason",
                "cosmetic",
                "--yes",
            ],
        ),
        ("approve", vec!["approve", "T1", "--yes"]),
        ("readmit", vec!["readmit", "w1", "--reason", "topped up"]),
    ]
}

/// Item 13: `--as human` is honoured by every human verb, whatever
/// `$SHPRD_ACTOR` says, and `--as` naming a non-human is refused by every one.
#[test]
fn every_human_verb_reads_the_actor_the_same_way() {
    for (verb, args) in human_verbs() {
        let dir = repo();
        let root = dir.path();
        run_that_needs_an_override(root);
        mark_dry(root, "w1");

        // `--as human` wins over an agent's environment: the actor check
        // passes, so any refusal is about something else.
        let output = run_with_actor(root, "agent:w9", "human", &args);
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        assert!(
            !stderr.contains("only a human"),
            "{verb}: --as human was ignored: {stderr}"
        );

        // A non-human `--as` is refused before anything is written, even with
        // `$SHPRD_ACTOR` naming a human.
        let before = last_event(root);
        let output = run_with_actor(root, "human", "orchestrator", &args);
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        assert_eq!(output.status.code(), Some(2), "{verb}: {stderr}");
        assert!(stderr.contains("only a human"), "{verb}: {stderr}");
        assert_eq!(last_event(root), before, "{verb}: nothing was logged");

        // And the environment alone says the same thing.
        let output = run_as(root, "agent:w9", &args);
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        assert!(stderr.contains("only a human"), "{verb}: {stderr}");
    }
}

/// Item 13, `sync`: `--as human` wins over an agent's environment too.
#[test]
fn sync_reads_the_actor_the_same_way() {
    let dir = repo();
    let root = dir.path();
    start(root);
    let output = run_with_actor(root, "agent:w9", "human", &["sync"]);
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        !stderr.contains("cannot sync the base"),
        "--as human was ignored: {stderr}"
    );
    let output = run_with_actor(root, "human", "agent:w9", &["sync"]);
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("cannot sync the base"), "{stderr}");
}
