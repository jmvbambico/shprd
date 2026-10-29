//! The local janitor: what shprd may clean up, decided from its own ledger.
//!
//! Nothing here looks at the repository to decide what to delete. The plan is
//! pure — folded state plus the event log in, a list of [`Action`]s out — and
//! only names that are already in the ledger *and* inside shprd's namespace and
//! worktree root are ever planned. Archiving always precedes the branch
//! deletion that follows it, so a deleted branch stays recoverable, and a
//! worktree removal never deletes a branch: only a `Live` `Branch` resource
//! earns a [`Action::DeleteBranch`]. The one remote shprd touches is `origin`,
//! and only to delete a `Live` `RemoteBranch` that the merged pull request names
//! as its head.
//!
//! A finished run's review packet folder, `<home>/.shprd/reviews/<run>`, is
//! cleaned up the same way: copied aside first and deleted only once that copy
//! succeeded. It is shprd's own directory rather than a git object, and its
//! path is derived from the layout and the run id alone, so the plan — not the
//! ledger — is what proves it may go. The removal is still recorded, because a
//! cleanup the log cannot show is a cleanup nobody can audit: nothing may be
//! created once the run has finished, so a folder no earlier record held is
//! closed by a removal the ledger accepts without a prior creation.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use shprd_core::event::{Event, EventKind, ResourceAction, ResourceKind, TaskId};
use shprd_core::state::{ResourceStatus, RunState, TaskStatus};

use crate::archive::{self, ArchiveError};
use crate::git::Git;
use crate::worktree::{self, Layout};

/// Milliseconds in a day, for the archive-expiry arithmetic.
pub const MILLIS_PER_DAY: u64 = 86_400_000;

/// The name the janitor groups the run's integration resources under.
///
/// They are not a task's: shprd records them with no task at all. This is the
/// layout segment that names the integration worktree's directory, and no real
/// task id starts with `_`, so the two cannot collide.
const INTEGRATION_TASK: &str = "_int";

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
    /// Delete a branch on `origin` that was pushed for a merged pull request.
    ///
    /// This is the only action that talks to a remote, and it is only ever
    /// planned for a branch the ledger recorded `Live` *and* that the merged
    /// pull request names as its head — so shprd deletes a remote branch only
    /// when the forge has already taken its commits.
    DeleteRemoteBranch {
        /// The branch, as it is named on the remote.
        branch: String,
    },
    /// Archive a finished run's review packet folder, then delete it.
    ///
    /// The folder is `<home>/.shprd/reviews/<run>`. It is copied to
    /// [`review_archive_dir`] first and removed only once that copy succeeded,
    /// so a failed archive never loses a packet. The path is derived from the
    /// layout and run id, so it is always shprd's own directory — which is why
    /// a finished run's folder is cleaned whether or not an earlier record ever
    /// held it. The removal is what the ledger records.
    ArchiveReviewPackets {
        /// The run the packets belong to.
        run: String,
        /// The folder holding them.
        dir: PathBuf,
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
            Self::DeleteRemoteBranch { branch } => format!("delete remote branch {branch}"),
            Self::ArchiveReviewPackets { run, dir, .. } => {
                format!("archive review packets {run}: {}", dir.display())
            }
        }
    }

    /// The ledger events this step appends when it succeeds, in order.
    ///
    /// Archiving records two: the branch's archive ref now exists, and the
    /// branch is archived — kept, but out of service. `layout` names the ref,
    /// because the ref is named after the branch.
    ///
    /// The ref name comes from [`archive::archive_ref_for_branch`] or not at
    /// all: a branch it refuses has no archive ref, so this returns that
    /// refusal rather than recording a ref shprd never made.
    ///
    /// A review packet folder's step records its removal, whether or not the
    /// ledger ever held the folder: the folder is shprd's own by construction,
    /// and `execute` only yields these events for a folder it actually removed,
    /// so a second apply with nothing left records nothing.
    pub fn events(&self, layout: &Layout) -> Result<Vec<EventKind>, ArchiveError> {
        Ok(match self {
            Self::ArchiveRef { task, branch } => vec![
                EventKind::Resource {
                    action: ResourceAction::Created,
                    kind: ResourceKind::ArchiveRef,
                    name: archive::archive_ref_for_branch(layout, branch)?,
                    task: recorded_task(task),
                },
                EventKind::Resource {
                    action: ResourceAction::Archived,
                    kind: ResourceKind::Branch,
                    name: branch.clone(),
                    task: recorded_task(task),
                },
            ],
            Self::RemoveWorktree { task, path } => vec![EventKind::Resource {
                action: ResourceAction::Removed,
                kind: ResourceKind::Worktree,
                name: path.display().to_string(),
                task: recorded_task(task),
            }],
            Self::DeleteBranch { task, branch } => vec![EventKind::Resource {
                action: ResourceAction::Removed,
                kind: ResourceKind::Branch,
                name: branch.clone(),
                task: recorded_task(task),
            }],
            Self::ExpireArchive { task, ref_name } => vec![EventKind::Resource {
                action: ResourceAction::Removed,
                kind: ResourceKind::ArchiveRef,
                name: ref_name.clone(),
                task: Some(task.clone()),
            }],
            Self::DeleteRemoteBranch { branch } => vec![EventKind::Resource {
                action: ResourceAction::Removed,
                kind: ResourceKind::RemoteBranch,
                name: branch.clone(),
                task: None,
            }],
            Self::ArchiveReviewPackets { dir, .. } => vec![EventKind::Resource {
                action: ResourceAction::Removed,
                kind: ResourceKind::ReviewPackets,
                name: dir.display().to_string(),
                task: None,
            }],
        })
    }

    /// The task this step is about, when it is about one.
    ///
    /// A remote branch belongs to the run rather than to a task, so it has
    /// none.
    pub fn task(&self) -> Option<&TaskId> {
        match self {
            Self::ArchiveRef { task, .. }
            | Self::RemoveWorktree { task, .. }
            | Self::DeleteBranch { task, .. }
            | Self::ExpireArchive { task, .. } => Some(task),
            Self::DeleteRemoteBranch { .. } | Self::ArchiveReviewPackets { .. } => None,
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
///
/// Three things happen at the run's level rather than a task's. A merged pull
/// request releases the integration worktree and its branch, cleaned up in the
/// same order; it releases the remote branches it pushed, deleted from `origin`
/// once the forge already holds their commits; and the end of the run makes its
/// review packet folder spent, so that folder is archived and deleted.
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
    // A merged pull request releases the run's own integration branch too: its
    // commits are on the base branch, so the worktree that carried them and
    // then the branch itself are spent. Archiving still comes first, as it does
    // per task, so the branch stays recoverable.
    if state.pr.as_ref().is_some_and(|pr| pr.merged)
        && let Some(run) = &state.run
        && layout.owns_branch(&run.integration_branch)
    {
        let branch_live = is_live(state, &ResourceKind::Branch, &run.integration_branch);
        let task = || INTEGRATION_TASK.to_string();
        if branch_live {
            actions.push(Action::ArchiveRef {
                task: task(),
                branch: run.integration_branch.clone(),
            });
        }
        if let Some(path) = integration_worktree(state, layout, &run.run_id) {
            actions.push(Action::RemoveWorktree { task: task(), path });
        }
        if branch_live {
            actions.push(Action::DeleteBranch {
                task: task(),
                branch: run.integration_branch.clone(),
            });
        }
    }
    // A merged pull request releases what it pushed. The ledger must hold the
    // remote branch `Live` and the merge must name that exact branch as its
    // head: a stale or foreign name is never deleted from `origin`.
    if let Some(pr) = &state.pr
        && pr.merged
    {
        for ((kind, name), record) in &state.resources {
            if *kind != ResourceKind::RemoteBranch
                || record.status != ResourceStatus::Live
                || !layout.owns_branch(name)
                || is_protected_branch(name)
                || pr.head != *name
            {
                continue;
            }
            actions.push(Action::DeleteRemoteBranch {
                branch: name.clone(),
            });
        }
    }
    // A run's review packets are spent when the run ends. They are shprd's own
    // files, so the run id is the key, and the path is derived rather than read
    // from the ledger: a folder no earlier record held is cleaned all the same.
    if let Some(run) = &state.run
        && review_packets_due(state)
        && let Some(dir) = review_packets_dir(layout, &run.run_id)
    {
        actions.push(Action::ArchiveReviewPackets {
            run: run.run_id.clone(),
            dir,
        });
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
            // The events are built from the same refusal-aware lookup the
            // archive itself used, so a branch that cannot be named is a
            // failure here too — and its deletion is skipped with it.
            Action::ArchiveRef { branch, .. } => archive::archive(repo, layout, branch)
                .and_then(|_| action.events(layout))
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
                        .and_then(|events| events.map_err(|err| err.to_string()))
                }
            }
            Action::RemoveWorktree { path, .. } => {
                worktree::remove_worktree_only(repo, layout, path, false)
                    .map(|()| action.events(layout))
                    .map_err(|err| err.to_string())
                    .and_then(|events| events.map_err(|err| err.to_string()))
            }
            Action::DeleteRemoteBranch { branch } => {
                if !layout.owns_branch(branch) || is_protected_branch(branch) {
                    Err(format!("branch {branch} may not be deleted from origin"))
                } else {
                    Git::new(repo)
                        .run(&["push", "origin", "--delete", branch])
                        .map(|_| action.events(layout))
                        .map_err(|err| err.to_string())
                        .and_then(|events| events.map_err(|err| err.to_string()))
                }
            }
            Action::ExpireArchive { ref_name, .. } => archive::expire(repo, ref_name)
                .map(|()| action.events(layout))
                .map_err(|err| err.to_string())
                .and_then(|events| events.map_err(|err| err.to_string())),
            // The copy comes first and the deletion only after it: a folder
            // this step could not archive is left exactly where it was. A
            // folder that was already gone has nothing to record, so its step
            // succeeds with no event; a folder this step removed records that
            // removal, which is the audit trail the ledger keeps.
            Action::ArchiveReviewPackets { run, dir } => {
                match archive_review_packets(layout, run, dir) {
                    Ok(true) => action.events(layout).map_err(|err| err.to_string()),
                    Ok(false) => Ok(Vec::new()),
                    Err(err) => Err(err),
                }
            }
        };
        outcomes.push(Outcome {
            action: action.clone(),
            result,
        });
    }
    outcomes
}

/// The task a resource event records, or `None` for a run-level resource.
///
/// The integration worktree and its branch belong to the run, so their removal
/// is recorded without a task, exactly as their creation was.
fn recorded_task(task: &TaskId) -> Option<TaskId> {
    if task == INTEGRATION_TASK {
        None
    } else {
        Some(task.clone())
    }
}

/// True when the ledger holds `name` of `kind` as a `Live` resource.
fn is_live(state: &RunState, kind: &ResourceKind, name: &str) -> bool {
    state
        .resources
        .get(&(*kind, name.to_string()))
        .is_some_and(|record| record.status == ResourceStatus::Live)
}

/// The run's own integration worktree, when the ledger holds it `Live`.
///
/// It must sit at `<root>/<repo>/<run>/_int` inside the worktree root: anything
/// else is a human's worktree that happens to end in the same segment.
fn integration_worktree(state: &RunState, layout: &Layout, run_id: &str) -> Option<PathBuf> {
    let name = INTEGRATION_TASK;
    state
        .resources
        .iter()
        .filter(|((kind, _), record)| {
            *kind == ResourceKind::Worktree && record.status == ResourceStatus::Live
        })
        .map(|((_, path), _)| PathBuf::from(path))
        .find(|path| {
            path.file_name().and_then(|part| part.to_str()) == Some(name)
                && path
                    .parent()
                    .and_then(Path::file_name)
                    .and_then(|part| part.to_str())
                    == Some(run_id)
                && worktree::inside_root(&layout.root, path)
        })
}

/// Archives one review packet folder and deletes it, returning whether there
/// was a folder to remove.
///
/// A missing folder is not an error: a second apply finds nothing to do and
/// records nothing. The path is `<home>/.shprd/reviews/<run>` with a validated
/// run id, so it is always shprd's own folder — which is why a run that finished
/// before any earlier record existed still has its folder cleaned.
fn archive_review_packets(layout: &Layout, run: &str, dir: &Path) -> Result<bool, String> {
    // `symlink_metadata` rather than `is_dir`: a link is never followed out of
    // shprd's own folder, so a replacement link is reported, not archived.
    let meta = match std::fs::symlink_metadata(dir) {
        Ok(meta) => Some(meta),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => return Err(format!("{}: could not be read: {err}", dir.display())),
    };
    let Some(meta) = meta else {
        return Ok(false);
    };
    if !meta.is_dir() {
        return Err(format!("{}: not a folder", dir.display()));
    }
    let archived = review_archive_dir(layout, run)
        .ok_or_else(|| format!("{run}: not a usable run id for an archive path"))?;
    archive::archive_dir(dir, &archived).map_err(|err| err.to_string())?;
    std::fs::remove_dir_all(dir)
        .map_err(|err| format!("{}: could not be removed: {err}", dir.display()))?;
    Ok(true)
}

/// True when a run's review packet folder is spent: the run has finished.
///
/// The packets feed the pull request body and the review gate, so a run that
/// has not finished keeps its folders untouched — merged or not. Only the end
/// of the run makes them somebody else's cleanup.
pub fn review_packets_due(state: &RunState) -> bool {
    state.finished
}

/// The folder holding a run's review packets: `<home>/.shprd/reviews/<run>`.
///
/// The layout's root is always `<home>/.shprd/wt`, so its parent is the shprd
/// state directory the `reviews` folder sits beside. A run id that is not a
/// plain identifier has no folder: it must never name a path.
pub fn review_packets_dir(layout: &Layout, run: &str) -> Option<PathBuf> {
    if !worktree::valid_id(run) {
        return None;
    }
    Some(layout.root.parent()?.join("reviews").join(run))
}

/// Where a run's review packets are archived: `<home>/.shprd/archive/reviews/<run>`.
///
/// It sits beside the worktree root rather than inside it, so the archive is
/// not mistaken for a worktree the janitor may remove.
pub fn review_archive_dir(layout: &Layout, run: &str) -> Option<PathBuf> {
    if !worktree::valid_id(run) {
        return None;
    }
    Some(
        layout
            .root
            .parent()?
            .join("archive")
            .join("reviews")
            .join(run),
    )
}

/// True for the branches a run is cut from, which shprd deletes nowhere.
fn is_protected_branch(branch: &str) -> bool {
    matches!(branch, "main" | "master")
}

/// True when the task's resources are free to go: it landed or a human dropped
/// it, or the run is over and nothing is still working on it.
///
/// A dropped task is spent like an integrated one. Nobody is going to push its
/// branch now, so the worktree and the branch are only storage — and the plan
/// archives the branch before either is removed, so whatever the task did is
/// still recoverable with `shprd restore`. Keeping them until the run ended
/// would leave a dropped task's worktree in the way of nothing, but it would
/// also leave it on disk for ever in a run a human kept working in.
fn cleanup_due(state: &RunState, task: &TaskId) -> bool {
    match state.tasks.get(task) {
        Some(record) => {
            matches!(record.status, TaskStatus::Integrated | TaskStatus::Dropped)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use shprd_core::state::{PrInfo, RunInfo};

    /// The retention window a plan needs; review packets never use it.
    const DAYS: u32 = 30;

    /// A run state with nothing but the run in it, finished or not, with an
    /// optional merged pull request.
    fn a_run(finished: bool, merged: bool) -> RunState {
        RunState {
            run: Some(RunInfo {
                run_id: "r1".to_string(),
                repo: "/tmp/repo".to_string(),
                base_branch: "main".to_string(),
                integration_branch: "integration/m1".to_string(),
            }),
            finished,
            pr: merged.then(|| PrInfo {
                number: 7,
                url: "https://example.invalid/pr/7".to_string(),
                head: "shprd/r1/int".to_string(),
                approved_head: None,
                merged: true,
            }),
            ..RunState::default()
        }
    }

    /// A temp home with one review packet in `<reviews>/r1/T1-a1/`.
    fn home_with_packets() -> (tempfile::TempDir, Layout, PathBuf) {
        let temp = match tempfile::tempdir() {
            Ok(temp) => temp,
            Err(err) => panic!("tempdir: {err}"),
        };
        let layout = Layout::default_for_home(temp.path());
        let dir = match review_packets_dir(&layout, "r1") {
            Some(dir) => dir,
            None => panic!("a plain run id must have a reviews folder"),
        };
        let packet = dir.join("T1-a1");
        if let Err(err) = fs::create_dir_all(&packet) {
            panic!("mkdir {}: {err}", packet.display());
        }
        if let Err(err) = fs::write(packet.join("findings.json"), "{}") {
            panic!("write findings: {err}");
        }
        (temp, layout, dir)
    }

    /// The archive folder a run's packets go to, panicking when it has none.
    fn archive_of(layout: &Layout, run: &str) -> PathBuf {
        match review_archive_dir(layout, run) {
            Some(path) => path,
            None => panic!("a plain run id must have an archive folder"),
        }
    }

    #[test]
    fn a_finished_run_plans_its_review_packet_folder_without_touching_it() {
        let (_home, layout, dir) = home_with_packets();
        let state = a_run(true, false);

        let actions = plan(&state, &[], &layout, 1_000, DAYS);
        assert_eq!(actions.len(), 1, "{actions:?}");
        match &actions[0] {
            Action::ArchiveReviewPackets { run, dir: planned } => {
                assert_eq!(run, "r1");
                assert_eq!(planned, &dir);
            }
            other => panic!("expected review packets, got {other:?}"),
        }
        assert!(dir.is_dir(), "a plan deletes nothing");
        assert!(
            !archive_of(&layout, "r1").exists(),
            "a plan archives nothing"
        );
    }

    #[test]
    fn a_merged_run_that_has_not_finished_plans_no_review_packets() {
        let (_home, layout, dir) = home_with_packets();
        let state = a_run(false, true);

        let actions = plan(&state, &[], &layout, 1_000, DAYS);
        assert!(actions.is_empty(), "{actions:?}");
        assert!(
            dir.join("T1-a1/findings.json").is_file(),
            "an unfinished run keeps its packets, merged or not"
        );
    }

    #[test]
    fn a_run_that_has_not_finished_never_plans_or_touches_its_review_packets() {
        let (_home, layout, dir) = home_with_packets();
        let state = a_run(false, false);

        let actions = plan(&state, &[], &layout, 1_000, DAYS);
        assert!(actions.is_empty(), "{actions:?}");
        let outcomes = execute(&layout.root, &layout, &actions);
        assert!(outcomes.is_empty(), "{outcomes:?}");
        assert!(
            dir.join("T1-a1/findings.json").is_file(),
            "the folder must survive"
        );
    }

    #[test]
    fn applying_archives_the_review_packet_folder_before_deleting_it() {
        let (_home, layout, dir) = home_with_packets();
        let state = a_run(true, false);

        let actions = plan(&state, &[], &layout, 1_000, DAYS);
        let outcomes = execute(&layout.root, &layout, &actions);
        assert_eq!(outcomes.len(), 1, "{outcomes:?}");
        let kinds = match &outcomes[0].result {
            Ok(kinds) => kinds,
            Err(err) => panic!("the step must succeed: {err}"),
        };
        assert_eq!(kinds.len(), 1, "{kinds:?}");
        match &kinds[0] {
            EventKind::Resource {
                action,
                kind,
                name,
                task,
            } => {
                assert_eq!(*action, ResourceAction::Removed);
                assert_eq!(*kind, ResourceKind::ReviewPackets);
                assert_eq!(name, &dir.display().to_string());
                assert_eq!(task, &None);
            }
            other => panic!("expected a resource event, got {other:?}"),
        }
        assert!(!dir.exists(), "the source folder must be gone");
        assert!(
            archive_of(&layout, "r1")
                .join("T1-a1/findings.json")
                .is_file(),
            "the archive holds the packet"
        );
    }

    #[test]
    fn a_failed_archive_leaves_the_review_packet_folder_in_place() {
        let (_home, layout, dir) = home_with_packets();
        let state = a_run(true, false);
        // A file where the archive folder belongs, so the copy cannot start.
        let shprd = match layout.root.parent() {
            Some(path) => path,
            None => panic!("the layout root has a parent"),
        };
        let blocker = shprd.join("archive");
        if let Err(err) = fs::write(&blocker, "not a folder") {
            panic!("write {}: {err}", blocker.display());
        }

        let actions = plan(&state, &[], &layout, 1_000, DAYS);
        let outcomes = execute(&layout.root, &layout, &actions);
        assert_eq!(outcomes.len(), 1, "{outcomes:?}");
        assert!(outcomes[0].result.is_err(), "{outcomes:?}");
        assert!(
            dir.join("T1-a1/findings.json").is_file(),
            "a failed archive must not lose the folder"
        );
    }

    #[test]
    fn a_finished_run_cleans_a_review_folder_no_earlier_apply_recorded() {
        let (_home, layout, dir) = home_with_packets();
        // Finished without a record ever being written: no earlier apply adopted
        // the folder, and after `run finish` nothing may create one. The folder
        // is still shprd's own, so it is archived and deleted — and the removal
        // is recorded, because a cleanup the log cannot show cannot be audited.
        let state = a_run(true, false);

        let actions = plan(&state, &[], &layout, 1_000, DAYS);
        assert_eq!(actions.len(), 1, "{actions:?}");
        let outcomes = execute(&layout.root, &layout, &actions);
        assert_eq!(outcomes.len(), 1, "{outcomes:?}");
        let kinds = match &outcomes[0].result {
            Ok(kinds) => kinds,
            Err(err) => panic!("a finished run's folder must be cleaned: {err}"),
        };
        assert_eq!(kinds.len(), 1, "the removal must be recorded: {kinds:?}");
        match &kinds[0] {
            EventKind::Resource {
                action,
                kind,
                name,
                task,
            } => {
                assert_eq!(*action, ResourceAction::Removed);
                assert_eq!(*kind, ResourceKind::ReviewPackets);
                assert_eq!(name, &dir.display().to_string());
                assert_eq!(task, &None);
            }
            other => panic!("expected a resource event, got {other:?}"),
        }
        assert!(!dir.exists(), "the folder must be gone");
        assert!(
            archive_of(&layout, "r1")
                .join("T1-a1/findings.json")
                .is_file(),
            "the packets must survive in the archive"
        );
    }

    #[test]
    fn a_second_apply_records_nothing_when_the_folder_is_already_gone() {
        let (_home, layout, _dir) = home_with_packets();
        let state = a_run(true, false);

        let actions = plan(&state, &[], &layout, 1_000, DAYS);
        let first = execute(&layout.root, &layout, &actions);
        assert_eq!(first.len(), 1, "{first:?}");
        assert!(
            first[0]
                .result
                .as_ref()
                .is_ok_and(|kinds| !kinds.is_empty())
        );

        let again = execute(&layout.root, &layout, &actions);
        assert_eq!(again.len(), 1, "{again:?}");
        match &again[0].result {
            Ok(kinds) => assert!(
                kinds.is_empty(),
                "nothing was removed, so nothing is recorded: {kinds:?}"
            ),
            Err(err) => panic!("a spent folder is not a failure: {err}"),
        }
    }
}
