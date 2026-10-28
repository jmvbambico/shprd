//! Throwaway repositories for tests that drive real git.
//!
//! Every test builds its own: no test reads or writes another's state, and the
//! layout root always lives inside the test's temp directory, never the real
//! home.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use shprd_core::event::{Actor, Event, EventKind, ReportStatus, ResourceAction, ResourceKind};
use shprd_git::worktree::Layout;

/// A fresh repository with one commit on `main`.
pub struct Repo {
    /// Keeps the temp directory alive for the length of the test.
    _temp: tempfile::TempDir,
    /// The repository itself: `<temp>/repo`.
    pub path: PathBuf,
}

impl Repo {
    /// Initializes a repository and commits a seed file.
    pub fn new() -> Repo {
        let temp = match tempfile::tempdir() {
            Ok(temp) => temp,
            Err(err) => panic!("tempdir: {err}"),
        };
        let path = temp.path().join("repo");
        if let Err(err) = std::fs::create_dir(&path) {
            panic!("mkdir {}: {err}", path.display());
        }
        let repo = Repo { _temp: temp, path };
        repo.git(&["init", "-b", "main"]);
        repo.git(&["config", "user.name", "shprd tests"]);
        repo.git(&["config", "user.email", "tests@example.invalid"]);
        repo.git(&["config", "commit.gpgsign", "false"]);
        repo.commit("seed.txt", "seed\n", "seed");
        repo
    }

    /// A layout whose root is inside this test's temp directory.
    pub fn layout(&self) -> Layout {
        Layout::default_for_home(self.path.parent().unwrap_or(&self.path))
    }

    /// Runs git in the repository and returns its trimmed stdout.
    pub fn git(&self, args: &[&str]) -> String {
        let output = match Command::new("git")
            .args(args)
            .current_dir(&self.path)
            .output()
        {
            Ok(output) => output,
            Err(err) => panic!("git {args:?}: {err}"),
        };
        if !output.status.success() {
            panic!(
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_string()
    }

    /// Writes `body` to `rel` inside the repository.
    pub fn write(&self, rel: &str, body: &str) {
        let path = self.path.join(rel);
        if let Some(parent) = path.parent()
            && let Err(err) = std::fs::create_dir_all(parent)
        {
            panic!("mkdir {}: {err}", parent.display());
        }
        if let Err(err) = std::fs::write(&path, body) {
            panic!("write {}: {err}", path.display());
        }
    }

    /// Commits everything as `message` and returns the new commit id.
    pub fn commit(&self, rel: &str, body: &str, message: &str) -> String {
        self.write(rel, body);
        self.git(&["add", "--", rel]);
        self.git(&["commit", "-m", message]);
        self.head_sha()
    }

    /// The commit HEAD points at.
    pub fn head_sha(&self) -> String {
        self.git(&["rev-parse", "HEAD"])
    }

    /// Makes an empty commit so the tree changes without touching a tracked
    /// path.
    pub fn commit_empty(&self, message: &str) -> String {
        self.git(&["commit", "--allow-empty", "-m", message]);
        self.head_sha()
    }
}

/// True when `path` is a directory.
pub fn is_dir(path: &Path) -> bool {
    path.is_dir()
}

/// Runs git in `dir` and returns its trimmed stdout.
///
/// Worktrees are separate directories, so a test that drives them needs git
/// somewhere other than [`Repo::path`].
pub fn git_in(dir: &Path, args: &[&str]) -> String {
    let output = match Command::new("git").args(args).current_dir(dir).output() {
        Ok(output) => output,
        Err(err) => panic!("git {args:?} in {}: {err}", dir.display()),
    };
    if !output.status.success() {
        panic!(
            "git {args:?} in {} failed: {}",
            dir.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string()
}

/// The commit HEAD points at in `dir`.
pub fn head_in(dir: &Path) -> String {
    git_in(dir, &["rev-parse", "HEAD"])
}

/// Writes `body` to `rel`, commits it, and returns the new commit id.
pub fn commit_in(dir: &Path, rel: &str, body: &str, message: &str) -> String {
    let path = dir.join(rel);
    if let Some(parent) = path.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        panic!("mkdir {}: {err}", parent.display());
    }
    if let Err(err) = std::fs::write(&path, body) {
        panic!("write {}: {err}", path.display());
    }
    git_in(dir, &["add", "--", rel]);
    git_in(dir, &["commit", "-m", message]);
    head_in(dir)
}

/// One event, as the log would hold it.
pub fn event(seq: u64, at_ms: u64, kind: EventKind) -> Event {
    Event {
        seq,
        at_ms,
        actor: Actor::Engine,
        kind,
    }
}

/// The events of a run whose single task reached `Integrated`.
///
/// The ledger records `branch` and `worktree` before the attempt runs, exactly
/// as the engine does when it creates them.
pub fn integrated_run(
    run: &str,
    task: &str,
    branch: &str,
    worktree: &Path,
    at_ms: u64,
) -> Vec<Event> {
    let worktree_text = worktree.display().to_string();
    let kinds = vec![
        EventKind::RunStarted {
            run_id: run.to_string(),
            repo: "/tmp/repo".to_string(),
            base_branch: "main".to_string(),
            integration_branch: "integration/m1".to_string(),
        },
        EventKind::TaskAdded {
            task: task.to_string(),
            title: "the task".to_string(),
            scope: vec!["crates/x".to_string()],
            acceptance: vec!["it works".to_string()],
            tags: Vec::new(),
            skills: Vec::new(),
            deps: Vec::new(),
        },
        EventKind::Resource {
            action: ResourceAction::Created,
            kind: ResourceKind::Branch,
            name: branch.to_string(),
            task: Some(task.to_string()),
        },
        EventKind::Resource {
            action: ResourceAction::Created,
            kind: ResourceKind::Worktree,
            name: worktree_text.clone(),
            task: Some(task.to_string()),
        },
        EventKind::Spawned {
            task: task.to_string(),
            attempt: 1,
            agent: "claude".to_string(),
            model: None,
            worktree: worktree_text,
            branch: branch.to_string(),
        },
        EventKind::Reported {
            task: task.to_string(),
            attempt: 1,
            status: ReportStatus::Completed,
            summary: "done".to_string(),
            tests: Vec::new(),
            changed_paths: vec!["crates/x/a.rs".to_string()],
            commit: None,
            skills_used: Vec::new(),
        },
        EventKind::GatePassed {
            task: task.to_string(),
            attempt: 1,
            gate: "lint".to_string(),
            findings: Vec::new(),
            source: None,
        },
        EventKind::Integrated {
            task: task.to_string(),
            commits: vec!["abc123".to_string()],
        },
    ];
    kinds
        .into_iter()
        .enumerate()
        .map(|(index, kind)| event(index as u64 + 1, at_ms, kind))
        .collect()
}
