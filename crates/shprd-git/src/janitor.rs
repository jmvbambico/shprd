//! The local janitor: what shprd may clean up, decided from its own ledger.
//!
//! Nothing here looks at the repository to decide what to delete. The plan is
//! pure — folded state plus the event log in, a list of [`Action`]s out — and
//! only names that are already in the ledger *and* inside shprd's namespace and
//! worktree root are ever planned. Archiving always precedes the branch
//! deletion that follows it, so a deleted branch stays recoverable, and a
//! worktree removal never deletes a branch: only a `Live` `Branch` resource
//! earns a [`Action::DeleteBranch`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use shprd_core::event::{Event, EventKind, ResourceAction, ResourceKind, TaskId};
use shprd_core::state::{ResourceStatus, RunState, TaskStatus};

use crate::archive;
use crate::worktree::{self, Layout};

/// Milliseconds in a day, for the archive-expiry arithmetic.
pub const MILLIS_PER_DAY: u64 = 86_400_000;

/// One step the janitor would take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Point the branch's own archive ref at its tip.
    ///
    /// The ref is named after the branch, so two live branches for one task
    /// are archived to two refs and never collide.
    ArchiveRef {
        /// The task.
        task: TaskId,
        /// The branch to archive.
        branch: String,
    },
    /// Remove a worktree shprd created, keeping its branch.
    ///
    /// The branch is not deleted here: it may be one the ledger never recorded
    /// creating. A recorded branch goes through [`Action::DeleteBranch`].
    RemoveWorktree {
        /// The task it was created for.
        task: TaskId,
        /// Where it lives.
        path: PathBuf,
    },
    /// Delete a local branch shprd created, after it was archived.
    DeleteBranch {
        /// The task it was created for.
        task: TaskId,
        /// The branch.
        branch: String,
    },
    /// Delete an archive ref whose age has passed the retention window.
    ExpireArchive {
        /// The task it holds history for.
        task: TaskId,
        /// The ref.
        ref_name: String,
    },
}

impl Action {
    /// A one-line, human-readable description of the step.
    pub fn describe(&self) -> String {
        match self {
            Self::ArchiveRef { task, branch } => {
                format!("archive {task}: {branch} -> refs/shprd/archive/")
            }
            Self::RemoveWorktree { task, path } => {
                format!("remove worktree {task}: {}", path.display())
            }
            Self::DeleteBranch { task, branch } => format!("delete branch {task}: {branch}"),
            Self::ExpireArchive { task, ref_name } => {
                format!("expire archive {task}: {ref_name}")
            }
        }
    }

    /// The ledger events this step appends when it succeeds, in order.
    ///
    /// Archiving records two: the branch's archive ref now exists, and the
    /// branch is archived — kept, but out of service. `layout` names the ref,
    /// because the ref is named after the branch.
    pub fn events(&self, layout: &Layout) -> Vec<EventKind> {
        match self {
            Self::ArchiveRef { task, branch } => vec![
                EventKind::Resource {
                    action: ResourceAction::Created,
                    kind: ResourceKind::ArchiveRef,
                    name: archive::archive_ref_for_branch(layout, branch)
                        .unwrap_or_else(|_| archive::archive_ref_name("", branch)),
                    task: Some(task.clone()),
                },
                EventKind::Resource {
                    action: ResourceAction::Archived,
                    kind: ResourceKind::Branch,
                    name: branch.clone(),
                    task: Some(task.clone()),
                },
            ],
            Self::RemoveWorktree { task, path } => vec![EventKind::Resource {
                action: ResourceAction::Removed,
                kind: ResourceKind::Worktree,
                name: path.display().to_string(),
                task: Some(task.clone()),
            }],
            Self::DeleteBranch { task, branch } => vec![EventKind::Resource {
                action: ResourceAction::Removed,
                kind: ResourceKind::Branch,
                name: branch.clone(),
                task: Some(task.clone()),
            }],
            Self::ExpireArchive { task, ref_name } => vec![EventKind::Resource {
                action: ResourceAction::Removed,
                kind: ResourceKind::ArchiveRef,
                name: ref_name.clone(),
                task: Some(task.clone()),
            }],
        }
    }

    /// The task this step is about.
    pub fn task(&self) -> &TaskId {
        match self {
            Self::ArchiveRef { task, .. }
            | Self::RemoveWorktree { task, .. }
            | Self::DeleteBranch { task, .. }
            | Self::ExpireArchive { task, .. } => task,
        }
    }
}

/// What one executed action did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// The step that ran.
    pub action: Action,
    /// The ledger events to append, in order, or the reason nothing was
    /// appended.
    pub result: Result<Vec<EventKind>, String>,
}

/// Plans the janitor's work. Pure: no git, no filesystem, no clock.
///
/// A resource is only ever considered while it is `Live` and present in the
/// ledger, and only when its task is `Integrated` — or when the run is
/// finished and the task is not still `Working`. Expiring an archive ref
/// depends on the age of the event that created it, nothing else.
///
/// Per task the order is archive, then worktree, then branch: a branch that is
/// checked out in a worktree cannot be deleted before that worktree is gone,
/// and nothing may be deleted before its commits are held by an archive ref.
/// A task whose resources are being cleaned up never has its archive ref
/// expired in the same plan, so cleanup cannot delete its own safety net.
pub fn plan(
    state: &RunState,
    events: &[Event],
    layout: &Layout,
    now_ms: u64,
    archive_days: u32,
) -> Vec<Action> {
    if state.run.is_none() {
        return Vec::new();
    }
    let mut per_task: BTreeMap<TaskId, DueResources> = BTreeMap::new();
    let mut stale: Vec<Action> = Vec::new();
    for ((kind, name), record) in &state.resources {
        if record.status != ResourceStatus::Live {
            continue;
        }
        let Some(task) = record.task.clone() else {
            continue;
        };
        match kind {
            ResourceKind::Worktree => {
                if !cleanup_due(state, &task) {
                    continue;
                }
                let path = PathBuf::from(name);
                if !worktree::inside_root(&layout.root, &path) {
                    continue;
                }
                per_task.entry(task).or_default().worktree = Some(path);
            }
            ResourceKind::Branch => {
                if !cleanup_due(state, &task) || !layout.owns_branch(name) {
                    continue;
                }
                per_task
                    .entry(task)
                    .or_default()
                    .branches
                    .push(name.clone());
            }
            ResourceKind::ArchiveRef => {
                if !archive::is_archive_ref(name) {
                    continue;
                }
                let created = created_at(events, record.created_seq);
                if created.is_some_and(|at_ms| is_expired(at_ms, now_ms, archive_days)) {
                    stale.push(Action::ExpireArchive {
                        task,
                        ref_name: name.clone(),
                    });
                }
            }
            _ => {}
        }
    }
    let mut actions = Vec::new();
    for (task, due) in per_task {
        for branch in &due.branches {
            actions.push(Action::ArchiveRef {
                task: task.clone(),
                branch: branch.clone(),
            });
        }
        if let Some(path) = due.worktree {
            actions.push(Action::RemoveWorktree {
                task: task.clone(),
                path,
            });
        }
        for branch in due.branches {
            actions.push(Action::DeleteBranch {
                task: task.clone(),
                branch,
            });
        }
    }
    for action in stale {
        if actions
            .iter()
            .any(|planned| planned.task() == action.task())
        {
            continue;
        }
        actions.push(action);
    }
    actions
}

/// What one task's ledger says is safe to clean up.
#[derive(Debug, Default)]
struct DueResources {
    /// The worktree shprd created for it, when it is inside the root.
    worktree: Option<PathBuf>,
    /// The branches shprd created for it, inside the namespace.
    branches: Vec<String>,
}

/// Executes `actions` against the repository at `repo`, one at a time.
///
/// A failing action does not stop the others; its [`Outcome`] carries the
/// reason instead of an event. A branch deletion whose own archive failed is
/// skipped, so a branch is never lost by a half-done cleanup — and only that
/// branch: another task's, or another branch's, cleanup still runs.
pub fn execute(repo: &Path, layout: &Layout, actions: &[Action]) -> Vec<Outcome> {
    let mut outcomes: Vec<Outcome> = Vec::with_capacity(actions.len());
    let mut failed_archive: Vec<String> = Vec::new();
    for action in actions {
        let result = match action {
            Action::ArchiveRef { branch, .. } => archive::archive(repo, layout, branch)
                .map(|_| action.events(layout))
                .map_err(|err| {
                    failed_archive.push(branch.clone());
                    err.to_string()
                }),
            Action::DeleteBranch { task: _, branch } => {
                if failed_archive.contains(branch) {
                    Err(format!("skipped: no archive ref for {branch}"))
                } else if !layout.owns_branch(branch) {
                    Err(format!("branch {branch} is outside the shprd namespace"))
                } else {
                    worktree::delete_branch(repo, layout, branch)
                        .map(|()| action.events(layout))
                        .map_err(|err| err.to_string())
                }
            }
            Action::RemoveWorktree { path, .. } => {
                worktree::remove_worktree_only(repo, layout, path, false)
                    .map(|()| action.events(layout))
                    .map_err(|err| err.to_string())
            }
            Action::ExpireArchive { ref_name, .. } => archive::expire(repo, ref_name)
                .map(|()| action.events(layout))
                .map_err(|err| err.to_string()),
        };
        outcomes.push(Outcome {
            action: action.clone(),
            result,
        });
    }
    outcomes
}

/// True when the task's resources are free to go: it landed, or the run is over
/// and nothing is still working on it.
fn cleanup_due(state: &RunState, task: &TaskId) -> bool {
    match state.tasks.get(task) {
        Some(record) => {
            record.status == TaskStatus::Integrated
                || (state.finished && record.status != TaskStatus::Working)
        }
        None => state.finished,
    }
}

/// When the creation that made the current record was appended, if the log
/// still holds it.
///
/// The record carries the seq of its own `Created` event, so a resource that
/// was removed and created again is aged from the re-creation rather than from
/// the first time it existed.
fn created_at(events: &[Event], seq: u64) -> Option<u64> {
    events
        .iter()
        .find(|event| event.seq == seq)
        .map(|event| event.at_ms)
}

/// True when an archive ref created at `created_ms` has outlived
/// `archive_days`.
fn is_expired(created_ms: u64, now_ms: u64, archive_days: u32) -> bool {
    let window = u64::from(archive_days).saturating_mul(MILLIS_PER_DAY);
    now_ms.saturating_sub(created_ms) > window
}
