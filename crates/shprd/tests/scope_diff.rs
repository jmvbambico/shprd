//! A task's declared scope is a hint, not a limit: a hand-in whose real diff
//! leaves it (even one whose `--changed` list omits the file) lands like any
//! other once its review passes, and an in-scope hand-in still lands.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use shprd_core::event::{Actor, Event, EventKind};
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

    /// Runs one tick pass with `--json` and returns its steps.
    fn tick(&self) -> Vec<Value> {
        let text = self.ok(&["tick", "--once", "--json"]);
        let value: Value = serde_json::from_str(&text)
            .unwrap_or_else(|err| panic!("the tick document did not parse: {err}\n{text}"));
        value
            .get("steps")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_else(|| panic!("the tick document has no steps: {text}"))
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

    /// Appends a review dispatch, which `agent review` would record.
    fn review_requested(&self, id: &str) {
        let path = self.root.join(".shprd").join("events.jsonl");
        let text = std::fs::read_to_string(&path).expect("read the log");
        let seq = text.lines().count() as u64 + 1;
        let event = Event {
            seq,
            at_ms: 1_700_000_000_000 + seq,
            actor: Actor::Human,
            kind: EventKind::ReviewRequested {
                task: id.to_string(),
                attempt: 1,
                reviewer: "c1".to_string(),
                degraded: false,
                note: None,
            },
        };
        let line = serde_json::to_string(&event).expect("serialize the request");
        std::fs::write(&path, format!("{text}{line}\n")).expect("append the request");
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

    /// Drives `id` from added to passed: a commit, a hand-in, a review.
    ///
    /// Returns the commit the task made.
    fn hand_in(&self, id: &str, base: &str, file: &str, body: &str) -> String {
        self.hand_in_with(id, base, file, body, None, true)
    }

    /// As [`Fixture::hand_in`], but the commit also holds `stray`, a file the
    /// task's scope does not cover and the hand-in's `--changed` leaves out. Without `verdict` it stops at the hand-in, which
    /// is where a review starts.
    fn hand_in_with(
        &self,
        id: &str,
        base: &str,
        file: &str,
        body: &str,
        stray: Option<&str>,
        verdict: bool,
    ) -> String {
        let worktree = self.worktree(id, base);
        self.ok(&[
            "agent",
            "task",
            "add",
            "--id",
            id,
            "--title",
            "a task",
            "--scope",
            file,
            "--acceptance",
            "it works",
        ]);
        self.ok(&[
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
            &worktree.display().to_string(),
            "--branch",
            &format!("shprd/r1/{id}"),
        ]);
        std::fs::write(worktree.join(file), body).expect("write the task's file");
        self.git_in(&worktree, &["add", "--", file]);
        if let Some(stray) = stray {
            std::fs::write(worktree.join(stray), "not mine\n").expect("write the stray file");
            self.git_in(&worktree, &["add", "--", stray]);
        }
        self.git_in(&worktree, &["commit", "-m", "the task's work"]);
        let head = self.rev(&worktree, "HEAD");
        self.ok(&[
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
            file,
            "--commit",
            &head,
        ]);
        if !verdict {
            return head;
        }
        self.review_requested(id);
        let packet = self
            .home
            .join(".shprd")
            .join("reviews")
            .join("r1")
            .join(format!("{id}-a1"));
        std::fs::create_dir_all(&packet).expect("mkdir the packet");
        std::fs::write(packet.join("findings.json"), "[]").expect("write the findings");
        self.ok(&[
            "--as",
            "engine",
            "agent",
            "review-verdict",
            "--task",
            id,
            "--attempt",
            "1",
        ]);
        head
    }

    /// The integration branch's head in the repository.
    fn integration_head(&self) -> String {
        self.rev(&self.root.clone(), "refs/heads/integration/m9")
    }
}

#[test]
fn a_file_outside_the_declared_scope_lands_once_reviewed() {
    let fixture = Fixture::new();
    fixture.start();
    fixture.hand_in_with(
        "T2",
        "integration/m9",
        "b.txt",
        "work\n",
        Some("stray2.txt"),
        true,
    );
    let output = fixture.run(&["land", "T2"]);
    assert!(output.status.success(), "{output:?}");
    let head = fixture.integration_head();
    assert_eq!(fixture.git(&["show", &format!("{head}:b.txt")]), "work");
    assert!(
        !fixture
            .git(&["show", &format!("{head}:stray2.txt")])
            .is_empty(),
        "the file outside the declared scope landed too"
    );
    let log = std::fs::read_to_string(fixture.root.join(".shprd").join("events.jsonl"))
        .expect("read the log");
    assert!(!log.contains("hand-in-refused"), "no scope card: {log}");
}

#[test]
fn an_in_scope_hand_in_still_lands() {
    let fixture = Fixture::new();
    fixture.start();
    fixture.hand_in("T1", "integration/m9", "a.txt", "work\n");
    let steps = fixture.tick();
    assert!(
        steps.iter().any(
            |step| step.get("action") == Some(&Value::String("land".into()))
                && step.get("ok") == Some(&Value::Bool(true))
        ),
        "{steps:?}"
    );
    let head = fixture.integration_head();
    assert_eq!(fixture.git(&["show", &format!("{head}:a.txt")]), "work");
}
