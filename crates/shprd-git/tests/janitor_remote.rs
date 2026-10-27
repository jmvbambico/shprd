//! The janitor's one remote: deleting branches the merged pull request pushed.
//!
//! Every test drives a real `origin` — a bare repository inside the test's temp
//! directory, never a real one — so nothing here reaches the network.

mod support;

use std::path::{Path, PathBuf};

use shprd_core::event::{Event, EventKind, ResourceAction, ResourceKind};
use shprd_core::state::RunState;
use shprd_git::janitor::{self, Action};
use shprd_git::worktree::Layout;

use support::{Repo, event, git_in, is_dir};

/// The default retention window the tests assume.
const DAYS: u32 = 30;
/// The run every fixture belongs to.
const RUN: &str = "r1";
/// The repository's directory name, as the layout would see it.
const REPO_NAME: &str = "repo";

/// Folds `events`, panicking when the log itself is invalid.
fn fold(events: &[Event]) -> RunState {
    match RunState::fold(events) {
        Ok(state) => state,
        Err(err) => panic!("the fixture log must fold: {err}"),
    }
}

/// Initializes a bare `origin` beside the repository and pushes `main` to it.
fn bare_origin(repo: &Repo) -> PathBuf {
    let path = repo.path.parent().unwrap_or(&repo.path).join("origin.git");
    let text = path.display().to_string();
    git_in(&repo.path, &["init", "--bare", &text]);
    repo.git(&["remote", "add", "origin", &text]);
    repo.git(&["push", "origin", "main"]);
    path
}

/// The branch names the bare repository at `origin` holds.
fn remote_heads(origin: &Path) -> Vec<String> {
    let listed = git_in(origin, &["for-each-ref", "--format=%(refname:short)"]);
    listed
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect()
}

/// The layout's integration worktree for `RUN`, created on its branch.
fn integration_worktree(repo: &Repo, layout: &Layout) -> PathBuf {
    let path = layout.path(REPO_NAME, RUN, "_int");
    if let Some(parent) = path.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        panic!("mkdir {}: {err}", parent.display());
    }
    let text = path.display().to_string();
    let branch = layout.branch_name(RUN, "int");
    repo.git(&["worktree", "add", "-b", &branch, &text, "main"]);
    path
}

/// The run's ledger: its integration worktree and branch, the pushed copy of
/// that branch, and the pull request proposing it — merged or not.
fn run_ledger(layout: &Layout, worktree: &Path, merged: bool) -> Vec<Event> {
    let branch = layout.branch_name(RUN, "int");
    let mut kinds = vec![
        EventKind::RunStarted {
            run_id: RUN.to_string(),
            repo: REPO_NAME.to_string(),
            base_branch: "main".to_string(),
            integration_branch: branch.clone(),
        },
        EventKind::Resource {
            action: ResourceAction::Created,
            kind: ResourceKind::Worktree,
            name: worktree.display().to_string(),
            task: None,
        },
        EventKind::Resource {
            action: ResourceAction::Created,
            kind: ResourceKind::Branch,
            name: branch.clone(),
            task: None,
        },
        EventKind::Resource {
            action: ResourceAction::Created,
            kind: ResourceKind::RemoteBranch,
            name: branch.clone(),
            task: None,
        },
        EventKind::PrOpened {
            number: 7,
            url: "https://example.invalid/pr/7".to_string(),
            head: branch.clone(),
        },
    ];
    if merged {
        kinds.push(EventKind::PrMerged {
            number: 7,
            head: branch,
        });
    }
    kinds
        .into_iter()
        .enumerate()
        .map(|(index, kind)| event(index as u64 + 1, 1_000, kind))
        .collect()
}

/// The events, with a `Live` remote branch recorded for `branch`.
fn with_remote(events: &[Event], branch: &str) -> Vec<Event> {
    let mut all = events.to_vec();
    let seq = all.len() as u64 + 1;
    all.push(event(
        seq,
        1_000,
        EventKind::Resource {
            action: ResourceAction::Created,
            kind: ResourceKind::RemoteBranch,
            name: branch.to_string(),
            task: None,
        },
    ));
    all
}

/// The branches the plan would delete from `origin`, in plan order.
fn remote_actions(actions: &[Action]) -> Vec<String> {
    actions
        .iter()
        .filter_map(|action| match action {
            Action::DeleteRemoteBranch { branch } => Some(branch.clone()),
            _ => None,
        })
        .collect()
}

/// Where `action` first matches `wanted` in the plan.
fn position(actions: &[Action], wanted: impl Fn(&Action) -> bool) -> usize {
    actions
        .iter()
        .position(wanted)
        .unwrap_or_else(|| panic!("the plan must hold the step: {actions:?}"))
}

#[test]
fn a_merged_run_deletes_its_pushed_integration_branch_from_origin() {
    let repo = Repo::new();
    let layout = repo.layout();
    let origin = bare_origin(&repo);
    let worktree = integration_worktree(&repo, &layout);
    let branch = layout.branch_name(RUN, "int");
    repo.git(&["push", "origin", &branch]);
    assert_eq!(
        remote_heads(&origin),
        vec!["main".to_string(), branch.clone()]
    );

    let events = run_ledger(&layout, &worktree, true);
    let state = fold(&events);
    let actions = janitor::plan(&state, &events, &layout, 2_000, DAYS);

    let archive = position(
        &actions,
        |action| matches!(action, Action::ArchiveRef { branch: name, .. } if *name == branch),
    );
    let remove = position(
        &actions,
        |action| matches!(action, Action::RemoveWorktree { path, .. } if *path == worktree),
    );
    let local = position(
        &actions,
        |action| matches!(action, Action::DeleteBranch { branch: name, .. } if *name == branch),
    );
    let remote = position(
        &actions,
        |action| matches!(action, Action::DeleteRemoteBranch { branch: name } if *name == branch),
    );
    assert!(
        archive < remove,
        "the worktree goes after the archive: {actions:?}"
    );
    assert!(
        remove < local,
        "the branch goes after the worktree: {actions:?}"
    );
    assert!(local < remote, "the remote copy goes last: {actions:?}");
    let outcomes = janitor::execute(&repo.path, &layout, &actions);
    let failed: Vec<String> = outcomes
        .iter()
        .filter_map(|outcome| outcome.result.as_ref().err().cloned())
        .collect();
    assert!(failed.is_empty(), "every step must succeed: {failed:?}");

    assert_eq!(remote_heads(&origin), vec!["main".to_string()]);
    assert!(!is_dir(&worktree), "the integration worktree is gone");
    assert_eq!(
        repo.git(&["branch", "--list", &branch]),
        "",
        "the integration branch is gone"
    );
    let recorded: Vec<EventKind> = outcomes
        .iter()
        .flat_map(|outcome| outcome.result.as_ref().ok().cloned().unwrap_or_default())
        .collect();
    let removed = recorded.iter().any(|kind| {
        matches!(
            kind,
            EventKind::Resource {
                action: ResourceAction::Removed,
                kind: ResourceKind::RemoteBranch,
                task: None,
                ..
            }
        )
    });
    assert!(
        removed,
        "the removal is recorded without a task: {recorded:?}"
    );
}

#[test]
fn an_open_pull_request_releases_nothing_on_origin() {
    let repo = Repo::new();
    let layout = repo.layout();
    let origin = bare_origin(&repo);
    let worktree = integration_worktree(&repo, &layout);
    let branch = layout.branch_name(RUN, "int");
    repo.git(&["push", "origin", &branch]);

    let events = run_ledger(&layout, &worktree, false);
    let state = fold(&events);
    let actions = janitor::plan(&state, &events, &layout, 2_000, DAYS);
    assert_eq!(
        remote_actions(&actions),
        Vec::<String>::new(),
        "an unmerged pull request releases nothing: {actions:?}"
    );
    assert!(
        !actions
            .iter()
            .any(|action| matches!(action, Action::DeleteBranch { .. })),
        "and neither is the integration branch deleted: {actions:?}"
    );

    janitor::execute(&repo.path, &layout, &actions);
    assert_eq!(
        remote_heads(&origin),
        vec!["main".to_string(), branch.clone()],
        "the pushed branch stays on origin"
    );
    assert!(is_dir(&worktree), "the worktree stays too");
}

#[test]
fn a_foreign_remote_branch_is_never_planned() {
    let repo = Repo::new();
    let layout = repo.layout();
    let worktree = integration_worktree(&repo, &layout);
    let branch = layout.branch_name(RUN, "int");

    let events = with_remote(&run_ledger(&layout, &worktree, true), "feature/x");
    let state = fold(&events);
    let actions = janitor::plan(&state, &events, &layout, 2_000, DAYS);
    assert_eq!(
        remote_actions(&actions),
        vec![branch.clone()],
        "only the run's own branch is planned: {actions:?}"
    );

    // Even a merged pull request naming a branch outside the namespace changes
    // nothing: shprd deletes only what it pushed.
    let mut named = with_remote(&run_ledger(&layout, &worktree, true), "feature/x");
    named.push(event(
        named.len() as u64 + 1,
        1_000,
        EventKind::PrMerged {
            number: 7,
            head: "feature/x".to_string(),
        },
    ));
    let state = fold(&named);
    let actions = janitor::plan(&state, &named, &layout, 2_000, DAYS);
    assert_eq!(
        remote_actions(&actions),
        Vec::<String>::new(),
        "a branch outside the namespace is never planned: {actions:?}"
    );
}

#[test]
fn main_is_never_planned() {
    let repo = Repo::new();
    let layout = repo.layout();
    let worktree = integration_worktree(&repo, &layout);
    let events = with_remote(&run_ledger(&layout, &worktree, true), "main");
    let mut events = events;
    events.push(event(
        events.len() as u64 + 1,
        1_000,
        EventKind::PrMerged {
            number: 7,
            head: "main".to_string(),
        },
    ));
    let state = fold(&events);
    let actions = janitor::plan(&state, &events, &layout, 2_000, DAYS);
    assert_eq!(
        remote_actions(&actions),
        Vec::<String>::new(),
        "the branch runs are cut from is never deleted: {actions:?}"
    );
}
