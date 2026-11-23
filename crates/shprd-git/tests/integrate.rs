//! Integration against real git: verified cherry-picks onto a run's
//! integration branch, and nothing touched outside its own worktree.

mod support;

use std::path::{Path, PathBuf};

use shprd_git::integrate::{
    self, IntegrateError, IntegrationWorktree, TaskCommits, open_integration,
};
use shprd_git::worktree::{self, Layout};

use support::{Repo, commit_in, git_in, head_in};

/// A task branch with commits, cut from `main`, in a worktree of its own.
struct Task {
    branch: String,
    base_sha: String,
    commits: Vec<String>,
    path: PathBuf,
}

impl Task {
    /// The claim a task makes about itself, with its scope.
    fn claim(&self, scope: &[&str]) -> TaskCommits {
        TaskCommits {
            branch: self.branch.clone(),
            base_sha: self.base_sha.clone(),
            claimed: self.commits.clone(),
            scope: scope.iter().map(|entry| (*entry).to_string()).collect(),
        }
    }
}

/// Cuts a task branch off `main` and commits one file per entry.
fn task(repo: &Repo, layout: &Layout, name: &str, files: &[(&str, &str)]) -> Task {
    let created = match worktree::create(&repo.path, layout, "r1", name, "main") {
        Ok(worktree) => worktree,
        Err(err) => panic!("create task worktree: {err}"),
    };
    let mut commits = Vec::new();
    for (rel, body) in files {
        commits.push(commit_in(
            &created.path,
            rel,
            body,
            &format!("{name}: {rel}"),
        ));
    }
    Task {
        branch: created.branch,
        base_sha: created.base_sha,
        commits,
        path: created.path,
    }
}

/// Opens the integration worktree of run `r1`, failing the test if it cannot.
fn open(repo: &Repo, layout: &Layout) -> IntegrationWorktree {
    match open_integration(&repo.path, layout, "repo", "r1", "main") {
        Ok(int) => int,
        Err(err) => panic!("open integration: {err}"),
    }
}

/// Runs `integrate` and returns the error, failing the test on success.
fn refused(int: &IntegrationWorktree, claim: &TaskCommits) -> IntegrateError {
    match integrate::integrate(int, claim) {
        Ok(done) => panic!("integration must be refused, got {done:?}"),
        Err(err) => err,
    }
}

/// The subjects of the last `count` commits in `dir`, oldest first.
fn subjects(dir: &Path, count: usize) -> Vec<String> {
    let raw = git_in(
        dir,
        &["log", &format!("-{count}"), "--reverse", "--format=%s"],
    );
    raw.lines().map(str::to_string).collect()
}

#[test]
fn lands_two_commits_in_order_with_trailers() {
    let repo = Repo::new();
    let layout = repo.layout();
    let t = task(
        &repo,
        &layout,
        "T1",
        &[("src/a.txt", "a\n"), ("src/b.txt", "b\n")],
    );
    let int = open(&repo, &layout);
    assert!(int.created, "the first call makes the worktree");
    assert_eq!(int.branch, "shprd/r1/int");
    assert_eq!(int.path, layout.path("repo", "r1", "_int"));

    let done = match integrate::integrate(&int, &t.claim(&["src"])) {
        Ok(done) => done,
        Err(err) => panic!("integration must succeed: {err}"),
    };
    assert_eq!(done.picked.len(), 2);
    assert_eq!(done.new_head, head_in(&int.path));
    assert_eq!(
        subjects(&int.path, 2),
        vec!["T1: src/a.txt", "T1: src/b.txt"]
    );
    let landed = std::fs::read_to_string(int.path.join("src/b.txt")).unwrap_or_default();
    assert_eq!(landed, "b\n");
    let trailers = git_in(&int.path, &["log", "-2", "--format=%b"]);
    for commit in &t.commits {
        let expected = format!("(cherry picked from commit {commit})");
        assert!(
            trailers.contains(&expected),
            "missing {expected} in {trailers}"
        );
    }
    assert_eq!(
        git_in(&repo.path, &["rev-parse", &t.branch]),
        t.commits[1],
        "the task branch must be untouched"
    );
    assert_eq!(
        repo.head_sha(),
        t.base_sha,
        "the user's checkout must be untouched"
    );
}

#[test]
fn refuses_a_claim_that_misses_a_commit() {
    let repo = Repo::new();
    let layout = repo.layout();
    let t = task(
        &repo,
        &layout,
        "T1",
        &[("src/a.txt", "a\n"), ("src/b.txt", "b\n")],
    );
    let int = open(&repo, &layout);
    let head = head_in(&int.path);
    let mut claim = t.claim(&["src"]);
    claim.claimed.truncate(1);
    let err = refused(&int, &claim);
    match &err {
        IntegrateError::ClaimMismatch {
            expected, claimed, ..
        } => {
            assert_eq!(expected.len(), 2, "{err}");
            assert_eq!(claimed.len(), 1, "{err}");
        }
        other => panic!("expected ClaimMismatch, got {other:?}"),
    }
    assert_eq!(head_in(&int.path), head, "nothing may be picked");
}

#[test]
fn refuses_a_claim_that_adds_a_commit() {
    let repo = Repo::new();
    let layout = repo.layout();
    let t = task(&repo, &layout, "T1", &[("src/a.txt", "a\n")]);
    let int = open(&repo, &layout);
    let mut claim = t.claim(&["src"]);
    claim.claimed.insert(0, t.base_sha.clone());
    match refused(&int, &claim) {
        IntegrateError::ClaimMismatch { expected, .. } => assert_eq!(expected, t.commits),
        other => panic!("expected ClaimMismatch, got {other:?}"),
    }
}

#[test]
fn refuses_a_path_outside_the_scope() {
    let repo = Repo::new();
    let layout = repo.layout();
    let t = task(
        &repo,
        &layout,
        "T1",
        &[("src/a.txt", "a\n"), ("docs/x.md", "x\n")],
    );
    let int = open(&repo, &layout);
    let head = head_in(&int.path);
    match refused(&int, &t.claim(&["src"])) {
        IntegrateError::OutsideScope { outside } => assert_eq!(outside, vec!["docs/x.md"]),
        other => panic!("expected OutsideScope, got {other:?}"),
    }
    assert_eq!(head_in(&int.path), head, "nothing may be picked");
}

#[test]
fn a_conflict_is_reported_and_the_worktree_put_back() {
    let repo = Repo::new();
    let layout = repo.layout();
    let t = task(&repo, &layout, "T1", &[("src/a.txt", "task\n")]);
    let int = open(&repo, &layout);
    let ours = commit_in(&int.path, "src/a.txt", "integration\n", "int: a");
    let task_tip = git_in(&repo.path, &["rev-parse", &t.branch]);
    let task_work = std::fs::read_to_string(t.path.join("src/a.txt")).unwrap_or_default();

    match refused(&int, &t.claim(&["src"])) {
        IntegrateError::Conflict { commit, files } => {
            assert_eq!(commit, t.commits[0]);
            assert_eq!(files, vec!["src/a.txt".to_string()]);
        }
        other => panic!("expected Conflict, got {other:?}"),
    }
    assert_eq!(head_in(&int.path), ours, "the head must be restored");
    assert_eq!(
        git_in(&int.path, &["status", "--porcelain"]),
        "",
        "the worktree must be clean again"
    );
    assert_eq!(
        git_in(&repo.path, &["rev-parse", &t.branch]),
        task_tip,
        "the task branch must be untouched"
    );
    let kept = std::fs::read_to_string(t.path.join("src/a.txt")).unwrap_or_default();
    assert_eq!(kept, task_work, "the task worktree must be untouched");
    assert_eq!(repo.head_sha(), t.base_sha);
}

#[test]
fn refuses_a_dirty_integration_worktree() {
    let repo = Repo::new();
    let layout = repo.layout();
    let t = task(&repo, &layout, "T1", &[("src/a.txt", "a\n")]);
    let int = open(&repo, &layout);
    let stray = int.path.join("src");
    if let Err(err) = std::fs::create_dir_all(&stray) {
        panic!("mkdir: {err}");
    }
    if let Err(err) = std::fs::write(int.path.join("src/stray.txt"), "stray\n") {
        panic!("write: {err}");
    }
    match refused(&int, &t.claim(&["src"])) {
        IntegrateError::Dirty { path } => assert_eq!(path, int.path),
        other => panic!("expected Dirty, got {other:?}"),
    }
    assert_eq!(head_in(&int.path), t.base_sha, "nothing may be picked");
}

#[test]
fn open_integration_is_idempotent() {
    let repo = Repo::new();
    let layout = repo.layout();
    let first = open(&repo, &layout);
    let second = open(&repo, &layout);
    assert!(first.created);
    assert!(!second.created, "the second call must find the worktree");
    assert_eq!(first.path, second.path);
    assert_eq!(first.branch, second.branch);
    assert_eq!(head_in(&second.path), repo.head_sha());
}

#[test]
fn open_integration_refuses_a_half_state() {
    let branch_only = Repo::new();
    let layout = branch_only.layout();
    branch_only.git(&["branch", "shprd/r1/int", "main"]);
    match open_integration(&branch_only.path, &layout, "repo", "r1", "main") {
        Ok(int) => panic!("a branch without a worktree must be refused, got {int:?}"),
        Err(IntegrateError::BadState { detail, .. }) => {
            assert!(detail.contains("worktree"), "{detail}");
        }
        Err(other) => panic!("expected BadState, got {other:?}"),
    }

    let path_only = Repo::new();
    let layout = path_only.layout();
    let orphan = layout.path("repo", "r1", "_int");
    if let Err(err) = std::fs::create_dir_all(&orphan) {
        panic!("mkdir {}: {err}", orphan.display());
    }
    match open_integration(&path_only.path, &layout, "repo", "r1", "main") {
        Ok(int) => panic!("a path without a branch must be refused, got {int:?}"),
        Err(IntegrateError::BadState { detail, .. }) => {
            assert!(detail.contains("branch"), "{detail}");
        }
        Err(other) => panic!("expected BadState, got {other:?}"),
    }
}

/// An integration worktree an older build made excludes nothing; opening it
/// again repairs that, so a landing that reuses it does not see shprd's own
/// state as untracked work.
#[test]
fn reopening_an_integration_worktree_repairs_its_state_exclusions() {
    let repo = Repo::new();
    let layout = repo.layout();
    let int = open(&repo, &layout);

    // What an older build left behind.
    let exclude = repo.path.join(".git/info/exclude");
    if let Err(err) = std::fs::write(&exclude, "# exclude\n") {
        panic!("write {}: {err}", exclude.display());
    }
    if let Err(err) = std::fs::create_dir_all(int.path.join(".shprd")) {
        panic!("mkdir the state dir: {err}");
    }
    if let Err(err) = std::fs::write(int.path.join(".shprd/events.jsonl"), "{}\n") {
        panic!("write the state file: {err}");
    }

    let again = open(&repo, &layout);
    assert!(!again.created, "the second open reuses the worktree");
    let text = match std::fs::read_to_string(&exclude) {
        Ok(text) => text,
        Err(err) => panic!("read {}: {err}", exclude.display()),
    };
    for pattern in ["/.shprd/", "/.dagr/"] {
        assert!(
            text.lines().any(|line| line.trim() == pattern),
            "{pattern} was not repaired into:\n{text}"
        );
    }
    let status = git_in(&int.path, &["status", "--porcelain"]);
    assert!(
        status.is_empty(),
        "shprd's own state still shows as a change: {status}"
    );
}

#[test]
fn a_task_that_merged_a_moved_main_is_not_refused_for_scope() {
    let repo = Repo::new();
    let layout = repo.layout();
    let t = task(&repo, &layout, "T1", &[("src/a.txt", "a\n")]);
    for n in 0..100 {
        repo.commit(
            &format!("gained/f{n}.txt"),
            "x\n",
            &format!("main gains {n}"),
        );
    }
    git_in(&t.path, &["merge", "--no-edit", "main"]);
    let own = commit_in(&t.path, "src/b.txt", "b\n", "T1: b");
    let int = open(&repo, &layout);
    let mut claim = t.claim(&["src"]);
    claim.claimed = git_in(
        &t.path,
        &["rev-list", "--reverse", &format!("{}..HEAD", t.base_sha)],
    )
    .lines()
    .map(str::to_string)
    .collect();
    assert!(claim.claimed.contains(&own));
    // Whatever else the pick does with main's own commits, the scope is not
    // what stops it: the hundred files main gained are not the task's.
    if let Err(IntegrateError::OutsideScope { outside }) = integrate::integrate(&int, &claim) {
        panic!("main's files were blamed on the task: {outside:?}");
    }
}

#[test]
fn two_such_tasks_neither_overlap_nor_blame_each_other() {
    use shprd_git::changed::task_changes;
    let repo = Repo::new();
    repo.git(&["branch", "t3"]);
    repo.git(&["branch", "t4"]);
    for n in 0..100 {
        repo.commit(
            &format!("gained/g{n}.txt"),
            "x\n",
            &format!("main gains {n}"),
        );
    }
    let scopes = [("t3", "a"), ("t4", "b")];
    for (name, dir) in scopes {
        repo.git(&["checkout", name]);
        repo.git(&["merge", "--no-edit", "main"]);
        for file in ["one.rs", "two.rs"] {
            repo.commit(&format!("{dir}/{file}"), name, &format!("{name}: {file}"));
        }
    }
    repo.git(&["checkout", "main"]);
    let mut changes = Vec::new();
    for (name, _) in scopes {
        match task_changes(&repo.path, "main", name) {
            Ok(paths) => changes.push(paths),
            Err(err) => panic!("task changes: {err}"),
        }
    }
    assert_eq!(changes[0].len(), 2);
    assert_eq!(changes[1].len(), 2);
    // The engine's overlap check compares what each task claims (its scope);
    // the real changes sit inside those claims, so the check stays clear.
    let scope_a = vec!["a".to_string()];
    let scope_b = vec!["b".to_string()];
    assert!(!shprd_core::scope::overlaps(&scope_a, &scope_b));
    assert!(!shprd_core::scope::overlaps(&changes[0], &changes[1]));
}
