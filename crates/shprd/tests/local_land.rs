//! End-to-end for landing in a repository with no remote.
//!
//! There is no `origin` here, so `shprd land` and `shprd tick` cannot push or
//! open a pull request. They land a task by merging its branch into the run's
//! integration branch inside shprd's own integration worktree. This test drives
//! the real binary through that path: two tasks land under `auto`, the
//! integration branch holds both, the user's checkout never moves, and a third
//! task that conflicts is undone and escalated.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use shprd_core::event::{Actor, Event, EventKind};
use tempfile::TempDir;

mod support;

use support::herdr::isolate;

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

    /// Writes the person's `settings.toml` with `tasks.merge = merge`.
    ///
    /// It is the file the brief's Settings block is printed from, so the test
    /// that lands under it proves the tick and the brief read one answer.
    fn write_settings(&self, merge: &str) {
        let dir = self.home.join(".config").join("shprd");
        std::fs::create_dir_all(&dir).expect("mkdir the settings dir");
        let body = format!("version = 1\n\n[tasks]\nmerge = \"{merge}\"\n");
        std::fs::write(dir.join("settings.toml"), body).expect("write the settings");
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

    /// The integration worktree shprd lands in.
    fn integration(&self) -> PathBuf {
        shprd_git::worktree::Layout::default_for_home(&self.home).path("repo", "r1", "_int")
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

#[test]
fn a_repository_without_a_remote_lands_locally_and_escalates_a_conflict() {
    let fixture = Fixture::new();
    fixture.start();
    let seed = fixture.seed.clone();
    let main_before = fixture.rev(&fixture.root, "HEAD");

    // Two tasks hand in, both cut from the integration branch.
    let t1 = fixture.hand_in("T1", "integration/m9", "a.txt", "the first task's work\n");
    let t2 = fixture.hand_in("T2", "integration/m9", "b.txt", "the second task's work\n");

    // The first pass lands under `auto` with no forge and no remote at all.
    let steps = fixture.tick();
    assert_eq!(
        actions(&steps),
        vec!["land:T1".to_string(), "land:T2".to_string()],
        "the tick plans a local landing for both ready tasks: {steps:?}"
    );
    assert!(
        steps
            .iter()
            .all(|step| step.get("ok") == Some(&Value::Bool(true))),
        "both local landings worked: {steps:?}"
    );

    // The declared integration branch holds both tasks' work.
    let head = fixture.integration_head();
    assert_ne!(head, seed, "the integration branch moved");
    assert_eq!(
        fixture.git(&["show", &format!("{head}:a.txt")]),
        "the first task's work",
        "the integration branch holds the first task"
    );
    assert_eq!(
        fixture.git(&["show", &format!("{head}:b.txt")]),
        "the second task's work",
        "the integration branch holds the second task"
    );
    assert!(
        fixture
            .git(&["merge-base", "--is-ancestor", &t1, &head])
            .is_empty(),
        "the first task's commit is in the integration branch"
    );
    assert!(
        fixture
            .git(&["merge-base", "--is-ancestor", &t2, &head])
            .is_empty(),
        "the second task's commit is in the integration branch"
    );

    // Landing an already integrated task again reports it and records nothing.
    let before_relaunch = fixture.integration_head();
    let printed = fixture.ok(&["land", "T1"]);
    assert!(printed.contains("already integrated"), "{printed}");
    assert_eq!(
        fixture.integration_head(),
        before_relaunch,
        "a re-run moved nothing"
    );

    // The user's checkout never moved, and never held the work in progress.
    assert_eq!(
        fixture.rev(&fixture.root, "HEAD"),
        main_before,
        "the user's checkout is untouched"
    );
    assert_eq!(
        fixture.git(&["status", "--porcelain", "--untracked-files=no"]),
        "",
        "the user's checkout stayed clean"
    );
    assert_eq!(
        fixture.rev(&fixture.root, "refs/heads/main"),
        seed,
        "the base branch never moved"
    );

    // A third task cut from the old integration head rewrites the same file:
    // its merge conflicts, is undone, and is escalated.
    fixture.write_config("auto");
    fixture.hand_in("T3", &seed, "a.txt", "a conflicting change\n");
    let before = fixture.integration_head();
    let output = fixture
        .command(&["land", "T3"])
        .output()
        .expect("land runs");
    assert_eq!(
        output.status.code(),
        Some(2),
        "a conflicting landing is refused, not reported as landed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let message = String::from_utf8_lossy(&output.stderr);
    assert!(message.contains("conflict"), "{message}");
    assert_eq!(
        fixture.integration_head(),
        before,
        "the conflicting merge was undone"
    );
    assert_eq!(
        fixture.git_in(&fixture.integration(), &["status", "--porcelain"]),
        "",
        "the integration worktree is clean after the undo"
    );
    assert_eq!(
        fixture.rev(&fixture.root, "HEAD"),
        main_before,
        "the conflict did not touch the user's checkout either"
    );

    let escalated = fixture.events().into_iter().any(|event| {
        matches!(&event.kind, EventKind::Escalated { task: Some(task), .. } if task == "T3")
    });
    assert!(escalated, "the conflict was escalated to a person");
    let landed = fixture
        .events()
        .into_iter()
        .any(|event| matches!(&event.kind, EventKind::TaskLanded { task, .. } if task == "T3"));
    assert!(!landed, "a conflicting task is not recorded as landed");
}

#[test]
fn a_local_ask_waits_for_a_person_and_local_checks_land_on_the_report() {
    let fixture = Fixture::new();
    fixture.write_config("ask");
    fixture.start();
    let head = fixture.hand_in("T1", "integration/m9", "a.txt", "the task's work\n");
    let seed = fixture.seed.clone();

    // `ask` is a question, not a landing: nothing is merged and the person is
    // told once.
    let steps = fixture.tick();
    assert_eq!(
        actions(&steps),
        vec!["needs_you:T1".to_string()],
        "{steps:?}"
    );
    assert_eq!(
        fixture.integration_head(),
        seed,
        "ask never lands without the person"
    );

    // `checks` has no forge to ask, so the hand-in's passing report is the
    // checks; the next pass lands it.
    fixture.write_config("checks");
    let steps = fixture.tick();
    assert_eq!(actions(&steps), vec!["land:T1".to_string()], "{steps:?}");
    assert_eq!(steps[0].get("ok"), Some(&Value::Bool(true)), "{steps:?}");
    assert_eq!(fixture.integration_head(), head);
    assert_eq!(
        fixture.git(&["show", "integration/m9:a.txt"]),
        "the task's work"
    );
}

#[test]
fn a_local_ask_can_be_approved_on_the_task_itself() {
    let fixture = Fixture::new();
    fixture.write_config("ask");
    fixture.start();
    let head = fixture.hand_in("T1", "integration/m9", "a.txt", "the task's work\n");
    let seed = fixture.seed.clone();

    // The ask names the verb a person can actually run: there is no pull
    // request, so nothing points at a forge.
    let steps = fixture.tick();
    assert_eq!(
        actions(&steps),
        vec!["needs_you:T1".to_string()],
        "{steps:?}"
    );
    let detail = steps[0]
        .get("detail")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(detail.contains("shprd approve T1"), "{detail}");
    assert!(!detail.contains("pull request"), "{detail}");
    assert_eq!(fixture.integration_head(), seed);

    // The task's own approval verb works without a pull request.
    let approved = fixture.ok(&["approve", "T1", "--yes"]);
    assert!(
        approved.contains("approved T1's local landing"),
        "{approved}"
    );

    // And the next pass lands the head the person approved.
    let steps = fixture.tick();
    assert_eq!(actions(&steps), vec!["land:T1".to_string()], "{steps:?}");
    assert_eq!(steps[0].get("ok"), Some(&Value::Bool(true)), "{steps:?}");
    assert_eq!(fixture.integration_head(), head);
    assert_eq!(
        fixture.git(&["show", "integration/m9:a.txt"]),
        "the task's work"
    );
}

#[test]
fn the_settings_file_alone_sets_the_merge_policy_the_tick_lands_under() {
    // The M9 gate's bugs 2 and 6 in one run: `tasks.merge` in the person's
    // settings was what the brief printed, but the tick read the legacy
    // `config.toml`, so `auto` needed a restart and a file nobody was editing.
    let fixture = Fixture::new();
    // Nothing in `config.toml` says how a task lands: only the settings do.
    std::fs::remove_file(fixture.root.join(".shprd").join("config.toml"))
        .expect("the repo config is removed");
    fixture.write_settings("auto");
    fixture.start();
    let head = fixture.hand_in("T1", "integration/m9", "a.txt", "the first task's work\n");
    let seed = fixture.integration_head();
    assert_ne!(
        seed, head,
        "the hand-in is on a branch, not the integration one"
    );

    let steps = fixture.tick();
    assert_eq!(actions(&steps), vec!["land:T1".to_string()], "{steps:?}");
    assert_eq!(steps[0].get("ok"), Some(&Value::Bool(true)), "{steps:?}");
    assert_eq!(
        fixture.integration_head(),
        head,
        "the landing wrote the branch the settings allow"
    );

    // A settings change to `ask` applies on the next pass, with no restart and
    // nothing written to `config.toml`.
    fixture.write_settings("ask");
    fixture.hand_in("T2", "integration/m9", "b.txt", "the second task's work\n");
    let before = fixture.integration_head();
    let steps = fixture.tick();
    assert_eq!(
        actions(&steps),
        vec!["needs_you:T2".to_string()],
        "an ask never lands by itself: {steps:?}"
    );
    assert_eq!(fixture.integration_head(), before, "the ask moved nothing");
}

/// An accepted task in a repository whose `origin` is `url`: the land step
/// merges locally and says so in one plain sentence, whatever `gh` thinks.
fn accepted_task_lands_with_origin(url: &str) -> (Fixture, String) {
    let fixture = Fixture::new();
    fixture.write_config("ask");
    fixture.git(&["remote", "add", "origin", url]);
    fixture.start();
    let head = fixture.hand_in("T1", "integration/m9", "a.txt", "the task's work\n");
    let steps = fixture.tick();
    assert_eq!(
        actions(&steps),
        vec!["needs_you:T1".to_string()],
        "{steps:?}"
    );
    fixture.ok(&["approve", "T1", "--yes"]);
    let steps = fixture.tick();
    assert_eq!(actions(&steps), vec!["land:T1".to_string()], "{steps:?}");
    assert_eq!(steps[0].get("ok"), Some(&Value::Bool(true)), "{steps:?}");
    assert_eq!(fixture.integration_head(), head);
    (fixture, head)
}

#[test]
fn accepting_lands_when_origin_is_a_local_bare_repository_and_pushes_it() {
    let temp = TempDir::new().expect("temp dir");
    let bare = temp.path().join("remote.git");
    let out = Command::new("git")
        .args(["init", "--bare", "-q"])
        .arg(&bare)
        .output()
        .expect("git runs");
    assert!(out.status.success());
    let (fixture, head) = accepted_task_lands_with_origin(bare.to_str().expect("utf8 path"));
    assert_eq!(
        fixture.git_in(&bare, &["rev-parse", "integration/m9"]),
        head,
        "the remote took the merge"
    );
}

#[test]
fn accepting_lands_when_origin_is_not_a_github_host() {
    // The host does not resolve and is never contacted: no `gh pr list`, so no
    // forge error, and the push to it fails quietly.
    let (_fixture, _head) =
        accepted_task_lands_with_origin("ssh://git@git.example.invalid/owner/repo.git");
}
