//! End-to-end tests for `shprd agent review-verdict`: the reviewer's findings
//! drive a decision made in code, and a failed round either sends the task back
//! or escalates it.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

/// The binary under test, built by cargo for this integration test.
const BIN: &str = env!("CARGO_BIN_EXE_shprd");

/// A throwaway repo root, plus the home directory shprd keeps config in.
struct Fixture {
    _repo: TempDir,
    root: PathBuf,
    _home: TempDir,
    home: PathBuf,
}

impl Fixture {
    /// A repo root and a home whose config allows two review rounds.
    fn new() -> Fixture {
        let repo = temp("repo");
        let home = temp("home");
        let fixture = Fixture {
            root: repo.path().join("root"),
            home: home.path().to_path_buf(),
            _repo: repo,
            _home: home,
        };
        makedir(&fixture.root);
        makedir(&fixture.home.join(".config").join("shprd"));
        let text = "[review]\nmust_fix = \"major\"\nrounds = 2\n";
        write(
            &fixture
                .home
                .join(".config")
                .join("shprd")
                .join("config.toml"),
            text,
        );
        fixture.start();
        fixture
    }

    /// Starts the run the verbs record against.
    fn start(&self) {
        self.ok(&[
            "run",
            "start",
            "--run-id",
            "m6",
            "--base",
            "main",
            "--integration",
            "integration/m6",
        ]);
    }

    /// Adds a task with one scope path and one acceptance line.
    fn add_task(&self, id: &str) {
        self.ok(&[
            "agent",
            "task",
            "add",
            "--id",
            id,
            "--title",
            "a task",
            "--scope",
            "crates/shprd",
            "--acceptance",
            "it works",
        ]);
    }

    /// Runs one attempt: spawn it, then report it completed.
    fn attempt(&self, id: &str, n: u32) {
        let n = n.to_string();
        self.ok(&[
            "agent",
            "spawn",
            "--record-only",
            "--task",
            id,
            "--attempt",
            &n,
            "--agent",
            "w1",
            "--worktree",
            "/tmp/wt",
            "--branch",
            "feature/x",
        ]);
        self.ok(&[
            "agent",
            "report",
            "--task",
            id,
            "--attempt",
            &n,
            "--status",
            "completed",
            "--summary",
            "did it",
        ]);
    }

    /// Decides the review for one attempt from a JSON findings file.
    fn review(&self, id: &str, n: u32, findings: &str) -> Output {
        let path = self.root.join(format!("findings-{id}-{n}.json"));
        write(&path, findings);
        self.run(&[
            "agent",
            "review-verdict",
            "--task",
            id,
            "--attempt",
            &n.to_string(),
            "--findings",
            &path.display().to_string(),
        ])
    }

    /// Runs the binary against this fixture's repo and home.
    fn run(&self, args: &[&str]) -> Output {
        let mut all: Vec<String> = vec![
            "--repo".into(),
            self.root.display().to_string(),
            "--home".into(),
            self.home.display().to_string(),
            "--no-dagr".into(),
        ];
        all.extend(args.iter().map(|arg| (*arg).to_string()));
        match Command::new(BIN).args(&all).output() {
            Ok(output) => output,
            Err(err) => panic!("running {BIN} failed: {err}"),
        }
    }

    /// Runs the binary and asserts it succeeded, returning stdout.
    fn ok(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "expected success for {args:?}, got {:?}\nstderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).to_string()
    }
}

/// A fresh temp dir, panicking rather than returning an error.
fn temp(label: &str) -> TempDir {
    match TempDir::new() {
        Ok(dir) => dir,
        Err(err) => panic!("tempdir for {label} failed: {err}"),
    }
}

/// Creates a directory, panicking on failure.
fn makedir(path: &Path) {
    if let Err(err) = std::fs::create_dir_all(path) {
        panic!("mkdir {}: {err}", path.display());
    }
}

/// Writes a file, panicking on failure.
fn write(path: &Path, text: &str) {
    if let Err(err) = std::fs::write(path, text) {
        panic!("write {}: {err}", path.display());
    }
}

/// The events in the run log, as JSON values.
fn events(root: &Path) -> Vec<Value> {
    let path = root.join(".shprd").join("events.jsonl");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) => panic!("read {}: {err}", path.display()),
    };
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| match serde_json::from_str(line) {
            Ok(value) => value,
            Err(err) => panic!("log line was not JSON ({err}): {line}"),
        })
        .collect()
}

/// The `kind` names in the log, in order.
fn kinds(root: &Path) -> Vec<String> {
    events(root)
        .iter()
        .filter_map(|event| {
            let kind = event.get("kind")?;
            Some(kind.as_str().unwrap_or_default().to_string())
        })
        .collect()
}

/// The first one-line finding, as JSON, with the given severity.
fn finding(severity: &str) -> String {
    format!(
        r#"[{{"severity":"{severity}","title":"a finding","location":null,"detail":"fix it"}}]"#
    )
}

#[test]
fn minor_findings_pass_the_review() {
    let fixture = Fixture::new();
    fixture.add_task("T1");
    fixture.attempt("T1", 1);
    let output = fixture.review("T1", 1, &finding("minor"));
    assert!(
        output.status.success(),
        "a minor-only review should pass, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(
        stdout.contains("review passed"),
        "expected a pass line, got {stdout:?}"
    );
    let kinds = kinds(&fixture.root);
    assert!(kinds.contains(&String::from("gate_passed")), "{kinds:?}");
    assert!(!kinds.contains(&String::from("gate_failed")), "{kinds:?}");
    assert!(!kinds.contains(&String::from("escalated")), "{kinds:?}");
}

#[test]
fn one_major_finding_fails_and_sends_the_task_back() {
    let fixture = Fixture::new();
    fixture.add_task("T1");
    fixture.attempt("T1", 1);
    let output = fixture.review("T1", 1, &finding("major"));
    assert!(
        output.status.success(),
        "the verb records, it does not fail"
    );
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(
        stdout.contains("review failed: 1 blocking (send back)"),
        "expected a send-back line, got {stdout:?}"
    );
    let kinds = kinds(&fixture.root);
    assert!(kinds.contains(&String::from("gate_failed")), "{kinds:?}");
    assert!(!kinds.contains(&String::from("escalated")), "{kinds:?}");
}

#[test]
fn the_second_failed_round_escalates_at_rounds_two() {
    let fixture = Fixture::new();
    fixture.add_task("T1");
    fixture.attempt("T1", 1);
    fixture.review("T1", 1, &finding("critical"));
    fixture.attempt("T1", 2);
    let output = fixture.review("T1", 2, &finding("critical"));
    assert!(
        output.status.success(),
        "the verb records, it does not fail"
    );
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(
        stdout.contains("review failed: 1 blocking (escalated)"),
        "expected an escalated line, got {stdout:?}"
    );
    let log = events(&fixture.root);
    let escalated: Vec<&Value> = log
        .iter()
        .filter(|event| event.get("kind").and_then(Value::as_str) == Some("escalated"))
        .collect();
    assert_eq!(escalated.len(), 1, "expected one escalation in {log:?}");
    let body = escalated[0].get("body").and_then(Value::as_str);
    assert_eq!(body, Some("review failed 2 times"));
}

#[test]
fn bad_findings_exit_2() {
    let fixture = Fixture::new();
    fixture.add_task("T1");
    fixture.attempt("T1", 1);
    for text in ["not json", &finding("blocker")] {
        let output = fixture.review("T1", 1, text);
        assert_eq!(
            output.status.code(),
            Some(2),
            "expected exit 2 for {text:?}, stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn a_passed_review_keeps_its_minor_findings() {
    // A pass with minor findings still carries them: the pull request body
    // prints the gate's findings, and an empty list would hide advice the
    // reviewer gave.
    let fixture = Fixture::new();
    fixture.add_task("T1");
    fixture.attempt("T1", 1);
    let output = fixture.review("T1", 1, &finding("minor"));
    assert!(output.status.success(), "a minor-only review should pass");
    let log = events(&fixture.root);
    let passed = log
        .iter()
        .find(|event| event.get("kind").and_then(Value::as_str) == Some("gate_passed"))
        .unwrap_or_else(|| panic!("no gate_passed event in {log:?}"));
    let findings = passed
        .get("findings")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("the pass recorded no findings list: {passed:?}"));
    assert_eq!(findings.len(), 1, "the pass lost its finding: {passed:?}");
    assert_eq!(
        findings[0].get("title").and_then(Value::as_str),
        Some("a finding")
    );
}
