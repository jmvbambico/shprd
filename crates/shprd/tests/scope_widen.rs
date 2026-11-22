//! `shprd agent task widen`: the coordinator proposes a wider scope, the person
//! answers the card once, and only a `widen` answer changes the scope and tells
//! the task's worker.
//!
//! The real binary raises the card; `shprd serve` is the API the person's card
//! calls. No agent CLI or terminal is involved: with no live worker the note is
//! settled as skipped, which is what these tests read.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;

mod support;

use support::isolate;

/// The binary under test, built by cargo for this integration test.
const BIN: &str = env!("CARGO_BIN_EXE_shprd");

/// How often a wait looks again.
const POLL: Duration = Duration::from_millis(20);

/// When a wait that starts now gives up.
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(120)
}

/// Waits until `done` holds, or fails saying what was waited for.
fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let limit = deadline();
    while !done() {
        assert!(Instant::now() < limit, "{what} did not happen");
        sleep(POLL);
    }
}

/// A child `shprd serve`, stopped on drop.
struct Service {
    child: Child,
}

impl Service {
    fn start(repo: &Path, state: &Path, home: &Path) -> Self {
        let mut command = Command::new(BIN);
        isolate(&mut command, state);
        let child = command
            .args([
                "--repo",
                &repo.display().to_string(),
                "--home",
                &home.display().to_string(),
                "serve",
                "--port",
                "0",
                "--json",
            ])
            .env("XDG_STATE_HOME", state)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_DATA_HOME", home.join(".local").join("share"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("the service starts");
        wait_until("the service wrote its record", || {
            service_dir(state).join("service.json").exists()
        });
        Self { child }
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The directory the service keeps its record and token in.
fn service_dir(state: &Path) -> PathBuf {
    state.join("shprd")
}

/// Runs the binary against `root` as `actor`.
fn run(root: &Path, actor: Option<&str>, args: &[&str]) -> std::process::Output {
    let mut all: Vec<String> = vec![
        "--repo".into(),
        root.display().to_string(),
        "--no-dagr".into(),
    ];
    all.extend(args.iter().map(|arg| (*arg).to_string()));
    let mut command = Command::new(BIN);
    isolate(&mut command, root);
    command
        .args(&all)
        .env_remove("SHPRD_ACTOR")
        .env_remove("SHPRD_TASK");
    if let Some(actor) = actor {
        command.env("SHPRD_ACTOR", actor);
    }
    command.output().expect("the binary runs")
}

/// Runs a command that must succeed.
fn ok(root: &Path, actor: Option<&str>, args: &[&str]) -> String {
    let output = run(root, actor, args);
    assert!(
        output.status.success(),
        "{args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

/// A repository with a run and task `T1` (scope `crates/shprd`).
fn fixture() -> (TempDir, TempDir, TempDir) {
    let repo = TempDir::new().expect("repo dir");
    let state = TempDir::new().expect("state dir");
    let home = TempDir::new().expect("home dir");
    let home_arg = home.path().display().to_string();
    ok(
        repo.path(),
        None,
        &[
            "--home",
            &home_arg,
            "run",
            "start",
            "--run-id",
            "t1",
            "--base",
            "main",
            "--integration",
            "integration/m9",
        ],
    );
    ok(
        repo.path(),
        None,
        &[
            "--home",
            &home_arg,
            "agent",
            "task",
            "add",
            "--id",
            "T1",
            "--title",
            "widen me",
            "--scope",
            "crates/shprd",
            "--acceptance",
            "done",
        ],
    );
    (repo, state, home)
}

/// Every event the log holds.
fn events(repo: &Path) -> Vec<Value> {
    fs::read_to_string(repo.join(".shprd").join("events.jsonl"))
        .expect("the log")
        .lines()
        .map(|line| serde_json::from_str(line).expect("an event line"))
        .collect()
}

/// How many events of `kind` the log holds.
fn count(repo: &Path, kind: &str) -> usize {
    events(repo)
        .iter()
        .filter(|event| event["kind"] == kind)
        .count()
}

/// Raises the card as the coordinator would.
fn raise(repo: &Path, task: &str) -> std::process::Output {
    run(
        repo,
        Some("agent:coordinator"),
        &[
            "agent",
            "task",
            "widen",
            "--task",
            task,
            "--scope",
            "docs/extra.md",
            "--scope",
            "crates/other",
            "--reason",
            "the change touches the docs",
        ],
    )
}

/// Answers `ask` on `T1` through the service's API.
fn answer(state: &Path, repo: &Path, answer: &str) -> u16 {
    let record: Value = serde_json::from_str(
        &fs::read_to_string(service_dir(state).join("service.json")).expect("the record"),
    )
    .expect("the record is JSON");
    let port = record["port"].as_u64().expect("a port");
    let token = fs::read_to_string(service_dir(state).join("token"))
        .expect("the token")
        .trim()
        .to_string();
    let agent = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build(),
    );
    let body = json!({"task": "T1", "ask": "scope-widen-1", "answer": answer});
    let _ = repo;
    let response = agent
        .post(&format!("http://127.0.0.1:{port}/api/asks/answer"))
        .header("authorization", &format!("Bearer {token}"))
        .header("content-type", "application/json")
        .header("x-shprd", "1")
        .send(body.to_string())
        .expect("the answer is sent");
    response.status().as_u16()
}

/// Every settled or pending note under `T1`'s queue, with its text.
fn notes(repo: &Path) -> Vec<(String, String)> {
    let dir = repo.join(".shprd").join("worker-notes").join("T1");
    let mut found: Vec<(String, String)> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| {
                    (
                        entry
                            .path()
                            .extension()
                            .and_then(|ext| ext.to_str())
                            .unwrap_or("")
                            .to_string(),
                        fs::read_to_string(entry.path()).unwrap_or_default(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    found.sort();
    found
}

#[test]
fn the_verb_raises_one_card_naming_the_paths_and_the_reason() {
    let (repo, _state, _home) = fixture();
    let output = raise(repo.path(), "T1");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(count(repo.path(), "ask_raised"), 1);
    assert_eq!(
        count(repo.path(), "scope_widened"),
        0,
        "the verb widens nothing"
    );
    let raised = events(repo.path())
        .into_iter()
        .find(|event| event["kind"] == "ask_raised")
        .expect("a card");
    let text = raised["text"].as_str().expect("text");
    assert!(
        text.contains("docs/extra.md") && text.contains("crates/other"),
        "{text}"
    );
    assert!(text.contains("the change touches the docs"), "{text}");
    let options: Vec<&str> = raised["options"]
        .as_array()
        .expect("options")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(
        options,
        [
            "widen the scope to docs/extra.md, crates/other",
            "keep the scope"
        ]
    );
}

#[test]
fn answering_widen_writes_the_event_and_queues_the_worker_message() {
    let (repo, state, home) = fixture();
    assert!(raise(repo.path(), "T1").status.success());
    let _service = Service::start(repo.path(), state.path(), home.path());
    let status = answer(
        state.path(),
        repo.path(),
        "widen the scope to docs/extra.md, crates/other",
    );
    assert_eq!(status, 200);
    assert_eq!(count(repo.path(), "scope_widened"), 1);
    let found = notes(repo.path());
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].1.contains("docs/extra.md") && found[0].1.contains("crates/other"),
        "{found:?}"
    );
    let widened = events(repo.path())
        .into_iter()
        .find(|event| event["kind"] == "scope_widened")
        .expect("the widening");
    assert_eq!(widened["paths"], json!(["docs/extra.md", "crates/other"]));
}

#[test]
fn answering_keep_writes_nothing() {
    let (repo, state, home) = fixture();
    assert!(raise(repo.path(), "T1").status.success());
    let _service = Service::start(repo.path(), state.path(), home.path());
    assert_eq!(answer(state.path(), repo.path(), "keep the scope"), 200);
    assert_eq!(count(repo.path(), "scope_widened"), 0);
    assert!(notes(repo.path()).is_empty());
}

#[test]
fn the_verb_is_refused_for_an_unknown_task() {
    let (repo, _state, _home) = fixture();
    let output = raise(repo.path(), "NOPE");
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("NOPE"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(count(repo.path(), "ask_raised"), 0);
}

#[test]
fn the_help_documents_the_verb() {
    let output = run(Path::new("."), None, &["agent", "task", "widen", "--help"]);
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(
        help.contains("keep the scope") && help.contains("--reason"),
        "{help}"
    );
}
