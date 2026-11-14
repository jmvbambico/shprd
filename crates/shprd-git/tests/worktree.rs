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

#[test]
fn refuses_to_remove_a_worktree_outside_the_root() {
    let repo = Repo::new();
    let layout = repo.layout();
    let outside = repo.path.join("outside-wt");
    let path_text = outside.display().to_string();
    repo.git(&["worktree", "add", "-b", "shprd/r1/OUT", &path_text, "main"]);

    let outside_wt = worktree(&outside, "shprd/r1/OUT");
    let err = match worktree::remove(&repo.path, &layout, &outside_wt, true) {
        Ok(()) => panic!("a worktree outside the root must be refused"),
        Err(err) => err,
    };
    assert!(matches!(err, WorktreeError::OutsideRoot { .. }), "{err:?}");
    assert!(is_dir(&outside), "the worktree must survive the refusal");
    assert!(
        !repo.git(&["branch", "--list", "shprd/r1/OUT"]).is_empty(),
        "the branch must survive the refusal"
    );
}

#[test]
fn a_sibling_directory_sharing_a_name_prefix_is_outside_the_root() {
    let repo = Repo::new();
    let layout = repo.layout();
    // `<root>-evil` starts with the root's characters but is a different place:
    // components, not string prefixes, decide.
    let sibling = layout
        .root
        .with_file_name(format!(
            "{}-evil",
            layout
                .root
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("wt")
        ))
        .join("repo/r1/M2.1");
    let path_text = sibling.display().to_string();
    repo.git(&["worktree", "add", "-b", "shprd/r1/SIB", &path_text, "main"]);

    let sibling_wt = worktree(&sibling, "shprd/r1/SIB");
    let err = match worktree::remove(&repo.path, &layout, &sibling_wt, true) {
        Ok(()) => panic!("a sibling directory must be refused"),
        Err(err) => err,
    };
    assert!(matches!(err, WorktreeError::OutsideRoot { .. }), "{err:?}");
    assert!(is_dir(&sibling), "the worktree must survive the refusal");
}

/// A repository with a bare `origin`, its `main` pushed there.
fn repo_with_origin() -> (Repo, std::path::PathBuf) {
    let repo = Repo::new();
    let base = match repo.path.parent() {
        Some(base) => base.to_path_buf(),
        None => panic!("the repository must have a parent"),
    };
    let origin = base.join("origin.git");
    let origin_text = origin.display().to_string();
    support::git_in(&base, &["init", "--bare", &origin_text]);
    repo.git(&["remote", "add", "origin", &origin_text]);
    repo.git(&["push", "-u", "origin", "main"]);
    (repo, origin)
}

#[test]
fn a_new_worktree_starts_from_the_fetched_remote_base() {
    let (repo, origin) = repo_with_origin();
    let layout = repo.layout();
    let base = match repo.path.parent() {
        Some(base) => base.to_path_buf(),
        None => panic!("the repository must have a parent"),
    };
    // Someone else's work lands on the remote: another clone commits and pushes.
    let other = base.join("other");
    support::git_in(
        &base,
        &[
            "clone",
            "-b",
            "main",
            &origin.display().to_string(),
            "other",
        ],
    );
    support::git_in(&other, &["config", "user.name", "someone else"]);
    support::git_in(&other, &["config", "user.email", "other@example.invalid"]);
    let merged = support::commit_in(&other, "b.txt", "merged elsewhere\n", "elsewhere");
    support::git_in(&other, &["push", "origin", "main"]);
    assert_ne!(
        merged,
        repo.head_sha(),
        "the local base is behind, which is the point"
    );

    let local_before = repo.head_sha();
    let created = match worktree::create(&repo.path, &layout, "r1", "M2.1", "main") {
        Ok(worktree) => worktree,
        Err(err) => panic!("create must succeed: {err}"),
    };
    assert_eq!(
        created.base_sha, merged,
        "the task branches from the remote's latest, not the stale local base"
    );
    assert_eq!(
        support::head_in(&created.path),
        merged,
        "the worktree's HEAD is the fetched tip"
    );
    assert_eq!(
        repo.head_sha(),
        local_before,
        "the local base branch itself never moves"
    );
}

#[test]
fn a_local_base_with_work_of_its_own_is_used_instead_of_the_remote() {
    let (repo, _origin) = repo_with_origin();
    let layout = repo.layout();
    let local = support::commit_in(&repo.path, "local.txt", "not pushed\n", "local only");

    let created = match worktree::create(&repo.path, &layout, "r1", "M2.1", "main") {
        Ok(worktree) => worktree,
        Err(err) => panic!("create must succeed: {err}"),
    };
    assert_eq!(
        created.base_sha, local,
        "the remote lacks this commit, so the local branch is where the task starts"
    );
}
