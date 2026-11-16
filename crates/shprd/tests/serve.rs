//! End-to-end for `shprd serve`: the real binary writes its discovery files,
//! answers the API on a loopback port, and takes its `service.json` away when
//! `SIGTERM` stops it.
//!
//! The service is a real child process on a real loopback socket, which is the
//! point: the router's own tests drive it in-process, and this one proves the
//! subcommand wires the same thing up. Nothing here leaves the machine.
//!
//! Two of these tests run the service on the **Herdr desktop host**, over the
//! fake `hc connect` the host crate's tests speak the protocol with and a fake
//! window, and assert it stops with exit code 0 on `SIGTERM` and on `SIGINT`.
//! That is a regression test with a history: the desktop host owns a tokio
//! runtime, and a runtime dropped inside the service's async context panicked on
//! every clean shutdown, so Ctrl-C used to exit 101.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;

mod support;

use support::herdr::{absent_socket, isolate};

/// The binary under test, built by cargo for this integration test.
const BIN: &str = env!("CARGO_BIN_EXE_shprd");

/// The fake `hc connect` the host crate's tests drive the client with.
///
/// Compiled in rather than read at run time, so there is one fake and not two,
/// and so this test does not care where the other crate's test support sits.
const FAKE_HC: &str = include_str!("../../shprd-mux-hc/tests/support/fake_hc.py");

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
    /// pointed at a multiplexer socket that does not exist (see [`isolate`]): a
    /// pass must never reach the person's own herdr session, and a start that
    /// cannot find a host is exactly the failure the tests about visible
    /// failures are about.
    fn start_with(
        repo: &Path,
        state: &Path,
        home: &Path,
        json: bool,
        stdout: Stdio,
        stderr: Stdio,
    ) -> Self {
        let mut command = Command::new(BIN);
        isolate(&mut command, state);
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
            // The fixture's own config and data homes: a service that read the
            // machine's real ones would see whatever the developer has installed.
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_DATA_HOME", home.join(".local").join("share"))
            // `isolate` already put `HERDR_SOCKET_PATH` at an absent socket in
            // `state`; this states the fact the failure tests below rely on.
            .env("HERDR_SOCKET_PATH", absent_socket(state))
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

    /// Starts a service on the Herdr desktop host: a fake `hc`, a fake window,
    /// and this fixture's own homes.
    ///
    /// It is record-only off, because a record-only service drops the host where
    /// it builds it; the host a real service keeps lives in the closures its
    /// passes run, and that is the one whose drop has to be quiet.
    fn start_on_desktop(repo: &Path, state: &Path, home: &Path) -> Self {
        let (hc, window) = fake_desktop(state);
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
            ])
            .env("XDG_STATE_HOME", state)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_DATA_HOME", home.join(".local").join("share"))
            .env("HERDR_APP_HOME", state.join("herdr-app"))
            // The two programs the desktop host is made of, and this test's own.
            .env("SHPRD_HC", &hc)
            .env("SHPRD_HERDR_APP", &window)
            .env_remove("SHPRD_TICK_RECORD_ONLY")
            .env_remove("SHPRD_ACTOR")
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
    fn stop(self) {
        let _ = self.stop_with("-TERM");
    }

    /// Sends `signal` (a `kill` name such as `-TERM`) and answers how the
    /// service ended.
    fn stop_with(mut self, signal: &str) -> std::process::ExitStatus {
        let _ = Command::new("kill")
            .arg(signal)
            .arg(self.child.id().to_string())
            .status();
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(status)) => return status,
                Ok(None) => sleep(Duration::from_millis(50)),
                Err(err) => panic!("waiting for the service failed: {err}"),
            }
        }
        panic!("the service did not stop after {signal}");
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A fake herdr-core and a fake Herdr window, as the two binaries the desktop
/// host runs.
///
/// The core is [`FAKE_HC`] behind a shell wrapper (the client spawns the binary
/// with `connect` as its only argument, as a real `hc` takes it); the window is a
/// script answering the `herdr-app do …` commands the host sends (M7ef §2.12).
fn fake_desktop(dir: &Path) -> (PathBuf, PathBuf) {
    let at = dir.join("desktop");
    fs::create_dir_all(&at).expect("make the fake desktop's folder");
    let script = at.join("fake_hc.py");
    fs::write(&script, FAKE_HC).expect("write the fake core");
    fs::write(at.join("mode"), "normal").expect("write the fake core's mode");
    let hc = at.join("hc");
    fs::write(
        &hc,
        format!(
            "#!/bin/sh\nFAKE_HC_DIR='{}' exec python3 '{}' \"$@\"\n",
            at.display(),
            script.display()
        ),
    )
    .expect("write the fake core's wrapper");
    let window = at.join("herdr-app");
    fs::write(
        &window,
        "#!/bin/sh
if [ \"$1\" = \"status\" ]; then
  echo '{\"layout\":\"main\"}'
  exit 0
fi
case \"$2\" in
  space.new) echo '{\"space\":\"spfake\",\"tab\":\"tbfake\",\"terminal\":\"t100\"}' ;;
  *)         echo '{\"ok\":true}' ;;
esac
",
    )
    .expect("write the fake window");
    for binary in [&hc, &window] {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mut permissions = fs::metadata(binary).expect("the fake exists").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(binary, permissions).expect("make the fake executable");
        }
    }
    (hc, window)
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
    let mut start = Command::new(BIN);
    isolate(&mut start, state.path());
    let started = start
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
    let mut command = Command::new(BIN);
    isolate(&mut command, state);
    let output = command
        .arg("--repo")
        .arg(repo)
        .arg("--home")
        .arg(home)
        .args(args)
        .env("XDG_STATE_HOME", state)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local").join("share"))
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
fn a_service_on_the_desktop_host_stops_with_code_zero_on_sigterm() {
    // The Herdr desktop host owns a tokio runtime. A runtime shut down inside an
    // async context used to panic — "Cannot drop a runtime in a context where
    // blocking is not allowed" — so every Ctrl-C on Herdr desktop exited 101.
    // This is the whole path a supervisor takes: start, then SIGTERM.
    let (repo, state, home) = repo_with_a_run();
    let service = Service::start_on_desktop(repo.path(), state.path(), home.path());
    let status = service.stop_with("-TERM");
    assert!(
        status.success(),
        "the service must stop quietly, and it stopped with {status}"
    );
}

#[test]
fn a_service_on_the_desktop_host_stops_with_code_zero_on_sigint() {
    let (repo, state, home) = repo_with_a_run();
    let service = Service::start_on_desktop(repo.path(), state.path(), home.path());
    let status = service.stop_with("-INT");
    assert!(
        status.success(),
        "Ctrl-C must stop the service quietly too, and it stopped with {status}"
    );
}

#[test]
fn a_second_serve_for_the_same_state_directory_is_refused() {
    let (repo, state, home) = repo_with_a_run();
    let first = Service::start(repo.path(), state.path(), home.path());
    let record = read_record(state.path());

    // A second service would take the record over and leave the first one
    // running with no way to find it, so it is refused — and the refusal names
    // the service that already holds the directory.
    let mut second_serve = Command::new(BIN);
    isolate(&mut second_serve, state.path());
    let second = second_serve
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
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .env("XDG_DATA_HOME", home.path().join(".local").join("share"))
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
fn a_serve_right_after_a_kill_takes_over_from_the_stale_record() {
    // Bug 7 of the M10 gate: after `pkill -f "shprd serve"` a start within a
    // second or two was refused with "already running (pid N)" — the flock is
    // still held by the dying process, and `service.json` still names it. The
    // start looks at the process, waits the lock out, and takes the directory.
    let (repo, state, home) = repo_with_a_run();
    let first = Service::start(repo.path(), state.path(), home.path());
    let stale = read_record(state.path());
    let stale_pid = stale["pid"].as_u64().expect("a pid");
    // `SIGKILL`, so the process cannot remove its own record: exactly the state a
    // `pkill -9` leaves behind.
    first.stop_with("-KILL");
    assert!(
        record_path(state.path()).exists(),
        "a killed service leaves its record behind"
    );

    // The start right after the kill must not believe the record. It waits for
    // the *new* record, since the dead one is already on disk and would
    // otherwise answer `wait_for_record` at once.
    let second = Service::start(repo.path(), state.path(), home.path());
    let deadline = Instant::now() + Duration::from_secs(15);
    let fresh = loop {
        let record = read_record(state.path());
        if record["id"] != stale["id"] {
            break record;
        }
        assert!(
            Instant::now() < deadline,
            "the new service never wrote its own record: {record}"
        );
        sleep(Duration::from_millis(50));
    };
    assert_ne!(
        fresh["id"], stale["id"],
        "the new start is its own service, not the dead one"
    );
    assert_ne!(
        fresh["pid"].as_u64(),
        Some(stale_pid),
        "the record now names the new process"
    );
    let port = fresh["port"].as_u64().expect("a port");
    let (status, body) =
        get(&format!("http://127.0.0.1:{port}/api/health"), None).expect("the new service answers");
    assert_eq!(status, 200);
    let health: Value = serde_json::from_str(&body).expect("health is JSON");
    assert_eq!(health["id"], fresh["id"]);

    second.stop();
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
    let mut status_command = Command::new(BIN);
    isolate(&mut status_command, state.path());
    let status = status_command
        .arg("--repo")
        .arg(repo.path())
        .arg("--home")
        .arg(home.path())
        .args(["status", "--json"])
        .env("XDG_STATE_HOME", state.path())
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .env("XDG_DATA_HOME", home.path().join(".local").join("share"))
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

    // A finished run is not warned about for ever: nothing can act on it, and no
    // pass will run again to clear the record.
    shprd(
        repo.path(),
        home.path(),
        state.path(),
        &["run", "finish", "--abandon", "--yes"],
    );
    let mut finished_command = Command::new(BIN);
    isolate(&mut finished_command, state.path());
    let finished = finished_command
        .arg("--repo")
        .arg(repo.path())
        .arg("--home")
        .arg(home.path())
        .arg("status")
        .env("XDG_STATE_HOME", state.path())
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .env("XDG_DATA_HOME", home.path().join(".local").join("share"))
        .output()
        .expect("status runs");
    assert!(finished.status.success());
    let finished_stderr = String::from_utf8_lossy(&finished.stderr);
    assert!(
        !finished_stderr.contains("the last pass failed"),
        "a finished run's failure is history, not a warning: {finished_stderr}"
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

#[test]
fn a_json_service_keeps_stdout_one_document_when_the_run_starts() {
    // The review of fix I: `--json` promises one machine-readable document on
    // stdout, and the "run started after serve" notice appended a human line to
    // the banner, so a consumer's `json.load` failed on the second line. The
    // notice is news for a person, so it goes to stderr in this mode.
    let (repo, state, home) = repo_without_a_run();
    fast_tick(repo.path());
    let out_path = state.path().join("serve.out");
    let err_path = state.path().join("serve.err");
    let service = Service::start_with(
        repo.path(),
        state.path(),
        home.path(),
        true,
        Stdio::from(fs::File::create(&out_path).expect("the stdout file")),
        Stdio::from(fs::File::create(&err_path).expect("the stderr file")),
    );
    let port = read_record(state.path())["port"].as_u64().expect("a port") as u16;
    let base = format!("http://127.0.0.1:{port}");
    let token = read_token(state.path());

    // The banner is one document, and it says there is no run.
    let banner = read_when(&out_path, "\"serving\"");
    let value: Value = serde_json::from_str(&banner).expect("the banner is one JSON document");
    assert!(value["run"].is_null(), "{value}");
    assert_eq!(value["hint"], Value::from(shprd_server::NO_RUN_HINT));

    // A run started afterwards reaches the board, and the service says so — on
    // stderr, where it cannot spoil the document.
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
    let notice = read_when(&err_path, "started after serve");
    assert!(notice.contains("r1"), "the notice names the run: {notice}");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let snapshot = state_of(&base, &token);
        if snapshot["run"]["id"] == "r1" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the run never reached the board: {snapshot}"
        );
        sleep(Duration::from_millis(100));
    }

    service.stop();

    // And stdout is still exactly the one document it started as.
    let out = fs::read_to_string(&out_path).expect("the output is written");
    let value: Value = serde_json::from_str(&out).expect("stdout is still one JSON document");
    assert!(value["run"].is_null(), "{value}");
}

#[test]
fn a_service_says_what_it_did_about_the_repositorys_space() {
    // Bug 6 of the M9 gate: the space label is derived from the folder name, so
    // leaving the space to a person is a run that refuses every pass until
    // someone reads a refusal. The service prepares the coordinator's space at
    // startup and says which it did. Here the socket names no host at all, so
    // the banner says it could not, in one line that names the label it wanted.
    let label = |repo: &Path| {
        format!(
            "shprd · {}",
            repo.file_name().expect("a repo name").to_string_lossy()
        )
    };

    let (repo, state, home) = repo_with_a_run();
    let out_path = state.path().join("serve.out");
    let err_path = state.path().join("serve.err");
    let service = Service::start_with(
        repo.path(),
        state.path(),
        home.path(),
        false,
        Stdio::from(fs::File::create(&out_path).expect("the stdout file")),
        Stdio::from(fs::File::create(&err_path).expect("the stderr file")),
    );
    let banner = read_when(&out_path, "space:");
    assert!(
        banner.contains(&label(repo.path())),
        "the banner names the label a run needs: {banner}"
    );
    assert!(
        banner.contains("not ready"),
        "and says the space is not there: {banner}"
    );
    let warning = read_when(&err_path, "not ready");
    assert!(
        warning.contains(&label(repo.path())),
        "the warning names the label too: {warning}"
    );
    service.stop();

    // The machine-readable banner carries the same news, as one document.
    let (repo, state, home) = repo_with_a_run();
    let out_path = state.path().join("serve.out");
    let service = Service::start_with(
        repo.path(),
        state.path(),
        home.path(),
        true,
        Stdio::from(fs::File::create(&out_path).expect("the stdout file")),
        Stdio::null(),
    );
    let banner = read_when(&out_path, "\"serving\"");
    let value: Value = serde_json::from_str(&banner).expect("the banner is one JSON document");
    assert_eq!(value["space"]["state"], "unavailable", "{value}");
    let reason = value["space"]["reason"]
        .as_str()
        .expect("the reason is a string");
    assert!(reason.contains(&label(repo.path())), "{reason}");
    service.stop();
}

#[test]
fn the_service_touches_a_change_file_a_herdr_mod_can_watch() {
    // A Herdr desktop mod cannot read a stream: its `fetch` is buffered and it
    // has no WebSocket (M7ef §2.11). Its live arm is the long poll plus
    // `watchFolder`, so the service keeps one small file that it rewrites after
    // every ledger write, and the window reads that instead of guessing.
    let (repo, state, home) = repo_with_a_run();
    // Held for the whole test: its `Drop` is what stops the service.
    let _service = Service::start(repo.path(), state.path(), home.path());
    let change = state.path().join("shprd").join("changes");

    // It is there as soon as the service is: a window that starts first has a
    // real answer rather than a missing file.
    let first: Value = serde_json::from_str(
        &fs::read_to_string(&change).expect("the change file is written at start"),
    )
    .expect("the change file is JSON");
    let start_seq = first["seq"].as_u64().expect("a sequence");
    assert_eq!(start_seq, 1, "the run start is the log's first event");
    assert!(first["at_ms"].as_u64().expect("a time") > 0);

    // A write through the API moves it, ahead of the answer the caller reads.
    let port = read_record(state.path())["port"].as_u64().expect("a port") as u16;
    let base = format!("http://127.0.0.1:{port}");
    let token = read_token(state.path());
    let added = serde_json::json!({
        "kind": "task.add",
        "task": "T1",
        "title": "a task",
        "scope": ["crates/a"],
        "acceptance": ["it works"],
    });
    assert_eq!(
        post_action(&format!("{base}/api/action"), &token, true, &added),
        Ok(200)
    );
    let second: Value =
        serde_json::from_str(&fs::read_to_string(&change).expect("the change file is there"))
            .expect("JSON");
    assert_eq!(second["seq"].as_u64(), Some(2), "{second}");
    assert!(
        second["at_ms"].as_u64() >= first["at_ms"].as_u64(),
        "the file moves forward"
    );

    // And a write made by another process — a worker's own verb — moves it too,
    // because the service watches the log and publishes what it finds.
    shprd(
        repo.path(),
        home.path(),
        state.path(),
        &["task", "status", "T1", "--msg", "from the command line"],
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut third = second.clone();
    while Instant::now() < deadline {
        third =
            serde_json::from_str(&fs::read_to_string(&change).expect("the change file is there"))
                .expect("JSON");
        if third["seq"].as_u64() == Some(3) {
            break;
        }
        sleep(Duration::from_millis(50));
    }
    assert_eq!(
        third["seq"].as_u64(),
        Some(3),
        "a write by another process reaches a watching mod: {third}"
    );
}
