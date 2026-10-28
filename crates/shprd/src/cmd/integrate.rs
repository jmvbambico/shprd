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
use shprd_mux::Mux;

use crate::cli::IntegrateArgs;
use crate::cmd::{emit, spawn};
use crate::ctx::{CliError, Ctx, resolve_home};

/// How many characters of a sha the human line shows.
const SHORT_SHA: usize = 7;

/// Lands a passed task's commits, or records a landing a caller did by hand.
///
/// The task has to be `Passed` with no unanswered failed gate, which is what
/// the `Integrated` rule checks, so a refused task touches no git state.
pub fn verb(
    mux: &dyn Mux,
    ctx: &Ctx,
    args: &IntegrateArgs,
    home: Option<&Path>,
) -> Result<(), CliError> {
    if args.record_only {
        return record_only(mux, ctx, args, home);
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
    close_worker_tab(mux, ctx, Some(&home), &args.task);
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
fn record_only(
    mux: &dyn Mux,
    ctx: &Ctx,
    args: &IntegrateArgs,
    home: Option<&Path>,
) -> Result<(), CliError> {
    let event = ctx.commit(EventKind::Integrated {
        task: args.task.clone(),
        commits: Vec::new(),
    })?;
    close_worker_tab(mux, ctx, home, &args.task);
    emit(ctx, &event)
}

/// Closes the worker tab an integrated task can no longer come back to.
///
/// Integration is the one event after which no attempt of the task can run in
/// its tab again: a failed review sends a task back only while it is not yet
/// integrated. That is why the tab outlives `reported` and ends here. A close
/// the host refuses is warned about, never fatal — the landing is recorded, and
/// run end sweeps whatever a refusal leaves behind.
fn close_worker_tab(mux: &dyn Mux, ctx: &Ctx, home: Option<&Path>, task: &str) {
    let Ok(home) = resolve_home(home) else {
        return;
    };
    let Ok(state) = ctx.state() else {
        return;
    };
    let Some(run) = state.run.as_ref().map(|run| run.run_id.clone()) else {
        return;
    };
    let briefs = home.join(".shprd").join("briefs");
    if let Err(err) = spawn::close_worker(mux, &briefs, &run, task) {
        eprintln!("warning: {err}");
    }
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
        source: None,
    })?;
    emit(ctx, &event)?;
    Err(CliError::Invalid(anyhow!(
        "integrate {}: {} conflict(s) with the integration branch, task marked failed",
        args.task,
        files.len()
    )))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::path::PathBuf;
    use std::process::Command;

    use shprd_core::event::{Actor, ReportStatus};
    use shprd_core::gate::REVIEW_GATE;
    use shprd_mux::fake::FakeMux;
    use shprd_mux::{TabId, naming};
    use tempfile::TempDir;

    use super::*;

    /// Runs one git command in `dir`, panicking on failure.
    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap_or_else(|err| panic!("git {args:?}: {err}"));
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A repository with `main`, a task branch one commit ahead, and a home.
    struct Scratch {
        repo: TempDir,
        home: TempDir,
    }

    impl Scratch {
        fn new() -> Scratch {
            let repo = TempDir::new().unwrap_or_else(|err| panic!("{err}"));
            let home = TempDir::new().unwrap_or_else(|err| panic!("{err}"));
            let root = repo.path();
            git(root, &["init", "-q", "-b", "main"]);
            git(root, &["config", "user.name", "shprd tests"]);
            git(root, &["config", "user.email", "tests@example.invalid"]);
            git(root, &["config", "commit.gpgsign", "false"]);
            std::fs::write(root.join("README.md"), "scratch\n").unwrap();
            git(root, &["add", "."]);
            git(root, &["commit", "-q", "-m", "first"]);
            git(root, &["checkout", "-q", "-b", "feature/t1"]);
            std::fs::write(root.join("README.md"), "scratch\nand the work\n").unwrap();
            git(root, &["commit", "-q", "-a", "-m", "t1 work"]);
            git(root, &["checkout", "-q", "main"]);
            Scratch { repo, home }
        }

        fn root(&self) -> &Path {
            self.repo.path()
        }

        fn repo_name(&self) -> String {
            self.root()
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_default()
        }

        /// The briefs directory the run records its tabs under.
        fn briefs(&self) -> PathBuf {
            self.home.path().join(".shprd").join("briefs")
        }

        /// A run with `T1` spawned on the task branch, unreported.
        fn ctx(&self) -> Ctx {
            let ctx = Ctx::new(self.root().to_path_buf(), Actor::Human, false, true);
            ctx.commit(EventKind::RunStarted {
                run_id: "t1".to_string(),
                repo: self.repo_name(),
                base_branch: "main".to_string(),
                integration_branch: "shprd/t1/int".to_string(),
            })
            .unwrap_or_else(|err| panic!("{err}"));
            ctx.commit(EventKind::TaskAdded {
                task: "T1".to_string(),
                title: "Land the work".to_string(),
                scope: vec!["README.md".to_string()],
                acceptance: vec!["it lands".to_string()],
                tags: Vec::new(),
                skills: Vec::new(),
                deps: Vec::new(),
            })
            .unwrap_or_else(|err| panic!("{err}"));
            ctx.commit(EventKind::Spawned {
                task: "T1".to_string(),
                attempt: 1,
                agent: "sh".to_string(),
                model: None,
                worktree: "/tmp/wt/T1".to_string(),
                branch: "feature/t1".to_string(),
            })
            .unwrap_or_else(|err| panic!("{err}"));
            ctx
        }

        /// Records the attempt's report, the step that must not close the tab.
        fn reported(&self, ctx: &Ctx) {
            ctx.commit(EventKind::Reported {
                task: "T1".to_string(),
                attempt: 1,
                status: ReportStatus::Completed,
                summary: "did the work".to_string(),
                tests: vec!["cargo test".to_string()],
                changed_paths: vec!["README.md".to_string()],
                commit: None,
                skills_used: Vec::new(),
            })
            .unwrap_or_else(|err| panic!("{err}"));
        }

        /// Records the review pass that makes the task integrable.
        fn passed(&self, ctx: &Ctx) {
            self.reported(ctx);
            ctx.commit(EventKind::GatePassed {
                task: "T1".to_string(),
                attempt: 1,
                gate: REVIEW_GATE.to_string(),
                findings: Vec::new(),
                source: None,
            })
            .unwrap_or_else(|err| panic!("{err}"));
        }

        /// A host with this repository's workspace, and the worker tab the
        /// attempt recorded beside its brief.
        fn mux_with_worker(&self) -> (FakeMux, String) {
            let mux = FakeMux::default();
            let workspace = mux
                .create_workspace(&naming::workspace_name(&self.repo_name()), self.root())
                .unwrap_or_else(|err| panic!("{err}"));
            let tab = mux
                .create_tab(&workspace.id, "worker", self.root())
                .unwrap_or_else(|err| panic!("{err}"));
            let record = spawn::tab_path(&self.briefs(), "t1", "T1", 1);
            std::fs::create_dir_all(self.briefs().join("t1")).unwrap_or_else(|err| panic!("{err}"));
            std::fs::write(record, tab.id.as_str()).unwrap_or_else(|err| panic!("{err}"));
            (mux, tab.id.to_string())
        }

        /// Whether `tab` is still open on `mux`.
        fn still_open(&self, mux: &FakeMux, tab: &str) -> bool {
            let workspace = mux
                .find_workspace(&naming::workspace_name(&self.repo_name()))
                .unwrap_or_else(|err| panic!("{err}"))
                .unwrap_or_else(|| panic!("the workspace is gone"));
            mux.list_tabs(&workspace.id)
                .unwrap_or_else(|err| panic!("{err}"))
                .iter()
                .any(|open| open.id == TabId::new(tab))
        }
    }

    /// The one-shot args a test integrates `T1` with.
    fn args() -> IntegrateArgs {
        IntegrateArgs {
            task: "T1".to_string(),
            record_only: false,
        }
    }

    #[test]
    fn a_reported_task_keeps_its_worker_tab() {
        // A failed review sends the task back to the same worker, so the report
        // cannot end its tab: only integration takes that possibility away.
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        scratch.reported(&ctx);
        let (mux, tab) = scratch.mux_with_worker();

        assert!(
            scratch.still_open(&mux, &tab),
            "a reported task's worker tab must stay open"
        );
    }

    #[test]
    fn integrating_a_task_closes_its_worker_tab() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        scratch.passed(&ctx);
        let (mux, tab) = scratch.mux_with_worker();
        assert!(
            scratch.still_open(&mux, &tab),
            "the attempt left no worker tab to close"
        );

        verb(&mux, &ctx, &args(), Some(scratch.home.path())).unwrap_or_else(|err| panic!("{err}"));

        assert_eq!(
            ctx.state().unwrap_or_else(|err| panic!("{err}")).tasks["T1"].status,
            shprd_core::state::TaskStatus::Integrated,
            "the landing was not recorded"
        );
        assert!(
            !scratch.still_open(&mux, &tab),
            "the worker tab {tab} outlived the task's integration"
        );
    }

    #[test]
    fn a_vanished_worker_tab_does_not_fail_the_integration() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        scratch.passed(&ctx);
        let (mux, tab) = scratch.mux_with_worker();
        // A human closed the tab first: that is the state integration wants.
        mux.close_tab(&TabId::new(&tab))
            .unwrap_or_else(|err| panic!("{err}"));

        verb(&mux, &ctx, &args(), Some(scratch.home.path())).unwrap_or_else(|err| panic!("{err}"));
    }
}
