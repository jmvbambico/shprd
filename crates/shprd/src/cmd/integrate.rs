//! `shprd agent integrate`: landing a passed task's commits on the run's
//! integration branch.
//!
//! The git side of the work lives in [`shprd_git::integrate`]; this module
//! checks the ledger, finds what the task promises, and records the outcome.

use std::path::Path;

use anyhow::{Context, anyhow};
use shprd_core::event::{EventKind, Finding, Severity};
use shprd_core::state::Task;
use shprd_git::git::Git;
use shprd_git::integrate::{self, IntegrateError, TaskCommits};
use shprd_git::worktree::Layout;

use crate::cli::IntegrateArgs;
use crate::cmd::emit;
use crate::ctx::{CliError, Ctx, resolve_home};

/// How many characters of a sha the human line shows.
const SHORT_SHA: usize = 7;

/// Lands a passed task's commits, or records a landing a caller did by hand.
///
/// The task has to be `Passed` with no unanswered failed gate, which is what
/// the `Integrated` rule checks, so a refused task touches no git state.
pub fn verb(ctx: &Ctx, args: &IntegrateArgs, home: Option<&Path>) -> Result<(), CliError> {
    if args.record_only {
        return record_only(ctx, args);
    }
    let landed = EventKind::Integrated {
        task: args.task.clone(),
        commits: Vec::new(),
    };
    ctx.check(&landed)?;
    let state = ctx.state()?;
    let run = state
        .run
        .as_ref()
        .ok_or_else(|| CliError::other(anyhow!("no run has started")))?;
    let task = state
        .tasks
        .get(&args.task)
        .cloned()
        .ok_or_else(|| CliError::other(anyhow!("unknown task {:?}", args.task)))?;
    let branch = latest_branch(&task)?;
    let home = resolve_home(home)?;
    let layout = Layout::default_for_home(&home);
    let repo_name = repo_name(ctx)?;
    let _lock = lock_run(&home, &repo_name, &run.run_id)?;
    let worktree = integrate::open_integration(
        &ctx.root,
        &layout,
        &repo_name,
        &run.run_id,
        &run.base_branch,
    )
    .map_err(|err| CliError::other(anyhow!(err).context("opening the integration worktree")))?;
    let base_sha = merge_base(&ctx.root, &branch, &worktree.branch)?;
    let claimed = rev_list(&ctx.root, &base_sha, &branch)?;
    let commits = TaskCommits {
        branch,
        base_sha,
        claimed,
        scope: task.scope.clone(),
    };
    let picked = match integrate::integrate(&worktree, &commits) {
        Ok(picked) => picked,
        Err(IntegrateError::Conflict { commit, files }) => {
            return send_back(ctx, args, &task, &commit, &files);
        }
        Err(err) => return Err(failed(err)),
    };
    let event = ctx.commit(EventKind::Integrated {
        task: args.task.clone(),
        commits: picked.picked.clone(),
    })?;
    emit(ctx, &event)?;
    println!(
        "integrated {}: {} commit(s), head {}",
        args.task,
        picked.picked.len(),
        short(&picked.new_head)
    );
    Ok(())
}

/// Records that a task's commits landed, without touching a repository.
///
/// This is the M1 path: the caller did the merge and only needs the ledger to
/// know about it, so no commit list is invented here.
fn record_only(ctx: &Ctx, args: &IntegrateArgs) -> Result<(), CliError> {
    let event = ctx.commit(EventKind::Integrated {
        task: args.task.clone(),
        commits: Vec::new(),
    })?;
    emit(ctx, &event)
}

/// The branch the task's latest attempt committed on.
fn latest_branch(task: &Task) -> Result<String, CliError> {
    match task.attempts.last() {
        Some(attempt) if !attempt.branch.trim().is_empty() => Ok(attempt.branch.clone()),
        Some(_) => Err(CliError::other(anyhow!(
            "task {:?} has no branch to integrate",
            task.id
        ))),
        None => Err(CliError::other(anyhow!(
            "task {:?} was never attempted",
            task.id
        ))),
    }
}

/// The name of the repository directory, which keeps repos apart on disk.
fn repo_name(ctx: &Ctx) -> Result<String, CliError> {
    ctx.root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| {
            CliError::other(anyhow!(
                "cannot name the repository: {} has no last path component",
                ctx.root.display()
            ))
        })
}

/// The commit both branches share, which is where the picks start.
fn merge_base(repo: &Path, branch: &str, integration: &str) -> Result<String, CliError> {
    let git = Git::new(repo);
    let base = git
        .run(&["merge-base", branch, integration])
        .map_err(|err| CliError::other(anyhow!(err).context("finding the merge base")))?;
    let base = base.trim().to_string();
    if base.is_empty() {
        return Err(CliError::other(anyhow!(
            "{branch:?} and {integration:?} share no commit"
        )));
    }
    Ok(base)
}

/// The commits on `branch` after `base`, oldest first.
fn rev_list(repo: &Path, base: &str, branch: &str) -> Result<Vec<String>, CliError> {
    let range = format!("{base}..{branch}");
    let raw = Git::new(repo)
        .run(&["rev-list", "--reverse", &range])
        .map_err(|err| CliError::other(anyhow!(err).context("listing the task's commits")))?;
    Ok(raw
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect())
}

/// Turns a failed integration into a process failure with its cause.
fn failed(err: IntegrateError) -> CliError {
    CliError::other(anyhow!(err).context("integrating the task"))
}

/// The short form of a sha, for the human line.
fn short(sha: &str) -> &str {
    sha.get(..SHORT_SHA).unwrap_or(sha)
}

/// One run's exclusive integration lock, released when it is dropped.
///
/// The lock is an advisory `flock` on a file under `<home>/.shprd/locks`, so it
/// is held by this process and not by a stray directory or pid file: the OS
/// drops it when the process exits, however it exits.
struct RunLock {
    /// The open file the lock sits on. Closing it releases the lock.
    _file: std::fs::File,
}

/// Takes the run's integration lock, blocking until it is free.
///
/// Two `integrate` runs on the same run would otherwise cherry-pick onto the
/// same branch at the same time, and both would see the head the other is
/// about to move.
fn lock_run(home: &Path, repo_name: &str, run: &str) -> Result<RunLock, CliError> {
    let dir = home.join(".shprd").join("locks");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating {}", dir.display()))
        .map_err(CliError::other)?;
    let path = dir.join(format!("{repo_name}-{run}-int.lock"));
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))
        .map_err(CliError::other)?;
    file.lock()
        .with_context(|| format!("locking {}", path.display()))
        .map_err(CliError::other)?;
    Ok(RunLock { _file: file })
}

/// Records a conflicting pick as a failed `integrate` gate and fails with
/// exit 2, so the task goes back to `Failed` and can be spawned again.
fn send_back(
    ctx: &Ctx,
    args: &IntegrateArgs,
    task: &Task,
    commit: &str,
    files: &[String],
) -> Result<(), CliError> {
    let attempt = task.attempts.last().map(|attempt| attempt.n).unwrap_or(1);
    let finding = Finding {
        severity: Severity::Major,
        title: "conflicts with the integration branch".to_string(),
        location: files.first().cloned(),
        detail: format!("cherry-pick of {commit} conflicts in {}", files.join(", ")),
    };
    let event = ctx.commit(EventKind::GateFailed {
        task: args.task.clone(),
        attempt,
        gate: "integrate".to_string(),
        findings: vec![finding],
    })?;
    emit(ctx, &event)?;
    Err(CliError::Invalid(anyhow!(
        "integrate {}: {} conflict(s) with the integration branch, task marked failed",
        args.task,
        files.len()
    )))
}
