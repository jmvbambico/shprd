//! End-to-end tests for `shprd sync`: the base moves under a run, and the
//! integration branch is brought up to it without the base being touched.
//!
//! They drive the real binary against a throwaway repository whose `origin` is
//! a local bare repository, and never against a real remote or the real home.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use shprd_core::event::{Actor, Event, EventKind};
use tempfile::TempDir;

/// The binary under test, built by cargo for this integration test.
const BIN: &str = env!("CARGO_BIN_EXE_shprd");

/// The environment variable that names the actor.
///
/// These tests exercise the human and orchestrator paths, so a worker's own
/// `SHPRD_ACTOR` must not leak into the binary.
const ACTOR_ENV: &str = "SHPRD_ACTOR";

/// A repository, a bare `origin`, and the home holding shprd's git state.
struct Fixture {
    /// Keeps the temp directory alive.
    _temp: TempDir,
    /// The repository the run works on.
    root: PathBuf,
    /// The home whose `.shprd` holds worktrees.
    home: PathBuf,
    /// The bare repository that stands in for the remote.
    origin: PathBuf,
}

/// Creates a directory, panicking on failure.
fn mkdir(path: &Path) {
    if let Err(err) = std::fs::create_dir_all(path) {
        panic!("mkdir {}: {err}", path.display());
    }
}

/// Writes a file, making its directory, panicking on failure.
fn write(path: &Path, body: &str) {
    if let Some(parent) = path.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        panic!("mkdir {}: {err}", parent.display());
    }
    if let Err(err) = std::fs::write(path, body) {
        panic!("write {}: {err}", path.display());
    }
}

impl Fixture {
    /// A repository on `main`, pushed to a fresh bare origin, with a config.
    fn new() -> Fixture {
        let temp = match TempDir::new() {
            Ok(temp) => temp,
            Err(err) => panic!("tempdir: {err}"),
        };
        let root = temp.path().join("repo");
        let home = temp.path().join("home");
        let origin = temp.path().join("origin.git");
        mkdir(&root);
        mkdir(&home);
        let fixture = Fixture {
            _temp: temp,
            root,
            home,
            origin,
        };
        fixture.git(&[
            "init",
            "--bare",
            fixture.origin.to_str().unwrap_or("origin.git"),
        ]);
        fixture.git(&["init", "-b", "main"]);
        fixture.git(&["config", "user.name", "shprd tests"]);
        fixture.git(&["config", "user.email", "tests@example.invalid"]);
        fixture.git(&["config", "commit.gpgsign", "false"]);
        fixture.git(&[
            "remote",
            "add",
            "origin",
            fixture.origin.to_str().unwrap_or("origin.git"),
        ]);
        write(&fixture.root.join("a.txt"), "one\n");
        fixture.git(&["add", "--", "a.txt"]);
        fixture.git(&["commit", "-m", "seed"]);
        fixture.git(&["push", "origin", "main"]);
        fixture.write_config();
        fixture
    }

    /// Writes `.shprd/config.toml`: gates that pass, no forge merge.
    fn write_config(&self) {
        let dir = self.root.join(".shprd");
        mkdir(&dir);
        write(
            &dir.join("config.toml"),
            "[gates]\nworker = [\"true\"]\nintegration = [\"true\"]\n\n[land]\nmerge = \"ask\"\n",
        );
    }

    /// Runs git in the repository, asserting success, and returns its stdout.
    fn git(&self, args: &[&str]) -> String {
        self.git_in(&self.root, args)
    }

    /// Runs git in `dir`, returning `None` when it fails.
    fn git_try(&self, dir: &Path, args: &[&str]) -> Option<String> {
        let output = Command::new("git").args(args).current_dir(dir).output();
        match output {
            Ok(output) if output.status.success() => {
                Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
            }
            _ => None,
        }
    }

    /// Runs git in `dir`, asserting success, and returns its stdout.
    fn git_in(&self, dir: &Path, args: &[&str]) -> String {
        let output = match Command::new("git").args(args).current_dir(dir).output() {
            Ok(output) => output,
            Err(err) => panic!("git {args:?} in {}: {err}", dir.display()),
        };
        assert!(
            output.status.success(),
            "git {args:?} in {} failed: {}",
            dir.display(),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    /// Runs the binary against this fixture.
    fn run(&self, args: &[&str]) -> Output {
        let mut all: Vec<String> = vec![
            "--repo".to_string(),
            self.root.display().to_string(),
            "--home".to_string(),
            self.home.display().to_string(),
            "--no-dagr".to_string(),
        ];
        all.extend(args.iter().map(|arg| (*arg).to_string()));
        match Command::new(BIN).args(&all).env_remove(ACTOR_ENV).output() {
            Ok(output) => output,
            Err(err) => panic!("running {BIN}: {err}"),
        }
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

    /// Runs the binary and asserts it was refused with exit 2.
    fn refused(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?} should have been refused, stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        String::from_utf8_lossy(&output.stderr).to_string()
    }

    /// Starts run `r1`, cut from `main`.
    fn start(&self) {
        self.ok(&[
            "run",
            "start",
            "--run-id",
            "r1",
            "--base",
            "main",
            "--integration",
            "main",
        ]);
    }

    /// Adds task `id` and drives it to a passed review, then to integrated.
    fn land_a_task(&self, id: &str) {
        self.ok(&[
            "agent",
            "task",
            "add",
            "--id",
            id,
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
            id,
            "--attempt",
            "1",
            "--agent",
            "w1",
            "--worktree",
            "/tmp/wt",
            "--branch",
            "feature/t1",
        ]);
        self.ok(&[
            "agent",
            "report",
            "--task",
            id,
            "--attempt",
            "1",
            "--status",
            "completed",
            "--summary",
            "done",
            "--changed",
            "a.txt",
        ]);
        self.review_requested(id);
        self.ok(&[
            "agent",
            "gate",
            "pass",
            "--task",
            id,
            "--attempt",
            "1",
            "--gate",
            "lint",
        ]);
        let packet = self
            .home
            .join(".shprd")
            .join("reviews")
            .join("r1")
            .join(format!("{id}-a1"));
        mkdir(&packet);
        write(&packet.join("findings.json"), "[]");
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
        self.commit_on("feature/t1", "the task's work");
        self.ok(&["agent", "integrate", "--task", id]);
    }

    /// Records the review request a reviewer tab would have made.
    ///
    /// `shprd agent review` opens a tab in a multiplexer, which no test here
    /// has, so the fact it would record is written directly.
    fn review_requested(&self, id: &str) {
        let path = self.root.join(".shprd").join("events.jsonl");
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) => panic!("read {}: {err}", path.display()),
        };
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
            },
        };
        let line = match serde_json::to_string(&event) {
            Ok(line) => line,
            Err(err) => panic!("serialize the review request: {err}"),
        };
        write(&path, &format!("{text}{line}\n"));
    }

    /// Commits one rewrite of `a.txt` on `branch`, cut from `main`.
    fn commit_on(&self, branch: &str, message: &str) -> String {
        self.git(&["checkout", "-b", branch, "main"]);
        write(&self.root.join("a.txt"), "feature\n");
        self.git(&["add", "--", "a.txt"]);
        self.git(&["commit", "-m", message]);
        self.git(&["checkout", "main"]);
        self.git(&["rev-parse", branch])
    }

    /// Commits a change to `main` and pushes it: a pull request merged meanwhile.
    fn move_base(&self, file: &str, body: &str) -> String {
        write(&self.root.join(file), body);
        self.git(&["add", "--", file]);
        self.git(&["commit", "-m", "a merged pull request"]);
        self.git(&["push", "origin", "main"]);
        self.git(&["rev-parse", "main"])
    }

    /// The integration worktree the run owns.
    fn integration(&self) -> PathBuf {
        shprd_git::worktree::Layout::default_for_home(&self.home).path("repo", "r1", "_int")
    }

    /// The integration worktree's head.
    fn integration_head(&self) -> String {
        self.git_in(&self.integration(), &["rev-parse", "HEAD"])
    }

    /// Every event in the log, oldest first.
    fn events(&self) -> Vec<Event> {
        let path = self.root.join(".shprd").join("events.jsonl");
        let body = match std::fs::read_to_string(&path) {
            Ok(body) => body,
            Err(err) => panic!("read {}: {err}", path.display()),
        };
        let mut events = Vec::new();
        for line in body.lines() {
            match serde_json::from_str::<Event>(line) {
                Ok(event) => events.push(event),
                Err(err) => panic!("bad event line {line:?}: {err}"),
            }
        }
        events
    }

    /// The last base sync the log recorded, if it recorded one.
    fn last_sync(&self) -> Option<(String, String, Actor)> {
        self.events()
            .into_iter()
            .rev()
            .find_map(|event| match event.kind {
                EventKind::BaseSynced { base_sha, int_sha } => {
                    Some((base_sha, int_sha, event.actor))
                }
                _ => None,
            })
    }

    /// Every escalation the log recorded, as bodies.
    fn escalations(&self) -> Vec<String> {
        self.events()
            .into_iter()
            .filter_map(|event| match event.kind {
                EventKind::Escalated { body, .. } => Some(body),
                _ => None,
            })
            .collect()
    }
}

#[test]
fn a_moved_base_is_merged_and_a_land_after_it_needs_a_new_gate() {
    let fixture = Fixture::new();
    fixture.start();
    fixture.land_a_task("T1");
    fixture.ok(&["gate"]);
    let gated = fixture.integration_head();

    // A pull request merges meanwhile: `main` moves on the remote.
    let moved = fixture.move_base("src/base.txt", "base\n");
    assert_eq!(fixture.last_sync(), None, "nothing was synced yet");

    let stdout = fixture.ok(&["--as", "orchestrator", "sync"]);
    assert!(
        stdout.contains("synced shprd/r1/int with main at "),
        "the human line says what happened: {stdout}"
    );
    let synced = fixture.integration_head();
    assert_ne!(synced, gated, "the sync moved the integration head");
    assert_eq!(
        fixture
            .git_in(&fixture.integration(), &["log", "-1", "--format=%P"])
            .split(' ')
            .count(),
        2,
        "a merge commit, so task commits keep their ids"
    );
    assert_eq!(
        fixture.last_sync(),
        Some((moved.clone(), synced.clone(), Actor::Orchestrator)),
        "the log records both ends and who synced"
    );
    // The gate ran on the old head, so landing has to ask for a fresh one.
    let message = fixture.refused(&["land"]);
    assert!(message.contains("run `shprd gate` again"), "{message}");
}

#[test]
fn land_hints_at_a_sync_it_will_not_do() {
    let fixture = Fixture::new();
    fixture.start();
    fixture.land_a_task("T1");
    fixture.ok(&["gate"]);
    // A commit after the gate makes it stale, and the base moves too.
    fixture.git_in(
        &fixture.integration(),
        &["commit", "--allow-empty", "-m", "after the gate"],
    );
    fixture.move_base("src/base.txt", "base\n");

    let message = fixture.refused(&["land"]);
    assert!(
        message.contains("main moved since this run started: run `shprd sync`, then `shprd gate`"),
        "land hints at the sync it will not do itself: {message}"
    );
    assert_eq!(fixture.last_sync(), None, "land never syncs by itself");
}

#[test]
fn a_conflicting_base_is_undone_and_escalated() {
    let fixture = Fixture::new();
    fixture.start();
    fixture.land_a_task("T1");
    let before = fixture.integration_head();

    // The base changes the very file the landed task did.
    fixture.move_base("a.txt", "the base's version\n");
    let message = fixture.refused(&["sync"]);

    assert!(message.contains("a.txt"), "the file is named: {message}");
    assert!(
        message.contains("resolve by adding a task scoped to those files, or merge by hand in"),
        "the advice is actionable: {message}"
    );
    assert_eq!(fixture.integration_head(), before, "the head must not move");
    assert_eq!(
        fixture.git_in(&fixture.integration(), &["status", "--porcelain"]),
        "",
        "the worktree must be clean again"
    );
    assert_eq!(
        fixture.git_try(
            &fixture.integration(),
            &["rev-parse", "--verify", "MERGE_HEAD"]
        ),
        None,
        "no merge may be left in progress"
    );
    assert_eq!(fixture.last_sync(), None, "a conflict syncs nothing");
    let escalations = fixture.escalations();
    assert_eq!(escalations.len(), 1, "{escalations:?}");
    assert!(escalations[0].contains("a.txt"), "{escalations:?}");
}

#[test]
fn an_up_to_date_base_records_nothing() {
    let fixture = Fixture::new();
    fixture.start();
    let stdout = fixture.ok(&["sync"]);
    assert!(
        stdout.contains("is up to date with main at "),
        "the human line says so: {stdout}"
    );
    assert_eq!(fixture.last_sync(), None, "nothing to record");
    assert!(fixture.escalations().is_empty(), "nothing to escalate");
}

#[test]
fn an_agent_may_not_sync_the_base() {
    let fixture = Fixture::new();
    fixture.start();
    fixture.move_base("src/base.txt", "base\n");
    let message = fixture.refused(&["--as", "agent:claude", "sync"]);
    assert!(message.contains("refused:"), "{message}");
    assert!(message.contains("cannot sync the base"), "{message}");
    assert_eq!(fixture.last_sync(), None, "nothing was recorded");
    assert!(
        !fixture.integration().join("src/base.txt").exists(),
        "nothing was merged"
    );
}
