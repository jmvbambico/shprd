//! Replays the jmanager T1 run (the 2026-11-23 incident) against the current
//! engine with a scripted forge and no host: the task's branch merges a main
//! that moved by 130 files, and still lands with one Accept, no scope card and
//! no human terminal step.
//!
//! `fixtures/t1/events.jsonl` is the T1 log reduced to the events that drive the
//! engine (the run start, the task, attempt 2's spawn, report, gates and the one
//! Accept), with the repository and worktree paths made relative.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use shprd_core::event::{Event, EventKind, LandOutcome};
use tempfile::TempDir;

mod support;

const BIN: &str = env!("CARGO_BIN_EXE_shprd");

/// The branch the layout gives T1 in run `r1`.
const BRANCH: &str = "shprd/r1/T1";

/// The reduced T1 events.
fn fixture() -> Vec<Value> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/t1/events.jsonl");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    text.lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("bad fixture line: {e}")))
        .collect()
}

/// A repository, a bare origin and a home.
struct Rig {
    _temp: TempDir,
    root: PathBuf,
    home: PathBuf,
    forge: PathBuf,
}

impl Rig {
    fn new() -> Rig {
        let temp = TempDir::new().unwrap_or_else(|e| panic!("tempdir: {e}"));
        let root = temp.path().join("repo");
        let home = temp.path().join("home");
        let origin = temp.path().join("origin.git");
        for dir in [&root, &home] {
            std::fs::create_dir_all(dir).unwrap_or_else(|e| panic!("mkdir: {e}"));
        }
        let rig = Rig {
            forge: home.join("forge.json"),
            _temp: temp,
            root,
            home,
        };
        git(
            &rig.root,
            &["init", "--bare", origin.to_str().unwrap_or("")],
        );
        git(&rig.root, &["init", "-b", "main"]);
        git(&rig.root, &["config", "user.name", "shprd tests"]);
        git(
            &rig.root,
            &["config", "user.email", "tests@example.invalid"],
        );
        git(&rig.root, &["config", "commit.gpgsign", "false"]);
        git(
            &rig.root,
            &["remote", "add", "origin", origin.to_str().unwrap_or("")],
        );
        write(
            &rig.root.join(".shprd/config.toml"),
            "[gates]\nworker = [\"true\"]\n\n[land]\nmerge = \"ask\"\n",
        );
        rig
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut command = Command::new(BIN);
        support::isolate(&mut command, &self.home);
        command
            .args(["--repo", &self.root.display().to_string()])
            .args(["--home", &self.home.display().to_string(), "--no-dagr"])
            .args(args)
            .env("SHPRD_FAKE_FORGE", &self.forge)
            .env_remove("SHPRD_ACTOR");
        command
            .output()
            .unwrap_or_else(|e| panic!("run {BIN}: {e}"))
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "{args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    /// Records the board's Accept for `task`: the `task_approved` event the
    /// desktop commits when the person accepts, pinned to `head`.
    fn accept(&self, task: &str, number: u64, head: &str) {
        let path = self.root.join(".shprd/events.jsonl");
        let mut text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read log: {e}"));
        let seq = text.lines().count() as u64 + 1;
        let event = Event {
            seq,
            at_ms: 1_700_000_100_000 + seq,
            actor: shprd_core::event::Actor::Human,
            kind: EventKind::TaskApproved {
                task: task.to_string(),
                number,
                head_sha: head.to_string(),
            },
        };
        text.push_str(&serde_json::to_string(&event).unwrap_or_default());
        text.push('\n');
        std::fs::write(&path, text).unwrap_or_else(|e| panic!("write log: {e}"));
    }

    fn events(&self) -> Vec<Event> {
        let text = std::fs::read_to_string(self.root.join(".shprd/events.jsonl"))
            .unwrap_or_else(|e| panic!("read log: {e}"));
        text.lines()
            .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("bad event: {e}")))
            .collect()
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn write(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap_or_else(|e| panic!("mkdir: {e}"));
    }
    std::fs::write(path, body).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

fn strings(value: &Value, key: &str) -> Vec<String> {
    value[key]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn the_t1_run_replays_to_landed_with_one_accept_and_no_scope_card() {
    let events = fixture();
    let find =
        |kind: &str| -> Vec<&Value> { events.iter().filter(|e| e["kind"] == kind).collect() };
    let added = find("task_added")[0];
    let reported = find("reported")[0];
    let changed = strings(reported, "changed_paths");
    assert_eq!(changed.len(), 10, "T1's real change is ten files");
    assert_eq!(find("task_approved").len(), 1, "the fixture has one Accept");

    let rig = Rig::new();
    // The repository as T1 found it: every file it would touch exists.
    for path in &changed {
        write(&rig.root.join(path), "v0\n");
    }
    git(&rig.root, &["add", "-A"]);
    git(&rig.root, &["commit", "-m", "seed"]);
    git(&rig.root, &["switch", "--detach"]);
    rig.ok(&[
        "run",
        "start",
        "--run-id",
        "r1",
        "--base",
        "main",
        "--integration",
        "main",
    ]);

    // `task_added`
    let mut add: Vec<String> = ["agent", "task", "add", "--id", "T1", "--title"]
        .map(String::from)
        .to_vec();
    add.push(added["title"].as_str().unwrap_or("").to_string());
    for path in strings(added, "scope") {
        add.extend(["--scope".to_string(), path]);
    }
    for item in strings(added, "acceptance") {
        add.extend(["--acceptance".to_string(), item]);
    }
    rig.ok(&add.iter().map(String::as_str).collect::<Vec<_>>());

    // `spawned`, on a real worktree cut from the run's base.
    let layout = shprd_git::worktree::Layout::default_for_home(&rig.home);
    let worktree = shprd_git::worktree::create(&rig.root, &layout, "r1", "T1", "main")
        .unwrap_or_else(|e| panic!("worktree: {e}"));
    assert_eq!(worktree.branch, BRANCH);
    let wt = worktree.path;
    rig.ok(&[
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
        &wt.display().to_string(),
        "--branch",
        BRANCH,
    ]);

    // The incident: main gains 130 files the task never touched, and the task
    // merges it.
    for n in 0..130 {
        write(&rig.root.join(format!("moved/f{n}.txt")), "from main\n");
    }
    git(&rig.root, &["add", "-A"]);
    git(&rig.root, &["commit", "-m", "main moves on"]);
    git(&wt, &["config", "user.name", "shprd tests"]);
    git(&wt, &["config", "user.email", "tests@example.invalid"]);
    git(&wt, &["merge", "main", "--no-edit"]);
    for path in &changed {
        write(&wt.join(path), "v1 by T1\n");
    }
    git(&wt, &["add", "-A"]);
    git(&wt, &["commit", "-m", "T1's work"]);
    let head = git(&wt, &["rev-parse", "HEAD"]);

    // `reported`, with the fixture's own paths and checks.
    let mut report: Vec<String> = [
        "agent",
        "report",
        "--task",
        "T1",
        "--attempt",
        "1",
        "--status",
        "completed",
        "--summary",
    ]
    .map(String::from)
    .to_vec();
    report.push(reported["summary"].as_str().unwrap_or("").to_string());
    for check in reported["checks"].as_array().cloned().unwrap_or_default() {
        report.extend([
            "--check".to_string(),
            format!(
                "{}|{}|{}",
                check["gate"].as_str().unwrap_or(""),
                check["command"].as_str().unwrap_or(""),
                check["exit_code"]
            ),
        ]);
    }
    for path in &changed {
        report.extend(["--changed".to_string(), path.clone()]);
    }
    report.extend(["--commit".to_string(), head.clone()]);
    let report_args: Vec<&str> = report.iter().map(String::as_str).collect();
    rig.ok(&report_args);

    // The engine's review: a request, then a passing verdict.
    support::pass_review(&rig.root, "T1");

    // `shprd land T1` opens the pull request and waits for the one Accept.
    let script = |approved: bool| {
        serde_json::json!({
            "open": [{"number": 1, "url": "https://example.invalid/pull/1", "head": BRANCH}],
            "status": [{"number": 1, "approved": approved, "checks_passed": true, "merged": false, "head_sha": head}],
        })
        .to_string()
    };
    write(&rig.forge, &script(false));
    let waiting = rig.ok(&["land", "T1"]);
    assert!(waiting.contains("waiting for your approval"), "{waiting}");
    // The one Accept is the person's click on the board, which commits
    // `task_approved` to the log; no terminal step is involved.
    rig.accept("T1", 1, &head);
    rig.ok(&["land", "T1"]);

    let log = rig.events();
    let landed: Vec<_> = log
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::TaskLanded { task, outcome, .. } if task == "T1" => Some(*outcome),
            _ => None,
        })
        .collect();
    assert_eq!(landed, vec![LandOutcome::Merged], "T1 reached Landed");
    let accepts = log
        .iter()
        .filter(|e| matches!(&e.kind, EventKind::TaskApproved { task, .. } if task == "T1"))
        .count();
    assert_eq!(accepts, 1, "one Accept, not eight");
    let cards = log
        .iter()
        .filter(|e| {
            matches!(
                e.kind,
                EventKind::AskRaised { .. }
                    | EventKind::Escalated { .. }
                    | EventKind::ScopeWidened { .. }
            )
        })
        .count();
    assert_eq!(cards, 0, "no scope card, widening or escalation: {log:#?}");
}
