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
use shprd_mux::{Mux, MuxError, TabId, Workspace};
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

/// The brief the attempt started with, as the agent that read it saw it.
///
/// The file the attempt was launched with is the record of what it was told, so
/// a hand-off continues that text instead of re-rendering it from state that
/// has moved on since. Only a missing or empty file falls back to the render.
fn base_brief(env: &SpawnEnv<'_>, task: &str, attempt: u32, input: &BriefInput<'_>) -> String {
    let path = spawn::brief_path(&env.briefs_dir, &env.run, task, attempt);
    match std::fs::read_to_string(&path) {
        Ok(text) if !text.trim().is_empty() => text,
        _ => spawn::render_brief(input),
    }
}

/// Renders the hand-off brief: the brief the attempt started with, then what
/// the newcomer needs that the outgoing agent did not have.
fn handoff_brief(base: &str, transfer: &Transfer<'_>) -> String {
    let mut out = base.to_string();
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
    let start = BriefInput {
        task,
        run: &env.run,
        attempt: attempt.n,
        agent: &to.agent,
        model: to.model.as_deref(),
        branch: &attempt.branch,
        worktree: &attempt.worktree,
        skills: &skills,
    };
    let brief = handoff_brief(
        &base_brief(env, &task.id, attempt.n, &start),
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
    // Two agents in one worktree would both write the same files, so the
    // hand-off stops here: the checkpoint commit is already the record of the
    // work the newcomer was meant to continue.
    match close_old_tab(mux, env, &task.id, attempt) {
        Ok(OldTab::Stopped) => {}
        Ok(OldTab::Unrecorded) => {
            let body = format!("cannot find {from}'s tab; hand-off aborted");
            escalate(env, &task.id, &body, "stop the run");
            return Err(CliError::other(anyhow!(body)));
        }
        Err(err) => {
            escalate(
                env,
                &task.id,
                &format!("could not stop {from}; hand-off aborted"),
                "stop the run",
            );
            return Err(err);
        }
    }
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
    let record = spawn::tab_path(&env.briefs_dir, &env.run, &task.id, attempt.n);
    if let Err(err) = spawn::write_tab_checked(&record, &tab_id) {
        // The newcomer is already running, but nothing would ever reach it: a
        // prompt follows the record, not the label. A cleanup the host refuses
        // is reported too, because a stray agent nobody has been told about is
        // one nobody stops.
        let body = match mux.close_tab(&tab_id) {
            Ok(()) => format!("hand-off to {} could not be recorded", to.agent),
            Err(close_err) => format!(
                "hand-off to {} could not be recorded; its tab could not be closed: \
                 {close_err}; stop it by hand",
                to.agent
            ),
        };
        escalate(env, &task.id, &body, "stop the run");
        return Err(err);
    }
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

/// What closing the outgoing tab proved about the old agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OldTab {
    /// The recorded tab was closed, or the host already did not know it.
    Stopped,
    /// No readable `.tab` record: nothing says the old agent ever started, let
    /// alone stopped, so the hand-off has no proof to proceed on.
    Unrecorded,
}

/// Closes the outgoing agent's tab, from the record the attempt left.
///
/// A host that no longer knows the tab (a human closed it, say) is not an
/// error: that is the state the hand-off wanted anyway. Any other failure is
/// one, because launching the newcomer while the outgoing agent still runs
/// would put two agents in one worktree. A missing record is reported as
/// [`OldTab::Unrecorded`] rather than success: without it there is no tab to
/// close and nothing to prove the old agent stopped.
fn close_old_tab(
    mux: &dyn Mux,
    env: &SpawnEnv<'_>,
    task: &str,
    attempt: &Attempt,
) -> Result<OldTab, CliError> {
    let record = spawn::tab_path(&env.briefs_dir, &env.run, task, attempt.n);
    let Ok(id) = std::fs::read_to_string(record) else {
        return Ok(OldTab::Unrecorded);
    };
    let id = id.trim().to_string();
    if id.is_empty() {
        return Ok(OldTab::Unrecorded);
    }
    let tab = TabId::new(id);
    match mux.close_tab(&tab) {
        Ok(()) | Err(MuxError::NotFound { .. }) => Ok(OldTab::Stopped),
        Err(err) => Err(CliError::other(anyhow!(
            "the outgoing agent's tab {tab} is still open: {err}"
        ))),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::process::Command;
    use std::time::Duration;

    use shprd_core::config::Skills;
    use shprd_core::event::Actor;
    use shprd_core::state::TaskStatus;
    use shprd_git::worktree::Layout;
    use shprd_mux::fake::{Call, FakeMux};
    use shprd_mux::naming;
    use shprd_roster::spec::HarnessSpec;
    use tempfile::TempDir;

    use crate::cmd::spawn::fault::{Fault, FlakyMux};
    use crate::cmd::spawn::{self, Program, SpawnRequest};
    use crate::ctx::Ctx;

    /// Runs one git command in `dir`, returning its trimmed stdout.
    fn git_out(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output()
            .unwrap_or_else(|err| panic!("git {args:?} failed: {err}"));
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    /// A harness spec for a fake CLI: the tests record the launch, never run
    /// it, so `interactive` is the harmless `sh`.
    fn fake_spec(id: &str) -> HarnessSpec {
        let text = format!(
            r#"
id = "{id}"
name = "Fake {id}"
vendor = "pi"
verified_version = "1.0.0"
dry_patterns = []

[detect]
binary = "{id}"
version_args = ["--version"]
signin_hint = "fake"
signin_args = []

[launch]
interactive = ["sh"]
model_args = ["--model", "{{model}}"]
verified = false

[models]
provider_qualified = false

[records]
kind = "jsonl"
location = "~/.fake/{{session}}.jsonl"
errors = "error"
verified = true

[quota]
source = "none"

[skills]
home_dirs = []
project_dirs = []
"#
        );
        toml::from_str::<HarnessSpec>(&text).unwrap_or_else(|err| panic!("the test spec: {err}"))
    }

    /// A repository with one commit on `main` and the integration branch, plus
    /// a home for briefs and worktrees.
    struct Scratch {
        repo: TempDir,
        home: TempDir,
    }

    impl Scratch {
        fn new() -> Scratch {
            let repo = TempDir::new().unwrap_or_else(|err| panic!("{err}"));
            let home = TempDir::new().unwrap_or_else(|err| panic!("{err}"));
            git_out(repo.path(), &["init", "-q", "-b", "main"]);
            std::fs::write(repo.path().join("README.md"), "scratch\n").unwrap_or_default();
            git_out(repo.path(), &["add", "."]);
            git_out(repo.path(), &["commit", "-q", "-m", "first"]);
            git_out(repo.path(), &["branch", "integration/m5"]);
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

        /// A context with a started run and one task `T1` in its plan.
        fn ctx(&self) -> Ctx {
            let ctx = Ctx::new(self.root().to_path_buf(), Actor::Human, false, true);
            ctx.commit(EventKind::RunStarted {
                run_id: "t1".to_string(),
                repo: self.repo_name(),
                base_branch: "main".to_string(),
                integration_branch: "integration/m5".to_string(),
            })
            .unwrap_or_else(|err| panic!("{err}"));
            ctx.commit(EventKind::TaskAdded {
                task: "T1".to_string(),
                title: "Hand a task over".to_string(),
                scope: vec!["crates/shprd".to_string()],
                acceptance: vec!["the next agent continues".to_string()],
                tags: vec!["rust".to_string()],
                skills: Vec::new(),
                deps: Vec::new(),
            })
            .unwrap_or_else(|err| panic!("{err}"));
            ctx
        }

        /// The environment a hand-off runs in, with `workers` configured.
        fn env<'a>(&self, ctx: &'a Ctx, workers: &[&str]) -> SpawnEnv<'a> {
            let mut base_env = BTreeMap::new();
            base_env.insert("PATH".to_string(), "/usr/bin".to_string());
            SpawnEnv {
                ctx,
                layout: Layout::default_for_home(self.home.path()),
                briefs_dir: self.home.path().join(".shprd").join("briefs"),
                run: "t1".to_string(),
                base_branch: "integration/m5".to_string(),
                repo_name: self.repo_name(),
                programs: workers
                    .iter()
                    .map(|id| Program {
                        spec: fake_spec(id),
                        installed: true,
                    })
                    .collect(),
                workers: workers.iter().map(|id| (*id).to_string()).collect(),
                skills: Skills {
                    workers: vec!["tdd".to_string()],
                    ..Skills::default()
                },
                compat: Vec::new(),
                base_env,
                // No wait: a fake pane that never reports an agent would
                // otherwise stall every test that launches one.
                ready_wait: Duration::ZERO,
            }
        }
    }

    /// A fake multiplexer holding the repo's workspace.
    fn mux_with_workspace(scratch: &Scratch) -> FakeMux {
        let mux = FakeMux::default().with_ready_agent();
        mux.create_workspace(
            &naming::workspace_name(&scratch.repo_name()),
            scratch.root(),
        )
        .unwrap_or_else(|err| panic!("{err}"));
        mux
    }

    /// Spawns the fake `sh` agent on `T1` and returns its worktree.
    fn seed(mux: &FakeMux, env: &SpawnEnv<'_>) -> PathBuf {
        let request = SpawnRequest {
            task: "T1".to_string(),
            agent: Some("sh".to_string()),
            effort: None,
        };
        let report = spawn::spawn(mux, env, &request).unwrap_or_else(|err| panic!("{err}"));
        report.worktree
    }

    /// What the human asked for: hand `T1` over, optionally to a named agent.
    fn request(to: Option<&str>) -> HandoffRequest {
        HandoffRequest {
            task: "T1".to_string(),
            reason: "the agent hit its limit".to_string(),
            to: to.map(str::to_string),
        }
    }

    #[test]
    fn a_hand_off_checkpoints_the_work_and_swaps_the_agent() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.env(&ctx, &["sh", "pi"]);
        let mux = mux_with_workspace(&scratch);
        let worktree = seed(&mux, &env);
        // The outgoing agent leaves half a change behind.
        std::fs::write(worktree.join("half.rs"), "fn half() {}\n").unwrap_or_default();
        let before = git_out(&worktree, &["rev-parse", "HEAD"]);
        let report = handoff(&mux, &env, &request(None)).unwrap_or_else(|err| panic!("{err}"));

        assert_eq!(report.from, "sh");
        assert_eq!(report.to, "pi");
        assert_eq!(report.attempt, 1);
        assert_ne!(report.checkpoint, before, "no checkpoint commit was made");
        assert_eq!(
            git_out(&worktree, &["rev-parse", "HEAD"]),
            report.checkpoint
        );
        assert_eq!(
            git_out(&worktree, &["log", "-1", "--format=%s"]),
            "wip(shprd): checkpoint before hand-off from sh"
        );
        assert!(
            git_out(&worktree, &["status", "--porcelain"]).is_empty(),
            "the checkpoint left the worktree dirty"
        );
        assert!(report.elapsed_ms < 60_000, "took {}ms", report.elapsed_ms);
        assert!(
            report.brief.to_string_lossy().ends_with("T1-a1-h1.md"),
            "{:?}",
            report.brief
        );
    }

    #[test]
    fn a_hand_off_records_the_event_and_moves_the_tab() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.env(&ctx, &["sh", "pi"]);
        let mux = mux_with_workspace(&scratch);
        let worktree = seed(&mux, &env);
        let record = spawn::tab_path(&env.briefs_dir, &env.run, "T1", 1);
        let old_tab = std::fs::read_to_string(&record)
            .unwrap_or_default()
            .trim()
            .to_string();

        let report = handoff(&mux, &env, &request(None)).unwrap_or_else(|err| panic!("{err}"));

        let state = ctx.state().unwrap_or_else(|err| panic!("{err}"));
        let task = state.tasks.get("T1").unwrap_or_else(|| panic!("no task"));
        assert_eq!(
            task.attempts.len(),
            1,
            "the hand-off must not add an attempt"
        );
        assert_eq!(task.attempts[0].agent, "pi");
        assert_eq!(task.status, TaskStatus::Working);
        let workspace = mux
            .find_workspace(&naming::workspace_name(&scratch.repo_name()))
            .unwrap_or_else(|err| panic!("{err}"))
            .unwrap_or_else(|| panic!("no workspace"));
        let tabs = mux
            .list_tabs(&workspace.id)
            .unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(tabs.len(), 1, "the old tab should be closed: {tabs:?}");
        assert_eq!(tabs[0].title, "\u{25cf} pi\u{b7}T1");
        assert!(
            mux.calls()
                .contains(&Call::CloseTab(TabId::new(old_tab.clone()))),
            "the old tab {:?} was not closed",
            old_tab
        );
        assert_eq!(
            std::fs::read_to_string(&record).unwrap_or_default().trim(),
            tabs[0].id.as_str(),
            "the tab record still points at the old tab"
        );
        let created = mux
            .calls()
            .iter()
            .rev()
            .find_map(|call| match call {
                Call::CreateTab(_, _, cwd) => Some(cwd.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no tab was created by the hand-off"));
        assert_eq!(created, worktree, "the newcomer sits elsewhere");
        assert_eq!(report.tab, "\u{25cf} pi\u{b7}T1");
    }

    #[test]
    fn the_brief_says_where_to_continue() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.env(&ctx, &["sh", "pi"]);
        let mux = mux_with_workspace(&scratch);
        let worktree = seed(&mux, &env);
        // What the outgoing agent last printed, which the newcomer should see.
        let pane = mux
            .calls()
            .iter()
            .find_map(|call| match call {
                Call::Run(pane, _, _) => Some(pane.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("the CLI was never started"));
        mux.set_screen(&pane, "half of the parser is written");
        std::fs::write(worktree.join("half.rs"), "fn half() {}\n").unwrap_or_default();

        let report = handoff(&mux, &env, &request(None)).unwrap_or_else(|err| panic!("{err}"));

        let brief = std::fs::read_to_string(&report.brief).unwrap_or_default();
        assert!(
            brief.contains("Hand a task over"),
            "the original brief: {brief}"
        );
        assert!(brief.contains("## You are taking over"), "{brief}");
        assert!(brief.contains("handed from `sh` to `pi`"), "{brief}");
        assert!(brief.contains("the agent hit its limit"), "{brief}");
        assert!(brief.contains(&report.checkpoint), "{brief}");
        assert!(brief.contains("do not start over"), "{brief}");
        assert!(
            brief.contains("half.rs"),
            "the diffstat is missing: {brief}"
        );
        assert!(brief.contains("half of the parser is written"), "{brief}");
    }

    #[test]
    fn no_worker_available_escalates_and_exits_two() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        // Only the agent handing over is configured, so the roster wraps onto
        // itself and comes up empty.
        let env = scratch.env(&ctx, &["sh"]);
        let mux = mux_with_workspace(&scratch);
        seed(&mux, &env);

        let err = match handoff(&mux, &env, &request(None)) {
            Ok(report) => panic!("handed off to {} anyway", report.to),
            Err(err) => err,
        };

        assert_eq!(err.exit_code(), 2);
        let state = ctx.state().unwrap_or_else(|err| panic!("{err}"));
        let escalation = state
            .escalations
            .last()
            .unwrap_or_else(|| panic!("nothing was escalated"));
        assert_eq!(escalation.body, "no worker available to take over");
        assert_eq!(escalation.default_action, "wait for an agent to come back");
        assert_eq!(
            state.tasks.get("T1").map(|task| task.attempts.len()),
            Some(1)
        );
    }

    #[test]
    fn a_dry_next_worker_is_skipped() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.env(&ctx, &["sh", "pi", "opencode"]);
        let mux = mux_with_workspace(&scratch);
        seed(&mux, &env);
        ctx.commit(EventKind::Dry {
            agent: "pi".to_string(),
            reason: "quota".to_string(),
            reset_at_ms: None,
        })
        .unwrap_or_else(|err| panic!("{err}"));

        let report = handoff(&mux, &env, &request(None)).unwrap_or_else(|err| panic!("{err}"));

        assert_eq!(report.to, "opencode");
    }

    #[test]
    fn an_invalid_to_exits_two_and_changes_nothing() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.env(&ctx, &["sh", "pi"]);
        let mux = mux_with_workspace(&scratch);
        let worktree = seed(&mux, &env);
        let before = git_out(&worktree, &["rev-parse", "HEAD"]);

        let err = match handoff(&mux, &env, &request(Some("nope/model"))) {
            Ok(report) => panic!("accepted {report:?}"),
            Err(err) => err,
        };

        assert_eq!(err.exit_code(), 2);
        assert_eq!(git_out(&worktree, &["rev-parse", "HEAD"]), before);
        let state = ctx.state().unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(
            state
                .tasks
                .get("T1")
                .and_then(|task| task.latest_attempt())
                .map(|attempt| attempt.agent.clone()),
            Some("sh".to_string())
        );
    }

    /// The number of tabs the hand-off opened so far.
    fn tabs_created(mux: &FakeMux) -> usize {
        mux.calls()
            .iter()
            .filter(|call| matches!(call, Call::CreateTab(..)))
            .count()
    }

    #[test]
    fn a_hand_off_that_cannot_stop_the_old_agent_starts_no_new_one() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.env(&ctx, &["sh", "pi"]);
        let mux = mux_with_workspace(&scratch);
        let worktree = seed(&mux, &env);
        // Work the checkpoint has something to commit.
        std::fs::write(worktree.join("half.rs"), "fn half() {}\n").unwrap_or_default();
        let before = git_out(&worktree, &["rev-parse", "HEAD"]);
        let flaky = FlakyMux::new(mux, Fault::CloseTab);
        let opened = tabs_created(flaky.inner());

        let err = match handoff(&flaky, &env, &request(None)) {
            Ok(report) => panic!("handed off to {} anyway", report.to),
            Err(err) => err,
        };

        assert_eq!(err.exit_code(), 1);
        assert_eq!(
            tabs_created(flaky.inner()),
            opened,
            "a second agent was launched into the worktree"
        );
        let (state, events) = ctx.load().unwrap_or_else(|err| panic!("{err}"));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event.kind, EventKind::HandedOff { .. })),
            "the aborted hand-off was recorded as done"
        );
        let escalation = state
            .escalations
            .last()
            .unwrap_or_else(|| panic!("nothing was escalated"));
        assert_eq!(escalation.body, "could not stop sh; hand-off aborted");
        assert_ne!(
            git_out(&worktree, &["rev-parse", "HEAD"]),
            before,
            "the checkpoint commit should stay"
        );
    }

    /// A missing `.tab` record proves nothing about the outgoing agent, so the
    /// hand-off must abort instead of starting a second agent beside it.
    #[test]
    fn a_hand_off_without_a_tab_record_starts_no_new_one() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.env(&ctx, &["sh", "pi"]);
        let mux = mux_with_workspace(&scratch);
        let worktree = seed(&mux, &env);
        // Work the checkpoint has something to commit.
        std::fs::write(worktree.join("half.rs"), "fn half() {}\n").unwrap_or_default();
        let before = git_out(&worktree, &["rev-parse", "HEAD"]);
        let record = spawn::tab_path(&env.briefs_dir, &env.run, "T1", 1);
        std::fs::remove_file(&record).unwrap_or_else(|err| panic!("{err}"));
        let opened = tabs_created(&mux);

        let err = match handoff(&mux, &env, &request(None)) {
            Ok(report) => panic!("handed off to {} anyway", report.to),
            Err(err) => err,
        };

        assert_eq!(err.exit_code(), 1);
        assert_eq!(
            tabs_created(&mux),
            opened,
            "a second agent was launched into the worktree"
        );
        let (state, events) = ctx.load().unwrap_or_else(|err| panic!("{err}"));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event.kind, EventKind::HandedOff { .. })),
            "the aborted hand-off was recorded as done"
        );
        assert_eq!(
            state
                .escalations
                .last()
                .map(|escalation| escalation.body.as_str()),
            Some("cannot find sh's tab; hand-off aborted")
        );
        assert_ne!(
            git_out(&worktree, &["rev-parse", "HEAD"]),
            before,
            "the checkpoint commit should stay"
        );
    }

    /// A [`FakeMux`] that closes `keep` but refuses every other tab: the old
    /// agent's tab must still close, so the hand-off reaches the record write,
    /// and the newcomer's cleanup close is the one that fails.
    struct RefusesNewcomerClose {
        inner: FakeMux,
        keep: TabId,
    }

    impl Mux for RefusesNewcomerClose {
        fn capabilities(&self) -> shprd_mux::Capabilities {
            self.inner.capabilities()
        }

        fn find_workspace(&self, name: &str) -> shprd_mux::Result<Option<Workspace>> {
            self.inner.find_workspace(name)
        }

        fn create_workspace(&self, name: &str, cwd: &Path) -> shprd_mux::Result<Workspace> {
            self.inner.create_workspace(name, cwd)
        }

        fn focus_workspace(&self, workspace: &shprd_mux::WorkspaceId) -> shprd_mux::Result<()> {
            self.inner.focus_workspace(workspace)
        }

        fn list_tabs(
            &self,
            workspace: &shprd_mux::WorkspaceId,
        ) -> shprd_mux::Result<Vec<shprd_mux::Tab>> {
            self.inner.list_tabs(workspace)
        }

        fn create_tab(
            &self,
            workspace: &shprd_mux::WorkspaceId,
            title: &str,
            cwd: &Path,
        ) -> shprd_mux::Result<shprd_mux::Tab> {
            self.inner.create_tab(workspace, title, cwd)
        }

        fn rename_tab(&self, tab: &TabId, title: &str) -> shprd_mux::Result<()> {
            self.inner.rename_tab(tab, title)
        }

        fn focus_tab(&self, tab: &TabId) -> shprd_mux::Result<()> {
            self.inner.focus_tab(tab)
        }

        fn close_tab(&self, tab: &TabId) -> shprd_mux::Result<()> {
            if *tab != self.keep {
                return Err(MuxError::command("close tab", "the host refused"));
            }
            self.inner.close_tab(tab)
        }

        fn split(
            &self,
            pane: &shprd_mux::PaneId,
            direction: shprd_mux::Direction,
            ratio: f32,
            cwd: &Path,
        ) -> shprd_mux::Result<shprd_mux::PaneId> {
            self.inner.split(pane, direction, ratio, cwd)
        }

        fn run(
            &self,
            pane: &shprd_mux::PaneId,
            argv: &[String],
            env: &[(String, Option<String>)],
        ) -> shprd_mux::Result<()> {
            self.inner.run(pane, argv, env)
        }

        fn send_text(&self, pane: &shprd_mux::PaneId, text: &str) -> shprd_mux::Result<()> {
            self.inner.send_text(pane, text)
        }

        fn prompt(&self, pane: &shprd_mux::PaneId, text: &str) -> shprd_mux::Result<()> {
            self.inner.prompt(pane, text)
        }

        fn read(&self, pane: &shprd_mux::PaneId, lines: usize) -> shprd_mux::Result<String> {
            self.inner.read(pane, lines)
        }

        fn agent_state(
            &self,
            pane: &shprd_mux::PaneId,
        ) -> shprd_mux::Result<shprd_mux::AgentState> {
            self.inner.agent_state(pane)
        }

        fn next_events(&self, timeout: Duration) -> shprd_mux::Result<Vec<shprd_mux::MuxEvent>> {
            self.inner.next_events(timeout)
        }
    }

    /// A cleanup the host refused is the human's only clue that a stray agent is
    /// still running, so the escalation must say so rather than hide it.
    #[test]
    fn a_hand_off_that_cannot_record_and_cannot_clean_up_says_so() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.env(&ctx, &["sh", "pi"]);
        let mux = mux_with_workspace(&scratch);
        seed(&mux, &env);
        let record = spawn::tab_path(&env.briefs_dir, &env.run, "T1", 1);
        let old = std::fs::read_to_string(&record).unwrap_or_else(|err| panic!("{err}"));
        // Readable, so the old tab is still closed, but not writable.
        std::fs::set_permissions(&record, std::fs::Permissions::from_mode(0o400))
            .unwrap_or_else(|err| panic!("{err}"));
        let mux = RefusesNewcomerClose {
            inner: mux,
            keep: TabId::new(old.trim()),
        };

        let err = match handoff(&mux, &env, &request(None)) {
            Ok(report) => panic!("handed off to {} anyway", report.to),
            Err(err) => err,
        };

        assert_eq!(err.exit_code(), 1);
        let (state, events) = ctx.load().unwrap_or_else(|err| panic!("{err}"));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event.kind, EventKind::HandedOff { .. })),
            "an unrecorded hand-off was logged as done"
        );
        assert_eq!(
            state
                .escalations
                .last()
                .map(|escalation| escalation.body.as_str()),
            Some(
                "hand-off to pi could not be recorded; its tab could not be closed: \
                 close tab: the host refused; stop it by hand"
            )
        );
    }

    /// A tab no record points at is a tab no prompt reaches, so a hand-off that
    /// cannot write its record must not report success.
    #[test]
    fn a_hand_off_that_cannot_record_its_tab_fails() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.env(&ctx, &["sh", "pi"]);
        let mux = mux_with_workspace(&scratch);
        seed(&mux, &env);
        let record = spawn::tab_path(&env.briefs_dir, &env.run, "T1", 1);
        // Readable, so the old tab is still closed, but not writable.
        std::fs::set_permissions(&record, std::fs::Permissions::from_mode(0o400))
            .unwrap_or_else(|err| panic!("{err}"));

        let err = match handoff(&mux, &env, &request(None)) {
            Ok(report) => panic!("handed off to {} anyway", report.to),
            Err(err) => err,
        };

        assert_eq!(err.exit_code(), 1);
        let (state, events) = ctx.load().unwrap_or_else(|err| panic!("{err}"));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event.kind, EventKind::HandedOff { .. })),
            "an unrecorded hand-off was logged as done"
        );
        assert_eq!(
            state
                .escalations
                .last()
                .map(|escalation| escalation.body.as_str()),
            Some("hand-off to pi could not be recorded")
        );
        let workspace = mux
            .find_workspace(&naming::workspace_name(&scratch.repo_name()))
            .unwrap_or_else(|err| panic!("{err}"))
            .unwrap_or_else(|| panic!("no workspace"));
        let tabs = mux
            .list_tabs(&workspace.id)
            .unwrap_or_else(|err| panic!("{err}"));
        assert!(
            tabs.is_empty(),
            "the newcomer's tab was left open: {tabs:?}"
        );
    }

    /// The newcomer continues the brief the attempt actually ran with, not a
    /// rendering of the state as it looks now.
    #[test]
    fn the_hand_off_brief_starts_from_the_attempt_s_real_brief() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.env(&ctx, &["sh", "pi"]);
        let mux = mux_with_workspace(&scratch);
        seed(&mux, &env);
        let original = spawn::brief_path(&env.briefs_dir, &env.run, "T1", 1);
        std::fs::write(&original, "# the brief this attempt ran with\n")
            .unwrap_or_else(|err| panic!("{err}"));

        let report = handoff(&mux, &env, &request(None)).unwrap_or_else(|err| panic!("{err}"));

        let brief = std::fs::read_to_string(&report.brief).unwrap_or_default();
        assert!(
            brief.starts_with("# the brief this attempt ran with"),
            "the hand-off did not continue the attempt's brief: {brief:?}"
        );
        assert!(brief.contains("## You are taking over"), "{brief:?}");
    }
}
