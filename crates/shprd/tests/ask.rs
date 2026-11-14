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

use serde_json::Value;
use tempfile::TempDir;

/// The binary under test, built by cargo for this integration test.
const BIN: &str = env!("CARGO_BIN_EXE_shprd");

/// A child `shprd serve`, stopped on drop so a failing test leaves nothing
/// running.
struct Service {
    child: Child,
}

impl Service {
    /// Starts the service and waits for its discovery record.
    fn start(repo: &Path, state: &Path, home: &Path) -> Self {
        let child = Command::new(BIN)
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
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if path.exists() {
                return;
            }
            sleep(Duration::from_millis(50));
        }
        panic!("the service never wrote {}", path.display());
    }

    /// Stops the service with `SIGTERM`, as a supervisor would, and waits.
    fn stop(mut self) {
        let _ = Command::new("kill")
            .arg("-TERM")
            .arg(self.child.id().to_string())
            .status();
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => sleep(Duration::from_millis(50)),
                Err(err) => panic!("waiting for the service failed: {err}"),
            }
        }
        panic!("the service did not stop after SIGTERM");
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
    Command::new(BIN)
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

/// One POST of an ask's answer.
fn post_answer(url: &str, token: &str, body: &Value) -> Result<(u16, String), u16> {
    match ureq::post(url)
        .header("authorization", &format!("Bearer {token}"))
        .header("content-type", "application/json")
        .header("x-shprd", "1")
        .send(body.to_string())
    {
        Ok(mut response) => {
            let status = response.status().as_u16();
            let body = response.body_mut().read_to_string().unwrap_or_default();
            Ok((status, body))
        }
        Err(ureq::Error::StatusCode(code)) => Err(code),
        Err(err) => panic!("the answer failed: {err}"),
    }
}

/// Waits for a child that should finish, and returns what it printed.
fn wait_for_child(mut child: Child) -> std::process::Output {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().expect("the child's output"),
            Ok(None) if Instant::now() < deadline => sleep(Duration::from_millis(50)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("the ask hook never finished");
            }
            Err(err) => panic!("waiting for the ask hook failed: {err}"),
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
    let hook = Command::new(BIN)
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
            "30",
        ])
        .env_remove("SHPRD_ACTOR")
        .env_remove("SHPRD_TASK")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the ask hook starts");

    // The ask reaches the queue and the API lists it.
    let ask_path = repo.path().join(".shprd/asks/T1/ask-1.json");
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline && !ask_path.exists() {
        sleep(Duration::from_millis(50));
    }
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

    // A card answers it through the API.
    let answer = serde_json::json!({"task": "T1", "ask": "ask-1", "answer": "allow"});
    let (status, body) =
        post_answer(&format!("{base}/api/asks/answer"), &token, &answer).expect("the answer lands");
    assert_eq!(status, 200, "{body}");
    let reply: Value = serde_json::from_str(&body).expect("the reply is JSON");
    assert_eq!(reply["ok"], true);
    assert_eq!(reply["ask"]["id"], "ask-1");

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
    assert_eq!(
        post_answer(&format!("{base}/api/asks/answer"), &token, &answer),
        Err(409)
    );
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
fn a_hook_that_times_out_prints_nothing() {
    let (repo, state, home) = repo_with_a_task();
    let _service = Service::start(repo.path(), state.path(), home.path());
    let output = Command::new(BIN)
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
        started.elapsed() < Duration::from_secs(20),
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
