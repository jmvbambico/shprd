//! `shprd agent handoff`: move a task from one agent to the next.
//!
//! A hand-off keeps the attempt's worktree, branch and log entry and changes
//! only who works in them: the outgoing agent's work is checkpointed, a
//! hand-off brief says where to continue, and the tab is replaced. Every
//! decision is made before the first side effect, the way [`spawn`] does it.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::anyhow;
use shprd_core::event::{Event, EventKind};
use shprd_core::log::{Clock, SystemClock};
use shprd_core::state::{Attempt, RunState, Task, TaskStatus};
use shprd_git::git::Git;
use shprd_git::worktree::Worktree;
use shprd_mux::{Mux, TabId, Workspace};
use shprd_roster::models::AgentRef;

use crate::cli::HandoffArgs;
use crate::cmd::spawn::{self, BriefInput, Launching, Pick, Program, SpawnEnv};
use crate::ctx::{CliError, Ctx};

/// What the caller asked for.
#[derive(Debug, Clone)]
pub struct HandoffRequest {
    /// The task to hand over.
    pub task: String,
    /// Why it is being handed over.
    pub reason: String,
    /// The agent taking it over, as `<program>/<model>`; `None` picks the next
    /// configured worker after the outgoing one.
    pub to: Option<String>,
}

/// The attempt being handed over: the latest one, which must be Working.
fn working_attempt(task: &Task) -> Result<&Attempt, CliError> {
    if task.status != TaskStatus::Working {
        return Err(CliError::Invalid(anyhow!(
            "task {:?} is {:?}, not Working: only a task an agent is working on can be handed off",
            task.id,
            task.status
        )));
    }
    task.latest_attempt()
        .ok_or_else(|| CliError::Invalid(anyhow!("task {:?} has no attempt to hand off", task.id)))
}

/// The agent that takes the task over, or `None` when none can.
///
/// An explicit `--to` wins even when its CLI is not installed, as it does for a
/// spawn: the human may be pointing at a wrapper. It must still parse against
/// the harness table, so a typo is refused before any effect. Without one the
/// roster decides, which can come up empty.
fn pick_to(
    state: &RunState,
    programs: &[Program],
    workers: &[String],
    from: &str,
    explicit: Option<&str>,
) -> Result<Option<Pick>, CliError> {
    let specs = spawn::spec_table(programs);
    let Some(raw) = explicit else {
        return Ok(next_worker(
            state,
            programs,
            workers,
            from,
            SystemClock.now_ms(),
        ));
    };
    let parsed = AgentRef::parse(raw, &specs).map_err(|err| {
        CliError::Invalid(anyhow!(err).context(format!("`{raw}` is not a usable agent")))
    })?;
    let program = spawn::program_of(programs, &parsed.program)?;
    Ok(Some(Pick {
        agent: parsed.program,
        model: parsed.model,
        launch: program.spec.launch.clone(),
    }))
}

/// The next configured worker after `from`, wrapping around the roster once.
///
/// A worker is skipped when it is the one handing over, when its string does
/// not parse, when its CLI is not installed, or when the log has it dry right
/// now: handing work to an agent that cannot take it would only cost a tab.
fn next_worker(
    state: &RunState,
    programs: &[Program],
    workers: &[String],
    from: &str,
    now_ms: u64,
) -> Option<Pick> {
    let specs = spawn::spec_table(programs);
    let start = workers
        .iter()
        .position(|raw| AgentRef::parse(raw, &specs).is_ok_and(|parsed| parsed.program == from))
        .map(|index| index + 1)
        .unwrap_or(0);
    for offset in 0..workers.len() {
        let raw = &workers[(start + offset) % workers.len()];
        let Ok(parsed) = AgentRef::parse(raw, &specs) else {
            eprintln!("warning: skipping configured worker `{raw}`");
            continue;
        };
        if parsed.program == from || dry_now(state, &parsed.program, now_ms) {
            continue;
        }
        let found = programs
            .iter()
            .find(|program| program.spec.id == parsed.program && program.installed);
        if let Some(program) = found {
            return Some(Pick {
                agent: parsed.program,
                model: parsed.model,
                launch: program.spec.launch.clone(),
            });
        }
    }
    None
}

/// Whether the log has `agent` on cooldown as of `now_ms`.
///
/// A cooldown without an end is dry until a human re-admits the agent; one with
/// an end expires on its own, so a reset in the past no longer counts.
fn dry_now(state: &RunState, agent: &str, now_ms: u64) -> bool {
    match state.agents.get(agent) {
        Some(info) if info.dry => match info.dry_until_ms {
            Some(until) => until > now_ms,
            None => true,
        },
        _ => false,
    }
}

/// Commits whatever the outgoing agent left uncommitted, and returns the head
/// the incoming agent resumes from.
///
/// A clean worktree already is a checkpoint, so nothing is committed then: the
/// hand-off must never add an empty commit to the attempt's history.
fn checkpoint(worktree: &Path, from: &str) -> Result<String, CliError> {
    let git = Git::new(worktree);
    let clean = git.is_clean().map_err(|err| {
        CliError::other(anyhow!(err).context(format!(
            "looking for uncommitted work in {}",
            worktree.display()
        )))
    })?;
    if !clean {
        let message = format!("wip(shprd): checkpoint before hand-off from {from}");
        git.run(&["add", "-A"])
            .map_err(|err| CliError::other(anyhow!(err).context("staging the outgoing work")))?;
        // The commit carries its own identity and skips the hooks: an
        // unattended checkpoint must not depend on the machine's git config.
        git.run(&[
            "-c",
            "user.name=shprd",
            "-c",
            "user.email=shprd@localhost",
            "commit",
            "--no-verify",
            "-m",
            &message,
        ])
        .map_err(|err| CliError::other(anyhow!(err).context("committing the checkpoint")))?;
    }
    git.head_sha()
        .map_err(|err| CliError::other(anyhow!(err).context("reading the checkpoint")))
}

/// What the attempt changed against the run's integration branch, as git
/// summarizes it. A branch git cannot compare is reported, never fatal.
fn diffstat(worktree: &Path, base: &str) -> String {
    match Git::new(worktree).run(&["diff", "--stat", &format!("{base}..HEAD")]) {
        Ok(text) if text.trim().is_empty() => "(no changes yet)".to_string(),
        Ok(text) => text,
        Err(err) => format!("(not available: {err})"),
    }
}

/// Where a hand-off brief lives:
/// `<briefs>/<run>/<task>-a<attempt>-h<handoff>.md`.
fn handoff_brief_path(briefs: &Path, run: &str, task: &str, attempt: u32, handoff: u32) -> PathBuf {
    briefs
        .join(run)
        .join(format!("{task}-a{attempt}-h{handoff}.md"))
}

/// How many times this attempt was handed over before, from the log.
///
/// Only hand-offs after the attempt's own `Spawned` event count, so a retry
/// whose attempt number repeats in a later run still starts at zero, and a
/// task handed over twice in one attempt numbers its briefs `h1` and `h2`.
fn previous_handoffs(events: &[Event], task: &str, attempt: u32) -> u32 {
    let spawned_at = events
        .iter()
        .filter(|event| match &event.kind {
            EventKind::Spawned {
                task: spawned,
                attempt: spawned_attempt,
                ..
            } => spawned == task && *spawned_attempt == attempt,
            _ => false,
        })
        .map(|event| event.seq)
        .max();
    let Some(spawned_at) = spawned_at else {
        return 0;
    };
    let count = events
        .iter()
        .filter(|event| {
            event.seq > spawned_at
                && matches!(&event.kind, EventKind::HandedOff { task: handed, .. } if handed == task)
        })
        .count();
    u32::try_from(count).unwrap_or(u32::MAX)
}

/// How many lines of the outgoing agent's pane the brief carries.
const PANE_TAIL_LINES: usize = 30;

/// The end of the pane the outgoing agent ran in, when its tab still exists.
///
/// Reading the pane is the cheapest way to tell the incoming agent what the
/// other one was in the middle of; a tab the human already closed is not an
/// error, the brief just says nothing about it.
fn old_pane_tail(
    mux: &dyn Mux,
    workspace: &Workspace,
    env: &SpawnEnv<'_>,
    task: &str,
    attempt: &Attempt,
) -> Option<String> {
    let record = spawn::tab_path(&env.briefs_dir, &env.run, task, attempt.n);
    let wanted = std::fs::read_to_string(record).ok()?;
    let wanted = wanted.trim().to_string();
    let tabs = mux.list_tabs(&workspace.id).ok()?;
    let tab = tabs.iter().find(|tab| tab.id.as_str() == wanted)?;
    let text = mux.read(&tab.root_pane, PANE_TAIL_LINES).ok()?;
    let text = text.trim_end().to_string();
    if text.is_empty() { None } else { Some(text) }
}

/// What the hand-off section of the brief says, as data.
struct Transfer<'a> {
    /// The agent handing the task over.
    from: &'a str,
    /// The agent taking it over.
    to: &'a str,
    /// Why the human handed it over.
    reason: &'a str,
    /// The commit the incoming agent resumes from.
    checkpoint: &'a str,
    /// The run's integration branch, which the diff is taken against.
    base_branch: &'a str,
    /// `git diff --stat` in the task's worktree.
    diffstat: &'a str,
    /// The end of the outgoing agent's pane, when it was readable.
    pane_tail: Option<&'a str>,
}

/// Renders the hand-off brief: the brief the attempt started with, then what
/// the newcomer needs that the outgoing agent did not have.
fn handoff_brief(base: &BriefInput<'_>, transfer: &Transfer<'_>) -> String {
    let mut out = spawn::render_brief(base);
    out.push_str("\n## You are taking over\n\n");
    out.push_str(&format!(
        "- this attempt was handed from `{}` to `{}`\n",
        transfer.from, transfer.to
    ));
    out.push_str(&format!("- why: {}\n", transfer.reason));
    out.push_str(&format!("- checkpoint: `{}`\n", transfer.checkpoint));
    out.push_str("- continue from the checkpoint; do not start over; do not rewrite history\n");
    out.push_str(&format!(
        "\n### Changes since `{}`\n\n```\n{}\n```\n",
        transfer.base_branch, transfer.diffstat
    ));
    out.push_str("\n### The end of the previous agent's pane\n\n");
    match transfer.pane_tail {
        Some(tail) => out.push_str(&format!("```\n{tail}\n```\n")),
        None => out.push_str("(not available: that tab is gone)\n"),
    }
    out
}

/// What a successful hand-off did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffReport {
    /// The task that changed hands.
    pub task: String,
    /// The attempt number that continues.
    pub attempt: u32,
    /// The agent that handed it over.
    pub from: String,
    /// The agent that took it over.
    pub to: String,
    /// The commit the incoming agent resumes from.
    pub checkpoint: String,
    /// The title of the new agent's tab.
    pub tab: String,
    /// The hand-off brief the incoming agent reads.
    pub brief: PathBuf,
    /// How long the hand-off took, from call to launched agent.
    pub elapsed_ms: u64,
}

/// Hands a task's attempt from one agent to the next.
///
/// The order is [`spawn`]'s: every decision is made and the log is asked before
/// the first side effect, so a refused hand-off leaves nothing behind. The
/// outgoing work is checkpointed first, because that commit is the only record
/// of what the human is asking someone else to finish.
pub fn handoff(
    mux: &dyn Mux,
    env: &SpawnEnv<'_>,
    req: &HandoffRequest,
) -> Result<HandoffReport, CliError> {
    let started = Instant::now();
    let (state, events) = env.ctx.load()?;
    let task = spawn::task_of(&state, &req.task)?;
    let attempt = working_attempt(task)?;
    let from = attempt.agent.clone();
    let Some(to) = pick_to(
        &state,
        &env.programs,
        &env.workers,
        &from,
        req.to.as_deref(),
    )?
    else {
        escalate(
            env,
            &task.id,
            "no worker available to take over",
            "wait for an agent to come back",
        );
        return Err(CliError::Invalid(anyhow!(
            "no worker available to take over {:?}",
            task.id
        )));
    };
    // Refuse before any effect: a refused hand-off must leave nothing behind.
    env.ctx.check(&EventKind::HandedOff {
        task: task.id.clone(),
        from_agent: from.clone(),
        to_agent: to.agent.clone(),
        checkpoint: "<pending>".to_string(),
    })?;
    let worktree = PathBuf::from(&attempt.worktree);
    let handoff_no = previous_handoffs(&events, &task.id, attempt.n) + 1;
    let brief_path = handoff_brief_path(&env.briefs_dir, &env.run, &task.id, attempt.n, handoff_no);
    let checkpoint = checkpoint(&worktree, &from)?;
    let workspace = spawn::workspace_of(mux, env)?;
    let pane_tail = old_pane_tail(mux, &workspace, env, &task.id, attempt);
    let stats = diffstat(&worktree, &env.base_branch);
    let skills = spawn::select_skills(&env.skills, &env.compat, &task.tags);
    let brief = handoff_brief(
        &BriefInput {
            task,
            run: &env.run,
            attempt: attempt.n,
            agent: &to.agent,
            model: to.model.as_deref(),
            branch: &attempt.branch,
            worktree: &attempt.worktree,
            skills: &skills,
        },
        &Transfer {
            from: &from,
            to: &to.agent,
            reason: &req.reason,
            checkpoint: &checkpoint,
            base_branch: &env.base_branch,
            diffstat: &stats,
            pane_tail: pane_tail.as_deref(),
        },
    );
    spawn::write_brief(&brief_path, &brief)?;
    close_old_tab(mux, env, &task.id, attempt);
    // The worktree is not created here: the newcomer continues in the one the
    // attempt already has, on the branch the checkpoint sits on.
    let continuing = Worktree {
        path: worktree,
        branch: attempt.branch.clone(),
        base_sha: checkpoint.clone(),
    };
    let opened = spawn::open_agent(
        mux,
        env,
        &workspace,
        &Launching {
            pick: &to,
            task: &task.id,
            effort: None,
            brief: &brief,
            brief_path: &brief_path,
            worktree: &continuing,
        },
    );
    let (tab_id, title) = match opened {
        Ok(opened) => opened,
        Err(err) => {
            escalate(
                env,
                &task.id,
                &format!("hand-off to {} failed: {err}", to.agent),
                "retry the hand-off",
            );
            return Err(CliError::other(anyhow!(
                "handing {:?} to {}: {err}",
                task.id,
                to.agent
            )));
        }
    };
    // The record follows the tab: a later prompt must reach the newcomer, not
    // the tab this hand-off just closed.
    spawn::write_tab(
        &spawn::tab_path(&env.briefs_dir, &env.run, &task.id, attempt.n),
        &tab_id,
    );
    let kind = EventKind::HandedOff {
        task: task.id.clone(),
        from_agent: from.clone(),
        to_agent: to.agent.clone(),
        checkpoint: checkpoint.clone(),
    };
    if let Err(err) = env.ctx.commit(kind) {
        spawn::close_tab(mux, &tab_id);
        return Err(err);
    }
    Ok(HandoffReport {
        task: task.id.clone(),
        attempt: attempt.n,
        from,
        to: to.agent.clone(),
        checkpoint,
        tab: title,
        brief: brief_path,
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    })
}

/// Closes the outgoing agent's tab, from the record the attempt left.
///
/// A host that no longer knows the tab (a human closed it, say) is not an
/// error: that is the state the hand-off wanted anyway.
fn close_old_tab(mux: &dyn Mux, env: &SpawnEnv<'_>, task: &str, attempt: &Attempt) {
    let record = spawn::tab_path(&env.briefs_dir, &env.run, task, attempt.n);
    let Ok(id) = std::fs::read_to_string(record) else {
        return;
    };
    let id = id.trim().to_string();
    if id.is_empty() {
        return;
    }
    let tab = TabId::new(id);
    if let Err(err) = mux.close_tab(&tab) {
        eprintln!("warning: could not close tab {tab}: {err}");
    }
}

/// Records something a human has to decide, warning when the log refuses it.
fn escalate(env: &SpawnEnv<'_>, task: &str, body: &str, default_action: &str) {
    let kind = EventKind::Escalated {
        task: Some(task.to_string()),
        body: body.to_string(),
        default_action: default_action.to_string(),
    };
    if let Err(err) = env.ctx.commit(kind) {
        eprintln!("warning: could not record the escalation: {err}");
    }
}

/// The `shprd agent handoff` verb: builds the environment, hands the task over
/// and prints what happened.
pub fn handoff_verb(ctx: &Ctx, args: &HandoffArgs, home: Option<&Path>) -> Result<(), CliError> {
    let mux = spawn::real_mux();
    let env = spawn::build_env(ctx, home)?;
    let request = HandoffRequest {
        task: args.task.clone(),
        reason: args.reason.clone(),
        to: args.to.clone(),
    };
    let report = handoff(&mux, &env, &request)?;
    if ctx.json {
        let value = serde_json::json!({
            "task": report.task,
            "attempt": report.attempt,
            "from": report.from,
            "to": report.to,
            "checkpoint": report.checkpoint,
            "tab": report.tab,
            "brief": report.brief.display().to_string(),
            "elapsed_ms": report.elapsed_ms,
        });
        println!("{value}");
        return Ok(());
    }
    println!(
        "ok handed {} attempt {} from {} to {} at {} (tab {:?})",
        report.task, report.attempt, report.from, report.to, report.checkpoint, report.tab
    );
    Ok(())
}
