//! End-to-end tests for `shprd agent review`'s refusals.
//!
//! They drive the real binary against a throwaway repo root, a throwaway home and
//! the real harness data, so they prove the verb is reachable from the command
//! line and that a refused review writes nothing. The effects (the packet, the
//! tab, the launched CLI) need a host, so they are unit-tested with a fake
//! multiplexer in `cmd::review`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

/// The binary under test, built by cargo for this integration test.
const BIN: &str = env!("CARGO_BIN_EXE_shprd");

/// The repository's data directory: the harness specs the verb parses against.
fn data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("data")
}

/// A repository root, a home and the config that belongs to them.
struct Scratch {
    repo: TempDir,
    home: TempDir,
}

impl Scratch {
    /// A scratch pair with `reviewer` written as `roster.reviewer`, if any.
    fn new(reviewer: Option<&str>) -> Scratch {
        let repo = TempDir::new().unwrap_or_else(|err| panic!("{err}"));
        let home = TempDir::new().unwrap_or_else(|err| panic!("{err}"));
        let scratch = Scratch { repo, home };
        if let Some(reviewer) = reviewer {
            let config = scratch.home.path().join(".config").join("shprd");
            std::fs::create_dir_all(&config).unwrap_or_else(|err| panic!("{err}"));
            std::fs::write(
                config.join("config.toml"),
                format!("[roster]\nreviewer = \"{reviewer}\"\n"),
            )
            .unwrap_or_else(|err| panic!("{err}"));
        }
        scratch
    }

    fn root(&self) -> &Path {
        self.repo.path()
    }
}

/// Runs the binary with `--repo`, `SHPRD_HOME` and `SHPRD_DATA_DIR` set.
fn run(scratch: &Scratch, args: &[&str]) -> Output {
    let mut all: Vec<String> = vec!["--repo".into(), scratch.root().display().to_string()];
    all.extend(args.iter().map(|arg| (*arg).to_string()));
    Command::new(BIN)
        .args(&all)
        .env("SHPRD_HOME", scratch.home.path())
        .env("SHPRD_DATA_DIR", data_dir())
        .output()
        .unwrap_or_else(|err| panic!("running {BIN} failed: {err}"))
}

/// Runs the binary and asserts it succeeded.
fn ok(scratch: &Scratch, args: &[&str]) {
    let output = run(scratch, args);
    assert!(
        output.status.success(),
        "expected success for {args:?}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Runs the binary and asserts it exited 2 with `needle` on stderr.
fn refused(scratch: &Scratch, args: &[&str], needle: &str) -> String {
    let output = run(scratch, args);
    assert_eq!(
        output.status.code(),
        Some(2),
        "expected exit 2 for {args:?}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        stderr.contains(needle),
        "expected {needle:?} for {args:?}, got {stderr:?}"
    );
    stderr
}

/// Starts a run and adds the task `T1`.
fn plan(scratch: &Scratch) {
    ok(
        scratch,
        &[
            "run",
            "start",
            "--run-id",
            "t1",
            "--base",
            "main",
            "--integration",
            "integration/m1",
        ],
    );
    ok(
        scratch,
        &[
            "agent",
            "task",
            "add",
            "--id",
            "T1",
            "--title",
            "a task",
            "--scope",
            "README.md",
            "--acceptance",
            "it works",
        ],
    );
}

/// Records attempt 1 of `T1`, written by `agent`, and its completed report.
fn reported(scratch: &Scratch, agent: &str) {
    ok(
        scratch,
        &[
            "agent",
            "spawn",
            "--record-only",
            "--task",
            "T1",
            "--attempt",
            "1",
            "--agent",
            agent,
            "--worktree",
            "/tmp/wt/T1",
            "--branch",
            "feature/t1",
        ],
    );
    ok(
        scratch,
        &[
            "agent",
            "report",
            "--task",
            "T1",
            "--attempt",
            "1",
            "--status",
            "completed",
            "--summary",
            "made it real",
        ],
    );
}

#[test]
fn review_refuses_a_task_that_has_not_reported() {
    let scratch = Scratch::new(Some("codex/gpt-5"));
    plan(&scratch);

    refused(
        &scratch,
        &["agent", "review", "--task", "T1"],
        "not Reported",
    );
}

#[test]
fn review_refuses_when_no_reviewer_is_configured() {
    let scratch = Scratch::new(None);
    plan(&scratch);
    reported(&scratch, "pi/m1");

    refused(
        &scratch,
        &["agent", "review", "--task", "T1"],
        "no reviewer is configured",
    );
}

#[test]
fn review_refuses_a_reviewer_from_the_authors_vendor() {
    // A `pi` worker on Claude and a `pi` reviewer on Claude run the same model
    // family, so the review would prove nothing even though the two models are
    // different.
    let scratch = Scratch::new(Some("pi/anthropic/claude-opus-5-5"));
    plan(&scratch);
    reported(&scratch, "pi/anthropic/claude-sonnet-4");

    let stderr = refused(
        &scratch,
        &["agent", "review", "--task", "T1"],
        "both run models from the anthropic family",
    );
    assert!(stderr.contains("--degraded"), "{stderr}");
    // A refused review writes no packet.
    assert!(
        !scratch.home.path().join(".shprd").join("reviews").exists(),
        "a refused review must write nothing"
    );
}

#[test]
fn review_refuses_a_worker_on_a_router() {
    // `kilo/kilo-auto/free` names no model family, so nothing proves the
    // reviewer is independent: the review is refused, never assumed.
    let scratch = Scratch::new(Some("codex/gpt-5-codex"));
    plan(&scratch);
    reported(&scratch, "kilo/kilo-auto/free");

    let stderr = refused(
        &scratch,
        &["agent", "review", "--task", "T1"],
        "cannot prove the reviewer is a different vendor: kilo/kilo-auto/free has no known \
         model family",
    );
    assert!(stderr.contains("--degraded"), "{stderr}");
}
