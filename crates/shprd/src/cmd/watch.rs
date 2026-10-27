//! The `watch` verb: poll every working agent and apply the dry/hand-off state
//! machine to what its pane and its transcript say.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use shprd_core::event::{Event, EventKind};
use shprd_core::log::{Clock, SystemClock};
use shprd_core::state::{RunState, Task, TaskStatus};
use shprd_mux::{Mux, MuxError, PaneId, naming};
use shprd_watch::machine::{Action, Health, Inputs, Policy, Watch};
use shprd_watch::records::RecordError;
use shprd_watch::records::claude::scan_claude;
use shprd_watch::records::pi::scan_pi;
use shprd_watch::records::sqlite::{scan_kilo, scan_opencode};
use shprd_watch::screen::{Sampler, ScreenSignal, sample};

use crate::cli::WatchArgs;
use crate::cmd::handoff::{HandoffRequest, handoff};
use crate::cmd::spawn::{self, Program, SpawnEnv};
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
    let (watch, sampler) = watcher
        .watches
        .entry(key.clone())
        .or_insert_with(|| (Watch::new(), Sampler::new()));
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

/// The `watch` verb: poll every working agent once per interval.
///
/// The loop only ends when the caller stops it: an unattended run wants the
/// watchdog up for as long as the run lasts, and `--once` is how a caller asks
/// for a single pass.
pub fn run_verb(ctx: &Ctx, args: &WatchArgs, home: Option<&Path>) -> Result<(), CliError> {
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
    use shprd_core::event::Actor;
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
        let record = spawn::tab_path(&env.briefs_dir, &env.run, "T1", 1);
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

    /// The events the run log holds now.
    fn events(ctx: &Ctx) -> Vec<Event> {
        ctx.load().unwrap_or_else(|err| panic!("{err}")).1
    }

    /// True when `fresh` holds an event matching `wanted`.
    fn appended(fresh: &[Event], wanted: fn(&EventKind) -> bool) -> bool {
        fresh.iter().any(|event| wanted(&event.kind))
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
            if appended(fresh, |kind| matches!(kind, EventKind::Dry { .. })) {
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

    /// A pane the host closed is a dead agent: the next tick acts on it.
    #[test]
    fn a_closed_pane_goes_dry_and_is_handed_off_on_the_next_tick() {
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
            appended(fresh, |kind| matches!(kind, EventKind::Dry { .. })),
            "a closed pane must mark the agent dry"
        );
        assert!(
            appended(fresh, |kind| matches!(kind, EventKind::HandedOff { .. })),
            "a closed pane must hand the task off"
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
}
