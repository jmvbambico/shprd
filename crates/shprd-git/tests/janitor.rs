//! The ledger-driven janitor: what it plans, and what executes out of it.

mod support;

use std::path::Path;

use shprd_core::event::{Event, EventKind, ReportStatus, ResourceAction, ResourceKind};
use shprd_core::state::RunState;
use shprd_git::archive;
use shprd_git::janitor::{self, Action};
use shprd_git::worktree;

use support::{Repo, event, integrated_run};

/// The default retention window the tests assume.
const DAYS: u32 = 30;

/// Folds `events`, panicking when the log itself is invalid.
fn fold(events: &[Event]) -> RunState {
    match RunState::fold(events) {
        Ok(state) => state,
        Err(err) => panic!("the fixture log must fold: {err}"),
    }
}

/// The events, with extra ledger entries appended.
fn with_extra(events: &[Event], kind: ResourceKind, name: &str, task: &str) -> Vec<Event> {
    let mut all = events.to_vec();
    let seq = all.len() as u64 + 1;
    let at_ms = all.last().map(|event| event.at_ms).unwrap_or(0);
    all.push(event(
        seq,
        at_ms,
        EventKind::Resource {
            action: ResourceAction::Created,
            kind,
            name: name.to_string(),
            task: Some(task.to_string()),
        },
    ));
    all
}

/// The events, with a second task added and an archive ref recorded for it.
fn archived_task(events: &[Event], task: &str, ref_name: &str) -> Vec<Event> {
    let mut all = events.to_vec();
    let mut seq = all.len() as u64 + 1;
    let at_ms = all.last().map(|event| event.at_ms).unwrap_or(0);
    all.push(event(
        seq,
        at_ms,
        EventKind::TaskAdded {
            task: task.to_string(),
            title: "the archived task".to_string(),
            scope: vec!["crates/x".to_string()],
            acceptance: vec!["it works".to_string()],
            tags: Vec::new(),
            skills: Vec::new(),
            deps: Vec::new(),
        },
    ));
    seq += 1;
    all.push(event(
        seq,
        at_ms,
        EventKind::Resource {
            action: ResourceAction::Created,
            kind: ResourceKind::ArchiveRef,
            name: ref_name.to_string(),
            task: Some(task.to_string()),
        },
    ));
    all
}

/// The `at_ms` of the last event in `events`.
fn all_at(events: &[Event]) -> u64 {
    events.last().map(|event| event.at_ms).unwrap_or(0)
}

#[test]
fn never_plans_a_branch_outside_the_namespace_or_a_worktree_outside_the_root() {
    let repo = Repo::new();
    let layout = repo.layout();
    let outside = repo.path.join("outside-wt");
    let events = integrated_run("r1", "T1", "feature/x", &outside, 1_000);
    let state = fold(&events);

    let actions = janitor::plan(&state, &events, &layout, 1_000, DAYS);
    assert!(actions.is_empty(), "nothing may be planned: {actions:?}");

    let outcomes = janitor::execute(&repo.path, &layout, &actions);
    assert!(outcomes.is_empty(), "{outcomes:?}");
    repo.git(&["branch", "feature/x", "main"]);
    assert!(
        !repo.git(&["branch", "--list", "feature/x"]).is_empty(),
        "a branch shprd did not create must survive"
    );
}

#[test]
fn archives_before_deleting_and_restores_the_branch_afterwards() {
    let repo = Repo::new();
    let layout = repo.layout();
    let created = match worktree::create(&repo.path, &layout, "r1", "T1", "main") {
        Ok(created) => created,
        Err(err) => panic!("create must succeed: {err}"),
    };
    support::git_in(&created.path, &["commit", "--allow-empty", "-m", "work"]);
    let tip = support::git_in(&created.path, &["rev-parse", "HEAD"]);

    let events = integrated_run("r1", "T1", &created.branch, &created.path, 1_000);
    let state = fold(&events);
    let actions = janitor::plan(&state, &events, &layout, 2_000, DAYS);
    assert_eq!(actions.len(), 3, "{actions:?}");
    let archive_at = match actions
        .iter()
        .position(|action| matches!(action, Action::ArchiveRef { .. }))
    {
        Some(at) => at,
        None => panic!("an archive step: {actions:?}"),
    };
    let delete_at = match actions
        .iter()
        .position(|action| matches!(action, Action::DeleteBranch { .. }))
    {
        Some(at) => at,
        None => panic!("a delete step: {actions:?}"),
    };
    assert!(
        archive_at < delete_at,
        "archive must come first: {actions:?}"
    );

    let outcomes = janitor::execute(&repo.path, &layout, &actions);
    for outcome in &outcomes {
        assert!(outcome.result.is_ok(), "{outcome:?}");
    }
    assert!(!created.path.exists(), "the worktree must be gone");
    assert!(
        repo.git(&["branch", "--list", &created.branch]).is_empty(),
        "the branch must be gone"
    );

    let branch = match archive::restore(&repo.path, &layout, "r1", "T1") {
        Ok(branch) => branch,
        Err(err) => panic!("restore must succeed: {err}"),
    };
    assert_eq!(branch, created.branch);
    assert_eq!(repo.git(&["rev-parse", &branch]), tip, "the same commit");
}

#[test]
fn a_worktree_is_removed_without_deleting_an_unrecorded_branch() {
    let repo = Repo::new();
    let layout = repo.layout();
    let created = match worktree::create(&repo.path, &layout, "r1", "T1", "main") {
        Ok(created) => created,
        Err(err) => panic!("create must succeed: {err}"),
    };
    // The ledger knows the worktree but never recorded the branch.
    let events: Vec<Event> = integrated_run("r1", "T1", &created.branch, &created.path, 1_000)
        .into_iter()
        .filter(|event| {
            !matches!(
                &event.kind,
                EventKind::Resource {
                    action: ResourceAction::Created,
                    kind: ResourceKind::Branch,
                    ..
                }
            )
        })
        .collect();
    let state = fold(&events);
    assert!(
        !state
            .resources
            .contains_key(&(ResourceKind::Branch, created.branch.clone())),
        "the fixture must not record the branch"
    );

    let actions = janitor::plan(&state, &events, &layout, 2_000, DAYS);
    let outcomes = janitor::execute(&repo.path, &layout, &actions);
    for outcome in &outcomes {
        assert!(outcome.result.is_ok(), "{outcome:?}");
    }
    assert!(!created.path.exists(), "the worktree must be gone");
    assert!(
        !repo.git(&["branch", "--list", &created.branch]).is_empty(),
        "a branch the ledger never recorded must survive"
    );
}

#[test]
fn plans_nothing_while_the_task_is_still_working() {
    let repo = Repo::new();
    let layout = repo.layout();
    let created = match worktree::create(&repo.path, &layout, "r1", "T1", "main") {
        Ok(created) => created,
        Err(err) => panic!("create must succeed: {err}"),
    };
    let mut events = integrated_run("r1", "T1", &created.branch, &created.path, 1_000);
    events.truncate(5);
    let state = fold(&events);
    assert_eq!(
        state.tasks.get("T1").map(|task| task.status),
        Some(shprd_core::state::TaskStatus::Working)
    );
    let actions = janitor::plan(&state, &events, &layout, 2_000, DAYS);
    assert!(
        actions.is_empty(),
        "a working task must be left alone: {actions:?}"
    );
    assert!(created.path.exists(), "the worktree must survive");
}

#[test]
fn expires_an_archive_ref_only_after_the_retention_window() {
    let repo = Repo::new();
    let layout = repo.layout();
    let ref_name = archive::archive_ref_name("r1", "T2");
    let base = integrated_run(
        "r1",
        "T1",
        "shprd/r1/T1",
        &layout.root.join("repo/r1/T1"),
        1_000,
    );
    let events = archived_task(&base, "T2", &ref_name);
    let state = fold(&events);
    let window = u64::from(DAYS) * janitor::MILLIS_PER_DAY;

    let fresh = janitor::plan(&state, &events, &layout, 1_000 + window, DAYS);
    assert!(
        !fresh
            .iter()
            .any(|action| matches!(action, Action::ExpireArchive { .. })),
        "a ref exactly at the window must stay: {fresh:?}"
    );
    let stale = janitor::plan(&state, &events, &layout, 1_001 + window, DAYS);
    assert!(
        stale
            .iter()
            .any(|action| matches!(action, Action::ExpireArchive { .. })),
        "a ref past the window must expire: {stale:?}"
    );
}

#[test]
fn archive_age_is_measured_from_the_current_creation() {
    let repo = Repo::new();
    let layout = repo.layout();
    let ref_name = archive::archive_ref_name("r1", "T2");
    let base = integrated_run(
        "r1",
        "T1",
        "shprd/r1/T1",
        &layout.root.join("repo/r1/T1"),
        1_000,
    );
    let mut events = archived_task(&base, "T2", &ref_name);
    let mut seq = events.len() as u64 + 1;
    let at_ms = all_at(&events);
    events.push(event(
        seq,
        at_ms,
        EventKind::Resource {
            action: ResourceAction::Removed,
            kind: ResourceKind::ArchiveRef,
            name: ref_name.clone(),
            task: Some("T2".to_string()),
        },
    ));
    seq += 1;
    let recreated_ms = 9_000;
    events.push(event(
        seq,
        recreated_ms,
        EventKind::Resource {
            action: ResourceAction::Created,
            kind: ResourceKind::ArchiveRef,
            name: ref_name.clone(),
            task: Some("T2".to_string()),
        },
    ));
    let state = fold(&events);
    let window = u64::from(DAYS) * janitor::MILLIS_PER_DAY;

    let fresh = janitor::plan(&state, &events, &layout, recreated_ms + window, DAYS);
    assert!(
        !fresh
            .iter()
            .any(|action| matches!(action, Action::ExpireArchive { .. })),
        "age counts from the re-creation: {fresh:?}"
    );
    let stale = janitor::plan(&state, &events, &layout, recreated_ms + window + 1, DAYS);
    assert!(
        stale
            .iter()
            .any(|action| matches!(action, Action::ExpireArchive { .. })),
        "a re-created ref still expires: {stale:?}"
    );
}

#[test]
fn a_failed_archive_skips_its_branch_deletion_and_the_rest_still_runs() {
    let repo = Repo::new();
    let layout = repo.layout();
    let gone = "shprd/r1/GONE";
    let kept = "shprd/r1/KEPT";
    repo.git(&["branch", kept, "main"]);
    let events = integrated_run("r1", "T1", gone, &layout.root.join("repo/r1/T1"), 1_000);
    let events = with_extra(&events, ResourceKind::Branch, kept, "T1");
    let state = fold(&events);
    let actions = janitor::plan(&state, &events, &layout, 2_000, DAYS);
    assert!(actions.len() >= 4, "{actions:?}");

    let outcomes = janitor::execute(&repo.path, &layout, &actions);
    let failed_delete = outcomes
        .iter()
        .find(|outcome| matches!(&outcome.action, Action::DeleteBranch { branch, .. } if branch == gone))
        .and_then(|outcome| outcome.result.as_ref().err().cloned());
    assert!(
        failed_delete.is_some_and(|err| err.contains("skipped")),
        "{outcomes:?}"
    );
    assert!(
        repo.git(&["branch", "--list", kept]).is_empty(),
        "the other branch must still be cleaned up"
    );
}

#[test]
fn two_live_branches_for_one_task_are_archived_to_distinct_refs() {
    let repo = Repo::new();
    let layout = repo.layout();
    let second = "shprd/r1/T1-retry";
    let created = match worktree::create(&repo.path, &layout, "r1", "T1", "main") {
        Ok(created) => created,
        Err(err) => panic!("create must succeed: {err}"),
    };
    let tip = support::commit_in(&created.path, "work.txt", "work\n", "work");
    repo.git(&["branch", second, &tip]);
    let first = created.branch.as_str();
    let events = integrated_run("r1", "T1", first, &created.path, 1_000);
    let events = with_extra(&events, ResourceKind::Branch, second, "T1");
    let state = fold(&events);

    let actions = janitor::plan(&state, &events, &layout, 2_000, DAYS);
    let outcomes = janitor::execute(&repo.path, &layout, &actions);
    for outcome in &outcomes {
        assert!(outcome.result.is_ok(), "{outcome:?}");
    }
    for branch in [first, second] {
        assert!(
            repo.git(&["branch", "--list", branch]).is_empty(),
            "{branch} must be deleted"
        );
    }
    // The task's own branch is attempt 1's; the extra branch keeps its own name.
    let first_ref = "refs/shprd/archive/r1/T1-a1";
    let second_ref = "refs/shprd/archive/r1/T1-retry";
    assert_ne!(first_ref, second_ref);
    for ref_name in [first_ref, second_ref] {
        assert_eq!(
            repo.git(&["rev-parse", ref_name]),
            tip,
            "{ref_name} must hold the branch tip"
        );
    }
}

#[test]
fn execute_reports_the_event_for_each_step() {
    let repo = Repo::new();
    let layout = repo.layout();
    let branch = "shprd/r1/T1";
    repo.git(&["branch", branch, "main"]);
    let events = integrated_run("r1", "T1", branch, &layout.root.join("repo/r1/T1"), 1_000);
    let state = fold(&events);
    let actions: Vec<Action> = janitor::plan(&state, &events, &layout, 2_000, DAYS)
        .into_iter()
        .filter(|action| {
            matches!(
                action,
                Action::ArchiveRef { .. } | Action::DeleteBranch { .. }
            )
        })
        .collect();
    let outcomes = janitor::execute(&repo.path, &layout, &actions);
    assert_eq!(outcomes.len(), 2, "{outcomes:?}");
    let first = match &outcomes[0].result {
        Ok(kinds) => kinds.clone(),
        Err(err) => panic!("the archive must succeed: {err}"),
    };
    assert_eq!(first.len(), 2, "the ref and the branch: {first:?}");
    match first[0].clone() {
        EventKind::Resource {
            action,
            kind,
            name,
            task,
        } => {
            assert_eq!(action, ResourceAction::Created);
            assert_eq!(kind, ResourceKind::ArchiveRef);
            assert_eq!(name, archive::archive_ref_for_attempt("r1", "T1", 1));
            assert_eq!(task.as_deref(), Some("T1"));
        }
        other => panic!("expected a resource event, got {other:?}"),
    }
    match first[1].clone() {
        EventKind::Resource {
            action,
            kind,
            name,
            task,
        } => {
            assert_eq!(action, ResourceAction::Archived);
            assert_eq!(kind, ResourceKind::Branch);
            assert_eq!(name, "shprd/r1/T1");
            assert_eq!(task.as_deref(), Some("T1"));
        }
        other => panic!("expected a resource event, got {other:?}"),
    }
}

#[test]
fn an_outside_worktree_path_is_never_executed_even_if_the_branch_is_ours() {
    let repo = Repo::new();
    let layout = repo.layout();
    let outside: &Path = &repo.path.join("outside-wt");
    let text = outside.display().to_string();
    repo.git(&["worktree", "add", "-b", "shprd/r1/OUT", &text, "main"]);
    let events = integrated_run("r1", "T1", "shprd/r1/OUT", outside, 1_000);
    let state = fold(&events);
    let actions = janitor::plan(&state, &events, &layout, 2_000, DAYS);
    assert!(
        !actions
            .iter()
            .any(|action| matches!(action, Action::RemoveWorktree { .. })),
        "the worktree must not be planned: {actions:?}"
    );
    let _ = janitor::execute(&repo.path, &layout, &actions);
    assert!(outside.is_dir(), "the worktree must survive");
}

#[test]
fn a_branch_outside_the_namespace_is_never_archived_and_a_ref_outside_it_is_refused() {
    let repo = Repo::new();
    let layout = repo.layout();
    let outside = "not-ours/T1";
    repo.git(&["branch", outside, "main"]);
    let events = integrated_run("r1", "T1", outside, &layout.root.join("repo/r1/T1"), 1_000);
    let state = fold(&events);
    let actions = janitor::plan(&state, &events, &layout, 2_000, DAYS);
    assert!(
        !actions
            .iter()
            .any(|action| matches!(action, Action::ArchiveRef { .. })),
        "a branch outside the namespace has no archive ref: {actions:?}"
    );

    // A step that names a ref outside the archive namespace writes nothing and
    // reports the refusal.
    let action = Action::ArchiveRef {
        task: "T1".to_string(),
        branch: outside.to_string(),
        ref_name: "refs/heads/kept".to_string(),
    };
    let outcomes = janitor::execute(&repo.path, &layout, &[action]);
    assert_eq!(outcomes.len(), 1, "{outcomes:?}");
    assert!(
        outcomes[0].result.is_err(),
        "the refusal is reported, not an event: {outcomes:?}"
    );
    assert!(!repo.git(&["branch", "--list", "kept"]).contains("kept"));
}

/// The events of a run where `T1` ran twice on one branch, so both attempts
/// worked on the same branch name, and the second attempt ended as asked: landed
/// on the integration branch, or dropped by a human.
fn retried_events(worktree: &Path, dropped: bool) -> Vec<Event> {
    let branch = "shprd/r1/T1".to_string();
    let path = worktree.display().to_string();
    let task = || "T1".to_string();
    let report = |attempt: u32, status: ReportStatus| EventKind::Reported {
        task: task(),
        attempt,
        status,
        summary: "as reported".to_string(),
        tests: Vec::new(),
        changed_paths: Vec::new(),
        commit: None,
        skills_used: Vec::new(),
    };
    let mut kinds = vec![
        EventKind::RunStarted {
            run_id: "r1".to_string(),
            repo: "/tmp/repo".to_string(),
            base_branch: "main".to_string(),
            integration_branch: "integration/m1".to_string(),
        },
        EventKind::TaskAdded {
            task: task(),
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
            name: branch.clone(),
            task: Some(task()),
        },
        EventKind::Resource {
            action: ResourceAction::Created,
            kind: ResourceKind::Worktree,
            name: path.clone(),
            task: Some(task()),
        },
        EventKind::Spawned {
            task: task(),
            attempt: 1,
            agent: "claude".to_string(),
            model: None,
            worktree: path.clone(),
            branch: branch.clone(),
        },
        report(1, ReportStatus::Failed),
        EventKind::Spawned {
            task: task(),
            attempt: 2,
            agent: "claude".to_string(),
            model: None,
            worktree: path.clone(),
            branch: branch.clone(),
        },
    ];
    if dropped {
        kinds.push(EventKind::TaskDropped {
            task: task(),
            reason: "no longer worth it".to_string(),
        });
    } else {
        kinds.push(report(2, ReportStatus::Completed));
        kinds.push(EventKind::GatePassed {
            task: task(),
            attempt: 2,
            gate: "lint".to_string(),
            findings: Vec::new(),
            source: None,
        });
        kinds.push(EventKind::Integrated {
            task: task(),
            commits: vec!["abc123".to_string()],
        });
    }
    kinds
        .into_iter()
        .enumerate()
        .map(|(index, kind)| event(index as u64 + 1, 5_000, kind))
        .collect()
}

/// Archives attempt 1, leaves attempt 2's commit on the same branch, and runs
/// the janitor over the ledger the retry left behind.
///
/// Returns the repository, the worktree path, and both attempts' commits.
fn cleanup_after_a_retry(dropped: bool) -> (Repo, std::path::PathBuf, String, String) {
    let repo = Repo::new();
    let layout = repo.layout();
    let created = match worktree::create(&repo.path, &layout, "r1", "T1", "main") {
        Ok(created) => created,
        Err(err) => panic!("create must succeed: {err}"),
    };
    // The retry archived attempt 1 under its own ref before it reused the branch.
    let first = support::commit_in(&created.path, "one.txt", "one\n", "attempt one");
    let first_ref = archive::archive_ref_for_attempt("r1", "T1", 1);
    if let Err(err) = archive::archive_to(&repo.path, &created.branch, &first_ref) {
        panic!("archiving attempt 1 must succeed: {err}");
    }
    let second = support::commit_in(&created.path, "two.txt", "two\n", "attempt two");

    let events = retried_events(&created.path, dropped);
    let events = with_extra(&events, ResourceKind::ArchiveRef, &first_ref, "T1");
    let state = fold(&events);
    let actions = janitor::plan(&state, &events, &layout, 6_000, DAYS);
    for outcome in janitor::execute(&repo.path, &layout, &actions) {
        assert!(outcome.result.is_ok(), "{outcome:?}");
    }
    (repo, created.path, first, second)
}

#[test]
fn a_retried_task_is_archived_under_its_final_attempts_ref_and_then_cleaned_up() {
    let (repo, path, _, second) = cleanup_after_a_retry(false);
    let final_ref = archive::archive_ref_for_attempt("r1", "T1", 2);
    assert_eq!(
        repo.git(&["rev-parse", &final_ref]),
        second,
        "attempt 2's work is kept under its own ref"
    );
    assert!(
        !path.exists(),
        "the worktree must be gone: the archive could not collide"
    );
    assert!(
        repo.git(&["branch", "--list", "shprd/r1/T1"]).is_empty(),
        "the branch must be gone once its archive exists"
    );
}

#[test]
fn a_dropped_final_attempt_is_archived_under_its_own_ref_and_cleaned_up() {
    let (repo, path, first, second) = cleanup_after_a_retry(true);
    let first_ref = archive::archive_ref_for_attempt("r1", "T1", 1);
    let final_ref = archive::archive_ref_for_attempt("r1", "T1", 2);
    assert_eq!(
        repo.git(&["rev-parse", &first_ref]),
        first,
        "attempt 1's archive is untouched"
    );
    assert_eq!(repo.git(&["rev-parse", &final_ref]), second);
    assert!(!path.exists(), "the worktree must be gone");
    assert!(
        repo.git(&["branch", "--list", "shprd/r1/T1"]).is_empty(),
        "the dropped attempt's branch must be gone, not stranded"
    );
}
