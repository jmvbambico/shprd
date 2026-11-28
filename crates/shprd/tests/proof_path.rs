//! `--proof` paths: a tracked file is refused with a message naming
//! `.shprd/proof/`, an untracked one under it is recorded.

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

mod support;

use support::isolate;

/// The binary under test, built by cargo for this integration test.
const BIN: &str = env!("CARGO_BIN_EXE_shprd");

/// Runs the binary with `--repo <root>` and the given arguments.
fn run(root: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(BIN);
    isolate(&mut command, root);
    match command
        .arg("--repo")
        .arg(root)
        .args(args)
        .env_remove("SHPRD_ACTOR")
        .output()
    {
        Ok(output) => output,
        Err(err) => panic!("running {BIN} failed: {err}"),
    }
}

/// Runs a git command in `root`, panicking when it fails.
fn git(root: &Path, args: &[&str]) {
    match Command::new("git").arg("-C").arg(root).args(args).output() {
        Ok(out) if out.status.success() => {}
        Ok(out) => panic!("git {args:?}: {}", String::from_utf8_lossy(&out.stderr)),
        Err(err) => panic!("git {args:?}: {err}"),
    }
}

/// A git repo root with a task whose first attempt is recorded, one tracked
/// proof file and one untracked proof file under `.shprd/proof/`.
fn fixture() -> TempDir {
    let dir = match TempDir::new() {
        Ok(dir) => dir,
        Err(err) => panic!("tempdir failed: {err}"),
    };
    let root = dir.path();
    git(root, &["init", "-q", "-b", "main"]);
    for file in ["proof/tests.txt", ".shprd/proof/notes.txt"] {
        let path = root.join(file);
        if let Some(parent) = path.parent()
            && let Err(err) = std::fs::create_dir_all(parent)
        {
            panic!("mkdir {}: {err}", parent.display());
        }
        if let Err(err) = std::fs::write(&path, "ok") {
            panic!("write {}: {err}", path.display());
        }
    }
    git(root, &["add", "proof/tests.txt"]);
    git(
        root,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "commit",
            "-q",
            "-m",
            "seed",
        ],
    );
    for args in [
        &[
            "run",
            "start",
            "--run-id",
            "t1",
            "--base",
            "main",
            "--integration",
            "integration/m1",
        ][..],
        &[
            "agent",
            "task",
            "add",
            "--id",
            "T1",
            "--title",
            "a task",
            "--scope",
            "crates/shprd",
            "--acceptance",
            "it works",
        ],
        &[
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
            "/tmp/wt",
            "--branch",
            "feature/x",
        ],
    ] {
        let out = run(root, args);
        assert!(out.status.success(), "{args:?}: {:?}", out);
    }
    dir
}

/// A hand-in carrying one `--proof`.
fn report(root: &Path, proof: &str) -> Output {
    run(
        root,
        &[
            "agent",
            "report",
            "--check",
            "tests|cargo test|0",
            "--task",
            "T1",
            "--attempt",
            "1",
            "--status",
            "completed",
            "--summary",
            "the menu opens",
            "--proof",
            proof,
        ],
    )
}

/// A proof that git tracks is refused, and the refusal names `.shprd/proof/`.
#[test]
fn a_tracked_proof_is_refused() {
    let dir = fixture();
    let out = report(dir.path(), "proof/tests.txt");
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("tracked"), "{stderr}");
    assert!(stderr.contains(".shprd/proof/"), "{stderr}");
}

/// An untracked proof under `.shprd/proof/` is accepted and recorded.
#[test]
fn an_untracked_proof_under_shprd_proof_is_accepted() {
    let dir = fixture();
    let out = report(dir.path(), ".shprd/proof/notes.txt");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
