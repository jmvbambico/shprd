//! One live opencode worker, handed its brief with nobody typing.
//!
//! This is the M10 gate's bug 3, end to end: on herdr-core 0.1.21 a program
//! whose only status source is the plugin it installs never reports, so shprd
//! used to wait out its ready timeout and report a brief it never sent. Here a
//! real opencode is started through `shprd agent spawn`, and the only thing that
//! delivers the brief is the ready line its harness names — `Ask anything…` on
//! the bottom of its pane. A file the brief asks for appearing in the task's
//! worktree is the proof that the agent read it and started working.
//!
//! `#[ignore]` because it starts a real `herdr server`, a real `opencode`, and
//! costs one model turn. Run it by hand with
//!
//! ```text
//! cargo test -p shprd --test opencode_live -- --ignored --nocapture
//! ```
//!
//! Nothing of the person's is reachable from here: `HOME` and
//! `HERDR_SOCKET_PATH` point into a fresh temporary directory (so the private
//! `herdr server` and the opencode it starts both live there), `SHPRD_HOME`,
//! `SHPRD_DATA_DIR` and every XDG variable too, and the opencode credential the
//! turn needs is copied into that home by this test and removed with it. The
//! person's own `~/.config/opencode`, `~/.local/share/opencode` and herdr
//! session are never addressed; the only thing read from their home is the
//! `auth.json` the copied credential comes from, which is read-only.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

#[path = "../../shprd-mux-herdr/tests/support/live_server.rs"]
mod live_server;

use live_server::LiveServer;
use live_server::herdr_env::isolate;

/// The binary under test, built by cargo for this integration test.
const BIN: &str = env!("CARGO_BIN_EXE_shprd");

/// How long the agent gets to write the file the brief asks for.
///
/// A free OpenCode Zen model answers a two-line brief in about twenty seconds
/// (measured on this machine); the margin covers a cold start of the TUI and a
/// queued provider turn.
const AGENT_DEADLINE: Duration = Duration::from_secs(240);

/// The file the brief asks the agent to write, relative to its worktree.
const WANTED: &str = "notes/opencode-live.txt";

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

/// A file holding the person's opencode credential, if this machine has one.
///
/// The credential is the account's, not shprd's: it is read to let a real turn
/// happen, and written into the temporary home so the live agent can sign in.
/// `None` means this machine has no credential and the test skips rather than
/// failing, because an unsigned-in CLI is the account's state and not a bug.
fn credential(home: &Path) -> Option<PathBuf> {
    let found = home
        .join(".local")
        .join("share")
        .join("opencode")
        .join("auth.json");
    found.is_file().then_some(found)
}

#[test]
#[ignore = "starts a real herdr server and a real opencode turn; run by hand"]
fn an_opencode_worker_receives_its_brief_with_nobody_typing() {
    // A skip is the machine's answer, not a failure: no `herdr`, no `opencode`,
    // no credential, or a `TMPDIR` too long for a unix socket path.
    let Some(server) = LiveServer::start() else {
        return;
    };
    let Some(opencode) = shprd_agents::detect::find_binary("opencode", &path_env()) else {
        eprintln!("skipped: opencode is not installed");
        return;
    };
    let Some(auth) = credential(&real_home()) else {
        eprintln!("skipped: this machine has no ~/.local/share/opencode/auth.json");
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

    // The home the agent's own state lives in, with the credential copied in:
    // the CLI is given this as HOME by the launch, so its session records land
    // here and never in the person's.
    let home = server.home().join("home");
    let opencode_data = home.join(".local").join("share").join("opencode");
    std::fs::create_dir_all(&opencode_data).unwrap();
    std::fs::copy(&auth, opencode_data.join("auth.json")).unwrap();

    let shprd = |args: &[&str]| -> Output {
        let mut command = Command::new(BIN);
        // The isolation rule first, then this test's own private server: a live
        // test is the one place a child may reach a multiplexer, and it is the
        // one this test started in a temp directory, never the person's.
        isolate(&mut command, &home);
        command
            .args(args)
            .env("HOME", &home)
            .env("SHPRD_HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_DATA_HOME", home.join(".local").join("share"))
            .env("XDG_STATE_HOME", home.join(".local").join("state"))
            .env("XDG_CACHE_HOME", home.join(".cache"))
            .env("HERDR_SOCKET_PATH", server.socket())
            // The real ready wait: the whole point is that the ready line lands
            // inside it rather than the wait running out.
            .env("SHPRD_AGENT_READY_TIMEOUT_MS", "120000")
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
        "Write the note an opencode worker is told to write",
        "--scope",
        "notes",
        "--acceptance",
        "the file notes/opencode-live.txt exists in the worktree and holds the line: opencode live",
    ]);
    // The workspace shprd looks for, created by hand here: `agent spawn` runs
    // outside the service, which is what prepares it for a person.
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

    // Free models cost nothing, and the model is one `opencode models` lists.
    let agent = format!("opencode/{}", free_model(&opencode));
    let spawned = shprd(&[
        "--repo", &repo_arg, "agent", "spawn", "--task", "M4.4", "--agent", &agent,
    ]);
    let stdout = String::from_utf8_lossy(&spawned.stdout).to_string();
    let stderr = String::from_utf8_lossy(&spawned.stderr).to_string();
    assert!(spawned.status.success(), "spawn failed: {stderr}");
    assert!(stdout.contains("ok spawned M4.4 attempt 1"), "{stdout}");
    // The bug this test exists for: the delivery must not be a warning.
    assert!(
        !stderr.contains("brief not delivered"),
        "the brief never reached opencode with nobody typing: {stderr}"
    );

    // The spawn record says how it was ready, and which of the two it is
    // depends on the host, not on the program: the herdr TUI detects agents
    // itself, so it reports opencode's state; a herdr-core without
    // `pane.report_agent` cannot, and the ready line is what delivers there
    // (`tests/opencode_desktop_live.rs` is the one that proves that half).
    // Either way the brief must not be a warning, which is asserted above.
    let record_path = home
        .join(".shprd")
        .join("briefs")
        .join("m4")
        .join("M4.4-a1.spawn");
    let record = std::fs::read_to_string(&record_path)
        .unwrap_or_else(|err| panic!("no spawn record at {}: {err}", record_path.display()));
    assert!(
        record.contains("ready=host-status") || record.contains("ready=ready-line"),
        "the spawn record must say what said the program was ready: {record}"
    );
    assert!(
        record.contains("agent=opencode"),
        "the spawn record should name the program: {record}"
    );
    assert!(
        record.contains("status=hook, screen"),
        "opencode claims no OSC 7501 report: {record}"
    );

    // And the agent is doing the work: the file the brief asks for appears in
    // the task's worktree, with nobody having typed anything.
    let worktree = home
        .join(".shprd")
        .join("wt")
        .join("repo")
        .join("m4")
        .join("M4.4");
    let wanted = worktree.join(WANTED);
    let started = Instant::now();
    while started.elapsed() < AGENT_DEADLINE {
        if wanted.is_file() {
            break;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    assert!(
        wanted.is_file(),
        "opencode never wrote {} within {:?}; its pane said: {}",
        wanted.display(),
        AGENT_DEADLINE,
        pane_tail(&home, &server)
    );
    let written = std::fs::read_to_string(&wanted).unwrap_or_default();
    assert!(
        written.contains("opencode live"),
        "the file is not what the brief asked for: {written:?}"
    );

    // The task space is left for the test's own teardown: the service is what
    // closes it on a landing, and no landing runs here.
}

/// The first free model `opencode models` lists, or the fallback the gate used.
///
/// A free model is the whole reason this test can run unattended: a paid one
/// would make a real charge on a person's account for a test.
fn free_model(opencode: &Path) -> String {
    let output = Command::new(opencode).arg("models").output();
    if let Ok(output) = output
        && output.status.success()
    {
        let text = String::from_utf8_lossy(&output.stdout);
        if let Some(model) = text
            .lines()
            .map(str::trim)
            .find(|line| line.ends_with("-free") && !line.is_empty())
        {
            return model.to_string();
        }
    }
    "deepseek-v4.1-flash-free".to_string()
}

/// What the worker's pane holds, for a failure message a person can read.
///
/// The tab's id is in the record the spawn wrote, and `herdr agent read` takes
/// one: a pane of a program herdr never detected as an agent is still readable,
/// which is exactly the case this test cares about.
fn pane_tail(home: &Path, server: &LiveServer) -> String {
    let record = home
        .join(".shprd")
        .join("briefs")
        .join("m4")
        .join("M4.4-a1.tab");
    let Ok(tab) = std::fs::read_to_string(&record) else {
        return format!("(no tab record at {})", record.display());
    };
    let tab = tab.trim().to_string();
    match server.herdr(&["agent", "read", &tab, "--lines", "40"]) {
        Ok(output) => String::from_utf8_lossy(&output.stdout).to_string(),
        Err(error) => format!("(the pane could not be read: {error})"),
    }
}

/// The person's real home, read only to find the credential this test copies.
fn real_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// The `PATH` the process was started with.
fn path_env() -> String {
    std::env::var("PATH").unwrap_or_default()
}
