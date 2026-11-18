//! The two routes that belong to a task rather than to the run: adding one, and
//! reading what its branch changed.
//!
//! `POST /api/tasks` is the person's `shprd agent task add`. It appends the same
//! `task_added` event the CLI does, under the service's own actor, so a task a
//! board added is a task the engine starts and the CLI lists — one event, one
//! set of rules, nothing to keep in step. The only thing it decides by itself is
//! the task's id, because a form asks for a title and not for an identifier: the
//! id is made from the title and never collides with a task the run holds.
//!
//! `GET /api/tasks/{id}/changes` is what the accept card shows beside the
//! hand-in: the files the task's branch changed under the branch its attempt was
//! cut from, with line counts. It reads git, not the log — the log says an
//! attempt exists, and only the repository says what it did.
//!
//! Both are read-only against the machine except for the one append, so both are
//! safe to ask often.

use std::path::Path;

use axum::body::Bytes;
use shprd_core::event::EventKind;
use shprd_core::state::RunState;
use shprd_git::changed;
use shprd_git::git::Git;

use crate::api::AppState;
use crate::dto::{
    ActionReply, COMMITS_LISTED, CommitDto, FileChangeDto, NewTaskDto, TaskChangesDto,
};
use crate::error::ApiReject;
use crate::hub::Hub;
use crate::ledger;
use crate::service::AfterWrite;

/// How long a task id made from a title may be.
///
/// It goes into a branch name, a worktree folder and a tab title, all of which
/// a person reads; past this it is a sentence, not a name.
pub const ID_MAX: usize = 40;

/// The id a task is stored under when a caller names none.
///
/// A title becomes its words, lower-cased, joined by `-`: ASCII letters, digits
/// and the `-` between them, because that is what survives a branch name and a
/// path untouched. A title with nothing usable in it is `task`. When the run
/// already holds the id, a number is added — `-2`, `-3` — until one is free, so
/// two tasks with the same title are two tasks rather than a refusal.
pub fn task_id(title: &str, taken: impl Fn(&str) -> bool) -> String {
    let mut slug = String::new();
    for word in title
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
    {
        let word = word.to_ascii_lowercase();
        let needed = slug.len() + usize::from(!slug.is_empty()) + word.len();
        if needed > ID_MAX {
            break;
        }
        if !slug.is_empty() {
            slug.push('-');
        }
        slug.push_str(&word);
    }
    if slug.is_empty() {
        slug.push_str("task");
    }
    if !taken(&slug) {
        return slug;
    }
    for n in 2u32.. {
        let suffix = format!("-{n}");
        let room = ID_MAX.saturating_sub(suffix.len());
        let mut candidate = slug.clone();
        candidate.truncate(room);
        candidate.push_str(&suffix);
        if !taken(&candidate) {
            return candidate;
        }
    }
    unreachable!("the counter cannot run out before every id is taken")
}

/// `POST /api/tasks`: adds one task and answers with it, folded.
///
/// The body is [`NewTaskDto`]; a title, a scope and an acceptance line are what
/// the rules need, and a missing one is the rule's refusal rather than a second
/// copy of the check here.
pub(crate) fn add(
    state: &AppState,
    hub: &Hub,
    after_write: Option<&AfterWrite>,
    body: &Bytes,
) -> Result<ActionReply, ApiReject> {
    let request: NewTaskDto = serde_json::from_slice(body).map_err(|err| {
        ApiReject::bad_request(format!(
            "the body is not a task: {err}; send {{\"title\": \"…\", \"scope\": [\"…\"], \
             \"acceptance\": [\"…\"]}}"
        ))
    })?;
    let (folded, _) = ledger::load(&state.repo)
        .map_err(|err| ApiReject::internal(format!("could not read the run log: {err}")))?;
    let id = match request.id.as_deref().map(str::trim) {
        Some(id) if !id.is_empty() => id.to_string(),
        // An id that is only spaces is a caller that meant to name one and did
        // not: it is refused rather than replaced, because the answer would be a
        // task under a name it did not ask for.
        Some(_) => {
            return Err(ApiReject::bad_request(
                "an \"id\" that is only spaces is not one: leave it out and the task is named \
                 from its title",
            ));
        }
        None => task_id(&request.title, |id| folded.tasks.contains_key(id)),
    };
    let kind = EventKind::TaskAdded {
        task: id,
        title: request.title,
        scope: request.scope,
        acceptance: request.acceptance,
        tags: request.tags,
        skills: request.skills,
        deps: request.deps,
    };
    // The same commit path every other write takes: the rules check it, the log
    // is appended under its lock, the waiters wake and the binary's after-write
    // step runs.
    let ctx = crate::actions::Context {
        repo: &state.repo,
        actor: &state.actor,
        hub,
        after_write,
        engine: state.engine.as_ref(),
        merge: None,
    };
    crate::actions::commit(&ctx, kind)
}

/// `GET /api/tasks/{id}/changes`: the task's branch against its base.
///
/// The base is the run's integration branch, which is the branch every attempt
/// is cut from and the branch the task lands on — not the run's base branch,
/// which is where the integration branch itself came from. Diffing against the
/// latter lists every file the other tasks landed while this one worked (M10
/// gate bug 8).
///
/// The diff runs from the merge base, so it is the task's own work and not
/// everything its base branch moved while the agent worked. A task with no
/// attempt has no branch to compare and is refused as a wrong state — not found
/// would say the task does not exist, and it does.
pub(crate) fn changes(state: &AppState, task_id: &str) -> Result<TaskChangesDto, ApiReject> {
    let (folded, _) = ledger::load(&state.repo)
        .map_err(|err| ApiReject::internal(format!("could not read the run log: {err}")))?;
    changes_in(&state.repo, &folded, task_id)
}

/// [`changes`] against an already-folded run, so a test drives it without a log.
pub(crate) fn changes_in(
    repo: &Path,
    folded: &RunState,
    task_id: &str,
) -> Result<TaskChangesDto, ApiReject> {
    let task = folded
        .tasks
        .get(task_id)
        .ok_or_else(|| ApiReject::unknown_task(task_id))?;
    let run = folded.run.as_ref().ok_or_else(|| {
        ApiReject::refused("no run has started: a task has no base branch to compare against")
    })?;
    let attempt = task.latest_attempt().ok_or_else(|| {
        ApiReject::refused(format!(
            "{task_id} has no attempt yet: nothing has been written on its branch"
        ))
    })?;
    // The attempt was cut from the run's integration branch (`SpawnEnv::base_branch`
    // in `spawn.rs`), and that is also where a landing puts its commits. The
    // integration branch's name is fixed for the run, so merge-basing against its
    // current tip still gives the commit this attempt branched from, even after
    // other tasks landed ahead of it.
    let base = run.integration_branch.clone();
    let branch = attempt.branch.clone();
    let git = Git::new(repo);
    let merge_base = git
        .run(&["merge-base", &base, &branch])
        .map_err(|err| git_failure(task_id, &base, &branch, &err.to_string()))?
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    if merge_base.is_empty() {
        return Err(ApiReject::refused(format!(
            "{task_id}'s branch {branch} and {base} share no commit: there is nothing to compare"
        )));
    }
    let stats = changed::file_stats(repo, &merge_base, &branch)
        .map_err(|err| git_failure(task_id, &base, &branch, &err.to_string()))?;
    let files: Vec<FileChangeDto> = stats
        .into_iter()
        .map(|stat| FileChangeDto {
            path: stat.path,
            added: stat.added,
            deleted: stat.deleted,
            binary: stat.binary,
        })
        .collect();
    let range = format!("{merge_base}..{branch}");
    let commit_count = git
        .run(&["rev-list", "--count", &range])
        .map_err(|err| git_failure(task_id, &base, &branch, &err.to_string()))?
        .trim()
        .parse::<u64>()
        .unwrap_or(0);
    let limit = format!("--max-count={COMMITS_LISTED}");
    let commits: Vec<CommitDto> = git
        .run(&["log", &limit, "--format=%h%x09%s", &range])
        .map_err(|err| git_failure(task_id, &base, &branch, &err.to_string()))?
        .lines()
        .filter_map(|line| {
            let (sha, subject) = line.split_once('\t')?;
            Some(CommitDto {
                sha: sha.to_string(),
                subject: subject.to_string(),
            })
        })
        .collect();
    Ok(TaskChangesDto {
        commit_count,
        commits,
        task: task_id.to_string(),
        added: files.iter().map(|file| file.added).sum(),
        deleted: files.iter().map(|file| file.deleted).sum(),
        base,
        branch,
        merge_base,
        files,
    })
}

/// One refusal for a git command that did not answer.
///
/// It is a refusal rather than a server error: the repository is the person's,
/// and a branch that was deleted or a worktree that was never made is something
/// they can fix. The git line is kept, because it is what says which of the two
/// it was.
fn git_failure(task: &str, base: &str, branch: &str, why: &str) -> ApiReject {
    ApiReject::refused(format!(
        "could not compare {task}'s branch {branch} with {base}: {why}"
    ))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn a_title_becomes_a_name_a_branch_can_carry() {
        assert_eq!(
            task_id("Add the M10.5 board", |_| false),
            "add-the-m10-5-board"
        );
        assert_eq!(
            task_id("  spaces   and\ttabs ", |_| false),
            "spaces-and-tabs"
        );
        assert_eq!(task_id("Ünïcode?", |_| false), "n-code");
        assert_eq!(task_id("!!!", |_| false), "task");
        assert_eq!(task_id("", |_| false), "task");
    }

    #[test]
    fn a_long_title_is_cut_at_a_word() {
        let long = task_id(
            "a title that is much too long to be a task id because it is a sentence",
            |_| false,
        );
        assert!(long.len() <= ID_MAX, "{long}");
        assert!(!long.ends_with('-'), "{long}");
        assert!(long.starts_with("a-title-that-is"), "{long}");
    }

    #[test]
    fn a_name_the_run_already_holds_gets_a_number() {
        let taken = |id: &str| id == "tabs" || id == "tabs-2";
        assert_eq!(task_id("Tabs", taken), "tabs-3");
        assert_eq!(task_id("Tabs", |_| false), "tabs");
        // The suffix keeps the id inside the limit.
        let long = "a title that is much too long to be a task id because it is a";
        let id = task_id(long, |id| id == task_id(long, |_| false));
        assert!(id.len() <= ID_MAX, "{id}");
        assert!(id.ends_with("-2"), "{id}");
    }
}
