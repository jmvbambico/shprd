//! A task's scope implies its own crates' tests and the synced copies of shared
//! desktop files, so a hand-in that changes them is not refused; declared paths
//! still decide whether two tasks may run together.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::process::{Command, Output};

use tempfile::TempDir;

mod support;

use support::isolate;

/// The binary under test, built by cargo for this integration test.
const BIN: &str = env!("CARGO_BIN_EXE_shprd");

/// A repository with one commit, a home, and a started run.
struct Fixture {
    /// Keeps the temp directory alive.
    _temp: TempDir,
    /// The repository.
    root: PathBuf,
    /// The home.
    home: PathBuf,
    /// The one commit.
    seed: String,
}

impl Fixture {
    fn new() -> Fixture {
        let temp = TempDir::new().expect("temp dir");
        let root = temp.path().join("repo");
        let home = temp.path().join("home");
        std::fs::create_dir_all(&root).expect("mkdir the repo");
        std::fs::create_dir_all(&home).expect("mkdir the home");
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .current_dir(&root)
                .output()
                .expect("git runs");
            assert!(output.status.success(), "git {args:?}: {output:?}");
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        };
        git(&["init", "-b", "main"]);
        git(&["config", "user.name", "shprd tests"]);
        git(&["config", "user.email", "tests@example.invalid"]);
        git(&["config", "commit.gpgsign", "false"]);
        git(&["commit", "--allow-empty", "-m", "seed"]);
        git(&["branch", "integration/m9"]);
        let seed = git(&["rev-parse", "HEAD"]);
        let fixture = Fixture {
            _temp: temp,
            root,
            home,
            seed,
        };
        let out = fixture.run(&[
            "run",
            "start",
            "--run-id",
            "r1",
            "--base",
            "main",
            "--integration",
            "integration/m9",
        ]);
        assert!(out.status.success(), "{out:?}");
        fixture
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut command = Command::new(BIN);
        isolate(&mut command, &self.home);
        command
            .args([
                "--repo",
                &self.root.display().to_string(),
                "--home",
                &self.home.display().to_string(),
                "--no-dagr",
            ])
            .args(args)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("XDG_STATE_HOME", self.home.join(".local").join("state"))
            .env("XDG_DATA_HOME", self.home.join(".local").join("share"))
            .env_remove("SHPRD_ACTOR");
        command.output().expect("the binary runs")
    }

    /// Adds task `id` with `scope` and starts its first attempt.
    fn start_task(&self, id: &str, scope: &str) -> Output {
        let added = self.run(&[
            "agent",
            "task",
            "add",
            "--id",
            id,
            "--title",
            "a task",
            "--scope",
            scope,
            "--acceptance",
            "it works",
        ]);
        assert!(added.status.success(), "{added:?}");
        self.run(&[
            "agent",
            "spawn",
            "--record-only",
            "--task",
            id,
            "--attempt",
            "1",
            "--agent",
            "w1",
            "--worktree",
            &self.root.display().to_string(),
            "--branch",
            &format!("shprd/r1/{id}"),
        ])
    }

    /// Hands in `id` as completed, saying it changed `path`.
    fn hand_in(&self, id: &str, path: &str) -> Output {
        self.run(&[
            "agent",
            "report",
            "--check",
            "tests|cargo test|0",
            "--task",
            id,
            "--attempt",
            "1",
            "--status",
            "completed",
            "--summary",
            "done",
            "--changed",
            path,
            "--commit",
            &self.seed,
        ])
    }
}

#[test]
fn a_crate_scope_covers_its_own_tests_but_not_another_crates() {
    let fixture = Fixture::new();
    assert!(
        fixture
            .start_task("T1", "crates/shprd-core/src/prompts.rs")
            .status
            .success()
    );
    let wrong = fixture.hand_in("T1", "crates/shprd/tests/x.rs");
    assert!(!wrong.status.success(), "{wrong:?}");
    let right = fixture.hand_in("T1", "crates/shprd-core/tests/x.rs");
    assert!(right.status.success(), "{right:?}");
}

#[test]
fn a_shared_lib_scope_covers_the_synced_copies() {
    let fixture = Fixture::new();
    assert!(
        fixture
            .start_task("T1", "host/herdr-desktop/mods/lib/chat.js")
            .status
            .success()
    );
    let copy = fixture.hand_in("T1", "host/herdr-desktop/mods/shprd-chat/lib/chat.js");
    assert!(copy.status.success(), "{copy:?}");
}

#[test]
fn tasks_whose_implied_paths_overlap_still_start_together() {
    let fixture = Fixture::new();
    let first = fixture.start_task("T1", "crates/shprd-core/src/a.rs");
    assert!(first.status.success(), "{first:?}");
    // T2 declares a tests path T1 only implies.
    let second = fixture.start_task("T2", "crates/shprd-core/tests/x.rs");
    assert!(second.status.success(), "{second:?}");
}
