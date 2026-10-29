//! Base syncs against real git: a bare `origin` moves, and the run's own
//! integration branch is the only thing written.

mod support;

use std::path::{Path, PathBuf};

use shprd_git::git::Git;
use shprd_git::integrate::{IntegrationWorktree, open_integration};
use shprd_git::sync::{self, SyncError, SyncOutcome};
use shprd_git::worktree::Layout;

use support::{Repo, commit_in, git_in, head_in};

/// A repository whose `origin` is a bare repository inside the temp directory.
struct Fixture {
    repo: Repo,
    layout: Layout,
    origin: PathBuf,
}

impl Fixture {
    /// A repository on `main`, pushed to a fresh bare `origin`.
    fn new() -> Fixture {
        let repo = Repo::new();
        let origin = repo.path.parent().unwrap_or(&repo.path).join("origin.git");
        let text = origin.to_str().unwrap_or("origin.git").to_string();
        repo.git(&["init", "--bare", &text]);
        repo.git(&["remote", "add", "origin", &text]);
        repo.git(&["push", "origin", "main"]);
        let layout = repo.layout();
        Fixture {
            repo,
            layout,
            origin,
        }
    }

    /// The run's integration worktree, created on first call.
    fn integration(&self) -> IntegrationWorktree {
        match open_integration(&self.repo.path, &self.layout, "repo", "r1", "main") {
            Ok(int) => int,
            Err(err) => panic!("open integration: {err}"),
        }
    }

    /// Commits a change to `main` in the user's checkout and pushes it, which
    /// is what a pull request merged meanwhile does to the base.
    fn move_main(&self, file: &str, body: &str, message: &str) -> String {
        let sha = commit_in(&self.repo.path, file, body, message);
        self.repo.git(&["push", "origin", "main"]);
        sha
    }

    /// The commits `origin/main` holds.
    fn origin_main(&self) -> String {
        git_in(&self.origin, &["rev-parse", "refs/heads/main"])
    }
}

/// Runs `sync` and returns its outcome, failing the test on error.
fn synced(int: &IntegrationWorktree) -> SyncOutcome {
    match sync::sync_base(int, "main") {
        Ok(outcome) => outcome,
        Err(err) => panic!("sync must succeed: {err}"),
    }
}

/// Runs `sync` and returns its error, failing the test on success.
fn refused(int: &IntegrationWorktree) -> SyncError {
    match sync::sync_base(int, "main") {
        Ok(outcome) => panic!("sync must be refused, got {outcome:?}"),
        Err(err) => err,
    }
}

/// True when a merge is in progress in `dir`.
fn merging(dir: &Path) -> bool {
    Git::new(dir)
        .run(&["rev-parse", "--verify", "--quiet", "MERGE_HEAD"])
        .is_ok()
}

#[test]
fn a_moved_base_is_merged_in_with_a_merge_commit() {
    let fixture = Fixture::new();
    let int = fixture.integration();
    let own = commit_in(&int.path, "src/int.txt", "int\n", "the task's work");
    let moved = fixture.move_main("src/base.txt", "base\n", "a merged pull request");

    match synced(&int) {
        SyncOutcome::Merged { base_sha, int_sha } => {
            assert_eq!(base_sha, moved, "the base commit merged in is named");
            assert_eq!(int_sha, head_in(&int.path), "the new head is the merge");
            let parents = git_in(&int.path, &["log", "-1", "--format=%P"]);
            assert_eq!(
                parents.split(' ').count(),
                2,
                "a merge commit, not a rebase: {parents}"
            );
        }
        other => panic!("expected a merge, got {other:?}"),
    }
    let base = std::fs::read_to_string(int.path.join("src/base.txt")).unwrap_or_default();
    assert_eq!(base, "base\n", "the base's work is in the branch");
    assert_eq!(
        git_in(&int.path, &["rev-parse", "HEAD^1"]),
        own,
        "the run's own head is the merge's first parent, id unchanged"
    );
    assert!(!merging(&int.path), "nothing left half-merged");
}

#[test]
fn a_conflicting_base_is_aborted_and_named() {
    let fixture = Fixture::new();
    let int = fixture.integration();
    commit_in(
        &int.path,
        "a.txt",
        "the task's version\n",
        "the task's work",
    );
    let before = head_in(&int.path);
    fixture.move_main("a.txt", "the base's version\n", "a merged pull request");

    match refused(&int) {
        SyncError::Conflict { files, path, .. } => {
            assert_eq!(files, vec!["a.txt".to_string()], "the file is named");
            assert_eq!(
                path, int.path,
                "the advice names the worktree to resolve in"
            );
        }
        other => panic!("expected a conflict, got {other:?}"),
    }
    assert_eq!(head_in(&int.path), before, "the head must not move");
    assert_eq!(
        git_in(&int.path, &["status", "--porcelain"]),
        "",
        "the worktree must be clean again"
    );
    assert!(!merging(&int.path), "no merge may be left in progress");
}

#[test]
fn a_sync_never_writes_the_base() {
    let fixture = Fixture::new();
    let int = fixture.integration();
    let moved = fixture.move_main("src/base.txt", "base\n", "a merged pull request");
    synced(&int);
    assert_eq!(
        fixture.origin_main(),
        moved,
        "the base is where the forge left it"
    );
    assert_eq!(
        fixture.repo.head_sha(),
        moved,
        "the user's checkout is untouched"
    );
}

#[test]
fn an_unmoved_base_is_up_to_date_and_moves_nothing() {
    let fixture = Fixture::new();
    let int = fixture.integration();
    let before = head_in(&int.path);
    match synced(&int) {
        SyncOutcome::UpToDate { head } => assert_eq!(head, before),
        other => panic!("expected up to date, got {other:?}"),
    }
    // A second sync after a merge that already happened moves nothing either.
    fixture.move_main("src/base.txt", "base\n", "a merged pull request");
    assert!(matches!(synced(&int), SyncOutcome::Merged { .. }));
    let after = head_in(&int.path);
    match synced(&int) {
        SyncOutcome::UpToDate { head } => assert_eq!(head, after),
        other => panic!("expected up to date, got {other:?}"),
    }
}

#[test]
fn a_dirty_integration_worktree_is_refused() {
    let fixture = Fixture::new();
    let int = fixture.integration();
    if let Err(err) = std::fs::write(int.path.join("stray.txt"), "stray\n") {
        panic!("write stray: {err}");
    }
    match refused(&int) {
        SyncError::Dirty { path } => assert_eq!(path, int.path),
        other => panic!("expected a dirty refusal, got {other:?}"),
    }
}

#[test]
fn base_moved_reads_the_tracking_ref_without_fetching() {
    let fixture = Fixture::new();
    let int = fixture.integration();
    let git = Git::new(&int.path);
    match sync::base_moved(&git, "main") {
        Ok(moved) => assert!(!moved, "a fresh branch is level with the base"),
        Err(err) => panic!("base_moved: {err}"),
    }
    fixture.move_main("src/base.txt", "base\n", "a merged pull request");
    // No sync yet, and no fetch: the tracking ref is what the push updated.
    match sync::base_moved(&git, "main") {
        Ok(moved) => assert!(moved, "the base moved under the run"),
        Err(err) => panic!("base_moved: {err}"),
    }
    synced(&int);
    match sync::base_moved(&git, "main") {
        Ok(moved) => assert!(!moved, "a synced branch is level with the base"),
        Err(err) => panic!("base_moved: {err}"),
    }
    match sync::base_moved(&git, "no-such-branch") {
        Ok(moved) => assert!(!moved, "a base nobody fetched says nothing"),
        Err(err) => panic!("base_moved: {err}"),
    }
}
