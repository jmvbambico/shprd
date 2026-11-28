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
fn realigning_keeps_a_local_ref_with_commits_origin_lacks_and_names_them() {
    let (repo, bare) = forge_repo();
    let (_theirs, ours) = diverge(&repo, &bare);
    repo.git(&["switch", "-q", "-c", "elsewhere"]);
    let out = integrate::realign_outcome(&repo.path, "main", |_| false);
    match out {
        integrate::Realign::Kept(commits) => {
            assert_eq!(commits.len(), 1, "{commits:?}");
            assert!(commits[0].starts_with(&ours[..7]), "{commits:?}");
        }
        other => panic!("expected Kept, got {other:?}"),
    }
    assert!(!integrate::realign_with_remote(&repo.path, "main"));
    assert_eq!(repo.git(&["rev-parse", "main"]), ours);
}

#[test]
fn realigning_moves_a_local_ref_whose_commits_are_all_on_origin_by_content() {
    let (repo, bare) = forge_repo();
    // A local-only commit that origin later got again as a different commit
    // with the same change (a rebase or squash landing).
    repo.commit("same.txt", "same\n", "landing");
    let local_tip = repo.git(&["rev-parse", "main"]);
    let other = bare.parent().unwrap().join("carry");
    let run = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(&other)
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}");
    };
    let out = std::process::Command::new("git")
        .args(["clone", "-q"])
        .arg(&bare)
        .arg(&other)
        .output()
        .unwrap();
    assert!(out.status.success());
    run(&["config", "user.name", "t"]);
    run(&["config", "user.email", "t@example.invalid"]);
    std::fs::write(other.join("same.txt"), "same\n").unwrap();
    run(&["add", "."]);
    run(&["commit", "-q", "-m", "landing (carried over)"]);
    run(&["push", "-q", "origin", "main"]);
    let theirs = {
        let o = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&other)
            .output()
            .unwrap();
        String::from_utf8(o.stdout).unwrap().trim().to_string()
    };
    assert_ne!(theirs, local_tip);
    repo.git(&["switch", "-q", "-c", "elsewhere"]);
    assert!(integrate::realign_with_remote(&repo.path, "main"));
    assert_eq!(repo.git(&["rev-parse", "main"]), theirs);
    assert!(repo.git(&["cat-file", "-t", &local_tip]).contains("commit"));
}

#[test]
fn a_github_remote_always_expects_the_forge_so_a_landing_never_goes_local() {
    let (repo, _bare) = forge_repo();
    assert!(shprd_git::land::forge_expected(&Git::new(&repo.path)));
    let bare_only = Repo::new();
    assert!(!shprd_git::land::forge_expected(&Git::new(&bare_only.path)));
}

#[test]
fn a_non_github_remote_keeps_its_local_only_integration_commit() {
    let repo = Repo::new();
    let bare = repo.path.parent().unwrap().join("plain-origin.git");
    let out = std::process::Command::new("git")
        .args(["init", "--bare", "-b", "main"])
        .arg(&bare)
        .output()
        .unwrap();
    assert!(out.status.success());
    repo.git(&["remote", "add", "origin", bare.to_str().unwrap()]);
    repo.git(&["push", "-u", "origin", "main"]);
    // A local-only landing on main that the remote never had.
    repo.commit("local.txt", "local\n", "local-only landing");
    let ours = repo.git(&["rev-parse", "main"]);
    repo.git(&["switch", "-q", "-c", "elsewhere"]);
    // Both the server idle path and the CLI landing call this helper.
    assert!(!integrate::realign_with_remote(&repo.path, "main"));
    assert_eq!(repo.git(&["rev-parse", "main"]), ours);
}

/// A repo whose `origin` is named at `host` (served by a local bare repo
/// through `insteadOf`), with `main` pushed and then diverged from the remote.
fn hosted_repo(host: &str) -> (Repo, String, String) {
    let repo = Repo::new();
    let bare = repo.path.parent().unwrap().join("hosted.git");
    let out = std::process::Command::new("git")
        .args(["init", "--bare", "-b", "main"])
        .arg(&bare)
        .output()
        .unwrap();
    assert!(out.status.success());
    let url = format!("https://{host}/acme/widgets.git");
    repo.git(&["remote", "add", "origin", &url]);
    repo.git(&["config", &format!("url.{}.insteadOf", bare.display()), &url]);
    repo.git(&["push", "-u", "origin", "main"]);
    let (theirs, ours) = diverge(&repo, &bare);
    (repo, theirs, ours)
}

#[test]
fn a_configured_non_github_forge_realigns_the_local_integration_ref() {
    let (repo, theirs, _ours) = hosted_repo("git.example.net");
    repo.git(&["switch", "-q", "-c", "elsewhere"]);
    let git = Git::new(&repo.path);
    // A forge client that drives the host: its landings go through the forge.
    assert!(shprd_git::land::forge_expected_with(&git, |h| h == "git.example.net"));
    // The forge is recognised, so the realignment runs (it keeps the diverged
    // ref and names the commit); with no forge it would not look at all.
    assert!(matches!(
        integrate::realign_outcome(&repo.path, "main", |h| h == "git.example.net"),
        integrate::Realign::Kept(_)
    ));
    assert_ne!(repo.git(&["rev-parse", "main"]), theirs);
}

#[test]
fn a_repository_with_no_forge_never_has_its_integration_ref_reset() {
    // A hosted remote nobody can drive, and a remote with no host at all.
    let (repo, _theirs, ours) = hosted_repo("git.example.net");
    repo.git(&["switch", "-q", "-c", "elsewhere"]);
    assert_eq!(
        integrate::realign_outcome(&repo.path, "main", |_| false),
        integrate::Realign::Unchanged
    );
    assert_eq!(repo.git(&["rev-parse", "main"]), ours);
}

#[test]
fn realigning_a_ref_equal_to_origin_reads_aligned() {
    let (repo, _bare) = forge_repo();
    repo.git(&["switch", "-q", "-c", "elsewhere"]);
    assert_eq!(
        integrate::realign_outcome(&repo.path, "main", |_| false),
        integrate::Realign::Aligned
    );
}
