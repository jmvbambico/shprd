//! End-to-end for `shprd serve`: the real binary writes its discovery files,
//! answers the API on a loopback port, and takes its `service.json` away when
//! `SIGTERM` stops it.
//!
//! The service is a real child process on a real loopback socket, which is the
//! point: the router's own tests drive it in-process, and this one proves the
//! subcommand wires the same thing up. Nothing here leaves the machine.

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
    /// Starts the service and waits for the discovery file.
    fn start(repo: &Path, state: &Path, home: &Path) -> Self {
        Self::start_with(repo, state, home, true, Stdio::null(), Stdio::null())
    }

    /// Starts the service with its output going where the test can read it.
    ///
    /// `json` picks the banner's shape. Every service these tests start is
    /// pointed at a multiplexer socket that does not exist: a pass must never
    /// reach the person's own herdr session, and a start that cannot find a host
    /// is exactly the failure the tests about visible failures are about.
    fn start_with(
        repo: &Path,
        state: &Path,
        home: &Path,
        json: bool,
        stdout: Stdio,
        stderr: Stdio,
    ) -> Self {
        let mut command = Command::new(BIN);
        command.args([
            "--repo",
            &repo.display().to_string(),
            "--home",
            &home.display().to_string(),
            "serve",
            "--port",
            "0",
        ]);
        if json {
            command.arg("--json");
        }
        let child = command
            .env("XDG_STATE_HOME", state)
            .env("HERDR_SOCKET_PATH", state.join("no-herdr-server.sock"))
            .env_remove("SHPRD_TICK_RECORD_ONLY")
            .env_remove("SHPRD_ACTOR")
            .stdout(stdout)
            .stderr(stderr)
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

/// A throwaway repository root, with a run started in it.
fn repo_with_a_run() -> (TempDir, TempDir, TempDir) {
    let repo = TempDir::new().expect("repo dir");
    let state = TempDir::new().expect("state dir");
    let home = TempDir::new().expect("home dir");
    let started = Command::new(BIN)
        .args([
            "--repo",
            &repo.path().display().to_string(),
            "--home",
            &home.path().display().to_string(),
            "run",
            "start",
            "--run-id",
            "r1",
            "--base",
            "main",
            "--integration",
            "integration/m8",
        ])
        .output()
        .expect("run start runs");
    assert!(
        started.status.success(),
        "run start failed: {}",
        String::from_utf8_lossy(&started.stderr)
    );
    (repo, state, home)
}

/// A throwaway repository root, with no run started in it at all.
fn repo_without_a_run() -> (TempDir, TempDir, TempDir) {
    (
        TempDir::new().expect("repo dir"),
        TempDir::new().expect("state dir"),
        TempDir::new().expect("home dir"),
    )
}

/// Makes the repository's passes run a second apart, so a test need not wait
/// for the fifteen-second default.
fn fast_tick(repo: &Path) {
    let dir = repo.join(".shprd");
    fs::create_dir_all(&dir).expect("mkdir .shprd");
    fs::write(dir.join("config.toml"), "[tick]\ninterval_secs = 1\n")
        .expect("write the tick config");
}

/// Runs one `shprd` verb in `repo`, asserting it worked.
fn shprd(repo: &Path, home: &Path, state: &Path, args: &[&str]) -> String {
    let output = Command::new(BIN)
        .arg("--repo")
        .arg(repo)
        .arg("--home")
        .arg(home)
        .args(args)
        .env("XDG_STATE_HOME", state)
        .output()
        .expect("the verb runs");
    assert!(
        output.status.success(),
        "shprd {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// The `/api/state` document, as of the moment it is read.
fn state_of(base: &str, token: &str) -> Value {
    let (status, body) = get(&format!("{base}/api/state"), Some(token)).expect("state answers");
    assert_eq!(status, 200);
    serde_json::from_str(&body).expect("state is JSON")
}

/// The record the service wrote.
fn read_record(state: &Path) -> Value {
    let text = fs::read_to_string(record_path(state)).expect("the record is there");
    serde_json::from_str(&text).expect("the record is JSON")
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

/// One POST of an action envelope.
fn post_action(url: &str, token: &str, write_header: bool, body: &Value) -> Result<u16, u16> {
    let mut request = ureq::post(url)
        .header("authorization", &format!("Bearer {token}"))
        .header("content-type", "application/json");
    if write_header {
        request = request.header("x-shprd", "1");
    }
    match request.send(body.to_string()) {
        Ok(response) => Ok(response.status().as_u16()),
        Err(ureq::Error::StatusCode(code)) => Err(code),
        Err(err) => panic!("the action failed: {err}"),
    }
}

#[test]
fn a_second_serve_for_the_same_state_directory_is_refused() {
    let (repo, state, home) = repo_with_a_run();
    let first = Service::start(repo.path(), state.path(), home.path());
    let record = read_record(state.path());

    // A second service would take the record over and leave the first one
    // running with no way to find it, so it is refused — and the refusal names
    // the service that already holds the directory.
    let second = Command::new(BIN)
        .args([
            "--repo",
            &repo.path().display().to_string(),
            "--home",
            &home.path().display().to_string(),
            "serve",
            "--port",
            "0",
            "--json",
        ])
        .env("XDG_STATE_HOME", state.path())
        .output()
        .expect("the second serve runs");
    assert_eq!(
        second.status.code(),
        Some(2),
        "a refused verb exits 2, as the rest of the CLI does"
    );
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        stderr.contains("already running"),
        "the refusal says what is running: {stderr}"
    );
    assert_eq!(
        read_record(state.path()),
        record,
        "the running service's record is untouched by the refusal"
    );

    // The first service is still there, and still discoverable.
    let port = record["port"].as_u64().expect("a port");
    let (status, _) = get(&format!("http://127.0.0.1:{port}/api/health"), None)
        .expect("the first service still answers");
    assert_eq!(status, 200);

    first.stop();
    assert!(!record_path(state.path()).exists());
}

#[test]
fn serve_answers_the_api_and_leaves_no_record_behind() {
    let (repo, state, home) = repo_with_a_run();
    let service = Service::start(repo.path(), state.path(), home.path());
    let record = read_record(state.path());
    let port = record["port"].as_u64().expect("a port") as u16;
    let id = record["id"].as_str().expect("an id").to_string();
    assert_eq!(record["version"], Value::from(shprd_server::VERSION));
    assert_eq!(record["pid"].as_u64(), Some(u64::from(service.child.id())));
    let base = format!("http://127.0.0.1:{port}");

    // The handshake needs no token, and carries the id the record holds.
    let (status, body) = get(&format!("{base}/api/health"), None).expect("health answers");
    assert_eq!(status, 200);
    let health: Value = serde_json::from_str(&body).expect("health is JSON");
    assert_eq!(health["id"], Value::from(id));
    assert_eq!(health["ok"], Value::from(true));

    // Everything else needs the token.
    assert_eq!(get(&format!("{base}/api/state"), None), Err(401));
    let token = read_token(state.path());
    let (status, body) = get(&format!("{base}/api/state"), Some(&token)).expect("state answers");
    assert_eq!(status, 200);
    let snapshot: Value = serde_json::from_str(&body).expect("state is JSON");
    assert_eq!(snapshot["seq"], Value::from(1));

    // A write needs both the token and the custom header.
    let action = serde_json::json!({
        "kind": "task.add",
        "task": "T1",
        "title": "from the test",
        "scope": ["crates/shprd"],
        "acceptance": ["it works"],
    });
    assert_eq!(
        post_action(&format!("{base}/api/action"), &token, false, &action),
        Err(403)
    );
    assert_eq!(
        post_action(&format!("{base}/api/action"), &token, true, &action),
        Ok(200)
    );

    // The long poll answers the event the write appended.
    let (status, body) = get(
        &format!("{base}/api/changes?since=1&timeout=5"),
        Some(&token),
    )
    .expect("the change answers");
    assert_eq!(status, 200);
    let change: Value = serde_json::from_str(&body).expect("the change is JSON");
    assert_eq!(change["seq"], Value::from(2));
    assert_eq!(change["events"][0]["kind"], Value::from("task_added"));

    // The service's writes leave the dagr document behind, as the CLI's do:
    // the document `run start` wrote is removed, then a write brings it back.
    let dagr = repo.path().join(".dagr").join("run.json");
    fs::remove_dir_all(repo.path().join(".dagr")).expect("the document is removed");
    assert!(!dagr.exists());

    // `task.status` is the CLI's progress verb: it appends and records the line.
    let progress = serde_json::json!({
        "kind": "task.status",
        "task": "T1",
        "msg": "halfway there",
        "progress": 50,
    });
    assert_eq!(
        post_action(&format!("{base}/api/action"), &token, true, &progress),
        Ok(200)
    );
    let (_, body) = get(&format!("{base}/api/state"), Some(&token)).expect("state answers");
    let snapshot: Value = serde_json::from_str(&body).expect("state is JSON");
    assert_eq!(snapshot["seq"], Value::from(3));
    assert_eq!(
        snapshot["tasks"][0]["last_status"]["message"],
        Value::from("halfway there")
    );
    assert_eq!(
        snapshot["tasks"][0]["last_status"]["progress"],
        Value::from(50)
    );
    assert!(
        dagr.exists(),
        "a write through the service rewrites the dagr document"
    );

    // A clean stop takes service.json with it.
    service.stop();
    assert!(
        !record_path(state.path()).exists(),
        "a clean shutdown removes service.json"
    );
    assert_eq!(
        fs::read_to_string(state.path().join("shprd").join("token"))
            .expect("the token stays for the next start")
            .trim()
            .len(),
        64
    );
}

#[test]
fn a_service_started_before_the_run_says_so_and_sees_it_without_a_restart() {
    // Bug 7 of the M9 gate: a service started in a repository whose run does
    // not exist yet looked exactly like a run with no tasks — in the banner and
    // on the board. Both say what to do instead, and the board picks the run up
    // the moment `shprd run start` writes it.
    let (repo, state, home) = repo_without_a_run();
    fast_tick(repo.path());
    let banner_path = state.path().join("serve.out");
    let banner = fs::File::create(&banner_path).expect("the banner file");
    let service = Service::start_with(
        repo.path(),
        state.path(),
        home.path(),
        false,
        Stdio::from(banner),
        Stdio::null(),
    );
    let port = read_record(state.path())["port"].as_u64().expect("a port") as u16;
    let base = format!("http://127.0.0.1:{port}");
    let token = read_token(state.path());

    // The banner names the verb, not an empty run.
    let text = fs::read_to_string(&banner_path).expect("the banner is written");
    assert!(text.contains("no run yet"), "the banner says so: {text}");
    assert!(
        text.contains("shprd run start"),
        "the banner names the verb: {text}"
    );

    // And the board says the same thing.
    let snapshot = state_of(&base, &token);
    assert!(snapshot["run"].is_null(), "{snapshot}");
    assert_eq!(
        snapshot["hint"],
        Value::from(shprd_server::NO_RUN_HINT),
        "{snapshot}"
    );

    // A run started by another process reaches the same service, with no
    // restart: the snapshot is folded on every request.
    shprd(
        repo.path(),
        home.path(),
        state.path(),
        &[
            "run",
            "start",
            "--run-id",
            "r1",
            "--base",
            "main",
            "--integration",
            "integration/m9",
        ],
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    let snapshot = loop {
        let snapshot = state_of(&base, &token);
        if snapshot["run"]["id"] == "r1" {
            break snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "the run never reached the board: {snapshot}"
        );
        sleep(Duration::from_millis(100));
    };
    assert!(
        snapshot.get("hint").is_none(),
        "a board that can advance needs no hint: {snapshot}"
    );
    assert_eq!(
        snapshot["run"]["integration_branch"],
        Value::from("integration/m9")
    );

    // The service said so on the output the banner used, once the first pass
    // after the run start had run.
    let out = read_when(&banner_path, "started after serve");
    assert!(out.contains("r1"), "the service names the run: {out}");

    service.stop();
}

#[test]
fn a_failed_step_warns_on_the_services_stderr_and_reaches_the_board() {
    // Bug 1 of the M9 gate: `serve` installed no subscriber, so every
    // `tracing::warn!` from a failed step was dropped and the operator never
    // heard that the pass had failed. The warning is on stderr now, and the
    // failure is on the board beside the tasks it left where they were.
    let (repo, state, home) = repo_with_a_run();
    fast_tick(repo.path());
    shprd(
        repo.path(),
        home.path(),
        state.path(),
        &[
            "agent",
            "task",
            "add",
            "--id",
            "T1",
            "--title",
            "a task nothing can start",
            "--scope",
            "a.txt",
            "--acceptance",
            "it works",
        ],
    );

    let log_path = state.path().join("serve.err");
    let log = fs::File::create(&log_path).expect("the log file");
    let service = Service::start_with(
        repo.path(),
        state.path(),
        home.path(),
        true,
        Stdio::null(),
        Stdio::from(log),
    );
    let port = read_record(state.path())["port"].as_u64().expect("a port") as u16;
    let base = format!("http://127.0.0.1:{port}");
    let token = read_token(state.path());

    // The pass cannot start the task — there is no multiplexer at the socket
    // this test points at — and says so on the board.
    let deadline = Instant::now() + Duration::from_secs(30);
    let snapshot = loop {
        let snapshot = state_of(&base, &token);
        let failed = snapshot["last_tick"]["failed"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if !failed.is_empty() {
            break snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "the failed pass never reached the board: {snapshot}"
        );
        sleep(Duration::from_millis(100));
    };
    let failed = &snapshot["last_tick"]["failed"][0];
    assert_eq!(failed["task"], Value::from("T1"), "the task is named");
    assert_eq!(failed["step"], Value::from("start"), "the step is named");
    let error = failed["error"].as_str().expect("the error is a string");
    assert!(!error.is_empty(), "the error says why: {failed}");
    assert!(
        snapshot["last_tick"]["steps"].as_u64().unwrap_or_default() > 0,
        "the pass is counted: {snapshot}"
    );
    assert!(
        !snapshot["tasks"].is_null() && snapshot["tasks"][0]["id"] == "T1",
        "the board still shows the task the pass could not start: {snapshot}"
    );

    service.stop();

    // `shprd status` says the same thing in one line on stderr, so the board on
    // stdout is untouched — a `--json` caller still gets one document.
    let status = Command::new(BIN)
        .arg("--repo")
        .arg(repo.path())
        .arg("--home")
        .arg(home.path())
        .args(["status", "--json"])
        .env("XDG_STATE_HOME", state.path())
        .output()
        .expect("status runs");
    assert!(
        status.status.success(),
        "status failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    serde_json::from_slice::<Value>(&status.stdout).expect("the status JSON is still one document");
    let status_warning = String::from_utf8_lossy(&status.stderr);
    assert!(
        status_warning.contains("warning: the last pass failed at `start` on T1"),
        "status says the pass failed: {status_warning}"
    );

    // The same failure, in the service's own words on stderr: proof that
    // `serve` installed the subscriber every `tracing` line goes through.
    let stderr = fs::read_to_string(&log_path).expect("the log is written");
    assert!(
        stderr.contains("tick step failed"),
        "the failed step was logged: {stderr}"
    );
    assert!(stderr.contains("T1"), "the line names the task: {stderr}");
    // The log's reason is the board's error. The first words are compared and
    // not the whole line: a `tracing` field is quoted and escaped when it holds
    // a quote, and the error is the same text either way.
    let lead: String = error.chars().take(24).collect();
    assert!(
        stderr.contains(&lead),
        "the board's error is the log's error: {stderr}"
    );
}

/// Reads `path` until it holds `needle`, or says what it had.
fn read_when(path: &Path, needle: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let text = fs::read_to_string(path).unwrap_or_default();
        if text.contains(needle) {
            return text;
        }
        assert!(
            Instant::now() < deadline,
            "{} never said {needle:?}: {text}",
            path.display()
        );
        sleep(Duration::from_millis(100));
    }
}
