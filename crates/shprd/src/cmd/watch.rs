//! The `watch` verb: poll every working agent and apply the dry/hand-off state
//! machine to what its pane and its transcript say.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use shprd_core::event::{Event, EventKind};
use shprd_core::state::{RunState, Task, TaskStatus};
use shprd_mux::{Mux, MuxError, PaneId, naming};
use shprd_watch::machine::{Action, Health, Inputs, Policy, Watch};
use shprd_watch::records::RecordError;
use shprd_watch::records::claude::scan_claude;
use shprd_watch::records::pi::scan_pi;
use shprd_watch::records::sqlite::{scan_kilo, scan_opencode};
use shprd_watch::screen::{Sampler, ScreenSignal, sample};

use crate::cmd::handoff::{HandoffRequest, handoff};
use crate::cmd::spawn::{self, Program, SpawnEnv};
use crate::ctx::CliError;

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
    /// The thresholds and backoffs the state machine runs under.
    policy: Policy,
}

impl Watcher {
    /// A watcher with no memory yet, running under `policy`.
    pub fn new(policy: Policy) -> Self {
        Self {
            watches: BTreeMap::new(),
            policy,
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
        for task in state.tasks.values() {
            if task.status != TaskStatus::Working {
                continue;
            }
            tick_task(self, mux, env, &state, &events, task, now_ms, &mut lines)?;
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
}

/// One pass over one working task: read its pane and record, then act.
fn tick_task(
    watcher: &mut Watcher,
    mux: &dyn Mux,
    env: &SpawnEnv<'_>,
    state: &RunState,
    events: &[Event],
    task: &Task,
    now_ms: u64,
    lines: &mut Vec<String>,
) -> Result<(), CliError> {
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

    let key = (task.id.clone(), attempt.n, agent.clone());
    let (watch, sampler) = watcher
        .watches
        .entry(key)
        .or_insert_with(|| (Watch::new(), Sampler::new()));
    let pane = pane_for(mux, env, &tab);
    let (screen, process_exited) = observe(
        mux,
        pane.as_ref(),
        &program.spec.dry_patterns,
        sampler,
        now_ms,
    );
    let since_ms = spawned_at_ms(events, &task.id, attempt.n);
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
            Action::HandOff => {
                let request = HandoffRequest {
                    task: task.id.clone(),
                    reason: dry_reason(watch),
                    to: None,
                };
                match handoff(mux, env, &request) {
                    Ok(report) => lines.push(format!("handed off {} to {}", task.id, report.to)),
                    Err(err) => escalate(env, &task.id, &err, lines),
                }
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
    Ok(())
}

/// Records that a hand-off failed, without stopping the pass.
fn escalate(env: &SpawnEnv<'_>, task: &str, err: &CliError, lines: &mut Vec<String>) {
    let kind = EventKind::Escalated {
        task: Some(task.to_string()),
        body: format!("hand-off of {task} failed: {err}"),
        default_action: String::from("stop the run"),
    };
    match env.ctx.commit(kind) {
        Ok(_) => lines.push(format!("escalated {task}: {err}")),
        Err(err) => lines.push(format!("cannot escalate {task}: {err}")),
    }
}

/// Why the watchdog calls this attempt dry, for the hand-off brief.
fn dry_reason(watch: &Watch) -> String {
    match &watch.health {
        Health::Dry { reason, .. } => reason.clone(),
        _ => String::from("the agent stopped working"),
    }
}

/// The tab id an attempt recorded when it spawned, if the record is there.
fn recorded_tab(env: &SpawnEnv<'_>, task: &str, attempt: u32) -> Option<String> {
    let path = spawn::tab_path(&env.briefs_dir, &env.run, task, attempt);
    Some(std::fs::read_to_string(path).ok()?.trim().to_string())
}

/// The home directory the environment's briefs sit under.
fn home_of<'a>(env: &'a SpawnEnv<'_>) -> Option<&'a Path> {
    env.briefs_dir.parent()?.parent()
}

/// The pane `tab` holds, or `None` when the host no longer has it.
fn pane_for(mux: &dyn Mux, env: &SpawnEnv<'_>, tab: &str) -> Option<PaneId> {
    let name = naming::workspace_name(&env.repo_name);
    let workspace = mux.find_workspace(&name).ok()??;
    let tabs = mux.list_tabs(&workspace.id).ok()?;
    tabs.iter()
        .find(|listed| listed.id.as_str() == tab)
        .map(|listed| listed.root_pane.clone())
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use tempfile::TempDir;

    use super::{claude_record, pi_record};

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
}
