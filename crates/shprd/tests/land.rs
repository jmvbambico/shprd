//! End-to-end tests for `shprd land`: one task's own pull request, and the
//! run-level pull request the integration branch used to land from. They drive
//! the real binary against a throwaway repository whose `origin` is a local bare
//! repository, with a scripted forge standing in for `gh` through
//! `SHPRD_FAKE_FORGE`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use shprd_core::event::{Actor, Event, EventKind};
use tempfile::TempDir;

/// The binary under test, built by cargo for this integration test.
const BIN: &str = env!("CARGO_BIN_EXE_shprd");

/// The environment variable that points `land` at a scripted forge.
const FAKE_FORGE: &str = "SHPRD_FAKE_FORGE";

/// The environment variable that names the actor.
///
/// These tests exercise the human path, so a worker's own `SHPRD_ACTOR` must
/// not leak into the binary and make it think a human is an agent. A test that
/// wants an agent actor sets it explicitly on its own command.
const ACTOR_ENV: &str = "SHPRD_ACTOR";

/// The branch the run pushes.
const INT_BRANCH: &str = "shprd/r1/int";

/// A repository, a bare `origin`, a home, and the temp directory holding them.
struct Fixture {
    /// Keeps the temp directory alive.
    _temp: TempDir,
    /// The repository the run works on.
    root: PathBuf,
    /// The home whose `.shprd` holds worktrees and locks.
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

/// Writes a file, panicking on failure.
fn write(path: &Path, body: &str) {
    if let Err(err) = std::fs::write(path, body) {
        panic!("write {}: {err}", path.display());
    }
}

impl Fixture {
    /// A repository on `main` with a seed commit, and an empty bare origin.
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
        fixture.write_config("ask", None);
        fixture
    }

    /// Runs git in the repository, asserting success, and returns its stdout.
    fn git(&self, args: &[&str]) -> String {
        let output = match Command::new("git")
            .args(args)
            .current_dir(&self.root)
            .output()
        {
            Ok(output) => output,
            Err(err) => panic!("git {args:?}: {err}"),
        };
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    /// Writes `.shprd/config.toml` with the givens.
    fn write_config(&self, merge: &str, integration: Option<&str>) {
        let dir = self.root.join(".shprd");
        mkdir(&dir);
        let gate = integration.unwrap_or("true");
        let body = format!(
            "[gates]\nworker = [\"true\"]\nintegration = [\"{gate}\"]\n\n[land]\nmerge = \"{merge}\"\n"
        );
        write(&dir.join("config.toml"), &body);
    }

    /// The scripted forge's file.
    fn forge_script(&self) -> PathBuf {
        self.home.join("forge.json")
    }

    /// Scripts the forge: `open`/`status` JSON, written before each call.
    fn script_forge(&self, script: &str) {
        write(&self.forge_script(), script);
    }

    /// Everything the scripted forge was asked by the most recent run, one call
    /// per line: each process starts from its script and rewrites the journal.
    fn forge_calls(&self) -> Vec<String> {
        let path = PathBuf::from(format!("{}.calls", self.forge_script().display()));
        match std::fs::read_to_string(&path) {
            Ok(text) => text.lines().map(str::to_string).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// One command against this fixture, with the scripted forge in place.
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
            .env_remove(ACTOR_ENV);
        command
    }

    /// Runs the binary against this fixture.
    fn run(&self, args: &[&str]) -> Output {
        if !self.forge_script().exists() {
            self.script_forge("{}");
        }
        match self.command(args).output() {
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

    /// Starts run `r1`, cut from `main`, integrating on `main`.
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

    /// Adds task `id` and leaves it working: spawned, never reported.
    fn working_task(&self, id: &str) {
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
    }

    /// Appends a recorded review dispatch to the run log.
    ///
    /// `shprd agent review` opens a reviewer tab in a multiplexer, which no
    /// test here has, so the fact it would record is written directly.
    fn review_requested(&self, id: &str, reviewer: &str, degraded: bool) {
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
                reviewer: reviewer.to_string(),
                degraded,
            },
        };
        let line = match serde_json::to_string(&event) {
            Ok(line) => line,
            Err(err) => panic!("serialize the review request: {err}"),
        };
        write(&path, &format!("{text}{line}\n"));
    }

    /// Drives task `id` from added to passed, with an independent review.
    fn passed_task(&self, id: &str) {
        self.passed_task_with(id, false);
    }

    /// Drives task `id` from added to passed, with a review that is degraded or
    /// not, and a passing `review` gate.
    fn passed_task_with(&self, id: &str, degraded: bool) {
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
        self.review_requested(id, "c1", degraded);
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
        // The reviewer writes its packet; the engine decides from it.
        let packet = self
            .home
            .join(".shprd")
            .join("reviews")
            .join("r1")
            .join(format!("{id}-a1"));
        if let Err(err) = std::fs::create_dir_all(&packet) {
            panic!("mkdir {}: {err}", packet.display());
        }
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
    }

    /// Commits one rewrite of `file` on `branch`, cut from `main`.
    fn commit_on(&self, branch: &str, file: &str, body: &str, message: &str) {
        self.git(&["checkout", "-b", branch, "main"]);
        write(&self.root.join(file), body);
        self.git(&["add", "--", file]);
        self.git(&["commit", "-m", message]);
        self.git(&["checkout", "main"]);
    }

    /// Passes `id`, lands its branch, then runs the full gate.
    fn land_a_task(&self, id: &str) {
        self.land_a_task_with(id, false);
    }

    /// Lands `id` after a review that is degraded or not.
    fn land_a_task_with(&self, id: &str, degraded: bool) {
        self.passed_task_with(id, degraded);
        self.commit_on("feature/t1", "a.txt", "feature\n", "the task's work");
        self.ok(&["agent", "integrate", "--task", id]);
        self.ok(&["gate"]);
    }

    /// The integration worktree the run owns.
    fn integration(&self) -> PathBuf {
        shprd_git::worktree::Layout::default_for_home(&self.home).path("repo", "r1", "_int")
    }

    /// The integration worktree's head.
    fn integration_head(&self) -> String {
        self.git_in(&self.integration(), &["rev-parse", "HEAD"])
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

    /// The bare origin's head for `branch`, or `None` when it is absent.
    fn origin_head(&self, branch: &str) -> Option<String> {
        let output = match Command::new("git")
            .args(["rev-parse", "--verify", &format!("refs/heads/{branch}")])
            .current_dir(&self.origin)
            .output()
        {
            Ok(output) => output,
            Err(err) => panic!("git rev-parse in the origin: {err}"),
        };
        if !output.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
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

    /// The pull request the log recorded opening, if any.
    fn opened_pr(&self) -> Option<(u64, String)> {
        for event in self.events().into_iter().rev() {
            if let EventKind::PrOpened { number, url, .. } = event.kind {
                return Some((number, url));
            }
        }
        None
    }

    /// The head the log's last approval named, if any.
    fn approved_head(&self) -> Option<String> {
        for event in self.events().into_iter().rev() {
            if let EventKind::Approved { head_sha, .. } = event.kind {
                return Some(head_sha);
            }
        }
        None
    }

    /// True when the log recorded a merge of `number`.
    fn recorded_merge(&self, number: u64) -> bool {
        self.events().iter().any(
            |event| matches!(&event.kind, EventKind::PrMerged { number: n, .. } if *n == number),
        )
    }

    /// Writes `[land]` with `merge`, protecting exactly `protected`.
    ///
    /// The per-task tests need `main` unprotected to exercise an automatic
    /// merge, which the default list protects on purpose.
    fn write_land_config(&self, merge: &str, protected: &[&str]) {
        let dir = self.root.join(".shprd");
        mkdir(&dir);
        let list = protected
            .iter()
            .map(|name| format!("\"{name}\""))
            .collect::<Vec<_>>()
            .join(", ");
        write(
            &dir.join("config.toml"),
            &format!(
                "[gates]\nworker = [\"true\"]\n\n[land]\nmerge = \"{merge}\"\nprotected = [{list}]\n"
            ),
        );
    }

    /// A real worktree for `task` on `branch`, cut from `main`.
    fn task_worktree(&self, task: &str, branch: &str) -> PathBuf {
        let layout = shprd_git::worktree::Layout::default_for_home(&self.home);
        match shprd_git::worktree::create(&self.root, &layout, "r1", task, "main") {
            Ok(worktree) => {
                assert_eq!(worktree.branch, branch, "the layout names the branch");
                worktree.path
            }
            Err(err) => panic!("creating the task's worktree: {err}"),
        }
    }

    /// A task that reported a commit, passed a gate and was reviewed
    /// independently, with its attempt working in `worktree` on `branch`.
    fn passed_task_in(&self, id: &str, worktree: &Path, branch: &str) {
        let path = worktree.display().to_string();
        let head = self.git_in(worktree, &["rev-parse", "HEAD"]);
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
            &path,
            "--branch",
            branch,
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
            "--commit",
            &head,
        ]);
        self.review_requested(id, "c1", false);
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
        if let Err(err) = std::fs::create_dir_all(&packet) {
            panic!("mkdir {}: {err}", packet.display());
        }
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
    }

    /// A task with work to land: its own worktree with one commit, reported and
    /// reviewed. Returns the worktree and its head.
    fn landable_task(&self, id: &str, branch: &str) -> (PathBuf, String) {
        let worktree = self.task_worktree(id, branch);
        write(&worktree.join("a.txt"), "the task's work\n");
        self.git_in(&worktree, &["add", "--", "a.txt"]);
        self.git_in(&worktree, &["commit", "-m", "the task's work"]);
        let head = self.git_in(&worktree, &["rev-parse", "HEAD"]);
        self.passed_task_in(id, &worktree, branch);
        (worktree, head)
    }

    /// The outcomes the log recorded for `task`, oldest first.
    fn landed_outcomes(&self, task: &str) -> Vec<EventKind> {
        self.events()
            .into_iter()
            .filter(
                |event| matches!(&event.kind, EventKind::TaskLanded { task: t, .. } if t == task),
            )
            .map(|event| event.kind)
            .collect()
    }
}

/// A forge script for one task's pull request: #1 at `head`, optionally already
/// open for `branch`.
fn task_script(branch: &str, head: &str, checks: Option<bool>, open: bool) -> String {
    let mut script = serde_json::json!({
        "status": [{
            "number": 1,
            "approved": false,
            "checks_passed": checks,
            "merged": false,
            "head_sha": head,
        }],
    });
    if open {
        script["open"] = serde_json::json!([{
            "number": 1,
            "url": "https://example.invalid/pull/1",
            "head": branch,
        }]);
    }
    script.to_string()
}

/// The two vars a per-task landing test starts from: a run cut from `main`,
/// a landable task on its own branch, and the config the policy needs.
fn landing_task_fixture(merge: &str) -> (Fixture, String) {
    let fixture = Fixture::new();
    fixture.write_land_config(merge, &[]);
    fixture.start();
    let (_, head) = fixture.landable_task("T1", "shprd/r1/T1");
    (fixture, head)
}

/// The calls the scripted forge recorded that start with `prefix`.
fn calls_with(calls: &[String], prefix: &str) -> Vec<String> {
    calls
        .iter()
        .filter(|call| call.starts_with(prefix))
        .cloned()
        .collect()
}

/// The body file of the last `create` or `edit_body` the scripted forge saw.
fn body_of(call: &str) -> PathBuf {
    let path = match call.rsplit_once(' ') {
        Some((_, path)) => path,
        None => panic!("a forge call with no body file: {call}"),
    };
    PathBuf::from(path)
}

/// A forge script whose pull request #1 has `status`.
fn status_script(head: &str, approved: bool, checks: Option<bool>) -> String {
    let script = serde_json::json!({
        "status": [{
            "number": 1,
            "url": "https://example.invalid/pull/1",
            "head": INT_BRANCH,
            "approved": approved,
            "checks_passed": checks,
            "merged": false,
            "head_sha": head,
        }],
    });
    script.to_string()
}

#[test]
fn every_precondition_refuses_and_writes_no_pull_request() {
    let fixture = Fixture::new();
    let message = fixture.refused(&["land"]);
    assert!(message.contains("no run yet"), "{message}");

    fixture.start();
    let message = fixture.refused(&["land"]);
    assert!(message.contains("no task is integrated"), "{message}");

    let busy = Fixture::new();
    busy.start();
    busy.working_task("T1");
    let message = busy.refused(&["land"]);
    assert!(message.contains("is Working"), "{message}");

    let ungated = Fixture::new();
    ungated.start();
    ungated.passed_task("T1");
    ungated.commit_on("feature/t1", "a.txt", "feature\n", "the task's work");
    ungated.ok(&["agent", "integrate", "--task", "T1"]);
    let message = ungated.refused(&["land"]);
    assert!(message.contains("no full gate has run"), "{message}");

    assert!(
        fixture.forge_calls().is_empty(),
        "a refused land asks the forge nothing"
    );
}

#[test]
fn a_stale_gate_is_refused_after_the_head_moves() {
    let fixture = Fixture::new();
    fixture.start();
    fixture.land_a_task("T1");
    // A commit after the gate moves the integration head past the one it ran on.
    let worktree = fixture.integration();
    fixture.git_in(
        &worktree,
        &["commit", "--allow-empty", "-m", "after the gate"],
    );
    fixture.script_forge("{}");
    let message = fixture.refused(&["land"]);
    assert!(message.contains("run `shprd gate` again"), "{message}");
    assert!(fixture.opened_pr().is_none(), "nothing was opened");
}

#[test]
fn a_full_run_pushes_the_branch_and_opens_one_pull_request() {
    let fixture = Fixture::new();
    fixture.start();
    fixture.land_a_task("T1");
    fixture.script_forge("{}");
    let head = fixture.integration_head();

    let printed = fixture.ok(&["land"]);
    assert!(
        printed.contains("https://example.invalid/pull/1"),
        "the url is printed: {printed}"
    );
    assert_eq!(
        fixture.origin_head(INT_BRANCH).as_deref(),
        Some(head.as_str()),
        "the branch reached the bare origin"
    );
    let calls = fixture.forge_calls();
    let created = calls_with(&calls, "create ");
    assert_eq!(created.len(), 1, "{calls:?}");
    assert!(
        created[0].starts_with("create main shprd/r1/int r1: a task "),
        "{calls:?}"
    );
    assert_eq!(
        calls_with(&calls, "edit_body ").len(),
        0,
        "nothing to edit yet: {calls:?}"
    );
    assert_eq!(
        fixture.opened_pr(),
        Some((1, "https://example.invalid/pull/1".to_string()))
    );

    // A second land refreshes the same pull request instead of opening another,
    // which is what the forge reports once the first one exists.
    fixture.script_forge(
        r#"{"open":[{"number":1,"url":"https://example.invalid/pull/1","head":"shprd/r1/int"}]}"#,
    );
    fixture.ok(&["land"]);
    let calls = fixture.forge_calls();
    assert_eq!(
        calls_with(&calls, "create ").len(),
        0,
        "the second land must not open a second pull request: {calls:?}"
    );
    let edits = calls_with(&calls, "edit_body ");
    assert_eq!(edits.len(), 1, "{calls:?}");
    assert!(edits[0].starts_with("edit_body 1 "), "{calls:?}");
    let events = fixture.events();
    let opens = events
        .iter()
        .filter(|event| matches!(event.kind, EventKind::PrOpened { .. }))
        .count();
    assert_eq!(opens, 2, "each land records what the forge holds");
    let pushed = events
        .iter()
        .filter(|event| {
            matches!(
                &event.kind,
                EventKind::Resource {
                    action: shprd_core::event::ResourceAction::Created,
                    kind: shprd_core::event::ResourceKind::RemoteBranch,
                    ..
                }
            )
        })
        .count();
    assert_eq!(
        pushed, 1,
        "the remote branch is recorded once, not per land"
    );
}

#[test]
fn the_body_records_the_review_marker_and_the_attribution() {
    let fixture = Fixture::new();
    fixture.start();
    fixture.land_a_task("T1");
    fixture.script_forge("{}");
    fixture.ok(&["land"]);
    let calls = fixture.forge_calls();
    let opened = match calls_with(&calls, "create ").first() {
        Some(call) => call.clone(),
        None => panic!("no create call: {calls:?}"),
    };
    let body = match std::fs::read_to_string(body_of(&opened)) {
        Ok(body) => body,
        Err(err) => panic!("read the body: {err}"),
    };
    assert!(body.contains("## Tasks"), "{body}");
    assert!(body.contains("cross-vendor-review: passed"), "{body}");
    assert!(body.contains("https://claude.com/claude-code"), "{body}");

    let degraded = Fixture::new();
    degraded.start();
    degraded.land_a_task_with("T1", true);
    degraded.script_forge("{}");
    degraded.ok(&["land"]);
    let calls = degraded.forge_calls();
    let opened = match calls_with(&calls, "create ").first() {
        Some(call) => call.clone(),
        None => panic!("no create call: {calls:?}"),
    };
    let body = match std::fs::read_to_string(body_of(&opened)) {
        Ok(body) => body,
        Err(err) => panic!("read the body: {err}"),
    };
    assert!(body.contains("degraded-review"), "{body}");
    assert!(!body.contains("cross-vendor-review: passed"), "{body}");
}

/// A fixture with one landed task and its pull request open.
fn landed() -> Fixture {
    let fixture = Fixture::new();
    fixture.start();
    fixture.land_a_task("T1");
    fixture.script_forge("{}");
    fixture.ok(&["land"]);
    fixture
}

#[test]
fn merge_without_an_opened_pull_request_is_refused() {
    let fixture = Fixture::new();
    fixture.start();
    fixture.land_a_task("T1");
    fixture.script_forge("{}");
    let message = fixture.refused(&["land", "--merge"]);
    assert!(message.contains("no pull request yet"), "{message}");
}

#[test]
fn merge_waits_for_approval_and_merges_once_a_human_approves() {
    let fixture = landed();
    let head = fixture.integration_head();

    fixture.script_forge(&status_script(&head, false, Some(true)));
    let message = fixture.refused(&["land", "--merge"]);
    assert!(
        message.contains("waiting for your approval: run `shprd approve`"),
        "{message}"
    );
    assert!(calls_with(&fixture.forge_calls(), "merge ").is_empty());

    fixture.script_forge(&status_script(&head, true, Some(true)));
    let printed = fixture.ok(&["land", "--merge"]);
    assert!(
        printed.contains("merged https://example.invalid/pull/1"),
        "{printed}"
    );
    assert_eq!(
        calls_with(&fixture.forge_calls(), "merge "),
        vec![format!("merge 1 {head}")],
        "the merge names the commit it merges"
    );
    assert!(fixture.recorded_merge(1), "the merge is in the log");
}

#[test]
fn approve_records_the_human_approval_at_the_integration_head() {
    let fixture = landed();
    let head = fixture.integration_head();
    let printed = fixture.ok(&["approve", "--yes"]);
    assert!(
        printed.contains("approved https://example.invalid/pull/1"),
        "{printed}"
    );
    assert_eq!(fixture.approved_head(), Some(head));
}

#[test]
fn approve_without_a_terminal_and_without_yes_is_refused() {
    let fixture = landed();
    let message = fixture.refused(&["approve"]);
    assert!(message.contains("stdin is not a terminal"), "{message}");
    assert_eq!(fixture.approved_head(), None);
}

#[test]
fn approve_is_refused_for_an_agent_actor_even_with_yes() {
    let fixture = landed();
    let output = match fixture
        .command(&["approve", "--yes"])
        .env("SHPRD_ACTOR", "orchestrator")
        .output()
    {
        Ok(output) => output,
        Err(err) => panic!("running the binary: {err}"),
    };
    assert_eq!(output.status.code(), Some(2));
    let message = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(message.contains("only a human approves"), "{message}");
    assert_eq!(fixture.approved_head(), None, "nothing was written");
}

#[test]
fn approve_without_a_pull_request_is_refused() {
    let fixture = Fixture::new();
    fixture.start();
    fixture.land_a_task("T1");
    let message = fixture.refused(&["approve", "--yes"]);
    assert!(message.contains("no pull request yet"), "{message}");
}

#[test]
fn a_shprd_approval_at_the_head_merges() {
    let fixture = landed();
    let head = fixture.integration_head();
    fixture.ok(&["approve", "--yes"]);
    // The forge still calls the pull request unapproved: its author cannot
    // approve it there, which is the whole point of `shprd approve`.
    fixture.script_forge(&status_script(&head, false, Some(true)));
    let printed = fixture.ok(&["land", "--merge"]);
    assert!(
        printed.contains("merged https://example.invalid/pull/1"),
        "{printed}"
    );
    assert_eq!(
        calls_with(&fixture.forge_calls(), "merge "),
        vec![format!("merge 1 {head}")],
        "the merge names the commit it merges"
    );
    assert!(fixture.recorded_merge(1), "the merge is in the log");
}

#[test]
fn an_approval_of_an_older_head_still_waits() {
    let fixture = landed();
    fixture.ok(&["approve", "--yes"]);
    let approved = fixture.integration_head();
    let worktree = fixture.integration();
    fixture.git_in(
        &worktree,
        &["commit", "--allow-empty", "-m", "after the approval"],
    );
    fixture.ok(&["gate"]);
    let head = fixture.integration_head();
    assert_ne!(head, approved, "the head moved past the approval");
    fixture.script_forge(&status_script(&head, false, Some(true)));
    let message = fixture.refused(&["land", "--merge"]);
    assert!(message.contains("waiting for your approval"), "{message}");
    assert!(calls_with(&fixture.forge_calls(), "merge ").is_empty());
    assert!(!fixture.recorded_merge(1));
}

#[test]
fn checks_that_have_not_passed_are_refused() {
    let fixture = landed();
    let head = fixture.integration_head();
    for checks in [None, Some(false)] {
        fixture.script_forge(&status_script(&head, true, checks));
        let message = fixture.refused(&["land", "--merge"]);
        assert!(message.contains("checks have not passed"), "{message}");
    }
    assert!(calls_with(&fixture.forge_calls(), "merge ").is_empty());
}

#[test]
fn a_stale_head_is_refused() {
    let fixture = landed();
    fixture.script_forge(&status_script("deadbeef", true, Some(true)));
    let message = fixture.refused(&["land", "--merge"]);
    assert!(message.contains("is at head deadbeef"), "{message}");
    assert!(!fixture.recorded_merge(1));
}

#[test]
fn auto_never_merges_into_a_protected_base() {
    let fixture = landed();
    fixture.write_config("auto", None);
    let head = fixture.integration_head();
    fixture.script_forge(&status_script(&head, false, Some(true)));
    let message = fixture.refused(&["land", "--merge"]);
    assert!(
        message.contains("waiting for your approval: run `shprd approve`"),
        "main is protected, so auto still waits: {message}"
    );
    assert!(!fixture.recorded_merge(1));
}

/// A forge script whose pull request #1 is already merged on the forge.
fn merged_script(head: &str) -> String {
    let script = serde_json::json!({
        "status": [{
            "number": 1,
            "url": "https://example.invalid/pull/1",
            "head": INT_BRANCH,
            "approved": true,
            "checks_passed": Some(true),
            "merged": true,
            "head_sha": head,
        }],
    });
    script.to_string()
}

/// Every `PrMerged` head the log recorded, oldest first.
fn merged_heads(fixture: &Fixture) -> Vec<String> {
    fixture
        .events()
        .into_iter()
        .filter_map(|event| match event.kind {
            EventKind::PrMerged { head, .. } => Some(head),
            _ => None,
        })
        .collect()
}

#[test]
fn a_merge_records_the_branch_so_the_janitor_deletes_it_remotely() {
    let fixture = landed();
    let head = fixture.integration_head();
    fixture.script_forge(&status_script(&head, true, Some(true)));
    fixture.ok(&["land", "--merge"]);
    assert_eq!(
        merged_heads(&fixture),
        vec![INT_BRANCH.to_string()],
        "the merge names the head branch, not its sha"
    );
    let plan = fixture.ok(&["janitor"]);
    assert!(
        plan.contains(&format!("delete remote branch {INT_BRANCH}")),
        "the janitor plans deleting the merged branch remotely: {plan}"
    );
}

#[test]
fn a_merge_done_on_the_forge_is_recorded_once() {
    let fixture = landed();
    let head = fixture.integration_head();
    fixture.script_forge(&merged_script(&head));
    let printed = fixture.ok(&["land", "--merge"]);
    assert!(
        printed.contains("merged https://example.invalid/pull/1"),
        "{printed}"
    );
    assert_eq!(
        merged_heads(&fixture),
        vec![INT_BRANCH.to_string()],
        "the merge GitHub already did is in the log"
    );
    assert!(
        calls_with(&fixture.forge_calls(), "merge ").is_empty(),
        "the forge is not asked for a second merge"
    );
    fixture.ok(&["land", "--merge"]);
    assert_eq!(
        merged_heads(&fixture).len(),
        1,
        "a second land records no second merge"
    );
}

#[test]
fn an_outside_merge_counts_only_at_the_integration_head() {
    let fixture = landed();
    let head = fixture.integration_head();
    let stale = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
    fixture.script_forge(&merged_script(stale));
    let message = fixture.refused(&["land", "--merge"]);
    assert!(
        message.contains(
            "the pull request was merged at deadbee, but the integration branch is now at "
        ) && message.contains(&head[..7]),
        "{message}"
    );
    assert!(
        message.contains("open a new pull request with `shprd land`"),
        "{message}"
    );
    assert!(
        merged_heads(&fixture).is_empty(),
        "a merge at an older head is not in the log"
    );

    let fresh = landed();
    let head = fresh.integration_head();
    fresh.script_forge(&merged_script(&head));
    fresh.ok(&["land", "--merge"]);
    assert_eq!(
        merged_heads(&fresh),
        vec![INT_BRANCH.to_string()],
        "a merge at the integration head is in the log"
    );
}

#[test]
fn a_dropped_task_stops_blocking_landing_and_is_listed_in_the_pull_request() {
    // T2 is Working and will never integrate, which is exactly what blocks
    // `land`; the human's drop is what unblocks the run.
    let fixture = Fixture::new();
    fixture.start();
    // T2 works in its own scope, so two live tasks may hold the plan at once.
    fixture.ok(&[
        "agent",
        "task",
        "add",
        "--id",
        "T2",
        "--title",
        "a task",
        "--scope",
        "b.txt",
        "--acceptance",
        "it works",
    ]);
    fixture.ok(&[
        "agent",
        "spawn",
        "--record-only",
        "--task",
        "T2",
        "--attempt",
        "1",
        "--agent",
        "w2",
        "--worktree",
        "/tmp/wt2",
        "--branch",
        "feature/t2",
    ]);
    fixture.land_a_task("T1");
    fixture.script_forge("{");
    let message = fixture.refused(&["land"]);
    assert!(message.contains("is Working"), "{message}");

    let printed = fixture.ok(&[
        "drop",
        "T2",
        "--reason",
        "the human changed their mind",
        "--yes",
    ]);
    assert!(printed.contains("dropped T2"), "{printed}");
    assert!(
        fixture.ok(&["status"]).contains("⊘ T2 dropped"),
        "the task reads dropped"
    );

    fixture.script_forge("{}");
    fixture.ok(&["land"]);
    let calls = fixture.forge_calls();
    let opened = match calls_with(&calls, "create ").first() {
        Some(call) => call.clone(),
        None => panic!("no create call: {calls:?}"),
    };
    let body = match std::fs::read_to_string(body_of(&opened)) {
        Ok(body) => body,
        Err(err) => panic!("read the body: {err}"),
    };
    assert!(
        body.contains("## Dropped\n\n- T2: the human changed their mind"),
        "{body}"
    );
    assert!(
        body.contains("cross-vendor-review: passed"),
        "the dropped task is not an integrated task to review: {body}"
    );
}

#[test]
fn finish_ends_a_run_whose_remaining_task_was_dropped() {
    // The work landed and merged; the other task was dropped. Nothing is
    // stranded, so this is a normal finish: no `--abandon`, no question.
    let fixture = landed();
    let head = fixture.integration_head();
    fixture.script_forge(&status_script(&head, true, Some(true)));
    fixture.ok(&["land", "--merge"]);
    fixture.working_task("T2");
    fixture.ok(&["drop", "T2", "--reason", "not needed", "--yes"]);

    fixture.ok(&["run", "finish"]);
    let finished = fixture.events().into_iter().rev().find_map(|event| {
        if let EventKind::RunFinished { abandoned } = event.kind {
            Some(abandoned)
        } else {
            None
        }
    });
    assert_eq!(
        finished,
        Some(Vec::new()),
        "the run ended with nothing abandoned"
    );
}

#[test]
fn drop_is_refused_without_a_terminal_and_for_an_agent_actor() {
    let fixture = Fixture::new();
    fixture.start();
    fixture.ok(&[
        "agent",
        "task",
        "add",
        "--id",
        "T2",
        "--title",
        "a task",
        "--scope",
        "b.txt",
        "--acceptance",
        "it works",
    ]);
    let before = fixture.events().len();

    // No terminal and no --yes: nobody answered the question, so nothing is
    // dropped and nothing is written.
    let message = fixture.refused(&["drop", "T2", "--reason", "not needed"]);
    assert!(message.contains("stdin is not a terminal"), "{message}");
    assert_eq!(fixture.events().len(), before, "nothing was logged");

    // An agent pane may not drop its own work, even with `--yes`.
    let output = match fixture
        .command(&["drop", "T2", "--reason", "not needed", "--yes"])
        .env("SHPRD_ACTOR", "orchestrator")
        .output()
    {
        Ok(output) => output,
        Err(err) => panic!("running {BIN}: {err}"),
    };
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("SHPRD_ACTOR=orchestrator"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fixture.events().len(), before, "nothing was logged");
}

#[test]
fn auto_landing_opens_one_task_pull_request_and_merges_it() {
    let (fixture, head) = landing_task_fixture("auto");
    fixture.script_forge(&task_script("shprd/r1/T1", &head, Some(true), false));
    let printed = fixture.ok(&["land", "T1"]);
    assert!(
        printed.contains("merged https://example.invalid/pull/1 (task T1)"),
        "{printed}"
    );
    let calls = fixture.forge_calls();
    assert_eq!(calls_with(&calls, "create").len(), 1, "{calls:?}");
    assert_eq!(
        calls_with(&calls, &format!("merge 1 {head}")).len(),
        1,
        "the merge names the commit it merges: {calls:?}"
    );
    // The branch is on the remote, which is what the pull request proposes.
    assert_eq!(
        fixture.git(&["rev-parse", "refs/remotes/origin/shprd/r1/T1"]),
        head
    );
    // The body is the task's own, written under shprd's home, never the repo.
    let body_file = body_of(&calls_with(&calls, "create")[0]);
    let body = match std::fs::read_to_string(&body_file) {
        Ok(body) => body,
        Err(err) => panic!("read {}: {err}", body_file.display()),
    };
    assert!(body.starts_with("Task: T1"), "{body}");
    assert!(body.contains("## Summary\n\ndone"), "{body}");
    assert!(body.contains("cross-vendor-review: passed"), "{body}");
    assert!(
        body_file.starts_with(&fixture.home),
        "the body lives in shprd's home: {}",
        body_file.display()
    );
    let outcomes = fixture.landed_outcomes("T1");
    assert_eq!(outcomes.len(), 1, "{outcomes:?}");
    assert!(
        matches!(&outcomes[0], EventKind::TaskLanded { outcome, head: h, number, base, .. }
            if *outcome == shprd_core::event::LandOutcome::Merged
                && h == &head
                && *number == Some(1)
                && base == "main"),
        "{outcomes:?}"
    );
}

#[test]
fn a_second_landing_of_a_merged_task_records_nothing() {
    let (fixture, head) = landing_task_fixture("auto");
    fixture.script_forge(&task_script("shprd/r1/T1", &head, Some(true), false));
    fixture.ok(&["land", "T1"]);
    let before = fixture.events().len();
    let printed = fixture.ok(&["land", "T1"]);
    assert!(
        printed.contains("already merged https://example.invalid/pull/1"),
        "{printed}"
    );
    assert_eq!(
        fixture.events().len(),
        before,
        "a landing that already finished writes no second record"
    );
}

#[test]
fn ask_landing_waits_for_the_human_then_merges_after_approve() {
    let (fixture, head) = landing_task_fixture("ask");
    fixture.script_forge(&task_script("shprd/r1/T1", &head, Some(true), false));
    let printed = fixture.ok(&["land", "T1"]);
    assert!(
        printed.contains("waiting for your approval: run `shprd approve T1`"),
        "{printed}"
    );
    assert!(
        calls_with(&fixture.forge_calls(), "merge").is_empty(),
        "nothing merges before the human: {:?}",
        fixture.forge_calls()
    );
    assert!(
        fixture.landed_outcomes("T1").is_empty(),
        "a waiting landing has no outcome"
    );

    let approved = fixture.ok(&["approve", "T1", "--yes"]);
    assert!(
        approved.contains("approved https://example.invalid/pull/1"),
        "{approved}"
    );
    let head_approved = fixture
        .events()
        .into_iter()
        .find_map(|event| match event.kind {
            EventKind::TaskApproved {
                task,
                number,
                head_sha,
            } if task == "T1" && number == 1 => Some(head_sha),
            _ => None,
        });
    assert_eq!(head_approved.as_deref(), Some(head.as_str()));

    fixture.script_forge(&task_script("shprd/r1/T1", &head, Some(true), true));
    let printed = fixture.ok(&["land", "T1"]);
    assert!(printed.contains("merged"), "{printed}");
    let outcomes = fixture.landed_outcomes("T1");
    assert_eq!(outcomes.len(), 1, "{outcomes:?}");
}

#[test]
fn checks_landing_waits_for_the_checks_and_merges_once_they_pass() {
    let (fixture, head) = landing_task_fixture("checks");
    fixture.script_forge(&task_script("shprd/r1/T1", &head, None, false));
    let printed = fixture.ok(&["land", "T1"]);
    assert!(printed.contains("waiting for its checks"), "{printed}");
    assert!(
        calls_with(&fixture.forge_calls(), "merge").is_empty(),
        "{:?}",
        fixture.forge_calls()
    );

    fixture.script_forge(&task_script("shprd/r1/T1", &head, Some(true), true));
    let printed = fixture.ok(&["land", "T1"]);
    assert!(printed.contains("merged"), "{printed}");
    let calls = fixture.forge_calls();
    assert!(
        calls_with(&calls, "create").is_empty(),
        "the open pull request is reused, not opened again: {calls:?}"
    );
    assert_eq!(calls_with(&calls, "edit_body").len(), 1, "{calls:?}");
}

#[test]
fn a_task_without_an_independent_review_is_refused_and_asks_no_forge() {
    let fixture = Fixture::new();
    fixture.write_land_config("auto", &[]);
    fixture.start();
    fixture.passed_task_with("T1", true);
    fixture.script_forge("{}");
    let message = fixture.refused(&["land", "T1"]);
    assert!(message.contains("may not land yet"), "{message}");
    assert!(message.contains("degraded"), "{message}");
    assert!(
        fixture.forge_calls().is_empty(),
        "a refusal asks the forge nothing: {:?}",
        fixture.forge_calls()
    );
}

#[test]
fn a_task_landing_refuses_the_run_level_merge_flag() {
    let fixture = Fixture::new();
    fixture.start();
    let message = fixture.refused(&["land", "T1", "--merge"]);
    assert!(
        message.contains("`--merge` merges the run's pull request"),
        "{message}"
    );
}

#[test]
fn a_dropped_task_has_nothing_to_land() {
    let (fixture, head) = landing_task_fixture("auto");
    fixture.script_forge(&task_script("shprd/r1/T1", &head, Some(true), false));
    fixture.ok(&[
        "drop",
        "T1",
        "--reason",
        "the human changed their mind",
        "--yes",
    ]);
    let message = fixture.refused(&["land", "T1"]);
    assert!(message.contains("was dropped"), "{message}");
    assert!(
        fixture.forge_calls().is_empty(),
        "nothing reaches the forge: {:?}",
        fixture.forge_calls()
    );
}

#[test]
fn a_task_whose_checks_failed_is_refused_until_they_pass() {
    let (fixture, head) = landing_task_fixture("checks");
    fixture.script_forge(&task_script("shprd/r1/T1", &head, Some(false), false));
    let message = fixture.refused(&["land", "T1"]);
    assert!(message.contains("checks failed"), "{message}");
    assert!(
        calls_with(&fixture.forge_calls(), "merge").is_empty(),
        "{:?}",
        fixture.forge_calls()
    );
    assert!(
        fixture.landed_outcomes("T1").is_empty(),
        "a refused landing records no outcome"
    );
}

#[test]
fn a_protected_base_waits_for_a_human_even_under_auto() {
    let fixture = Fixture::new();
    fixture.write_land_config("auto", &["main"]);
    fixture.start();
    let (_, head) = fixture.landable_task("T1", "shprd/r1/T1");
    fixture.script_forge(&task_script("shprd/r1/T1", &head, Some(true), false));
    let printed = fixture.ok(&["land", "T1"]);
    assert!(
        printed.contains("waiting for your approval: run `shprd approve T1`"),
        "{printed}"
    );
    assert!(
        calls_with(&fixture.forge_calls(), "merge").is_empty(),
        "a protected base is never merged without the human: {:?}",
        fixture.forge_calls()
    );
}
