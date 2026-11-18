//! The rule that a spawned `shprd serve` can never see the person's herdr,
//! tested from the binary's side.
//!
//! `herdr_env.rs` (M10 fix X) is the rule every spawning test applies, and its
//! own test file proves what that rule does to a `Command`. This file is the
//! other half: it proves that the **tests which actually start `serve`** go
//! through it, and that the binary itself refuses the leak when a person or a
//! worker starts it by hand.
//!
//! # Why both halves
//!
//! The incident M11 fix Y closed was not a test that forgot a rule — it was a
//! *live check*. A worker ran `shprd serve` from `<tmp>/SHP-66/live/repo` with
//! the pane's `HERDR_SOCKET_PATH` still in the environment, and the service made
//! a workspace labelled `shprd · repo` in the person's own window. Twice in one
//! day.
//!
//! So there are two things to keep true, and this file keeps both:
//!
//! 1. **Every test that starts the service hands it an isolated environment.**
//!    Asserted here by spawning the binary the way the tests do and reading back
//!    what the child actually received — not what the test meant to send.
//! 2. **The service refuses the leak on its own.** Asserted by running the real
//!    binary against a temporary checkout with the person's socket in its
//!    environment, and reading the banner it prints: no space, no graph
//!    terminal, and a reason that names both paths.
//!
//! # What it cannot check
//!
//! Whether a workspace appeared in somebody's window. Nothing in a test process
//! can ask the person's herdr anything without reaching it, so the checks are
//! the two that can be made without touching it: the environment the child got,
//! and the banner the service printed.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

mod support;

use support::herdr::{PRIVATE_HC_SESSION, absent_socket, isolate};

/// The binary under test, built by cargo for this integration test.
const BIN: &str = env!("CARGO_BIN_EXE_shprd");

/// A throwaway repository, home and state directory, as the serve tests make.
fn fixture() -> (tempfile::TempDir, tempfile::TempDir, tempfile::TempDir) {
    (
        tempfile::TempDir::new().expect("repo dir"),
        tempfile::TempDir::new().expect("state dir"),
        tempfile::TempDir::new().expect("home dir"),
    )
}

/// The value `command` set for `name`, or `None` when it left it alone.
fn value(command: &Command, name: &str) -> Option<std::ffi::OsString> {
    command
        .get_envs()
        .find(|(key, _)| *key == std::ffi::OsStr::new(name))
        .map(|(_, value)| value.unwrap_or_default().to_owned())
}

/// A service command built exactly as `serve.rs` builds one, so what this file
/// asserts is what those tests send.
fn serve_command(state: &Path, repo: &Path, home: &Path) -> Command {
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
    command
}

/// **Half one.** The environment a spawned service is given has no host in it:
/// the socket cannot exist, the session is nobody's, and every `HERDR_*` name
/// the test process itself holds is removed rather than inherited.
#[test]
fn a_spawned_service_cannot_see_the_socket_that_started_the_test() {
    let (repo, state, home) = fixture();
    let command = serve_command(state.path(), repo.path(), home.path());

    // The socket it gets is one nothing creates, inside the test's own folder.
    let socket = value(&command, "HERDR_SOCKET_PATH").expect("the socket is set, not left out");
    assert_eq!(
        PathBuf::from(&socket),
        absent_socket(state.path()),
        "a child that tries to reach a multiplexer must find none"
    );
    assert!(
        Path::new(&socket).starts_with(state.path()),
        "and the absent socket is inside the test's own folder: {}",
        Path::new(&socket).display()
    );

    // The session is nobody's. An unset `HC_SESSION` is the *default* session,
    // which is the person's, so leaving it out is the leak.
    assert_eq!(
        value(&command, "HC_SESSION").as_deref(),
        Some(std::ffi::OsStr::new(PRIVATE_HC_SESSION))
    );

    // Every host variable this process actually holds is removed by name. If the
    // test is not inside herdr there is nothing inherited to remove; the
    // assertions above still prove the rule, and the note says which run this
    // was.
    let mut removed = 0;
    for (name, _) in std::env::vars_os() {
        let Some(name) = name.to_str().map(str::to_string) else {
            continue;
        };
        if !support::herdr::is_a_host_variable(&name) {
            continue;
        }
        if matches!(
            name.as_str(),
            "HERDR_SOCKET_PATH" | "HC_SESSION" | "HERDR_APP_HOME"
        ) {
            continue;
        }
        assert_eq!(
            value(&command, &name),
            Some(std::ffi::OsString::new()),
            "{name} must be removed from the child, not inherited"
        );
        removed += 1;
    }
    if removed == 0 {
        eprintln!("not inside herdr: no inherited host variable to remove in this run");
    }
}

/// **Half one, the other way round.** A test that starts the service and then
/// overrides the socket — the live tests, which point at a `herdr server` they
/// started themselves — keeps every other part of the rule. This is the shape
/// `chat_live.rs`, `spawn_live.rs` and `opencode_live.rs` use.
#[test]
fn a_live_test_that_names_its_own_server_loses_the_rest_of_the_rule() {
    let (repo, state, home) = fixture();
    let mut command = serve_command(state.path(), repo.path(), home.path());
    // Exactly what a live test does after `isolate`: name its own private server.
    let private = state.path().join("private.sock");
    command.env("HERDR_SOCKET_PATH", &private);

    assert_eq!(
        value(&command, "HERDR_SOCKET_PATH").as_deref(),
        Some(private.as_os_str()),
        "a live test's own server is the one the child reaches"
    );
    assert_eq!(
        value(&command, "HC_SESSION").as_deref(),
        Some(std::ffi::OsStr::new(PRIVATE_HC_SESSION)),
        "and the desktop's session is still one nobody runs"
    );
}

/// **Half two.** The service refuses the leak by itself, when a person or a
/// worker starts it by hand.
///
/// The checkout is under the temporary directory and the socket named in the
/// environment is outside it — the shape of both incidents. The service must
/// still serve (the API is a board to read), and must say in its banner that it
/// did not touch the host.
#[test]
fn a_service_started_by_hand_from_a_temporary_checkout_refuses_the_host() {
    let (repo, state, home) = fixture();
    // A run, so the banner has something to name and the start is a real one.
    let mut start = Command::new(BIN);
    start
        .args([
            "--repo",
            &repo.path().display().to_string(),
            "--home",
            &home.path().display().to_string(),
            "run",
            "start",
            "--run-id",
            "guard",
            "--base",
            "main",
            "--integration",
            "integration/m11",
        ])
        .env("XDG_STATE_HOME", state.path())
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .env("XDG_DATA_HOME", home.path().join(".local").join("share"));
    isolate(&mut start, state.path());
    let started = start.output().expect("run start runs");
    assert!(
        started.status.success(),
        "run start failed: {}",
        String::from_utf8_lossy(&started.stderr)
    );

    // The leak, deliberately: the guard is what must stop it. This test does not
    // apply `isolate`, because it is the one about what happens without it.
    //
    // The socket has to be *outside* the temporary directory for the guard to see
    // the disagreement at all, and it is a path that does not exist, so even a
    // service that ignored the guard could reach nothing. `/nonexistent` is the
    // path the incidents' own recipe uses for exactly this reason.
    let outside = PathBuf::from("/nonexistent-outside-the-temp-dir/herdr.sock");
    assert!(
        !outside.starts_with(std::env::temp_dir()),
        "the fixture's socket must be outside the temp dir for the guard to fire: {}",
        outside.display()
    );
    let out_path = state.path().join("serve.out");
    let err_path = state.path().join("serve.err");
    let mut command = Command::new(BIN);
    command
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
        .env_remove("SHPRD_ALLOW_DEFAULT_SOCKET")
        .stdout(Stdio::from(
            fs::File::create(&out_path).expect("the stdout file"),
        ))
        .stderr(Stdio::from(
            fs::File::create(&err_path).expect("the stderr file"),
        ));
    // Every host variable removed first; only the socket is then put outside the
    // temp dir, which is exactly what makes the guard fire.
    isolate(&mut command, state.path());
    command.env("HERDR_SOCKET_PATH", &outside);
    let mut child = command.spawn().expect("the service starts");

    // The banner is one JSON document, written after the record.
    let banner = wait_for(&out_path, "\"serving\"");
    let value: serde_json::Value =
        serde_json::from_str(&banner).expect("the banner is one JSON document");

    let _ = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status();
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if child.try_wait().ok().flatten().is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();

    assert_eq!(
        value["space"]["state"], "withheld",
        "the service did not touch the host: {value}"
    );
    assert_eq!(
        value["space"]["label"],
        serde_json::Value::Null,
        "and names no label it never wanted: {value}"
    );
    let reason = value["space"]["reason"].as_str().expect("a reason");
    assert!(
        reason.contains(&repo.path().display().to_string()),
        "the reason names the checkout: {reason}"
    );
    assert!(
        reason.contains(&outside.display().to_string()),
        "and the socket it would have driven: {reason}"
    );
    assert!(
        reason.contains("SHPRD_ALLOW_DEFAULT_SOCKET"),
        "and the way out for a person whose checkout really is under /tmp: {reason}"
    );

    // The service served anyway, which is what keeps the board readable.
    let served = fs::read_to_string(&out_path).unwrap_or_default();
    assert!(
        served.contains("127.0.0.1"),
        "the API is still up: {served}"
    );
    // And the same news went to the log, for the person reading a supervisor's
    // file rather than the banner.
    let warned = fs::read_to_string(&err_path).unwrap_or_default();
    assert!(
        warned.contains("not ready") || warned.contains("withheld"),
        "the refusal is logged as well as printed: {warned}"
    );
}

/// Reads `path` until it holds `wanted`, or gives up with what it has.
fn wait_for(path: &Path, wanted: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let text = fs::read_to_string(path).unwrap_or_default();
        if text.contains(wanted) || Instant::now() > deadline {
            return text;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
