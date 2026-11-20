//! End-to-end tests for `shprd web up | down | status`.
//!
//! A real service runs in this process, started the way `shprd serve` starts it
//! but with a fake `cloudflared` on its `PATH`, and the real binary talks to it
//! over the loopback door. Nothing here reaches a network or a multiplexer, and
//! every path is under a temp home.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value;
use shprd_remote::PasswordStore;
use shprd_server::{Config, Service};
use tempfile::TempDir;

mod support;

use support::herdr::isolate;

const BIN: &str = env!("CARGO_BIN_EXE_shprd");

/// A fake `cloudflared`: prints its version, or a quick-tunnel banner and waits.
const FAKE: &str = "#!/bin/sh\n\
    if [ \"$1\" = \"--version\" ]; then echo 'cloudflared version 0.0.0-fake'; exit 0; fi\n\
    echo $$ > \"$FAKE_PID_FILE\"\n\
    echo 'INF |  https://fake-quiet-river-words.trycloudflare.com  |' >&2\n\
    exec sleep 120\n";

struct Fixture {
    home: TempDir,
    bin: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let bin = TempDir::new().unwrap();
        let path = bin.path().join("cloudflared");
        // The fake gets its pid file from its own directory, so the binary under
        // test needs no environment to pass it on.
        let script = FAKE.replace(
            "\"$FAKE_PID_FILE\"",
            &format!("\"{}\"", bin.path().join("pid").display()),
        );
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            home: TempDir::new().unwrap(),
            bin,
        }
    }

    fn state(&self) -> PathBuf {
        shprd_server::repo_state_dir(&self.home.path().join(".local/state"), self.home.path())
    }

    fn set_password(&self) {
        std::fs::create_dir_all(self.state()).unwrap();
        PasswordStore::new(&self.state())
            .set_with_cost("a long enough password", 8, 1, 1)
            .unwrap();
    }

    /// Starts a service over the fixture's state directory.
    async fn service(&self) -> tokio::sync::oneshot::Sender<()> {
        std::fs::create_dir_all(self.state()).unwrap();
        let repo = self.home.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let mut config = Config::new(repo, self.state());
        config.path_env = Some(self.bin.path().display().to_string());
        let service = Service::start(config).await.unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            let _ = service
                .run(async {
                    let _ = stopped.await;
                })
                .await;
        });
        stop
    }

    /// Runs `shprd web <args>` on a blocking thread, so the service keeps
    /// answering while the binary waits for it.
    async fn web(&self, args: &[&str]) -> Output {
        let home = self.home.path().to_path_buf();
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        tokio::task::spawn_blocking(move || {
            let mut command = Command::new(BIN);
            isolate(&mut command, &home);
            command
                .arg("web")
                .args(&args)
                .arg("--home")
                .arg(&home)
                .env("HOME", &home)
                .env("SHPRD_HOME", &home)
                .env("SHPRD_REPO", &home)
                .env_remove("XDG_STATE_HOME")
                .env_remove("XDG_CONFIG_HOME")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            command.output().expect("the binary runs")
        })
        .await
        .unwrap()
    }
}

fn out(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn err(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

fn alive(pid: u32) -> bool {
    Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn pid_of(dir: &Path) -> u32 {
    std::fs::read_to_string(dir.join("pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

#[tokio::test]
async fn up_without_a_service_says_to_start_one() {
    let fixture = Fixture::new();
    fixture.set_password();
    let output = fixture.web(&["up", "--tunnel", "cloudflared"]).await;
    assert_eq!(output.status.code(), Some(2), "{}", err(&output));
    assert!(
        err(&output).contains("no shprd service is running"),
        "{}",
        err(&output)
    );
}

#[tokio::test]
async fn a_public_tunnel_is_refused_without_a_password_and_there_is_no_flag_around_it() {
    let fixture = Fixture::new();
    let stop = fixture.service().await;
    let output = fixture.web(&["up", "--tunnel", "cloudflared"]).await;
    assert_eq!(output.status.code(), Some(2), "{}", err(&output));
    assert!(
        err(&output).contains("no login password is set"),
        "{}",
        err(&output)
    );
    assert!(err(&output).contains("shprd web password set"));
    // Nothing was spawned.
    assert!(!fixture.bin.path().join("pid").exists());
    let status = fixture.web(&["status", "--json"]).await;
    let value: Value = serde_json::from_str(&out(&status)).unwrap();
    assert_eq!(value["on"], Value::Bool(false));
    assert!(value["why"].as_str().unwrap().contains("password"));
    // There is no --force.
    let forced = fixture
        .web(&["up", "--tunnel", "cloudflared", "--force"])
        .await;
    assert_ne!(forced.status.code(), Some(0));
    let _ = stop.send(());
}

#[tokio::test]
async fn up_status_and_down_with_the_link_masked_unless_asked_for() {
    let fixture = Fixture::new();
    fixture.set_password();
    let stop = fixture.service().await;

    let up = fixture.web(&["up", "--tunnel", "cloudflared"]).await;
    assert!(up.status.success(), "{}", err(&up));
    let text = out(&up);
    assert!(text.contains("tunnel up: cloudflared"), "{text}");
    assert!(
        text.contains("0.0.0-fake"),
        "the program's version is printed: {text}"
    );
    assert!(text.contains("https://****.trycloudflare.com"), "{text}");
    assert!(
        !text.contains("fake-quiet-river"),
        "the address is masked: {text}"
    );
    let pid = pid_of(fixture.bin.path());
    assert!(alive(pid));

    let status = fixture.web(&["status"]).await;
    assert!(out(&status).contains("tunnel on: cloudflared"));
    assert!(
        !out(&status).contains("fake-quiet-river"),
        "{}",
        out(&status)
    );

    let json = fixture.web(&["status", "--json"]).await;
    let value: Value = serde_json::from_str(&out(&json)).unwrap();
    assert_eq!(value["on"], Value::Bool(true));
    assert_eq!(value["link"], "https://****.trycloudflare.com");
    assert!(value.get("qr_svg").is_none(), "no code without --show-url");

    let shown = fixture.web(&["status", "--show-url"]).await;
    let text = out(&shown);
    assert!(
        text.contains("https://fake-quiet-river-words.trycloudflare.com"),
        "{text}"
    );
    assert!(
        text.lines().count() > 15
            && (text.contains('█') || text.contains('▀') || text.contains('▄')),
        "a QR code of half-blocks follows: {text}"
    );

    // One at a time.
    let second = fixture.web(&["up", "--tunnel", "cloudflared"]).await;
    assert_eq!(second.status.code(), Some(2));
    assert!(err(&second).contains("already"), "{}", err(&second));

    // The record on disk is private and holds the address.
    use std::os::unix::fs::PermissionsExt as _;
    let file = fixture.state().join("web.json");
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(
        std::fs::read_to_string(&file)
            .unwrap()
            .contains("fake-quiet-river")
    );

    let down = fixture.web(&["down"]).await;
    assert!(down.status.success(), "{}", err(&down));
    assert!(!alive(pid), "web down left no child behind");
    let after = fixture.web(&["status"]).await;
    assert!(out(&after).contains("tunnel off"), "{}", out(&after));
    let _ = stop.send(());
}

#[tokio::test]
async fn the_tunnel_dies_with_the_service() {
    let fixture = Fixture::new();
    fixture.set_password();
    let stop = fixture.service().await;
    let up = fixture.web(&["up", "--tunnel", "cloudflared"]).await;
    assert!(up.status.success(), "{}", err(&up));
    let pid = pid_of(fixture.bin.path());
    assert!(alive(pid));
    let _ = stop.send(());
    for _ in 0..100 {
        if !alive(pid) {
            // And the record says so.
            let text = std::fs::read_to_string(fixture.state().join("web.json")).unwrap();
            let value: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(value["on"], Value::Bool(false), "{text}");
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("the tunnel outlived the service");
}

#[tokio::test]
async fn status_never_needs_a_service_and_calls_a_dead_services_record_stale() {
    let fixture = Fixture::new();
    std::fs::create_dir_all(fixture.state()).unwrap();
    let off = fixture.web(&["status"]).await;
    assert!(out(&off).contains("tunnel off"), "{}", out(&off));

    // A record left on by a service that was killed.
    std::fs::write(
        fixture.state().join("web.json"),
        r#"{"on":true,"provider":"cloudflared","link":"https://old-dead-words.trycloudflare.com","at":1}"#,
    )
    .unwrap();
    // ... whose service.json is still there, naming a process that is gone.
    std::fs::write(
        fixture.state().join("service.json"),
        r#"{"port":1,"id":"dead","pid":4194302,"version":"0"}"#,
    )
    .unwrap();
    let stale = fixture.web(&["status", "--json"]).await;
    let value: Value = serde_json::from_str(&out(&stale)).unwrap();
    assert_eq!(value["on"], Value::Bool(false));
    assert_eq!(value["stale"], Value::Bool(true));
    assert!(!out(&stale).contains("old-dead-words"), "{}", out(&stale));
}

#[tokio::test]
async fn an_unknown_provider_is_refused_by_the_parser() {
    let fixture = Fixture::new();
    let output = fixture.web(&["up", "--tunnel", "carrier-pigeon"]).await;
    assert_ne!(output.status.code(), Some(0));
    assert!(err(&output).contains("carrier-pigeon"), "{}", err(&output));
}
