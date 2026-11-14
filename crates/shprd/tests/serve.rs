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
