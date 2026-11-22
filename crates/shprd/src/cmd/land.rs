//! `shprd land`: one task's own pull request, or (before M8.3) the run's.
//!
//! Landing is the only place shprd talks to a forge, and the only place it
//! pushes. With a task named, the task's branch gets its own pull request into
//! the run's base branch, and `[land] merge` decides what happens next: `ask`
//! waits for the human's `shprd approve <task>`, `checks` for the forge's
//! checks, `auto` merges at once. A protected base branch always asks. Landing
//! refuses a task whose latest attempt has no passing hand-in or no
//! engine-recorded cross-vendor review, because a pull request nobody checked
//! is not a landing.
//!
//! A repository with no remote, or with no usable forge, has nothing to push
//! and no pull request to open: with a task named, the same landing rules apply
//! locally, merging the task's branch into the run's integration branch inside
//! shprd's own worktree. A conflict there is undone and escalated.
//!
//! Without a task, the old run-level path runs unchanged: the run's integration
//! branch leaves as one pull request, everything it decides comes from the log
//! (the run must be active, no task may still be in flight, at least one task
//! must have landed, and the run-wide gate must have passed on the very head
//! being pushed), and `--merge` applies `[land] merge` to it. A refusal writes
//! nothing and exits 2.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::anyhow;
use shprd_core::config::{self, Config, MergePolicy};
use shprd_core::event::{Event, EventKind, ResourceAction, ResourceKind};
use shprd_core::gate::REVIEW_GATE;
use shprd_core::rules;
use shprd_core::state::{RunState, Task, TaskStatus};
use shprd_git::forge::{FakeForge, Forge, GhForge};
use shprd_git::git::Git;
use shprd_git::integrate;
use shprd_git::land;
use shprd_git::sync;

use crate::cli::LandArgs;
use crate::cmd::run_lock;
use crate::ctx::{CliError, Ctx, resolve_home, resolve_layout};

/// Environment variable naming a scripted forge, used by the integration tests.
///
/// Production runs always use `gh`; only a test sets this, and then every forge
/// call goes to the script named here instead of the network.
pub const FAKE_FORGE_ENV: &str = "SHPRD_FAKE_FORGE";

/// The line a task's body ends with when its review was independent.
///
/// The composition itself lives in `shprd_views::pr::task_pr_body_marked`, so
/// the service's tick pushes the same bytes this verb does.
const REVIEW_PASSED: &str = shprd_views::pr::REVIEW_PASSED;

/// The line a body ends with when no independent review could be had.
const REVIEW_DEGRADED: &str = "degraded-review";

/// The attribution line every generated pull request ends with.
const ATTRIBUTION: &str = shprd_views::pr::ATTRIBUTION;

/// How much of a sha a human line shows.
const SHORT_SHA: usize = 7;

/// Lands one task, or with no task opens the run's pull request.
pub fn land_verb(ctx: &Ctx, args: &LandArgs, home: Option<&Path>) -> Result<(), CliError> {
    if let Some(task) = &args.task {
        return land_task(ctx, args, home, task);
    }
    let config = load_config(ctx, home)?;
    let (state, events) = ctx.load()?;
    let run = state.run.as_ref().ok_or_else(|| {
        CliError::Invalid(anyhow!(
            "no run yet: start one with `shprd run start` before landing"
        ))
    })?;
    let run_id = run.run_id.clone();
    let base_branch = run.base_branch.clone();
    if state.finished {
        return Err(CliError::Invalid(anyhow!(
            "the run {run_id} has finished: there is nothing left to land"
        )));
    }
    check_in_flight(&state)?;
    let integrated = integrated_tasks(&state);
    if integrated.is_empty() {
        return Err(CliError::Invalid(anyhow!(
            "no task is integrated: land a task with `shprd agent integrate` first"
        )));
    }
    let home = resolve_home(home)?;
    let layout = resolve_layout(Some(&home), &ctx.root)?;
    let repo_name = repo_name(ctx)?;
    // Held from the head read to the push: a sync or a cherry-pick landing
    // meanwhile would move the head the gate is pinned to, and the branch would
    // leave between the gate check and the push.
    let _lock = run_lock::lock_run(&home, &repo_name, &run_id)?;
    let worktree =
        integrate::open_integration(&ctx.root, &layout, &repo_name, &run_id, &base_branch)
            .map_err(|err| {
                CliError::other(anyhow!(err).context("opening the integration worktree"))
            })?;
    let git = Git::new(&worktree.path);
    let head = git
        .head_sha()
        .map_err(|err| CliError::other(anyhow!(err).context("reading the integration head")))?;
    hint_moved_base(&git, &base_branch);
    check_full_gate(&state, &head)?;

    if args.merge {
        return merge(ctx, &config, &worktree, &head);
    }
    open(ctx, &state, &events, &worktree, &integrated)
}

/// Lands one task through its own pull request, as `[land] merge` says.
///
/// Everything is decided from the log before anything is pushed: the task must
/// exist, its latest attempt must have handed in and been reviewed
/// independently, and a landing that already finished is reported as finished
/// rather than repeated.
fn land_task(
    ctx: &Ctx,
    args: &LandArgs,
    home: Option<&Path>,
    task_id: &str,
) -> Result<(), CliError> {
    if args.merge {
        return Err(CliError::Invalid(anyhow!(
            "`--merge` merges the run's pull request; a task lands by `[land] merge` in the config"
        )));
    }
    let config = load_config(ctx, home)?;
    let (state, events) = ctx.load()?;
    let run = state.run.as_ref().ok_or_else(|| {
        CliError::Invalid(anyhow!(
            "no run yet: start one with `shprd run start` before landing"
        ))
    })?;
    let run_id = run.run_id.clone();
    let base = run.base_branch.clone();
    if state.finished {
        return Err(CliError::Invalid(anyhow!(
            "the run {run_id} has finished: there is nothing left to land"
        )));
    }
    let task = state.tasks.get(task_id).ok_or_else(|| {
        CliError::Invalid(anyhow!(
            "no task {task_id:?} in this run: `shprd status` lists the ones it holds"
        ))
    })?;
    if task.status == TaskStatus::Dropped {
        return Err(CliError::Invalid(anyhow!(
            "task {task_id:?} was dropped: there is nothing to land"
        )));
    }
    if let Some(pr) = &task.pr
        && pr.merged
    {
        // The landing already finished, and the log says so: nothing to push,
        // nothing to merge, nothing to record.
        if !ctx.json {
            println!("already merged {}", pr.url);
        }
        return Ok(());
    }
    if task.status == TaskStatus::Integrated {
        // A local landing leaves no pull request to name, so this is how a
        // re-run reports the work is already in rather than refusing it.
        if !ctx.json {
            println!("{task_id} is already integrated");
        }
        return Ok(());
    }
    // The hand-in and the review are the engine's to judge, and the rule that
    // judges them is the one every landing verb asks: M8.4's own gates, so a
    // task whose hand-in names no checks, or whose checks failed, cannot reach
    // a pull request through this path.
    if let Err(refusal) = rules::ready_to_land(task) {
        return Err(CliError::Invalid(anyhow!(
            "task {task_id:?} may not land yet: {refusal}"
        )));
    }
    let attempt = task
        .latest_attempt()
        .ok_or_else(|| CliError::Invalid(anyhow!("task {task_id:?} has no attempt to land")))?;
    let worktree = PathBuf::from(&attempt.worktree);
    let branch = attempt.branch.clone();
    let git = Git::new(&worktree);
    let head = git.head_sha().map_err(|err| {
        CliError::other(anyhow!(err).context(format!(
            "reading the head of {} in {}",
            attempt.branch,
            worktree.display()
        )))
    })?;
    // An approval counts only for the commit it named: a human who approved an
    // older head said nothing about the commits pushed since. A local landing
    // has no pull request, so the approval is read from the task itself.
    let approved = task.approved_head.as_deref() == Some(head.as_str());
    // Whether the branch had work of its own, for the one case git cannot tell
    // a merged branch from one that never moved.
    let has_work = attempt
        .report
        .as_ref()
        .and_then(|report| report.commit.as_ref())
        .is_some()
        || task.pr.is_some();
    // A repository with no remote, or with no forge to drive, lands locally:
    // the task's branch is merged into the run's integration branch in shprd's
    // own worktree; a non-GitHub remote is then pushed to, best effort.
    let local = local_landing(&Git::new(&ctx.root));
    // The scope is judged on the branch's real diff, not on the paths the
    // hand-in listed: a file the worker left off the list is caught here. It
    // is read against the same base the review's packet used (the run's base
    // branch) on a forge landing, and against the integration branch the task
    // is merged into on a local one.
    let scope_base = if local {
        &run.integration_branch
    } else {
        &base
    };
    if let Err(err) = land::check_scope(&git, scope_base, &branch, &task.scope) {
        return Err(CliError::Invalid(anyhow!(err.plain(task_id))));
    }
    if local {
        return land_task_local(
            ctx, &events, task, task_id, run, &branch, approved, has_work, &base, &config, home,
        );
    }
    let title = format!("{task_id}: {}", task.title);
    let body = render_task_body(task);
    let home = resolve_home(home)?;
    let repo_name = repo_name(ctx)?;
    // Held from here to the merge: a spawn replacing the attempt meanwhile
    // would delete the branch under the push.
    let _lock = run_lock::lock_run(&home, &repo_name, &run_id)?;
    let body_file = write_task_body(&home, &run_id, task_id, &body)?;
    let landing = land::Landing {
        task: task_id,
        branch: &branch,
        base: &base,
        title: &title,
        body: &body_file,
        has_work,
    };
    let forge = forge(&worktree)?;
    let policy = config.land.policy_for(&base);
    let landed = match land::land(forge.as_ref(), &git, &landing, policy, approved) {
        Ok(landed) => landed,
        // A branch that clashes with what landed after it is closed out and
        // sent back for a fresh attempt: the planner does the sending.
        Err(err @ land::LandError::Conflict { pr, .. }) => {
            if let Some(number) = pr {
                // Best effort: a pull request that cannot be closed is left for
                // a human, and the new attempt opens its own.
                let _ = forge.close(number, &stale_pr_comment(task_id));
            }
            record_conflict(ctx, task, task_id, &base, &err)?;
            return Err(CliError::Invalid(anyhow!(conflict_message(task_id, &err))));
        }
        // These three leave the pull request where a human can look at it, and
        // the next landing can act: they are refusals (exit 2), not failures.
        Err(
            err @ (land::LandError::ChecksFailed { .. }
            | land::LandError::HeadMoved { .. }
            | land::LandError::NotMerged { .. }),
        ) => {
            return Err(CliError::Invalid(anyhow!(err.plain(task_id))));
        }
        Err(err) => {
            return Err(CliError::other(anyhow!(err.plain(task_id))));
        }
    };
    record_landing(ctx, &events, task_id, &base, &landed)?;
    if !ctx.json {
        println!("{}", landing_line(task_id, &base, &landed));
    }
    Ok(())
}

/// Lands one task into the run's integration branch, with no forge involved.
///
/// The worktree is shprd's own worktree on `run.integration_branch`, so the
/// user's checkout is never touched. The merge policy is the same one the forge
/// path applies; `checks` reads the task's own hand-in because there is no CI
/// to ask. A conflicting merge is undone and recorded as a failed gate plus an
/// escalation, and the task can then be sent back to a worker.
#[expect(clippy::too_many_arguments, reason = "one local landing's whole input")]
fn land_task_local(
    ctx: &Ctx,
    events: &[Event],
    task: &Task,
    task_id: &str,
    run: &shprd_core::state::RunInfo,
    branch: &str,
    approved: bool,
    has_work: bool,
    base: &str,
    config: &Config,
    home: Option<&Path>,
) -> Result<(), CliError> {
    let home = resolve_home(home)?;
    let repo_name = repo_name(ctx)?;
    // Held from the head read to the merge: a spawn replacing the attempt
    // meanwhile would move the branch under the merge.
    let _lock = run_lock::lock_run(&home, &repo_name, &run.run_id)?;
    let layout = resolve_layout(Some(&home), &ctx.root)?;
    let worktree = integrate::open_integration_branch(
        &ctx.root,
        &layout,
        &repo_name,
        &run.run_id,
        &run.integration_branch,
        base,
    )
    .map_err(|err| CliError::other(anyhow!(err).context("opening the integration worktree")))?;
    let policy = config.land.policy_for(&run.integration_branch);
    let landing = land::LocalLanding {
        task: task_id,
        branch,
        base: &run.integration_branch,
        has_work,
    };
    let checks_passed = local_checks_passed(task);
    let landed = match land::land_local(&worktree, &landing, policy, approved, checks_passed) {
        Ok(landed) => landed,
        Err(err @ land::LandError::Conflict { .. }) => {
            record_conflict(ctx, task, task_id, &run.integration_branch, &err)?;
            return Err(CliError::Invalid(anyhow!(conflict_message(task_id, &err))));
        }
        Err(err) => {
            return Err(CliError::other(anyhow!(err.plain(task_id))));
        }
    };
    record_local_landing(ctx, events, task_id, &run.integration_branch, &landed)?;
    if !ctx.json {
        println!(
            "{}",
            local_landing_line(task_id, &run.integration_branch, &landed)
        );
    }
    Ok(())
}

/// The line a local landing prints: what happened, and what a human does next.
fn local_landing_line(task_id: &str, base: &str, landed: &land::LocalLanded) -> String {
    match landed {
        land::LocalLanded::Merged { head } => {
            format!("merged {task_id} into {base} at {}", short(head))
        }
        land::LocalLanded::AlreadyMerged { .. } => format!("{task_id} is already in {base}"),
        land::LocalLanded::SquashMerged { .. } => {
            format!("{task_id}'s work is already in {base}, as other commits")
        }
        land::LocalLanded::NothingToMerge { .. } => {
            format!("nothing to merge: {task_id}'s branch has no work beyond {base}")
        }
        land::LocalLanded::Waiting { reason, .. } => reason.local_advice(task_id),
    }
}

/// Whether the task's own hand-in says its checks passed.
///
/// A repository with no forge has no CI to ask, so the hand-in's checks are the
/// local report the `checks` policy reads.
fn local_checks_passed(task: &Task) -> bool {
    task.latest_attempt()
        .and_then(|attempt| attempt.report.as_ref())
        .is_some_and(|report| {
            !report.checks.is_empty()
                && report.checks.iter().all(|check| check.exit_code == Some(0))
        })
}

/// Records a local landing's outcome, once.
fn record_local_landing(
    ctx: &Ctx,
    events: &[Event],
    task_id: &str,
    base: &str,
    landed: &land::LocalLanded,
) -> Result<(), CliError> {
    let Some(outcome) = landed.outcome() else {
        return Ok(());
    };
    let head = landed.head().to_string();
    let known = events.iter().any(|event| {
        matches!(
            &event.kind,
            EventKind::TaskLanded { task, head: recorded, outcome: recorded_outcome, .. }
                if task == task_id && recorded == &head && *recorded_outcome == outcome
        )
    });
    if known {
        return Ok(());
    }
    let event = ctx.commit(EventKind::TaskLanded {
        task: task_id.to_string(),
        base: base.to_string(),
        head,
        number: None,
        outcome,
    })?;
    crate::cmd::emit(ctx, &event)?;
    Ok(())
}

/// What the stale pull request is closed with.
fn stale_pr_comment(task_id: &str) -> String {
    format!(
        "{task_id} clashed with work that landed after it was cut. It is being redone on top \
         of the latest, and the new attempt opens its own pull request."
    )
}

/// Records a landing that clashed with the integration branch as a failed
/// `integrate` gate.
///
/// The failed gate takes the task out of `Passed`, so the next tick does not try
/// the same conflicting merge again, and it is what the planner reads to send
/// the task back as a new attempt: the finding is the part of the new brief that
/// says to carry the earlier commits over and resolve these files. No card is
/// raised, because nobody has to decide anything.
fn record_conflict(
    ctx: &Ctx,
    task: &Task,
    task_id: &str,
    base: &str,
    err: &land::LandError,
) -> Result<(), CliError> {
    let land::LandError::Conflict { files, .. } = err else {
        return Ok(());
    };
    let attempt = task.attempts.last().map(|attempt| attempt.n).unwrap_or(1);
    let which = if files.is_empty() {
        String::from("the files it touches")
    } else {
        files.join(", ")
    };
    let finding = land::conflict_finding(base, files);
    let event = ctx.commit(EventKind::GateFailed {
        task: task_id.to_string(),
        attempt,
        gate: rules::LANDING_CONFLICT_GATE.to_string(),
        findings: vec![finding],
        source: None,
    })?;
    crate::cmd::emit(ctx, &event)?;
    // A note in the ledger, not a question: nothing is left to decide.
    let event = ctx.commit(EventKind::Escalated {
        task: Some(task_id.to_string()),
        body: format!(
            "{} (files in conflict: {which})",
            land::conflict_words(task_id)
        ),
        default_action: String::from("redo the task on top of the latest"),
    })?;
    crate::cmd::emit(ctx, &event)?;
    Ok(())
}

/// What a clashing landing prints: the plain sentence, then the files.
fn conflict_message(task_id: &str, err: &land::LandError) -> String {
    match err {
        land::LandError::Conflict { files, .. } if !files.is_empty() => format!(
            "{} (files in conflict: {})",
            err.plain(task_id),
            files.join(", ")
        ),
        _ => err.plain(task_id),
    }
}

/// The line a landing prints: what happened, and what a human does next.
fn landing_line(task_id: &str, base: &str, landed: &land::Landed) -> String {
    match landed {
        land::Landed::Merged { pr, .. } => format!("merged {} (task {task_id})", pr.url),
        land::Landed::AlreadyMerged { pr: Some(pr), .. } => {
            format!("already merged {} (task {task_id})", pr.url)
        }
        land::Landed::AlreadyMerged { pr: None, .. } => {
            format!("{task_id} is already in {base}")
        }
        land::Landed::SquashMerged { .. } => {
            format!("{task_id}'s work is already in {base}, as other commits")
        }
        land::Landed::NothingToMerge { .. } => {
            format!("nothing to merge: {task_id}'s branch has no work beyond {base}")
        }
        land::Landed::Waiting { pr, reason, .. } => {
            format!("{} ({})", reason.advice(task_id), pr.url)
        }
    }
}

/// Records what the landing touched: its pull request, then its outcome.
///
/// Both are recorded once. A landing a human re-runs reads the log, sees the
/// same pull request at the same head and the same outcome, and records
/// nothing: the log is a history, not a tally.
fn record_landing(
    ctx: &Ctx,
    events: &[Event],
    task_id: &str,
    base: &str,
    landed: &land::Landed,
) -> Result<(), CliError> {
    if let Some(pr) = landed.pr() {
        let known = events.iter().any(|event| {
            matches!(
                &event.kind,
                EventKind::TaskPrOpened { task, number, head, .. }
                    if task == task_id && *number == pr.number && head == &pr.head
            )
        });
        if !known {
            let event = ctx.commit(EventKind::TaskPrOpened {
                task: task_id.to_string(),
                number: pr.number,
                url: pr.url.clone(),
                head: pr.head.clone(),
            })?;
            crate::cmd::emit(ctx, &event)?;
        }
    }
    let Some(outcome) = landed.outcome() else {
        return Ok(());
    };
    let head = landed.head().to_string();
    let known = events.iter().any(|event| {
        matches!(
            &event.kind,
            EventKind::TaskLanded { task, head: recorded, outcome: recorded_outcome, .. }
                if task == task_id && recorded == &head && *recorded_outcome == outcome
        )
    });
    if known {
        return Ok(());
    }
    let event = ctx.commit(EventKind::TaskLanded {
        task: task_id.to_string(),
        base: base.to_string(),
        head,
        number: landed.number(),
        outcome,
    })?;
    crate::cmd::emit(ctx, &event)?;
    Ok(())
}

/// The per-task body: the task's facts, the review line, the attribution.
///
/// Landing refuses a task without an engine-recorded cross-vendor review, so
/// the marker is a claim the log already proves by the time this runs.
fn render_task_body(task: &Task) -> String {
    shprd_views::pr::task_pr_body_marked(task)
}

/// Writes one task's pull request body under shprd's own home.
///
/// Never in the repository: a landing must not leave a file in the tree it is
/// proposing to merge.
fn write_task_body(home: &Path, run: &str, task: &str, body: &str) -> Result<PathBuf, CliError> {
    let dir = home.join(".shprd").join("pr");
    std::fs::create_dir_all(&dir).map_err(|err| {
        CliError::other(anyhow!(err).context(format!("creating {}", dir.display())))
    })?;
    let path = dir.join(format!("{}-{}.md", file_stem(run), file_stem(task)));
    std::fs::write(&path, body).map_err(|err| {
        CliError::other(anyhow!(err).context(format!("writing {}", path.display())))
    })?;
    Ok(path)
}

/// A run or task id reduced to something a file name can hold.
fn file_stem(id: &str) -> String {
    let stem: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    if stem.is_empty() {
        String::from("task")
    } else {
        stem
    }
}

/// Refuses while any task is still working or waiting to be gated.
fn check_in_flight(state: &RunState) -> Result<(), CliError> {
    let busy = state
        .tasks
        .values()
        .find(|task| matches!(task.status, TaskStatus::Working | TaskStatus::Reported));
    match busy {
        Some(task) => Err(CliError::Invalid(anyhow!(
            "task {:?} is {:?}: land once every task is integrated",
            task.id,
            task.status
        ))),
        None => Ok(()),
    }
}

/// Every task that landed, in plan order, which is at least one by the caller.
fn integrated_tasks(state: &RunState) -> Vec<&Task> {
    state
        .tasks
        .values()
        .filter(|task| task.status == TaskStatus::Integrated)
        .collect()
}

/// Says the base moved under the run, without changing anything.
///
/// Landing a branch cut from an old base either conflicts at the forge or
/// proposes a merge the base already has, so a human is told to sync and gate
/// again. Never automatic: a sync moves the integration head, which is exactly
/// what a gate is pinned to, so the decision to take that step is theirs.
///
/// A missing remote-tracking ref reads as "did not move": a hint must not fail
/// a landing, and a run with no `origin` has no base to compare with. The
/// fetch this does first is best-effort for the same reason.
fn hint_moved_base(git: &Git, base: &str) {
    // The local tracking ref only knows what some earlier command fetched, so
    // ask the remote first: a merge since then is exactly what this hint is
    // for. A fetch that fails, offline or with no `origin`, is silent and the
    // local ref answers as it did before.
    sync::refresh_base(git, base);
    match sync::base_moved(git, base) {
        Ok(true) => {
            eprintln!("{base} moved since this run started: run `shprd sync`, then `shprd gate`")
        }
        Ok(false) => {}
        Err(err) => eprintln!("warning: could not tell whether {base} moved: {err}"),
    }
}

/// Refuses unless the run-wide gate passed on exactly this head.
fn check_full_gate(state: &RunState, head: &str) -> Result<(), CliError> {
    match &state.last_full_gate {
        None => Err(CliError::Invalid(anyhow!(
            "no full gate has run: run `shprd gate` before landing"
        ))),
        Some((gate_head, false, _)) => Err(CliError::Invalid(anyhow!(
            "the last full gate failed on head {gate_head}: fix it and run `shprd gate` again"
        ))),
        Some((gate_head, true, _)) if gate_head != head => Err(CliError::Invalid(anyhow!(
            "the last full gate passed on head {gate_head}, but the integration branch is at \
             {head}: run `shprd gate` again"
        ))),
        Some((_, true, _)) => Ok(()),
    }
}

/// Pushes the branch, refreshes or opens the pull request, records it.
fn open(
    ctx: &Ctx,
    state: &RunState,
    events: &[Event],
    worktree: &integrate::IntegrationWorktree,
    integrated: &[&Task],
) -> Result<(), CliError> {
    let run = state
        .run
        .as_ref()
        .ok_or_else(|| CliError::other(anyhow!("no run to land")))?;
    let body = render_body(state, events);
    let body_file = write_body(worktree, &body)?;
    let title = match integrated.first() {
        Some(task) => format!("{}: {}", run.run_id, task.title),
        None => run.run_id.clone(),
    };
    push(worktree, state, ctx)?;
    let forge = forge(&worktree.path)?;
    let branch = worktree.branch.clone();
    let pr = match forge.find_open(&branch).map_err(|err| {
        CliError::other(anyhow!(err).context("asking the forge for the pull request"))
    })? {
        Some(pr) => {
            forge.edit_body(pr.number, &body_file).map_err(|err| {
                CliError::other(anyhow!(err).context("refreshing the pull request"))
            })?;
            pr
        }
        None => forge
            .create(&run.base_branch, &branch, &title, &body_file)
            .map_err(|err| CliError::other(anyhow!(err).context("opening the pull request")))?,
    };
    let event = ctx.commit(EventKind::PrOpened {
        number: pr.number,
        url: pr.url.clone(),
        head: branch,
    })?;
    crate::cmd::emit(ctx, &event)?;
    if !ctx.json {
        println!("{}", pr.url);
    }
    Ok(())
}

/// Pushes the integration branch, recording the remote branch once.
fn push(
    worktree: &integrate::IntegrationWorktree,
    state: &RunState,
    ctx: &Ctx,
) -> Result<(), CliError> {
    let git = Git::new(&worktree.path);
    git.run(&["push", "-u", "origin", &worktree.branch])
        .map_err(|err| {
            CliError::other(anyhow!(err).context(format!(
                "pushing {} to origin from {}",
                worktree.branch,
                worktree.path.display()
            )))
        })?;
    let key = (ResourceKind::RemoteBranch, worktree.branch.clone());
    if matches!(
        state.resources.get(&key).map(|record| record.status),
        Some(shprd_core::state::ResourceStatus::Live)
    ) {
        return Ok(());
    }
    ctx.commit(EventKind::Resource {
        action: ResourceAction::Created,
        kind: ResourceKind::RemoteBranch,
        name: worktree.branch.clone(),
        task: None,
    })?;
    Ok(())
}

/// Where the body is written: beside the integration worktree, in shprd's own
/// state directory, so no file of the user's repository is touched.
fn write_body(worktree: &integrate::IntegrationWorktree, body: &str) -> Result<PathBuf, CliError> {
    let dir = worktree
        .path
        .parent()
        .ok_or_else(|| CliError::other(anyhow!("the integration worktree has no parent")))?;
    let path = dir.join("pr-body.md");
    std::fs::write(&path, body).map_err(|err| {
        CliError::other(anyhow!(err).context(format!("writing {}", path.display())))
    })?;
    Ok(path)
}

/// The pull request body: the run's facts, the review marker, the attribution.
fn render_body(state: &RunState, events: &[Event]) -> String {
    let mut body = shprd_views::pr::pr_body(state, events);
    if !body.ends_with('\n') {
        body.push('\n');
    }
    body.push('\n');
    body.push_str(&review_marker(state));
    body.push('\n');
    body.push('\n');
    body.push_str(ATTRIBUTION);
    body.push('\n');
    body
}

/// The review marker for the body, followed by one line per task that made it
/// degraded.
///
/// `cross-vendor-review: passed` is a claim about recorded facts: every
/// integrated task's latest attempt needs a review that was not degraded and a
/// passed `review` gate that carries its reviewer provenance. Anything else —
/// no review requested, only degraded reviews, no passed review gate, a verdict
/// nothing proves, a review a human overrode — is `degraded-review`, and the
/// reasons say which tasks caused it and why.
fn review_marker(state: &RunState) -> String {
    let gaps = review_gaps(state);
    if gaps.is_empty() {
        return REVIEW_PASSED.to_string();
    }
    let mut marker = String::from(REVIEW_DEGRADED);
    for gap in gaps {
        marker.push_str("\n- ");
        marker.push_str(&gap);
    }
    marker
}

/// One line per integrated task whose latest attempt lacks an independent,
/// passing review.
fn review_gaps(state: &RunState) -> Vec<String> {
    let mut gaps = Vec::new();
    for task in integrated_tasks(state) {
        let Some(attempt) = task.latest_attempt() else {
            gaps.push(format!("{}: no attempt to review", task.id));
            continue;
        };
        if attempt.reviews.is_empty() {
            gaps.push(format!(
                "{}: no review was requested for attempt {}",
                task.id, attempt.n
            ));
            continue;
        }
        if !attempt.reviews.iter().any(|review| !review.degraded) {
            let reviewers: Vec<&str> = attempt
                .reviews
                .iter()
                .map(|review| review.reviewer.as_str())
                .collect();
            gaps.push(format!(
                "{}: every review requested for attempt {} was degraded ({})",
                task.id,
                attempt.n,
                reviewers.join(", ")
            ));
            continue;
        }
        // A human who overrode the review gate took the decision themselves:
        // that is not a cross-vendor review, and the body has to say so.
        if let Some(override_record) = task
            .overrides
            .iter()
            .rev()
            .find(|record| record.rule == REVIEW_GATE)
        {
            gaps.push(format!(
                "{}: the review gate was overridden ({})",
                task.id, override_record.reason
            ));
            continue;
        }
        let verdict = attempt
            .gates
            .iter()
            .rev()
            .find(|gate| gate.gate == REVIEW_GATE);
        match verdict {
            // A pass with no provenance proves only that something recorded a
            // pass: no reviewer wrote the findings it names.
            Some(gate) if gate.passed && gate.source.is_none() => gaps.push(format!(
                "{}: review verdict has no reviewer provenance",
                task.id
            )),
            Some(gate) if gate.passed => {}
            _ => gaps.push(format!(
                "{}: attempt {} has no passed review gate",
                task.id, attempt.n
            )),
        }
    }
    gaps
}

/// The short form of a sha, for the human line.
pub(crate) fn short(sha: &str) -> &str {
    sha.get(..SHORT_SHA).unwrap_or(sha)
}

/// Merges the run's pull request when the policy allows it.
fn merge(
    ctx: &Ctx,
    config: &Config,
    worktree: &integrate::IntegrationWorktree,
    head: &str,
) -> Result<(), CliError> {
    let state = ctx.state()?;
    let run = state
        .run
        .as_ref()
        .ok_or_else(|| CliError::other(anyhow!("no run to merge")))?;
    let pr = state.pr.as_ref().ok_or_else(|| {
        CliError::Invalid(anyhow!(
            "no pull request yet: run `shprd land` before `shprd land --merge`"
        ))
    })?;
    let url = pr.url.clone();
    let number = pr.number;
    let base = run.base_branch.clone();
    // The ledger's head is the branch, not the sha: the janitor deletes a
    // remote branch only when the merged pull request names it as its head.
    let branch = worktree.branch.clone();
    let forge = forge(&worktree.path)?;
    let status = forge.status(number).map_err(|err| {
        CliError::other(anyhow!(err).context("asking the forge for the pull request"))
    })?;
    if status.merged {
        // The forge merged the pull request without shprd. That is a fact
        // about this branch only when the forge merged the very commit the
        // integration branch is at now: a merge at an older head leaves
        // commits the pull request never contained, and recording them as
        // merged would let the janitor delete them.
        if status.head_sha != head {
            return Err(CliError::Invalid(anyhow!(
                "the pull request was merged at {}, but the integration branch is now at {}: \
                 open a new pull request with `shprd land`",
                short(&status.head_sha),
                short(head)
            )));
        }
        // The merge is recorded once and never twice.
        if !pr.merged {
            let event = ctx.commit(EventKind::PrMerged {
                number,
                head: branch,
            })?;
            crate::cmd::emit(ctx, &event)?;
        }
        println!("merged {url}");
        return Ok(());
    }
    if status.head_sha != head {
        return Err(CliError::Invalid(anyhow!(
            "the pull request is at head {}, but the integration branch is at {head}: push and \
             run `shprd land` again",
            status.head_sha
        )));
    }
    if status.checks_passed != Some(true) {
        return Err(CliError::Invalid(anyhow!(
            "the pull request's checks have not passed on {head}: wait for CI and try again"
        )));
    }
    let policy = config.land.policy_for(&base);
    // An approval recorded in shprd counts exactly like the forge's, as long as
    // it names this head: what a human said about older commits says nothing
    // about the ones pushed since.
    let approved_here = pr.approved_head.as_deref() == Some(head);
    let allowed = status.approved
        || approved_here
        || matches!(policy, MergePolicy::Checks | MergePolicy::Auto);
    if !allowed {
        return Err(CliError::Invalid(anyhow!(
            "waiting for your approval: run `shprd approve`, or approve {url} on GitHub"
        )));
    }
    forge
        .merge(number, head)
        .map_err(|err| CliError::other(anyhow!(err).context("merging the pull request")))?;
    let event = ctx.commit(EventKind::PrMerged {
        number,
        head: branch,
    })?;
    crate::cmd::emit(ctx, &event)?;
    if !ctx.json {
        println!("merged {url}");
    }
    Ok(())
}

/// Which forge a run drives.
///
/// The fake forge exists for tests only, so the choice is a pure function of
/// the environment and the build: a release binary always drives `gh`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ForgeChoice {
    /// The script `SHPRD_FAKE_FORGE` names; debug builds only.
    Fake(PathBuf),
    /// The real `gh`, run against the worktree it is asked about.
    Gh,
}

/// The forge the environment asks for, honoured only in a debug build.
///
/// `debug` is true in a build with debug assertions, which is every test
/// binary; a release build ignores the variable entirely, so nothing but a
/// real merge can be recorded as one.
fn choose_forge(env_value: Option<String>, debug: bool) -> ForgeChoice {
    match env_value {
        Some(path) if debug && !path.is_empty() => ForgeChoice::Fake(PathBuf::from(path)),
        _ => ForgeChoice::Gh,
    }
}

/// The forge to drive: `gh` here, or, in a debug build, the script
/// `SHPRD_FAKE_FORGE` names.
fn forge(dir: &Path) -> Result<Box<dyn Forge>, CliError> {
    let env_value =
        std::env::var_os(FAKE_FORGE_ENV).map(|value| value.to_string_lossy().into_owned());
    match choose_forge(env_value, cfg!(debug_assertions)) {
        ForgeChoice::Fake(script) => {
            eprintln!("warning: using a fake forge ({FAKE_FORGE_ENV}); nothing reaches GitHub");
            let fake = FakeForge::from_script(&script).map_err(|err| {
                CliError::other(anyhow!(err).context("reading the scripted forge"))
            })?;
            Ok(Box::new(fake))
        }
        ForgeChoice::Gh => Ok(Box::new(GhForge::new(dir))),
    }
}

/// Whether a landing must take the local path instead of a forge.
///
/// A repository with no remote has nowhere to push and no pull request to open,
/// so it lands locally. So does one with a remote but no usable forge: `gh` is
/// missing or signed out, and a landing that would fail at the first forge call
/// can still put the work on the run's integration branch. A scripted forge in a
/// debug build is always usable, so the tests that drive one never take this
/// path by accident.
pub(crate) fn local_landing(git: &Git) -> bool {
    if cfg!(debug_assertions)
        && std::env::var_os(FAKE_FORGE_ENV).is_some_and(|value| !value.is_empty())
    {
        return !shprd_git::worktree::has_remote(git);
    }
    !land::uses_forge(git, |host| gh_ready(git.dir(), host))
}

/// Whether `gh` is installed and signed in, asked in the repository it would
/// drive.
///
/// `gh auth status` answers from the local configuration, so this makes no
/// network call; anything it cannot answer counts as "no forge", which is the
/// safe answer because the local path writes to shprd's own worktree only.
fn gh_ready(dir: &Path, host: &str) -> bool {
    Command::new("gh")
        .args(["auth", "status", "--hostname", host])
        .current_dir(dir)
        .env("GH_PROMPT_DISABLED", "1")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// The config's `[land]`, with the repo file winning over the global one.
pub(crate) fn load_config(ctx: &Ctx, home: Option<&Path>) -> Result<Config, CliError> {
    let home = resolve_home(home)?;
    let global = home.join(".config").join("shprd").join("config.toml");
    let repo = ctx.root.join(".shprd").join("config.toml");
    config::load(Some(&global), Some(&repo))
        .map_err(|err| CliError::other(anyhow!(err).context("loading the config")))
}

/// The name of the repository directory, which keeps repos apart on disk.
pub(crate) fn repo_name(ctx: &Ctx) -> Result<String, CliError> {
    ctx.root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| CliError::other(anyhow!("{} has no directory name", ctx.root.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use shprd_core::event::{Actor, Finding, ReportStatus, ReviewSource, Severity};
    use shprd_core::rules::INTEGRATE_AFTER_FAILED_GATE;

    /// An event with `kind` at position `seq`.
    fn ev(seq: u64, kind: EventKind) -> Event {
        Event {
            seq,
            at_ms: seq * 1000,
            actor: Actor::Engine,
            kind,
        }
    }

    /// A run with one task spawned and reported, ready to be reviewed.
    fn reported_log(task: &str) -> Vec<Event> {
        vec![
            ev(
                1,
                EventKind::RunStarted {
                    run_id: "r1".to_string(),
                    repo: "/tmp/repo".to_string(),
                    base_branch: "main".to_string(),
                    integration_branch: "integration/m1".to_string(),
                },
            ),
            ev(
                2,
                EventKind::TaskAdded {
                    task: task.to_string(),
                    title: "a task".to_string(),
                    scope: vec!["a.txt".to_string()],
                    acceptance: vec!["it works".to_string()],
                    tags: Vec::new(),
                    skills: Vec::new(),
                    deps: Vec::new(),
                },
            ),
            ev(
                3,
                EventKind::Spawned {
                    task: task.to_string(),
                    attempt: 1,
                    agent: "w1".to_string(),
                    model: None,
                    worktree: "/tmp/wt".to_string(),
                    branch: "feature/t1".to_string(),
                },
            ),
            ev(
                4,
                EventKind::Reported {
                    task: task.to_string(),
                    attempt: 1,
                    status: ReportStatus::Completed,
                    summary: "done".to_string(),
                    tests: Vec::new(),
                    checks: vec![shprd_core::event::CheckReport::passed(
                        "tests",
                        "cargo test",
                    )],
                    changed_paths: vec!["a.txt".to_string()],
                    commit: Some("abc123".to_string()),
                    skills_used: Vec::new(),
                    proofs: Vec::new(),
                },
            ),
        ]
    }

    /// Records a review dispatch at `seq`.
    fn reviewed(seq: u64, task: &str, reviewer: &str, degraded: bool) -> Event {
        ev(
            seq,
            EventKind::ReviewRequested {
                task: task.to_string(),
                attempt: 1,
                reviewer: reviewer.to_string(),
                degraded,
                note: None,
            },
        )
    }

    /// The provenance a real verdict carries, for `reviewer`'s request at
    /// `request_seq` (`0` when the log records no request at all).
    fn provenance(reviewer: &str, request_seq: u64) -> ReviewSource {
        ReviewSource {
            reviewer: reviewer.to_string(),
            findings_sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
                .to_string(),
            request_seq,
        }
    }

    /// Records a passed `review` gate at `seq`, answering the request at
    /// `request_seq`.
    fn review_passed(seq: u64, task: &str, request_seq: u64) -> Event {
        ev(
            seq,
            EventKind::GatePassed {
                task: task.to_string(),
                attempt: 1,
                gate: REVIEW_GATE.to_string(),
                findings: Vec::new(),
                source: Some(provenance("c1", request_seq)),
            },
        )
    }

    /// Records a passed `review` gate with no provenance: a verdict recorded
    /// before the engine started stamping the reviewer's findings.
    fn review_passed_unproven(seq: u64, task: &str) -> Event {
        ev(
            seq,
            EventKind::GatePassed {
                task: task.to_string(),
                attempt: 1,
                gate: REVIEW_GATE.to_string(),
                findings: Vec::new(),
                source: None,
            },
        )
    }

    /// Records a human overriding `rule` at `seq`.
    fn overrode(seq: u64, task: &str, rule: &str, reason: &str) -> Event {
        ev(
            seq,
            EventKind::Override {
                task: task.to_string(),
                rule: rule.to_string(),
                reason: reason.to_string(),
            },
        )
    }

    /// Records the task landing at `seq`.
    fn integrated(seq: u64, task: &str) -> Event {
        ev(
            seq,
            EventKind::Integrated {
                task: task.to_string(),
                commits: vec!["abc123".to_string()],
            },
        )
    }

    /// Folds `events`, panicking on a rule error.
    fn folded(events: &[Event]) -> RunState {
        match RunState::fold(events) {
            Ok(state) => state,
            Err(err) => panic!("fold failed: {err}"),
        }
    }

    #[test]
    fn a_clean_review_reads_as_passed() {
        let mut log = reported_log("T1");
        log.push(reviewed(5, "T1", "c1", false));
        log.push(review_passed(6, "T1", 5));
        log.push(integrated(7, "T1"));
        assert_eq!(review_marker(&folded(&log)), REVIEW_PASSED);
    }

    #[test]
    fn a_restarted_review_still_reads_as_one_clean_review() {
        // A brief that never arrived is requested again for the same attempt:
        // both requests describe the one review, so the marker still reads a
        // pass rather than a task reviewed twice.
        let mut log = reported_log("T1");
        log.push(reviewed(5, "T1", "c1", false));
        log.push(reviewed(6, "T1", "c1", false));
        log.push(review_passed(7, "T1", 6));
        log.push(integrated(8, "T1"));
        assert_eq!(review_marker(&folded(&log)), REVIEW_PASSED);
    }

    #[test]
    fn one_degraded_review_names_the_task() {
        let mut log = reported_log("T1");
        log.push(reviewed(5, "T1", "w2", true));
        log.push(review_passed(6, "T1", 5));
        log.push(integrated(7, "T1"));
        let marker = review_marker(&folded(&log));
        assert!(marker.starts_with(REVIEW_DEGRADED), "{marker}");
        assert!(!marker.contains(REVIEW_PASSED), "{marker}");
        assert!(marker.contains("T1"), "{marker}");
        assert!(marker.contains("every review requested"), "{marker}");
        assert!(marker.contains("w2"), "{marker}");
    }

    #[test]
    fn a_passed_review_gate_without_a_recorded_review_is_degraded() {
        let mut log = reported_log("T1");
        log.push(review_passed(5, "T1", 0));
        log.push(integrated(6, "T1"));
        let marker = review_marker(&folded(&log));
        assert!(marker.starts_with(REVIEW_DEGRADED), "{marker}");
        assert!(marker.contains("T1"), "{marker}");
        assert!(marker.contains("no review was requested"), "{marker}");
    }

    #[test]
    fn an_independent_review_without_a_passed_gate_is_degraded() {
        let mut log = reported_log("T1");
        log.push(reviewed(5, "T1", "c1", false));
        // Its own gates passed; only the review gate is missing.
        log.push(ev(
            6,
            EventKind::GatePassed {
                task: "T1".to_string(),
                attempt: 1,
                gate: "lint".to_string(),
                findings: Vec::new(),
                source: None,
            },
        ));
        log.push(integrated(7, "T1"));
        let marker = review_marker(&folded(&log));
        assert!(marker.starts_with(REVIEW_DEGRADED), "{marker}");
        assert!(marker.contains("no passed review gate"), "{marker}");
    }

    #[test]
    fn an_unproven_verdict_degrades_the_marker() {
        // A pass nothing proves is the bug the marker has to catch: the log
        // says a review passed, but no reviewer wrote the findings it decided.
        let mut log = reported_log("T1");
        log.push(reviewed(5, "T1", "c1", false));
        log.push(review_passed_unproven(6, "T1"));
        log.push(integrated(7, "T1"));
        let marker = review_marker(&folded(&log));
        assert!(marker.starts_with(REVIEW_DEGRADED), "{marker}");
        assert!(!marker.contains(REVIEW_PASSED), "{marker}");
        assert!(
            marker.contains("T1: review verdict has no reviewer provenance"),
            "{marker}"
        );
    }

    #[test]
    fn a_proven_verdict_passes_the_marker() {
        let mut log = reported_log("T1");
        log.push(reviewed(5, "T1", "c1", false));
        log.push(review_passed(6, "T1", 5));
        log.push(integrated(7, "T1"));
        assert_eq!(review_marker(&folded(&log)), REVIEW_PASSED);
    }

    #[test]
    fn an_overridden_review_gate_degrades_the_marker_with_its_reason() {
        let mut log = reported_log("T1");
        log.push(reviewed(5, "T1", "c1", false));
        log.push(review_passed(6, "T1", 5));
        log.push(overrode(
            7,
            "T1",
            REVIEW_GATE,
            "the reviewer was unavailable",
        ));
        log.push(integrated(8, "T1"));
        let marker = review_marker(&folded(&log));
        assert!(marker.starts_with(REVIEW_DEGRADED), "{marker}");
        assert!(
            marker.contains("T1: the review gate was overridden (the reviewer was unavailable)"),
            "{marker}"
        );
    }

    #[test]
    fn an_override_of_a_failed_review_degrades_the_marker() {
        // The task passed a later gate, so an `integrate_after_failed_gate`
        // override lets it land although the review failed. What landed is
        // work a reviewer rejected, so the body must not claim a cross-vendor
        // review of it.
        let mut log = reported_log("T1");
        log.push(reviewed(5, "T1", "c1", false));
        log.push(ev(
            6,
            EventKind::GateFailed {
                task: "T1".to_string(),
                attempt: 1,
                gate: REVIEW_GATE.to_string(),
                findings: vec![Finding {
                    severity: Severity::Major,
                    title: "the retry loop loses work".to_string(),
                    location: None,
                    detail: "a crash drops the batch".to_string(),
                }],
                source: None,
            },
        ));
        log.push(ev(
            7,
            EventKind::GatePassed {
                task: "T1".to_string(),
                attempt: 1,
                gate: "lint".to_string(),
                findings: Vec::new(),
                source: None,
            },
        ));
        log.push(overrode(
            8,
            "T1",
            INTEGRATE_AFTER_FAILED_GATE,
            "shipping the fixed batch handling",
        ));
        log.push(integrated(9, "T1"));
        let state = folded(&log);
        assert_eq!(
            state.tasks.get("T1").map(|task| task.status),
            Some(shprd_core::state::TaskStatus::Integrated),
            "the override let it land"
        );
        let marker = review_marker(&state);
        assert!(marker.starts_with(REVIEW_DEGRADED), "{marker}");
        assert!(!marker.contains(REVIEW_PASSED), "{marker}");
        assert!(
            marker.contains("T1: attempt 1 has no passed review gate"),
            "{marker}"
        );
    }

    #[test]
    fn only_a_debug_build_honours_the_fake_forge() {
        let script = "/tmp/forge.json".to_string();
        assert_eq!(
            choose_forge(Some(script.clone()), false),
            ForgeChoice::Gh,
            "a release build never trusts SHPRD_FAKE_FORGE"
        );
        assert_eq!(
            choose_forge(Some(script.clone()), true),
            ForgeChoice::Fake(PathBuf::from(&script))
        );
        assert_eq!(choose_forge(None, true), ForgeChoice::Gh);
        assert_eq!(choose_forge(Some(String::new()), true), ForgeChoice::Gh);
    }
}
