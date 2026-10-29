//! The `watch` verb: poll every working agent and apply the dry/hand-off state
//! machine to what its pane and its transcript say.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use shprd_core::event::{Actor, Event, EventKind};
use shprd_core::gate::REVIEW_GATE;
use shprd_core::log::{Clock, SystemClock};
use shprd_core::state::{RunState, Task, TaskStatus};
use shprd_mux::{Mux, MuxError, PaneId, naming};
use shprd_watch::machine::{Action, EXIT_REASON, Health, Inputs, ParkReason, Policy, Watch};
use shprd_watch::records::RecordError;
use shprd_watch::records::claude::scan_claude;
use shprd_watch::records::pi::scan_pi;
use shprd_watch::records::sqlite::{scan_kilo, scan_opencode};
use shprd_watch::screen::{Sampler, ScreenSignal, sample};

use crate::cli::WatchArgs;
use crate::cmd::handoff::{HandoffRequest, handoff};
use crate::cmd::spawn::{self, Program, SpawnEnv};
use crate::cmd::{review, verdict};
use crate::ctx::{CliError, Ctx};

/// The newest `*.jsonl` file in `dir`, if the directory holds one.
///
/// A directory that cannot be read yields `None`: a missing record says
/// nothing about the agent's health.
fn newest_jsonl(dir: &Path) -> Option<PathBuf> {
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()? {
        let path = entry.ok()?.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
        let later = match &newest {
            Some((seen, _)) => modified >= *seen,
            None => true,
        };
        if later {
            newest = Some((modified, path));
        }
    }
    newest.map(|(_, path)| path)
}

/// The newest record `claude` wrote for `worktree`.
///
/// Its project directory is the worktree path with every `/` and `.` turned
/// into `-`, so `/.worktrees` becomes `--worktrees`.
fn claude_record(home: &Path, worktree: &Path) -> Option<PathBuf> {
    let slug = worktree.to_string_lossy().replace(['/', '.'], "-");
    newest_jsonl(&home.join(".claude").join("projects").join(slug))
}

/// The newest record `pi` wrote for `worktree`.
///
/// Its session directory is the worktree path without the leading `/`, with
/// `/` turned into `-`, wrapped in `--`.
fn pi_record(home: &Path, worktree: &Path) -> Option<PathBuf> {
    let slug = worktree
        .to_string_lossy()
        .trim_start_matches('/')
        .replace('/', "-");
    let dir = home.join(".pi").join("agent").join("sessions");
    newest_jsonl(&dir.join(format!("--{slug}--")))
}

/// The newest provider error `program` recorded for `worktree` at or after
/// `since_ms`.
///
/// A record that cannot be read is not an error here: a missing or
/// half-written transcript is not evidence that the agent is dry, and codex
/// keeps no record we can classify, so both answer `None`.
fn record_error(
    program: &Program,
    home: &Path,
    worktree: &Path,
    since_ms: u64,
) -> Option<RecordError> {
    let db = |name: &str| home.join(".local").join("share").join(name);
    let scan = match program.spec.id.as_str() {
        "claude" => scan_claude(&claude_record(home, worktree)?, since_ms),
        "pi" => scan_pi(&pi_record(home, worktree)?, since_ms),
        "opencode" => scan_opencode(&db("opencode").join("opencode.db"), worktree, since_ms),
        "kilo" => scan_kilo(&db("kilo").join("kilo.db"), worktree, since_ms),
        _ => return None,
    };
    scan.ok()?.last_error
}

/// Every working attempt's watchdog, one pane each.
pub struct Watcher {
    /// Per attempt: the state machine's memory and the screen sampler.
    watches: BTreeMap<(String, u32, String), (Watch, Sampler)>,
    /// Per agent that went dry: the memory its attempt left behind.
    ///
    /// An attempt's watch is dropped when it is handed off, but the outgoing
    /// agent's dry mark outlives the attempt: without this map nothing would
    /// ever clear it, and the roster would skip that agent for good.
    cooldowns: BTreeMap<String, Watch>,
    /// The thresholds and backoffs the state machine runs under.
    policy: Policy,
    /// Per rejected `findings.json`: the mtime already escalated.
    ///
    /// A reviewer's bad file is one escalation, not one per tick: the mark is
    /// cleared by a rewrite, which is the reviewer's second chance.
    rejected: BTreeMap<PathBuf, SystemTime>,
}

impl Watcher {
    /// A watcher with no memory yet, running under `policy`.
    pub fn new(policy: Policy) -> Self {
        Self {
            watches: BTreeMap::new(),
            cooldowns: BTreeMap::new(),
            policy,
            rejected: BTreeMap::new(),
        }
    }

    /// Polls every working attempt once at `now_ms`, returning what it did.
    ///
    /// One task going wrong never stops the pass: a failed hand-off is
    /// escalated and the next task is still watched.
    pub fn tick(
        &mut self,
        mux: &dyn Mux,
        env: &SpawnEnv<'_>,
        now_ms: u64,
    ) -> Result<Vec<String>, CliError> {
        let (state, events) = env.ctx.load()?;
        let mut lines = Vec::new();
        self.tick_cooldowns(env, &state, now_ms, &mut lines)?;
        self.tick_reviews(mux, env, &state, &mut lines)?;
        for task in state.tasks.values() {
            if task.status != TaskStatus::Working {
                continue;
            }
            tick_task(self, mux, env, (&state, &events), task, now_ms, &mut lines)?;
        }
        let working = |task: &String, n: &u32, agent: &String| {
            state
                .tasks
                .get(task)
                .and_then(|task| task.attempts.last())
                .is_some_and(|last| last.n == *n && &last.agent == agent)
        };
        self.watches
            .retain(|key, _| working(&key.0, &key.1, &key.2));
        Ok(lines)
    }

    /// Records the verdict of every review that has finished writing.
    ///
    /// The reviewer runs outside the repository, so it cannot record anything
    /// itself: it writes `findings.json` and stops, and this pass is what turns
    /// that file into the gate result the run waits for.
    fn tick_reviews(
        &mut self,
        mux: &dyn Mux,
        env: &SpawnEnv<'_>,
        state: &RunState,
        lines: &mut Vec<String>,
    ) -> Result<(), CliError> {
        let Some(reviews) = reviews_dir(env) else {
            return Ok(());
        };
        // A verdict is the engine's to record, whoever started the watch: the
        // reviewer writes findings and nothing else, so the actor that folds
        // them into a gate result is always the engine.
        let engine = env.ctx.as_actor(Actor::Engine);
        for task in state.tasks.values() {
            let Some(attempt) = awaiting_review(task) else {
                continue;
            };
            let path = review::findings_path(&reviews, &env.run, &task.id, attempt);
            let Some(modified) = settled_mtime(&path) else {
                continue;
            };
            if verdict::read_findings(&path).is_err() {
                if self.rejected.get(&path) == Some(&modified) {
                    continue;
                }
                self.rejected.insert(path, modified);
                env.ctx.commit(EventKind::Escalated {
                    task: Some(task.id.clone()),
                    body: format!("reviewer wrote an invalid findings.json for {}", task.id),
                    default_action: INVALID_FINDINGS_ACTION.to_string(),
                })?;
                lines.push(format!("escalated {}: invalid findings.json", task.id));
                continue;
            }
            let outcome = verdict::submit_verdict(mux, &engine, home_of(env), &task.id, attempt)?;
            let word = match outcome {
                verdict::Outcome::Passed => "passed",
                verdict::Outcome::SentBack { .. } | verdict::Outcome::Escalated { .. } => "failed",
            };
            lines.push(format!("\u{25c7} {} review {word}", task.id));
        }
        Ok(())
    }

    /// Polls the agents a hand-off left behind, so their cooldown still ends.
    ///
    /// A watch that emitted [`Action::HandOff`] moves here when its attempt is
    /// over: the outgoing agent is still marked dry in the log, and only a
    /// probe that is due can clear that mark. An entry the log no longer calls
    /// dry is dropped, because there is nothing left to clear.
    fn tick_cooldowns(
        &mut self,
        env: &SpawnEnv<'_>,
        state: &RunState,
        now_ms: u64,
        lines: &mut Vec<String>,
    ) -> Result<(), CliError> {
        let policy = self.policy.clone();
        let idle = Inputs {
            now_ms,
            record_error: None,
            serve: None,
            screen: None,
            process_exited: false,
        };
        let mut finished: Vec<String> = Vec::new();
        for (agent, watch) in self.cooldowns.iter_mut() {
            if !state.agents.get(agent).is_some_and(|info| info.dry) {
                finished.push(agent.clone());
                continue;
            }
            let probe_due = watch
                .step(&idle, &policy)
                .iter()
                .any(|action| matches!(action, Action::Canary));
            if !probe_due {
                continue;
            }
            // No quota call is wired yet, so a probe that is due always reads
            // as recovered: the dry mark existed to make the agent wait, and
            // the wait is over.
            let follow = watch.canary_result(true, now_ms, &policy);
            if !follow
                .iter()
                .any(|action| matches!(action, Action::MarkAvailable))
            {
                continue;
            }
            // The watchdog is the engine, whatever actor started it: a
            // re-admission is the engine's, and only the engine or a human
            // may record one.
            env.ctx
                .as_actor(Actor::Engine)
                .commit(EventKind::Available {
                    agent: agent.clone(),
                    reason: String::from("reset time passed"),
                })?;
            lines.push(format!("available again: {agent}"));
            finished.push(agent.clone());
        }
        for agent in finished {
            self.cooldowns.remove(&agent);
        }
        Ok(())
    }
}

/// One pass over one working task: read its pane and record, then act.
fn tick_task(
    watcher: &mut Watcher,
    mux: &dyn Mux,
    env: &SpawnEnv<'_>,
    run: (&RunState, &[Event]),
    task: &Task,
    now_ms: u64,
    lines: &mut Vec<String>,
) -> Result<(), CliError> {
    let (state, events) = run;
    let Some(attempt) = task.attempts.last() else {
        return Ok(());
    };
    let agent = attempt.agent.clone();
    let Some(tab) = recorded_tab(env, &task.id, attempt.n) else {
        return Ok(());
    };
    let Ok(program) = spawn::program_of(&env.programs, &agent) else {
        return Ok(());
    };

    let pane = match pane_for(mux, env, &tab) {
        Ok(pane) => pane,
        Err(MuxError::NotFound { .. }) => None,
        // A host that cannot answer says nothing about the agent: skipping the
        // tick keeps the watch, because a transient error is not an exit.
        Err(err) => {
            eprintln!("warning: cannot look up the tab of {}: {err}", task.id);
            return Ok(());
        }
    };
    let key = (task.id.clone(), attempt.n, agent.clone());
    // What the log already knows outlives any one watchdog: a restart must not
    // hand off a worker that already died its budget's worth of times, nor
    // escalate an attempt a human has already been asked about.
    let streak = exit_streak(events, &task.id);
    let parked = parked(events, &task.id);
    let (watch, sampler) = watcher
        .watches
        .entry(key.clone())
        .or_insert_with(|| (Watch::new(), Sampler::new()));
    watch.resume(streak, parked);
    let (screen, process_exited) = observe(
        mux,
        pane.as_ref(),
        &program.spec.dry_patterns,
        sampler,
        now_ms,
    );
    let since_ms = spawned_at_ms(events, &task.id, attempt.n);
    // A pane that is not there yet is not a worker that died: a hand-off starts
    // a new process, and reading the gap as an exit would spend the whole exit
    // budget before the newcomer had a chance to run. A start the log places
    // after `now_ms` means the two clocks disagree, which proves nothing
    // either, so only a start this tick can measure counts.
    let started_at = worker_started_at_ms(events, &task.id, attempt.n);
    let starting_up = started_at <= now_ms && now_ms.saturating_sub(started_at) < EXIT_GRACE_MS;
    let process_exited = process_exited && !starting_up;
    let record_error = home_of(env)
        .and_then(|home| record_error(program, home, Path::new(&attempt.worktree), since_ms));
    let inputs = Inputs {
        now_ms,
        record_error,
        serve: None,
        screen,
        process_exited,
    };
    let mut queue = watch.step(&inputs, &watcher.policy);
    let mut handed_off: Option<(String, bool)> = None;
    queue.reverse();
    while let Some(action) = queue.pop() {
        match action {
            Action::MarkDry {
                reason,
                reset_at_ms,
            } => {
                if state.agents.get(&agent).is_some_and(|info| info.dry) {
                    continue;
                }
                env.ctx.commit(EventKind::Dry {
                    agent: agent.clone(),
                    reason: reason.clone(),
                    reset_at_ms,
                })?;
                lines.push(format!("dry {} ({agent}): {reason}", task.id));
            }
            Action::HandOff { reason, dry } => {
                let request = HandoffRequest {
                    task: task.id.clone(),
                    reason,
                    to: None,
                    // A dry agent needs a different worker; an agent whose
                    // process died is still perfectly able to take its own
                    // task back in a fresh process.
                    same_worker: !dry,
                };
                match handoff(mux, env, &request) {
                    Ok(report) => {
                        lines.push(format!("handed off {} to {}", task.id, report.to));
                        handed_off = Some((report.to, dry));
                    }
                    Err(err) => {
                        escalate(env, &task.id, &err, lines);
                        // One failed hand-off is one escalation, not one a
                        // tick: the same call would fail the same way, so the
                        // watch parks until the log says something changed.
                        watch.park(
                            now_ms,
                            format!("{}: {err}", ParkReason::HandoffFailed.marker()),
                        );
                        break;
                    }
                }
            }
            Action::Escalate { reason } => {
                env.ctx.commit(EventKind::Escalated {
                    task: Some(task.id.clone()),
                    body: format!("{agent} {reason}"),
                    default_action: String::from("pick the task up by hand"),
                })?;
                lines.push(format!("escalated {}: {reason}", task.id));
            }
            // No quota call is wired yet, so a probe that is due always reads
            // as recovered: the dry mark existed to make the agent wait, and
            // the wait is over.
            Action::Canary => {
                let due = matches!(
                    &watch.health,
                    Health::Dry { retry_at_ms, .. } if now_ms >= *retry_at_ms
                );
                if due {
                    let mut follow = watch.canary_result(true, now_ms, &watcher.policy);
                    follow.reverse();
                    queue.extend(follow);
                }
            }
            Action::MarkAvailable => {
                env.ctx.commit(EventKind::Available {
                    agent: agent.clone(),
                    reason: String::from("reset time passed"),
                })?;
                lines.push(format!("available again: {agent}"));
            }
        }
    }
    // The attempt is over, but what the watch knows is not always done with.
    if let Some((to, dry)) = handed_off
        && let Some((mut watch, _)) = watcher.watches.remove(&key)
    {
        if dry {
            // The outgoing agent's cooldown outlives the attempt: its watch
            // moves to the map that clears the dry mark when the probe is due.
            watcher.cooldowns.insert(agent.clone(), watch);
        } else {
            // A lost worker dry nothing, so the attempt simply continues under
            // its new process. The watch follows it, because the count of exits
            // is what turns a crash loop into an escalation.
            watch.after_handoff();
            watcher
                .watches
                .insert((task.id.clone(), attempt.n, to), (watch, Sampler::new()));
        }
    }
    Ok(())
}

/// Records that a hand-off failed, without stopping the pass.
fn escalate(env: &SpawnEnv<'_>, task: &str, err: &CliError, lines: &mut Vec<String>) {
    let kind = EventKind::Escalated {
        task: Some(task.to_string()),
        body: format!(
            "{}: {}",
            ParkReason::HandoffFailed.marker(),
            handoff_failure_detail(task, err)
        ),
        default_action: String::from("stop the run"),
    };
    match env.ctx.commit(kind) {
        Ok(_) => lines.push(format!("escalated {task}: {err}")),
        Err(err) => lines.push(format!("cannot escalate {task}: {err}")),
    }
}

/// How long a worker may still show no pane after it started before the
/// watchdog reads that as its process having exited.
///
/// A hand-off writes its event before the newcomer's tab answers, so without
/// this every hand-off would look like two exits in a row.
const EXIT_GRACE_MS: u64 = 60_000;

/// The tab id an attempt recorded when it spawned, if the record is there.
fn recorded_tab(env: &SpawnEnv<'_>, task: &str, attempt: u32) -> Option<String> {
    let path = spawn::tab_path(&env.briefs_dir, &env.run, task, attempt);
    Some(std::fs::read_to_string(path).ok()?.trim().to_string())
}

/// The home directory the environment's briefs sit under.
fn home_of<'a>(env: &'a SpawnEnv<'_>) -> Option<&'a Path> {
    env.briefs_dir.parent()?.parent()
}

/// The directory review packets live under, `<home>/.shprd/reviews`.
fn reviews_dir(env: &SpawnEnv<'_>) -> Option<PathBuf> {
    Some(home_of(env)?.join(".shprd").join("reviews"))
}

/// How long a findings file must sit untouched before a tick trusts it.
///
/// A reviewer writes its file in one go, but a tick can fall between the open
/// and the last byte: reading a file younger than this would decide a review
/// from half of it.
const FINDINGS_SETTLE: Duration = Duration::from_secs(2);

/// What a human is asked to do about a reviewer's invalid findings file.
const INVALID_FINDINGS_ACTION: &str = "read the packet and decide the review by hand";

/// The mtime of `path`, when the file is old enough for the watchdog to read.
///
/// A restart asks the same question: findings that are not settled yet may
/// still be being written, so `review --again` must not delete them.
pub(crate) fn settled_mtime(path: &Path) -> Option<SystemTime> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let age = SystemTime::now().duration_since(modified).ok()?;
    (age >= FINDINGS_SETTLE).then_some(modified)
}

/// The attempt of `task` a review is still owed for, if there is one.
///
/// A review was asked for, and no review gate result is recorded yet: the task
/// sits in `Reported` until that result arrives, however long the reviewer
/// takes.
fn awaiting_review(task: &Task) -> Option<u32> {
    if task.status != TaskStatus::Reported {
        return None;
    }
    let attempt = task.attempts.last()?;
    let decided = attempt.gates.iter().any(|gate| gate.gate == REVIEW_GATE);
    (!attempt.reviews.is_empty() && !decided).then_some(attempt.n)
}

/// The pane `tab` holds, or `None` when the host no longer has it.
///
/// Only a host that answered without the tab says the agent is gone. A host
/// that failed to answer is an error the caller must not read as an exit: a
/// transient failure would otherwise mark a working agent dry and hand its task
/// to somebody else.
fn pane_for(mux: &dyn Mux, env: &SpawnEnv<'_>, tab: &str) -> Result<Option<PaneId>, MuxError> {
    let name = naming::workspace_name(&env.repo_name);
    let Some(workspace) = mux.find_workspace(&name)? else {
        return Ok(None);
    };
    let tabs = mux.list_tabs(&workspace.id)?;
    Ok(tabs
        .iter()
        .find(|listed| listed.id.as_str() == tab)
        .map(|listed| listed.root_pane.clone()))
}

/// Reads the pane once, folding a closed pane into "the process exited".
fn observe(
    mux: &dyn Mux,
    pane: Option<&PaneId>,
    patterns: &[String],
    sampler: &mut Sampler,
    now_ms: u64,
) -> (Option<ScreenSignal>, bool) {
    let Some(pane) = pane else {
        return (None, true);
    };
    match sample(mux, pane, patterns, sampler, now_ms) {
        Ok(signal) => (Some(signal), false),
        Err(MuxError::NotFound { .. }) => (None, true),
        Err(_) => (None, false),
    }
}

/// When the attempt's current worker started: its spawn, or the hand-off that
/// replaced it, whichever is newer. Zero when the log says neither, which only
/// errs towards believing the first observation.
fn worker_started_at_ms(events: &[Event], task: &str, attempt: u32) -> u64 {
    events
        .iter()
        .rev()
        .find(|event| match &event.kind {
            EventKind::Spawned {
                task: id,
                attempt: n,
                ..
            } => id == task && *n == attempt,
            // A hand-off does not carry the attempt, but it cannot cross one
            // either: a new attempt starts with a spawn.
            EventKind::HandedOff { task: id, .. } => id == task,
            _ => false,
        })
        .map(|event| event.at_ms)
        .unwrap_or(0)
}

/// The words a failed hand-off is escalated in.
fn handoff_failure_detail(task: &str, err: &CliError) -> String {
    format!("the hand-off of {task} failed: {err}")
}

/// Whether this task's watch is parked, and since when.
///
/// The one rule for the whole question: among the task's events of kind
/// `Spawned`, `HandedOff` and a parking `Escalated`, the latest by `seq` decides
/// it, and the watch is parked iff that latest event is a parking escalation.
/// So every way of lifting a parking is the same event shape - a spawn or a
/// hand-off after the escalation, whether a new attempt, a human handing the
/// task to a fresh worker or a human retrying a failed hand-off - and every
/// other kind of escalation, which is a human's to answer, leaves the parking
/// exactly as it was.
fn parked(events: &[Event], task: &str) -> Option<(u64, ParkReason)> {
    let mut latest: Option<(u64, Option<(u64, ParkReason)>)> = None;
    for event in events {
        let parking = match &event.kind {
            EventKind::Spawned { task: id, .. } | EventKind::HandedOff { task: id, .. }
                if id == task =>
            {
                None
            }
            EventKind::Escalated {
                task: Some(id),
                body,
                ..
            } if id == task => {
                // An escalation about anything but a parking leaves the rule's
                // question unanswered, so it must not count as "the latest".
                let Some(why) = ParkReason::of_body(body) else {
                    continue;
                };
                Some((event.at_ms, why))
            }
            _ => continue,
        };
        if latest.is_none_or(|(seq, _)| event.seq >= seq) {
            latest = Some((event.seq, parking));
        }
    }
    latest.and_then(|(_, parking)| parking)
}

/// How many of this task's workers the log says have already exited in a row.
///
/// The log is the one source of truth for the exit streak: a watchdog that
/// restarts reads the count back instead of starting at zero, so a crash loop
/// still meets the budget it earned. Only a `HandedOff` carrying [`EXIT_REASON`]
/// is an exit, and it adds one. A spawn starts the count over, and so does a
/// hand-off for any other reason - a human handing the task to another worker -
/// because both are a new worker, which has proved nothing yet and must not
/// inherit the budget the last one spent.
fn exit_streak(events: &[Event], task: &str) -> usize {
    let mut streak: usize = 0;
    for event in events {
        match &event.kind {
            EventKind::Spawned { task: id, .. } if id == task => streak = 0,
            EventKind::HandedOff {
                task: id, reason, ..
            } if id == task => {
                streak = if reason.as_deref() == Some(EXIT_REASON) {
                    streak.saturating_add(1)
                } else {
                    0
                };
            }
            _ => {}
        }
    }
    streak
}

/// When the attempt's `spawned` event was written, or zero if the log lost it.
fn spawned_at_ms(events: &[Event], task: &str, attempt: u32) -> u64 {
    events
        .iter()
        .rev()
        .find(|event| match &event.kind {
            EventKind::Spawned {
                task: id,
                attempt: n,
                ..
            } => id == task && *n == attempt,
            _ => false,
        })
        .map(|event| event.at_ms)
        .unwrap_or(0)
}

/// The line a watchdog prints when the run already has one.
const ALREADY_WATCHING: &str = "another watchdog is already running for this run";

/// The file a run's single watchdog holds: `watch.lock` beside the event log,
/// so one run's claim is never another's.
fn lock_path(log_path: &Path) -> PathBuf {
    log_path.with_file_name("watch.lock")
}

/// Claims the run's watchdog slot, holding it until the process exits.
///
/// One watchdog per run is what makes the verdicts and the hand-offs add up,
/// and `up` cannot promise it alone: when a host refuses to close both the
/// stale pane and its replacement, two `shprd watch` processes are left in one
/// run. The kernel's lock settles that without one polling the other: the OS
/// drops it when the process dies, so a crashed watchdog never leaves a claim
/// behind, and a duplicate gets `Ok(None)` and steps aside.
///
/// `Ok(None)` is not an error: the duplicate is harmless once it exits.
fn claim_run(log_path: &Path) -> Result<Option<std::fs::File>, CliError> {
    let path = lock_path(log_path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| lock_error(&path, source))?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|source| lock_error(&path, source))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(source)) => Err(lock_error(&path, source)),
    }
}

/// The failure a lock file's I/O maps to, naming the file.
fn lock_error(path: &Path, source: std::io::Error) -> CliError {
    CliError::other(anyhow::anyhow!(
        "could not lock {}: {source}",
        path.display()
    ))
}

/// The `watch` verb: poll every working agent once per interval.
///
/// The loop only ends when the caller stops it: an unattended run wants the
/// watchdog up for as long as the run lasts, and `--once` is how a caller asks
/// for a single pass.
pub fn run_verb(ctx: &Ctx, args: &WatchArgs, home: Option<&Path>) -> Result<(), CliError> {
    // The claim comes first, before the watchdog reads anything: a duplicate
    // must do nothing but step aside, and a second reading of the run's state
    // is already a second watchdog's work.
    let Some(_claim) = claim_run(&ctx.log_path)? else {
        println!("{ALREADY_WATCHING}");
        return Ok(());
    };
    let mux = spawn::real_mux();
    let env = spawn::build_env(ctx, home)?;
    let mut watcher = Watcher::new(Policy::default());
    loop {
        let now_ms = SystemClock.now_ms();
        for line in watcher.tick(&mux, &env, now_ms)? {
            println!("{line}");
        }
        if args.once {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(args.interval_ms));
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::process::Command;

    use shprd_core::config::Skills;
    use shprd_core::event::{Actor, ReportStatus, ResourceAction, ResourceKind};
    use shprd_git::worktree::Layout;
    use shprd_mux::fake::FakeMux;
    use shprd_mux::{AgentState, PaneId};
    use shprd_roster::spec::HarnessSpec;
    use tempfile::TempDir;

    use crate::cmd::spawn::SpawnRequest;
    use crate::cmd::spawn::fault::{Fault, FlakyMux};

    use super::*;

    /// A worktree path with both a dot and several separators in it.
    const WORKTREE: &str = "/repos/.worktrees/shprd/m5-watch";

    /// An empty home directory.
    fn home() -> TempDir {
        TempDir::new().unwrap_or_else(|err| panic!("{err}"))
    }

    /// Writes a one-line record `name` under `dir`, returning its path.
    fn touch(dir: &Path, name: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap_or_else(|err| panic!("{err}"));
        let path = dir.join(name);
        std::fs::write(&path, "{}\n").unwrap_or_else(|err| panic!("{err}"));
        path
    }

    #[test]
    fn the_lock_sits_beside_the_event_log() {
        assert_eq!(
            lock_path(Path::new("/repos/shprd/.shprd/events.jsonl")),
            Path::new("/repos/shprd/.shprd/watch.lock")
        );
        // The duplicate is told why it exits, in the words the human reads.
        assert!(
            ALREADY_WATCHING.contains("another watchdog is already running"),
            "{ALREADY_WATCHING}"
        );
    }

    #[test]
    fn only_one_watchdog_claims_one_run() {
        let home = home();
        let log = home.path().join(".shprd").join("events.jsonl");
        let first = claim_run(&log)
            .expect("the first claim is taken")
            .expect("nothing else holds the run");
        assert!(
            claim_run(&log)
                .expect("the second claim is tried")
                .is_none(),
            "the second watchdog steps aside"
        );
        drop(first);
        // Another test in this process may be between `fork` and `exec` right
        // now: the forked child holds a duplicate of the lock file's open file
        // description, and `flock` locks belong to that description, so the
        // lock can outlive `drop(first)` for as long as the child takes to
        // exec. Production never forks mid-claim; only this immediate re-claim
        // is racy, so poll for a bounded time rather than assume the lock dies
        // the instant the file is closed.
        let mut waited_ms = 0;
        assert!(
            loop {
                if claim_run(&log)
                    .expect("a claim after the first is gone")
                    .is_some()
                {
                    break true;
                }
                if waited_ms >= 2_000 {
                    break false;
                }
                std::thread::sleep(Duration::from_millis(10));
                waited_ms += 10;
            },
            "the lock dies with the process that held it"
        );
    }

    #[test]
    fn one_runs_claim_is_not_anothers() {
        let home = home();
        let first = home.path().join("one/.shprd/events.jsonl");
        let second = home.path().join("two/.shprd/events.jsonl");
        let _claimed = claim_run(&first)
            .expect("the first run's claim is taken")
            .expect("nothing else holds it");
        assert!(
            claim_run(&second)
                .expect("the second run's claim is taken")
                .is_some(),
            "a run's watchdog does not keep another run's from starting"
        );
    }

    #[test]
    fn claude_record_slugifies_every_separator_and_dot() {
        let home = home();
        let dir = home
            .path()
            .join(".claude/projects/-repos--worktrees-shprd-m5-watch");
        let path = touch(&dir, "session.jsonl");
        assert_eq!(claude_record(home.path(), Path::new(WORKTREE)), Some(path));
    }

    #[test]
    fn pi_record_slugs_without_the_leading_slash() {
        let home = home();
        let dir = home
            .path()
            .join(".pi/agent/sessions/--repos-.worktrees-shprd-m5-watch--");
        let path = touch(&dir, "session.jsonl");
        assert_eq!(pi_record(home.path(), Path::new(WORKTREE)), Some(path));
    }

    #[test]
    fn both_builders_pick_the_newest_record() {
        let home = home();
        let claude_dir = home
            .path()
            .join(".claude/projects/-repos--worktrees-shprd-m5-watch");
        let pi_dir = home
            .path()
            .join(".pi/agent/sessions/--repos-.worktrees-shprd-m5-watch--");
        touch(&claude_dir, "old.jsonl");
        touch(&pi_dir, "old.jsonl");
        std::thread::sleep(Duration::from_millis(20));
        let claude_new = touch(&claude_dir, "new.jsonl");
        let pi_new = touch(&pi_dir, "new.jsonl");

        assert_eq!(
            claude_record(home.path(), Path::new(WORKTREE)),
            Some(claude_new)
        );
        assert_eq!(pi_record(home.path(), Path::new(WORKTREE)), Some(pi_new));
    }

    #[test]
    fn a_missing_record_directory_answers_none() {
        let home = home();
        assert_eq!(claude_record(home.path(), Path::new(WORKTREE)), None);
        assert_eq!(pi_record(home.path(), Path::new(WORKTREE)), None);
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

    /// A repository with one commit, the integration branch, and a home for
    /// briefs and worktrees.
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
                title: "Watch a task".to_string(),
                scope: vec!["crates/shprd".to_string()],
                acceptance: vec!["the watchdog acts".to_string()],
                tags: vec!["rust".to_string()],
                skills: Vec::new(),
                deps: Vec::new(),
            })
            .unwrap_or_else(|err| panic!("{err}"));
            ctx
        }

        /// The environment a watch pass runs in, with `workers` configured.
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

    /// Spawns the fake `sh` agent on `T1`.
    fn seed(mux: &FakeMux, env: &SpawnEnv<'_>) {
        let request = SpawnRequest {
            task: "T1".to_string(),
            agent: Some("sh".to_string()),
            effort: None,
        };
        spawn::spawn(mux, env, &request).unwrap_or_else(|err| panic!("{err}"));
    }

    /// The tab id `T1`'s first attempt recorded.
    fn tab_id(env: &SpawnEnv<'_>) -> String {
        tab_id_of(env, 1)
    }

    /// The tab id `T1`'s attempt `attempt` recorded.
    fn tab_id_of(env: &SpawnEnv<'_>, attempt: u32) -> String {
        let record = spawn::tab_path(&env.briefs_dir, &env.run, "T1", attempt);
        std::fs::read_to_string(record)
            .unwrap_or_default()
            .trim()
            .to_string()
    }

    /// The pane the tab `tab` holds.
    fn pane_of_tab(mux: &FakeMux, scratch: &Scratch, tab: &str) -> PaneId {
        let name = naming::workspace_name(&scratch.repo_name());
        let workspace = mux
            .find_workspace(&name)
            .unwrap_or_else(|err| panic!("{err}"))
            .unwrap_or_else(|| panic!("no workspace"));
        mux.list_tabs(&workspace.id)
            .unwrap_or_else(|err| panic!("{err}"))
            .into_iter()
            .find(|listed| listed.id.as_str() == tab)
            .unwrap_or_else(|| panic!("no tab {tab}"))
            .root_pane
    }

    /// True when `kind` is a hand-off.
    fn is_handoff(kind: &EventKind) -> bool {
        matches!(kind, EventKind::HandedOff { .. })
    }

    /// True when `kind` is an escalation.
    fn is_escalation(kind: &EventKind) -> bool {
        matches!(kind, EventKind::Escalated { .. })
    }

    /// How many events in `fresh` match `wanted`.
    fn count(fresh: &[Event], wanted: fn(&EventKind) -> bool) -> usize {
        fresh.iter().filter(|event| wanted(&event.kind)).count()
    }

    /// Closes the attempt's tab - the host's way of saying the worker's process
    /// is gone - and returns the events the next tick appended.
    fn one_exit(
        watcher: &mut Watcher,
        ctx: &Ctx,
        env: &SpawnEnv<'_>,
        mux: &FakeMux,
        attempt: u32,
        now: u64,
    ) -> Vec<Event> {
        let before = events(ctx).len();
        let tab = shprd_mux::TabId::new(tab_id_of(env, attempt));
        match mux.close_tab(&tab) {
            Ok(()) | Err(shprd_mux::MuxError::NotFound { .. }) => {}
            Err(err) => panic!("{err}"),
        }
        watcher
            .tick(mux, env, now)
            .unwrap_or_else(|err| panic!("{err}"));
        events(ctx)[before..].to_vec()
    }

    /// The events the run log holds now.
    fn events(ctx: &Ctx) -> Vec<Event> {
        ctx.load().unwrap_or_else(|err| panic!("{err}")).1
    }

    /// True when `fresh` holds an event matching `wanted`.
    fn appended(fresh: &[Event], wanted: fn(&EventKind) -> bool) -> bool {
        fresh.iter().any(|event| wanted(&event.kind))
    }

    /// Commits `T1` through a spawn and a report, then asks for a review: the
    /// state the watchdog's review pass is written for.
    fn awaiting_review_task(ctx: &Ctx) {
        ctx.commit(EventKind::Spawned {
            task: "T1".to_string(),
            attempt: 1,
            agent: "codex".to_string(),
            model: None,
            worktree: "/tmp/wt".to_string(),
            branch: "feature/x".to_string(),
        })
        .unwrap_or_else(|err| panic!("{err}"));
        ctx.commit(EventKind::Reported {
            task: "T1".to_string(),
            attempt: 1,
            status: ReportStatus::Completed,
            summary: "did it".to_string(),
            tests: Vec::new(),
            changed_paths: Vec::new(),
            commit: None,
            skills_used: Vec::new(),
        })
        .unwrap_or_else(|err| panic!("{err}"));
        ctx.commit(EventKind::ReviewRequested {
            task: "T1".to_string(),
            attempt: 1,
            reviewer: "codex".to_string(),
            degraded: false,
        })
        .unwrap_or_else(|err| panic!("{err}"));
    }

    /// Writes `T1`'s `findings.json`, optionally aged past the settle window.
    fn write_findings(env: &SpawnEnv<'_>, scratch: &Scratch, text: &str, age: Duration) -> PathBuf {
        let dir = scratch.home.path().join(".shprd").join("reviews");
        let path = review::findings_path(&dir, &env.run, "T1", 1);
        std::fs::create_dir_all(path.parent().unwrap_or(&dir))
            .unwrap_or_else(|err| panic!("{err}"));
        std::fs::write(&path, text).unwrap_or_else(|err| panic!("{err}"));
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap_or_else(|err| panic!("{err}"));
        file.set_modified(SystemTime::now() - age)
            .unwrap_or_else(|err| panic!("{err}"));
        path
    }

    /// The events one tick appended, starting from the log's current length.
    fn tick_events(
        watcher: &mut Watcher,
        ctx: &Ctx,
        env: &SpawnEnv<'_>,
        now: u64,
    ) -> (Vec<String>, Vec<Event>) {
        let before = events(ctx).len();
        let mux = FakeMux::default();
        let lines = watcher
            .tick(&mux, env, now)
            .unwrap_or_else(|err| panic!("{err}"));
        (lines, events(ctx)[before..].to_vec())
    }

    /// The M5 gate: a stuck agent is marked dry and handed off, in one tick.
    #[test]
    fn a_stalled_agent_goes_dry_and_is_handed_off_in_one_tick() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.env(&ctx, &["sh", "pi"]);
        let mux = mux_with_workspace(&scratch);
        seed(&mux, &env);
        let pane = pane_of_tab(&mux, &scratch, &tab_id(&env));
        // The same screen on every poll, and an agent the host still calls
        // working: only silence can explain the lack of movement.
        mux.set_screen(&pane, "step 1\nstep 2\n");
        mux.set_state(&pane, AgentState::Working);

        let mut watcher = Watcher::new(Policy::default());
        let t0 = 1_700_000_000_000u64;
        let mut dry_after_ms = None;
        for step in 0..=30u64 {
            let before = events(&ctx).len();
            watcher
                .tick(&mux, &env, t0 + 10_000 * step)
                .unwrap_or_else(|err| panic!("{err}"));
            let log = events(&ctx);
            let fresh = &log[before..];
            if let Some(EventKind::Dry { reset_at_ms, .. }) = fresh
                .iter()
                .map(|event| &event.kind)
                .find(|kind| matches!(kind, EventKind::Dry { .. }))
            {
                assert!(
                    reset_at_ms.is_some_and(|reset| reset > t0),
                    "a dry mark with no reset would be dry for ever"
                );
                dry_after_ms = Some(10_000 * step);
                eprintln!("DRY_AT {dry_after_ms:?}");
                assert!(
                    appended(fresh, |kind| matches!(kind, EventKind::HandedOff { .. })),
                    "the tick that marks the agent dry must hand the task off too"
                );
                break;
            }
        }
        assert_eq!(
            dry_after_ms,
            Some(300_000),
            "the agent must go dry 120s after it is merely suspect"
        );
    }

    /// A pane the host closed is a lost worker, not an unusable agent: the next
    /// tick hands the task to a fresh process of the same agent, and nothing is
    /// marked dry, so a one-agent roster is not dead-ended.
    #[test]
    fn a_closed_pane_is_handed_off_to_the_same_agent_and_dries_nothing() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.env(&ctx, &["sh", "pi"]);
        let mux = mux_with_workspace(&scratch);
        seed(&mux, &env);
        let tab = tab_id(&env);
        let pane = pane_of_tab(&mux, &scratch, &tab);
        mux.set_state(&pane, AgentState::Working);

        let mut watcher = Watcher::new(Policy::default());
        let t0 = 1_700_000_000_000u64;
        watcher
            .tick(&mux, &env, t0)
            .unwrap_or_else(|err| panic!("{err}"));
        mux.close_tab(&shprd_mux::TabId::new(tab))
            .unwrap_or_else(|err| panic!("{err}"));

        let before = events(&ctx).len();
        watcher
            .tick(&mux, &env, t0 + 10_000)
            .unwrap_or_else(|err| panic!("{err}"));
        let log = events(&ctx);
        let fresh = &log[before..];
        assert!(
            !appended(fresh, |kind| matches!(kind, EventKind::Dry { .. })),
            "a closed pane is a lost worker, not a dry program"
        );
        let handoffs: Vec<&EventKind> = fresh
            .iter()
            .map(|event| &event.kind)
            .filter(|kind| matches!(kind, EventKind::HandedOff { .. }))
            .collect();
        assert_eq!(handoffs.len(), 1, "a closed pane hands the task off once");
        match handoffs[0] {
            EventKind::HandedOff {
                from_agent,
                to_agent,
                reason,
                ..
            } => {
                assert_eq!(from_agent, to_agent, "a fresh process of the same agent");
                assert_eq!(reason.as_deref(), Some("agent process exited"));
            }
            other => panic!("not a hand-off: {other:?}"),
        }
    }

    /// A worker that dies every time is not replaced for ever: once the
    /// policy's exit budget is spent the watchdog asks a human, because a
    /// fourth identical attempt would only die the same way. The roster here
    /// holds one agent, which is the case a dry program used to dead-end.
    #[test]
    fn a_worker_that_keeps_dying_escalates_instead_of_looping() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.env(&ctx, &["sh"]);
        let mux = mux_with_workspace(&scratch);
        seed(&mux, &env);
        let policy = Policy::default();
        let t0 = 1_700_000_000_000u64;
        let mut watcher = Watcher::new(policy.clone());

        let mut handoffs = 0;
        let mut escalations = 0;
        for round in 0..=policy.exit_budget {
            // Each round's worker dies before it does anything.
            let tab = tab_id(&env);
            mux.close_tab(&shprd_mux::TabId::new(tab))
                .unwrap_or_else(|err| panic!("{err}"));
            let before = events(&ctx).len();
            watcher
                .tick(&mux, &env, t0 + 10_000 * round as u64)
                .unwrap_or_else(|err| panic!("{err}"));
            let log = events(&ctx);
            let fresh = &log[before..];
            handoffs += fresh
                .iter()
                .filter(|event| matches!(event.kind, EventKind::HandedOff { .. }))
                .count();
            escalations += fresh
                .iter()
                .filter(|event| matches!(event.kind, EventKind::Escalated { .. }))
                .count();
            assert!(
                !appended(fresh, |kind| matches!(kind, EventKind::Dry { .. })),
                "a lost worker never dries the agent"
            );
        }
        assert_eq!(
            handoffs, policy.exit_budget,
            "the budget's worth of exits is still replaced silently"
        );
        assert_eq!(escalations, 1, "the exit past the budget escalates");
    }

    /// The exit streak is the log's, not a counter in memory: a watchdog that
    /// restarts must not hand off an attempt whose budget is already spent.
    #[test]
    fn a_restarted_watcher_escalates_an_attempt_the_log_already_spent() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.env(&ctx, &["sh"]);
        let mux = mux_with_workspace(&scratch);
        seed(&mux, &env);
        let policy = Policy::default();
        let t0 = 1_700_000_000_000u64;

        for round in 0..policy.exit_budget {
            // Every round is a fresh watchdog: no memory but the log's.
            let mut watcher = Watcher::new(policy.clone());
            let fresh = one_exit(
                &mut watcher,
                &ctx,
                &env,
                &mux,
                1,
                t0 + 10_000 * round as u64,
            );
            assert_eq!(
                count(&fresh, is_handoff),
                1,
                "exit {} is still replaced silently",
                round + 1
            );
            assert_eq!(count(&fresh, is_escalation), 0);
        }

        let mut watcher = Watcher::new(policy.clone());
        let fresh = one_exit(&mut watcher, &ctx, &env, &mux, 1, t0 + 100_000);
        assert_eq!(
            count(&fresh, is_handoff),
            0,
            "the budget the log already spent is not handed off again"
        );
        assert_eq!(
            count(&fresh, is_escalation),
            1,
            "the exit past the budget escalates, restart or not"
        );
    }

    /// A restarted watchdog reads the escalation its predecessor recorded and
    /// leaves the attempt alone: one escalation per attempt, not one per run.
    #[test]
    fn a_restarted_watcher_does_nothing_for_an_attempt_the_log_escalated() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.env(&ctx, &["sh"]);
        let mux = mux_with_workspace(&scratch);
        seed(&mux, &env);
        let policy = Policy::default();
        let t0 = 1_700_000_000_000u64;

        let mut spent = 0;
        for round in 0..=policy.exit_budget {
            let mut watcher = Watcher::new(policy.clone());
            let fresh = one_exit(
                &mut watcher,
                &ctx,
                &env,
                &mux,
                1,
                t0 + 10_000 * round as u64,
            );
            spent += count(&fresh, is_escalation);
        }
        assert_eq!(spent, 1, "the attempt escalates once");

        for step in 0..2 {
            let mut watcher = Watcher::new(policy.clone());
            let fresh = one_exit(
                &mut watcher,
                &ctx,
                &env,
                &mux,
                1,
                t0 + 100_000 + 10_000 * step,
            );
            assert!(
                fresh.is_empty(),
                "a restarted watchdog retried an escalated attempt: {fresh:?}"
            );
        }
    }

    /// A hand-off that fails is one escalation, not one a tick: the watch parks
    /// and only a change in the log sets it going again.
    #[test]
    fn a_failed_hand_off_escalates_once_and_parks() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.env(&ctx, &["sh"]);
        let mux = mux_with_workspace(&scratch);
        seed(&mux, &env);
        // The one worker in the roster is dry, so no hand-off can be made.
        ctx.commit(EventKind::Dry {
            agent: "sh".to_string(),
            reason: "quota".to_string(),
            reset_at_ms: None,
        })
        .unwrap_or_else(|err| panic!("{err}"));
        let policy = Policy::default();
        let t0 = 1_700_000_000_000u64;

        let mut watcher = Watcher::new(policy.clone());
        let first = one_exit(&mut watcher, &ctx, &env, &mux, 1, t0);
        assert_eq!(count(&first, is_handoff), 0, "no worker could take over");
        let ours = first
            .iter()
            .filter(|event| {
                matches!(&event.kind, EventKind::Escalated { body, .. }
                    if ParkReason::of_body(body) == Some(ParkReason::HandoffFailed))
            })
            .count();
        assert_eq!(ours, 1, "the failed hand-off records one escalation");

        for step in 1..4u64 {
            let mut restarted = Watcher::new(policy.clone());
            let fresh = one_exit(&mut restarted, &ctx, &env, &mux, 1, t0 + 10_000 * step);
            assert!(
                fresh.is_empty(),
                "a parked watch retried the hand-off: {fresh:?}"
            );
        }

        // A human re-admits the agent and retries the hand-off by hand: that is
        // the change the parking waits for.
        ctx.commit(EventKind::Available {
            agent: "sh".to_string(),
            reason: "quota reset".to_string(),
        })
        .unwrap_or_else(|err| panic!("{err}"));
        ctx.commit(EventKind::HandedOff {
            task: "T1".to_string(),
            from_agent: "sh".to_string(),
            to_agent: "sh".to_string(),
            checkpoint: "c0".to_string(),
            reason: Some("a human retried the hand-off".to_string()),
        })
        .unwrap_or_else(|err| panic!("{err}"));
        let mut restarted = Watcher::new(policy.clone());
        let fresh = one_exit(&mut restarted, &ctx, &env, &mux, 1, t0 + 100_000);
        assert_eq!(
            count(&fresh, is_handoff),
            1,
            "a hand-off since the escalation un-parks the watch"
        );
        assert_eq!(count(&fresh, is_escalation), 0);
    }

    /// A hand-off that moves the task to another worker moves the watch with
    /// it: the next tick reads the new pane, and the new worker's exit is what
    /// is handed off, not the old worker's again.
    #[test]
    fn a_dry_hand_off_moves_the_watch_to_the_new_agent() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.env(&ctx, &["sh", "pi"]);
        let mux = mux_with_workspace(&scratch);
        seed(&mux, &env);
        let pane = pane_of_tab(&mux, &scratch, &tab_id(&env));
        mux.set_screen(&pane, "step 1\nstep 2\n");
        mux.set_state(&pane, AgentState::Working);
        let policy = Policy::default();
        let t0 = 1_700_000_000_000u64;
        let mut watcher = Watcher::new(policy.clone());

        // Silence takes the idle `sh` out of play and hands the task to `pi`.
        let mut handed_to = None;
        for step in 0..=30u64 {
            let before = events(&ctx).len();
            watcher
                .tick(&mux, &env, t0 + 10_000 * step)
                .unwrap_or_else(|err| panic!("{err}"));
            let log = events(&ctx);
            if let Some(EventKind::HandedOff { to_agent, .. }) = log[before..]
                .iter()
                .map(|event| &event.kind)
                .find(|kind| matches!(kind, EventKind::HandedOff { .. }))
            {
                handed_to = Some(to_agent.clone());
                break;
            }
        }
        assert_eq!(
            handed_to.as_deref(),
            Some("pi"),
            "the dry worker is replaced"
        );

        // The new worker's pane goes away: the watch acts on it, in the new
        // agent's name, and the streak is what the log counts for the attempt.
        let fresh = one_exit(&mut watcher, &ctx, &env, &mux, 1, t0 + 400_000);
        assert_eq!(count(&fresh, is_handoff), 1, "{fresh:?}");
        assert_eq!(count(&fresh, is_escalation), 0);
        match fresh
            .iter()
            .map(|event| &event.kind)
            .find(|kind| matches!(kind, EventKind::HandedOff { .. }))
        {
            Some(EventKind::HandedOff {
                from_agent,
                to_agent,
                reason,
                ..
            }) => {
                assert_eq!(from_agent, "pi", "the pane that went away is pi's");
                assert_eq!(to_agent, "pi", "a fresh process of the same agent");
                assert_eq!(reason.as_deref(), Some(EXIT_REASON));
            }
            other => panic!("not a hand-off: {other:?}"),
        }
        assert!(
            watcher
                .watches
                .contains_key(&(String::from("T1"), 1, String::from("pi"))),
            "the watch stays on the attempt's current worker"
        );
    }

    /// The parking is the log's, so one watchdog that keeps running has to be
    /// un-parked by a human's retry: a restart used to be the only thing that
    /// cleared the escalation, and a running watch stayed parked for good.
    #[test]
    fn a_human_hand_off_unparks_a_running_watch() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.env(&ctx, &["sh"]);
        let mux = mux_with_workspace(&scratch);
        seed(&mux, &env);
        // The roster's one worker is dry, so the first hand-off cannot be made.
        ctx.commit(EventKind::Dry {
            agent: "sh".to_string(),
            reason: "quota".to_string(),
            reset_at_ms: None,
        })
        .unwrap_or_else(|err| panic!("{err}"));
        let policy = Policy::default();
        let t0 = 1_700_000_000_000u64;

        // One watcher for the whole test: no restart may be what sets it going.
        let mut watcher = Watcher::new(policy.clone());
        let first = one_exit(&mut watcher, &ctx, &env, &mux, 1, t0);
        assert_eq!(count(&first, is_handoff), 0, "no worker could take over");
        let parked_escalations = |fresh: &[Event]| {
            fresh
                .iter()
                .filter(|event| {
                    matches!(&event.kind, EventKind::Escalated { body, .. }
                        if ParkReason::of_body(body) == Some(ParkReason::HandoffFailed))
                })
                .count()
        };
        assert_eq!(parked_escalations(&first), 1, "the failure escalates once");

        for step in 1..3u64 {
            let fresh = one_exit(&mut watcher, &ctx, &env, &mux, 1, t0 + 10_000 * step);
            assert!(
                fresh.is_empty(),
                "a parked watch retried the hand-off: {fresh:?}"
            );
        }

        // A human re-admits the agent and carries the hand-off out by hand.
        ctx.commit(EventKind::Available {
            agent: "sh".to_string(),
            reason: "quota reset".to_string(),
        })
        .unwrap_or_else(|err| panic!("{err}"));
        ctx.commit(EventKind::HandedOff {
            task: "T1".to_string(),
            from_agent: "sh".to_string(),
            to_agent: "sh".to_string(),
            checkpoint: "c0".to_string(),
            reason: Some("a human retried the hand-off".to_string()),
        })
        .unwrap_or_else(|err| panic!("{err}"));

        // The new worker's grace is the log's, not the parking's: a pane that
        // has not answered yet is not the newcomer's exit.
        let handoff_at = events(&ctx)
            .last()
            .map(|event| event.at_ms)
            .unwrap_or_else(|| panic!("the log lost the human's hand-off"));
        let before = events(&ctx).len();
        watcher
            .tick(&mux, &env, handoff_at + 1_000)
            .unwrap_or_else(|err| panic!("{err}"));
        let early = events(&ctx)[before..].to_vec();
        assert!(
            early.is_empty(),
            "a hand-off's grace is measured from the log: {early:?}"
        );

        let fresh = one_exit(&mut watcher, &ctx, &env, &mux, 1, t0 + 100_000);
        assert_eq!(
            count(&fresh, is_handoff),
            1,
            "the running watch monitors the new worker again"
        );
        assert_eq!(parked_escalations(&fresh), 0, "the parking was lifted");
    }

    /// A spent exit budget parks the watch, and a human re-spawning the
    /// attempt is what sets it going again while that watchdog keeps running.
    #[test]
    fn a_new_spawn_unparks_a_running_watch() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.env(&ctx, &["sh"]);
        let mux = mux_with_workspace(&scratch);
        seed(&mux, &env);
        let policy = Policy::default();
        let t0 = 1_700_000_000_000u64;
        let mut watcher = Watcher::new(policy.clone());

        let mut escalations = 0;
        for round in 0..=policy.exit_budget {
            let fresh = one_exit(
                &mut watcher,
                &ctx,
                &env,
                &mux,
                1,
                t0 + 10_000 * round as u64,
            );
            escalations += count(&fresh, is_escalation);
        }
        assert_eq!(escalations, 1, "the exit past the budget escalates");

        let parked = one_exit(&mut watcher, &ctx, &env, &mux, 1, t0 + 100_000);
        assert!(
            parked.is_empty(),
            "a parked watch kept deciding: {parked:?}"
        );

        // A human gives up on the attempt and spawns the next one: the log now
        // holds a worker newer than the escalation, so the watchdog watches
        // again instead of staying parked.
        ctx.commit(EventKind::Reported {
            task: "T1".to_string(),
            attempt: 1,
            status: ReportStatus::Failed,
            summary: "died four times".to_string(),
            tests: Vec::new(),
            changed_paths: Vec::new(),
            commit: None,
            skills_used: Vec::new(),
        })
        .unwrap_or_else(|err| panic!("{err}"));
        // The human also clears the dead attempt's worktree and branch.
        let (state, _) = ctx.load().unwrap_or_else(|err| panic!("{err}"));
        let dead = &state.tasks["T1"].attempts[0];
        std::fs::remove_dir_all(&dead.worktree).unwrap_or_else(|err| panic!("{err}"));
        git_out(scratch.root(), &["worktree", "prune"]);
        git_out(scratch.root(), &["branch", "-D", &dead.branch]);
        for (kind, name) in [
            (ResourceKind::Worktree, dead.worktree.clone()),
            (ResourceKind::Branch, dead.branch.clone()),
        ] {
            ctx.commit(EventKind::Resource {
                action: ResourceAction::Removed,
                kind,
                name,
                task: Some("T1".to_string()),
            })
            .unwrap_or_else(|err| panic!("{err}"));
        }
        let request = SpawnRequest {
            task: "T1".to_string(),
            agent: Some("sh".to_string()),
            effort: None,
        };
        spawn::spawn(&mux, &env, &request).unwrap_or_else(|err| panic!("{err}"));

        let fresh = one_exit(&mut watcher, &ctx, &env, &mux, 2, t0 + 110_000);
        assert_eq!(
            count(&fresh, is_handoff),
            1,
            "the new worker's exit is handed off, not parked: {fresh:?}"
        );
    }

    /// One rule decides whether a watch is parked: the newest event among a
    /// task's spawns, hand-offs and parking escalations says so, and the kinds
    /// of event that say nothing about a parking are not consulted at all.
    #[test]
    fn the_rule_reads_the_newest_parking_event_of_the_task() {
        let dealt = |seq: u64, kind: EventKind| Event {
            seq,
            at_ms: 1_700_000_000_000 + seq,
            actor: Actor::Engine,
            kind,
        };
        let spawned = |seq: u64| {
            dealt(
                seq,
                EventKind::Spawned {
                    task: "T1".to_string(),
                    attempt: 1,
                    agent: "sh".to_string(),
                    model: None,
                    worktree: "/wt/T1".to_string(),
                    branch: "feature/t1".to_string(),
                },
            )
        };
        let handed_off = |seq: u64, reason: &str| {
            dealt(
                seq,
                EventKind::HandedOff {
                    task: "T1".to_string(),
                    from_agent: "sh".to_string(),
                    to_agent: "pi".to_string(),
                    checkpoint: "c0".to_string(),
                    reason: Some(reason.to_string()),
                },
            )
        };
        let parked_by = |seq: u64, why: ParkReason| {
            dealt(
                seq,
                EventKind::Escalated {
                    task: Some("T1".to_string()),
                    body: format!("sh {}: something went wrong", why.marker()),
                    default_action: "pick the task up by hand".to_string(),
                },
            )
        };
        let other_escalation = |seq: u64| {
            dealt(
                seq,
                EventKind::Escalated {
                    task: Some("T1".to_string()),
                    body: "sh agent is dry: quota".to_string(),
                    default_action: "pick the task up by hand".to_string(),
                },
            )
        };
        let other_task = dealt(
            90,
            EventKind::Escalated {
                task: Some("T2".to_string()),
                body: format!("sh {}: not T1's business", ParkReason::ExitBudget.marker()),
                default_action: "pick the task up by hand".to_string(),
            },
        );

        // Nothing has happened: nothing parks.
        assert_eq!(parked(&[], "T1"), None);

        // A spawn, an exit hand-off, then the exit budget's escalation.
        let events = vec![
            spawned(1),
            handed_off(2, EXIT_REASON),
            parked_by(3, ParkReason::ExitBudget),
        ];
        assert_eq!(
            parked(&events, "T1"),
            Some((1_700_000_000_003, ParkReason::ExitBudget)),
            "the exit budget's escalation parks the watch"
        );

        // An escalation about a dry agent is not a parking and does not touch
        // the rule: parked stays parked, whatever else the log records.
        let mut events = events;
        events.push(other_escalation(4));
        assert_eq!(
            parked(&events, "T1"),
            Some((1_700_000_000_003, ParkReason::ExitBudget)),
            "only a later spawn or hand-off lifts a parking"
        );

        // A human handing the task to a fresh worker lifts it: the hand-off is
        // newer than the escalation, whatever the attempt was doing before.
        events.push(handed_off(5, "a human did it by hand"));
        assert_eq!(parked(&events, "T1"), None, "a hand-off lifts the parking");

        // A failed hand-off parks in its own right, and a new attempt's spawn
        // is what lifts it.
        events.push(parked_by(6, ParkReason::HandoffFailed));
        assert_eq!(
            parked(&events, "T1"),
            Some((1_700_000_000_006, ParkReason::HandoffFailed))
        );
        events.push(spawned(7));
        assert_eq!(parked(&events, "T1"), None, "a new attempt is watched");

        // Another task's parking is not this task's.
        events.push(other_task);
        assert_eq!(parked(&events, "T1"), None);
    }

    /// The streak and the escalation belong to their attempt, not their task:
    /// the next attempt starts from zero and is watched again, which is what
    /// un-parks a watch.
    #[test]
    fn a_new_attempt_starts_with_no_streak_and_no_escalation() {
        let dealt = |seq: u64, kind: EventKind| Event {
            seq,
            at_ms: 1_700_000_000_000 + seq,
            actor: Actor::Human,
            kind,
        };
        let spawned = |seq: u64, attempt: u32| {
            dealt(
                seq,
                EventKind::Spawned {
                    task: "T1".to_string(),
                    attempt,
                    agent: "sh".to_string(),
                    model: None,
                    worktree: "/wt/T1".to_string(),
                    branch: "feature/t1".to_string(),
                },
            )
        };
        let exited = |seq: u64| {
            dealt(
                seq,
                EventKind::HandedOff {
                    task: "T1".to_string(),
                    from_agent: "sh".to_string(),
                    to_agent: "sh".to_string(),
                    checkpoint: "c0".to_string(),
                    reason: Some(EXIT_REASON.to_string()),
                },
            )
        };
        let mut events = vec![
            spawned(1, 1),
            exited(2),
            exited(3),
            exited(4),
            dealt(
                5,
                EventKind::Escalated {
                    task: Some("T1".to_string()),
                    body: format!(
                        "sh {}: {EXIT_REASON} 4 times in a row on this attempt: pick up by hand",
                        ParkReason::ExitBudget.marker()
                    ),
                    default_action: "pick the task up by hand".to_string(),
                },
            ),
        ];
        assert_eq!(exit_streak(&events, "T1"), 3);
        assert_eq!(
            parked(&events, "T1"),
            Some((events[4].at_ms, ParkReason::ExitBudget))
        );

        events.push(spawned(6, 2));
        assert_eq!(
            exit_streak(&events, "T1"),
            0,
            "a new attempt starts with a fresh budget"
        );
        assert!(
            parked(&events, "T1").is_none(),
            "a new attempt is a human's no longer"
        );
    }

    /// A hand-off that is not an exit hands the task to a new worker, and a new
    /// worker starts with a fresh budget: a human's hand-off must not leave the
    /// newcomer one exit away from an escalation it never earned. The count is
    /// the log's, so this reads the same before and after a restart.
    #[test]
    fn a_human_hand_off_starts_a_fresh_exit_streak() {
        let dealt = |seq: u64, kind: EventKind| Event {
            seq,
            at_ms: 1_700_000_000_000 + seq,
            actor: Actor::Human,
            kind,
        };
        let spawned = |seq: u64, attempt: u32| {
            dealt(
                seq,
                EventKind::Spawned {
                    task: "T1".to_string(),
                    attempt,
                    agent: "sh".to_string(),
                    model: None,
                    worktree: "/wt/T1".to_string(),
                    branch: "feature/t1".to_string(),
                },
            )
        };
        let handed_off = |seq: u64, reason: &str| {
            dealt(
                seq,
                EventKind::HandedOff {
                    task: "T1".to_string(),
                    from_agent: "sh".to_string(),
                    to_agent: "sh".to_string(),
                    checkpoint: "c0".to_string(),
                    reason: Some(reason.to_string()),
                },
            )
        };
        let budget = 3;

        let mut events = vec![spawned(1, 1)];
        assert_eq!(exit_streak(&events, "T1"), 0, "a fresh spawn has no exits");
        for (n, seq) in (1..=budget as u64).zip(2..) {
            events.push(handed_off(seq, EXIT_REASON));
            assert_eq!(
                exit_streak(&events, "T1"),
                n as usize,
                "each exit is the next one of the streak"
            );
        }

        // A human hands the same attempt to another worker: a full budget again.
        events.push(handed_off(2 + budget as u64, "a human handed it on"));
        assert_eq!(
            exit_streak(&events, "T1"),
            0,
            "a human's hand-off starts a fresh budget"
        );
        events.push(handed_off(3 + budget as u64, EXIT_REASON));
        assert_eq!(
            exit_streak(&events, "T1"),
            1,
            "the new worker's exit is the first of its streak"
        );
        assert!(
            exit_streak(&events, "T1") <= budget,
            "the new worker still has its budget"
        );
    }

    /// A host that cannot list its tabs says nothing about the agent: no tick
    /// may read that as an exit and hand the task to somebody else.
    #[test]
    fn a_transient_mux_error_is_not_a_dead_agent() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.env(&ctx, &["sh", "pi"]);
        let mux = mux_with_workspace(&scratch);
        seed(&mux, &env);
        let pane = pane_of_tab(&mux, &scratch, &tab_id(&env));
        mux.set_state(&pane, AgentState::Working);
        let flaky = FlakyMux::new(mux, Fault::ListTabs);

        let mut watcher = Watcher::new(Policy::default());
        let before = events(&ctx).len();
        let t0 = 1_700_000_000_000u64;
        for step in 0..5u64 {
            watcher
                .tick(&flaky, &env, t0 + 10_000 * step)
                .unwrap_or_else(|err| panic!("{err}"));
        }

        let log = events(&ctx);
        let fresh = &log[before..];
        assert!(
            !appended(fresh, |kind| matches!(kind, EventKind::Dry { .. })),
            "a transient mux error marked the agent dry"
        );
        assert!(
            !appended(fresh, |kind| matches!(kind, EventKind::HandedOff { .. })),
            "a transient mux error handed the task off"
        );
        assert!(
            fresh.is_empty(),
            "the failing ticks appended {} events",
            fresh.len()
        );
    }

    /// A hand-off ends the attempt, not the outgoing agent's cooldown: once its
    /// retry time passes, the roster must see it as available again.
    #[test]
    fn a_handed_off_agent_is_available_again_after_its_retry_time() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.env(&ctx, &["sh", "pi"]);
        let mux = mux_with_workspace(&scratch);
        seed(&mux, &env);
        let pane = pane_of_tab(&mux, &scratch, &tab_id(&env));
        mux.set_screen(&pane, "step 1\nstep 2\n");
        mux.set_state(&pane, AgentState::Working);

        let mut watcher = Watcher::new(Policy::default());
        let t0 = 1_700_000_000_000u64;
        let mut dry_at = None;
        for step in 0..=30u64 {
            let now_ms = t0 + 10_000 * step;
            watcher
                .tick(&mux, &env, now_ms)
                .unwrap_or_else(|err| panic!("{err}"));
            let dry = events(&ctx)
                .iter()
                .any(|event| matches!(&event.kind, EventKind::Dry { agent, .. } if agent == "sh"));
            if dry {
                dry_at = Some(now_ms);
                break;
            }
        }
        let dry_at = dry_at.unwrap_or_else(|| panic!("the agent never went dry"));

        // The first dry backoff is 900s; a tick past it is the probe.
        let before = events(&ctx).len();
        watcher
            .tick(&mux, &env, dry_at + 910_000)
            .unwrap_or_else(|err| panic!("{err}"));
        let log = events(&ctx);
        let fresh = &log[before..];
        assert!(
            appended(fresh, |kind| matches!(
                kind,
                EventKind::Available { agent, .. } if agent == "sh"
            )),
            "the handed-off agent stayed dry: {} events",
            fresh.len()
        );
    }
    /// The reviewer writes findings and stops: the tick that sees them records
    /// the pass, and a file still being written is left alone.
    #[test]
    fn a_finished_review_is_recorded_by_the_watchdog() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.env(&ctx, &["codex"]);
        awaiting_review_task(&ctx);
        let mut watcher = Watcher::new(Policy::default());
        write_findings(&env, &scratch, "[]", Duration::from_millis(50));

        let (lines, fresh) = tick_events(&mut watcher, &ctx, &env, 1_700_000_000_000);
        assert!(
            !appended(&fresh, |kind| matches!(kind, EventKind::GatePassed { .. })),
            "a findings file a moment old was read: {lines:?}"
        );

        write_findings(&env, &scratch, "[]", Duration::from_secs(10));
        let (lines, fresh) = tick_events(&mut watcher, &ctx, &env, 1_700_000_010_000);
        assert!(
            appended(&fresh, |kind| matches!(
                kind,
                EventKind::GatePassed { gate, attempt, .. }
                    if gate == REVIEW_GATE && *attempt == 1
            )),
            "no review gate result was recorded: {lines:?}"
        );
        assert!(
            lines.iter().any(|line| line == "\u{25c7} T1 review passed"),
            "{lines:?}"
        );
        // The watch records the verdict as the engine, whoever started it: the
        // context here is a human's, and only the engine may record a review.
        let verdict = fresh
            .iter()
            .find(|event| {
                matches!(&event.kind, EventKind::GatePassed { gate, .. } if gate == REVIEW_GATE)
            })
            .unwrap_or_else(|| panic!("no verdict to inspect: {lines:?}"));
        assert_eq!(verdict.actor, Actor::Engine, "the verdict's actor");
        assert!(
            matches!(
                &verdict.kind,
                EventKind::GatePassed { source: Some(source), .. }
                    if !source.reviewer.is_empty() && !source.findings_sha256.is_empty()
            ),
            "the verdict carried no provenance: {:?}",
            verdict.kind
        );
    }

    /// A blocking finding fails the gate, and the line says so.
    #[test]
    fn a_failing_review_is_recorded_as_a_failed_gate() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.env(&ctx, &["codex"]);
        awaiting_review_task(&ctx);
        let findings = r#"[{"severity":"critical","title":"wrong","location":null,
            "detail":"it does not do what the contract asks"}]"#;
        write_findings(&env, &scratch, findings, Duration::from_secs(10));

        let mut watcher = Watcher::new(Policy::default());
        let (lines, fresh) = tick_events(&mut watcher, &ctx, &env, 1_700_000_000_000);
        assert!(
            appended(&fresh, |kind| matches!(
                kind,
                EventKind::GateFailed { gate, .. } if gate == REVIEW_GATE
            )),
            "no failed review gate was recorded: {lines:?}"
        );
        assert!(
            lines.iter().any(|line| line == "\u{25c7} T1 review failed"),
            "{lines:?}"
        );
    }

    /// An unreadable file is one escalation, not one per tick.
    #[test]
    fn an_invalid_findings_file_is_escalated_once() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.env(&ctx, &["codex"]);
        awaiting_review_task(&ctx);
        write_findings(&env, &scratch, "not json", Duration::from_secs(10));

        let mut watcher = Watcher::new(Policy::default());
        let (first, fresh) = tick_events(&mut watcher, &ctx, &env, 1_700_000_000_000);
        assert!(
            appended(&fresh, |kind| matches!(
                kind,
                EventKind::Escalated { task, body, .. }
                    if task.as_deref() == Some("T1")
                        && body.contains("invalid findings.json for T1")
            )),
            "an invalid file was not escalated: {first:?}"
        );
        let (_, fresh) = tick_events(&mut watcher, &ctx, &env, 1_700_000_010_000);
        assert!(
            !appended(&fresh, |kind| matches!(kind, EventKind::Escalated { .. })),
            "the same invalid file was escalated twice"
        );
        assert!(
            !appended(&fresh, |kind| matches!(
                kind,
                EventKind::GatePassed { .. } | EventKind::GateFailed { .. }
            )),
            "an invalid file decided a review"
        );
    }
}
