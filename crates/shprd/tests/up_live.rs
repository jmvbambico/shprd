//! One live `up` against a private herdr server.
//!
//! `#[ignore]` because it starts a real `herdr server`, which the unit gate must
//! never do. Run it by hand with
//!
//! ```text
//! cargo test -p shprd --test up_live -- --ignored
//! ```
//!
//! It covers the one case the unit tests cannot: the orchestrator's CLI has no
//! system-prompt flag, so its brief has to be typed in, and herdr only prompts
//! a pane it has detected as an agent. A plain `sh` is never detected, so `up`
//! must still succeed and say the brief did not land.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};
use std::time::{Duration, Instant};

/// A private `herdr server` that stops itself when the test ends.
struct Server {
    home: PathBuf,
    socket: PathBuf,
    child: Option<Child>,
}

impl Server {
    /// Starts a server in a short temporary path, waiting until it answers.
    fn start() -> Server {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let home = std::env::temp_dir()
            .join("shprd-up")
            .join(format!("{}-{stamp}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        let socket = home.join("herdr.sock");
        let child = Command::new("herdr")
            .arg("server")
            .env("HOME", &home)
            .env("HERDR_SOCKET_PATH", &socket)
            .stdout(std::process::Stdio::null())
            .stderr(std::fs::File::create(home.join("server.log")).unwrap())
            .spawn()
            .expect("herdr must be installed to run this test");
        let server = Server {
            home,
            socket,
            child: Some(child),
        };
        let started = Instant::now();
        loop {
            match server.herdr(&["workspace", "list"]) {
                Ok(_) => break,
                Err(err) => assert!(
                    started.elapsed() < Duration::from_secs(20),
                    "herdr server did not answer on {}: {err}",
                    server.socket.display()
                ),
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        server
    }

    /// Runs one `herdr` command against this server.
    fn herdr(&self, args: &[&str]) -> Result<Output, String> {
        let output = Command::new("herdr")
            .args(args)
            .env("HOME", &self.home)
            .env("HERDR_SOCKET_PATH", &self.socket)
            .output()
            .map_err(|err| err.to_string())?;
        if output.status.success() {
            Ok(output)
        } else {
            Err(String::from_utf8_lossy(&output.stderr).to_string())
        }
    }

    /// Everything the host knows, as one string.
    fn snapshot(&self) -> String {
        let output = self.herdr(&["api", "snapshot"]).unwrap();
        String::from_utf8_lossy(&output.stdout).to_string()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = Command::new("herdr")
                .args(["server", "stop"])
                .env("HOME", &self.home)
                .env("HERDR_SOCKET_PATH", &self.socket)
                .status();
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

/// Runs one `git` command in `dir`, failing the test when it does.
fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .expect("git must be installed");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The binary under test, built by cargo for this integration test.
const BIN: &str = env!("CARGO_BIN_EXE_shprd");

/// A harness spec for a CLI with no system-prompt flag, so the brief is typed in.
const SH_SPEC: &str = r#"id = "sh"
name = "Shell"
vendor = "openai"
verified_version = "1"
dry_patterns = []
[detect]
binary = "sh"
version_args = []
signin_hint = ""
signin_args = []
[launch]
interactive = ["sh"]
model_args = ["-m", "{model}"]
verified = false
[models]
[records]
kind = "jsonl"
location = "~/nowhere.jsonl"
errors = "none"
verified = true
[quota]
source = "none"
[skills]
home_dirs = []
project_dirs = []
"#;

#[test]
#[ignore = "starts a real herdr server; run by hand"]
fn up_warns_when_the_orchestrator_is_never_detected() {
    let server = Server::start();

    // A repository with one commit on the branch `up` cuts the run from.
    let repo = server.home.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "live\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "first"]);

    // The only agent CLI the run can pick is the fake `sh` spec.
    let data = server.home.join("data");
    std::fs::create_dir_all(data.join("harness")).unwrap();
    std::fs::write(data.join("harness").join("sh.toml"), SH_SPEC).unwrap();

    // A home whose global config names that CLI as the orchestrator.
    let home = server.home.join("home");
    let config = home.join(".config").join("shprd");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("config.toml"),
        "[roster]\norchestrator = \"sh\"\n",
    )
    .unwrap();

    let shprd = |args: &[&str]| -> Output {
        Command::new(BIN)
            .args(args)
            .env("HOME", &home)
            .env("SHPRD_HOME", &home)
            .env("SHPRD_DATA_DIR", &data)
            .env("HERDR_SOCKET_PATH", &server.socket)
            // herdr will never call a plain `sh` an agent, so the wait is short.
            .env("SHPRD_AGENT_READY_TIMEOUT_MS", "2000")
            .env_remove("GH_TOKEN")
            .output()
            .expect("the shprd binary must run")
    };
    let repo_arg = repo.display().to_string();

    let first = shprd(&["--repo", &repo_arg, "up"]);
    let warning = String::from_utf8_lossy(&first.stderr).to_string();
    assert!(first.status.success(), "up failed: {warning}");
    assert!(
        warning.contains("brief not delivered to \u{2605} sh\u{b7}orchestrator"),
        "no warning in {warning}"
    );
    assert!(
        warning.contains("orchestrator.md"),
        "the warning must name the brief file: {warning}"
    );

    // Both tabs are there, and the orchestrator's pane is rooted in the repo.
    let snapshot = server.snapshot();
    assert!(snapshot.contains("shprd\u{b7}chat"), "{snapshot}");
    assert!(
        snapshot.contains("\u{2605} sh\u{b7}orchestrator"),
        "{snapshot}"
    );
    assert!(
        snapshot.contains(&repo_arg),
        "the orchestrator's pane is not in the repo: {snapshot}"
    );

    // A second `up` resumes: still green, still no extra tab.
    let second = shprd(&["--repo", &repo_arg, "up"]);
    let again = String::from_utf8_lossy(&second.stderr).to_string();
    assert!(second.status.success(), "second up failed: {again}");
    let after = server.snapshot();
    assert_eq!(
        after.matches("shprd\u{b7}chat").count(),
        1,
        "second up added a chat tab: {after}"
    );
}
