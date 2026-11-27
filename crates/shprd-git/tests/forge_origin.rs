//! With a GitHub remote, origin is the truth: new worktrees start from it, a
//! review diff is taken against it, and a stale local ref is brought back.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use shprd_git::git::Git;
use shprd_git::{integrate, sync, worktree};
use support::Repo;

/// A repo whose `origin` is named at github.com (served by a local bare repo
/// through `insteadOf`), with `main` pushed.
fn forge_repo() -> (Repo, std::path::PathBuf) {
    let repo = Repo::new();
    let bare = repo.path.parent().unwrap().join("origin.git");
    let out = std::process::Command::new("git")
        .args(["init", "--bare", "-b", "main"])
        .arg(&bare)
        .output()
        .unwrap();
    assert!(out.status.success());
    repo.git(&[
        "remote",
        "add",
        "origin",
        "https://github.com/acme/widgets.git",
    ]);
    repo.git(&[
        "config",
        &format!("url.{}.insteadOf", bare.display()),
        "https://github.com/acme/widgets.git",
    ]);
    repo.git(&["push", "-u", "origin", "main"]);
    (repo, bare)
}

/// Origin gains a commit the local `main` lacks, while local `main` gains one
/// origin lacks: the diverged state a local-only landing leaves behind.
fn diverge(repo: &Repo, bare: &std::path::Path) -> (String, String) {
    let other = bare.parent().unwrap().join("other");
    let out = std::process::Command::new("git")
        .args(["clone", "-q"])
        .arg(bare)
        .arg(&other)
        .output()
        .unwrap();
    assert!(out.status.success());
    let run = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(&other)
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}");
    };
    run(&["config", "user.name", "t"]);
    run(&["config", "user.email", "t@example.invalid"]);
    std::fs::write(other.join("t1.txt"), "t1\n").unwrap();
    run(&["add", "."]);
    run(&["commit", "-q", "-m", "T1"]);
    run(&["push", "-q", "origin", "main"]);
    let theirs = String::from_utf8(
        std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&other)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    repo.commit("local.txt", "local\n", "local only");
    let ours = repo.git(&["rev-parse", "HEAD"]);
    (theirs, ours)
}

#[test]
fn a_new_worktree_starts_from_origin_even_when_the_local_ref_diverged() {
    let (repo, bare) = forge_repo();
    let (theirs, _ours) = diverge(&repo, &bare);
    let wt = worktree::create(&repo.path, &repo.layout(), "r1", "t7", "main").unwrap();
    let head = Git::new(&wt.path).head_sha().unwrap();
    assert_eq!(head, theirs);
}

#[test]
fn a_review_diff_is_taken_against_origin_not_a_diverged_local_ref() {
    let (repo, bare) = forge_repo();
    let (theirs, _ours) = diverge(&repo, &bare);
    // A branch cut from the stale local ref, with one commit of its own.
    repo.git(&["switch", "-q", "-c", "task"]);
    repo.commit("task.txt", "task\n", "task work");
    let git = Git::new(&repo.path);
    let base = sync::attempt_base(&git, "main", "task", true).unwrap();
    // The merge base with origin/main is the shared seed, never the local-only
    // commit, so the diff cannot be inflated by a ref origin does not have.
    let expected = repo.git(&["merge-base", "origin/main", "task"]);
    assert_eq!(base, expected);
    assert_ne!(base, theirs);
}

#[test]
fn realigning_moves_a_diverged_local_ref_back_to_origin() {
    let (repo, bare) = forge_repo();
    let (theirs, ours) = diverge(&repo, &bare);
    repo.git(&["switch", "-q", "-c", "elsewhere"]);
    assert!(integrate::realign_with_remote(&repo.path, "main"));
    assert_eq!(repo.git(&["rev-parse", "main"]), theirs);
    // The local-only commit is not lost: it is still an object, reachable by sha.
    assert!(repo.git(&["cat-file", "-t", &ours]).contains("commit"));
    // A second pass has nothing to do, and a checked-out branch is left alone.
    assert!(!integrate::realign_with_remote(&repo.path, "main"));
}

#[test]
fn a_github_remote_always_expects_the_forge_so_a_landing_never_goes_local() {
    let (repo, _bare) = forge_repo();
    assert!(shprd_git::land::forge_expected(&Git::new(&repo.path)));
    let bare_only = Repo::new();
    assert!(!shprd_git::land::forge_expected(&Git::new(&bare_only.path)));
}
