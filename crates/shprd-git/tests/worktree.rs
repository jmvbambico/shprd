//! Worktree creation and removal, against real git.

mod support;

use shprd_git::worktree::{self, Worktree, WorktreeError};

use support::{Repo, is_dir};

fn worktree(path: &std::path::Path, branch: &str) -> Worktree {
    Worktree {
        path: path.to_path_buf(),
        branch: branch.to_string(),
        base_sha: String::new(),
    }
}

#[test]
fn creates_then_removes_a_worktree() {
    let repo = Repo::new();
    let layout = repo.layout();
    let created = match worktree::create(&repo.path, &layout, "r1", "M2.1", "main") {
        Ok(worktree) => worktree,
        Err(err) => panic!("create must succeed: {err}"),
    };
    assert_eq!(created.branch, "shprd/r1/M2.1");
    assert!(is_dir(&created.path), "{}", created.path.display());
    assert_eq!(created.base_sha, repo.head_sha());
    assert_eq!(repo.git(&["rev-parse", "--abbrev-ref", "HEAD"]), "main");

    if let Err(err) = worktree::remove(&repo.path, &layout, &created, false) {
        panic!("remove must succeed: {err}");
    }
    assert!(!created.path.exists(), "the directory must be gone");
    let branches = repo.git(&["branch", "--list", "shprd/r1/M2.1"]);
    assert!(branches.is_empty(), "the branch must be gone: {branches}");
}

#[test]
fn refuses_a_branch_that_already_exists() {
    let repo = Repo::new();
    let layout = repo.layout();
    repo.git(&["branch", "shprd/r1/M2.1"]);
    let err = match worktree::create(&repo.path, &layout, "r1", "M2.1", "main") {
        Ok(_) => panic!("an existing branch must be refused"),
        Err(err) => err,
    };
    assert!(matches!(err, WorktreeError::BranchExists { .. }), "{err:?}");
}

#[test]
fn refuses_to_remove_a_branch_outside_the_namespace() {
    let repo = Repo::new();
    let layout = repo.layout();
    let path = layout.root.join("repo/feature/x");
    if let Some(parent) = path.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        panic!("mkdir {}: {err}", parent.display());
    }
    let path_text = path.display().to_string();
    repo.git(&["worktree", "add", "-b", "feature/x", &path_text, "main"]);

    let outside = worktree(&path, "feature/x");
    let err = match worktree::remove(&repo.path, &layout, &outside, true) {
        Ok(()) => panic!("a branch outside the namespace must be refused"),
        Err(err) => err,
    };
    assert!(
        matches!(err, WorktreeError::OutsideNamespace { .. }),
        "{err:?}"
    );
    assert!(is_dir(&path), "the worktree must survive the refusal");
    assert!(
        !repo.git(&["branch", "--list", "feature/x"]).is_empty(),
        "the branch must survive the refusal"
    );
}

#[test]
fn refuses_a_dirty_worktree_without_force() {
    let repo = Repo::new();
    let layout = repo.layout();
    let created = match worktree::create(&repo.path, &layout, "r1", "M2.1", "main") {
        Ok(worktree) => worktree,
        Err(err) => panic!("create must succeed: {err}"),
    };
    let dirty = created.path.join("dirty.txt");
    if let Err(err) = std::fs::write(&dirty, "uncommitted\n") {
        panic!("write {}: {err}", dirty.display());
    }

    let err = match worktree::remove(&repo.path, &layout, &created, false) {
        Ok(()) => panic!("a dirty worktree must be refused without force"),
        Err(err) => err,
    };
    assert!(matches!(err, WorktreeError::Dirty { .. }), "{err:?}");
    assert!(is_dir(&created.path), "the worktree must survive");

    if let Err(err) = worktree::remove(&repo.path, &layout, &created, true) {
        panic!("force must remove a dirty worktree: {err}");
    }
    assert!(!created.path.exists(), "the directory must be gone");
    assert!(
        repo.git(&["branch", "--list", "shprd/r1/M2.1"]).is_empty(),
        "the branch must be gone"
    );
}

#[test]
fn refuses_an_unusable_run_or_task_id() {
    let repo = Repo::new();
    let layout = repo.layout();
    for (run, task) in [
        ("", "M2.1"),
        ("r1", ""),
        ("r1/../evil", "M2.1"),
        ("r1", "a b"),
        ("r1", "../escape"),
        ("r 1", "M2.1"),
    ] {
        let err = match worktree::create(&repo.path, &layout, run, task, "main") {
            Ok(worktree) => panic!("{run:?}/{task:?} must be refused, got {worktree:?}"),
            Err(err) => err,
        };
        assert!(matches!(err, WorktreeError::InvalidId { .. }), "{err:?}");
    }
}
