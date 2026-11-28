//! A review's `contract.md` lists every check the worker handed in, with its
//! command, exit code and output tail, so the reviewer can name the gates.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

mod support;

use support::isolate;

/// The binary under test, built by cargo for this integration test.
const BIN: &str = env!("CARGO_BIN_EXE_shprd");

/// The variable that makes a pass record its start instead of launching a CLI.
const RECORD_ONLY: &str = "SHPRD_TICK_RECORD_ONLY";

/// The variable that names the actor, which a worker's own shell may have set.
const ACTOR_ENV: &str = "SHPRD_ACTOR";

/// A repository with no remote, a home, and an `integration/m9` branch.
struct Fixture {
    /// Keeps the temp directory alive.
    _temp: TempDir,
    /// The repository the run works on.
    root: PathBuf,
    /// The home whose `.shprd` holds worktrees, reviews and locks.
    home: PathBuf,
    /// The commit `main` and `integration/m9` start from.
    seed: String,
}

impl Fixture {
    /// A repository on `main` with one commit, and an `integration/m9` branch.
    fn new() -> Fixture {
        let temp = TempDir::new().expect("temp dir");
        let root = temp.path().join("repo");
        let home = temp.path().join("home");
        std::fs::create_dir_all(&root).expect("mkdir the repo");
        std::fs::create_dir_all(&home).expect("mkdir the home");
        let fixture = Fixture {
            _temp: temp,
            root,
            home,
            seed: String::new(),
        };
        fixture.git(&["init", "-b", "main"]);
        fixture.git(&["config", "user.name", "shprd tests"]);
        fixture.git(&["config", "user.email", "tests@example.invalid"]);
        fixture.git(&["config", "commit.gpgsign", "false"]);
        fixture.git(&["commit", "--allow-empty", "-m", "seed"]);
        std::fs::write(fixture.root.join("a.txt"), "one\n").expect("write the seed");
        fixture.git(&["add", "--", "a.txt"]);
        fixture.git(&["commit", "-m", "the seed"]);
        fixture.git(&["branch", "integration/m9"]);
        let seed = fixture.rev(&fixture.root.clone(), "HEAD");
        let mut fixture = fixture;
        fixture.seed = seed;
        fixture.write_config("auto");
        fixture
    }

    /// Writes `.shprd/config.toml` with merge policy `merge`.
    fn write_config(&self, merge: &str) {
        let dir = self.root.join(".shprd");
        std::fs::create_dir_all(&dir).expect("mkdir .shprd");
        let body = format!(
            "[gates]\nworker = [\"true\"]\n\n[land]\nmerge = \"{merge}\"\nprotected = []\n"
        );
        std::fs::write(dir.join("config.toml"), body).expect("write the config");
    }

    /// Runs git in the repository, asserting success.
    fn git(&self, args: &[&str]) -> String {
        self.git_in(&self.root.clone(), args)
    }

    /// Runs git in `dir`, asserting success, and returns its trimmed stdout.
    fn git_in(&self, dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git runs");
        assert!(
            output.status.success(),
            "git {args:?} in {} failed: {}",
            dir.display(),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    /// Resolves `rev` in `dir`.
    fn rev(&self, dir: &Path, rev: &str) -> String {
        self.git_in(dir, &["rev-parse", rev])
    }

    /// One command against this fixture.
    fn command(&self, args: &[&str]) -> Command {
        let mut all: Vec<String> = vec![
            "--repo".to_string(),
            self.root.display().to_string(),
            "--home".to_string(),
            self.home.display().to_string(),
            "--no-dagr".to_string(),
        ];
        all.extend(args.iter().map(|arg| (*arg).to_string()));
        let mut command = Command::new(BIN);
        isolate(&mut command, &self.home);
        command
            .args(&all)
            .env(RECORD_ONLY, "1")
            // This fixture's own XDG directories: a pass's last record belongs
            // in the temp home, never in the developer's or the runner's real
            // state directory.
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("XDG_STATE_HOME", self.home.join(".local").join("state"))
            .env("XDG_DATA_HOME", self.home.join(".local").join("share"))
            .env_remove(ACTOR_ENV);
        command
    }

    /// Runs the binary against this fixture.
    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().expect("the binary runs")
    }

    /// Runs the binary and asserts it succeeded, returning stdout.
    fn ok(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "{args:?} failed ({:?}): {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).to_string()
    }

    /// Starts run `r1`, cut from `main`, integrating on `integration/m9`.
    fn start(&self) {
        self.ok(&[
            "run",
            "start",
            "--run-id",
            "r1",
            "--base",
            "main",
            "--integration",
            "integration/m9",
        ]);
    }

    /// A task's own worktree, cut from `integration/m9` at `base`, on
    /// `shprd/r1/<id>`.
    fn worktree(&self, id: &str, base: &str) -> PathBuf {
        let layout = shprd_git::worktree::Layout::default_for_home(&self.home);
        let path = layout.path("repo", "r1", id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir the worktree's parent");
        }
        let path_text = path.display().to_string();
        let branch = format!("shprd/r1/{id}");
        self.git(&["worktree", "add", "-b", &branch, &path_text, base]);
        path
    }

    /// Hands task `T1` in with three checks, the first with an output tail.
    fn hand_in(&self) -> PathBuf {
        let worktree = self.worktree("T1", "integration/m9");
        self.ok(&[
            "agent",
            "task",
            "add",
            "--id",
            "T1",
            "--title",
            "a task",
            "--scope",
            "a.txt",
            "--acceptance",
            "it works",
        ]);
        self.ok(&[
            "agent",
            "spawn",
            "--record-only",
            "--task",
            "T1",
            "--attempt",
            "1",
            "--agent",
            "w1",
            "--worktree",
            &worktree.display().to_string(),
            "--branch",
            "shprd/r1/T1",
        ]);
        std::fs::write(worktree.join("a.txt"), "work\n").expect("write the file");
        self.git_in(&worktree, &["add", "--", "a.txt"]);
        self.git_in(&worktree, &["commit", "-m", "the task's work"]);
        let head = self.rev(&worktree, "HEAD");
        self.ok(&[
            "agent",
            "report",
            "--check",
            "fmt|cargo fmt --all --check|0",
            "--check",
            "clippy|cargo clippy --workspace|0|Finished dev profile",
            "--check",
            "tests|cargo test --workspace|0|test result: ok. 42 passed",
            "--task",
            "T1",
            "--attempt",
            "1",
            "--status",
            "completed",
            "--summary",
            "done",
            "--changed",
            "a.txt",
            "--commit",
            &head,
        ]);
        worktree
    }
}

#[test]
fn the_contract_lists_every_check_the_worker_handed_in() {
    let fixture = Fixture::new();
    fixture.start();
    fixture.hand_in();
    let dir = fixture.home.join(".config").join("shprd");
    std::fs::create_dir_all(&dir).expect("mkdir the settings dir");
    std::fs::write(
        dir.join("settings.toml"),
        "version = 1\n\n[roles.reviewer]\nagent = \"codex\"\nmodel = \"gpt-5\"\n",
    )
    .expect("write the settings");
    // No host is reachable, so the launch fails; the packet is written first.
    let mut command = fixture.command(&["agent", "review", "--task", "T1", "--degraded"]);
    command.env("SHPRD_HC", "/usr/bin/false");
    let _ = command.output().expect("the binary runs");
    let contract = std::fs::read_to_string(
        fixture
            .home
            .join(".shprd")
            .join("reviews")
            .join("r1")
            .join("T1-a1")
            .join("contract.md"),
    )
    .expect("the packet's contract exists");
    for needle in [
        "fmt: `cargo fmt --all --check` exited 0",
        "clippy: `cargo clippy --workspace` exited 0",
        "tests: `cargo test --workspace` exited 0",
        "Finished dev profile",
        "test result: ok. 42 passed",
    ] {
        assert!(contract.contains(needle), "missing {needle:?}:\n{contract}");
    }
}
