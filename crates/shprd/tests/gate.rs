//! End-to-end tests for `shprd gate`: the run-wide gate over the integration
//! worktree. They drive the real binary against a throwaway repository.

use std::path::PathBuf;
use std::process::{Command, Output};

use shprd_core::event::{Event, EventKind};
use shprd_git::worktree::Layout;
use tempfile::TempDir;

/// The binary under test, built by cargo for this integration test.
const BIN: &str = env!("CARGO_BIN_EXE_shprd");

/// A throwaway repository with one commit on `main`, and a home beside it.
struct Fixture {
    /// Keeps the temp directory alive.
    _temp: TempDir,
    /// The repository itself.
    repo: PathBuf,
    /// The home `--home` points at.
    home: PathBuf,
}

impl Fixture {
    /// Builds the repository, commits `files`, and starts run `t1`.
    fn new(files: &[(&str, &str)]) -> Fixture {
        let temp = match TempDir::new() {
            Ok(temp) => temp,
            Err(err) => panic!("tempdir failed: {err}"),
        };
        let repo = temp.path().join("repo");
        let home = temp.path().join("home");
        if let Err(err) = std::fs::create_dir_all(&repo) {
            panic!("mkdir {}: {err}", repo.display());
        }
        if let Err(err) = std::fs::create_dir_all(&home) {
            panic!("mkdir {}: {err}", home.display());
        }
        let fixture = Fixture {
            _temp: temp,
            repo,
            home,
        };
        fixture.git(&["init", "-b", "main"]);
        fixture.git(&["config", "user.name", "shprd tests"]);
        fixture.git(&["config", "user.email", "tests@example.invalid"]);
        fixture.git(&["config", "commit.gpgsign", "false"]);
        for (name, body) in files {
            if let Err(err) = std::fs::write(fixture.repo.join(name), body) {
                panic!("write {name}: {err}");
            }
        }
        fixture.git(&["add", "--", "."]);
        fixture.git(&["commit", "-m", "seed"]);
        fixture.write_config(None);
        fixture.ok(&[
            "run",
            "start",
            "--run-id",
            "t1",
            "--base",
            "main",
            "--integration",
            "integration/m1",
        ]);
        fixture
    }

    /// Writes `.shprd/config.toml` with `gates.integration = commands`.
    fn write_config(&self, commands: Option<&[&str]>) {
        let dir = self.repo.join(".shprd");
        if let Err(err) = std::fs::create_dir_all(&dir) {
            panic!("mkdir {}: {err}", dir.display());
        }
        let body = match commands {
            Some(commands) => {
                let list: Vec<String> = commands
                    .iter()
                    .map(|command| format!("\"{command}\""))
                    .collect();
                format!("[gates]\nintegration = [{}]\n", list.join(", "))
            }
            None => "[gates]\nworker = [\"true\"]\n".to_string(),
        };
        let path = dir.join("config.toml");
        if let Err(err) = std::fs::write(&path, body) {
            panic!("write {}: {err}", path.display());
        }
    }

    /// Runs git here, returning its trimmed stdout.
    fn git(&self, args: &[&str]) -> String {
        let output = match Command::new("git")
            .args(args)
            .current_dir(&self.repo)
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
        String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_string()
    }

    /// The binary, `--repo`, `--home`, `--no-dagr`, then `args`.
    fn run(&self, args: &[&str]) -> Output {
        let mut all: Vec<String> = vec![
            "--repo".into(),
            self.repo.display().to_string(),
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

    /// The integration worktree the run owns.
    fn integration(&self) -> PathBuf {
        Layout::default_for_home(&self.home).path("repo", "t1", "_int")
    }

    /// Every event in the log, oldest first.
    fn events(&self) -> Vec<Event> {
        let path = self.repo.join(".shprd").join("events.jsonl");
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
}

/// The last full gate the log recorded.
fn last_full_gate(fixture: &Fixture) -> (String, bool, Vec<String>) {
    for event in fixture.events().into_iter().rev() {
        if let EventKind::FullGate {
            head,
            passed,
            steps,
        } = event.kind
        {
            let commands = steps.iter().map(|step| step.command.clone()).collect();
            return (head, passed, commands);
        }
    }
    panic!("no full gate in the log");
}

#[test]
fn a_passing_gate_is_recorded_and_exits_zero() {
    let fixture = Fixture::new(&[("README.md", "hi\n")]);
    fixture.write_config(Some(&["test -f README.md"]));
    let stdout = fixture.ok(&["gate"]);
    assert!(stdout.contains("✓ test -f README.md"), "{stdout}");
    let (head, passed, commands) = last_full_gate(&fixture);
    assert!(passed, "the gate should have passed");
    assert_eq!(commands, vec!["test -f README.md".to_string()]);
    let expected = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(fixture.integration())
        .output();
    let expected = match expected {
        Ok(output) => String::from_utf8_lossy(&output.stdout).trim().to_string(),
        Err(err) => panic!("git rev-parse: {err}"),
    };
    assert_eq!(head, expected, "the head is the integration worktree's");
}

#[test]
fn a_failing_gate_is_recorded_and_exits_one() {
    let fixture = Fixture::new(&[("README.md", "hi\n")]);
    fixture.write_config(Some(&["test -f README.md", "test -f MISSING.md"]));
    let output = fixture.run(&["gate"]);
    assert_eq!(
        output.status.code(),
        Some(1),
        "a failed gate exits 1\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(stdout.contains("✗ test -f MISSING.md"), "{stdout}");
    let (_, passed, commands) = last_full_gate(&fixture);
    assert!(!passed);
    assert_eq!(commands.len(), 2, "the gate stops at the first failure");
}

#[test]
fn no_integration_gate_is_refused_and_records_nothing() {
    let fixture = Fixture::new(&[("README.md", "hi\n")]);
    fixture.write_config(None);
    let before = fixture.events().len();
    let output = fixture.run(&["gate"]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        stderr.contains(
            "no integration gate configured: set [gates] integration in .shprd/config.toml"
        ),
        "{stderr}"
    );
    assert_eq!(fixture.events().len(), before, "nothing may be appended");
}

#[test]
fn a_dirty_integration_worktree_is_refused() {
    let fixture = Fixture::new(&[("README.md", "hi\n")]);
    fixture.write_config(Some(&["test -f README.md"]));
    fixture.ok(&["gate"]);
    let before = fixture.events().len();
    let stray = fixture.integration().join("stray.txt");
    if let Err(err) = std::fs::write(&stray, "uncommitted\n") {
        panic!("write {}: {err}", stray.display());
    }
    let output = fixture.run(&["gate"]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(stderr.contains("has uncommitted changes"), "{stderr}");
    assert_eq!(fixture.events().len(), before, "nothing may be appended");
}

#[test]
fn a_gate_that_changes_the_worktree_fails() {
    let fixture = Fixture::new(&[("README.md", "hi\n")]);
    fixture.write_config(Some(&["echo x > stray.txt"]));
    let output = fixture.run(&["gate"]);
    assert_eq!(
        output.status.code(),
        Some(1),
        "a gate that writes into the worktree exits 1\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let (_, passed, commands) = last_full_gate(&fixture);
    assert!(
        !passed,
        "a gate that leaves the worktree dirty must not pass"
    );
    assert_eq!(
        commands.last().map(String::as_str),
        Some("worktree changed by the gate"),
        "{commands:?}"
    );
}
