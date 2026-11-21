//! End-to-end tests for `shprd web`: the password typed at a terminal, and the
//! sessions it ends.
//!
//! Each test gets its own home, so the password and the session list it writes
//! are never the developer's. `stdin` is a pipe, which is the second way in the
//! verb accepts a password (a terminal gets `stty -echo`; a pipe gets the two
//! lines, and is how a script answers).
//!
//! A session is created through `shprd_remote` directly: the only way one exists
//! in real life is a browser login, and this is the CLI half of it — what the
//! person sees and ends.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::Arc;

use serde_json::Value;
use shprd_core::log::SystemClock;
use shprd_remote::Sessions;
use tempfile::TempDir;

mod support;

use support::herdr::isolate;

/// The binary under test, built by cargo for this integration test.
const BIN: &str = env!("CARGO_BIN_EXE_shprd");

/// A password that passes every rule.
const PASSWORD: &str = "a long enough password";

/// A temp home, with the binary run against it.
struct Fixture {
    home: TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self {
            home: TempDir::new().expect("a temp home"),
        }
    }

    /// The state directory the service and these verbs share.
    fn state(&self) -> PathBuf {
        shprd_server::state_dir(&self.home.path().join(".local/state"))
    }

    /// Runs `shprd web <args>` with `stdin` as the given lines.
    fn run(&self, args: &[&str], stdin: &str) -> Output {
        let mut command = Command::new(BIN);
        isolate(&mut command, self.home.path());
        command
            .args(args)
            .arg("--home")
            .arg(self.home.path())
            .env("HOME", self.home.path())
            .env("SHPRD_HOME", self.home.path())
            .env("SHPRD_REPO", self.home.path())
            .env_remove("XDG_STATE_HOME")
            .env_remove("XDG_CONFIG_HOME")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().expect("the binary runs");
        child
            .stdin
            .as_mut()
            .expect("a piped stdin")
            .write_all(stdin.as_bytes())
            .expect("the password is written");
        child.wait_with_output().expect("the binary answers")
    }

    /// Starts one session, the way a browser login would, and returns its id.
    fn session(&self, remember: bool) -> String {
        std::fs::create_dir_all(self.state()).expect("the state dir");
        let sessions = Sessions::new(&self.state(), Arc::new(SystemClock));
        sessions
            .create(
                Some(
                    "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) \
                 AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1",
                ),
                remember,
            )
            .expect("a session")
            .0
    }

    /// The sessions the service would find.
    fn sessions(&self) -> Vec<shprd_remote::SessionRecord> {
        Sessions::new(&self.state(), Arc::new(SystemClock))
            .list()
            .expect("a list")
    }
}

/// The stdout of an output, which must have succeeded.
fn ok(output: &Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

/// The stdout and stderr of an output that must have been a refusal (exit 2).
fn refused(output: &Output) -> String {
    assert_eq!(
        output.status.code(),
        Some(2),
        "a refusal exits 2: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stderr).to_string()
}

#[test]
fn a_password_is_typed_twice_and_stored_private() {
    let fixture = Fixture::new();
    let output = fixture.run(
        &["web", "password", "set"],
        &format!("{PASSWORD}\n{PASSWORD}\n"),
    );
    let text = ok(&output);
    assert!(text.contains("is set"), "{text}");

    let path = fixture.state().join("web-auth.json");
    let text = std::fs::read_to_string(&path).expect("the password file");
    assert!(text.contains("$argon2id$v=19$m=65536,t=3,p=1$"), "{text}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path)
            .expect("the file")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "the file is private");
        let dir = std::fs::metadata(fixture.state())
            .expect("the dir")
            .permissions()
            .mode();
        assert_eq!(dir & 0o777, 0o700, "the directory is private");
    }
}

#[test]
fn the_two_entries_must_match_and_nothing_is_written_when_they_do_not() {
    let fixture = Fixture::new();
    let output = fixture.run(
        &["web", "password", "set"],
        "a long enough password\na shorter one\n",
    );
    let text = refused(&output);
    assert!(text.contains("differ"), "{text}");
    assert!(!fixture.state().join("web-auth.json").exists());
}

#[test]
fn a_short_password_and_a_common_one_are_refused() {
    let fixture = Fixture::new();
    let short = refused(&fixture.run(&["web", "password", "set"], "elevenchars\nelevenchars\n"));
    assert!(short.contains("12 characters"), "{short}");

    let common = refused(&fixture.run(&["web", "password", "set"], "Password1234\nPassword1234\n"));
    assert!(common.contains("commonest"), "{common}");
    assert!(!fixture.state().join("web-auth.json").exists());
}

#[test]
fn setting_a_password_ends_every_session_and_so_does_clearing_it() {
    let fixture = Fixture::new();
    fixture.session(false);
    fixture.session(true);
    assert_eq!(fixture.sessions().len(), 2);

    let output = fixture.run(
        &["web", "password", "set"],
        &format!("{PASSWORD}\n{PASSWORD}\n"),
    );
    let text = ok(&output);
    assert!(text.contains("2 sessions ended"), "{text}");
    assert!(fixture.sessions().is_empty(), "a change revokes them all");

    fixture.session(false);
    assert_eq!(fixture.sessions().len(), 1);
    let cleared = ok(&fixture.run(&["web", "password", "clear"], ""));
    assert!(cleared.contains("cleared"), "{cleared}");
    assert!(!fixture.state().join("web-auth.json").exists());
    assert!(fixture.sessions().is_empty());
}

#[test]
fn sessions_lists_the_short_ids_and_revokes_by_one() {
    let fixture = Fixture::new();
    let phone = fixture.session(false);
    fixture.session(true);

    let listed = ok(&fixture.run(&["web", "sessions", "--json"], ""));
    let json: Value = serde_json::from_str(&listed).expect("json");
    let rows = json["sessions"].as_array().expect("rows");
    assert_eq!(rows.len(), 2);
    for row in rows {
        let id = row["id"].as_str().expect("an id");
        assert_eq!(id.len(), 12, "a short hash, not the cookie's id: {id}");
        assert!(!id.contains(&phone), "the raw id is never printed");
    }
    let first = rows[0]["id"].as_str().expect("an id").to_string();
    assert_eq!(rows[0]["label"], "phone · safari");

    let revoked = ok(&fixture.run(&["web", "sessions", "--revoke", &first], ""));
    assert!(revoked.contains("1 session ended"), "{revoked}");
    assert_eq!(fixture.sessions().len(), 1);

    let all = ok(&fixture.run(&["web", "sessions", "--revoke-all"], ""));
    assert!(all.contains("1 session ended"), "{all}");
    assert!(fixture.sessions().is_empty());

    let none = ok(&fixture.run(&["web", "sessions"], ""));
    assert!(none.contains("no sessions"), "{none}");
}

#[test]
fn an_id_that_names_no_session_is_a_refusal() {
    let fixture = Fixture::new();
    fixture.session(false);
    let text = refused(&fixture.run(&["web", "sessions", "--revoke", "does-not-exist"], ""));
    assert!(text.contains("no session starts with"), "{text}");
    assert_eq!(fixture.sessions().len(), 1, "nothing was revoked");
}

#[test]
fn both_revoke_flags_at_once_is_a_usage_error() {
    let fixture = Fixture::new();
    let output = fixture.run(&["web", "sessions", "--revoke", "abc", "--revoke-all"], "");
    assert!(!output.status.success());
    let text = String::from_utf8_lossy(&output.stderr);
    assert!(text.contains("cannot be used with"), "{text}");
}

#[test]
fn the_password_lives_in_the_state_directory_the_service_reads() {
    let fixture = Fixture::new();
    ok(&fixture.run(
        &["web", "password", "set"],
        &format!("{PASSWORD}\n{PASSWORD}\n"),
    ));
    let state = fixture.state();
    assert_eq!(
        state,
        shprd_server::state_dir(&fixture.home.path().join(".local/state")),
        "the same directory the one service's discovery files live in"
    );
    assert!(state.join("web-auth.json").is_file());
}
