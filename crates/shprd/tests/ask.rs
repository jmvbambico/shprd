//! End-to-end for needs-you asks: the real binary raises one as a program's
//! hook would, the API answers it, and the waiting hook prints the decision the
//! program expects.
//!
//! No agent CLI is involved: `shprd ask hook` is the child process a Claude
//! Code or Codex hook runs, and `shprd serve` is the API a card calls.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;

mod support;

use support::herdr::isolate;

/// The binary under test, built by cargo for this integration test.
const BIN: &str = env!("CARGO_BIN_EXE_shprd");

/// How often a wait looks again, ms: short, so an idle machine finishes at once.
const POLL: Duration = Duration::from_millis(20);

/// How long one wait in this file may take before the test gives up, seconds.
///
/// Every wait here crosses a process boundary — a service binding a socket, a
/// hook raising an ask, a log append another process makes — while the rest of
/// the workspace's tests run on the same machine. An idle machine satisfies
/// each in milliseconds; a loaded one can take far longer, so the deadline is
/// generous, and `SHPRD_TEST_DEADLINE_SECS` stretches it further for a slow
/// box without editing the test.
fn deadline_secs() -> u64 {
    std::env::var("SHPRD_TEST_DEADLINE_SECS")
        .ok()
        .and_then(|text| text.parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .unwrap_or(120)
}

/// When a wait that starts now gives up.
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(deadline_secs())
}

/// How long a hook waits for its answer: past the test's own deadline, so a
/// slow answer is never mistaken for a hook that timed out.
fn hook_timeout_secs() -> u64 {
    deadline_secs().saturating_mul(2)
}

/// Waits until `done` holds, or fails saying what was waited for.
///
/// A poll re-checks a fact another process makes true; nothing here sleeps to
/// give another process time, so a loaded machine loses no race — it only
/// waits longer for the fact to appear.
fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = deadline();
    loop {
        if done() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{what} did not happen within {}s",
            deadline_secs()
        );
        sleep(POLL);
    }
}

/// A child `shprd serve`, stopped on drop so a failing test leaves nothing
/// running.
struct Service {
    child: Child,
}

impl Service {
    /// Starts the service and waits for its discovery record.
    ///
    /// `isolate` comes first, and it is what this whole file turns on: `serve`
    /// prepares the repository's workspace at startup through the real herdr
    /// adapter, so a service that inherited the pane's `HERDR_SOCKET_PATH` would
    /// create one in the person's own session. Pointed at a socket that does not
    /// exist, the service warns that the workspace is not ready instead, and the
    /// asks these tests are about still work.
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
        let service = Self { child };
        service.wait_for_record(state);
        service
    }

    /// Polls for `service.json`, which the service writes after it binds.
    fn wait_for_record(&self, state: &Path) {
        let path = record_path(state);
        wait_until(&format!("the service wrote {}", path.display()), || {
            path.exists()
        });
    }

    /// Stops the service with `SIGTERM`, as a supervisor would, and waits.
    fn stop(mut self) {
        let _ = Command::new("kill")
            .arg("-TERM")
            .arg(self.child.id().to_string())
            .status();
        let deadline = deadline();
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if Instant::now() < deadline => sleep(POLL),
                Ok(None) => panic!("the service did not stop after SIGTERM"),
                Err(err) => panic!("waiting for the service failed: {err}"),
            }
        }
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Where the service writes its discovery file.
fn record_path(state: &Path) -> PathBuf {
    state.join("shprd").join("service.json")
}

/// Runs the binary with `--repo <root> --no-dagr` and the given arguments.
fn run(root: &Path, args: &[&str]) -> std::process::Output {
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
        .env_remove("SHPRD_TASK")
        .output()
        .expect("the binary runs")
}

/// Runs a command that must succeed.
fn ok(root: &Path, args: &[&str]) -> String {
    let output = run(root, args);
    assert!(
        output.status.success(),
        "{args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

/// Runs the binary with an extra environment variable set.
fn ok_env(root: &Path, args: &[&str], env: &[(&str, &str)]) -> std::process::Output {
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
    for (key, value) in env {
        command.env(key, value);
    }
    command.output().expect("the binary runs")
}

/// The shipped `data/` directory, which holds the real harness files.
fn data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../data")
        .canonicalize()
        .expect("the shipped data directory")
}

/// Every file under `dir`, relative, sorted; empty when there is none.
fn tree(dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        let Ok(entries) = fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            if let Ok(relative) = path.strip_prefix(dir) {
                found.push(relative.display().to_string());
            }
        }
    }
    found.sort();
    found
}

/// Spawns the binary with an extra environment, without waiting for it.
fn spawn_env(root: &Path, args: &[&str], env: &[(&str, &str)]) -> Child {
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
        .env_remove("SHPRD_TASK")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in env {
        command.env(key, value);
    }
    command.spawn().expect("the binary starts")
}

/// A throwaway repository with a run and one task.
fn repo_with_a_task() -> (TempDir, TempDir, TempDir) {
    let repo = TempDir::new().expect("repo dir");
    let state = TempDir::new().expect("state dir");
    let home = TempDir::new().expect("home dir");
    ok(
        repo.path(),
        &[
            "--home",
            &home.path().display().to_string(),
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
        &[
            "--home",
            &home.path().display().to_string(),
            "agent",
            "task",
            "add",
            "--id",
            "T1",
            "--title",
            "needs you",
            "--scope",
            "crates/shprd",
            "--acceptance",
            "an ask is answered",
        ],
    );
    (repo, state, home)
}

/// The token a client reads from the state directory.
fn read_token(state: &Path) -> String {
    fs::read_to_string(state.join("shprd").join("token"))
        .expect("the token is there")
        .trim()
        .to_string()
}

/// One GET, with the token when it has one.
fn get(url: &str, token: Option<&str>) -> Result<(u16, String), u16> {
    let mut request = ureq::get(url);
    if let Some(token) = token {
        request = request.header("authorization", &format!("Bearer {token}"));
    }
    match request.call() {
        Ok(mut response) => {
            let status = response.status().as_u16();
            let body = response.body_mut().read_to_string().unwrap_or_default();
            Ok((status, body))
        }
        Err(ureq::Error::StatusCode(code)) => Err(code),
        Err(err) => panic!("the request failed: {err}"),
    }
}

/// One POST of an ask's answer, as `(status, body)` whatever the status.
///
/// The body is read on a refusal too, so a failure message carries the
/// service's own words instead of only its status.
fn post_answer(url: &str, token: &str, body: &Value) -> (u16, String) {
    let agent = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build(),
    );
    match agent
        .post(url)
        .header("authorization", &format!("Bearer {token}"))
        .header("content-type", "application/json")
        .header("x-shprd", "1")
        .send(body.to_string())
    {
        Ok(mut response) => {
            let status = response.status().as_u16();
            let body = response.body_mut().read_to_string().unwrap_or_default();
            (status, body)
        }
        Err(err) => panic!("the answer failed: {err}"),
    }
}

/// Waits for a child that should finish, and returns what it printed.
fn wait_for_child(mut child: Child) -> std::process::Output {
    let deadline = deadline();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().expect("the child's output"),
            Ok(None) if Instant::now() < deadline => sleep(POLL),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("the child never finished");
            }
            Err(err) => panic!("waiting for the child failed: {err}"),
        }
    }
}

/// Every event the log holds, newest last.
fn events(repo: &Path) -> Vec<Value> {
    let path = repo.join(".shprd").join("events.jsonl");
    fs::read_to_string(path)
        .expect("the log is there")
        .lines()
        .map(|line| serde_json::from_str(line).expect("an event line"))
        .collect()
}

/// How many `ask_raised` events the log holds.
fn raised_count(repo: &Path) -> usize {
    events(repo)
        .iter()
        .filter(|event| event["kind"] == "ask_raised")
        .count()
}

/// The task object `id` names, from a `status --json` or `/api/state` document.
fn task_of(document: &str, id: &str) -> Value {
    let doc: Value = serde_json::from_str(document).expect("the document is JSON");
    match doc["tasks"]
        .as_array()
        .and_then(|tasks| tasks.iter().find(|task| task["id"] == id))
    {
        Some(task) => task.clone(),
        None => panic!("task {id} missing from {document}"),
    }
}

#[test]
fn a_hook_waits_for_an_answer_a_card_posts_and_prints_it() {
    let (repo, state, home) = repo_with_a_task();
    let service = Service::start(repo.path(), state.path(), home.path());
    let record: Value = serde_json::from_str(
        &fs::read_to_string(record_path(state.path())).expect("the record is there"),
    )
    .expect("the record is JSON");
    let port = record["port"].as_u64().expect("a port");
    let base = format!("http://127.0.0.1:{port}");
    let token = read_token(state.path());

    // The hook a program runs: it raises the ask and waits for an answer.
    let hook_timeout = hook_timeout_secs().to_string();
    let mut hook_command = Command::new(BIN);
    isolate(&mut hook_command, repo.path());
    let hook = hook_command
        .args([
            "--repo",
            &repo.path().display().to_string(),
            "--no-dagr",
            "ask",
            "hook",
            "--program",
            "claude",
            "--task",
            "T1",
            "--kind",
            "permission",
            "--text",
            "Run: cargo test",
            "--option",
            "Allow",
            "--option",
            "Deny",
            "--id",
            "ask-1",
            "--timeout-secs",
            &hook_timeout,
        ])
        .env_remove("SHPRD_ACTOR")
        .env_remove("SHPRD_TASK")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the ask hook starts");

    // The ask reaches the ledger, the queue and the API. The ledger's line is
    // the fact every reader folds, so that is what is waited for — a queue file
    // seen a moment before the append would make `status` and the board race.
    let ask_path = repo.path().join(".shprd/asks/T1/ask-1.json");
    wait_until("the ask reaches the ledger", || {
        raised_count(repo.path()) == 1 && ask_path.exists()
    });
    assert!(
        ask_path.exists(),
        "the hook wrote no ask at {}",
        ask_path.display()
    );
    let (status, body) = get(&format!("{base}/api/asks"), Some(&token)).expect("asks answer");
    assert_eq!(status, 200);
    let listed: Value = serde_json::from_str(&body).expect("asks are JSON");
    assert_eq!(listed["asks"][0]["id"], "ask-1");
    assert_eq!(listed["asks"][0]["task"], "T1");
    assert_eq!(listed["asks"][0]["kind"], "permission");
    assert_eq!(listed["asks"][0]["text"], "Run: cargo test");
    assert_eq!(listed["asks"][0]["options"][0], "Allow");

    // The open card is visible where a person looks: the CLI's status (both the
    // human line and `--json`) and the service's board.
    let human = ok(repo.path(), &["status"]);
    assert!(
        human.contains("T1 Needs you"),
        "the status line must name the open ask: {human}"
    );
    let after_raise = task_of(&ok(repo.path(), &["status", "--json"]), "T1");
    assert_eq!(after_raise["status"], json!("queued"), "the stored state");
    assert_eq!(after_raise["derived"]["status"], json!("needs_you"));
    assert_eq!(after_raise["derived"]["need"], json!("asks"));
    assert_eq!(after_raise["derived"]["reason"], json!("Run: cargo test"));
    let (status, body) = get(&format!("{base}/api/state"), Some(&token)).expect("state answers");
    assert_eq!(status, 200);
    let board: Value = serde_json::from_str(&body).expect("the state is JSON");
    assert_eq!(board["tasks"][0]["status"], json!("queued"));
    assert_eq!(board["tasks"][0]["derived"]["status"], json!("needs_you"));
    assert_eq!(board["tasks"][0]["derived"]["need"], json!("asks"));

    // A card answers it through the API.
    let answer = serde_json::json!({"task": "T1", "ask": "ask-1", "answer": "allow"});
    let (status, body) = post_answer(&format!("{base}/api/asks/answer"), &token, &answer);
    assert_eq!(status, 200, "{body}");
    let reply: Value = serde_json::from_str(&body).expect("the reply is JSON");
    assert_eq!(reply["ok"], true);
    assert_eq!(reply["ask"]["id"], "ask-1");

    // The answer takes Needs you back out of both views.
    let after_answer = task_of(&ok(repo.path(), &["status", "--json"]), "T1");
    assert_eq!(after_answer["status"], json!("queued"));
    assert_eq!(after_answer["derived"]["status"], json!("to_do"));
    assert_eq!(after_answer["derived"]["need"], Value::Null);
    let (_, body) = get(&format!("{base}/api/state"), Some(&token)).expect("state answers");
    let board: Value = serde_json::from_str(&body).expect("the state is JSON");
    assert_eq!(board["tasks"][0]["derived"]["status"], json!("to_do"));
    assert_eq!(board["tasks"][0]["derived"]["need"], Value::Null);

    // The waiting hook prints the program's decision and exits 0.
    let output = wait_for_child(hook);
    assert!(
        output.status.success(),
        "the hook failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let printed: Value = serde_json::from_slice(&output.stdout).expect("a JSON decision");
    assert_eq!(
        printed["hookSpecificOutput"]["decision"]["behavior"],
        "allow"
    );
    assert_eq!(
        printed["hookSpecificOutput"]["hookEventName"],
        "PermissionRequest"
    );

    // The ledger records the raise and the answer, once each.
    let kinds: Vec<String> = events(repo.path())
        .iter()
        .filter_map(|event| event["kind"].as_str())
        .filter(|kind| kind.starts_with("ask_"))
        .map(str::to_string)
        .collect();
    assert_eq!(kinds, vec!["ask_raised", "ask_answered"]);

    // A second answer is refused: an ask is answered once.
    let (status, body) = post_answer(&format!("{base}/api/asks/answer"), &token, &answer);
    assert_eq!(status, 409, "{body}");
    let (status, body) = get(&format!("{base}/api/asks"), Some(&token)).expect("asks answer");
    assert_eq!(status, 200);
    let listed: Value = serde_json::from_str(&body).expect("asks are JSON");
    assert_eq!(
        listed["asks"].as_array().map(Vec::len),
        Some(0),
        "the answered ask is gone: {body}"
    );

    service.stop();
}

#[test]
fn a_raise_waits_for_an_answer_a_card_posts_and_prints_it() {
    let (repo, state, home) = repo_with_a_task();
    let service = Service::start(repo.path(), state.path(), home.path());
    let record: Value = serde_json::from_str(
        &fs::read_to_string(record_path(state.path())).expect("the record is there"),
    )
    .expect("the record is JSON");
    let port = record["port"].as_u64().expect("a port");
    let base = format!("http://127.0.0.1:{port}");
    let token = read_token(state.path());

    // The verb a worker runs when its CLI has no ask hook of its own. It is
    // launched the way a worker is: inside the task's worktree, with the
    // task's environment and no flags naming the run.
    let wait = hook_timeout_secs().to_string();
    let mut child = spawn_env(
        repo.path(),
        &[
            "ask",
            "raise",
            "--kind",
            "question",
            "--text",
            "Should add_ints stay two-argument?",
            "--option",
            "Keep two arguments",
            "--option",
            "Make it variadic",
            "--id",
            "raise-1",
            "--timeout-secs",
            &wait,
        ],
        &[("SHPRD_TASK", "T1"), ("SHPRD_ACTOR", "agent:pi")],
    );

    // The ask reaches the ledger and the queue, and the verb is still
    // blocking: it exits only when an answer is there.
    let ask_path = repo.path().join(".shprd/asks/T1/raise-1.json");
    wait_until("the raise reaches the ledger", || {
        raised_count(repo.path()) == 1 && ask_path.exists()
    });
    match child.try_wait() {
        Ok(None) => {}
        Ok(Some(status)) => panic!("the raise exited before any answer, with {status}"),
        Err(err) => panic!("waiting for the raise failed: {err}"),
    }

    // The card is the worker's, with the choices the verb was given.
    let raised = events(repo.path())
        .into_iter()
        .find(|event| event["kind"] == "ask_raised")
        .expect("the ledger holds the raise");
    assert_eq!(raised["actor"]["type"], "agent");
    assert_eq!(raised["actor"]["id"], "pi");
    let (status, body) = get(&format!("{base}/api/asks"), Some(&token)).expect("asks answer");
    assert_eq!(status, 200);
    let listed: Value = serde_json::from_str(&body).expect("asks are JSON");
    assert_eq!(listed["asks"][0]["id"], "raise-1");
    assert_eq!(listed["asks"][0]["task"], "T1");
    assert_eq!(listed["asks"][0]["kind"], "question");
    assert_eq!(listed["asks"][0]["options"][1], "Make it variadic");

    // The person answers the card through the API.
    let answer = serde_json::json!({
        "task": "T1",
        "ask": "raise-1",
        "answer": "Keep two arguments"
    });
    let (status, body) = post_answer(&format!("{base}/api/asks/answer"), &token, &answer);
    assert_eq!(status, 200, "{body}");

    // The blocking verb prints the answer in the person's own words and exits
    // 0, which is what the worker carries on with.
    let output = wait_for_child(child);
    assert!(
        output.status.success(),
        "the raise failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "Keep two arguments"
    );

    // The ledger records the answer once, and the card is gone.
    assert_eq!(
        events(repo.path())
            .iter()
            .filter(|event| event["kind"] == "ask_answered")
            .count(),
        1
    );
    let (_, body) = get(&format!("{base}/api/asks"), Some(&token)).expect("asks answer");
    let listed: Value = serde_json::from_str(&body).expect("asks are JSON");
    assert_eq!(listed["asks"].as_array().map(Vec::len), Some(0), "{body}");

    service.stop();
}

/// A raise outside any task's worktree says so at once, rather than waiting for
/// an answer to a card the run could never show.
#[test]
fn a_raise_without_a_task_says_what_is_missing() {
    let (repo, _state, _home) = repo_with_a_task();
    let output = run(
        repo.path(),
        &[
            "ask",
            "raise",
            "--kind",
            "question",
            "--text",
            "Which database?",
        ],
    );
    assert!(
        !output.status.success(),
        "a raise with no task must be refused"
    );
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(said.contains("no task for this ask"), "{said}");
    assert!(said.contains("--task"), "{said}");
    // A coordinator's launch carries no `$SHPRD_TASK`, so the refusal must name
    // the verb that does work without one.
    assert!(said.contains("shprd agent escalate"), "{said}");
}

#[test]
fn two_hooks_racing_the_same_id_leave_the_ask_answerable() {
    let (repo, state, home) = repo_with_a_task();
    let service = Service::start(repo.path(), state.path(), home.path());
    let record: Value = serde_json::from_str(
        &fs::read_to_string(record_path(state.path())).expect("the record is there"),
    )
    .expect("the record is JSON");
    let port = record["port"].as_u64().expect("a port");
    let base = format!("http://127.0.0.1:{port}");
    let token = read_token(state.path());

    // Two hooks, the same explicit id. The queue's create-new raise lets only
    // one own the ask; the other must not delete the winner's file, and the
    // ledger must end up with one `ask_raised` however the two commits
    // interleave — a duplicate would leave a log no reader can fold.
    let hook_timeout = hook_timeout_secs().to_string();
    let hook_args = [
        "ask",
        "hook",
        "--program",
        "claude",
        "--task",
        "T1",
        "--kind",
        "permission",
        "--text",
        "A",
        "--id",
        "dup-1",
        "--timeout-secs",
        &hook_timeout,
    ];
    let mut hooks = vec![
        spawn_env(repo.path(), &hook_args, &[]),
        spawn_env(repo.path(), &hook_args, &[]),
    ];

    let ask_path = repo.path().join(".shprd/asks/T1/dup-1.json");
    let give_up = deadline();
    while !(ask_path.exists() && raised_count(repo.path()) == 1) {
        if Instant::now() >= give_up {
            // Say what the hooks themselves said, or the failure is only
            // "nothing happened" on a machine nobody can look at again.
            let mut said = String::new();
            for mut child in hooks.drain(..) {
                let _ = child.kill();
                if let Ok(output) = child.wait_with_output() {
                    said.push_str(&String::from_utf8_lossy(&output.stderr));
                    said.push_str(&String::from_utf8_lossy(&output.stdout));
                }
            }
            panic!(
                "one hook never raised the ask within {}s; the hooks said: {said}",
                deadline_secs()
            );
        }
        sleep(POLL);
    }
    assert_eq!(raised_count(repo.path()), 1, "exactly one ask_raised");
    assert!(
        ask_path.exists(),
        "the racing hooks left no queue file, so the ask can never be answered"
    );

    // The ask is answerable, not stranded as a 404.
    let answer = serde_json::json!({"task": "T1", "ask": "dup-1", "answer": "allow"});
    let (status, body) = post_answer(&format!("{base}/api/asks/answer"), &token, &answer);
    assert_eq!(status, 200, "{body}");

    // Both hooks exit cleanly once the answer is there; at least one printed the
    // program's decision.
    let mut printed = 0;
    for child in hooks {
        let output = wait_for_child(child);
        assert!(
            output.status.success(),
            "a hook failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        if !output.stdout.is_empty() {
            let value: Value = serde_json::from_slice(&output.stdout).expect("a JSON decision");
            assert_eq!(value["hookSpecificOutput"]["decision"]["behavior"], "allow");
            printed += 1;
        }
    }
    // A losing hook's cleanup, if it ran at all, has run by now: both have
    // exited, so nothing is sleeping to give one time. The winner's file is
    // still there to be read.
    assert!(
        ask_path.exists(),
        "a losing hook deleted the winner's ask file"
    );
    assert!(printed >= 1, "no hook printed the answer");
    assert_eq!(
        raised_count(repo.path()),
        1,
        "two hooks still raise the ask once, however they interleave"
    );
    assert_eq!(
        events(repo.path())
            .iter()
            .filter(|event| event["kind"] == "ask_answered")
            .count(),
        1,
        "the ask is answered once"
    );
    // The log still folds, so every reader keeps answering.
    let (status, body) = get(&format!("{base}/api/state"), Some(&token)).expect("state answers");
    assert_eq!(status, 200, "the log still folds: {body}");

    service.stop();
}

#[test]
fn a_hook_that_times_out_prints_nothing() {
    let (repo, state, home) = repo_with_a_task();
    let _service = Service::start(repo.path(), state.path(), home.path());
    let mut hook_command = Command::new(BIN);
    isolate(&mut hook_command, repo.path());
    let output = hook_command
        .args([
            "--repo",
            &repo.path().display().to_string(),
            "--no-dagr",
            "ask",
            "hook",
            "--program",
            "claude",
            "--task",
            "T1",
            "--kind",
            "question",
            "--text",
            "Which database?",
            "--id",
            "ask-2",
            "--timeout-secs",
            "0",
        ])
        .env_remove("SHPRD_ACTOR")
        .env_remove("SHPRD_TASK")
        .stdin(Stdio::null())
        .output()
        .expect("the ask hook runs");
    assert!(output.status.success());
    assert!(
        output.stdout.is_empty(),
        "a timeout decides nothing, got {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn an_install_writes_the_hook_only_inside_the_worktree() {
    let repo = TempDir::new().expect("repo dir");
    let worktree = TempDir::new().expect("worktree");
    let home = TempDir::new().expect("home dir");
    let data = data_dir().display().to_string();
    let output = ok_env(
        repo.path(),
        &[
            "ask",
            "install",
            "--program",
            "claude",
            "--worktree",
            &worktree.path().display().to_string(),
        ],
        &[
            ("SHPRD_DATA_DIR", &data),
            ("HOME", &home.path().display().to_string()),
        ],
    );
    assert!(
        output.status.success(),
        "the install failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Claude Code's own project file gets the hook, and the script it runs.
    let settings = worktree.path().join(".claude/settings.local.json");
    let value: Value = serde_json::from_str(&fs::read_to_string(&settings).expect("read settings"))
        .expect("settings are JSON");
    let command = value["hooks"]["PermissionRequest"][0]["hooks"][0]["command"]
        .as_str()
        .expect("a command");
    let hook = worktree.path().join(".shprd/ask-hook.sh");
    assert!(
        command.contains(hook.to_str().expect("hook path")),
        "{command}"
    );
    assert!(hook.exists(), "the hook script is written");
    assert_eq!(
        tree(worktree.path()),
        vec![".claude/settings.local.json", ".shprd/ask-hook.sh"],
        "nothing else is written into the worktree"
    );

    // Not one byte lands outside the worktree: the person's home is untouched,
    // and no global config file appeared there.
    assert_eq!(
        tree(home.path()),
        Vec::<String>::new(),
        "the person's home is never written"
    );
    assert_eq!(
        tree(repo.path()),
        Vec::<String>::new(),
        "nothing outside the worktree is written, not even the run repo"
    );

    // Installing again changes nothing.
    let output = ok_env(
        repo.path(),
        &[
            "ask",
            "install",
            "--program",
            "claude",
            "--worktree",
            &worktree.path().display().to_string(),
        ],
        &[
            ("SHPRD_DATA_DIR", &data),
            ("HOME", &home.path().display().to_string()),
        ],
    );
    assert!(output.status.success());
    let printed = String::from_utf8_lossy(&output.stdout);
    assert!(printed.contains("already installed"), "{printed}");
    let again: Value = serde_json::from_str(&fs::read_to_string(&settings).expect("read settings"))
        .expect("settings are JSON");
    assert_eq!(again, value, "the second install changed the file");
}

#[test]
fn a_codex_install_turns_the_worktrees_own_hooks_on() {
    let repo = TempDir::new().expect("repo dir");
    let worktree = TempDir::new().expect("worktree");
    let home = TempDir::new().expect("home dir");
    let data = data_dir().display().to_string();
    let output = ok_env(
        repo.path(),
        &[
            "ask",
            "install",
            "--program",
            "codex",
            "--worktree",
            &worktree.path().display().to_string(),
        ],
        &[
            ("SHPRD_DATA_DIR", &data),
            ("HOME", &home.path().display().to_string()),
        ],
    );
    assert!(
        output.status.success(),
        "the install failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let hooks = worktree.path().join(".codex/hooks.json");
    assert!(hooks.exists(), "the project hooks file is written");
    let config = fs::read_to_string(worktree.path().join(".codex/config.toml"))
        .expect("read the project config");
    assert!(config.contains("hooks = true"), "{config}");
    assert_eq!(
        tree(home.path()),
        Vec::<String>::new(),
        "the person's ~/.codex is never written"
    );
    assert_eq!(
        tree(repo.path()),
        Vec::<String>::new(),
        "nothing outside the worktree is written, not even the run repo"
    );
    assert_eq!(
        tree(worktree.path()),
        vec![
            ".codex/config.toml",
            ".codex/hooks.json",
            ".shprd/ask-hook.sh"
        ],
        "nothing else is written into the worktree"
    );
}

#[test]
fn a_programs_harness_says_how_long_its_hook_waits() {
    let (repo, _state, _home) = repo_with_a_task();
    // A data directory whose claude harness asks for a one-second wait, so a
    // hook that reads it returns at once and one that does not would sit for
    // the 1800-second default.
    let data = TempDir::new().expect("data dir");
    fs::create_dir_all(data.path().join("harness")).expect("harness dir");
    let text = fs::read_to_string(data_dir().join("harness/claude.toml")).expect("read harness");
    let text = text.replace("timeout_seconds = 1800", "timeout_seconds = 1");
    assert!(
        text.contains("timeout_seconds = 1"),
        "the harness was edited"
    );
    fs::write(data.path().join("harness/claude.toml"), text).expect("write harness");
    let started = Instant::now();
    let child = spawn_env(
        repo.path(),
        &[
            "ask",
            "hook",
            "--program",
            "claude",
            "--task",
            "T1",
            "--kind",
            "question",
            "--text",
            "Which database?",
            "--id",
            "ask-3",
        ],
        &[("SHPRD_DATA_DIR", &data.path().display().to_string())],
    );
    let output = wait_for_child(child);
    assert!(output.status.success());
    assert!(
        output.stdout.is_empty(),
        "a timeout decides nothing, got {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        started.elapsed() < Duration::from_secs(deadline_secs()),
        "the hook waited far longer than its harness says"
    );
    let raised: Value = serde_json::from_str(
        &fs::read_to_string(repo.path().join(".shprd/asks/T1/ask-3.json"))
            .expect("the ask is there"),
    )
    .expect("the ask is JSON");
    assert_eq!(
        raised["expires_at_ms"].as_u64().expect("an expiry")
            - raised["created_at_ms"].as_u64().expect("a start"),
        1_000,
        "the ask expires when the harness's timeout says"
    );
}
