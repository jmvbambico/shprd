//! End-to-end for `shprd tick`: the advance pass moves a run on by itself.
//!
//! One scenario, driven through the real binary: two tasks, `B` waiting on `A`
//! with `--dep`, `[land] merge = "auto"`. The first pass starts `A` (and not
//! `B`, whose dependency has not landed); `A` hands in and is reviewed; the next
//! pass lands it under `auto`; the pass after that starts `B`.
//!
//! The forge is the scripted one `shprd land` already honours
//! (`SHPRD_FAKE_FORGE`), and the start records the attempt instead of opening a
//! terminal tab (`SHPRD_TICK_RECORD_ONLY`), so nothing here needs a multiplexer,
//! an installed agent CLI or a network.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use shprd_core::event::{Actor, Event, EventKind};
use tempfile::TempDir;

/// The binary under test, built by cargo for this integration test.
const BIN: &str = env!("CARGO_BIN_EXE_shprd");

/// The variable that points the pass at a scripted forge.
const FAKE_FORGE: &str = "SHPRD_FAKE_FORGE";

/// The variable that makes a pass record its start instead of launching a CLI.
const RECORD_ONLY: &str = "SHPRD_TICK_RECORD_ONLY";

/// The variable that names the actor, which a worker's own shell may have set.
const ACTOR_ENV: &str = "SHPRD_ACTOR";

/// A repository on `main`, a bare `origin`, a home, and a config with a policy.
struct Fixture {
    /// Keeps the temp directory alive.
    _temp: TempDir,
    /// The repository the run works on.
    root: PathBuf,
    /// The home whose `.shprd` holds worktrees, briefs and locks.
    home: PathBuf,
    /// The bare repository standing in for the remote.
    origin: PathBuf,
}

impl Fixture {
    /// A repository with one commit on `main`, pushed to a bare `origin`.
    fn new(merge: &str) -> Fixture {
        let temp = TempDir::new().expect("temp dir");
        let root = temp.path().join("repo");
        let home = temp.path().join("home");
        let origin = temp.path().join("origin.git");
        std::fs::create_dir_all(&root).expect("mkdir the repo");
        std::fs::create_dir_all(&home).expect("mkdir the home");
        let fixture = Fixture {
            _temp: temp,
            root,
            home,
            origin,
        };
        fixture.git(&[
            "init",
            "--bare",
            fixture.origin.to_str().expect("a text path"),
        ]);
        fixture.git(&["init", "-b", "main"]);
        fixture.git(&["config", "user.name", "shprd tests"]);
        fixture.git(&["config", "user.email", "tests@example.invalid"]);
        fixture.git(&["config", "commit.gpgsign", "false"]);
        fixture.git(&[
            "remote",
            "add",
            "origin",
            fixture.origin.to_str().expect("a text path"),
        ]);
        std::fs::write(fixture.root.join("a.txt"), "one\n").expect("write the seed");
        fixture.git(&["add", "--", "a.txt"]);
        fixture.git(&["commit", "-m", "seed"]);
        fixture.git(&["push", "-u", "origin", "main"]);
        fixture.write_config(merge);
        fixture
    }

    /// Writes `.shprd/config.toml`: one running task, and no protected branch.
    ///
    /// `main` is unprotected on purpose: the default list protects it, and a
    /// protected branch always asks, which would never exercise `auto`.
    fn write_config(&self, merge: &str) {
        let dir = self.root.join(".shprd");
        std::fs::create_dir_all(&dir).expect("mkdir .shprd");
        let body = format!(
            "[gates]\nworker = [\"true\"]\n\n[tick]\nmax_running = 1\n\n[land]\nmerge = \"{merge}\"\nprotected = []\n"
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
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
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

    /// The scripted forge's file.
    fn forge_script(&self) -> PathBuf {
        self.home.join("forge.json")
    }

    /// Writes the script the forge answers from.
    fn script_forge(&self, script: &Value) {
        std::fs::write(self.forge_script(), script.to_string()).expect("write the forge script");
    }

    /// Every call the scripted forge recorded, one per line.
    fn forge_calls(&self) -> Vec<String> {
        let path = PathBuf::from(format!("{}.calls", self.forge_script().display()));
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
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
        command
            .args(&all)
            .env(FAKE_FORGE, self.forge_script().display().to_string())
            .env(RECORD_ONLY, "1")
            .env_remove(ACTOR_ENV);
        command
    }

    /// Runs the binary against this fixture.
    fn run(&self, args: &[&str]) -> Output {
        if !self.forge_script().exists() {
            self.script_forge(&serde_json::json!({}));
        }
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

    /// Runs one tick pass with `--json` and parses its document.
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

    /// Starts run `r1` on `main`.
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

    /// Adds task `id`, optionally waiting on `dep`.
    fn add_task(&self, id: &str, scope: &str, dep: Option<&str>) {
        let mut args = vec![
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
        ];
        if let Some(dep) = dep {
            args.push("--dep");
            args.push(dep);
        }
        self.ok(&args);
    }

    /// Every event in the log, oldest first.
    fn events(&self) -> Vec<Event> {
        let path = self.root.join(".shprd").join("events.jsonl");
        let text = std::fs::read_to_string(&path).expect("read the log");
        text.lines()
            .map(|line| serde_json::from_str::<Event>(line).expect("an event line"))
            .collect()
    }

    /// The attempt the log recorded for `task`, as (worktree, branch).
    fn attempt_of(&self, task: &str) -> (PathBuf, String) {
        let state = shprd_core::state::RunState::fold(&self.events()).expect("the log folds");
        let found = state
            .tasks
            .get(task)
            .and_then(|found| found.latest_attempt())
            .unwrap_or_else(|| panic!("task {task} has no attempt"));
        (PathBuf::from(&found.worktree), found.branch.clone())
    }

    /// Appends a review dispatch, which `agent review` would record.
    ///
    /// The verb opens a reviewer tab in a multiplexer, which no test here has,
    /// so the fact it would record is written directly — as `tests/land.rs` does.
    fn review_requested(&self, id: &str, reviewer: &str) {
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
                reviewer: reviewer.to_string(),
                degraded: false,
            },
        };
        let line = serde_json::to_string(&event).expect("serialize the request");
        std::fs::write(&path, format!("{text}{line}\n")).expect("append the request");
    }

    /// Drives `id` from started to ready to land: a commit, a hand-in with a
    /// passing check, an independent review and a passing review gate.
    fn hand_in(&self, id: &str) -> String {
        let (worktree, _branch) = self.attempt_of(id);
        // A file name of its own: `a.txt` and `A.txt` are one file on a
        // case-insensitive disk, which would make the commit below empty.
        let name = format!("task-{id}.txt");
        std::fs::write(worktree.join(&name), "the task's work\n").expect("write the task's file");
        self.git_in(&worktree, &["add", "--", &name]);
        self.git_in(&worktree, &["commit", "-m", "the task's work"]);
        let head = self.git_in(&worktree, &["rev-parse", "HEAD"]);
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
            &name,
            "--commit",
            &head,
        ]);
        self.review_requested(id, "c1");
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
}

/// A forge script with one pull request for `task`'s branch, open at `head`,
/// with passing checks and not merged.
///
/// The open row is what lets the pass find the pull request by the task's
/// branch — the same question `shprd land` asks — so the status below is the
/// one the tick reads.
fn forge_at(task: &str, head: &str) -> Value {
    serde_json::json!({
        "open": [{
            "number": 1,
            "url": "https://example.invalid/pull/1",
            "head": format!("shprd/r1/{task}"),
        }],
        "status": [{
            "number": 1,
            "approved": false,
            "checks_passed": true,
            "merged": false,
            "head_sha": head,
        }],
        "next_number": 1,
    })
}

/// The actions a pass reported, in order, as `action:task` pairs.
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
fn the_tick_starts_a_task_lands_it_and_then_starts_the_next() {
    let fixture = Fixture::new("auto");
    fixture.start();
    fixture.add_task("A", "task-A.txt", None);
    fixture.add_task("B", "task-B.txt", Some("A"));

    // Pass 1: A starts. B waits, because A has not landed yet — and the cap of
    // one running task would hold it back anyway.
    let steps = fixture.tick();
    assert_eq!(actions(&steps), vec!["start:A".to_string()], "{steps:?}");
    assert!(
        steps[0]["ok"].as_bool().unwrap_or(false),
        "the start worked: {steps:?}"
    );

    // A hands in and is reviewed; the forge reports its pull request open.
    let head = fixture.hand_in("A");
    fixture.script_forge(&forge_at("A", &head));

    // Pass 2: A is ready to land and the policy is `auto`, so it lands. B still
    // waits: the landing happens in this very pass, so nothing may start beside
    // it and B's dependency was unmet when the pass was planned.
    let steps = fixture.tick();
    assert_eq!(actions(&steps), vec!["land:A".to_string()], "{steps:?}");
    assert!(
        steps[0]["ok"].as_bool().unwrap_or(false),
        "the landing worked: {steps:?}"
    );
    let state = shprd_core::state::RunState::fold(&fixture.events()).expect("the log folds");
    assert_eq!(
        state.tasks.get("A").map(|task| task.status),
        Some(shprd_core::state::TaskStatus::Integrated),
        "A landed"
    );
    assert!(
        fixture
            .forge_calls()
            .iter()
            .any(|call| call.starts_with("merge 1 ")),
        "the forge was asked to merge: {:?}",
        fixture.forge_calls()
    );

    // Pass 3: A is in, so B's `--after` wait is over and it starts.
    let steps = fixture.tick();
    assert_eq!(actions(&steps), vec!["start:B".to_string()], "{steps:?}");

    // Pass 4: nothing is left to do, and a pass that found nothing says so.
    let steps = fixture.tick();
    assert!(steps.is_empty(), "the run is as far as it goes: {steps:?}");
}

#[test]
fn a_second_pass_over_the_same_state_repeats_nothing() {
    let fixture = Fixture::new("auto");
    fixture.start();
    fixture.add_task("A", "task-A.txt", None);

    let first = fixture.tick();
    assert_eq!(actions(&first), vec!["start:A".to_string()], "{first:?}");

    // The second pass must not start A again: its attempt is recorded, so it is
    // Working rather than Queued.
    let second = fixture.tick();
    assert!(
        second.is_empty(),
        "A was started once, not twice: {second:?}"
    );
    let state = shprd_core::state::RunState::fold(&fixture.events()).expect("the log folds");
    let attempts = state
        .tasks
        .get("A")
        .map(|task| task.attempts.len())
        .unwrap_or(0);
    assert_eq!(attempts, 1, "A has one attempt");
}

#[test]
fn a_needs_you_is_recorded_once_and_never_repeated() {
    let fixture = Fixture::new("ask");
    fixture.start();
    fixture.add_task("A", "task-A.txt", None);
    fixture.tick();
    let head = fixture.hand_in("A");
    fixture.script_forge(&forge_at("A", &head));

    let first = fixture.tick();
    assert_eq!(
        actions(&first),
        vec!["needs_you:A".to_string()],
        "the ask policy says so once: {first:?}"
    );
    let second = fixture.tick();
    assert!(
        second.is_empty(),
        "the ask was already recorded: {second:?}"
    );
    let asked = fixture
        .events()
        .iter()
        .filter(|event| {
            matches!(
                &event.kind,
                EventKind::Escalated { task: Some(task), .. } if task == "A"
            )
        })
        .count();
    assert_eq!(asked, 1, "one escalation, not one per pass");
    assert!(
        !fixture
            .forge_calls()
            .iter()
            .any(|call| call.starts_with("merge ")),
        "an ask policy never merges: {:?}",
        fixture.forge_calls()
    );
}

#[test]
fn a_pull_request_merged_outside_shprd_is_recorded_as_landed() {
    let fixture = Fixture::new("ask");
    fixture.start();
    fixture.add_task("A", "task-A.txt", None);
    fixture.add_task("B", "task-B.txt", Some("A"));
    fixture.tick();
    let head = fixture.hand_in("A");
    let mut script = forge_at("A", &head);
    script["status"][0]["merged"] = serde_json::json!(true);
    fixture.script_forge(&script);

    let steps = fixture.tick();
    assert_eq!(
        actions(&steps),
        vec!["mark_landed:A".to_string()],
        "{steps:?}"
    );
    assert!(
        steps[0]["ok"].as_bool().unwrap_or(false),
        "the record worked: {steps:?}"
    );
    let state = shprd_core::state::RunState::fold(&fixture.events()).expect("the log folds");
    assert_eq!(
        state.tasks.get("A").map(|task| task.status),
        Some(shprd_core::state::TaskStatus::Integrated)
    );
    // The merge was somebody else's, so the pass never asked the forge to make
    // one: it only read.
    assert!(
        !fixture
            .forge_calls()
            .iter()
            .any(|call| call.starts_with("merge ") || call.starts_with("create ")),
        "nothing was merged or opened: {:?}",
        fixture.forge_calls()
    );
}

#[test]
fn the_pass_prints_a_document_a_script_can_read() {
    let fixture = Fixture::new("auto");
    fixture.start();
    fixture.add_task("A", "task-A.txt", None);
    let text = fixture.ok(&["tick", "--once", "--json"]);
    let value: Value = serde_json::from_str(&text).expect("the document parses");
    let steps = value["steps"].as_array().expect("steps is a list");
    assert_eq!(steps.len(), 1);
    let step = &steps[0];
    for key in ["action", "task", "ok", "detail"] {
        assert!(step.get(key).is_some(), "a step carries {key}: {step}");
    }
    assert_eq!(step["action"], "start");
    assert_eq!(step["task"], "A");
    assert!(step["seq"].as_u64().is_some(), "the log line is named");

    // The human line, without `--json`, says the same thing in one line.
    let line = fixture.ok(&["tick", "--once"]);
    assert!(line.starts_with("ok nothing to advance"), "{line:?}");
}
