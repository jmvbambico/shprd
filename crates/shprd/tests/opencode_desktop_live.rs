//! One live opencode worker on a private Herdr **desktop**, with nobody typing.
//!
//! This is the other half of the M10 gate's bug 3, and the one that actually
//! reproduces it: herdr's TUI detects an agent itself, but herdr-core 0.1.21 has
//! no `pane.report_agent`, so an opencode plugin that reports through it
//! silently does nothing and the pane is `Unknown` for ever. The `host_can_report`
//! capability is `false` here, `agent_state` stays `Unknown`, and the only thing
//! that can deliver the brief is the ready line its harness names.
//!
//! What it proves, in the order it proves it:
//!
//! 1. the host really cannot carry a plugin's report (`agent_reports` is false),
//!    which is the fact the fallback is chosen by;
//! 2. `shprd agent spawn` starts a terminal for a real opencode in the private
//!    core, hands it the brief through the ready line, and records that;
//! 3. the agent is at work: the file the brief asks for appears in its worktree.
//!
//! `#[ignore]` because it needs `hc`, Herdr.app, `opencode` and a model turn.
//! Run it by hand with
//!
//! ```text
//! cargo test -p shprd --test opencode_desktop_live -- --ignored --nocapture
//! ```
//!
//! Nothing of the person's is reachable: `HC_SESSION`, `HERDR_APP_HOME` and all
//! five `XDG_*` variables point into this test's own temporary folder (M7ef
//! §2.1), the window is one this test starts and stops, `HOME` is the private
//! one the agent's session records go into, and the opencode credential it needs
//! is copied in read-only from the person's and removed with the folder.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../../shprd-mux-hc/tests/support/live.rs"]
mod live;

// The shared isolation rule (M10 fix X), included with `#[path]` the same way
// the live-desktop helper is: this test's private desktop is named *after* the
// rule, so the rule holds for every name it does not mention too.
#[path = "../../shprd-mux-herdr/tests/support/herdr_env.rs"]
mod herdr_env;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use herdr_env::isolate;
use live::Live;
use shprd_mux::Host;

/// The binary under test, built by cargo for this integration test.
const BIN: &str = env!("CARGO_BIN_EXE_shprd");

/// How long the agent gets to write the file the brief asks for.
const AGENT_DEADLINE: Duration = Duration::from_secs(240);

/// The file the brief asks the agent to write, relative to its worktree.
const WANTED: &str = "notes/opencode-live.txt";

#[test]
#[ignore = "needs a real hc, a private Herdr desktop and an opencode turn; run by hand"]
fn an_opencode_worker_on_a_private_desktop_receives_its_brief_with_nobody_typing() {
    // A skip is this machine's answer, not a failure.
    let mut live = match Live::new() {
        Ok(live) => live,
        Err(reason) => {
            eprintln!("skipped: {reason}");
            return;
        }
    };
    if let Err(reason) = live.start_window() {
        eprintln!("skipped: {reason}");
        return;
    }
    let Some(opencode) = shprd_agents::detect::find_binary("opencode", &path_env()) else {
        eprintln!("skipped: opencode is not installed");
        live.stop();
        return;
    };
    let Some(auth) = credential(&real_home()) else {
        eprintln!("skipped: this machine has no ~/.local/share/opencode/auth.json");
        live.stop();
        return;
    };

    // The home the agent's own state lives in, with the credential copied in so
    // a real turn can happen and nothing lands in the person's opencode store.
    let home = live.root().join("home");
    let opencode_data = home.join(".local").join("share").join("opencode");
    std::fs::create_dir_all(&opencode_data).unwrap();
    std::fs::copy(&auth, opencode_data.join("auth.json")).unwrap();

    // A repository with one commit and the integration branch in it.
    let repo = live.root().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "live\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "first"]);
    git(&repo, &["branch", "integration/m4"]);

    let shprd = |args: &[&str]| -> Output {
        let mut command = Command::new(BIN);
        // The rule first: every `HERDR_*`, `HC_*` and `TUIOS_*` name this test
        // inherited is removed, and the socket is one nothing listens on. Then
        // the private desktop's own environment, which is what makes the run
        // reach *this* test's core and window rather than the person's.
        isolate(&mut command, live.root());
        for (key, value) in live.environment() {
            command.env(key, value);
        }
        command
            .args(args)
            .env("HOME", &home)
            .env("SHPRD_HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_DATA_HOME", home.join(".local").join("share"))
            .env("XDG_STATE_HOME", home.join(".local").join("state"))
            .env("XDG_CACHE_HOME", home.join(".cache"))
            .env("SHPRD_AGENT_READY_TIMEOUT_MS", "120000")
            // The host the run must drive is the one this test started.
            .env("SHPRD_HC", live.hc())
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

    // 1. The fact the fallback is chosen by: this core carries no plugin report.
    let endpoint = shprd_mux_hc::Endpoint::with_binary(live.hc().to_string_lossy())
        .with_session(live.session())
        .with_directories(live.directories())
        .with_timeout(Duration::from_secs(30));
    let mut window = shprd_mux_hc::Window::from_endpoint(&endpoint);
    for (key, value) in live.environment() {
        window = window.with_variable(key, value);
    }
    let host = shprd_mux_hc::HcHost::connect(endpoint, window).expect("the private core answers");
    assert!(
        !host.capabilities().agent_reports,
        "this test is for a core without `pane.report_agent`; a newer core should re-enable the \
         plugin path instead"
    );
    assert!(
        host.capabilities().agent_state,
        "the core still reports state; it just cannot hear a program's own report"
    );

    // The workspace shprd looks for, made through the core this test started.
    let name = format!(
        "shprd \u{b7} {}",
        repo.file_name().unwrap().to_string_lossy()
    );
    host.ensure_space(&name, &repo)
        .expect("the private core makes the repository's space");

    // 2. The spawn, with nobody typing.
    let agent = format!("opencode/{}", free_model(&opencode));
    let spawned = shprd(&[
        "--repo", &repo_arg, "agent", "spawn", "--task", "M4.4", "--agent", &agent,
    ]);
    let stdout = String::from_utf8_lossy(&spawned.stdout).to_string();
    let stderr = String::from_utf8_lossy(&spawned.stderr).to_string();
    assert!(spawned.status.success(), "spawn failed: {stderr}");
    assert!(stdout.contains("ok spawned M4.4·a1"), "{stdout}");
    assert!(
        !stderr.contains("brief not delivered"),
        "the brief never reached opencode with nobody typing: {stderr}"
    );

    // The record says the ready line delivered it — on a core that cannot report
    // the program, that is the only thing that could have.
    let record_path = home
        .join(".shprd")
        .join("briefs")
        .join("m4")
        .join("M4.4-a1.spawn");
    let record = std::fs::read_to_string(&record_path)
        .unwrap_or_else(|err| panic!("no spawn record at {}: {err}", record_path.display()));
    assert!(
        record.contains("ready=ready-line"),
        "on a core without `pane.report_agent` the ready line is what delivers: {record}"
    );
    assert!(
        record.contains("agent=opencode"),
        "the spawn record should name the program: {record}"
    );
    assert!(
        record.contains("status=hook, screen"),
        "opencode claims no OSC 7501 report: {record}"
    );

    // 3. The agent is doing the work with nobody having typed anything.
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
        pane_tail(&home, &host)
    );
    let written = std::fs::read_to_string(&wanted).unwrap_or_default();
    assert!(
        written.contains("opencode live"),
        "the file is not what the brief asked for: {written:?}"
    );

    // Everything this test made is torn down with it, the window and the core
    // included: `Live::stop` is what keeps a private desktop private.
    live.stop();
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

/// The person's opencode credential, read only to copy into the test's home.
fn credential(home: &Path) -> Option<PathBuf> {
    let found = home
        .join(".local")
        .join("share")
        .join("opencode")
        .join("auth.json");
    found.is_file().then_some(found)
}

/// The first free model `opencode models` lists, or the fallback the gate used.
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
fn pane_tail(home: &Path, host: &dyn Host) -> String {
    let record = home
        .join(".shprd")
        .join("briefs")
        .join("m4")
        .join("M4.4-a1.tab");
    let Ok(tab) = std::fs::read_to_string(&record) else {
        return format!("(no tab record at {})", record.display());
    };
    let wanted = tab.trim();
    let spaces = host.list_spaces().unwrap_or_default();
    for space in &spaces {
        for listed in host.list_tabs(&space.id).unwrap_or_default() {
            if listed.id.as_str() == wanted {
                return host
                    .read(&listed.root_pane, 40)
                    .unwrap_or_else(|err| format!("(the pane could not be read: {err})"));
            }
        }
    }
    format!("(no tab {wanted} among the host's spaces)")
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
