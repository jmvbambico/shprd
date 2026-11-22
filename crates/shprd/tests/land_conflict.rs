//! A task whose branch clashes with work that landed after it was cut goes back
//! for a fresh attempt by itself, and lands after it.
//!
//! The first test drives the real binary through a local landing: a clean task
//! lands as before, a clashing one is refused in plain words naming the files,
//! one pass sends it back (without spending a review round), and the new
//! attempt, which merges the latest integration branch, lands. The second does
//! the same for a task landing through a pull request: the stale pull request is
//! closed and the task is recorded as clashing.

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

    /// Every event in the log, oldest first.
    fn events(&self) -> Vec<Event> {
        let path = self.root.join(".shprd").join("events.jsonl");
        let text = std::fs::read_to_string(&path).expect("read the log");
        text.lines()
            .map(|line| serde_json::from_str::<Event>(line).expect("an event line"))
            .collect()
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

/// The actions a pass reported, as `action:task` pairs.
fn actions(steps: &[Value]) -> Vec<String> {
    steps
        .iter()
        .map(|step| {
            let action = step
                .get("action")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_string();
            let task = step
                .get("task")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_string();
            format!("{action}:{task}")
        })
        .collect()
}

impl Fixture {
    /// What the second attempt of `task` does: brings the first attempt's
    /// commits onto the latest integration branch, resolves `file` to `body`,
    /// commits, hands in and is reviewed.
    fn redo(&self, task: &str, file: &str, body: &str) {
        let worktree =
            shprd_git::worktree::Layout::default_for_home(&self.home).path("repo", "r1", task);
        assert!(worktree.exists(), "the new attempt has its worktree");
        let attempt = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&worktree)
                .output()
                .expect("git runs")
        };
        // The brief says to merge the latest integration branch when the branch
        // holds the earlier commits, else to take them from the archive ref.
        if attempt(&["merge", "integration/m9", "--no-edit"])
            .status
            .success()
        {
            let refs = self.git(&["for-each-ref", "--format=%(refname)", "refs/shprd/archive"]);
            let archive = refs.lines().next().expect("the old attempt is archived");
            let pick = attempt(&["cherry-pick", archive]);
            assert!(!pick.status.success(), "the pick clashes: {pick:?}");
        }
        std::fs::write(worktree.join(file), body).expect("resolve");
        self.git_in(&worktree, &["add", "--", file]);
        self.git_in(
            &worktree,
            &[
                "-c",
                "core.editor=true",
                "commit",
                "-m",
                "resolve the clash",
            ],
        );
        let head = self.rev(&worktree, "HEAD");
        self.ok(&[
            "agent",
            "report",
            "--check",
            "tests|cargo test|0",
            "--task",
            task,
            "--attempt",
            "2",
            "--status",
            "completed",
            "--summary",
            "done",
            "--changed",
            file,
            "--commit",
            &head,
        ]);
        let path = self.root.join(".shprd").join("events.jsonl");
        let text = std::fs::read_to_string(&path).expect("read the log");
        let seq = text.lines().count() as u64 + 1;
        let event = Event {
            seq,
            at_ms: 1_700_000_000_000 + seq,
            actor: Actor::Human,
            kind: EventKind::ReviewRequested {
                task: task.to_string(),
                attempt: 2,
                reviewer: "c1".to_string(),
                degraded: false,
                note: None,
            },
        };
        let line = serde_json::to_string(&event).expect("serialize");
        std::fs::write(&path, format!("{text}{line}\n")).expect("append");
        let packet = self
            .home
            .join(".shprd/reviews/r1")
            .join(format!("{task}-a2"));
        std::fs::create_dir_all(&packet).expect("mkdir the packet");
        std::fs::write(packet.join("findings.json"), "[]").expect("findings");
        self.ok(&[
            "--as",
            "engine",
            "agent",
            "review-verdict",
            "--task",
            task,
            "--attempt",
            "2",
        ]);
    }
}

/// The plain words a person is told.
const CLASHED: &str =
    "T3 clashed with work that landed after it; it is being redone on top of the latest";

#[test]
fn a_clean_task_lands_and_a_clashing_one_is_redone_and_lands_after() {
    let fixture = Fixture::new();
    fixture.start();
    let seed = fixture.seed.clone();

    // A clean task lands as before.
    fixture.hand_in("T1", "integration/m9", "a.txt", "the first task's work\n");
    let steps = fixture.tick();
    assert_eq!(actions(&steps), vec!["land:T1".to_string()], "{steps:?}");
    assert!(
        steps
            .iter()
            .all(|s| s.get("ok") == Some(&Value::Bool(true)))
    );

    // A task cut from the old head rewrites the same file: it clashes.
    fixture.hand_in("T3", &seed, "a.txt", "a conflicting change\n");
    let before = fixture.integration_head();
    let output = fixture.run(&["land", "T3"]);
    assert_eq!(output.status.code(), Some(2));
    let message = String::from_utf8_lossy(&output.stderr);
    assert!(message.contains(CLASHED), "plain words: {message}");
    assert!(message.contains("a.txt"), "the files are named: {message}");
    assert!(
        !message.contains("did not accept"),
        "never the bare forge words: {message}"
    );
    assert_eq!(fixture.integration_head(), before, "nothing landed");

    // The next pass sends it back, once.
    let steps = fixture.tick();
    assert_eq!(
        actions(&steps),
        vec!["send_back:T3".to_string()],
        "{steps:?}"
    );
    assert!(
        steps
            .iter()
            .all(|s| s.get("ok") == Some(&Value::Bool(true))),
        "{steps:?}"
    );
    let again = fixture.tick();
    assert!(
        !actions(&again).contains(&"send_back:T3".to_string()),
        "a pass plans no second send-back for the same attempt: {again:?}"
    );

    // It was a send-back, not a review round: the review never failed.
    let failed_reviews = fixture
        .events()
        .into_iter()
        .filter(|event| {
            matches!(&event.kind, EventKind::GateFailed { task, gate, .. }
                if task == "T3" && gate == "review")
        })
        .count();
    assert_eq!(failed_reviews, 0);

    // The brief the new attempt gets carries the failed gate and its files.
    let finding = fixture
        .events()
        .into_iter()
        .find_map(|event| match event.kind {
            EventKind::GateFailed {
                task,
                gate,
                findings,
                ..
            } if task == "T3" && gate == "integrate" => findings.into_iter().next(),
            _ => None,
        })
        .expect("the clash is recorded as a failed gate");
    assert!(finding.detail.contains("a.txt"), "{finding:?}");
    assert!(finding.title.contains("being redone"), "{finding:?}");
    let spawned = fixture.events().into_iter().any(
        |event| matches!(&event.kind, EventKind::Spawned { task, attempt: 2, .. } if task == "T3"),
    );
    assert!(spawned, "a second attempt was started");

    // The new attempt merges the latest, resolves the clash and hands in.
    fixture.redo(
        "T3",
        "a.txt",
        "the first task's work\nand a conflicting change\n",
    );

    let steps = fixture.tick();
    assert_eq!(actions(&steps), vec!["land:T3".to_string()], "{steps:?}");
    assert!(
        steps
            .iter()
            .all(|s| s.get("ok") == Some(&Value::Bool(true))),
        "{steps:?}"
    );
    let head = fixture.integration_head();
    assert_eq!(
        fixture.git(&["show", &format!("{head}:a.txt")]),
        "the first task's work\nand a conflicting change"
    );
}

#[test]
fn a_clashing_pull_request_is_closed_and_the_task_is_recorded_as_clashing() {
    let fixture = Fixture::new();
    fixture.start();
    // Cut from main, then main moves under the same file.
    let head = fixture.hand_in("T3", "main", "a.txt", "a conflicting change\n");
    fixture.git(&["checkout", "-q", "main"]);
    std::fs::write(fixture.root.join("a.txt"), "work that landed first\n").unwrap();
    fixture.git(&["commit", "-qam", "work that landed first"]);
    let origin = fixture.root.parent().unwrap().join("origin.git");
    fixture.git(&["init", "-q", "--bare", origin.to_str().unwrap()]);
    fixture.git(&["remote", "add", "origin", origin.to_str().unwrap()]);
    let script = fixture.home.join("forge.json");
    let body = serde_json::json!({
        "status": [{"number": 1, "approved": true, "checks_passed": true,
                    "merged": false, "head_sha": head}],
    });
    std::fs::write(&script, body.to_string()).unwrap();
    let mut command = fixture.command(&["land", "T3"]);
    let output = command.env("SHPRD_FAKE_FORGE", &script).output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    let message = String::from_utf8_lossy(&output.stderr);
    assert!(message.contains(CLASHED), "{message}");
    assert!(message.contains("a.txt"), "{message}");
    let calls = std::fs::read_to_string(format!("{}.calls", script.display())).unwrap();
    assert!(
        calls.contains("close 1"),
        "the stale pull request was closed: {calls}"
    );
    assert!(
        !calls.contains("merge 1"),
        "no merge was attempted: {calls}"
    );
    let clashed = fixture.events().into_iter().any(|event| {
        matches!(&event.kind, EventKind::GateFailed { task, gate, .. }
            if task == "T3" && gate == "integrate")
    });
    assert!(clashed, "the clash is recorded for the planner");
}
