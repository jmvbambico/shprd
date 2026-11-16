//! One live spawn against a private herdr server.
//!
//! `#[ignore]` because it starts a real `herdr server`, which the unit gate must
//! never do. Run it by hand with
//!
//! ```text
//! cargo test -p shprd --test spawn_live -- --ignored
//! ```
//!
//! Nothing real is reachable from here: `HOME` and `HERDR_SOCKET_PATH` point into
//! a fresh temporary directory, `SHPRD_HOME` and `SHPRD_DATA_DIR` too, and the
//! only agent CLI the run can pick is a fake `sh` spec written by this test.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::process::{Command, Output};

#[path = "../../shprd-mux-herdr/tests/support/live_server.rs"]
mod live_server;

use live_server::LiveServer;
// The live-server helper already carries the isolation rule, and re-exporting
// it from there rather than including `herdr_env.rs` a second time keeps one
// module per rule: a test crate that loaded it twice would not compile.
use live_server::herdr_env::isolate;

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

#[test]
#[ignore = "starts a real herdr server; run by hand"]
fn spawn_opens_a_worker_tab_in_the_worktree() {
    // A skip is the machine's answer, not a failure: no `herdr`, or a `TMPDIR`
    // too long for a unix socket path. The helper says which, in one line.
    let Some(server) = LiveServer::start() else {
        return;
    };

    // A repository with one commit and the integration branch in it.
    let repo = server.home().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "live\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "first"]);
    git(&repo, &["branch", "integration/m4"]);

    // A data directory whose only agent CLI is `sh`, so no real CLI starts.
    let data = server.home().join("data");
    std::fs::create_dir_all(data.join("harness")).unwrap();
    std::fs::write(
        data.join("harness").join("sh.toml"),
        r#"id = "sh"
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
"#,
    )
    .unwrap();

    let home = server.home().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let shprd = |args: &[&str]| -> Output {
        let mut command = Command::new(BIN);
        // The rule first, then this test's own private server: a live test is
        // the one place a child may reach a multiplexer, and it is the one this
        // test started in a temp directory, never the person's.
        isolate(&mut command, &home);
        command
            .args(args)
            .env("HOME", &home)
            .env("SHPRD_HOME", &home)
            .env("SHPRD_DATA_DIR", &data)
            .env("HERDR_SOCKET_PATH", server.socket())
            // A plain `sh` is never detected as an agent, so the wait is short
            // and the delivery is expected to fail its way into a warning.
            .env("SHPRD_AGENT_READY_TIMEOUT_MS", "2000")
            .env_remove("GH_TOKEN")
            .output()
            .expect("the shprd binary must run")
    };
    let ok = |args: &[&str]| -> String {
        let output = shprd(args);
        assert!(
            output.status.success(),
            "{args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).to_string()
    };
    let repo_arg = repo.display().to_string();
    ok(&[
        "--repo",
        &repo_arg,
        "run",
        "start",
        "--run-id",
        "m4",
        "--base",
        "main",
        "--integration",
        "integration/m4",
    ]);
    ok(&[
        "--repo",
        &repo_arg,
        "agent",
        "task",
        "add",
        "--id",
        "M4.4",
        "--title",
        "Make spawn real",
        "--scope",
        "crates/shprd",
        "--acceptance",
        "spawn opens a tab",
    ]);
    // The workspace shprd looks for, created by hand here.
    let name = format!(
        "shprd \u{b7} {}",
        repo.file_name().unwrap().to_string_lossy()
    );
    server
        .herdr(&[
            "workspace",
            "create",
            "--label",
            &name,
            "--cwd",
            &repo_arg,
            "--no-focus",
        ])
        .expect("the test must be able to create its own workspace");

    let spawned = shprd(&[
        "--repo", &repo_arg, "agent", "spawn", "--task", "M4.4", "--agent", "sh",
    ]);
    let out = String::from_utf8_lossy(&spawned.stdout).to_string();
    let warning = String::from_utf8_lossy(&spawned.stderr).to_string();
    assert!(spawned.status.success(), "spawn failed: {warning}");
    assert!(out.contains("ok spawned M4.4 attempt 1"), "{out}");
    // The spec has no system-prompt flag, so the brief is typed in, and herdr
    // only prompts a pane it has detected as an agent. Nobody detected this
    // shell, so the spawn says so and stays successful.
    assert!(
        warning.contains("brief not delivered to sh\u{b7}M4.4"),
        "no warning in {warning}"
    );
    assert!(warning.contains("brief not delivered"), "{warning}");
    assert!(
        warning.contains("--task M4.4"),
        "the warning must say how to resend: {warning}"
    );

    // The tab exists, titled for the worker, and its title names the task.
    let tabs = ok(&["--repo", &repo_arg, "state", "--json"]);
    assert!(tabs.contains("M4.4"), "{tabs}");
    let snapshot =
        String::from_utf8_lossy(&server.herdr(&["api", "snapshot"]).unwrap().stdout).to_string();
    assert!(
        snapshot.contains("sh\u{b7}M4.4"),
        "no worker tab in {snapshot}"
    );
    assert!(
        snapshot.contains(".shprd/wt"),
        "the tab's pane is not in a worktree: {snapshot}"
    );

    // The brief the human is told to resend says what "done" means.
    let brief = home
        .join(".shprd")
        .join("briefs")
        .join("m4")
        .join("M4.4-a1.md");
    let text = std::fs::read_to_string(&brief).unwrap_or_else(|err| panic!("{err}"));
    assert!(text.contains("spawn opens a tab"), "{text}");
    assert!(text.contains("shprd/m4/M4.4"), "{text}");
}
