//! `shprd agent spawn` and `shprd agent prompt`: put an agent on a task.
//!
//! Spawning is the one verb that has effects outside the log, so all of it lives
//! in [`spawn`], which takes the [`Mux`] as a parameter: the binary hands it a
//! real adapter, and the tests hand it a [`shprd_mux::fake::FakeMux`]. Nothing in
//! this module reads the environment or starts a process of its own, with one
//! exception: when an effort level has to be checked against a model, the chosen
//! program is asked for its catalog through [`shprd_agents::adapter::catalog`],
//! and a test injects that catalog into [`SpawnEnv::catalogs`] instead.
//!
//! A spawn is one attempt at one task: it picks an agent, selects the skills
//! that attempt must load, writes the brief, creates the worktree, opens the
//! agent's tab, starts the CLI there and only then records the attempt. Every
//! step after the worktree undoes the earlier ones when it fails, so a refused
//! or broken spawn leaves neither a worktree nor an event behind.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::anyhow;
use shprd_agents::asks::{AskHookInstaller, AskHookPaths, HarnessAskHook};
use shprd_agents::data::DataDir;
use shprd_agents::env::{EnvContext, LaunchEnv, Role as EnvRole, launch_env};
use shprd_agents::models::{AgentRef, Catalog};
use shprd_agents::skills::{CompatEntry, Role as SkillRole, Verdict};
use shprd_agents::spec::{BlockingPrompt, HarnessSpec, LaunchSpec, Values, render_args};
use shprd_core::config::Skills;
use shprd_core::event::{Actor, EventKind, ResourceAction, ResourceKind};
use shprd_core::prompts::{self, PromptRef};
use shprd_core::settings::{
    Key, Layer, Loaded, Resolved, ResolvedRole, RoleField, Source, Sourced, TaskKey, Value,
};
use shprd_core::state::{ResourceStatus, RunState, Task, TaskStatus};
use shprd_git::archive;
use shprd_git::git::Git;
use shprd_git::worktree::{self, Layout, SeedEntry, Worktree, parse_seed_entries};
use shprd_mux::{AgentState, Host, PaneId};

use crate::cmd::folder_trust;
use crate::ctx::{CliError, Ctx};

/// How long one model listing may take while a spawn decides whether to pass an
/// effort flag.
///
/// The listing only feeds a warning, so a slow or absent CLI must not hold the
/// spawn: a catalog that does not answer in time leaves the setting as it was.
const CATALOG_TIMEOUT: Duration = Duration::from_secs(5);

/// One agent CLI the spawn can launch.
#[derive(Debug, Clone)]
pub struct Program {
    /// The harness spec: its id is the program name, e.g. `claude`, and it
    /// carries both the launch recipe and the model rules an agent string is
    /// parsed against.
    pub spec: HarnessSpec,
    /// Whether the CLI is installed on this machine.
    pub installed: bool,
}

/// Everything [`spawn`] needs from the caller's environment.
///
/// It is plain data plus the run log, so a test builds one without touching a
/// real home directory, a real config file or a real CLI.
pub struct SpawnEnv<'a> {
    /// The run log: where events are checked and appended.
    pub ctx: &'a Ctx,
    /// The worktree root and branch namespace.
    pub layout: Layout,
    /// The directory brief files are written under, `<home>/.shprd/briefs`.
    pub briefs_dir: PathBuf,
    /// The person's home directory, which holds their settings.
    pub home: PathBuf,
    /// The directory the role prompts are read from and installed into:
    /// `<config_home>/shprd/prompts`, with `$XDG_CONFIG_HOME` winning over
    /// `<home>/.config`.
    ///
    /// It is resolved once, by [`build_env`], so a spawn never reads
    /// `$XDG_CONFIG_HOME` and a test that builds its own home keeps its prompts
    /// there whatever the process's own XDG settings say.
    pub prompts_dir: PathBuf,
    /// The settings as they resolve for this repository, before a task's own
    /// layer is added: the brief's Settings block is rendered from them.
    pub settings: Loaded,
    /// The run this task belongs to.
    pub run: String,
    /// The branch a worktree is cut from: the run's integration branch.
    pub base_branch: String,
    /// The repository's directory name, which the worktree path carries.
    pub repo_name: String,
    /// Every CLI the machine has a spec for, in spec order.
    pub programs: Vec<Program>,
    /// The configured worker agents, in fallback order.
    pub workers: Vec<String>,
    /// `[skills]` from the config: the skills every worker is given.
    pub skills: Skills,
    /// The repository's worker gate commands, from its resolved `[gates]`
    /// config. Empty when it configures none, and then the brief says so rather
    /// than telling the worker to run another project's gates.
    pub gates: Vec<String>,
    /// The model catalogs already read for this spawn, by program id. A test
    /// injects fixtures here; on a machine the map is empty and the spawn asks
    /// only the program it picked.
    pub catalogs: BTreeMap<String, Catalog>,
    /// The data the engine read: the harness table, the model catalogs and the
    /// skills tables. A directory named with `SHPRD_DATA_DIR` is handed to the
    /// launched CLI as the same variable, so the agent's own `shprd` finds the
    /// same harnesses the engine did; the embedded copy needs no variable.
    pub data_dir: DataDir,
    /// The curated skill compatibility list.
    pub compat: Vec<CompatEntry>,
    /// The environment the launched CLI would inherit.
    pub base_env: BTreeMap<String, String>,
    /// How long a freshly launched worker gets to be detected as an agent
    /// before its brief counts as undelivered. Production reads it from the
    /// environment; tests set it explicitly.
    pub ready_wait: Duration,
    /// The roots the agent CLIs write their session records under: the real
    /// `HOME` they inherit (`HOME` is a protected variable, so a spawn never
    /// rewrites it) and the XDG data home they resolve. The chat and the
    /// watchdog read a CLI's own file, so they look where *that* process wrote,
    /// never under the engine's `SHPRD_HOME`.
    pub transcript_roots: shprd_agents::transcript::Roots,
}

/// What the caller asked for.
#[derive(Debug, Clone)]
pub struct SpawnRequest {
    /// The task to work on.
    pub task: String,
    /// The agent to use, as `<program>/<model>`; `None` picks a worker.
    pub agent: Option<String>,
    /// The effort level for this attempt, when one was asked for.
    pub effort: Option<String>,
}

/// What a successful spawn did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnReport {
    /// The task that got the agent.
    pub task: String,
    /// The attempt number this spawn recorded.
    pub attempt: u32,
    /// The program that was launched, e.g. `claude`.
    pub agent: String,
    /// The model it was launched with, when one was chosen.
    pub model: Option<String>,
    /// The worktree the agent works in.
    pub worktree: PathBuf,
    /// The branch the agent commits on.
    pub branch: String,
    /// The title of the tab the agent runs in.
    pub tab: String,
    /// The brief file the agent was pointed at.
    pub brief: PathBuf,
    /// The skills the attempt must load, in the order they were selected.
    pub skills: Vec<String>,
    /// What the person should know about the launch: an effort level the model
    /// cannot use, and so on. Nothing here is fatal.
    pub warnings: Vec<String>,
    /// The attempt this spawn archived and replaced, when it was a retry.
    pub replaced: Option<Replaced>,
}

/// An attempt a retry archived and replaced.
///
/// A task's attempts all share one worktree path and one branch name, so a new
/// attempt can only start once the one before it is out of the way. Its branch
/// is archived first, which is what keeps its commits reachable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replaced {
    /// The attempt number that was archived.
    pub attempt: u32,
    /// The archive ref holding its commits.
    pub ref_name: String,
    /// The commit its branch ended on, when the new attempt carries that
    /// attempt's commits forward by starting from it. `None` when the earlier
    /// attempt left nothing of its own (or already landed), so the new one
    /// starts from the base.
    pub carried: Option<String>,
    /// The earlier attempt's own worktree, kept and brought up to date, when
    /// the new attempt works in it (and in its `target/`) instead of a fresh
    /// one. `None` when the worktree was replaced.
    pub reused: Option<Worktree>,
}

impl Replaced {
    /// The one line a human reads.
    pub fn line(&self, task: &str) -> String {
        format!(
            "archived attempt {} of {task} as {}",
            self.attempt, self.ref_name
        )
    }
}

/// The agent a spawn picked, with everything needed to start it.
#[derive(Debug, Clone)]
pub(crate) struct Pick {
    /// The program name, which is the harness spec id.
    pub(crate) agent: String,
    /// The model to select, when the agent string carried one.
    pub(crate) model: Option<String>,
    /// How to start the program.
    pub(crate) launch: LaunchSpec,
    /// The prompts the CLI waits on a human to answer before it reads anything.
    pub(crate) blocking_prompts: Vec<BlockingPrompt>,
    /// The lines the program's TUI prints once it can take its brief.
    ///
    /// Read off the bottom of the pane when the host cannot report the
    /// program's status at all ([`Pick::own_status`] is false).
    pub(crate) ready_lines: Vec<String>,
    /// Whether this program has any status source of its own.
    ///
    /// From the harness's `[status] sources`: false for a program whose every
    /// source is a plugin or hook, which is opencode's and kilo's shape. The
    /// host's own `agent_reports` capability decides the rest.
    pub(crate) own_status: bool,
    /// The program's `[status] sources`, as its harness file names them, for
    /// the spawn record.
    pub(crate) status_words: String,
}

impl Pick {
    /// The readiness this pick's program has, as the delivery needs it.
    pub(crate) fn readiness(&self) -> super::deliver::Readiness<'_> {
        super::deliver::Readiness {
            lines: &self.ready_lines,
            prompts: &self.blocking_prompts,
            own_status: self.own_status,
        }
    }

    /// The program's status sources, as its harness file names them.
    pub(crate) fn status_words(&self) -> &str {
        &self.status_words
    }
}

/// Spawns an agent for one task attempt.
///
/// The order is deliberate: every rule is checked and every decision is made
/// before the first side effect, and the effects are undone in reverse if a
/// later one fails.
pub fn spawn(
    mux: &dyn Host,
    env: &SpawnEnv<'_>,
    req: &SpawnRequest,
) -> Result<SpawnReport, CliError> {
    let state = env.ctx.state()?;
    let task = task_of(&state, &req.task)?;
    // The one gate every start passes: a retry action, a review-out answer, a
    // clash redo and `shprd agent spawn` all end here, so the cap holds even
    // for a caller that did not ask first. Callers that can answer softly ask
    // `queued_for_slot` themselves and report the wait instead of an error.
    if let Some(queued) = queued_for_slot(env, &state, &task.id) {
        return Err(CliError::Invalid(anyhow!("{queued}")));
    }
    let role = env.settings.resolve(None).role(BRIEF_ROLE);
    // The worker role's choice comes first, then the configured workers: a
    // person who set `roles.worker.agent` gets that program, and `--agent`
    // still wins over both.
    let candidates = role_candidates(role.as_ref(), &env.workers);
    let mut pick = choose_agent(req.agent.as_deref(), &candidates, &env.programs)?;
    // The worker holds an ordered list of program lines; the task runs the one
    // whose program was picked, with that line's model, effort, fast mode and
    // access mode.
    let lines = role.as_ref().map(ResolvedRole::lines).unwrap_or_default();
    let line = lines.iter().find(|line| line.agent == pick.agent);
    if pick.model.is_none() {
        if let Some(line) = line {
            pick.model = line.model.clone();
        } else if req.agent.is_none()
            && lines.is_empty()
            && let Some(model) = role
                .as_ref()
                .and_then(|role| role.model.as_ref())
                .filter(|model| !model.value.trim().is_empty())
        {
            pick.model = Some(model.value.clone());
        }
    }
    let effort = req
        .effort
        .clone()
        .or_else(|| line.and_then(|line| line.effort.clone()))
        .or_else(|| {
            // The role's own effort belongs to the role's program: another
            // program a fallback picked does not inherit it.
            if !lines.is_empty() {
                return None;
            }
            role.as_ref()
                .and_then(|role| role.effort.as_ref())
                .map(|effort| effort.value.clone())
                .filter(|effort| !effort.trim().is_empty())
        })
        .filter(|effort| !effort.trim().is_empty());
    let program_line = program_line_note(&lines, &pick.agent, req.agent.is_some());
    // What the catalog says about the level decides whether the flag is passed;
    // the setting itself stays as it was written, so the brief can say both what
    // was asked for and why it does nothing.
    let (launch_effort, effort_warning) =
        effective_effort(env, &pick.agent, pick.model.as_deref(), effort.clone());
    let mut warnings: Vec<String> = effort_warning.into_iter().collect();
    let attempt = task.attempts.len() as u32 + 1;
    // Without worktrees every attempt would edit and commit in the one checkout,
    // so a second task starting while another is in flight would mix their work.
    if !uses_worktrees(env)
        && let Some(busy) = state
            .tasks
            .values()
            .find(|other| other.id != task.id && !replaceable(other.status))
    {
        return Err(CliError::Invalid(anyhow!(
            "tasks.worktrees is off, so one task at a time works in the repository, and {:?} is \
             still {:?}: let it finish, or turn `tasks.worktrees` back on",
            busy.id,
            busy.status
        )));
    }
    let (worktree, branch) = work_place(env, &task.id)?;
    // A start that finds the task's worker already up records it instead of
    // failing: an earlier start opened the window and then lost the attempt (the
    // record failed and the close was refused, or the process died between the
    // two), and opening a second window or reporting "could not be created" over
    // the first would strand a worker that is doing the whole task.
    if uses_worktrees(env)
        && let Some(report) = adopt_live_worker(mux, env, task, attempt, &pick, &worktree, &branch)?
    {
        return Ok(report);
    }
    let skills = select_skills(&env.skills, &env.compat, &task.tags);
    // What the launch applies is decided before the brief is written, because
    // the Settings block names it.
    let program = program_of(env.programs.as_slice(), &pick.agent)?;
    let chosen = match line {
        Some(line) => applied(&program.spec, line.perm.as_deref(), line.fast),
        None => applied(
            &program.spec,
            named(role.as_ref().and_then(|role| role.perm.as_ref())),
            role.as_ref().is_some_and(|role| role.fast.value),
        ),
    };
    // How this program raises a needs-you card is the harness's fact alone
    // (`[asks] supported`), so the brief never has to know a program's name.
    let ask = AskRoute::of_harness(program.spec.asks.supported);
    // The role's words and the Settings block are read before anything is
    // touched: an agent whose prompt cannot be read must cost nothing.
    let context = brief_context(env, BRIEF_ROLE, &pick, effort.as_deref(), &chosen)?;
    let seeds = parse_seed_entries(&context.settings.tasks().worktree_files);
    if uses_worktrees(env) {
        warnings.extend(unseen_warning(env, task, &seeds));
    }
    let brief_path = brief_path(&env.briefs_dir, &env.run, &task.id, attempt);
    // A retry replaces what the failed attempt left behind, so the fresh
    // worktree below can take the same path and branch name. A task still in
    // flight is refused here, before anything is removed.
    // Without a worktree there is nothing of an earlier attempt's to remove: it
    // worked in the repository itself, which a retry must never delete.
    let replaced = if uses_worktrees(env) {
        replace_previous(mux, env, &state, task, attempt)?
    } else {
        None
    };
    let kind = spawned_event(&task.id, attempt, &pick, &worktree, &branch);
    // Refuse before any effect: a refused spawn must leave nothing behind.
    env.ctx.check(&kind)?;
    // A kept worktree and branch are already in the ledger.
    let reused = replaced.as_ref().and_then(|earlier| earlier.reused.clone());
    if uses_worktrees(env) && reused.is_none() {
        for resource in resource_kinds(&task.id, &worktree, &branch) {
            env.ctx.check(&resource)?;
        }
    }
    let earlier_asks: Vec<_> = state
        .asks
        .iter()
        .filter(|((owner, _), _)| *owner == task.id)
        .map(|(_, record)| record.clone())
        .collect();
    let brief = shprd_views::brief::task_brief_with_asks(
        &BriefInput {
            task,
            run: &env.run,
            attempt,
            agent: &pick.agent,
            model: pick.model.as_deref(),
            branch: &branch,
            worktree: &worktree.display().to_string(),
            skills: &skills,
            gates: &env.gates,
            archive: replaced.as_ref().map(|earlier| earlier.ref_name.as_str()),
            prompt: Some(&context.prompt),
            ask,
            proof: context.proof,
            settings: Some(&context.settings),
            role: context.role.as_ref(),
            warnings: &warnings,
            program_line: program_line.as_deref(),
        },
        &earlier_asks,
    );
    write_brief(&brief_path, &brief)?;
    // The repository's one space comes first: shprd's to make rather than a
    // person's to open, and where a host that keeps tabs puts the agent. A host
    // with headless agents makes no tab and no space for the task at all. A host
    // that refuses (tuios without the `admin` grant, say) refuses here, before
    // the worktree exists.
    let space = agent_space(mux, env)?;
    let carried = replaced
        .as_ref()
        .and_then(|earlier| earlier.carried.as_deref());
    let created = match reused.clone() {
        Some(kept) => kept,
        None => create_worktree(env, &task.id, carried)?,
    };
    if uses_worktrees(env) && reused.is_none() {
        warnings.extend(seed_worktree(env, &created.path, &seeds));
    }
    // A kept worktree is the earlier attempt's: a failure here must not take it
    // (and its build) away.
    let discard = |created: &Worktree| {
        if reused.is_none() {
            discard_worktree(env, created);
        }
    };
    // The ask hook goes in before the program starts, because a program reads
    // its hooks once at launch. A worktree that cannot take it is a spawn that
    // would promise cards it can never raise, so it fails and undoes itself.
    // With `tasks.worktrees` off the "worktree" is the person's own checkout,
    // which is not ours to write into.
    if !uses_worktrees(env) {
        // nothing to install into
    } else if let Err(err) = install_ask_hook(env, &pick.agent, &created.path) {
        discard(&created);
        return Err(err);
    }
    // From here on every failure undoes everything: the tab first, then the
    // worktree, and never an event.
    let opened = open_agent(
        mux,
        env,
        &space,
        &Launching {
            pick: &pick,
            applied: &chosen,
            task: &task.id,
            effort: launch_effort.as_deref(),
            brief: &brief,
            brief_path: &brief_path,
            worktree: &created,
        },
    );
    let (tab_id, tab, ready) = match opened {
        Ok(opened) => opened,
        Err(err) => {
            discard(&created);
            return Err(err);
        }
    };
    // The tab id goes next to the brief: a label is only 16 characters, so two
    // tasks can share one, and the id cannot. The record is what says which
    // handle (a tab, or a headless agent's) is the worker's.
    write_tab(
        &tab_path(&env.briefs_dir, &env.run, &task.id, attempt),
        &tab_id,
    );
    if let Err(err) = record_attempt(env, &task.id, attempt, &pick, &created, reused.is_none()) {
        // Atomic: the attempt is recorded with the window up, or the window is
        // closed and nothing is left. A window the host will not close is a live
        // worker, so it is recorded (once more) and kept, never orphaned.
        match unwind_unrecorded(
            mux,
            &tab_id,
            || record_attempt(env, &task.id, attempt, &pick, &created, reused.is_none()),
            err,
        ) {
            Unwound::Closed(err) => {
                discard(&created);
                return Err(err);
            }
            Unwound::Recorded => {}
            Unwound::Stranded(err) => {
                // The worktree and the tab record stay: the next start finds the
                // live worker there and records it.
                return Err(CliError::other(anyhow!(
                    "the worker window for {} is open but its attempt could not be recorded \
                     ({err}); it was kept, and the next start records it",
                    task.id
                )));
            }
        }
    }
    close_review_out_asks(env, &state, &task.id);
    // What said the program was ready goes beside the tab record, so a person
    // reading the run can tell a host that reported the agent from a program
    // read off its own screen — and a lost delivery from a delivered one. It is
    // written last: a spawn that did not land an attempt is one no reader should
    // find a record for.
    write_spawn_record(
        &spawn_record_path(&env.briefs_dir, &env.run, &task.id, attempt),
        &pick,
        ready,
    );
    Ok(SpawnReport {
        task: task.id.clone(),
        attempt,
        agent: pick.agent,
        model: pick.model,
        worktree: created.path,
        branch: created.branch,
        tab,
        brief: brief_path,
        skills,
        warnings,
        replaced,
    })
}

/// What became of a window whose attempt could not be recorded.
enum Unwound {
    /// The window is closed: nothing is left running.
    Closed(CliError),
    /// The window would not close, but the attempt was recorded on a retry.
    Recorded,
    /// The window would not close and the attempt still cannot be recorded.
    Stranded(CliError),
}

/// Undoes a window opened for an attempt that was never recorded.
///
/// The window closes first. A host that refuses leaves a live worker, so the
/// record is tried again, because a worker with an attempt is acceptable and one
/// without is the stranded start this guards against.
fn unwind_unrecorded(
    mux: &dyn Host,
    tab: &shprd_mux::TabId,
    mut record: impl FnMut() -> Result<(), CliError>,
    err: CliError,
) -> Unwound {
    match mux.close_tab(tab) {
        Ok(()) | Err(shprd_mux::MuxError::NotFound { .. }) => Unwound::Closed(err),
        Err(close) => {
            eprintln!("warning: could not close tab {}: {close}", tab.as_str());
            match record() {
                Ok(()) => Unwound::Recorded,
                Err(retry) => Unwound::Stranded(retry),
            }
        }
    }
}

/// Closes the open "review rounds spent" ask of `task`, answered with the
/// choice that spends one more round, now that a new attempt has started.
///
/// A retry or a send-back is that answer in effect: left open, the ask would
/// read as unanswered to the next review. Both the queued card and the ledger
/// are closed; a failure to do either is a warning, because the attempt is
/// already recorded.
fn close_review_out_asks(env: &SpawnEnv<'_>, state: &RunState, task: &str) {
    use shprd_core::gate::{RETRY_CHOICE, ROUNDS_SPENT_ASK_PREFIX};
    use shprd_core::log::{Clock, SystemClock};
    let open: Vec<String> = state
        .asks
        .iter()
        .filter(|((owner, ask), record)| {
            owner == task && ask.starts_with(ROUNDS_SPENT_ASK_PREFIX) && record.answer.is_none()
        })
        .map(|((_, ask), _)| ask.clone())
        .collect();
    let queue = shprd_core::ask::AskQueue::for_repo(&env.ctx.root);
    for ask in open {
        // The card may be answered in the queue already (a person's answer the
        // log has not folded yet): that answer stands.
        let _ = queue.answer(task, &ask, RETRY_CHOICE, SystemClock.now_ms());
        if let Err(err) = env.ctx.commit(EventKind::AskAnswered {
            task: task.to_string(),
            ask: ask.clone(),
            answer: RETRY_CHOICE.to_string(),
        }) {
            eprintln!("warning: could not close the review-out card {ask} of {task}: {err}");
        }
    }
}

/// Records the worker a lost start left running, when there is one.
///
/// The signs are the attempt's own worktree on disk and a tab that is still up:
/// the one its `.tab` record names, else one titled as this task's worker is.
/// The tab must hold an agent (anything but a finished or unreadable pane), so a
/// stale record never adopts a corpse. Returns `None` when there is nothing to
/// adopt, and the ordinary start goes on.
fn adopt_live_worker(
    mux: &dyn Host,
    env: &SpawnEnv<'_>,
    task: &Task,
    attempt: u32,
    pick: &Pick,
    worktree: &Path,
    branch: &str,
) -> Result<Option<SpawnReport>, CliError> {
    if !worktree.exists() {
        return Ok(None);
    }
    let record = tab_path(&env.briefs_dir, &env.run, &task.id, attempt);
    let recorded = std::fs::read_to_string(&record).ok();
    let title = worker_title(&pick.agent, &task.id);
    let tabs = all_handles(mux);
    let found = tabs
        .iter()
        .find(|tab| match recorded.as_deref().map(str::trim) {
            Some(id) if !id.is_empty() => tab.id.as_str() == id,
            // Without a record only a first attempt matches by title: on a retry
            // the same title is the failed attempt's own tab, not a new one.
            _ => task.attempts.is_empty() && tab.title == title,
        });
    let Some(tab) = found else {
        return Ok(None);
    };
    if !matches!(
        mux.agent_state(&tab.root_pane),
        Ok(AgentState::Working | AgentState::Blocked | AgentState::Idle | AgentState::Unknown)
    ) {
        return Ok(None);
    }
    let base_sha = Git::new(worktree).head_sha().map_err(|err| {
        CliError::other(anyhow!(err).context("reading the running worker's worktree"))
    })?;
    let created = Worktree {
        path: worktree.to_path_buf(),
        branch: branch.to_string(),
        base_sha,
    };
    let kind = spawned_event(&task.id, attempt, pick, worktree, branch);
    env.ctx.check(&kind)?;
    record_attempt(env, &task.id, attempt, pick, &created, true)?;
    write_tab(&record, &tab.id);
    // Nothing says how this window's brief went, so the spawn record says the
    // host cannot vouch for it, the safe direction for the live set.
    write_spawn_record(
        &spawn_record_path(&env.briefs_dir, &env.run, &task.id, attempt),
        pick,
        None,
    );
    Ok(Some(SpawnReport {
        task: task.id.clone(),
        attempt,
        agent: pick.agent.clone(),
        model: pick.model.clone(),
        worktree: created.path,
        branch: created.branch,
        tab: tab.title.clone(),
        brief: brief_path(&env.briefs_dir, &env.run, &task.id, attempt),
        skills: Vec::new(),
        warnings: vec![String::from(
            "the task's worker was already running: its attempt is recorded, no second window was opened",
        )],
        replaced: None,
    }))
}

/// The reason a start waits as queued: `tasks.max_running` workers (other than
/// `task`) are working already. `None` when a slot is free.
///
/// Every way a start begins — the tick, a retry or start action, a review-out
/// answer, a clash redo, `shprd agent spawn` — ends in a spawn, so this is
/// asked in front of the spawn and the cap holds whichever path got there.
pub(crate) fn queued_for_slot(env: &SpawnEnv<'_>, state: &RunState, task: &str) -> Option<String> {
    let max = env.settings.resolve(None).tasks().max_running.max(1);
    let running = state
        .tasks
        .values()
        .filter(|other| other.id != task && other.status == TaskStatus::Working)
        .count();
    (running >= usize::try_from(max).unwrap_or(usize::MAX)).then(|| {
        format!(
            "{} {running} of {max} workers are running (`tasks.max_running`); {task} starts \
             when one finishes",
            shprd_server::start_queue::QUEUED_PREFIX
        )
    })
}

/// Answers a start the cap held back: puts `task` in the service's start queue,
/// so a later pass starts it once a slot is free, and returns the plain line.
///
/// A queue that cannot be written is said in the line rather than failing the
/// start: the person is told the task waits, and that the wait is not kept.
pub(crate) fn queue_start(ctx: &Ctx, home: Option<&Path>, task: &str, queued: String) -> String {
    let dir = crate::ctx::resolve_home(home).map(|home| {
        shprd_server::repo_state_dir(&crate::ctx::resolve_state_home(&home), &ctx.root)
    });
    match dir.map(|dir| shprd_server::start_queue::enqueue(&dir, task)) {
        Ok(Ok(())) => queued,
        _ => {
            format!("{queued} (the wait could not be saved: start it again when a worker is free)")
        }
    }
}

/// The effort to pass and the warning when the model cannot use it.
///
/// The setting itself is not changed here, only what reaches the argv: a level
/// the catalog says the model cannot use is not passed, and the warning says
/// why, so a person is not left believing it applies.
fn effective_effort(
    env: &SpawnEnv<'_>,
    program: &str,
    model: Option<&str>,
    effort: Option<String>,
) -> (Option<String>, Option<String>) {
    let Some(effort) = effort else {
        return (None, None);
    };
    let Some(catalog) = catalog_for(env, program) else {
        return (Some(effort), None);
    };
    match catalog.effort_warning(program, model, &effort) {
        Some(warning) => (None, Some(warning)),
        None => (Some(effort), None),
    }
}

/// The catalog for `program`: the one a caller injected, else the one the
/// program gives itself.
///
/// A test injects a fixture into [`SpawnEnv::catalogs`] and no CLI runs. On a
/// machine the map is empty, and only the program the spawn picked is asked,
/// with [`CATALOG_TIMEOUT`]; a listing that cannot be read is `None`, because
/// this only ever feeds a warning.
fn catalog_for(env: &SpawnEnv<'_>, program: &str) -> Option<Catalog> {
    if let Some(catalog) = env.catalogs.get(program) {
        return Some(catalog.clone());
    }
    let spec = env
        .programs
        .iter()
        .find(|candidate| candidate.spec.id == program)
        .map(|candidate| &candidate.spec)?;
    let path_env = std::env::var("PATH").unwrap_or_default();
    env.data_dir.catalog(spec, &path_env, CATALOG_TIMEOUT)
}

/// The name of the space a repository's coordinator tabs live in.
///
/// One repository, one space: a human reads the name on their host's bar and
/// knows which checkout they are looking at. Tasks and agents get none of their
/// own, so it is the only space shprd makes.
pub(crate) fn repo_space_name(repo_name: &str) -> String {
    shprd_mux::repo_space_name(repo_name)
}

/// The title of a task's agent tab: `<agent>·<task>`.
///
/// The role glyphs the old naming module prefixed are gone with the
/// tab-per-agent launcher. The host's ids are what shprd follows, so a title
/// is only what a human reads; the task's own name is on its space.
pub(crate) fn worker_title(agent: &str, task: &str) -> String {
    format!("{agent}·{task}")
}

/// A repository's own name: the last component of its path.
///
/// It is the input to [`repo_space_name`], so this is
/// where the labels' one derivation gets its input from, for a caller holding a
/// path rather than a [`SpawnEnv`] (`shprd serve` does, before there is a run to
/// spawn for).
pub(crate) fn repo_name(repo: &Path) -> Result<String, CliError> {
    repo.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .ok_or_else(|| CliError::other(anyhow!("{} has no directory name", repo.display())))
}

/// The space an agent is started for, found or made at the repository root.
///
/// There is one per repository ([`repo_space_name`]): a host with headless
/// agents puts nothing in it for a worker or reviewer (they are labelled with
/// it), and a host that keeps tabs puts the agent's tab there. Only the label
/// shprd derives is reused or created, as [`ensure_repo_space`] does.
pub(crate) fn agent_space(
    mux: &dyn Host,
    env: &SpawnEnv<'_>,
) -> Result<shprd_mux::Space, CliError> {
    Ok(ensure_repo_space(mux, &env.ctx.root)?.space)
}

/// The space labelled `name`, or the refusal a missing one earns.
///
/// The label is derived ([`repo_space_name`]) and a host refuses a duplicate, so
/// nothing is ever renamed or adopted here: the space shprd uses is the one
/// whose label is exactly `name`. A refusal names that label and the ones the
/// host does have, because the label is the one thing a person can fix.
///
/// No verb calls this today — a task's space is always ensured rather than
/// looked up, because its label is shprd's own — but the refusal is the one a
/// person reads when a hand-named space is what they need, so it is kept and
/// tested rather than deleted with the last caller.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn space_named(mux: &dyn Host, name: &str) -> Result<shprd_mux::Space, CliError> {
    let found = mux.find_space(name).map_err(|err| {
        CliError::other(anyhow!(err).context(format!("looking for the space {name:?}")))
    })?;
    if let Some(space) = found {
        return Ok(space);
    }
    // A host that cannot list still gets the refusal; it just cannot name what
    // it does have, and says so rather than guessing.
    let existing = match mux.list_spaces() {
        Ok(spaces) => spaces.into_iter().map(|space| space.name).collect(),
        Err(_) => Vec::new(),
    };
    Err(CliError::Invalid(anyhow!(missing_space_message(
        name, &existing
    ))))
}

/// What a person reads when the coordinator's space is not there.
///
/// The expected label comes first and the labels the host does have follow, so a
/// space named almost right (`shprd · gate repo` for the folder `repo`) is
/// visible as the difference it is. `shprd serve` creates the space at startup,
/// which is why the refusal can point at it.
///
/// No verb calls this today for the same reason [`space_named`] is kept: the
/// refusal it builds is the one a person reads, and it is worth its own test.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn missing_space_message(expected: &str, existing: &[String]) -> String {
    let mut message = format!(
        "no space {expected:?} yet: the coordinator's tabs open in the space labelled \
         `shprd · <repo folder name>`, and shprd leaves every other space alone. \
         `shprd serve` creates it at startup; without a service, create it in your multiplexer \
         with that exact label"
    );
    match existing.split_first() {
        Some(_) => {
            let listed: Vec<String> = existing.iter().map(|name| format!("{name:?}")).collect();
            message.push_str(&format!(". The host has: {}", listed.join(", ")));
        }
        None => message.push_str(". The host has no space at all"),
    }
    message
}

/// The coordinator's space for a repository, made at the repository root when
/// the host has none.
///
/// `shprd serve` calls this at startup: the label is derived from the folder
/// name rather than chosen by a person, so leaving the space to be created by
/// hand is a run that silently cannot start a task (M9 gate bug 6). Only the
/// space with exactly that label is reused, and only that label is created;
/// anything else on the host is left alone.
pub(crate) struct PreparedSpace {
    /// The space the coordinator's tabs open in.
    pub(crate) space: shprd_mux::Space,
    /// True when this call created it, false when it was already there.
    pub(crate) created: bool,
    /// True when the space was the window's own plain space, adopted for the
    /// first time: its default shell tab (and another product's empty
    /// "Assistant" tab) are the window's, not the person's work.
    pub(crate) fresh: bool,
}

/// [`PreparedSpace`]: find the coordinator's space, or create it.
pub(crate) fn ensure_repo_space(mux: &dyn Host, repo: &Path) -> Result<PreparedSpace, CliError> {
    ensure_repo_space_from(mux, repo, &[])
}

/// [`ensure_repo_space`], also adopting a plain space open on one of `also` (the
/// folder the person added inside the repository).
pub(crate) fn ensure_repo_space_from(
    mux: &dyn Host,
    repo: &Path,
    also: &[&Path],
) -> Result<PreparedSpace, CliError> {
    let name = repo_space_name(&repo_name(repo)?);
    // Found by shprd's own mark, so a space the person renamed is still the
    // one; a space with no mark is found by its old name once and gets marked.
    let (space, created, fresh) = shprd_mux::ensure_marked_space_detail(mux, &name, repo, also)
        .map_err(|err| {
            CliError::other(anyhow!(err).context(format!(
                "finding or creating the space {name:?} at {}",
                repo.display()
            )))
        })?;
    Ok(PreparedSpace {
        space,
        created,
        fresh,
    })
}

/// One program the "+" menu started, as `POST /api/agents` answers it.
pub(crate) struct OpenedProgram {
    /// The harness id that was started.
    pub(crate) program: String,
    /// The words the tab is named with, from the harness table.
    pub(crate) name: String,
    /// The host's id for the new tab.
    pub(crate) tab: String,
}

/// Starts a harness's CLI in a new tab in the repository's own space.
///
/// This is *not* a spawn: there is no task, no brief, no worktree and no event.
/// It is Shepherdr's "+" menu row *Start an agent* — `run("tab.new", {name,
/// space, argv})` with the plain launch line (`mods/new-menu-tasks/main.js:70`)
/// — which is a person starting a conversation, not a task's attempt. So the
/// argv is the harness table's own `[launch] interactive` and nothing is
/// appended to it: no access mode, no model, no fast switch. Those belong to a
/// role, and a role belongs to a task.
///
/// The tab opens in `space` when the window names the one it is showing, else in
/// the space `shprd serve` prepared for the repository, at the repository root, named with the program's own name (`Claude Code`), which is
/// what Shepherdr names it.
pub(crate) fn open_program(
    mux: &dyn Host,
    data: &DataDir,
    repo: &Path,
    program: &str,
    space: Option<&str>,
) -> Result<OpenedProgram, CliError> {
    let specs = data.harness_specs().map_err(|err| {
        CliError::other(
            anyhow!(err).context(format!("loading the harness table from {}", data.label())),
        )
    })?;
    let Some(spec) = specs.iter().find(|spec| spec.id == program) else {
        let known: Vec<&str> = specs.iter().map(|spec| spec.id.as_str()).collect();
        return Err(CliError::Invalid(anyhow!(
            "no harness spec for {program:?} in the data directory; it names {}",
            known.join(", ")
        )));
    };
    let target = match space {
        Some(id) => shprd_mux::SpaceId::new(id),
        None => ensure_repo_space(mux, repo)?.space.id,
    };
    let tab = mux
        .spawn(&target, &spec.name, &spec.launch.interactive, &[], repo)
        .map_err(|err| CliError::other(anyhow!(err).context("starting the agent's tab")))?;
    Ok(OpenedProgram {
        program: spec.id.clone(),
        name: spec.name.clone(),
        tab: tab.id.as_str().to_string(),
    })
}

/// Opens the agent's tab and starts its CLI in it.
///
/// Returns the tab's id and title. A host that has no `system_prompt_args`
/// cannot take the brief as an argument, so the brief is typed in as the agent's
/// first turn instead.
/// What [`open_agent`] needs to start one attempt's CLI.
pub(crate) struct Launching<'a> {
    /// The agent that was picked.
    pub(crate) pick: &'a Pick,
    /// The access mode and fast switch this launch applies.
    pub(crate) applied: &'a Applied,
    /// The task it works on.
    pub(crate) task: &'a str,
    /// The effort it runs at, when one was asked for.
    pub(crate) effort: Option<&'a str>,
    /// The brief the agent reads.
    pub(crate) brief: &'a str,
    /// The brief file's path.
    pub(crate) brief_path: &'a Path,
    /// The worktree the agent works in.
    pub(crate) worktree: &'a Worktree,
}

/// Starts the agent's CLI (headless where the host can, a tab of the
/// repository's space where it cannot), handing it the brief.
///
/// Returns the tab's id, its title and what said the program was ready for its
/// brief (`None` when the brief did not arrive).
pub(crate) fn open_agent(
    mux: &dyn Host,
    env: &SpawnEnv<'_>,
    space: &shprd_mux::Space,
    launching: &Launching<'_>,
) -> Result<(shprd_mux::TabId, String, Option<super::deliver::ReadyBy>), CliError> {
    let Launching {
        pick,
        applied,
        task,
        effort,
        brief,
        brief_path,
        worktree,
    } = *launching;
    let title = worker_title(&pick.agent, task);
    // One call makes the tab and starts the program in it, so a launch that
    // cannot start is a spawn that leaves no tab behind: [`Host::spawn`]'s own
    // contract. The brief still follows as its own turn, because a host starts
    // a program and never types for it.
    let program = program_of(env.programs.as_slice(), &pick.agent)?;
    let (mut argv, brief_in_args) = launch_argv(
        &pick.launch,
        &program.spec,
        applied,
        pick.model.as_deref(),
        effort,
        brief,
        brief_path,
    )?;
    trust_launch_folder(&pick.agent, &worktree.path, &env.base_env, &mut argv);
    let mut pairs = env_pairs(launch_env(
        &env.base_env,
        EnvRole::Worker,
        &EnvContext {
            actor: format!("agent:{}", pick.agent),
            task: Some(task.to_string()),
            run: Some(env.run.clone()),
            repo: worktree.path.clone(),
            home: Some(env.home.clone()),
            bin: shprd_agents::env::service_bin(),
            data: env.data_dir.env_value().map(Path::to_path_buf),
        },
    ));
    // A worker the coordinator's own `shprd tick` starts inherits the
    // coordinator's shell, so a coordinator-only permission rule is removed.
    if env
        .base_env
        .get(folder_trust::OPENCODE_PERMISSION)
        .is_some_and(|value| folder_trust::is_coordinator_rule(value))
    {
        pairs.push((folder_trust::OPENCODE_PERMISSION.to_string(), None));
    }
    // The coordinator that is running in the repository's space is the agent's
    // parent, so the host can keep it under a restarted coordinator; none
    // running leaves it parentless, which is a worker all the same.
    let parent = coordinator_pane_of(&env.home, &env.ctx.root, mux, space);
    let tab = mux
        .spawn_agent(&shprd_mux::AgentLaunch {
            space: &space.id,
            title: &title,
            argv: &argv,
            env: &pairs,
            cwd: &worktree.path,
            role: shprd_mux::AgentRole::Worker,
            task: Some(task),
            program: &pick.agent,
            parent: parent.as_ref(),
        })
        .map_err(|err| CliError::other(anyhow!(err).context("starting the agent")))?;
    // The tab exists from here on, so every failure closes it again: a tab left
    // open points at a worktree the failing spawn is about to remove.
    match hand_over(
        mux,
        env,
        pick,
        brief,
        brief_path,
        task,
        &title,
        &tab,
        brief_in_args,
    ) {
        Ok(ready) => Ok((tab.id, title, ready)),
        Err(err) => {
            close_tab(mux, &tab.id);
            Err(err)
        }
    }
}

/// Hands the agent that is already running in `tab` its brief.
///
/// A brief that does not arrive is reported, never fatal: the CLI is running,
/// and the human can resend the brief into the tab. What arrives answers what
/// said the program was ready, which the caller records beside the attempt.
#[expect(clippy::too_many_arguments, reason = "one launch's whole input")]
fn hand_over(
    mux: &dyn Host,
    env: &SpawnEnv<'_>,
    pick: &Pick,
    brief: &str,
    brief_path: &Path,
    task: &str,
    title: &str,
    tab: &shprd_mux::Tab,
    brief_in_args: bool,
) -> Result<Option<super::deliver::ReadyBy>, CliError> {
    // A brief that arrived as a system prompt leaves the CLI idle, waiting for a
    // first turn, so it gets a one-line kickoff; an agent with no system-prompt
    // flag gets the whole brief typed in, as before.
    let (text, what) = if brief_in_args {
        (super::deliver::KICKOFF, "kickoff")
    } else {
        (brief, "brief")
    };
    let delivery =
        super::deliver::deliver_brief(mux, &tab.root_pane, text, &pick.readiness(), env.ready_wait);
    let Some(reason) = delivery.reason() else {
        return Ok(delivery.ready());
    };
    eprintln!(
        "warning: {what} not delivered to {title}: {reason}; resend with: shprd agent prompt --task {task} \"$(cat {})\"",
        brief_path.display()
    );
    let escalation = EventKind::Escalated {
        task: Some(task.to_string()),
        body: shprd_views::cards::BRIEF_LOST.to_string(),
        default_action: match delivery.blocked_by(&pick.agent) {
            Some(cause) => format!("{cause}: answer it in the tab, then resend the brief"),
            None => "resend the brief".to_string(),
        },
    };
    if let Err(err) = env.ctx.commit(escalation) {
        eprintln!("warning: could not record the escalation: {err}");
    }
    Ok(None)
}

/// Closes a tab this spawn opened, ignoring the failure for the same reason as
/// [`discard_worktree`].
pub(crate) fn close_tab(mux: &dyn Host, tab: &shprd_mux::TabId) {
    if let Err(err) = mux.close_tab(tab) {
        eprintln!("warning: could not close tab {}: {err}", tab.as_str());
    }
}

/// A pane or tab shprd tried to close and the host kept.
///
/// Run end records the run's last event whether or not the mux lets go, so a
/// refusal there is not an error the verb can return: it is collected instead,
/// named the way a human needs it — what it was, and the id that closes it —
/// and every leftover goes into one escalation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leftover {
    /// What it is, in the human's words: `watchdog pane`, `worker tab for T1`.
    pub what: String,
    /// The mux id it still holds.
    pub id: String,
}

impl Leftover {
    /// A leftover of `what`, holding `id`.
    pub(crate) fn new(what: impl Into<String>, id: impl std::fmt::Display) -> Self {
        Self {
            what: what.into(),
            id: id.to_string(),
        }
    }

    /// The one line a human reads: what it is, then the id that closes it.
    pub(crate) fn line(&self) -> String {
        format!("{} {}", self.what, self.id)
    }
}

/// Closes a pane shprd opened, counting a pane the host has forgotten as
/// closed, and answers what the host kept.
///
/// The same rule [`close_recorded`] applies to a tab, for a pane a caller can go
/// on without: a pane a human already closed is in the state the caller wanted,
/// and a pane the host keeps is handed back as a [`Leftover`] so the run's end
/// can list everything still open in one place.
pub(crate) fn close_pane_leftover(
    mux: &dyn Host,
    what: &str,
    pane: &shprd_mux::PaneId,
) -> Option<Leftover> {
    match mux.close_pane(pane) {
        Ok(()) | Err(shprd_mux::MuxError::NotFound { .. }) => None,
        Err(_) => Some(Leftover::new(what, pane.as_str())),
    }
}

/// Stops the agents a landed or dropped task still has: its worker and its
/// reviewer.
///
/// There is no task space to close any more: workers and reviewers are
/// headless on a host that has them and tabs of the repository's space on one
/// that does not. Either way each attempt wrote a record beside its brief
/// (`<task>-a<n>.tab`, `<task>-a<n>-review.tab`) holding the handle
/// [`Host::spawn_agent`] returned, and closing the handle stops the agent. A
/// handle the host has already forgotten is [`MuxError::NotFound`], which counts
/// as stopped, so a second landing racing this one cannot fail on the normal
/// path.
pub fn stop_task_agents(
    mux: &dyn Host,
    briefs: &Path,
    run: &str,
    task: &str,
) -> Result<(), CliError> {
    close_task_tabs(mux, briefs, run, task)
}

/// Closes the tabs a run recorded for `task`, once no attempt can use them.
///
/// The records beside the briefs are the only list of tabs a run opened:
/// `<task>-a<n>.tab` for a worker and `<task>-a<n>-review.tab` for a reviewer
/// both live in `<briefs>/<run>/`, and the whole name has to match, so one
/// task's records never stand in for another's (`T1-a1.tab` is not
/// `T10-a1.tab`, and `T1-audit-a1.tab` is not `T1`'s).
pub fn close_worker(mux: &dyn Host, briefs: &Path, run: &str, task: &str) -> Result<(), CliError> {
    close_records(mux, briefs, run, "worker's", move |name| {
        is_task_tab_record(name, task) && !name.ends_with(REVIEW_TAB_SUFFIX)
    })
}

/// The stem suffix a reviewer's tab record carries: the `-review` that
/// [`REVIEW_TAB_SUFFIX`] puts in front of the `.tab` extension.
const REVIEW_STEM_SUFFIX: &str = "-review";

/// Whether a tab record's file name is one of `task`'s own.
///
/// A record is exactly `<task>-a<digits>.tab` for a worker or
/// `<task>-a<digits>-review.tab` for a reviewer, with one or more ASCII digits
/// as the attempt and nothing else in the name. A prefix test is not enough: it
/// would hand `T1-audit-a1.tab` to a drop of `T1`, and the drop would close a
/// tab another task still needs.
fn is_task_tab_record(file_name: &str, task: &str) -> bool {
    let Some(stem) = file_name.strip_suffix(".tab") else {
        return false;
    };
    let stem = stem.strip_suffix(REVIEW_STEM_SUFFIX).unwrap_or(stem);
    let Some(rest) = stem.strip_prefix(task) else {
        return false;
    };
    let Some(attempt) = rest.strip_prefix("-a") else {
        return false;
    };
    !attempt.is_empty() && attempt.bytes().all(|byte| byte.is_ascii_digit())
}

/// Closes every tab the run recorded for `task`: the worker's and the
/// reviewer's alike.
///
/// [`close_worker`] leaves the reviewer's records alone because a task that is
/// still in the run gets its reviewer tab closed by the verdict that decides the
/// review ([`review::close_reviewer`](super::review::close_reviewer)). A drop
/// has no verdict to wait for: the task is leaving the run, so both kinds of
/// record are closed here, and the caller counts every refusal — a tab the host
/// keeps still has an agent in it.
pub(crate) fn close_task_tabs(
    mux: &dyn Host,
    briefs: &Path,
    run: &str,
    task: &str,
) -> Result<(), CliError> {
    close_records(mux, briefs, run, "task's", move |name| {
        is_task_tab_record(name, task)
    })
}

/// Closes every tab the run recorded, as the run's last act.
///
/// Run end has no task in hand, so it sweeps the lot: every `.tab` record under
/// the run's briefs names a tab shprd opened, and nothing shprd did not open has
/// a record there — which is why a tab a human opened is never in reach. A
/// refusal is not fatal: the tabs are cleanup, not the point, so each one the
/// host keeps comes back as a [`Leftover`].
pub(crate) fn close_run_tabs(mux: &dyn Host, ctx: &Ctx, home: Option<&Path>) -> Vec<Leftover> {
    let Ok(home) = crate::ctx::resolve_home(home) else {
        return Vec::new();
    };
    let Ok(state) = ctx.state() else {
        return Vec::new();
    };
    let Some(run) = state.run.as_ref().map(|run| run.run_id.clone()) else {
        return Vec::new();
    };
    let briefs = home.join(".shprd").join("briefs");
    let mut leftovers = Vec::new();
    if let Err(err) = sweep_records(mux, &briefs, &run, |_| true, &mut leftovers) {
        eprintln!("warning: {err}");
    }
    leftovers
}

/// Ends the run's mux footprint: the tabs the run recorded, then the watchdog's
/// pane.
///
/// Everything the host refuses is collected by name and id, printed for the
/// human, and recorded as one [`EventKind::Escalated`]. The caller must have
/// checked [`EventKind::RunFinished`] first and commit it after: the ledger
/// refuses anything but cleanup once the run is finished, so this is the last
/// point at which an escalation can land — and a refused run closes nothing.
pub fn close_at_run_end(
    mux: &dyn Host,
    ctx: &Ctx,
    home: Option<&Path>,
    watch_pane: Option<&shprd_mux::PaneId>,
) -> Vec<Leftover> {
    let mut leftovers = close_run_tabs(mux, ctx, home);
    if let Some(pane) = watch_pane {
        leftovers.extend(close_pane_leftover(mux, "watchdog pane", pane));
    }
    for leftover in &leftovers {
        eprintln!(
            "warning: could not close the {}: close it yourself",
            leftover.line()
        );
    }
    escalate_leftovers(ctx, &leftovers);
    leftovers
}

/// Records what the host kept as the run's last escalation.
///
/// One event for the whole sweep, because the human has one job left: close what
/// the line lists. Nothing left over means no event — an escalation nobody has
/// to act on is noise in the ledger.
fn escalate_leftovers(ctx: &Ctx, leftovers: &[Leftover]) {
    if leftovers.is_empty() {
        return;
    }
    let listed: Vec<String> = leftovers.iter().map(Leftover::line).collect();
    let kind = EventKind::Escalated {
        task: None,
        body: format!(
            "the run ended with {} still open: {}",
            leftovers.len(),
            listed.join("; ")
        ),
        default_action: "close them yourself: each entry names what it is and its mux id"
            .to_string(),
    };
    if let Err(err) = ctx.commit(kind) {
        eprintln!("warning: could not record the escalation: {err}");
    }
}

/// The suffix a reviewer's tab record carries, which tells its records apart
/// from a worker's in the same run folder.
pub const REVIEW_TAB_SUFFIX: &str = "-review.tab";

/// Closes the tab one record names, counting a tab the host has forgotten as
/// closed.
///
/// A host that no longer knows a tab (a human closed it, say) is not an error:
/// that is the state the caller wanted anyway. `what` names the tab's owner in
/// the error, and an unreadable record names no tab at all.
pub(crate) fn close_recorded(mux: &dyn Host, what: &str, record: &Path) -> Result<(), CliError> {
    let Ok(id) = std::fs::read_to_string(record) else {
        return Ok(());
    };
    let id = id.trim();
    if id.is_empty() {
        return Ok(());
    }
    let tab = shprd_mux::TabId::new(id);
    match mux.close_tab(&tab) {
        Ok(()) | Err(shprd_mux::MuxError::NotFound { .. }) => Ok(()),
        Err(err) => Err(CliError::other(anyhow!(
            "the {what} tab {tab} recorded in {} is still open: {err}",
            record.display()
        ))),
    }
}

/// Closes every tab record in the run's folder that `wanted` picks out.
fn close_records(
    mux: &dyn Host,
    briefs: &Path,
    run: &str,
    what: &str,
    wanted: impl Fn(&str) -> bool,
) -> Result<(), CliError> {
    for record in tab_records(briefs, run, wanted)? {
        close_recorded(mux, what, &record)?;
    }
    Ok(())
}

/// Sweeps every tab record `wanted` picks out, collecting the tabs the host
/// keeps instead of stopping at the first refusal.
fn sweep_records(
    mux: &dyn Host,
    briefs: &Path,
    run: &str,
    wanted: impl Fn(&str) -> bool,
    leftovers: &mut Vec<Leftover>,
) -> Result<(), CliError> {
    for record in tab_records(briefs, run, wanted)? {
        let Ok(id) = std::fs::read_to_string(&record) else {
            continue;
        };
        let id = id.trim();
        if id.is_empty() {
            continue;
        }
        let tab = shprd_mux::TabId::new(id);
        match mux.close_tab(&tab) {
            Ok(()) | Err(shprd_mux::MuxError::NotFound { .. }) => {}
            Err(_) => leftovers.push(Leftover::new(tab_record_what(&record), &tab)),
        }
    }
    Ok(())
}

/// What a tab record names, in the words a human needs to find it again.
///
/// A spawn writes `<task>-a<n>.tab` beside the brief and a review writes
/// `<task>-a<n>-review.tab`, so the record's own name says whose tab it is and
/// which task it belonged to. A name that is neither falls back to the path,
/// which is still something the human can read.
fn tab_record_what(record: &Path) -> String {
    let name = record
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let unnamed = || format!("the tab recorded in {}", record.display());
    let Some(stem) = name.strip_suffix(".tab") else {
        return unnamed();
    };
    let (stem, role) = match stem.strip_suffix("-review") {
        Some(stem) => (stem, "reviewer"),
        None => (stem, "worker"),
    };
    match stem.rsplit_once("-a") {
        Some((task, attempt))
            if !task.is_empty()
                && !attempt.is_empty()
                && attempt.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            format!("{role} tab for {task}")
        }
        _ => unnamed(),
    }
}

/// The tab records in the run's folder that `wanted` picks out, in name order.
///
/// A directory read promises nothing, and two sweeps of one run should close the
/// same tabs in the same order.
fn tab_records(
    briefs: &Path,
    run: &str,
    wanted: impl Fn(&str) -> bool,
) -> Result<Vec<PathBuf>, CliError> {
    let dir = briefs.join(run);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        // No run folder is no records: nothing was ever opened to close.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => {
            return Err(CliError::other(
                anyhow!(err).context(format!("reading the tab records in {}", dir.display())),
            ));
        }
    };
    let mut records = Vec::new();
    for entry in entries {
        let path = entry
            .map_err(|err| CliError::other(anyhow!(err).context("reading a tab record name")))?
            .path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if name.ends_with(".tab") && wanted(name) {
            records.push(path);
        }
    }
    records.sort();
    Ok(records)
}

/// The ledger entries a spawn writes for its worktree and branch.
fn resource_kinds(task: &str, worktree: &Path, branch: &str) -> [EventKind; 2] {
    [
        EventKind::Resource {
            action: ResourceAction::Created,
            kind: ResourceKind::Worktree,
            name: worktree.display().to_string(),
            task: Some(task.to_string()),
        },
        EventKind::Resource {
            action: ResourceAction::Created,
            kind: ResourceKind::Branch,
            name: branch.to_string(),
            task: Some(task.to_string()),
        },
    ]
}

/// True when each attempt gets its own worktree (`tasks.worktrees`, on unless
/// someone turned it off).
fn uses_worktrees(env: &SpawnEnv<'_>) -> bool {
    env.settings.resolve(None).tasks().worktrees
}

/// What a new worktree is cut from: `tasks.base` when it names one, else the
/// run's integration branch, at its freshest commit
/// ([`shprd_git::sync::freshest_base`]): a task redone after a landing merged
/// on the forge starts from that landing, not from a stale local ref.
fn base_for_new_work(env: &SpawnEnv<'_>) -> String {
    let tasks = env.settings.resolve(None).tasks();
    let base = match tasks.base.trim() {
        "" => env.base_branch.clone(),
        base => base.to_owned(),
    };
    shprd_git::sync::freshest_base(&Git::new(&env.ctx.root), &base)
}

/// Where an attempt works and the branch it commits on.
///
/// With worktrees on that is the layout's path and the branch `tasks.branch`
/// names. With them off (`tasks.worktrees = false`) it is the repository itself
/// on the branch it already has checked out.
fn work_place(env: &SpawnEnv<'_>, task: &str) -> Result<(PathBuf, String), CliError> {
    if uses_worktrees(env) {
        return Ok((
            env.layout.path(&env.repo_name, &env.run, task),
            env.layout.branch_name(&env.run, task),
        ));
    }
    let branch = Git::new(&env.ctx.root)
        .current_branch()
        .map_err(|err| CliError::other(anyhow!(err).context("reading the checked-out branch")))?
        .unwrap_or_else(|| String::from("HEAD"));
    Ok((env.ctx.root.clone(), branch))
}

/// Creates the attempt's worktree from `carried` (the earlier attempt's last
/// commit, on a retry) or else [`base_for_new_work`], or, with
/// worktrees off, points the attempt at the repository's own checkout.
fn create_worktree(
    env: &SpawnEnv<'_>,
    task: &str,
    carried: Option<&str>,
) -> Result<Worktree, CliError> {
    if !uses_worktrees(env) {
        let (path, branch) = work_place(env, task)?;
        let base_sha = Git::new(&path).head_sha().map_err(|err| {
            CliError::other(anyhow!(err).context("reading the repository's head"))
        })?;
        return Ok(Worktree {
            path,
            branch,
            base_sha,
        });
    }
    worktree::create(
        &env.ctx.root,
        &env.layout,
        &env.run,
        task,
        // A retry continues from where the attempt before it ended.
        carried
            .map_or_else(|| base_for_new_work(env), str::to_owned)
            .as_str(),
    )
    .map_err(|err| plain_failure(task, "its new workspace could not be created", &err))
}

/// The warning for untracked or git-ignored files of the main checkout that the
/// task's scope or words name and `tasks.worktree_files` does not provide: a
/// worktree is cut from a branch, so its worker will not see them.
fn unseen_warning(env: &SpawnEnv<'_>, task: &Task, seeds: &[SeedEntry]) -> Option<String> {
    let texts: Vec<&str> = std::iter::once(task.title.as_str())
        .chain(task.scope.iter().map(String::as_str))
        .chain(task.acceptance.iter().map(String::as_str))
        .collect();
    let found = worktree::unseen_mentions(&env.ctx.root, &texts, seeds);
    if found.is_empty() {
        return None;
    }
    Some(format!(
        "the task names untracked or git-ignored files the worker will not see in its worktree: \
         {}; commit them, or list them in `tasks.worktree_files` (they are in the main checkout, {})",
        found.join(", "),
        env.ctx.root.display()
    ))
}

/// Copies or links `tasks.worktree_files` into the new worktree; one warning
/// for each entry that could not be placed.
fn seed_worktree(env: &SpawnEnv<'_>, worktree: &Path, seeds: &[SeedEntry]) -> Vec<String> {
    worktree::seed_files(&env.ctx.root, worktree, seeds)
        .into_iter()
        .filter_map(|outcome| match outcome {
            worktree::SeedOutcome::Placed(_) => None,
            worktree::SeedOutcome::Skipped(entry, why) => Some(format!(
                "tasks.worktree_files: {:?} was not placed in the worktree: {why}",
                entry.path
            )),
        })
        .collect()
}

/// Puts `program`'s ask hook into the worktree it is about to work in.
///
/// It is what `shprd ask install` does, run at spawn so a worker's
/// `AskUserQuestion` and permission prompts become Needs-you cards without a
/// person installing anything. The config written is the program's own
/// worktree-local one (`.claude/settings.local.json`, `.codex/hooks.json`);
/// the installer refuses any target outside the worktree, so a person's own
/// settings are never touched. A program with no hook is left alone.
fn install_ask_hook(env: &SpawnEnv<'_>, program: &str, worktree: &Path) -> Result<(), CliError> {
    let spec = &program_of(env.programs.as_slice(), program)?.spec;
    let installer = HarnessAskHook::for_spec(&spec.asks, &spec.id);
    if !installer.available() {
        return Ok(());
    }
    let binary = shprd_agents::env::service_bin().ok_or_else(|| {
        CliError::other(anyhow!("cannot resolve the shprd binary for the ask hook"))
    })?;
    let hook = worktree.join(".shprd").join("ask-hook.sh");
    installer
        .install(&AskHookPaths::new(&hook, worktree, &binary))
        .map(|_| ())
        .map_err(|err| {
            CliError::other(anyhow!(err).context(format!(
                "installing the {program} ask hook in {}",
                worktree.display()
            )))
        })
}

/// Removes a worktree this spawn created, ignoring the failure: the caller is
/// already returning a more useful error.
fn discard_worktree(env: &SpawnEnv<'_>, created: &Worktree) {
    // The repository's own checkout is not ours to remove.
    if !uses_worktrees(env) {
        return;
    }
    if let Err(err) = worktree::remove(&env.ctx.root, &env.layout, created, true) {
        eprintln!(
            "warning: could not remove {}: {err}",
            created.path.display()
        );
    }
}

/// Archives the previous attempt's branch and removes its worktree and branch.
///
/// Every attempt at a task works at the same path on the same branch, so a new
/// attempt can only start once the one it replaces is out of the way. The
/// branch is archived first, with [`archive::archive_to`], and only then
/// deleted, so no commit is ever lost: each attempt's archive ref holds its tip,
/// and the new worker's brief names the ref so it can cherry-pick what is still
/// useful. Every removal is recorded, because the ledger is what licenses the
/// next cleanup.
///
/// Only a failed attempt may be replaced. One that is still working, or has
/// reported and is waiting on its gates or on integration, is work in flight:
/// removing its worktree under the agent's feet would throw that work away, so
/// it is refused with the reason.
fn replace_previous(
    mux: &dyn Host,
    env: &SpawnEnv<'_>,
    state: &RunState,
    task: &Task,
    attempt: u32,
) -> Result<Option<Replaced>, CliError> {
    let Some(previous) = task.attempts.last().map(|earlier| earlier.n) else {
        return Ok(None);
    };
    if attempt <= previous {
        return Ok(None);
    }
    let path = env.layout.path(&env.repo_name, &env.run, &task.id);
    let branch = env.layout.branch_name(&env.run, &task.id);
    let branch_exists = branch_exists(env, &branch).map_err(|err| {
        plain_failure(
            &task.id,
            "the old attempt's branch could not be looked up",
            &err,
        )
    })?;
    if !branch_exists && !path.exists() {
        return Ok(None);
    }
    if !replaceable(task.status) {
        return Err(CliError::Invalid(anyhow!(
            "task {:?} is {:?}: attempt {previous} cannot be archived and replaced \
             until it fails or is integrated",
            task.id,
            task.status
        )));
    }
    let ref_name = archive::archive_ref_for_attempt(&env.run, &task.id, previous);
    let task_id = Some(task.id.clone());
    // What the new attempt starts from: the branch's last commit, when the
    // attempt made commits of its own that have not landed.
    let carried = if branch_exists && task.status != TaskStatus::Integrated {
        carried_head(env, &branch).map_err(|err| {
            plain_failure(&task.id, "the old attempt's work could not be read", &err)
        })?
    } else {
        None
    };
    let mut archive_events: Vec<EventKind> = Vec::new();
    if branch_exists {
        if allows(
            state,
            ResourceKind::ArchiveRef,
            &ref_name,
            ResourceAction::Created,
        ) {
            archive_events.push(EventKind::Resource {
                action: ResourceAction::Created,
                kind: ResourceKind::ArchiveRef,
                name: ref_name.clone(),
                task: task_id.clone(),
            });
        }
        if allows(
            state,
            ResourceKind::Branch,
            &branch,
            ResourceAction::Archived,
        ) {
            archive_events.push(EventKind::Resource {
                action: ResourceAction::Archived,
                kind: ResourceKind::Branch,
                name: branch.clone(),
                task: task_id.clone(),
            });
        }
    }
    let path_text = path.display().to_string();
    let worktree_event = allows(
        state,
        ResourceKind::Worktree,
        &path_text,
        ResourceAction::Removed,
    )
    .then(|| EventKind::Resource {
        action: ResourceAction::Removed,
        kind: ResourceKind::Worktree,
        name: path_text.clone(),
        task: task_id.clone(),
    });
    let branch_event = allows(
        state,
        ResourceKind::Branch,
        &branch,
        ResourceAction::Removed,
    )
    .then(|| EventKind::Resource {
        action: ResourceAction::Removed,
        kind: ResourceKind::Branch,
        name: branch.clone(),
        task: task_id.clone(),
    });
    // Everything the ledger will be asked to accept is checked before the first
    // removal: a replacement the ledger refuses must cost nothing.
    for event in archive_events
        .iter()
        .chain(worktree_event.iter())
        .chain(branch_event.iter())
    {
        env.ctx.check(event)?;
    }
    // The branch's own `Archived` event waits: a worktree that is kept keeps its
    // branch live.
    let (ref_events, branch_archived): (Vec<EventKind>, Vec<EventKind>) =
        archive_events.into_iter().partition(|event| {
            matches!(
                event,
                EventKind::Resource {
                    kind: ResourceKind::ArchiveRef,
                    ..
                }
            )
        });
    if branch_exists {
        archive::archive_to(&env.ctx.root, &branch, &ref_name).map_err(|err| {
            plain_failure(&task.id, "the old attempt's work could not be saved", &err)
        })?;
        for event in ref_events {
            env.ctx.commit(event)?;
        }
    }
    // The previous attempt's tab points at a worktree that is about to go, and
    // its worker has already been sent back: leaving the tab open would leave a
    // pane typing into a directory that no longer exists (or, when the
    // worktree is kept, into the new worker's own folder).
    if let Err(err) = close_worker(mux, &env.briefs_dir, &env.run, &task.id) {
        eprintln!("warning: {err}");
    }
    // A new attempt takes over the previous attempt's worktree, with its
    // `target/` and every other build output, unless the worktree is gone or
    // the task is being redone after a clash, which needs a clean base.
    if branch_exists
        && task.status != TaskStatus::Integrated
        && !shprd_core::rules::landing_conflict(task)
        && let Some(reused) = reuse_worktree(env, &path, &branch)
    {
        return Ok(Some(Replaced {
            attempt: previous,
            ref_name,
            carried,
            reused: Some(reused),
        }));
    }
    for event in branch_archived {
        env.ctx.commit(event)?;
    }
    // Git knows the old worktree by the path it was made at, which can be
    // spelled differently from the layout's now (`/tmp` against `/private/tmp`,
    // a symlinked home), so what holds the branch is asked of git rather than
    // assumed from the layout's string.
    let held = worktrees_holding(env, &branch).map_err(|err| {
        plain_failure(
            &task.id,
            "the old attempt's workspace could not be found",
            &err,
        )
    })?;
    if worktree_event.is_some() || path.exists() || !held.is_empty() {
        for place in held.iter().chain(std::iter::once(&path)) {
            if place.exists() {
                worktree::remove_worktree_only(&env.ctx.root, &env.layout, place, true).map_err(
                    |err| {
                        plain_failure(
                            &task.id,
                            "the old attempt's workspace is still in use",
                            &err,
                        )
                    },
                )?;
            }
        }
        // A registration whose folder is already gone would still pin the branch.

        let _ = Git::new(&env.ctx.root).run(&["worktree", "prune"]);
        if let Some(event) = worktree_event {
            env.ctx.commit(event)?;
        }
    }
    if branch_exists {
        worktree::delete_branch(&env.ctx.root, &env.layout, &branch).map_err(|err| {
            plain_failure(
                &task.id,
                "the old attempt's branch could not be cleared",
                &err,
            )
        })?;
        if let Some(event) = branch_event {
            env.ctx.commit(event)?;
        }
    }
    Ok(Some(Replaced {
        attempt: previous,
        ref_name,
        carried,
        reused: None,
    }))
}

/// Brings the previous attempt's worktree up to date with the integration
/// branch so the next attempt can work in it, or says it cannot.
///
/// The worktree must still be there with its branch checked out and no
/// uncommitted change to tracked files. The branch is rebased onto the freshest
/// base ([`base_for_new_work`]): the earlier attempt's commits are already kept
/// under its archive ref, and a rebase that stops is undone, which leaves
/// the worktree as it was for the clean replacement to remove.
fn reuse_worktree(env: &SpawnEnv<'_>, path: &Path, branch: &str) -> Option<Worktree> {
    if !path.is_dir() {
        return None;
    }
    let git = Git::new(path);
    if git.current_branch().ok().flatten().as_deref() != Some(branch) {
        return None;
    }
    let dirty = git
        .run(&["status", "--porcelain", "--untracked-files=no"])
        .ok()?;
    if !dirty.trim().is_empty() {
        return None;
    }
    let base = base_for_new_work(env);
    if git.run(&["rebase", &base]).is_err() {
        let _ = git.run(&["rebase", "--abort"]);
        return None;
    }
    let base_sha = git.rev_parse(&base).ok()?;
    Some(Worktree {
        path: path.to_path_buf(),
        branch: branch.to_string(),
        base_sha,
    })
}

/// The commit a retry should start from: `branch`'s tip, when the attempt
/// committed something the base does not already have. An attempt that made no
/// commits (or whose commits the base holds) gives `None`, so the retry starts
/// from the current base instead of an older point.
fn carried_head(
    env: &SpawnEnv<'_>,
    branch: &str,
) -> Result<Option<String>, shprd_git::git::GitError> {
    let git = Git::new(&env.ctx.root);
    let head = git.rev_parse(branch)?;
    let base = base_for_new_work(env);
    // Exit 1 is git's answer "not an ancestor", which is the attempt having
    // commits of its own; any other failure is a failure, not an answer.
    match git.run(&["merge-base", "--is-ancestor", &head, &base]) {
        Ok(_) => Ok(None),
        Err(shprd_git::git::GitError::Failed { code: Some(1), .. }) => Ok(Some(head)),
        Err(err) => Err(err),
    }
}

/// Every place git has `branch` checked out, as git spells the path.
fn worktrees_holding(
    env: &SpawnEnv<'_>,
    branch: &str,
) -> Result<Vec<PathBuf>, shprd_git::git::GitError> {
    let listing = Git::new(&env.ctx.root).run(&["worktree", "list", "--porcelain"])?;
    let wanted = format!("refs/heads/{branch}");
    let mut found = Vec::new();
    let mut current: Option<PathBuf> = None;
    for line in listing.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            current = Some(PathBuf::from(path));
        } else if line.strip_prefix("branch ") == Some(wanted.as_str())
            && let Some(path) = current.clone()
        {
            found.push(path);
        }
    }
    Ok(found)
}

/// A failure to start a new attempt, as a sentence a person can read.
///
/// The detail (git's own words) goes to the log, never to the card: the card
/// tells the person what happened, that nothing was lost and what to do.
fn plain_failure(task: &str, what: &str, detail: &dyn std::fmt::Display) -> CliError {
    eprintln!("retry of {task}: {what}: {detail}");
    CliError::other(anyhow!(
        "Could not start a new attempt of {task}: {what}. \
         Nothing was lost, and the old attempt's commits are kept. \
         Try Retry again, and if it keeps failing, ask your Assistant to look at it."
    ))
}

/// True when an attempt at a task in `status` may be archived and replaced.
///
/// A failed attempt is finished and retryable; an integrated one is done. A
/// task that is queued has no attempt to replace.
fn replaceable(status: TaskStatus) -> bool {
    !matches!(
        status,
        TaskStatus::Working | TaskStatus::Reported | TaskStatus::Passed
    )
}

/// True when the ledger holds `name` as a `kind` that `action` may be recorded
/// against, which is what [`EventKind::Resource`] checks in `shprd_core::rules`.
///
/// A resource the ledger has no record of earns no event: the removal still
/// happens — the path has to go for the retry to work — but shprd cannot claim
/// to have closed something it never opened. The warnings below say so.
fn allows(state: &RunState, kind: ResourceKind, name: &str, action: ResourceAction) -> bool {
    let found = state
        .resources
        .get(&(kind, name.to_string()))
        .map(|record| record.status);
    let allowed = match action {
        ResourceAction::Created => matches!(found, None | Some(ResourceStatus::Removed)),
        ResourceAction::Archived => found == Some(ResourceStatus::Live),
        ResourceAction::Removed => {
            matches!(found, Some(ResourceStatus::Live | ResourceStatus::Archived))
        }
    };
    if !allowed {
        eprintln!(
            "warning: the ledger has no {} {kind:?} {name:?}, so replacing it is not recorded",
            match action {
                ResourceAction::Created => "new",
                ResourceAction::Archived | ResourceAction::Removed => "live",
            }
        );
    }
    allowed
}

/// True when `branch` exists in the repository this spawn runs in.
fn branch_exists(env: &SpawnEnv<'_>, branch: &str) -> Result<bool, CliError> {
    Git::new(&env.ctx.root)
        .branch_exists(branch)
        .map_err(|err| {
            CliError::other(anyhow!(err).context(format!("looking for the branch {branch:?}")))
        })
}

/// The ledger entries a spawn writes, in order: its two resources, then the
/// attempt itself.
fn attempt_events(
    task: &str,
    attempt: u32,
    pick: &Pick,
    created: &Worktree,
    own_worktree: bool,
) -> Vec<EventKind> {
    // A worktree and a branch are ledger resources the janitor may remove; the
    // repository's own checkout and branch are not, so they are never listed.
    let mut events = if own_worktree {
        resource_kinds(task, &created.path, created.branch.as_str()).to_vec()
    } else {
        Vec::new()
    };
    events.push(spawned_event(
        task,
        attempt,
        pick,
        &created.path,
        &created.branch,
    ));
    events
}

/// The `Spawned` event one attempt appends.
fn spawned_event(
    task: &str,
    attempt: u32,
    pick: &Pick,
    worktree: &Path,
    branch: &str,
) -> EventKind {
    EventKind::Spawned {
        task: task.to_string(),
        attempt,
        agent: pick.agent.clone(),
        model: pick.model.clone(),
        worktree: worktree.display().to_string(),
        branch: branch.to_string(),
    }
}

/// Appends `events` in order, returning the ones that landed when one fails.
///
/// The caller needs them: an event that landed and is not compensated would
/// leave the state holding a resource the spawn is about to take away.
fn append_in_order(
    events: impl IntoIterator<Item = EventKind>,
    mut commit: impl FnMut(EventKind) -> Result<(), CliError>,
) -> Result<(), (CliError, Vec<EventKind>)> {
    let mut landed: Vec<EventKind> = Vec::new();
    for event in events {
        if let Err(err) = commit(event.clone()) {
            return Err((err, landed));
        }
        landed.push(event);
    }
    Ok(())
}

/// Appends the `Removed` counterpart of every resource event a half-written
/// attempt already landed, so the run never keeps a live resource the caller is
/// about to delete.
fn compensate(env: &SpawnEnv<'_>, landed: &[EventKind]) {
    for event in landed {
        let EventKind::Resource {
            kind, name, task, ..
        } = event
        else {
            continue;
        };
        let removed = EventKind::Resource {
            action: ResourceAction::Removed,
            kind: *kind,
            name: name.clone(),
            task: task.clone(),
        };
        if let Err(err) = env.ctx.commit(removed) {
            eprintln!("warning: could not record that {name} is gone: {err}");
        }
    }
}

/// Appends the worktree, the branch and then the attempt itself.
///
/// Should any of the three be refused or fail, the resources that did land get
/// their `Removed` events before the error goes back to the caller, which takes
/// the worktree and the tab away.
fn record_attempt(
    env: &SpawnEnv<'_>,
    task: &str,
    attempt: u32,
    pick: &Pick,
    created: &Worktree,
    new_worktree: bool,
) -> Result<(), CliError> {
    let events = attempt_events(
        task,
        attempt,
        pick,
        created,
        uses_worktrees(env) && new_worktree,
    );
    match append_in_order(events, |event| env.ctx.commit(event).map(|_| ())) {
        Ok(()) => Ok(()),
        Err((err, landed)) => {
            compensate(env, &landed);
            Err(err)
        }
    }
}

/// The task `id` names, or an error saying the run plan has no such task.
pub(crate) fn task_of<'a>(state: &'a RunState, id: &str) -> Result<&'a Task, CliError> {
    state.tasks.get(id).ok_or_else(|| {
        CliError::other(anyhow!(
            "no task {id:?} in this run: add it with `shprd agent task add`"
        ))
    })
}

/// The harness table the agent strings are parsed against.
///
/// [`AgentRef::parse`] takes borrowed specs, so this copies the ones the
/// environment already holds rather than making the caller hand both a program
/// list and a spec list.
pub(crate) fn spec_table(programs: &[Program]) -> Vec<HarnessSpec> {
    programs
        .iter()
        .map(|program| program.spec.clone())
        .collect()
}

/// The program the parser named, or an error when the data directory has no
/// spec for it.
pub(crate) fn program_of<'a>(
    programs: &'a [Program],
    agent: &str,
) -> Result<&'a Program, CliError> {
    programs
        .iter()
        .find(|program| program.spec.id == agent)
        .ok_or_else(|| {
            CliError::other(anyhow!(
                "no harness spec for {agent:?} in the data directory"
            ))
        })
}

/// One sentence for the brief's Settings block: which of the worker's program
/// lines a task got, why, and where the task goes if that program can't go on.
///
/// [`None`] for a worker with one line or none, whose brief reads as it always
/// did.
fn program_line_note(
    lines: &[shprd_core::settings::WorkerLine],
    picked: &str,
    asked: bool,
) -> Option<String> {
    if lines.len() < 2 {
        return None;
    }
    let order: Vec<&str> = lines.iter().map(|line| line.agent.as_str()).collect();
    let Some(index) = order.iter().position(|agent| *agent == picked) else {
        return Some(format!(
            "none of the worker's lines ({}): `{picked}` was picked with --agent or from the              configured workers",
            order.join(", ")
        ));
    };
    let why = if asked {
        "asked for with --agent"
    } else if index == 0 {
        "the default: the first line"
    } else {
        "the lines before it have no CLI installed here"
    };
    let next = match order.get(index + 1) {
        Some(next) => format!(
            "if `{picked}` can't go on (provider error or out of quota) the task moves to `{next}`"
        ),
        None => format!("`{picked}` is the last line, so there is no further program to move to"),
    };
    Some(format!(
        "{} of {} (`{picked}`), {why}; lines in order: {}; {next}",
        index + 1,
        lines.len(),
        order.join(", ")
    ))
}

/// The agent strings a spawn tries, in order: every program line of the worker
/// role, then the configured workers.
///
/// A role's program that is not installed falls through to the configured
/// workers, exactly as an uninstalled worker does, so a machine without it
/// still starts the task.
fn role_candidates(role: Option<&ResolvedRole>, workers: &[String]) -> Vec<String> {
    let mut candidates: Vec<String> = role
        .map(|role| role.lines().iter().map(|line| line.agent_ref()).collect())
        .unwrap_or_default();
    candidates.extend(workers.iter().cloned());
    candidates
}

/// The agent strings the coordinator tries, in order: its own role's chain
/// (the program and its fallbacks) and nothing else.
///
/// The configured workers are another role's programs, so they are used only
/// when the role names no program at all. A coordinator whose program is
/// missing says so and is retried; it never becomes the worker's program
/// (SHPR-69).
fn coordinator_candidates(role: Option<&ResolvedRole>, workers: &[String]) -> Vec<String> {
    let own: Vec<String> = role
        .map(|role| role.lines().iter().map(|line| line.agent_ref()).collect())
        .unwrap_or_default();
    if own.is_empty() {
        workers.to_vec()
    } else {
        own
    }
}

/// Picks the agent for an attempt.
///
/// An explicit `--agent` wins even when its CLI is not installed, because the
/// human may be pointing at a wrapper script, but it must parse against the
/// harness table: a typo is refused here rather than after a worktree exists.
/// Without one, the first configured worker whose CLI *is* installed takes the
/// attempt, so an unattended run never opens an empty tab; a worker whose
/// string does not parse is skipped with a warning, the same way an uninstalled
/// one is.
fn choose_agent(
    explicit: Option<&str>,
    workers: &[String],
    programs: &[Program],
) -> Result<Pick, CliError> {
    let specs = spec_table(programs);
    if let Some(raw) = explicit {
        let parsed = AgentRef::parse(raw, &specs).map_err(|err| {
            CliError::Invalid(anyhow!(err).context(format!("`{raw}` is not a usable agent")))
        })?;
        let program = program_of(programs, &parsed.program)?;
        return Ok(Pick {
            agent: parsed.program,
            model: parsed.model,
            launch: program.spec.launch.clone(),
            blocking_prompts: program.spec.blocking_prompts.clone(),
            ready_lines: program.spec.ready.screen.clone(),
            own_status: !program.spec.status.needs_the_host(),
            status_words: program.spec.status.words(),
        });
    }
    for raw in workers {
        let parsed = match AgentRef::parse(raw, &specs) {
            Ok(parsed) => parsed,
            Err(err) => {
                eprintln!("warning: skipping configured worker `{raw}`: {err}");
                continue;
            }
        };
        let found = programs
            .iter()
            .find(|program| program.spec.id == parsed.program && program.installed);
        if let Some(program) = found {
            return Ok(Pick {
                agent: parsed.program,
                model: parsed.model,
                launch: program.spec.launch.clone(),
                blocking_prompts: program.spec.blocking_prompts.clone(),
                ready_lines: program.spec.ready.screen.clone(),
                own_status: !program.spec.status.needs_the_host(),
                status_words: program.spec.status.words(),
            });
        }
    }
    let configured: Vec<&str> = workers.iter().map(String::as_str).collect();
    Err(CliError::other(anyhow!(
        "no installed worker CLI: pass --agent, or set one with `shprd set workers` \
         (configured: {})",
        if configured.is_empty() {
            "none".to_string()
        } else {
            configured.join(", ")
        }
    )))
}

/// The skills an attempt must load.
///
/// That is every configured worker skill, plus every curated skill whose
/// verdict is `allow`, whose roles include the worker and whose tags intersect
/// the task's tags. A skill the compatibility list denies is never included,
/// even when the config names it, because the curated verdict is the maintainer's
/// decision about whether an unattended agent may run it at all.
pub fn select_skills(skills: &Skills, compat: &[CompatEntry], tags: &[String]) -> Vec<String> {
    let denied: Vec<&str> = compat
        .iter()
        .filter(|entry| entry.verdict == Verdict::Deny)
        .map(|entry| entry.name.as_str())
        .collect();
    let mut chosen: Vec<String> = Vec::new();
    let push = |name: &str, chosen: &mut Vec<String>| {
        if !denied.contains(&name) && !chosen.iter().any(|kept| kept == name) {
            chosen.push(name.to_string());
        }
    };
    for name in &skills.workers {
        push(name, &mut chosen);
    }
    for entry in compat {
        let tagged = entry.tags.iter().any(|tag| tags.contains(tag));
        let for_workers = entry.roles.contains(&SkillRole::Worker);
        if entry.verdict == Verdict::Allow && for_workers && tagged {
            push(&entry.name, &mut chosen);
        }
    }
    chosen
}

/// Where a brief file lives: `<briefs>/<run>/<task>-a<attempt>.md`.
pub(crate) fn brief_path(briefs: &Path, run: &str, task: &str, attempt: u32) -> PathBuf {
    briefs.join(run).join(format!("{task}-a{attempt}.md"))
}

/// Writes a brief file, creating its directory first.
pub(crate) fn write_brief(path: &Path, text: &str) -> Result<(), CliError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| {
            CliError::other(anyhow!(err).context(format!("creating {}", parent.display())))
        })?;
    }
    std::fs::write(path, text)
        .map_err(|err| CliError::other(anyhow!(err).context(format!("writing {}", path.display()))))
}

/// What a launch puts on the command line beyond the program's base command:
/// the access mode and the fast switch.
///
/// A launch applies one access mode. The role names it when the program declares
/// that mode; otherwise it is the program's full-access mode, which is the
/// auto-approval shprd has always started unattended agents with. Fast is on
/// only when the role asks for it and the program has the switch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Applied {
    /// The access mode id on the command line, when the program declares one.
    pub(crate) perm: Option<String>,
    /// Whether the program's fast switch is on.
    pub(crate) fast: bool,
}

/// Decides what a launch applies from the role's choice and the program's spec.
pub(crate) fn applied(spec: &HarnessSpec, perm: Option<&str>, fast: bool) -> Applied {
    let perm = perm
        .filter(|id| spec.permissions.mode(id).is_some())
        .map(str::to_owned)
        .or_else(|| spec.permissions.full_mode().map(|mode| mode.id.clone()));
    Applied {
        fast: fast && spec.fast.available(),
        perm,
    }
}

/// The role's words and the settings that shape one attempt, for a brief.
///
/// The Settings block has to name the layer that set each value, and the values
/// a spawn decides itself are the task's own: the program it launches, the model
/// and the effort it chose. Those are put into a task layer, but only where the
/// role names nothing, so a program the person set still reads as theirs. The
/// access mode and the fast switch are replaced by what the launch applies, so a
/// line never claims a mode or a tier the command line does not carry.
pub(crate) struct BriefContext {
    /// The role's standing instructions, read now.
    pub(crate) prompt: String,
    /// The settings as this task resolves them.
    pub(crate) settings: Resolved,
    /// The role the block describes.
    pub(crate) role: Option<ResolvedRole>,
    /// Whether the run asks every task for one proof, from `tasks.proof`.
    pub(crate) proof: bool,
}

/// The role a worker's brief is written for.
pub(crate) const BRIEF_ROLE: &str = "worker";

/// The name of the takeover words a handover brief opens with. It is not a role:
/// the newcomer runs on the worker's settings and reads this prompt (the
/// shipped prompt `backup`, kept under its old id so an edit stays).
pub(crate) const TAKEOVER_ROLE: &str = "takeover";

/// The role whose settings a launch under `role_id` runs on: the worker's for a
/// takeover, else the role itself.
pub(crate) fn settings_role_id(role_id: &str) -> &str {
    if role_id == TAKEOVER_ROLE {
        BRIEF_ROLE
    } else {
        role_id
    }
}

/// Reads the role's prompt and resolves the settings for one attempt.
///
/// A prompt that cannot be read is refused before the worktree, the tab and the
/// event: an agent started without its words would do the wrong work quickly.
pub(crate) fn brief_context(
    env: &SpawnEnv<'_>,
    role_id: &str,
    pick: &Pick,
    effort: Option<&str>,
    applied: &Applied,
) -> Result<BriefContext, CliError> {
    let prompt_id = if role_id == TAKEOVER_ROLE {
        "backup"
    } else {
        role_id
    };
    let role_id = settings_role_id(role_id);
    let base = env.settings.resolve(None);
    let mut task = Layer::new();
    if let Some(role) = base.role(role_id) {
        let key = |field: RoleField| Key::Role {
            id: role_id.to_owned(),
            field,
        };
        // The block must name what actually runs: a value the role left unset,
        // or set to a program this machine does not have, is the task's own.
        if named(role.agent.as_ref()) != Some(pick.agent.as_str()) {
            task.set(key(RoleField::Agent), Value::Text(pick.agent.clone()));
        }
        if named(role.model.as_ref()) != pick.model.as_deref()
            && let Some(model) = &pick.model
        {
            task.set(key(RoleField::Model), Value::Text(model.clone()));
        }
        if named(role.effort.as_ref()) != effort
            && let Some(effort) = effort
        {
            task.set(key(RoleField::Effort), Value::Text(effort.to_owned()));
        }
    }
    if uses_worktrees(env) {
        // The main checkout is a named, read-only place: what the repository
        // holds that a worktree does not (untracked or ignored files).
        let tasks = base.tasks();
        let mut notes = tasks.start_notes.trim().to_owned();
        if !notes.is_empty() {
            notes.push_str("\n\n");
        }
        notes.push_str(&format!(
            "The main checkout is at {} (read-only: read from it, never write to it); files \
             that are untracked or git-ignored there are not in your worktree. A linked file \
             points into it: to change one, replace the link with a copy first.",
            env.ctx.root.display()
        ));
        task.set(Key::Task(TaskKey::StartNotes), Value::Text(notes));
    }
    let settings = env.settings.resolve(Some(&task));
    let mut role = settings.role(role_id);
    if let Some(role) = role.as_mut() {
        // What the launch applies, not what the settings wanted: a mode the
        // program does not declare is the program's full-access one, and a role
        // that asks for fast on a program without the switch runs without it.
        let kept = role.perm.clone();
        role.perm = match (applied.perm.as_deref(), named(kept.as_ref())) {
            (Some(id), Some(named)) if id == named => kept,
            (Some(id), _) => Some(Sourced::new(id.to_owned(), Source::BuiltIn)),
            (None, _) => None,
        };
        if role.fast.value != applied.fast {
            role.fast = Sourced::new(applied.fast, Source::BuiltIn);
        }
    }
    let prompt = if prompt_id == role_id {
        role_prompt(env, role_id)?
    } else {
        // The takeover words (the installed prompt, not a role's `prompt`
        // field), then the worker's own words.
        let takeover = PromptRef::parse(&format!("{}{prompt_id}", PromptRef::PREFIX))
            .map_err(invalid)?
            .read(&env.prompts_dir, &env.home)
            .map_err(invalid)?;
        format!("{}\n{}", takeover.trim_end(), role_prompt(env, role_id)?)
    };
    let proof = settings.tasks().proof;
    Ok(BriefContext {
        prompt,
        settings,
        role,
        proof,
    })
}

/// One resolved role field's text, for comparing it with what a launch chose.
fn named(field: Option<&Sourced<String>>) -> Option<&str> {
    field.map(|field| field.value.as_str())
}

/// The standing words of `role_id`: the file its `prompt` names, read.
///
/// A role that names no prompt has no words to give, so the built-in prompt of
/// its own id is used; a custom role's `prompt:<id>` or path is read as it
/// says. A prompt that cannot be read is refused before anything is started.
pub(crate) fn role_prompt(env: &SpawnEnv<'_>, role_id: &str) -> Result<String, CliError> {
    let resolved = env.settings.resolve(None);
    let reference = resolved
        .role(role_id)
        .map(|role| role.prompt.value)
        .unwrap_or_else(|| format!("{}{role_id}", PromptRef::PREFIX));
    let reference = PromptRef::parse(&reference).map_err(invalid)?;
    reference.read(&env.prompts_dir, &env.home).map_err(invalid)
}

/// A prompt that cannot be read is the person's to fix, so it is a refusal.
fn invalid(error: shprd_core::prompts::PromptError) -> CliError {
    CliError::Invalid(anyhow!(error))
}

/// The brief a task's agent reads: the role's words, the task, the Settings
/// block.
///
/// The rendering lives in [`shprd_views::brief`] with the run's other views, so
/// the whole document can be snapshot-tested without a worktree; the name
/// `render_brief` stays here because that is what this module's callers use.
pub use shprd_views::brief::{AskRoute, BriefInput, task_brief as render_brief};

/// Builds the environment a spawn runs in from the process environment, the
/// config and the run log.
///
/// Everything that touches the machine is resolved here, once, so [`spawn`]
/// itself stays pure logic plus the worktree and the multiplexer.
pub fn build_env<'a>(ctx: &'a Ctx, home: Option<&Path>) -> Result<SpawnEnv<'a>, CliError> {
    let data = crate::cmd::roster::resolve_data_dir();
    build_env_in(ctx, home, &data)
}

/// [`build_env`], with the shipped data already resolved.
///
/// The data is the one thing a test cannot ask the environment for without
/// racing every other test in the process, so it is passed in here: a spawn
/// whose harness table is the copy embedded in the binary is built the same way
/// an installed `shprd` builds it.
pub fn build_env_in<'a>(
    ctx: &'a Ctx,
    home: Option<&Path>,
    data: &DataDir,
) -> Result<SpawnEnv<'a>, CliError> {
    build_env_with(ctx, home, data, true)
}

/// [`build_env_in`] for the coordinator, which belongs to the repository and not
/// to a run: with no run started the environment carries an empty run id and
/// base branch, which nothing but a task's spawn reads.
pub(crate) fn build_repo_env<'a>(
    ctx: &'a Ctx,
    home: Option<&Path>,
    data: &DataDir,
) -> Result<SpawnEnv<'a>, CliError> {
    build_env_with(ctx, home, data, false)
}

/// The one body of [`build_env_in`] and [`build_repo_env`].
fn build_env_with<'a>(
    ctx: &'a Ctx,
    home: Option<&Path>,
    data: &DataDir,
    need_run: bool,
) -> Result<SpawnEnv<'a>, CliError> {
    let home = crate::ctx::resolve_home(home)?;
    let specs = data.harness_specs().map_err(|err| {
        CliError::other(
            anyhow!(err).context(format!("loading the harness table from {}", data.label())),
        )
    })?;
    let path_env = std::env::var("PATH").unwrap_or_default();
    let programs: Vec<Program> = specs
        .iter()
        .map(|spec| Program {
            installed: shprd_agents::detect::find_binary(&spec.detect.binary, &path_env).is_some(),
            spec: spec.clone(),
        })
        .collect();
    let global = home.join(".config").join("shprd").join("config.toml");
    let repo_config = ctx.root.join(".shprd").join("config.toml");
    let config = shprd_core::config::load(Some(&global), Some(&repo_config))
        .map_err(|err| CliError::other(anyhow!(err).context("loading the shprd config")))?;
    let compat = read_compat(data)?;
    let settings = Loaded::read(&home, Some(&ctx.root))
        .map_err(|err| CliError::other(anyhow!(err).context("loading the settings")))?;
    let state = ctx.state()?;
    let (run_id, base_branch) = match state.run.as_ref() {
        Some(run) => (run.run_id.clone(), run.integration_branch.clone()),
        None if need_run => {
            return Err(CliError::other(anyhow!(
                "no run yet: start one with `shprd run start`"
            )));
        }
        None => (String::new(), String::new()),
    };
    let repo_name = repo_name(&ctx.root)?;
    let base_env: BTreeMap<String, String> = std::env::vars().collect();
    // The roots the agent CLIs write under are read from the same environment
    // they will inherit, so a launch that carries `HOME` or `$XDG_DATA_HOME`
    // reads its own records and not the service's.
    let transcript_roots = shprd_agents::transcript::Roots::for_launch(&base_env);
    Ok(SpawnEnv {
        ctx,
        layout: Layout::from_settings(&home, &ctx.root),
        briefs_dir: home.join(".shprd").join("briefs"),
        prompts_dir: prompts::prompts_dir(&crate::ctx::resolve_config_home(&home)),
        home,
        settings,
        run: run_id,
        base_branch,
        repo_name,
        programs,
        workers: config.roster.workers.clone(),
        skills: config.skills.clone(),
        gates: config.gates.worker.clone(),
        catalogs: BTreeMap::new(),
        data_dir: data.clone(),
        compat,
        base_env,
        ready_wait: super::deliver::ready_wait(),
        transcript_roots,
    })
}

/// Reads the curated compatibility list, treating a missing file as an empty
/// list: a checkout without `data/skills` still spawns, it just adds no
/// tag-selected skills.
fn read_compat(data: &DataDir) -> Result<Vec<CompatEntry>, CliError> {
    let rel = "skills/compat.toml";
    if !data.contains(rel) {
        return Ok(Vec::new());
    }
    let list = data.compat().map_err(|err| {
        CliError::other(anyhow!(err).context(format!("loading {}", data.path(rel).display())))
    })?;
    Ok(list.skills)
}

/// The host the binary drives, chosen at the wiring edge.
///
/// Which one it is — Herdr desktop, the herdr TUI or tuios — is
/// [`crate::host::select`]'s decision, from the `host` setting and what this
/// machine has (M10.9). Every verb reaches a host through here, so nothing in
/// `cmd/` has to know which adapter it is holding: the trait's verbs are the
/// same, and the capability flags are what says which of them a host has.
pub fn real_mux() -> Box<dyn Host> {
    crate::host::select().1
}

/// Sends one turn of input to the tab a task's agent runs in.
///
/// Only the tab the active run's log says this task's latest attempt opened is
/// reachable, so a prompt can never land in another run's agent.
pub fn prompt(mux: &dyn Host, repo: &Path, task: &str, text: &str) -> Result<(), CliError> {
    let home = crate::ctx::resolve_home(None)?;
    // Reading the log needs the root; the actor and the flags only matter to a
    // verb that writes.
    let ctx = Ctx::new(repo.to_path_buf(), Actor::Human, false, true);
    prompt_in(mux, &home, &ctx, task, text)
}

/// [`prompt`], against an explicit home directory, so a test needs no
/// environment.
pub(crate) fn prompt_in(
    mux: &dyn Host,
    home: &Path,
    ctx: &Ctx,
    task: &str,
    text: &str,
) -> Result<(), CliError> {
    let pane = task_pane(mux, home, ctx, task)?;
    mux.write(&pane, text, true)
        .map_err(|err| CliError::other(anyhow!(err).context("prompting the agent")))
}

/// Types the latest attempt's brief into the agent's terminal again.
///
/// It is the resend the lost-brief warning names — `shprd agent prompt --task T
/// "$(cat <brief>)"` — as one call, for the board's Resend button.
pub(crate) fn resend_brief(
    mux: &dyn Host,
    home: &Path,
    ctx: &Ctx,
    task: &str,
) -> Result<String, CliError> {
    let run = active_run(ctx)?;
    let briefs = home.join(".shprd").join("briefs");
    let record = latest_tab_record(&briefs, &run, task).ok_or_else(|| no_open_tab(task))?;
    let brief = record.with_extension("md");
    let text = std::fs::read_to_string(&brief).map_err(|err| {
        CliError::other(anyhow!(err).context(format!("reading {}", brief.display())))
    })?;
    prompt_in(mux, home, ctx, task, &text)?;
    Ok(format!("brief delivered to {task}"))
}

/// The pane a task's agent runs in, resolved by the one path a prompt, a chat
/// line and the terminal all share.
pub(crate) fn task_pane(
    mux: &dyn Host,
    home: &Path,
    ctx: &Ctx,
    task: &str,
) -> Result<PaneId, CliError> {
    let run = active_run(ctx)?;
    // The record names the handle the attempt's agent was started with, looked
    // up among everything the host has (tabs and headless agents alike), so a
    // prompt can only reach the agent that attempt started.
    let record = latest_tab_record(&home.join(".shprd").join("briefs"), &run, task)
        .ok_or_else(|| no_open_tab(task))?;
    let wanted = std::fs::read_to_string(&record).map_err(|err| {
        CliError::other(anyhow!(err).context(format!("reading {}", record.display())))
    })?;
    all_handles(mux)
        .into_iter()
        .find(|tab| tab.id.as_str() == wanted.trim())
        .map(|tab| tab.root_pane)
        .ok_or_else(|| no_open_tab(task))
}

/// Every tab and every headless agent the host has, as the handles an attempt's
/// `.tab` record can name.
///
/// This is the one lookup the prompt, the chat, the watchdog, a hand-off and the
/// live set share: a host with headless agents lists them by label, a host that
/// keeps tabs lists the tabs of each space, and a caller never has to know which
/// one it is holding. A host that cannot be asked answers fewer handles, which
/// every caller reads as "not there" (the safe direction).
pub(crate) fn all_handles(mux: &dyn Host) -> Vec<shprd_mux::Tab> {
    let mut handles: Vec<shprd_mux::Tab> = mux
        .list_spaces()
        .map(|spaces| {
            spaces
                .iter()
                .flat_map(|space| mux.list_tabs(&space.id).unwrap_or_default())
                .collect()
        })
        .unwrap_or_default();
    handles.extend(
        mux.list_agents()
            .unwrap_or_default()
            .into_iter()
            .map(|agent| agent.tab),
    );
    handles
}

/// The directory the coordinator records live under: `<home>/.shprd/coordinators`.
const COORDINATORS_DIR: &str = "coordinators";

/// The older, machine-wide record: `<home>/.shprd/coordinator.pane`.
///
/// One file per machine meant a second `shprd serve` (another repository, a
/// test run) overwrote the first service's record. It is read only to migrate
/// ([`coordinator_record`]) and is never written or deleted.
fn legacy_coordinator_path(home: &Path) -> PathBuf {
    home.join(".shprd").join("coordinator.pane")
}

/// A name that is safe as one path component: ASCII letters, digits, `-` and
/// `_` stay, anything else becomes `_`. A name that had to change also gets a
/// short digest of the original, so two different names never share a path
/// (`a/b` and `a_b` stay apart).
fn path_safe(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let safe: String = text
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if safe == text {
        return safe;
    }
    let digest = Sha256::digest(text.as_bytes());
    let short: String = digest.iter().take(4).map(|b| format!("{b:02x}")).collect();
    format!("{safe}-{short}")
}

/// Where the pane of the coordinator shprd started is written.
///
/// It is `<home>/.shprd/coordinators/<repo-key>/<space-id>`, beside the tab
/// records. The repo key is the repository's folder name and a short digest of
/// its canonical path, so two repositories with the same folder name still get
/// their own directory; the file is named by the host's id of the repository's
/// space, so a second space on the same repository does not share it either.
/// This follows how the rest of shprd scopes by repository (one space per
/// repository, labelled from its folder) while keeping the record under the one
/// `--home` a service is given, where its other records already live. Three
/// lines: the host's own pane id, then the word `agent`, which says shprd
/// started an agent there, then the program. A record without the second line
/// names a pane shprd did not start (an older build recorded the space's
/// starter shell), and is never typed into.
pub(crate) fn coordinator_pane_path(
    home: &Path,
    repo: &Path,
    space: &shprd_mux::SpaceId,
) -> PathBuf {
    coordinator_dir(home, repo).join(path_safe(space.as_str()))
}

/// The folder one repository's coordinator records live in, one file per space.
fn coordinator_dir(home: &Path, repo: &Path) -> PathBuf {
    use sha2::{Digest, Sha256};
    let canonical = repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf());
    let digest = Sha256::digest(canonical.to_string_lossy().as_bytes());
    let short: String = digest.iter().take(6).map(|b| format!("{b:02x}")).collect();
    let name = canonical
        .file_name()
        .map(|name| path_safe(&name.to_string_lossy()))
        .unwrap_or_default();
    home.join(".shprd")
        .join(COORDINATORS_DIR)
        .join(format!("{name}-{short}"))
}

/// The line that marks a recorded pane as an agent shprd started.
const COORDINATOR_AGENT_MARK: &str = "agent";

/// Records the pane of the coordinator agent shprd started, for the
/// repository's `space`. No other repository's or space's record is touched.
pub(crate) fn record_coordinator_pane(
    home: &Path,
    repo: &Path,
    space: &shprd_mux::SpaceId,
    pane: &PaneId,
    program: &str,
) -> Result<(), CliError> {
    let path = coordinator_pane_path(home, repo, space);
    write_record(
        &path,
        &format!("{}\n{COORDINATOR_AGENT_MARK}\n{program}\n", pane.as_str()),
    )
}

fn write_record(path: &Path, text: &str) -> Result<(), CliError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|err| {
            CliError::other(anyhow!(err).context(format!("making {}", dir.display())))
        })?;
    }
    std::fs::write(path, text)
        .map_err(|err| CliError::other(anyhow!(err).context(format!("writing {}", path.display()))))
}

/// What a coordinator record says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CoordinatorRecord {
    /// The host's pane id.
    pub pane: PaneId,
    /// Whether the record carries the `agent` mark.
    pub agent: bool,
    /// The program the coordinator runs, when recorded.
    pub program: Option<String>,
}

fn parse_record(text: &str) -> Option<CoordinatorRecord> {
    let mut lines = text.lines().map(str::trim);
    let pane = lines.next().filter(|pane| !pane.is_empty())?;
    let agent = lines.next() == Some(COORDINATOR_AGENT_MARK);
    let program = lines
        .next()
        .filter(|program| !program.is_empty())
        .map(str::to_string);
    Some(CoordinatorRecord {
        pane: PaneId::new(pane),
        agent,
        program,
    })
}

/// The record of the coordinator for this repository's `space`.
///
/// The repository's own record wins. When there is none, the old machine-wide
/// file is read once for migration, and only if its pane is a tab of this
/// service's own space: then it is copied into the repository's record. A pane
/// the space does not have belongs to another service (or is stale) and is
/// ignored. The old file is never deleted: it may be another service's.
pub(crate) fn coordinator_record(
    home: &Path,
    repo: &Path,
    mux: &dyn Host,
    space: &shprd_mux::Space,
) -> Option<CoordinatorRecord> {
    let path = coordinator_pane_path(home, repo, &space.id);
    if let Ok(text) = std::fs::read_to_string(&path) {
        return parse_record(&text);
    }
    let text = std::fs::read_to_string(legacy_coordinator_path(home)).ok()?;
    let record = parse_record(&text)?;
    let alive_here = mux
        .list_tabs(&space.id)
        .ok()?
        .into_iter()
        .any(|tab| tab.root_pane == record.pane);
    if !alive_here {
        return None;
    }
    if let Err(err) = write_record(&path, &text) {
        tracing::warn!(%err, "the old coordinator record could not be migrated");
    }
    Some(record)
}

/// The coordinator running in `space`, for an agent to name as its parent.
pub(crate) fn coordinator_pane_of(
    home: &Path,
    repo: &Path,
    mux: &dyn Host,
    space: &shprd_mux::Space,
) -> Option<PaneId> {
    recorded_coordinator_pane(home, repo, mux, space)
}

/// A coordinator shprd can type into, as it was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FoundCoordinator {
    /// The host's pane id.
    pub(crate) pane: PaneId,
    /// The program it runs, when known.
    pub(crate) program: String,
    /// When its terminal started, in epoch milliseconds, when the host says.
    pub(crate) started_ms: Option<u64>,
}

/// The ids a repository's agents carry as their space: the space shprd keeps
/// for it and any other the host has open on the same folder (an older shprd's,
/// or one a person opened), because one repository is one space and an agent
/// started for either belongs to it. An agent whose space is in none of these is
/// not this repository's.
pub(crate) fn repo_space_ids(mux: &dyn Host, repo: &Path, space: &shprd_mux::Space) -> Vec<String> {
    let mut ids = vec![bare_id(space.id.as_str())];
    for other in mux.spaces_in_folder(repo).unwrap_or_default() {
        let id = bare_id(other.id.as_str());
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

/// A host id without a machine prefix.
fn bare_id(id: &str) -> String {
    id.rsplit('/').next().unwrap_or(id).to_string()
}

/// The coordinators of this repository that are running, newest last.
fn running_coordinators(
    mux: &dyn Host,
    repo: &Path,
    space: &shprd_mux::Space,
) -> Vec<shprd_mux::LiveAgent> {
    let ids = repo_space_ids(mux, repo, space);
    let mut found: Vec<shprd_mux::LiveAgent> = mux
        .list_coordinators()
        .unwrap_or_default()
        .into_iter()
        .filter(|agent| {
            agent.role == shprd_mux::AgentRole::Coordinator
                && agent.live
                && ids.contains(&bare_id(agent.space.as_str()))
        })
        .collect();
    found.sort_by_key(|agent| agent.started_ms);
    found
}

/// What stopping a closed space's agents did, for the caller's one line of log.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Retired {
    /// The coordinator and task agents the host was told to stop.
    pub(crate) stopped: usize,
    /// Agents the host kept, by pane id.
    pub(crate) kept: Vec<String>,
    /// Task tabs the host kept, by id (they never hold a coordinator record back).
    pub(crate) tabs_kept: Vec<String>,
}

/// The agents of the spaces that are gone: those the host lists, still running,
/// whose space is one of the `recorded` spaces of the repository and not one of
/// the `open` ones. Each pane once.
fn agents_of_closed_spaces(
    recorded: &[String],
    open: &[String],
    agents: Vec<shprd_mux::LiveAgent>,
) -> Vec<shprd_mux::LiveAgent> {
    let mut seen: Vec<PaneId> = Vec::new();
    let mut found = Vec::new();
    for agent in agents {
        let names = [
            path_safe(agent.space.as_str()),
            path_safe(&bare_id(agent.space.as_str())),
        ];
        let of_repo = names.iter().any(|name| recorded.contains(name));
        let still_open = names.iter().any(|name| open.contains(name));
        if !agent.live || !of_repo || still_open || seen.contains(&agent.tab.root_pane) {
            continue;
        }
        seen.push(agent.tab.root_pane.clone());
        found.push(agent);
    }
    found
}

/// Stops the agents shprd started for a repository whose space was closed: its
/// coordinator, and every worker and reviewer of its run.
///
/// The space is gone, so the agents are found the way they were recorded: the
/// coordinator records name the repository's spaces (`<home>/.shprd/coordinators/
/// <repo>/<space>`), and every agent the host lists for one of those spaces that
/// the host no longer has open is stopped. A host that cannot say which spaces
/// are open stops nothing, and a space still open for the folder (a folder added
/// again before this ran) keeps its agents and the run's tabs. Tasks, briefs and
/// the run log stay as they are, so a folder added again resumes the run with a
/// fresh coordinator. The records of the closed spaces are dropped: they name
/// panes that no longer run.
pub(crate) fn stop_repo_agents(mux: &dyn Host, ctx: &Ctx, home: &Path, repo: &Path) -> Retired {
    let mut retired = Retired::default();
    // Fail closed: an agent list or a list of open spaces the host could not give
    // is not an empty one, and nothing is known to be closed without them.
    let (Ok(mut agents), Ok(listed)) = (mux.list_coordinators(), mux.list_agents()) else {
        return retired;
    };
    agents.extend(listed);
    // Asked after the agents were listed, so a space opened in between (a folder
    // added again) is seen before anything of it is killed.
    let Ok(spaces) = mux.list_spaces() else {
        return retired;
    };
    let open: Vec<String> = spaces
        .iter()
        .flat_map(|space| {
            [
                path_safe(&bare_id(space.id.as_str())),
                path_safe(space.id.as_str()),
            ]
        })
        .collect();
    let reshown = mux
        .spaces_in_folder(repo)
        .map(|found| !found.is_empty())
        .unwrap_or(true);
    let records: Vec<PathBuf> = std::fs::read_dir(coordinator_dir(home, repo))
        .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
        .unwrap_or_default();
    let recorded: Vec<String> = records
        .iter()
        .filter_map(|path| path.file_name())
        .map(|name| name.to_string_lossy().to_string())
        .collect();
    // The run's own tabs belong to whichever space shows the folder now.
    if !reshown {
        retired.tabs_kept.extend(
            close_run_tabs(mux, ctx, Some(home))
                .into_iter()
                .map(|left| left.id),
        );
    }
    for agent in agents_of_closed_spaces(&recorded, &open, agents) {
        match mux.kill(&agent.tab.root_pane) {
            Ok(()) => retired.stopped += 1,
            Err(shprd_mux::MuxError::NotFound { .. }) => {}
            Err(err) => {
                tracing::warn!(pane = %agent.tab.root_pane.as_str(), %err, "an agent of a closed space could not be stopped");
                retired.kept.push(agent.tab.root_pane.as_str().to_string());
            }
        }
    }
    if retired.kept.is_empty() {
        for record in records {
            let closed = record
                .file_name()
                .is_some_and(|name| !open.contains(&name.to_string_lossy().to_string()));
            if closed {
                let _ = std::fs::remove_file(record);
            }
        }
    }
    retired
}

/// Ends every running coordinator of this repository except `keep`, and lets go
/// of the finished ones the host still keeps, so the space holds one.
///
/// Only coordinators started for this repository's space are touched, and only
/// the host's own stop is used; a failure is a warning, because a leftover is
/// better than a service that will not start.
pub(crate) fn end_other_coordinators(
    mux: &dyn Host,
    repo: &Path,
    space: &shprd_mux::Space,
    keep: Option<&PaneId>,
) {
    let ids = repo_space_ids(mux, repo, space);
    for agent in mux.list_coordinators().unwrap_or_default() {
        if agent.role != shprd_mux::AgentRole::Coordinator
            || !ids.contains(&bare_id(agent.space.as_str()))
            || keep.is_some_and(|keep| *keep == agent.tab.root_pane)
        {
            continue;
        }
        if let Err(err) = mux.kill(&agent.tab.root_pane) {
            tracing::warn!(pane = %agent.tab.root_pane.as_str(), %err, "an old coordinator could not be ended");
        }
    }
}

/// The coordinator agent shprd can type into, when one is running in `space`.
///
/// News and chat lines are typed into this pane and no other, so it is trusted
/// only while all three hold: the record carries the `agent` mark (shprd
/// started a program there, not a shell), the pane is still one of the space's
/// tabs, and the host does not say its program has ended. A person who closed
/// the tab, or whose coordinator exited, gets a refusal and not a line typed
/// into whatever is left.
///
/// When the record names no such pane (the service was restarted for a space
/// that was found by another route, or the record belongs to an earlier
/// space), the host's own list of running coordinators for this repository is
/// asked, and the newest one is taken and recorded: a coordinator that is
/// running is found, whatever wrote it down.
pub(crate) fn find_coordinator(
    home: &Path,
    repo: &Path,
    mux: &dyn Host,
    space: &shprd_mux::Space,
) -> Option<FoundCoordinator> {
    if let Some(found) = recorded_coordinator(home, repo, mux, space) {
        return Some(found);
    }
    let adopted = running_coordinators(mux, repo, space).pop()?;
    let pane = adopted.tab.root_pane.clone();
    if let Err(err) = record_coordinator_pane(home, repo, &space.id, &pane, &adopted.program) {
        tracing::warn!(%err, "a running coordinator could not be recorded");
    }
    Some(FoundCoordinator {
        pane,
        program: adopted.program,
        started_ms: adopted.started_ms,
    })
}

/// [`find_coordinator`]'s pane.
pub(crate) fn recorded_coordinator_pane(
    home: &Path,
    repo: &Path,
    mux: &dyn Host,
    space: &shprd_mux::Space,
) -> Option<PaneId> {
    find_coordinator(home, repo, mux, space).map(|found| found.pane)
}

/// The coordinator the record names, when it is still a running tab of the
/// space.
fn recorded_coordinator(
    home: &Path,
    repo: &Path,
    mux: &dyn Host,
    space: &shprd_mux::Space,
) -> Option<FoundCoordinator> {
    let record = coordinator_record(home, repo, mux, space)?;
    if !record.agent || mux.is_foreign_agent(&record.pane) {
        return None;
    }
    let pane = mux
        .list_tabs(&space.id)
        .ok()?
        .into_iter()
        .map(|tab| tab.root_pane)
        .find(|pane| *pane == record.pane)?;
    match mux.agent_state(&pane) {
        Ok(AgentState::Done) | Err(_) => return None,
        Ok(_) => {}
    }
    let started_ms = mux
        .list_coordinators()
        .unwrap_or_default()
        .into_iter()
        .find(|agent| agent.tab.root_pane == pane)
        .and_then(|agent| agent.started_ms);
    Some(FoundCoordinator {
        pane,
        program: record.program.unwrap_or_default(),
        started_ms,
    })
}

/// How `shprd serve` found the coordinator in the repository's space.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CoordinatorStart {
    /// A coordinator agent shprd started earlier is still running: reused.
    Running(PaneId),
    /// shprd started one now.
    Started(PaneId),
}

impl CoordinatorStart {
    /// The coordinator's pane.
    pub(crate) fn pane(&self) -> &PaneId {
        match self {
            Self::Running(pane) | Self::Started(pane) => pane,
        }
    }
}

/// Starts the coordinator agent in the repository's space, unless one is
/// already running there.
///
/// The program, model and effort are the `coordinator` role's settings; the
/// words it is given are the role's prompt (`assistant`). It is the one agent
/// that keeps a tab, because it is what a person talks to in the space. A
/// restart of `shprd serve` reuses a running coordinator and starts a new one
/// when the recorded one has ended. When the space was just made, the starter
/// shell the host put in it is closed once the agent runs, so the space holds
/// the coordinator and nothing a line could be typed into by mistake.
pub(crate) fn ensure_coordinator(
    mux: &dyn Host,
    env: &SpawnEnv<'_>,
    prepared: &PreparedSpace,
    defer: impl FnOnce(PendingPrompt),
) -> Result<CoordinatorStart, CliError> {
    let space = &prepared.space;
    // What older shprd left behind goes first: a coordinator in a bare terminal
    // tab is replaced by the chat tab, never kept beside it.
    tidy_old_coordinator_tabs(mux, space);
    remember_window_tabs(mux, env, prepared);
    // A coordinator that is running is reused and every other one this
    // repository's earlier starts left behind is ended: a restart never adds
    // one, and the Agents list does not fill up with them.
    if let Some(found) = find_coordinator(&env.home, &env.ctx.root, mux, space) {
        end_other_coordinators(mux, &env.ctx.root, space, Some(&found.pane));
        tidy_window_tabs(mux, env, space, None);
        return Ok(CoordinatorStart::Running(found.pane));
    }
    end_other_coordinators(mux, &env.ctx.root, space, None);
    // No coordinator is running, so a tab of ours that still holds a finished
    // one is cleared: the new agent takes the tab's place, not a second pane.
    close_own_tabs(mux, space, &[COORDINATOR_TAB]);
    let role = env.settings.resolve(None).role(COORDINATOR_ROLE);
    let title = assistant_title(mux, space, &coordinator_name(role.as_ref()));
    let candidates = coordinator_candidates(role.as_ref(), &env.workers);
    let mut pick = choose_agent(None, &candidates, &env.programs).map_err(|err| {
        CliError::other(anyhow!(
            "the coordinator could not start: none of its programs is installed here ({}); \
             install one or change `roles.coordinator` in the settings: {err}",
            if candidates.is_empty() {
                "none named".to_owned()
            } else {
                candidates.join(", ")
            }
        ))
    })?;
    // The settings that go with the program that was picked: its own line's
    // model, effort, fast mode and access mode. A fallback program never gets
    // another program's model; a role that names no program at all has only its
    // plain settings to give.
    let lines = role.as_ref().map(ResolvedRole::lines).unwrap_or_default();
    let line = lines.iter().find(|line| line.agent == pick.agent);
    let plain = |field: Option<&shprd_core::settings::Sourced<String>>| {
        field
            .map(|sourced| sourced.value.clone())
            .filter(|text| !text.trim().is_empty())
    };
    if pick.model.is_none() {
        pick.model = match line {
            Some(line) => line.model.clone(),
            None if lines.is_empty() => plain(role.as_ref().and_then(|role| role.model.as_ref())),
            None => None,
        };
    }
    let effort = match line {
        Some(line) => line.effort.clone(),
        None if lines.is_empty() => plain(role.as_ref().and_then(|role| role.effort.as_ref())),
        None => None,
    }
    .filter(|effort| !effort.trim().is_empty());
    let program = program_of(env.programs.as_slice(), &pick.agent)?;
    let chosen = match line {
        Some(line) => applied(&program.spec, line.perm.as_deref(), line.fast),
        None if lines.is_empty() => applied(
            &program.spec,
            named(role.as_ref().and_then(|role| role.perm.as_ref())),
            role.as_ref().is_some_and(|role| role.fast.value),
        ),
        None => applied(&program.spec, None, false),
    };
    let prompt = role_prompt(env, COORDINATOR_ROLE)?;
    let brief_path = env.briefs_dir.join("coordinator.md");
    write_brief(&brief_path, &prompt)?;
    let (mut argv, brief_in_args) = launch_argv(
        &pick.launch,
        &program.spec,
        &chosen,
        pick.model.as_deref(),
        effort.as_deref(),
        &prompt,
        &brief_path,
    )?;
    // The repository is the person's own, so no first-run dialog asks about it;
    // and the coordinator plans the work, it never edits it.
    trust_launch_folder(&pick.agent, &env.ctx.root, &env.base_env, &mut argv);
    let guard = folder_trust::coordinator_guard(&pick.agent);
    argv.extend(guard.args);
    let mut pairs = env_pairs(launch_env(
        &env.base_env,
        EnvRole::Orchestrator,
        &EnvContext {
            actor: format!("agent:{}", pick.agent),
            task: None,
            run: None,
            repo: env.ctx.root.clone(),
            home: Some(env.home.clone()),
            bin: shprd_agents::env::service_bin(),
            data: env.data_dir.env_value().map(Path::to_path_buf),
        },
    ));
    pairs.extend(
        guard
            .env
            .into_iter()
            .map(|(name, value)| (name, Some(value))),
    );
    let tab = mux
        .spawn_agent(&shprd_mux::AgentLaunch {
            space: &space.id,
            title: &title,
            argv: &argv,
            env: &pairs,
            cwd: &env.ctx.root,
            role: shprd_mux::AgentRole::Coordinator,
            task: None,
            program: &pick.agent,
            parent: None,
        })
        .map_err(|err| CliError::other(anyhow!(err).context("starting the coordinator")))?;
    // A program that exits at once (a model it does not know, a flag it
    // refuses) is no coordinator: it is cleared and the reason is the start's
    // error, so the banner and the chat say it and the next pass tries again.
    if let Some(reason) = exited_at_start(mux, &tab.root_pane, env.ready_wait) {
        close_tab(mux, &tab.id);
        return Err(CliError::other(anyhow!(
            "the coordinator's program `{}` exited as it started ({reason})",
            pick.agent
        )));
    }
    // A coordinator nobody can find again is one a restart would start a second
    // of, so a record that cannot be written stops the one just started.
    if let Err(err) = record_coordinator_pane(
        &env.home,
        &env.ctx.root,
        &space.id,
        &tab.root_pane,
        &pick.agent,
    ) {
        close_tab(mux, &tab.id);
        return Err(err);
    }
    tidy_window_tabs(mux, env, space, Some(&tab.id));
    // The tab shows the coordinator as a chat with the terminal one switch
    // away, as Shepherdr's Assistant does. A host with no window keeps the
    // plain terminal tab, and so does a window that cannot do it: the agent
    // runs and takes news either way.
    match mux.show_chat(&tab.root_pane, "coordinator") {
        Ok(()) | Err(shprd_mux::MuxError::Unsupported(_)) => {}
        Err(err) => eprintln!("warning: the coordinator's chat did not open: {err}"),
    }
    // A program with no system-prompt flag is given its words as its first turn,
    // once it is ready; one that took them as an argument is left to wait. The
    // wait for a program to become ready can be long, so it is the caller's to
    // run beside the service rather than in front of it.
    if !brief_in_args {
        defer(PendingPrompt {
            pane: tab.root_pane.clone(),
            text: prompt,
            pick,
            wait: env.ready_wait,
        });
    }
    Ok(CoordinatorStart::Started(tab.root_pane))
}

/// Where the tabs the window put in a space are remembered until the
/// coordinator runs: beside the coordinator's own record.
fn window_tabs_path(home: &Path, repo: &Path, space: &shprd_mux::SpaceId) -> PathBuf {
    let mut path = coordinator_pane_path(home, repo, space).into_os_string();
    path.push(".window-tabs");
    PathBuf::from(path)
}

/// Remembers, once, which tabs the window made in a space shprd has just made or
/// adopted for the first time: its default shell tab and another product's empty
/// "Assistant" tab. A coordinator that cannot start at first (and starts on a
/// retry) must still end with the assistant alone in the center, and a tab the
/// person opens meanwhile is never in this list.
///
/// A space adopted rather than made is only tidied when it is the window's plain
/// one and holds a single idle shell tab: a person's own tabs are never listed.
fn remember_window_tabs(mux: &dyn Host, env: &SpawnEnv<'_>, prepared: &PreparedSpace) {
    if !(prepared.created || prepared.fresh) {
        return;
    }
    let space = &prepared.space;
    let path = window_tabs_path(&env.home, &env.ctx.root, &space.id);
    if path.exists() {
        return;
    }
    // A coordinator already running here is never the window's tab.
    let coordinators: Vec<_> = mux
        .list_coordinators()
        .unwrap_or_default()
        .into_iter()
        .map(|agent| agent.tab.id)
        .collect();
    // A host that cannot list the tabs now is asked again at the next start:
    // an empty record would forget the window's tabs for good.
    let Ok(listed) = mux.list_tabs(&space.id) else {
        return;
    };
    let Ok(named) = mux.tab_names(&space.id) else {
        return;
    };
    // Only a tab no program runs in can be the other product's empty
    // "Assistant": a tab with a terminal that a person titled so is theirs.
    let program_tabs: Vec<_> = listed.iter().map(|tab| tab.id.clone()).collect();
    let shells: Vec<_> = listed
        .into_iter()
        .filter(|tab| {
            !tab.title.trim().eq_ignore_ascii_case(ASSISTANT_TITLE)
                && !is_coordinator_tab(tab.id.as_str())
                && !coordinators.contains(&tab.id)
        })
        .collect();
    let idle = shells.iter().all(|tab| {
        !matches!(
            mux.agent_state(&tab.root_pane),
            Ok(shprd_mux::AgentState::Working | shprd_mux::AgentState::Blocked)
        )
    });
    let mut ids: Vec<String> = Vec::new();
    if prepared.created || (shells.len() == 1 && idle) {
        ids.extend(shells.iter().map(|tab| tab.id.as_str().to_owned()));
    }
    // An Assistant tab another product's agent chats in is that product's, used
    // or not: it is never listed, so it is never closed. (An Assistant tab no
    // agent claims holds nothing and is the empty one the window made.)
    let claimed: Vec<String> = mux
        .foreign_agent_tabs()
        .iter()
        .map(|tab| bare_id(tab.as_str()))
        .collect();
    for (id, name) in named {
        if name.trim().eq_ignore_ascii_case(ASSISTANT_TITLE)
            && !claimed.contains(&bare_id(id.as_str()))
            && !is_coordinator_tab(id.as_str())
            && id.as_str() != OLD_COORDINATOR_TAB
            && !coordinators.contains(&id)
            && !program_tabs.contains(&id)
        {
            ids.push(id.as_str().to_owned());
        }
    }
    // Written even when empty, so a later start does not look again.
    if let Err(err) = write_record(&path, &ids.join("\n")) {
        tracing::debug!(%err, "the window's tabs were not remembered");
    }
}

/// Closes the tabs [`remember_window_tabs`] listed, now that the coordinator
/// runs, and shows the coordinator. `keep` is the coordinator's own tab, which
/// is never closed.
fn tidy_window_tabs(
    mux: &dyn Host,
    env: &SpawnEnv<'_>,
    space: &shprd_mux::Space,
    keep: Option<&shprd_mux::TabId>,
) {
    let path = window_tabs_path(&env.home, &env.ctx.root, &space.id);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    // A tab that could not be closed stays on the record for the next start.
    let mut left: Vec<&str> = Vec::new();
    for id in text.lines().map(str::trim).filter(|id| !id.is_empty()) {
        if keep.is_some_and(|keep| keep.as_str() == id) || is_coordinator_tab(id) {
            continue;
        }
        match mux.close_tab(&shprd_mux::TabId::new(id)) {
            Ok(()) | Err(shprd_mux::MuxError::NotFound { .. }) => {}
            Err(err) => {
                tracing::debug!(%err, tab = id, "a window tab was not closed");
                left.push(id);
            }
        }
    }
    if left.is_empty() {
        let _ = std::fs::remove_file(&path);
    } else {
        let _ = write_record(&path, &left.join("\n"));
    }
    if let Some(keep) = keep
        && let Err(err) = mux.focus_tab(keep)
    {
        tracing::debug!(%err, "the coordinator's tab was not brought to the front");
    }
}

/// How long a new agent is watched for an exit at start, at most.
const START_WATCH: Duration = Duration::from_millis(2500);

/// Why the program in `pane` ended as it started, when it did within the
/// watch: its exit code and the last words on its screen. `None` is "still
/// running". The pane is looked at once at least, however short `wait` is.
fn exited_at_start(mux: &dyn Host, pane: &PaneId, wait: Duration) -> Option<String> {
    let deadline = std::time::Instant::now() + wait.min(START_WATCH);
    loop {
        if let Ok(Some(outcome)) = mux.exit_of(pane) {
            let screen = mux.read(pane, 40).unwrap_or_default();
            let said = screen
                .lines()
                .map(str::trim)
                .rfind(|line| !line.is_empty())
                .map(|line| line.chars().take(200).collect::<String>());
            let code = outcome
                .exit_code
                .map_or_else(|| "no exit code".to_owned(), |code| format!("exit {code}"));
            return Some(match said {
                Some(said) => format!("{code}: {said}"),
                None => code,
            });
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The words a freshly started coordinator is still to be given, once it is
/// ready for them.
///
/// Starting the agent is quick and waiting for it to accept a turn is not (a
/// program the host cannot hear is waited for in full), so
/// [`ensure_coordinator`] hands this to its caller to deliver beside the service.
pub(crate) struct PendingPrompt {
    /// The coordinator's pane.
    pane: PaneId,
    /// The role's words.
    text: String,
    /// The program, for what says it is ready.
    pick: Pick,
    /// How long to wait for it.
    wait: Duration,
}

impl PendingPrompt {
    /// Waits for the program to be ready and types the words in; a prompt that
    /// does not arrive is a warning, never a failed coordinator.
    pub(crate) fn deliver(self, mux: &dyn Host) {
        let delivery = super::deliver::deliver_brief(
            mux,
            &self.pane,
            &self.text,
            &self.pick.readiness(),
            self.wait,
        );
        if let Some(reason) = delivery.reason() {
            eprintln!("warning: the coordinator's prompt was not delivered: {reason}");
        }
    }
}

/// The settings role the repository's coordinator agent runs as.
const COORDINATOR_ROLE: &str = "coordinator";

/// What the coordinator's tab is called, as Shepherdr calls its own.
const ASSISTANT_TITLE: &str = "Assistant";

/// The name shprd's tab takes when another product already has an "Assistant"
/// in the space.
const ASSISTANT_FALLBACK_TITLE: &str = "shprd";

/// The id the coordinator's tab had before the chat tab: its title, as a label.
const OLD_COORDINATOR_TAB: &str = "coordinator";

use shprd_mux_hc::host::{COORDINATOR_TAB, is_coordinator_tab};

/// Ends the repository's coordinator and starts it again from its role.
///
/// The tab is shprd's own ([`COORDINATOR_TAB`]) and is made again under the same
/// id and the role's current name, so the chat that follows it carries on; the
/// program, model and effort are read now, which is the point: a role changed in
/// the settings reaches a coordinator that was already running only this way.
pub(crate) fn restart_coordinator(
    mux: &dyn Host,
    ctx: &Ctx,
    home: &Path,
    defer: impl FnOnce(PendingPrompt),
) -> Result<(), CliError> {
    let data = crate::cmd::roster::resolve_data_dir();
    let env = build_repo_env(ctx, Some(home), &data)?;
    restart_coordinator_with(mux, ctx, &env, defer)
}

/// [`restart_coordinator`] with the environment it starts from already built.
fn restart_coordinator_with(
    mux: &dyn Host,
    ctx: &Ctx,
    env: &SpawnEnv<'_>,
    defer: impl FnOnce(PendingPrompt),
) -> Result<(), CliError> {
    let prepared = ensure_repo_space(mux, &ctx.root)?;
    // Every running coordinator of the repository ends first, so the one the
    // restart starts is the only one and is not mistaken for the old one.
    end_other_coordinators(mux, &ctx.root, &prepared.space, None);
    close_own_tabs(mux, &prepared.space, &[COORDINATOR_TAB]);
    // The host reports a stop a moment later: wait for it, or the old
    // coordinator is found running and kept.
    for _ in 0..30 {
        if running_coordinators(mux, &ctx.root, &prepared.space).is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    ensure_coordinator(mux, env, &prepared, defer).map(|_| ())
}

/// What the person calls the coordinator: its role's name, which is the
/// built-in "Assistant" until they rename it.
pub(crate) fn coordinator_name(role: Option<&shprd_core::settings::ResolvedRole>) -> String {
    role.map_or_else(
        || ASSISTANT_TITLE.to_owned(),
        shprd_core::settings::ResolvedRole::display_title,
    )
}

/// The name for the coordinator's tab: `wanted` (the role's name), unless a tab
/// of that name that is not shprd's own is already in the space (Shepherdr adds
/// an "Assistant" to every space), in which case "shprd". Another product's tab
/// is never renamed.
fn assistant_title(mux: &dyn Host, space: &shprd_mux::Space, wanted: &str) -> String {
    let taken = mux
        .tab_names(&space.id)
        .unwrap_or_default()
        .into_iter()
        .any(|(id, name)| {
            let own = is_coordinator_tab(id.as_str()) || id.as_str() == OLD_COORDINATOR_TAB;
            !own && name.trim().eq_ignore_ascii_case(wanted)
        });
    if taken {
        ASSISTANT_FALLBACK_TITLE.to_owned()
    } else {
        wanted.to_owned()
    }
}

/// Renames the coordinator's tab to the role's name, live.
///
/// Only the tab shprd made (its id is [`COORDINATOR_TAB`]) is ever renamed: a tab
/// the person or another product made keeps its name, whatever it is called.
/// `Ok(false)` is "nothing to rename": no space, or no coordinator tab yet.
pub(crate) fn rename_coordinator_tab(
    mux: &dyn Host,
    repo: &Path,
    wanted: &str,
) -> Result<bool, CliError> {
    let name = repo_space_name(&repo_name(repo)?);
    let Some(space) = shprd_mux::find_marked_space(mux, &name).map_err(|err| {
        CliError::other(anyhow!(err).context(format!("looking for the space {name:?}")))
    })?
    else {
        return Ok(false);
    };
    let title = assistant_title(mux, &space, wanted);
    let current = mux
        .list_tabs(&space.id)
        .unwrap_or_default()
        .into_iter()
        .find(|tab| is_coordinator_tab(tab.id.as_str()));
    let Some(tab) = current else {
        return Ok(false);
    };
    if tab.title == title {
        return Ok(true);
    }
    mux.rename_tab(&tab.id, &title)
        .map_err(|err| CliError::other(anyhow!(err).context("renaming the coordinator's tab")))?;
    Ok(true)
}

/// Closes the space's tabs whose id is exactly one of `ids`: tabs shprd made
/// itself, named by the ids it gives them and never by a title a person or
/// another product could have chosen.
fn close_own_tabs(mux: &dyn Host, space: &shprd_mux::Space, ids: &[&str]) {
    for tab in mux.list_tabs(&space.id).unwrap_or_default() {
        let id = tab.id.as_str();
        if ids.contains(&id) || (ids.contains(&COORDINATOR_TAB) && is_coordinator_tab(id)) {
            close_tab(mux, &tab.id);
        }
    }
}

/// Removes the coordinator tab older shprd versions made: a bare terminal in a
/// tab whose id is `coordinator`. Only that id, in shprd's own space; a tab
/// with any other id is left alone, whatever it is called.
fn tidy_old_coordinator_tabs(mux: &dyn Host, space: &shprd_mux::Space) {
    close_own_tabs(mux, space, &[OLD_COORDINATOR_TAB]);
}

/// The pane a run's coordinator may be typed into, when one is reachable.
///
/// Two panes are reachable, and both are named rather than guessed:
///
/// 1. the pane the run recorded when it launched a coordinator
///    (`OrchestratorLaunched`), which its launcher owns; and
/// 2. the pane `shprd serve` recorded for the repository's own space
///    ([`record_coordinator_pane`]), which is where a person starts the
///    coordinator in the M10 flow. The space is found by its derived label, and
///    the record is trusted only while its pane is still one of that space's
///    tabs.
///
/// `Ok(None)` is "nobody to tell right now": no such space, or no tab recorded
/// in it. It is not an error, because both chats and the news keep their words
/// for a later call rather than refusing outright.
pub(crate) fn coordinator_pane_for(
    mux: &dyn Host,
    home: &Path,
    repo: &Path,
    state: &RunState,
    space_name: &str,
) -> Result<Option<PaneId>, CliError> {
    if let Some(orchestrator) = state.orchestrator.as_ref() {
        return Ok(Some(PaneId::new(orchestrator.pane.clone())));
    }
    let space = shprd_mux::find_marked_space(mux, space_name).map_err(|err| {
        CliError::other(anyhow!(err).context(format!("looking for the space {space_name:?}")))
    })?;
    let Some(space) = space else {
        return Ok(None);
    };
    Ok(recorded_coordinator_pane(home, repo, mux, &space))
}

/// Sends one turn of input to the run's coordinator, as the person.
///
/// The pane is [`coordinator_pane_for`]'s: the run's own record when it
/// launched one, otherwise the repository space's recorded coordinator tab. A
/// pane that is neither is a refusal: shprd never types into a tab it cannot
/// name, so a chat line cannot land in whatever happens to be first on the
/// host's bar.
pub(crate) fn prompt_coordinator(
    mux: &dyn Host,
    home: &Path,
    ctx: &Ctx,
    text: &str,
) -> Result<(), CliError> {
    let pane = coordinator_pane(mux, home, ctx)?;
    mux.write(&pane, text, true)
        .map_err(|err| CliError::other(anyhow!(err).context("prompting the coordinator")))
}

/// The pane the coordinator's chat reaches, or the refusal a coordinator with
/// no reachable pane earns. The terminal resolves its pane through here too.
pub(crate) fn coordinator_pane(mux: &dyn Host, home: &Path, ctx: &Ctx) -> Result<PaneId, CliError> {
    let state = ctx.state()?;
    let name = repo_space_name(&repo_name(&ctx.root)?);
    coordinator_pane_for(mux, home, &ctx.root, &state, &name)?.ok_or_else(|| no_coordinator(&name))
}

/// The refusal a coordinator with no recorded pane earns.
fn no_coordinator(space: &str) -> CliError {
    CliError::Invalid(anyhow!(
        "no coordinator is running in the space {space:?} yet: the service for this \
         repository starts it with the space (the `coordinator` role's program), and it is \
         still starting or could not — nothing was typed, send again in a moment"
    ))
}

/// The run the folded event log names as active.
///
/// A prompt outside a run has no tab to reach: the run id is half of every tab
/// record's path.
///
/// A finished run counts as no run at all: after `shprd run finish` its tabs
/// are gone, and its tab records are history rather than somewhere to type.
fn active_run(ctx: &Ctx) -> Result<String, CliError> {
    let state = ctx.state()?;
    if state.finished {
        return Err(no_active_run());
    }
    state
        .run
        .as_ref()
        .map(|run| run.run_id.clone())
        .ok_or_else(no_active_run)
}

/// The refusal a prompt earns when no run is in flight.
fn no_active_run() -> CliError {
    CliError::Invalid(anyhow!("no active run: start one with `shprd run start`"))
}

/// The refusal a task with no reachable tab earns.
fn no_open_tab(task: &str) -> CliError {
    CliError::Invalid(anyhow!(
        "no open tab for task {task:?}: spawn it with `shprd agent spawn --task {task}`"
    ))
}

/// Where an attempt records the id of the tab it opened:
/// `<briefs>/<run>/<task>-a<attempt>.tab`, next to its brief.
pub(crate) fn tab_path(briefs: &Path, run: &str, task: &str, attempt: u32) -> PathBuf {
    briefs.join(run).join(format!("{task}-a{attempt}.tab"))
}

/// Where an attempt records how its launch went:
/// `<briefs>/<run>/<task>-a<attempt>.spawn`, beside the tab record.
pub(crate) fn spawn_record_path(briefs: &Path, run: &str, task: &str, attempt: u32) -> PathBuf {
    briefs.join(run).join(format!("{task}-a{attempt}.spawn"))
}

/// What a spawn record holds: how one attempt's launch went.
///
/// It is written beside the tab record ([`spawn_record_path`]) so anything that
/// later asks "is this attempt's agent there?" can tell a program the host
/// reports from one the host cannot hear at all (see [`crate::cmd::deliver`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SpawnRecord {
    /// The program that was launched.
    pub(crate) agent: String,
    /// How it was ready: a [`shprd_mux`]-independent word, one of
    /// [`super::deliver::ReadyBy::as_str`]'s values or `not-delivered`.
    pub(crate) ready: String,
    /// The program's `[status] sources`, as its harness file names them.
    pub(crate) status: String,
}

impl SpawnRecord {
    /// Whether the host can report this attempt's program at all.
    ///
    /// A brief that was read off the ready line is proof the host could not
    /// report the program: it stayed `Unknown` while the program was plainly
    /// running. That is the one fact a later reader needs to stop asking the
    /// host a question it cannot answer.
    pub(crate) fn host_cannot_report(&self) -> bool {
        self.ready == "ready-line"
    }
}

/// Reads an attempt's spawn record, or `None` when it has none to read.
///
/// A missing or unreadable record is not an error: it is an attempt spawned
/// before this record existed, or one whose launch failed before it got here,
/// and every caller's answer in that case is the host's own word.
pub(crate) fn read_spawn_record(path: &Path) -> Option<SpawnRecord> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut agent = String::new();
    let mut ready = String::new();
    let mut status = String::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "agent" => agent = value.trim().to_string(),
            "ready" => ready = value.trim().to_string(),
            "status" => status = value.trim().to_string(),
            _ => {}
        }
    }
    if agent.is_empty() || ready.is_empty() {
        return None;
    }
    Some(SpawnRecord {
        agent,
        ready,
        status,
    })
}

/// Records what said the program was ready for its brief.
///
/// A spawn record is one line of `key=value`, so a person can read it without a
/// program and a later reader can parse it without a schema:
///
/// ```text
/// agent=opencode
/// ready=ready-line
/// status=osc7501,hook,screen
/// ```
///
/// `ready` is `host-status` when the host reported the agent, `ready-line` when
/// its harness's line was read off the screen because the host could not report
/// it, and `not-delivered` when the brief never arrived at all.
///
/// A failure to write is a warning, never a failed launch: the agent is running
/// either way, and this record is how a person reads afterwards what happened.
pub(crate) fn write_spawn_record(path: &Path, pick: &Pick, ready: Option<super::deliver::ReadyBy>) {
    let ready = ready.map_or("not-delivered", super::deliver::ReadyBy::as_str);
    let line = format!(
        "agent={}\nready={}\nstatus={}\n",
        pick.agent,
        ready,
        pick.status_words()
    );
    if let Some(parent) = path.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        eprintln!("warning: creating {} failed: {err}", parent.display());
        return;
    }
    if let Err(err) = std::fs::write(path, line) {
        eprintln!("warning: writing {} failed: {err}", path.display());
    }
}

/// Records the tab an attempt opened, so a later prompt reaches that exact tab
/// rather than one whose truncated label happens to match.
///
/// A failure is only a warning: the agent is running, and a prompt that cannot
/// find the record says so.
pub(crate) fn write_tab(path: &Path, tab: &shprd_mux::TabId) {
    if let Err(err) = write_tab_checked(path, tab) {
        eprintln!("warning: {err}");
    }
}

/// Records the tab an attempt opened, reporting why it could not.
pub(crate) fn write_tab_checked(path: &Path, tab: &shprd_mux::TabId) -> Result<(), CliError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| {
            CliError::other(anyhow!(err).context(format!("creating {}", parent.display())))
        })?;
    }
    std::fs::write(path, tab.as_str())
        .map_err(|err| CliError::other(anyhow!(err).context(format!("writing {}", path.display()))))
}

/// The tasks that have an agent running right now, as the host reports it.
///
/// A task counts as live when the tab its newest attempt opened still exists and
/// the host says an agent is in it: Working, Idle and Blocked all mean the
/// process is there, and only Done or Unknown mean it is not. The id comes from
/// the `.tab` record a spawn writes, so a title that happens to match is never
/// mistaken for the attempt's own tab.
///
/// # When the host cannot report the program
///
/// `Unknown` is not always "not there". A core without `pane.report_agent`
/// leaves a program whose only sources are plugins (opencode, kilo, codex,
/// claude's hooks) reporting nothing at all, and a tab of it is then `Unknown`
/// for as long as it runs. The attempt's spawn record says which case this is:
/// a launch whose brief was read off the ready line is proof the host could not
/// see the program, so the tab's existence is the evidence instead and the task
/// stays live. Reading it as dead here is what would let the tick land work an
/// agent is still writing, which is the one thing the live set exists to stop.
///
/// A host that cannot be asked at all answers "nothing is live", which is the
/// safe direction for the tick: a task it cannot see is a task it will not start
/// a second attempt on.
pub fn live_tasks(mux: &dyn Host, home: &Path, state: &RunState) -> BTreeSet<String> {
    agent_states(mux, home, state).0
}

/// The tasks whose agent is there but idle: its process is up and it is not at
/// work. A task whose agent ended its turn without handing in is one of these.
pub fn idle_tasks(mux: &dyn Host, home: &Path, state: &RunState) -> BTreeSet<String> {
    agent_states(mux, home, state).1
}

/// The live tasks and, among them, the idle ones, from one reading of the host.
fn agent_states(
    mux: &dyn Host,
    home: &Path,
    state: &RunState,
) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut live = BTreeSet::new();
    let mut idle = BTreeSet::new();
    let Some(run) = state.run.as_ref() else {
        return (live, idle);
    };
    let tabs = all_handles(mux);
    let briefs = home.join(".shprd").join("briefs");
    for (id, task) in &state.tasks {
        // A task that cannot have an agent at work is not asked about: a long
        // plan would otherwise cost one host call per task per pass.
        if !matches!(
            task.status,
            TaskStatus::Working | TaskStatus::Reported | TaskStatus::Passed
        ) {
            continue;
        }
        let Some(record) = latest_tab_record(&briefs, &run.run_id, id) else {
            continue;
        };
        let Ok(text) = std::fs::read_to_string(&record) else {
            continue;
        };
        let wanted = text.trim();
        let Some(tab) = tabs.iter().find(|tab| tab.id.as_str() == wanted) else {
            continue;
        };
        // Done and a host error both mean no agent is at work. `Unknown` means
        // either that, or a program this host cannot report — and the attempt's
        // spawn record is what tells the two apart.
        match mux.agent_state(&tab.root_pane) {
            Ok(AgentState::Working | AgentState::Blocked) => {
                live.insert(id.clone());
            }
            Ok(AgentState::Idle) => {
                live.insert(id.clone());
                idle.insert(id.clone());
            }
            Ok(AgentState::Unknown) if unreadable_by_host(&briefs, &run.run_id, id) => {
                live.insert(id.clone());
            }
            _ => {}
        }
    }
    (live, idle)
}

/// Whether this task's newest attempt is a program the host cannot report.
///
/// The answer is the attempt's own spawn record: a delivery that was read off
/// the ready line means the host stayed `Unknown` while the program was up.
pub(crate) fn unreadable_by_host(briefs: &Path, run: &str, task: &str) -> bool {
    let Some(attempt) = latest_attempt(briefs, run, task) else {
        return false;
    };
    read_spawn_record(&spawn_record_path(briefs, run, task, attempt))
        .is_some_and(|record| record.host_cannot_report())
}

/// The number of a task's newest attempt in one run, from its tab records.
pub(crate) fn latest_attempt(briefs: &Path, run: &str, task: &str) -> Option<u32> {
    latest_tab_record(briefs, run, task).and_then(|path| {
        let name = path.file_name()?.to_string_lossy().to_string();
        let prefix = format!("{task}-a");
        name.strip_prefix(&prefix)
            .and_then(|rest| rest.strip_suffix(".tab"))
            .and_then(|digits| digits.parse::<u32>().ok())
    })
}

/// The tab record of a task's latest attempt in one run.
///
/// Only `run`'s directory is read: a task id reused in a later run must not
/// resolve to the older run's tab. The attempt number is part of the file name,
/// so the highest one names the tab a prompt should reach.
pub(crate) fn latest_tab_record(briefs: &Path, run: &str, task: &str) -> Option<PathBuf> {
    let prefix = format!("{task}-a");
    let mut best: Option<(u32, PathBuf)> = None;
    for file in std::fs::read_dir(briefs.join(run)).ok()?.flatten() {
        let name = file.file_name().to_string_lossy().to_string();
        let attempt = name
            .strip_prefix(&prefix)
            .and_then(|rest| rest.strip_suffix(".tab"))
            .and_then(|digits| digits.parse::<u32>().ok());
        let Some(attempt) = attempt else {
            continue;
        };
        if best.as_ref().is_none_or(|(kept, _)| attempt > *kept) {
            best = Some((attempt, file.path()));
        }
    }
    best.map(|(_, path)| path)
}

/// Tells the program its launch folder is trusted, so its own first-run dialog
/// never opens in the pane. A trust that cannot be said is only a note on stderr:
/// the dialog it was meant to prevent is still answered from the pane.
pub(crate) fn trust_launch_folder(
    program: &str,
    folder: &Path,
    base_env: &BTreeMap<String, String>,
    argv: &mut Vec<String>,
) {
    if let Err(err) = folder_trust::trust_folder(program, folder, base_env, argv) {
        eprintln!(
            "shprd: could not mark {} as trusted for {program}: {err}",
            folder.display()
        );
    }
}

/// Builds the argv the agent's CLI starts with.
///
/// The order is fixed by the launch spec: the base command, then the flags that
/// suppress approval prompts, then the model, the effort and finally the system
/// prompt. The second element of the result says whether the brief went in as an
/// argument, which is also what says whether it has to be typed in instead.
fn launch_argv(
    launch: &LaunchSpec,
    spec: &HarnessSpec,
    applied: &Applied,
    model: Option<&str>,
    effort: Option<&str>,
    brief: &str,
    brief_path: &Path,
) -> Result<(Vec<String>, bool), CliError> {
    let mut argv = launch.interactive.clone();
    // One access mode: the one the role named when the program declares it,
    // else the program's full-access mode. A program whose spec declares no
    // modes keeps the auto-approval flags it has always been started with.
    match applied
        .perm
        .as_deref()
        .and_then(|id| spec.permissions.mode(id))
    {
        Some(mode) => argv.extend(mode.args.iter().cloned()),
        None => {
            if let Some(auto) = &launch.auto_approve {
                argv.extend(auto.iter().cloned());
            }
        }
    }
    let values = Values {
        model: model.map(str::to_string),
        effort: effort.map(str::to_string),
        prompt: Some(brief.to_string()),
        prompt_file: Some(brief_path.display().to_string()),
        session: None,
    };
    argv.extend(render(
        launch,
        super::deliver::model_args(launch, model),
        &values,
    )?);
    if let (Some(effort_args), Some(_)) = (&launch.effort_args, effort) {
        argv.extend(render(launch, effort_args, &values)?);
    }
    // Fast is said out loud in both directions, as Shepherdr's `argvFor` does:
    // a program that can be told to turn fast off is, so a person's own setting
    // cannot quietly win over the role's.
    if spec.fast.available() {
        argv.extend(spec.fast.args(applied.fast).iter().cloned());
    }
    let mut brief_in_args = false;
    if let Some(system_prompt_args) = &launch.system_prompt_args {
        argv.extend(render(launch, system_prompt_args, &values)?);
        brief_in_args = true;
    }
    Ok((argv, brief_in_args))
}

/// Renders one argument template, naming the template when it cannot be filled.
fn render(
    launch: &LaunchSpec,
    template: &[String],
    values: &Values,
) -> Result<Vec<String>, CliError> {
    render_args(template, values).map_err(|err| {
        CliError::other(anyhow!(err).context(format!(
            "filling the launch arguments {template:?} of {:?}",
            launch.interactive
        )))
    })
}

/// Flattens a launch environment into the pairs [`Mux::run`] takes: set entries
/// first, then the removals, both in name order.
fn env_pairs(diff: LaunchEnv) -> Vec<(String, Option<String>)> {
    let mut pairs: Vec<(String, Option<String>)> = diff
        .set
        .into_iter()
        .map(|(name, value)| (name, Some(value)))
        .collect();
    pairs.extend(diff.remove.into_iter().map(|name| (name, None)));
    pairs
}

#[cfg(test)]
mod flow_tests {
    use super::*;
    use std::process::Command;

    use shprd_core::event::{Actor, Finding, ReportStatus, ReviewSource, Severity};
    use shprd_core::state::{ResourceRecord, ResourceStatus, TaskStatus};
    use shprd_mux::fake::{Call, FakeMux};
    use tempfile::TempDir;

    /// Runs one git command in `dir`, failing the test when it does.
    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output()
            .unwrap_or_else(|err| panic!("git {args:?} failed: {err}"));
        assert!(status.status.success(), "git {args:?}: {status:?}");
    }

    /// A harness spec for a fake CLI named `id`, parsed the way the data
    /// directory's specs are, so the tests exercise the real model rules.
    fn fake_spec(id: &str) -> HarnessSpec {
        fake_spec_with(id, false)
    }

    /// The same spec, but with the model rules `provider_qualified` sets, as
    /// `pi` has them.
    fn fake_spec_with(id: &str, provider_qualified: bool) -> HarnessSpec {
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
interactive = ["{id}"]
model_args = ["--model", "{{model}}"]
verified = false

[models]
provider_qualified = {provider_qualified}

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
        match toml::from_str::<HarnessSpec>(&text) {
            Ok(spec) => spec,
            Err(err) => panic!("the test spec is invalid: {err}"),
        }
    }

    /// A repository with one commit and the integration branch, and a home.
    struct Scratch {
        repo: TempDir,
        home: TempDir,
    }

    impl Scratch {
        fn new() -> Scratch {
            let repo = TempDir::new().unwrap_or_else(|err| panic!("{err}"));
            let home = TempDir::new().unwrap_or_else(|err| panic!("{err}"));
            git(repo.path(), &["init", "-q", "-b", "main"]);
            std::fs::write(repo.path().join("README.md"), "scratch\n").unwrap_or_default();
            git(repo.path(), &["add", "."]);
            git(repo.path(), &["commit", "-q", "-m", "first"]);
            git(repo.path(), &["branch", "integration/m4"]);
            Scratch { repo, home }
        }

        fn root(&self) -> &Path {
            self.repo.path()
        }

        /// The repo's directory name, which the workspace and worktree names use.
        fn repo_name(&self) -> String {
            self.root()
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_default()
        }

        /// A context with a started run and the task `T1` in its plan.
        fn ctx(&self) -> Ctx {
            let ctx = Ctx::new(self.root().to_path_buf(), Actor::Human, false, true);
            ctx.commit(EventKind::RunStarted {
                run_id: "t1".to_string(),
                repo: self.repo_name(),
                base_branch: "main".to_string(),
                integration_branch: "integration/m4".to_string(),
            })
            .unwrap_or_else(|err| panic!("{err}"));
            ctx.commit(EventKind::TaskAdded {
                task: "T1".to_string(),
                title: "Make spawn real".to_string(),
                scope: vec!["crates/shprd".to_string()],
                acceptance: vec!["spawn opens a tab".to_string()],
                tags: vec!["rust".to_string()],
                skills: Vec::new(),
                deps: Vec::new(),
            })
            .unwrap_or_else(|err| panic!("{err}"));
            ctx
        }

        /// The environment a spawn runs in, with a fake `sh` CLI.
        fn spawn_env<'a>(&self, ctx: &'a Ctx) -> SpawnEnv<'a> {
            let mut base_env = BTreeMap::new();
            base_env.insert("GH_TOKEN".to_string(), "secret".to_string());
            base_env.insert("PATH".to_string(), "/usr/bin".to_string());
            let transcript_roots = shprd_agents::transcript::Roots::for_launch(&base_env);
            SpawnEnv {
                ctx,
                layout: Layout::default_for_home(self.home.path()),
                briefs_dir: self.home.path().join(".shprd").join("briefs"),
                prompts_dir: shprd_core::prompts::prompts_dir(&self.home.path().join(".config")),
                home: self.home.path().to_path_buf(),
                settings: Loaded::default(),
                run: "t1".to_string(),
                base_branch: "integration/m4".to_string(),
                repo_name: self.repo_name(),
                programs: vec![Program {
                    spec: fake_spec("sh"),
                    installed: true,
                }],
                workers: vec!["sh".to_string()],
                skills: Skills {
                    workers: vec!["tdd".to_string()],
                    ..Skills::default()
                },
                gates: vec!["cargo test --workspace".to_string()],
                catalogs: BTreeMap::new(),
                data_dir: DataDir::resolve(Some(self.home.path()), None),
                compat: Vec::new(),
                base_env,
                // No wait: a fake pane that never reports an agent would
                // otherwise stall every test that launches one.
                ready_wait: Duration::ZERO,
                transcript_roots,
            }
        }

        /// [`Scratch::spawn_env`], with the person's settings read from `text`.
        ///
        /// It writes `~/.config/shprd/settings.toml` in the temp home, so a role's
        /// pick reaches a spawn the way it does on a real machine.
        fn spawn_env_with<'a>(&self, ctx: &'a Ctx, text: &str) -> SpawnEnv<'a> {
            let dir = self.home.path().join(".config").join("shprd");
            std::fs::create_dir_all(&dir).unwrap_or_else(|err| panic!("{err}"));
            std::fs::write(dir.join("settings.toml"), text).unwrap_or_else(|err| panic!("{err}"));
            let mut env = self.spawn_env(ctx);
            env.settings =
                Loaded::read(self.home.path(), None).unwrap_or_else(|err| panic!("{err}"));
            // What `build_env` does: the layout follows the settings.
            env.layout = Layout::default_for_home(self.home.path())
                .for_tasks(&env.settings.resolve(None).tasks());
            env
        }
    }

    /// The harness table two of the agent-string cases parse against: a plain
    /// `sh` and a `pi` that needs its model provider-qualified.
    fn programs() -> Vec<Program> {
        vec![
            Program {
                spec: fake_spec("sh"),
                installed: true,
            },
            Program {
                spec: fake_spec_with("pi", true),
                installed: true,
            },
        ]
    }

    /// The refusal an explicit agent string earns, panicking when it is
    /// accepted instead.
    fn refusal(agent: Option<&str>) -> CliError {
        match choose_agent(agent, &[], &programs()) {
            Ok(pick) => panic!("{agent:?} was accepted as {}", pick.agent),
            Err(err) => err,
        }
    }

    #[test]
    fn an_agent_string_with_no_model_is_refused() {
        let refused = refusal(Some("sh/"));
        assert_eq!(refused.exit_code(), 2);
        let text = refused.to_string();
        assert!(text.contains("names no model"), "{text}");
    }

    #[test]
    fn a_model_that_names_no_provider_is_refused_with_the_hint() {
        let refused = refusal(Some("pi/deepseek-x"));
        assert_eq!(refused.exit_code(), 2);
        let text = refused.to_string();
        assert!(text.contains("needs a provider"), "{text}");
        assert!(text.contains("pi/<provider>/deepseek-x"), "{text}");
    }

    #[test]
    fn a_configured_worker_that_does_not_parse_is_skipped() {
        let workers = vec!["pi/bare".to_string(), "sh".to_string()];
        let pick = choose_agent(None, &workers, &programs()).unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(pick.agent, "sh");
    }

    fn request() -> SpawnRequest {
        SpawnRequest {
            task: "T1".to_string(),
            agent: None,
            effort: None,
        }
    }

    /// The pick a spawn makes for the fake `sh` worker.
    fn pick() -> Pick {
        Pick {
            agent: "sh".to_string(),
            model: None,
            launch: fake_spec("sh").launch,
            blocking_prompts: Vec::new(),
            ready_lines: Vec::new(),
            // A fake `sh` reports no status of its own and no screen line, so
            // the readiness of this pick is the painted-screen fallback.
            own_status: false,
            status_words: "osc7501, screen".to_string(),
        }
    }

    /// The spawn plan of an installed binary: no `$SHPRD_DATA_DIR`, an empty
    /// data home, and only the harness table, model catalogs and skills tables
    /// the binary carries.
    #[test]
    fn a_spawn_plan_finds_the_pi_harness_in_the_embedded_data() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let data_home = TempDir::new().unwrap_or_else(|err| panic!("{err}"));
        let data = DataDir::resolve(None, Some(data_home.path()));
        assert_eq!(
            data.origin(),
            &shprd_agents::data::Origin::Embedded,
            "an empty data home is no override"
        );

        let env = build_env_in(&ctx, Some(scratch.home.path()), &data)
            .unwrap_or_else(|err| panic!("{err}"));
        let ids: Vec<&str> = env
            .programs
            .iter()
            .map(|program| program.spec.id.as_str())
            .collect();
        for id in ["claude", "codex", "kilo", "opencode", "pi"] {
            assert!(
                ids.contains(&id),
                "{id} is not in the shipped harness table: {ids:?}"
            );
        }
        // The rest of the plan is embedded too: claude's static catalog and the
        // curated skills tables read with no directory on disk anywhere.
        let claude = env
            .programs
            .iter()
            .find(|program| program.spec.id == "claude")
            .unwrap_or_else(|| panic!("no claude program"));
        assert!(
            env.data_dir
                .catalog(&claude.spec, "", Duration::ZERO)
                .is_some(),
            "claude's static catalog should come from the binary"
        );
        assert!(!env.compat.is_empty(), "the skills table should be shipped");
    }

    #[test]
    fn a_spawned_append_that_fails_leaves_no_live_resource() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.spawn_env(&ctx);
        let created = create_worktree(&env, "T1", None).unwrap_or_else(|err| panic!("{err}"));
        // The third append is the `Spawned` event; the two resources land.
        let mut commits = 0;
        let outcome = append_in_order(attempt_events("T1", 1, &pick(), &created, true), |event| {
            commits += 1;
            if commits == 3 {
                return Err(CliError::other(anyhow!("the log went away")));
            }
            env.ctx.commit(event).map(|_| ())
        });
        let (err, landed) = match outcome {
            Ok(()) => panic!("the attempt should not have landed"),
            Err(failed) => failed,
        };
        assert!(err.to_string().contains("the log went away"), "{err}");
        assert_eq!(landed.len(), 2, "only the two resources landed");
        compensate(&env, &landed);
        discard_worktree(&env, &created);

        let state = ctx.state().unwrap_or_else(|err| panic!("{err}"));
        let live: Vec<&ResourceRecord> = state
            .resources
            .values()
            .filter(|record| record.status == ResourceStatus::Live)
            .collect();
        assert!(live.is_empty(), "still live: {live:?}");
        assert!(!created.path.exists(), "the worktree is still there");
    }

    /// The seen failure: a worker window opened in the task's worktree but its
    /// attempt never reached the log, so every later start failed with "its new
    /// workspace could not be created" while the worker did the task.
    #[test]
    fn a_start_that_finds_the_worker_already_up_records_it() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.spawn_env(&ctx);
        let log = scratch.root().join(".shprd").join("events.jsonl");
        let before = std::fs::read_to_string(&log).unwrap_or_else(|err| panic!("{err}"));
        let mux = FakeMux::default().with_ready_agent();
        spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));
        // The window, the worktree and the tab record are up; the attempt is lost.
        std::fs::write(&log, before).unwrap_or_else(|err| panic!("{err}"));
        let ctx = Ctx::new(scratch.root().to_path_buf(), Actor::Human, false, true);
        let env = scratch.spawn_env(&ctx);
        let state = ctx.state().unwrap_or_else(|err| panic!("{err}"));
        assert!(state.tasks["T1"].attempts.is_empty());
        let windows = all_handles(&mux).len();

        let report = spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));

        assert_eq!(report.attempt, 1);
        assert_eq!(all_handles(&mux).len(), windows, "a second window opened");
        let state = ctx.state().unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(
            state.tasks["T1"].attempts.len(),
            1,
            "the attempt is recorded"
        );
    }

    #[test]
    fn a_window_that_will_not_close_keeps_its_attempt_instead_of_being_orphaned() {
        let tab = shprd_mux::TabId::new("t1");
        let fail = || CliError::other(anyhow!("log busy"));
        // The host closes it: nothing is left, and the original error stands.
        let mux = FakeMux::default();
        assert!(matches!(
            unwind_unrecorded(&mux, &tab, || Ok(()), fail()),
            Unwound::Closed(_)
        ));
        // The host refuses and the retry records: the worker is kept recorded.
        let mut refusing = FakeMux::default();
        refusing.failing_tab_closes(None, "held");
        let mut tries = 0;
        let out = unwind_unrecorded(
            &refusing,
            &tab,
            || {
                tries += 1;
                Ok(())
            },
            fail(),
        );
        assert!(matches!(out, Unwound::Recorded));
        assert_eq!(tries, 1);
        // Neither works: the worker is reported as kept, never silently lost.
        assert!(matches!(
            unwind_unrecorded(&refusing, &tab, || Err(fail()), fail()),
            Unwound::Stranded(_)
        ));
    }

    #[test]
    fn live_tasks_reads_the_host_state_of_each_attempts_own_tab() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.spawn_env(&ctx);
        let mux = FakeMux::default().with_ready_agent();
        let report = spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));
        // The agent is headless: found by the host's own listing.
        let pane = all_handles(&mux)
            .into_iter()
            .find(|tab| tab.title == report.tab)
            .map(|tab| tab.root_pane)
            .unwrap_or_else(|| panic!("no tab for {:?}", report.tab));

        let state = ctx.state().unwrap_or_else(|err| panic!("{err}"));
        let mut live = live_tasks(&mux, scratch.home.path(), &state);
        assert!(
            live.remove("T1"),
            "a working agent's task is live: {live:?}"
        );
        assert!(live.is_empty());

        // The agent's turn ended: its process is still there, so the task is
        // still live — a landing would judge a commit it may yet move.
        mux.set_state(&pane, AgentState::Idle);
        let live = live_tasks(&mux, scratch.home.path(), &state);
        assert!(
            live.contains("T1"),
            "an idle agent is still there: {live:?}"
        );

        // The process ended: nothing is live any more.
        mux.set_state(&pane, AgentState::Done);
        let live = live_tasks(&mux, scratch.home.path(), &state);
        assert!(live.is_empty(), "a finished agent is not live: {live:?}");
    }

    #[test]
    fn live_tasks_sees_nothing_without_the_repositorys_workspace() {
        // A repository the human never opened in the multiplexer has no tabs,
        // and the answer is "nothing is live" rather than an error: a pass must
        // not stop because a host was never asked.
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let state = ctx.state().unwrap_or_else(|err| panic!("{err}"));
        let live = live_tasks(&FakeMux::default(), scratch.home.path(), &state);
        assert!(live.is_empty());
    }

    /// A person who set the worker role's pick gets that program and model,
    /// even though another worker is configured, and the brief says so as
    /// theirs rather than as the program's own default.
    #[test]
    fn the_worker_roles_pick_beats_the_configured_workers() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let mut env = scratch.spawn_env_with(
            &ctx,
            "[roles.worker]\nagent = \"pi\"\nmodel = \"sonnet\"\neffort = \"high\"\n",
        );
        env.programs.push(Program {
            spec: fake_spec("pi"),
            installed: true,
        });
        let mux = FakeMux::default().with_ready_agent();
        mux.create_space(&repo_space_name(&scratch.repo_name()), scratch.root())
            .unwrap_or_else(|err| panic!("{err}"));

        let report = spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));

        assert_eq!(report.agent, "pi");
        assert_eq!(report.model.as_deref(), Some("sonnet"));
        let brief = std::fs::read_to_string(&report.brief).unwrap_or_default();
        assert!(brief.contains("- Program: pi (you)"), "{brief}");
        assert!(brief.contains("- Model: sonnet (you)"), "{brief}");
        assert!(brief.contains("- Effort: high (you)"), "{brief}");
    }

    /// A worker with several program lines: the default is the first, and a task
    /// asked for another line gets that line's own model and effort, and the
    /// brief says which line it got and where it goes next.
    #[test]
    fn a_task_picks_a_worker_line_and_gets_its_settings() {
        let settings = "[roles.worker]\nagent = \"pi\"\nmodel = \"sonnet\"\neffort = \"high\"\n\
                        agents = [\"kilo|kilo-model|low||\", \"opencode\"]\n";
        let run = |asked: Option<&str>| {
            let scratch = Scratch::new();
            let ctx = scratch.ctx();
            let mut env = scratch.spawn_env_with(&ctx, settings);
            for id in ["pi", "kilo", "opencode"] {
                env.programs.push(Program {
                    spec: fake_spec(id),
                    installed: true,
                });
            }
            let mux = FakeMux::default().with_ready_agent();
            mux.create_space(&repo_space_name(&scratch.repo_name()), scratch.root())
                .unwrap_or_else(|err| panic!("{err}"));
            let mut req = request();
            req.agent = asked.map(str::to_string);
            let report = spawn(&mux, &env, &req).unwrap_or_else(|err| panic!("{err}"));
            let brief = std::fs::read_to_string(&report.brief).unwrap_or_default();
            (report, brief)
        };

        // Nothing asked: the first line.
        let (first, brief) = run(None);
        assert_eq!(first.agent, "pi");
        assert_eq!(first.model.as_deref(), Some("sonnet"));
        assert!(
            brief.contains("1 of 3 (`pi`), the default: the first line"),
            "{brief}"
        );
        assert!(brief.contains("moves to `kilo`"), "{brief}");

        // Asked for the second line by its program: its model and effort.
        let (second, brief) = run(Some("kilo"));
        assert_eq!(second.agent, "kilo");
        assert_eq!(second.model.as_deref(), Some("kilo-model"));
        assert!(
            brief.contains("2 of 3 (`kilo`), asked for with --agent"),
            "{brief}"
        );
        assert!(brief.contains("- Model: kilo-model (this task)"), "{brief}");
        assert!(brief.contains("- Effort: low (this task)"), "{brief}");
        assert!(
            !brief.contains("- Fallback:"),
            "the lines replace the fallback line: {brief}"
        );
    }

    /// A model its catalog says cannot think is not passed an effort flag, and
    /// the launch says so: the setting looks applied but is not.
    #[test]
    fn a_no_thinking_model_is_not_passed_the_effort_flag() {
        use shprd_agents::models::{Catalog, CatalogModel};

        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let mut env = scratch.spawn_env_with(
            &ctx,
            "[roles.worker]\nagent = \"pi\"\nmodel = \"Command Code/deepseek/x\"\neffort = \"high\"\n",
        );
        env.programs.push(Program {
            spec: fake_spec("pi"),
            installed: true,
        });
        let catalog = Catalog {
            models: vec![CatalogModel {
                id: "Command Code/deepseek/x".to_string(),
                name: "x".to_string(),
                efforts: Vec::new(),
                default_effort: None,
                fast: false,
            }],
            efforts: vec!["off".to_string(), "low".to_string(), "high".to_string()],
        };
        env.catalogs.insert("pi".to_string(), catalog);
        let mux = FakeMux::default().with_ready_agent();
        mux.create_space(&repo_space_name(&scratch.repo_name()), scratch.root())
            .unwrap_or_else(|err| panic!("{err}"));

        let report = spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));

        assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
        assert!(
            report.warnings[0].contains("not thinking"),
            "{:?}",
            report.warnings
        );
        let launched: Vec<Vec<String>> = mux
            .calls()
            .into_iter()
            .filter_map(|call| match call {
                Call::Spawn(_, _, argv, _, _) => Some(argv),
                _ => None,
            })
            .collect();
        let argv = launched
            .first()
            .unwrap_or_else(|| panic!("no CLI was started"));
        assert!(
            !argv.iter().any(|arg| arg == "--thinking"),
            "a model that cannot think must not be passed a level: {argv:?}"
        );
        let brief = std::fs::read_to_string(&report.brief).unwrap_or_default();
        assert!(brief.contains("## Warnings"), "{brief}");
    }

    /// The shipped Claude Code spec, so a test uses the access modes and fast
    /// switch a real role can set.
    fn claude_spec() -> HarnessSpec {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/harness");
        shprd_agents::spec::load_dir(&dir)
            .unwrap_or_else(|err| panic!("the shipped harness table: {err}"))
            .into_iter()
            .find(|spec| spec.id == "claude")
            .unwrap_or_else(|| panic!("the shipped table has no claude spec"))
    }

    /// A spawn's argv, from what the fake host was asked to run.
    fn spawn_argv(calls: &[Call]) -> Vec<String> {
        calls
            .iter()
            .find_map(|call| match call {
                Call::Spawn(_, _, argv, _, _) => Some(argv.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("the CLI was not started: {calls:?}"))
    }

    /// The role's access mode and fast switch are on the command line and in
    /// the brief: a line the agent reads never claims something not in force.
    #[test]
    fn the_roles_access_mode_and_fast_switch_reach_the_launch_and_the_brief() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let mut env = scratch.spawn_env_with(
            &ctx,
            "[roles.worker]\nagent = \"claude\"\nperm = \"plan\"\nfast = true\n",
        );
        env.programs = vec![Program {
            spec: claude_spec(),
            installed: true,
        }];
        let mux = FakeMux::default().with_ready_agent();
        mux.create_space(&repo_space_name(&scratch.repo_name()), scratch.root())
            .unwrap_or_else(|err| panic!("{err}"));

        let report = spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));

        let argv = spawn_argv(&mux.calls());
        assert!(
            argv.windows(2)
                .any(|pair| pair == ["--permission-mode", "plan"]),
            "the role's mode is on the command line: {argv:?}"
        );
        assert!(
            argv.windows(2)
                .any(|pair| pair == ["--settings", "{\"fastMode\":true}"]),
            "the role's fast switch is on the command line: {argv:?}"
        );
        let brief = std::fs::read_to_string(&report.brief).unwrap_or_default();
        assert!(brief.contains("- Permissions: plan (you)"), "{brief}");
        assert!(brief.contains("- Fast mode: on (you)"), "{brief}");
    }

    /// A spawned worker's worktree gets the program's ask hook, so its question
    /// and permission prompts become cards (SHPR-57), and nothing outside the
    /// worktree is written.
    #[test]
    fn a_spawn_installs_the_ask_hook_in_the_workers_worktree() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let mut env = scratch.spawn_env_with(&ctx, "[roles.worker]\nagent = \"claude\"\n");
        env.programs = vec![Program {
            spec: claude_spec(),
            installed: true,
        }];
        let mux = FakeMux::default().with_ready_agent();
        mux.create_space(&repo_space_name(&scratch.repo_name()), scratch.root())
            .unwrap_or_else(|err| panic!("{err}"));

        let report = spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));

        let config = report.worktree.join(".claude/settings.local.json");
        let text = std::fs::read_to_string(&config)
            .unwrap_or_else(|err| panic!("no ask hook config at {}: {err}", config.display()));
        assert!(text.contains("ask-hook.sh"), "{text}");
        assert!(text.contains("PermissionRequest"), "{text}");
        assert!(report.worktree.join(".shprd/ask-hook.sh").is_file());
        // The person's own settings stay theirs.
        assert!(!scratch.home.path().join(".claude").exists());
    }

    /// With `tasks.worktrees` off the worker runs in the person's own checkout,
    /// and the spawn writes no hook config there.
    #[test]
    fn a_spawn_without_worktrees_leaves_the_persons_checkout_alone() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let mut env = scratch.spawn_env_with(
            &ctx,
            "[roles.worker]\nagent = \"claude\"\n[tasks]\nworktrees = false\n",
        );
        env.programs = vec![Program {
            spec: claude_spec(),
            installed: true,
        }];
        let mux = FakeMux::default().with_ready_agent();
        mux.create_space(&repo_space_name(&scratch.repo_name()), scratch.root())
            .unwrap_or_else(|err| panic!("{err}"));

        spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));

        assert!(!scratch.root().join(".claude").exists());
    }

    /// A program with no ask hook spawns without one and writes no config.
    #[test]
    fn a_spawn_of_a_program_with_no_ask_hook_writes_none() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.spawn_env(&ctx);
        let mux = FakeMux::default().with_ready_agent();
        mux.create_space(&repo_space_name(&scratch.repo_name()), scratch.root())
            .unwrap_or_else(|err| panic!("{err}"));

        let report = spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));

        assert!(!report.worktree.join(".claude").exists());
        assert!(!report.worktree.join(".shprd/ask-hook.sh").exists());
    }

    /// A mode the program does not declare is not claimed: the launch falls back
    /// to the program's full-access mode, and the brief says that instead.
    #[test]
    fn a_mode_the_program_lacks_is_not_claimed_in_the_brief() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let mut env = scratch.spawn_env_with(
            &ctx,
            "[roles.worker]\nagent = \"claude\"\nperm = \"read-only\"\n",
        );
        env.programs = vec![Program {
            spec: claude_spec(),
            installed: true,
        }];
        let mux = FakeMux::default().with_ready_agent();
        mux.create_space(&repo_space_name(&scratch.repo_name()), scratch.root())
            .unwrap_or_else(|err| panic!("{err}"));

        let report = spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));

        let argv = spawn_argv(&mux.calls());
        assert!(
            argv.contains(&"--dangerously-skip-permissions".to_string()),
            "the full-access mode is what runs: {argv:?}"
        );
        let brief = std::fs::read_to_string(&report.brief).unwrap_or_default();
        assert!(
            !brief.contains("- Permissions: read-only"),
            "the block does not claim a mode the program lacks: {brief}"
        );
    }

    /// The repository's own space, made on `mux`, as `shprd serve` would: a
    /// host that gives a new space its starter tab.
    fn repo_space(mux: &FakeMux, scratch: &Scratch) -> shprd_mux::Space {
        mux.create_space(&repo_space_name(&scratch.repo_name()), scratch.root())
            .unwrap_or_else(|err| panic!("{err}"))
    }

    /// Every write the fake saw, as `(pane, text)`.
    fn writes(mux: &FakeMux) -> Vec<(String, String)> {
        mux.calls()
            .into_iter()
            .filter_map(|call| match call {
                Call::Write(pane, text, _) => Some((pane.as_str().to_string(), text)),
                _ => None,
            })
            .collect()
    }

    /// Records the space's first tab as the coordinator agent's pane.
    fn record_first_tab(
        home: &Path,
        repo: &Path,
        mux: &FakeMux,
        space: &shprd_mux::Space,
    ) -> PaneId {
        let pane = mux
            .list_tabs(&space.id)
            .unwrap_or_else(|err| panic!("{err}"))
            .first()
            .map(|tab| tab.root_pane.clone())
            .unwrap_or_else(|| panic!("the space has a pane to record"));
        record_coordinator_pane(home, repo, &space.id, &pane, "sh")
            .unwrap_or_else(|err| panic!("{err}"));
        pane
    }

    /// The pane the service records is the pane the chat types into: this is the
    /// path every coordinator line in the M10 flow takes, since nothing launches
    /// a coordinator for the run to record.
    #[test]
    fn a_coordinator_line_goes_to_the_pane_the_service_recorded() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let mux = FakeMux::default().with_starter_tab();
        let space = repo_space(&mux, &scratch);
        let pane = record_first_tab(scratch.home.path(), scratch.root(), &mux, &space);
        let _ = mux.take_calls();

        prompt_coordinator(&mux, scratch.home.path(), &ctx, "hello coordinator")
            .unwrap_or_else(|err| panic!("{err}"));

        assert_eq!(
            writes(&mux),
            vec![(pane.as_str().to_string(), "hello coordinator".to_string())],
            "one line, into the recorded pane, and nowhere else"
        );
    }

    /// No record is no pane: shprd will not guess at one, so a chat line cannot
    /// land in whatever tab happens to be first.
    #[test]
    fn a_coordinator_line_without_a_recorded_pane_is_refused() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let mux = FakeMux::default().with_starter_tab();
        let _ = repo_space(&mux, &scratch);
        let _ = mux.take_calls();

        match prompt_coordinator(&mux, scratch.home.path(), &ctx, "hello") {
            Ok(()) => panic!("nothing recorded, so nothing may be typed into"),
            Err(err) => assert!(
                err.to_string().contains("no coordinator is running"),
                "the refusal says what is missing: {err}"
            ),
        }
        assert!(writes(&mux).is_empty(), "nothing was typed");
    }

    /// A record whose pane is no longer in the space is stale — the person
    /// closed that tab — and a stale record is a refusal, never a fallback.
    #[test]
    fn a_stale_coordinator_record_is_not_typed_into() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let mux = FakeMux::default().with_starter_tab();
        let space = repo_space(&mux, &scratch);
        let path = coordinator_pane_path(scratch.home.path(), scratch.root(), &space.id);
        let dir = path.parent().unwrap_or_else(|| panic!("a parent"));
        std::fs::create_dir_all(dir).unwrap_or_else(|err| panic!("{err}"));
        std::fs::write(&path, "p-closed\n").unwrap_or_else(|err| panic!("{err}"));
        let _ = mux.take_calls();

        let refused = prompt_coordinator(&mux, scratch.home.path(), &ctx, "hello").is_err();
        assert!(refused, "a pane the space no longer has is refused");
        assert!(writes(&mux).is_empty(), "nothing was typed");

        // And the space's own tab is still fine once it is the one recorded.
        let pane = record_first_tab(scratch.home.path(), scratch.root(), &mux, &space);
        let _ = mux.take_calls();
        prompt_coordinator(&mux, scratch.home.path(), &ctx, "hello")
            .unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(
            writes(&mux),
            vec![(pane.as_str().to_string(), "hello".to_string())]
        );
    }

    /// A person renames the repository's space; the next lookup (serve, spawn,
    /// live_news all share it) reuses the same space and makes no second one.
    #[test]
    fn a_renamed_repo_space_is_reused_not_made_again() {
        let scratch = Scratch::new();
        let mux = FakeMux::default();
        let first = ensure_repo_space(&mux, scratch.root()).unwrap_or_else(|err| panic!("{err}"));
        assert!(first.created);
        mux.rename_space(&first.space.id, "my own name");
        let again = ensure_repo_space(&mux, scratch.root()).unwrap_or_else(|err| panic!("{err}"));
        assert!(!again.created, "the renamed space was found");
        assert_eq!(again.space.id, first.space.id);
        let creates = mux
            .calls()
            .iter()
            .filter(|call| matches!(call, Call::CreateSpace(..)))
            .count();
        assert_eq!(creates, 1, "no second `shprd · <repo>` space is created");
    }

    /// A space made before marks existed is found by its old name and marked.
    #[test]
    fn an_unmarked_repo_space_is_found_by_name_and_gets_the_mark() {
        let scratch = Scratch::new();
        let mux = FakeMux::default();
        let name =
            repo_space_name(&repo_name(scratch.root()).unwrap_or_else(|err| panic!("{err}")));
        let old = mux
            .create_space(&name, scratch.root())
            .unwrap_or_else(|err| panic!("{err}"));
        let found = ensure_repo_space(&mux, scratch.root()).unwrap_or_else(|err| panic!("{err}"));
        assert!(!found.created);
        assert_eq!(found.space.id, old.id);
        mux.rename_space(&old.id, "renamed");
        let marked = mux
            .marked_space(&name)
            .unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(marked.map(|space| space.id), Some(old.id));
    }

    /// SHPR-72: a start that failed at first and succeeded on a retry (the space
    /// is no longer "just made") still ends with the assistant alone: the tab the
    /// window put in the space was remembered by the first look.
    #[test]
    fn a_retried_start_still_leaves_only_the_coordinator() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.spawn_env(&ctx);
        let mux = FakeMux::default().with_starter_tab().with_ready_agent();

        let first = ensure_repo_space(&mux, scratch.root()).unwrap_or_else(|err| panic!("{err}"));
        assert!(first.created);
        // The first start remembered the window's tab and then failed.
        remember_window_tabs(&mux, &env, &first);

        let again = ensure_repo_space(&mux, scratch.root()).unwrap_or_else(|err| panic!("{err}"));
        assert!(
            !again.created && !again.fresh,
            "a retry finds a marked space"
        );
        ensure_coordinator(&mux, &env, &again, |pending| pending.deliver(&mux))
            .unwrap_or_else(|err| panic!("{err}"));
        let tabs = mux
            .list_tabs(&again.space.id)
            .unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(tabs.len(), 1, "only the coordinator is left: {tabs:?}");
        assert_eq!(tabs[0].title, ASSISTANT_TITLE);
    }

    /// SHPR-88: a Shepherdr space adopted for the first time (not made by shprd,
    /// shown by the window under its folder's name) keeps the Assistant tab
    /// Shepherdr's agent chats in, with or without a terminal drawn in it; the
    /// empty Assistant tab no agent claims is still the window's and goes.
    #[test]
    fn an_adopted_space_keeps_the_assistant_tab_another_agent_claims() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.spawn_env(&ctx);
        let mux = FakeMux::default().with_ready_agent();
        let mut prepared =
            ensure_repo_space(&mux, scratch.root()).unwrap_or_else(|err| panic!("{err}"));
        prepared.created = false;
        prepared.fresh = true;
        let used = mux.with_mod_tab(&prepared.space.id, "tb-used", "Assistant");
        let empty = mux.with_mod_tab(&prepared.space.id, "tb-empty", "Assistant");
        mux.mark_foreign_tab(&used);
        let _ = mux.take_calls();

        ensure_coordinator(&mux, &env, &prepared, |pending| pending.deliver(&mux))
            .unwrap_or_else(|err| panic!("{err}"));

        let closed: Vec<_> = mux
            .calls()
            .into_iter()
            .filter_map(|call| match call {
                Call::CloseTab(tab) => Some(tab),
                _ => None,
            })
            .collect();
        assert!(
            !closed.contains(&used),
            "the claimed tab is left alone: {closed:?}"
        );
        assert!(
            closed.contains(&empty),
            "the empty one is the window's: {closed:?}"
        );
    }

    /// A space nobody just made or adopted is never tidied: its tabs are the
    /// person's.
    #[test]
    fn a_space_that_was_not_fresh_keeps_its_tabs() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.spawn_env(&ctx);
        let mux = FakeMux::default().with_starter_tab().with_ready_agent();
        let mut prepared =
            ensure_repo_space(&mux, scratch.root()).unwrap_or_else(|err| panic!("{err}"));
        prepared.created = false;
        prepared.fresh = false;
        ensure_coordinator(&mux, &env, &prepared, |pending| pending.deliver(&mux))
            .unwrap_or_else(|err| panic!("{err}"));
        let tabs = mux
            .list_tabs(&prepared.space.id)
            .unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(
            tabs.len(),
            2,
            "the person's tab and the coordinator: {tabs:?}"
        );
    }

    /// `shprd serve` starts the coordinator agent in the space it made, closes
    /// the starter shell it found there, and a restart finds it running instead
    /// of starting a second one.
    #[test]
    fn the_coordinator_is_started_once_and_reused_on_a_restart() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.spawn_env(&ctx);
        let mux = FakeMux::default().with_starter_tab().with_ready_agent();

        let first = ensure_repo_space(&mux, scratch.root()).unwrap_or_else(|err| panic!("{err}"));
        assert!(first.created);
        let starter = mux
            .list_tabs(&first.space.id)
            .unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(starter.len(), 1, "the host's starter shell");

        let started = ensure_coordinator(&mux, &env, &first, |pending| pending.deliver(&mux))
            .unwrap_or_else(|err| panic!("{err}"));
        let CoordinatorStart::Started(pane) = started else {
            panic!("a coordinator is started, not {started:?}");
        };
        let tabs = mux
            .list_tabs(&first.space.id)
            .unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(tabs.len(), 1, "only the coordinator is left: {tabs:?}");
        assert_eq!(tabs[0].root_pane, pane);
        assert_eq!(tabs[0].title, ASSISTANT_TITLE);
        let exe = std::env::current_exe().unwrap_or_default();
        let exe = exe.canonicalize().unwrap_or(exe);
        assert!(
            mux.calls().iter().any(|call| matches!(call,
                Call::Spawn(_, _, _, pairs, _)
                    if pairs.contains(&("SHPRD_BIN".to_string(), Some(exe.display().to_string())))
                        && pairs.iter().any(|(key, value)| key == "PATH"
                            && value.as_deref().is_some_and(|path| path.starts_with(&format!("{}:", exe.parent().map(|d| d.display().to_string()).unwrap_or_default())))))),
            "the coordinator carries the service's shprd"
        );
        assert!(
            mux.calls()
                .iter()
                .any(|call| matches!(call, Call::ShowChat(shown, space) if *shown == pane && space == "coordinator")),
            "the coordinator is shown as a chat"
        );

        // A restart: the space is there, the coordinator is running.
        let again = ensure_repo_space(&mux, scratch.root()).unwrap_or_else(|err| panic!("{err}"));
        assert!(!again.created);
        let calls_before = mux
            .calls()
            .iter()
            .filter(|call| matches!(call, Call::SpawnAgent(..)))
            .count();
        let reused = ensure_coordinator(&mux, &env, &again, |pending| pending.deliver(&mux))
            .unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(reused, CoordinatorStart::Running(pane.clone()));
        let calls_after = mux
            .calls()
            .iter()
            .filter(|call| matches!(call, Call::SpawnAgent(..)))
            .count();
        assert_eq!(calls_before, calls_after, "no second coordinator");

        // The coordinator ended: the next start makes a new one.
        mux.set_state(&pane, AgentState::Done);
        let replaced = ensure_coordinator(&mux, &env, &again, |pending| pending.deliver(&mux))
            .unwrap_or_else(|err| panic!("{err}"));
        assert!(
            matches!(replaced, CoordinatorStart::Started(_)),
            "{replaced:?}"
        );
    }

    /// A coordinator an earlier start left behind: running, with no record that
    /// names it.
    fn stray_coordinator(mux: &FakeMux, space: &shprd_mux::Space, repo: &Path) -> PaneId {
        mux.spawn_agent(&shprd_mux::AgentLaunch {
            space: &space.id,
            title: "Assistant",
            argv: &["claude".to_string()],
            env: &[],
            cwd: repo,
            role: shprd_mux::AgentRole::Coordinator,
            task: None,
            program: "claude",
            parent: None,
        })
        .unwrap_or_else(|err| panic!("{err}"))
        .root_pane
    }

    fn running(mux: &FakeMux) -> Vec<PaneId> {
        mux.list_coordinators()
            .unwrap_or_default()
            .into_iter()
            .filter(|agent| agent.live)
            .map(|agent| agent.tab.root_pane)
            .collect()
    }

    /// The service starts after earlier starts left coordinators running: it
    /// keeps one, ends the rest, and a later start adds none (SHPR-65 bug 3).
    #[test]
    fn old_coordinators_are_ended_and_a_restart_never_adds_one() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.spawn_env(&ctx);
        let mux = FakeMux::default().with_ready_agent();
        let prepared =
            ensure_repo_space(&mux, scratch.root()).unwrap_or_else(|err| panic!("{err}"));
        let old: Vec<PaneId> = (0..3)
            .map(|_| stray_coordinator(&mux, &prepared.space, scratch.root()))
            .collect();
        assert_eq!(running(&mux).len(), 3);

        let first = ensure_coordinator(&mux, &env, &prepared, |pending| pending.deliver(&mux))
            .unwrap_or_else(|err| panic!("{err}"));
        // The newest running one is kept and recorded; no fourth is started.
        assert_eq!(first, CoordinatorStart::Running(old[2].clone()));
        assert_eq!(running(&mux), vec![old[2].clone()], "one is left running");

        for _ in 0..3 {
            let again = ensure_coordinator(&mux, &env, &prepared, |pending| pending.deliver(&mux))
                .unwrap_or_else(|err| panic!("{err}"));
            assert_eq!(again, CoordinatorStart::Running(old[2].clone()));
        }
        assert_eq!(running(&mux).len(), 1, "restarts add none");
    }

    /// A running coordinator is found and typed into even when its record is
    /// missing or belongs to another space (SHPR-65 bug 2).
    #[test]
    fn a_running_coordinator_is_found_without_a_record_and_send_reaches_it() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let mux = FakeMux::default().with_ready_agent();
        let space = repo_space(&mux, &scratch);
        let pane = stray_coordinator(&mux, &space, scratch.root());
        let _ = mux.take_calls();
        prompt_coordinator(&mux, scratch.home.path(), &ctx, "hello")
            .unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(
            writes(&mux),
            vec![(pane.as_str().to_string(), "hello".to_string())]
        );
        // And it was recorded, so the next lookup is the fast one.
        let record = coordinator_record(scratch.home.path(), scratch.root(), &mux, &space);
        assert_eq!(record.map(|record| record.pane), Some(pane));
    }

    /// A coordinator that belongs to another repository's space is not this
    /// repository's, and is neither typed into nor ended.
    #[test]
    fn another_repositorys_coordinator_is_neither_found_nor_ended() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.spawn_env(&ctx);
        let mux = FakeMux::default().with_ready_agent();
        let other = mux
            .create_space("shprd · elsewhere", Path::new("/work/elsewhere"))
            .unwrap_or_else(|err| panic!("{err}"));
        let theirs = stray_coordinator(&mux, &other, Path::new("/work/elsewhere"));
        let prepared =
            ensure_repo_space(&mux, scratch.root()).unwrap_or_else(|err| panic!("{err}"));
        let started = ensure_coordinator(&mux, &env, &prepared, |pending| pending.deliver(&mux))
            .unwrap_or_else(|err| panic!("{err}"));
        assert!(
            matches!(started, CoordinatorStart::Started(_)),
            "{started:?}"
        );
        assert!(
            running(&mux).contains(&theirs),
            "the other repository's is untouched"
        );
        let _ = mux.take_calls();
        prompt_coordinator(&mux, scratch.home.path(), &ctx, "hi")
            .unwrap_or_else(|err| panic!("{err}"));
        assert_ne!(writes(&mux)[0].0, theirs.as_str());
    }

    /// A restart ends the running coordinator before it starts the new one, so
    /// the new one is the only one.
    #[test]
    fn a_coordinator_restart_leaves_exactly_one() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.spawn_env(&ctx);
        let mux = FakeMux::default().with_ready_agent();
        let prepared =
            ensure_repo_space(&mux, scratch.root()).unwrap_or_else(|err| panic!("{err}"));
        let first = ensure_coordinator(&mux, &env, &prepared, |pending| pending.deliver(&mux))
            .unwrap_or_else(|err| panic!("{err}"));
        restart_coordinator_with(&mux, &ctx, &env, |pending| pending.deliver(&mux))
            .unwrap_or_else(|err| panic!("{err}"));
        let now = running(&mux);
        assert_eq!(now.len(), 1, "{now:?}");
        assert_ne!(now[0], *first.pane(), "a new one, not the old");
    }

    /// Renaming the coordinator renames shprd's own tab and nothing else: a tab the
    /// person or another product made keeps its name, even one called the same.
    #[test]
    fn the_coordinators_tab_follows_its_name_and_no_other_tab_is_renamed() {
        let scratch = Scratch::new();
        let mux = FakeMux::default();
        let prepared =
            ensure_repo_space(&mux, scratch.root()).unwrap_or_else(|err| panic!("{err}"));
        let theirs = mux.with_tab_id(&prepared.space.id, "tb42", "Notes");
        mux.with_tab_id(&prepared.space.id, COORDINATOR_TAB, ASSISTANT_TITLE);
        let titles = |mux: &FakeMux| {
            mux.list_tabs(&prepared.space.id)
                .unwrap_or_else(|err| panic!("{err}"))
                .into_iter()
                .map(|tab| (tab.id.as_str().to_owned(), tab.title))
                .collect::<std::collections::BTreeMap<_, _>>()
        };
        assert_eq!(titles(&mux)[COORDINATOR_TAB], ASSISTANT_TITLE);

        let renamed = rename_coordinator_tab(&mux, scratch.root(), "Orchestrator")
            .unwrap_or_else(|err| panic!("{err}"));
        assert!(renamed);
        let after = titles(&mux);
        assert_eq!(after[COORDINATOR_TAB], "Orchestrator");
        assert_eq!(after[theirs.id.as_str()], "Notes", "their tab is untouched");

        // A tab that is not ours and has the wanted name is not displaced: ours
        // takes shprd's own fallback word.
        mux.with_tab_id(&prepared.space.id, "tb43", "QA");
        rename_coordinator_tab(&mux, scratch.root(), "QA").unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(titles(&mux)[COORDINATOR_TAB], "shprd");
        assert_eq!(titles(&mux)["tb43"], "QA");
    }

    /// With no coordinator tab of ours there is nothing to rename.
    #[test]
    fn there_is_no_tab_to_rename_before_the_coordinator_starts() {
        let scratch = Scratch::new();
        let mux = FakeMux::default();
        ensure_repo_space(&mux, scratch.root()).unwrap_or_else(|err| panic!("{err}"));
        assert!(
            !rename_coordinator_tab(&mux, scratch.root(), "Orchestrator")
                .unwrap_or_else(|err| panic!("{err}"))
        );
    }

    /// The coordinator's name is its role's: the built-in word until it is renamed.
    #[test]
    fn the_coordinators_name_comes_from_its_role() {
        use shprd_core::settings::{File, Request, resolve};
        assert_eq!(coordinator_name(None), "Assistant");
        let named = File::parse("[roles.coordinator]\ntitle = \"Orchestrator\"\n")
            .unwrap_or_else(|err| panic!("{err}"));
        let resolved = resolve(&named, &Request::default());
        assert_eq!(
            coordinator_name(resolved.role("coordinator").as_ref()),
            "Orchestrator"
        );
        let plain = resolve(&File::default(), &Request::default());
        assert_eq!(
            coordinator_name(plain.role("coordinator").as_ref()),
            "Assistant"
        );
    }

    /// The settings SHPR-69 was found with: the coordinator is `claude` with a
    /// model only claude knows, the worker is another program.
    const COORDINATOR_ON_CLAUDE: &str = "[roles.coordinator]\nagent = \"claude\"\n\
         model = \"opus[1m]\"\n[roles.worker]\nagent = \"sh\"\n";

    /// The coordinator's launch line, from the host's calls.
    fn coordinator_argv(mux: &FakeMux) -> Vec<String> {
        mux.calls()
            .into_iter()
            .rev()
            .find_map(|call| match call {
                Call::Spawn(_, _, argv, _, _) => Some(argv),
                _ => None,
            })
            .unwrap_or_default()
    }

    fn started(mux: &FakeMux, env: &SpawnEnv<'_>, scratch: &Scratch) -> Result<(), CliError> {
        let prepared = ensure_repo_space(mux, scratch.root()).unwrap_or_else(|err| panic!("{err}"));
        ensure_coordinator(mux, env, &prepared, |pending| pending.deliver(mux)).map(|_| ())
    }

    /// SHPR-69 bug 2: the coordinator's program is not installed, so it does
    /// not start as the worker's program (with the coordinator's model, which
    /// that program then refuses). It is an error that names the program.
    #[test]
    fn a_missing_coordinator_program_never_becomes_the_workers() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let mut env = scratch.spawn_env_with(&ctx, COORDINATOR_ON_CLAUDE);
        env.programs = vec![
            Program {
                spec: claude_spec(),
                installed: false,
            },
            Program {
                spec: fake_spec("sh"),
                installed: true,
            },
        ];
        env.workers = vec!["sh".to_owned()];
        let mux = FakeMux::default().with_ready_agent();

        let Err(err) = started(&mux, &env, &scratch) else {
            panic!("no coordinator program is installed");
        };

        let text = err.to_string();
        assert!(text.contains("claude"), "{text}");
        assert!(
            !mux.calls()
                .iter()
                .any(|call| matches!(call, Call::Spawn(..))),
            "nothing was started in the worker's place"
        );
    }

    /// A fallback program takes its own line's model, never the first line's.
    #[test]
    fn a_fallback_coordinator_program_never_gets_anothers_model() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let mut env = scratch.spawn_env_with(
            &ctx,
            "[roles.coordinator]\nagent = \"claude\"\nmodel = \"opus[1m]\"\n\
             fallback = [\"sh\"]\n",
        );
        env.programs = vec![
            Program {
                spec: claude_spec(),
                installed: false,
            },
            Program {
                spec: fake_spec("sh"),
                installed: true,
            },
        ];
        env.workers = Vec::new();
        let mux = FakeMux::default().with_ready_agent();

        started(&mux, &env, &scratch).unwrap_or_else(|err| panic!("{err}"));

        let argv = coordinator_argv(&mux);
        assert!(
            argv.iter().any(|arg| arg == "sh") && !argv.iter().any(|arg| arg.contains("opus")),
            "the fallback runs with its own default model: {argv:?}"
        );
    }

    /// With the program installed the configured model reaches it.
    #[test]
    fn the_coordinator_keeps_its_own_programs_model() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let mut env = scratch.spawn_env_with(&ctx, COORDINATOR_ON_CLAUDE);
        env.programs = vec![Program {
            spec: claude_spec(),
            installed: true,
        }];
        env.workers = Vec::new();
        let mux = FakeMux::default().with_ready_agent();

        started(&mux, &env, &scratch).unwrap_or_else(|err| panic!("{err}"));

        let argv = coordinator_argv(&mux);
        assert!(argv.iter().any(|arg| arg == "opus[1m]"), "{argv:?}");
    }

    /// The environment a launched program would inherit, with a private home so
    /// a trust entry never reaches the real `~/.claude.json`.
    fn private_env(env: &mut SpawnEnv<'_>, home: &Path) {
        env.base_env
            .insert("HOME".to_string(), home.display().to_string());
    }

    fn coordinator_env(mux: &FakeMux) -> Vec<(String, Option<String>)> {
        mux.calls()
            .into_iter()
            .find_map(|call| match call {
                Call::Spawn(_, _, _, env, _) => Some(env),
                _ => None,
            })
            .unwrap_or_default()
    }

    /// SHPR-71: a claude coordinator is launched without its edit tools and in
    /// a repository it already trusts, so no folder dialog opens.
    #[test]
    fn a_claude_coordinator_cannot_edit_and_its_folder_is_already_trusted() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let home = tempfile::TempDir::new().unwrap_or_else(|err| panic!("{err}"));
        let mut env = scratch.spawn_env_with(&ctx, COORDINATOR_ON_CLAUDE);
        private_env(&mut env, home.path());
        env.programs = vec![Program {
            spec: claude_spec(),
            installed: true,
        }];
        env.workers = Vec::new();
        let mux = FakeMux::default().with_ready_agent();

        started(&mux, &env, &scratch).unwrap_or_else(|err| panic!("{err}"));

        let argv = coordinator_argv(&mux);
        let at = argv
            .iter()
            .position(|arg| arg == "--disallowedTools")
            .unwrap_or_else(|| panic!("the edit tools are refused: {argv:?}"));
        assert!(argv[at + 1].starts_with("Edit,Write"), "{argv:?}");
        let text = std::fs::read_to_string(home.path().join(".claude.json"))
            .unwrap_or_else(|err| panic!("no trust entry written: {err}"));
        let doc: serde_json::Value =
            serde_json::from_str(&text).unwrap_or_else(|err| panic!("{err}"));
        let key = scratch.root().display().to_string();
        assert_eq!(doc["projects"][&key]["hasTrustDialogAccepted"], true);
    }

    /// SHPR-71: a claude worker's worktree is trusted too, and it is not
    /// refused its edit tools.
    #[test]
    fn a_claude_worker_is_trusted_in_its_worktree_and_keeps_its_edit_tools() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let home = tempfile::TempDir::new().unwrap_or_else(|err| panic!("{err}"));
        let mut env = scratch.spawn_env_with(&ctx, "[roles.worker]\nagent = \"claude\"\n");
        private_env(&mut env, home.path());
        env.programs = vec![Program {
            spec: claude_spec(),
            installed: true,
        }];
        let mux = FakeMux::default().with_ready_agent();
        mux.create_space(&repo_space_name(&scratch.repo_name()), scratch.root())
            .unwrap_or_else(|err| panic!("{err}"));

        let report = spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));

        let argv = spawn_argv(&mux.calls());
        assert!(
            !argv.iter().any(|arg| arg == "--disallowedTools"),
            "{argv:?}"
        );
        let text = std::fs::read_to_string(home.path().join(".claude.json"))
            .unwrap_or_else(|err| panic!("no trust entry written: {err}"));
        let doc: serde_json::Value =
            serde_json::from_str(&text).unwrap_or_else(|err| panic!("{err}"));
        let key = report.worktree.display().to_string();
        assert_eq!(doc["projects"][&key]["hasTrustDialogAccepted"], true);
    }

    /// SHPR-71: an opencode coordinator is told by its environment to deny
    /// edits; the workers it starts from its own shell are not.
    #[test]
    fn an_opencode_coordinators_edit_rule_never_reaches_its_workers() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let mut env = scratch.spawn_env_with(&ctx, "[roles.coordinator]\nagent = \"opencode\"\n");
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/harness");
        let opencode = shprd_agents::spec::load_dir(&dir)
            .unwrap_or_else(|err| panic!("{err}"))
            .into_iter()
            .find(|spec| spec.id == "opencode")
            .unwrap_or_else(|| panic!("no opencode spec"));
        env.programs = vec![Program {
            spec: opencode,
            installed: true,
        }];
        env.workers = Vec::new();
        let mux = FakeMux::default().with_ready_agent();
        started(&mux, &env, &scratch).unwrap_or_else(|err| panic!("{err}"));
        assert!(
            coordinator_env(&mux).contains(&(
                "OPENCODE_PERMISSION".to_string(),
                Some("{\"edit\":\"deny\"}".to_string())
            )),
            "{:?}",
            coordinator_env(&mux)
        );

        // The worker inherits the coordinator's shell: its launch removes it.
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let mut env = scratch.spawn_env(&ctx);
        env.base_env.insert(
            "OPENCODE_PERMISSION".to_string(),
            "{\"edit\":\"deny\"}".to_string(),
        );
        let mux = FakeMux::default().with_ready_agent();
        mux.create_space(&repo_space_name(&scratch.repo_name()), scratch.root())
            .unwrap_or_else(|err| panic!("{err}"));
        spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));
        assert!(
            coordinator_env(&mux).contains(&("OPENCODE_PERMISSION".to_string(), None)),
            "{:?}",
            coordinator_env(&mux)
        );
    }

    /// A program that dies as it starts is an error with its exit and what it
    /// said, its tab is cleared, and the next try starts it once it works.
    #[test]
    fn a_coordinator_that_exits_at_start_is_an_error_and_the_next_try_recovers() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let mut env = scratch.spawn_env(&ctx);
        env.workers = vec!["sh".to_owned()];
        let mux = FakeMux::default().with_ready_agent();
        mux.exit_coordinators_at_start(Some((
            Some(1),
            "Error: Model \"opus[1m]\" not found".to_owned(),
        )));

        let Err(err) = started(&mux, &env, &scratch) else {
            panic!("it exited at once");
        };

        let text = err.to_string();
        assert!(
            text.contains("exit 1") && text.contains("not found"),
            "{text}"
        );
        let space = repo_space(&mux, &scratch);
        assert!(
            mux.list_tabs(&space.id)
                .unwrap_or_default()
                .iter()
                .all(|tab| tab.title != "Assistant"),
            "the dead tab is cleared"
        );

        mux.exit_coordinators_at_start(None);
        started(&mux, &env, &scratch).unwrap_or_else(|err| panic!("{err}"));
        let prepared =
            ensure_repo_space(&mux, scratch.root()).unwrap_or_else(|err| panic!("{err}"));
        assert!(find_coordinator(&env.home, scratch.root(), &mux, &prepared.space).is_some());
    }

    /// SHPR-69 bug 1: a pane the host says is another product's agent is
    /// never found as the coordinator, even when a record names it.
    #[test]
    fn another_products_agent_is_never_the_recorded_coordinator() {
        let scratch = Scratch::new();
        let mux = FakeMux::default().with_starter_tab().with_ready_agent();
        let space = repo_space(&mux, &scratch);
        let pane = mux
            .list_tabs(&space.id)
            .unwrap_or_else(|err| panic!("{err}"))
            .first()
            .map(|tab| tab.root_pane.clone())
            .unwrap_or_else(|| panic!("no tab"));
        record_coordinator_pane(
            scratch.home.path(),
            scratch.root(),
            &space.id,
            &pane,
            "claude",
        )
        .unwrap_or_else(|err| panic!("{err}"));
        assert!(
            find_coordinator(scratch.home.path(), scratch.root(), &mux, &space).is_some(),
            "before the host says it is foreign, the record is trusted"
        );
        mux.mark_foreign(&pane);
        assert!(
            find_coordinator(scratch.home.path(), scratch.root(), &mux, &space).is_none(),
            "a foreign agent is never typed into"
        );
    }

    /// A tab named "Assistant" that shprd did not make is another product's: the
    /// coordinator's tab takes another name beside it, and nothing of the other
    /// tab is renamed or closed. An older shprd's `coordinator` tab is replaced.
    #[test]
    fn another_products_assistant_tab_is_left_alone_and_the_old_tab_goes() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.spawn_env(&ctx);
        let mux = FakeMux::default().with_ready_agent();
        // A space that was already there, so no tab in it is a starter shell.
        let mut prepared =
            ensure_repo_space(&mux, scratch.root()).unwrap_or_else(|err| panic!("{err}"));
        prepared.created = false;
        let foreign = mux.with_tab_id(&prepared.space.id, "tb42", "Assistant");
        let elsewhere = mux.with_tab_id(&prepared.space.id, "tb43", "coordinator");
        let old = mux.with_tab_id(&prepared.space.id, OLD_COORDINATOR_TAB, "coordinator");
        let _ = mux.take_calls();

        ensure_coordinator(&mux, &env, &prepared, |pending| pending.deliver(&mux))
            .unwrap_or_else(|err| panic!("{err}"));

        let tabs = mux
            .list_tabs(&prepared.space.id)
            .unwrap_or_else(|err| panic!("{err}"));
        let titled = |title: &str| tabs.iter().filter(|tab| tab.title == title).count();
        assert_eq!(titled("Assistant"), 1, "only the other product's: {tabs:?}");
        assert_eq!(titled("shprd"), 1, "shprd's own tab takes another name");
        assert!(tabs.iter().any(|tab| tab.id == foreign.id), "{tabs:?}");
        assert!(
            tabs.iter().any(|tab| tab.id == elsewhere.id),
            "a tab that only shares the old name is not shprd's to close"
        );
        assert!(!tabs.iter().any(|tab| tab.id == old.id), "the old tab went");
        let closed: Vec<_> = mux
            .calls()
            .into_iter()
            .filter_map(|call| match call {
                Call::CloseTab(tab) => Some(tab),
                _ => None,
            })
            .collect();
        assert_eq!(closed, vec![old.id], "nothing else was closed");
    }

    /// News and chat are never typed into a shell: a record without the agent
    /// mark (an older build's starter shell), or one whose coordinator has
    /// ended, is refused.
    #[test]
    fn a_shell_or_an_ended_coordinator_is_never_typed_into() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let mux = FakeMux::default().with_starter_tab().with_ready_agent();
        let space = repo_space(&mux, &scratch);
        let pane = mux
            .list_tabs(&space.id)
            .unwrap_or_else(|err| panic!("{err}"))
            .first()
            .map(|tab| tab.root_pane.clone())
            .unwrap_or_else(|| panic!("a pane"));
        let path = coordinator_pane_path(scratch.home.path(), scratch.root(), &space.id);
        std::fs::create_dir_all(path.parent().unwrap_or(scratch.home.path()))
            .unwrap_or_else(|err| panic!("{err}"));
        std::fs::write(&path, format!("{}\n", pane.as_str())).unwrap_or_else(|err| panic!("{err}"));
        assert!(
            prompt_coordinator(&mux, scratch.home.path(), &ctx, "hi").is_err(),
            "a bare record is a shell"
        );
        record_coordinator_pane(scratch.home.path(), scratch.root(), &space.id, &pane, "sh")
            .unwrap_or_else(|err| panic!("{err}"));
        mux.set_state(&pane, AgentState::Done);
        assert!(
            prompt_coordinator(&mux, scratch.home.path(), &ctx, "hi").is_err(),
            "an ended coordinator is not typed into"
        );
        assert!(writes(&mux).is_empty(), "nothing was typed");
    }

    /// Two repositories on one machine keep their own records: a second
    /// service recording its coordinator never rewrites the first's, and each
    /// reader finds its own.
    #[test]
    fn two_repositories_keep_their_own_coordinator_records() {
        let home = TempDir::new().unwrap_or_else(|err| panic!("{err}"));
        let (repo_a, repo_b) = (
            TempDir::new().unwrap_or_else(|err| panic!("{err}")),
            TempDir::new().unwrap_or_else(|err| panic!("{err}")),
        );
        let mux_a = FakeMux::default().with_starter_tab();
        let mux_b = FakeMux::default().with_starter_tab();
        let space_a = mux_a
            .create_space("a", repo_a.path())
            .unwrap_or_else(|err| panic!("{err}"));
        let space_b = mux_b
            .create_space("b", repo_b.path())
            .unwrap_or_else(|err| panic!("{err}"));
        let first = |mux: &FakeMux, space: &shprd_mux::Space| {
            mux.list_tabs(&space.id)
                .unwrap_or_else(|err| panic!("{err}"))
                .first()
                .map(|tab| tab.root_pane.clone())
                .unwrap_or_else(|| panic!("a pane"))
        };
        let (pane_a, pane_b) = (first(&mux_a, &space_a), first(&mux_b, &space_b));
        record_coordinator_pane(home.path(), repo_a.path(), &space_a.id, &pane_a, "claude")
            .unwrap_or_else(|err| panic!("{err}"));
        record_coordinator_pane(home.path(), repo_b.path(), &space_b.id, &pane_b, "codex")
            .unwrap_or_else(|err| panic!("{err}"));

        assert_ne!(
            coordinator_pane_path(home.path(), repo_a.path(), &space_a.id),
            coordinator_pane_path(home.path(), repo_b.path(), &space_b.id)
        );
        assert_eq!(
            recorded_coordinator_pane(home.path(), repo_a.path(), &mux_a, &space_a),
            Some(pane_a.clone())
        );
        assert_eq!(
            recorded_coordinator_pane(home.path(), repo_b.path(), &mux_b, &space_b),
            Some(pane_b)
        );
        let record = coordinator_record(home.path(), repo_a.path(), &mux_a, &space_a);
        assert_eq!(
            record.and_then(|r| r.program).as_deref(),
            Some("claude"),
            "the program is the repository's own"
        );
    }

    /// Space ids that differ only in characters a path cannot hold still get
    /// their own record paths.
    #[test]
    fn distinct_space_ids_never_share_a_record_path() {
        let home = Path::new("/h");
        let repo = Path::new("/r/x");
        let a = coordinator_pane_path(home, repo, &shprd_mux::SpaceId::new("space/a"));
        let b = coordinator_pane_path(home, repo, &shprd_mux::SpaceId::new("space_a"));
        assert_ne!(a, b);
    }

    /// The old machine-wide file is migrated only when it names a pane alive in
    /// this space, and is never deleted.
    #[test]
    fn the_old_record_is_migrated_only_for_a_live_pane_of_this_space() {
        let scratch = Scratch::new();
        let mux = FakeMux::default().with_starter_tab();
        let space = repo_space(&mux, &scratch);
        let pane = mux
            .list_tabs(&space.id)
            .unwrap_or_else(|err| panic!("{err}"))
            .first()
            .map(|tab| tab.root_pane.clone())
            .unwrap_or_else(|| panic!("a pane"));
        let legacy = scratch.home.path().join(".shprd").join("coordinator.pane");
        std::fs::create_dir_all(legacy.parent().unwrap_or(scratch.home.path()))
            .unwrap_or_else(|err| panic!("{err}"));
        let new_path = coordinator_pane_path(scratch.home.path(), scratch.root(), &space.id);

        // Another service's pane: ignored, nothing written.
        std::fs::write(&legacy, "p-elsewhere\nagent\nclaude\n")
            .unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(
            recorded_coordinator_pane(scratch.home.path(), scratch.root(), &mux, &space),
            None
        );
        assert!(!new_path.exists(), "a foreign record is not migrated");

        // This space's live pane: migrated, the old file stays.
        std::fs::write(&legacy, format!("{}\nagent\nclaude\n", pane.as_str()))
            .unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(
            recorded_coordinator_pane(scratch.home.path(), scratch.root(), &mux, &space),
            Some(pane)
        );
        assert!(new_path.exists(), "the record is migrated");
        assert!(legacy.exists(), "the old file is left alone");
    }

    /// A run that recorded its own coordinator pane wins over the service's
    /// record: it is the pane a launcher owns.
    #[test]
    fn the_runs_recorded_coordinator_pane_wins() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let mux = FakeMux::default().with_starter_tab();
        let space = repo_space(&mux, &scratch);
        let _ = record_first_tab(scratch.home.path(), scratch.root(), &mux, &space);
        let launched = mux
            .spawn(
                &space.id,
                "★ coordinator",
                &[String::from("sh")],
                &[],
                scratch.root(),
            )
            .unwrap_or_else(|err| panic!("{err}"));
        ctx.commit(EventKind::OrchestratorLaunched {
            title: launched.title.clone(),
            pane: launched.root_pane.as_str().to_string(),
            process_group: 1,
        })
        .unwrap_or_else(|err| panic!("{err}"));
        let _ = mux.take_calls();

        prompt_coordinator(&mux, scratch.home.path(), &ctx, "hello")
            .unwrap_or_else(|err| panic!("{err}"));

        assert_eq!(
            writes(&mux),
            vec![(launched.root_pane.as_str().to_string(), "hello".to_string())],
            "the run's own pane is used, not the service's record"
        );
    }

    #[test]
    fn spawn_creates_a_worktree_and_records_it() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.spawn_env(&ctx);
        let mux = FakeMux::default().with_ready_agent();

        let report = spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));

        assert_eq!(report.attempt, 1);
        assert_eq!(report.agent, "sh");
        assert_eq!(report.tab, "sh·T1");
        assert!(
            report.worktree.is_dir(),
            "no worktree at {:?}",
            report.worktree
        );
        assert_eq!(report.branch, "shprd/t1/T1");
        assert_eq!(report.skills, vec!["tdd"]);
        let brief = std::fs::read_to_string(&report.brief).unwrap_or_default();
        // The role's own words come first: the shipped worker prompt is
        // installed on first use and read into the brief.
        assert!(
            brief.starts_with("You are a shprd worker") && !brief.contains("<!--"),
            "the shipped worker prompt leads the brief, without its credit comment: {brief}"
        );
        assert!(brief.contains("You work on one shprd task"), "{brief}");
        assert!(brief.contains("Make spawn real"), "{brief}");
        assert!(brief.contains("crates/shprd"), "{brief}");
        // And the Settings block names the program this spawn chose as the
        // task's own, with the inherited values below it.
        assert!(brief.contains("## Your settings"), "{brief}");
        assert!(brief.contains("- Program: sh (this task)"), "{brief}");
        assert!(brief.contains("- Merge: ask (built in)"), "{brief}");
        // The tab id is recorded beside the brief, so a later prompt reaches
        // this exact tab even when another tab carries the same title.
        let recorded = tab_path(
            &scratch.home.path().join(".shprd").join("briefs"),
            "t1",
            "T1",
            1,
        );
        let tab_id = std::fs::read_to_string(&recorded).unwrap_or_default();
        let space = agent_space(&mux, &env).unwrap_or_else(|err| panic!("{err}"));
        let _ = &space;
        let tabs = all_handles(&mux);
        assert!(
            tabs.iter().any(|tab| tab.id.as_str() == tab_id),
            "{tab_id:?} names no tab in the task's space {tabs:?}"
        );

        let state = ctx.state().unwrap_or_else(|err| panic!("{err}"));
        let task = state.tasks.get("T1").unwrap_or_else(|| panic!("no task"));
        assert_eq!(task.status, TaskStatus::Working);
        assert_eq!(task.attempts.len(), 1);
        let resources: Vec<(&ResourceKind, &str)> = state
            .resources
            .keys()
            .map(|(kind, name)| (kind, name.as_str()))
            .collect();
        assert!(resources.contains(&(
            &ResourceKind::Worktree,
            report.worktree.to_str().unwrap_or_default()
        )));
        assert!(resources.contains(&(&ResourceKind::Branch, "shprd/t1/T1")));

        let calls = mux.calls();
        let tab = calls
            .iter()
            .find_map(|call| match call {
                Call::Spawn(owner, title, _argv, _env, cwd) => {
                    Some((owner.clone(), title.clone(), cwd.clone()))
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("no tab was created: {calls:?}"));
        let name = repo_space_name(&scratch.repo_name());
        let space = mux
            .find_space(&name)
            .unwrap_or_else(|err| panic!("{err}"))
            .unwrap_or_else(|| panic!("no space {name:?}"));
        assert_eq!(
            tab.0, space.id,
            "the agent is started for the repository's one space"
        );
        assert!(
            mux.list_tabs(&space.id).unwrap_or_default().is_empty(),
            "a worker gets no tab"
        );
        assert_eq!(mux.list_agents().unwrap_or_default().len(), 1);
        assert_eq!(tab.1, "sh·T1");
        assert_eq!(tab.2, report.worktree);
        let (argv, pairs) = calls
            .iter()
            .find_map(|call| match call {
                Call::Spawn(_, _, argv, pairs, _) => Some((argv.clone(), pairs.clone())),
                _ => None,
            })
            .unwrap_or_else(|| panic!("the CLI was not started: {calls:?}"));
        assert_eq!(argv, vec!["sh".to_string()]);
        assert!(pairs.contains(&("SHPRD_TASK".to_string(), Some("T1".to_string()))));
        assert!(pairs.contains(&("SHPRD_RUN".to_string(), Some("t1".to_string()))));
        assert!(pairs.contains(&("SHPRD_ACTOR".to_string(), Some("agent:sh".to_string()))));
        // The agent's `shprd` is this service's own, first on its PATH.
        let exe = std::env::current_exe().unwrap_or_default();
        let exe = exe.canonicalize().unwrap_or(exe);
        assert!(
            pairs.contains(&("SHPRD_BIN".to_string(), Some(exe.display().to_string()))),
            "{pairs:?}"
        );
        let dir = exe
            .parent()
            .map(|dir| dir.display().to_string())
            .unwrap_or_default();
        assert!(
            pairs.iter().any(|(key, value)| key == "PATH"
                && value
                    .as_deref()
                    .is_some_and(|path| path.starts_with(&format!("{dir}:")) || path == dir)),
            "{pairs:?}"
        );
        // The worker's own `shprd` must resolve this engine's config and
        // harnesses, not whatever `$HOME` holds (bug 11).
        assert!(
            pairs.contains(&(
                "SHPRD_HOME".to_string(),
                Some(env.home.display().to_string())
            )),
            "{pairs:?}"
        );
        assert!(
            pairs.contains(&(
                "SHPRD_DATA_DIR".to_string(),
                env.data_dir
                    .env_value()
                    .map(|dir| dir.display().to_string())
            )),
            "{pairs:?}"
        );
        assert!(!pairs.contains(&("GH_TOKEN".to_string(), Some("secret".to_string()))));
        assert!(pairs.contains(&("GH_TOKEN".to_string(), None)));
        // The spec has no system-prompt arguments, so the brief is typed in.
        let prompt = calls
            .iter()
            .find_map(|call| match call {
                Call::Write(_, text, _) => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("the brief was never typed in: {calls:?}"));
        assert!(prompt.contains("Make spawn real"), "{prompt}");
    }

    /// Runs one git command in `dir`, returning its trimmed stdout when it
    /// succeeds and `None` when git refuses.
    fn git_try(dir: &Path, args: &[&str]) -> Option<String> {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output()
            .unwrap_or_else(|err| panic!("git {args:?} failed: {err}"));
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Runs one git command in `dir` and returns its trimmed stdout, failing
    /// the test when git refuses.
    fn git_out(dir: &Path, args: &[&str]) -> String {
        git_try(dir, args).unwrap_or_else(|| panic!("git {args:?} refused in {}", dir.display()))
    }

    /// Commits `name` in `dir` and answers the commit's sha.
    fn commit_in(dir: &Path, name: &str, body: &str) -> String {
        std::fs::write(dir.join(name), body).unwrap_or_else(|err| panic!("{err}"));
        git(dir, &["add", "."]);
        git(dir, &["commit", "-q", "-m", name]);
        git_out(dir, &["rev-parse", "HEAD"])
    }

    /// A host with the repository's workspace open, the way a launcher leaves it.
    fn host(scratch: &Scratch) -> FakeMux {
        let mux = FakeMux::default().with_ready_agent();
        mux.create_space(&repo_space_name(&scratch.repo_name()), scratch.root())
            .unwrap_or_else(|err| panic!("{err}"));
        mux
    }

    /// Records a completed report for `attempt`, which leaves the task Reported.
    fn report_completed(ctx: &Ctx, attempt: u32) {
        ctx.commit(EventKind::Reported {
            task: "T1".to_string(),
            attempt,
            status: ReportStatus::Completed,
            summary: "done".to_string(),
            tests: vec!["cargo test".to_string()],
            checks: vec![shprd_core::event::CheckReport::passed(
                "tests",
                "cargo test",
            )],
            changed_paths: vec!["crates/shprd/src/cmd/spawn.rs".to_string()],
            commit: None,
            skills_used: Vec::new(),
            proofs: Vec::new(),
        })
        .unwrap_or_else(|err| panic!("{err}"));
    }

    /// Fails `attempt`'s review, which is what sends a task back for a retry.
    fn fail_review(ctx: &Ctx, attempt: u32) {
        report_completed(ctx, attempt);
        let engine = Ctx::new(ctx.root.clone(), Actor::Engine, false, true);
        let reviewer = "codex".to_string();
        let request = engine
            .commit(EventKind::ReviewRequested {
                task: "T1".to_string(),
                attempt,
                reviewer: reviewer.clone(),
                degraded: false,
                note: None,
            })
            .unwrap_or_else(|err| panic!("{err}"));
        engine
            .commit(EventKind::GateFailed {
                task: "T1".to_string(),
                attempt,
                gate: "review".to_string(),
                findings: vec![Finding {
                    severity: Severity::Major,
                    title: "the retry cannot start".to_string(),
                    location: Some("crates/shprd/src/cmd/spawn.rs:142".to_string()),
                    detail: "archive the failed attempt before making a new one".to_string(),
                }],
                source: Some(ReviewSource {
                    reviewer,
                    findings_sha256: "ab".repeat(32),
                    request_seq: request.seq,
                }),
            })
            .unwrap_or_else(|err| panic!("{err}"));
    }

    /// A person's stop leaves the attempt replaceable: the retry archives it,
    /// removes its worktree and branch, and starts attempt 2 in a fresh one.
    ///
    /// It is the half of M10 gate bug 1 the service test cannot see: the API
    /// refuses a retry only when the spawn path refuses it, and the spawn path
    /// refuses one only while the attempt is still in flight.
    #[test]
    fn a_stop_leaves_the_attempt_replaceable_and_a_retry_starts() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.spawn_env(&ctx);
        let mux = host(&scratch);
        let first = spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));
        let tip = commit_in(&first.worktree, "stopped.txt", "attempt one\n");
        ctx.commit(EventKind::TaskStopped {
            task: "T1".to_string(),
            attempt: 1,
        })
        .unwrap_or_else(|err| panic!("{err}"));

        let second = spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));

        assert_eq!(second.attempt, 2, "the retry is a new attempt");
        let replaced = second
            .replaced
            .clone()
            .unwrap_or_else(|| panic!("the retry must name the ref it archived"));
        assert_eq!(replaced.ref_name, "refs/shprd/archive/t1/T1-a1");
        // The stopped attempt's commits are kept, reachable from its ref.
        assert_eq!(
            git_out(
                scratch.root(),
                &["rev-parse", "--verify", "refs/shprd/archive/t1/T1-a1"]
            ),
            tip
        );
        // The repository's one space is still there, and the retry's worker is
        // headless like the first: no space or tab came back for the task.
        let name = repo_space_name(&env.repo_name);
        assert!(
            mux.find_space(&name)
                .unwrap_or_else(|err| panic!("{err}"))
                .is_some(),
            "the repository's space stays"
        );
        assert_eq!(mux.list_spaces().unwrap_or_default().len(), 1);
    }

    /// Every resource event the log holds, in order.
    fn resource_events(ctx: &Ctx) -> Vec<(ResourceAction, ResourceKind, String)> {
        let (_, events) = ctx.load().unwrap_or_else(|err| panic!("{err}"));
        events
            .iter()
            .filter_map(|event| match &event.kind {
                EventKind::Resource {
                    action, kind, name, ..
                } => Some((*action, *kind, name.clone())),
                _ => None,
            })
            .collect()
    }

    /// A failed review sends the attempt back, and the retry has to take over
    /// the same worktree path and branch name that attempt used. So the retry
    /// archives that attempt's branch first, then removes its worktree and
    /// branch, records all three, and only then creates the fresh pair.
    #[test]
    fn a_retry_after_a_failed_review_archives_and_replaces_the_attempt() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.spawn_env(&ctx);
        let mux = host(&scratch);
        let first = spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));
        assert!(first.replaced.is_none(), "a first attempt replaces nothing");
        let tip = commit_in(&first.worktree, "retry.txt", "attempt one\n");
        // An uncommitted change to a tracked file is what keeps the worktree
        // from being reused, so this is the replace path.
        std::fs::write(first.worktree.join("retry.txt"), "unsaved\n")
            .unwrap_or_else(|err| panic!("{err}"));
        fail_review(&ctx, 1);

        let second = spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));

        assert_eq!(second.attempt, 2);
        let replaced = second
            .replaced
            .clone()
            .unwrap_or_else(|| panic!("the retry must name the ref it archived"));
        assert_eq!(replaced.attempt, 1);
        assert!(replaced.reused.is_none());
        assert_eq!(replaced.ref_name, "refs/shprd/archive/t1/T1-a1");
        assert_eq!(
            replaced.line("janitor-reviews"),
            "archived attempt 1 of janitor-reviews as refs/shprd/archive/t1/T1-a1"
        );
        // The replaced attempt's commits are still reachable from its ref.
        assert_eq!(
            git_out(
                scratch.root(),
                &["rev-parse", "--verify", "refs/shprd/archive/t1/T1-a1"]
            ),
            tip
        );
        // The retry continues from the replaced attempt's last commit: its work is
        // in the new worktree and its branch, and the archive ref keeps it too.
        assert!(second.worktree.is_dir(), "the new worktree is missing");
        assert!(
            second.worktree.join("retry.txt").exists(),
            "the retry lost the replaced attempt's work"
        );
        assert_eq!(git_out(scratch.root(), &["rev-parse", &second.branch]), tip);
        assert_eq!(replaced.carried.as_deref(), Some(tip.as_str()));
        // The brief names the ref, so the worker can cherry-pick what is useful.
        let brief = std::fs::read_to_string(&second.brief).unwrap_or_default();
        assert!(brief.contains("refs/shprd/archive/t1/T1"), "{brief}");
        assert!(brief.contains("git cherry-pick"), "{brief}");
        // The ledger records the archive, then the two removals, then the fresh
        // pair — and the archive precedes the deletion it licenses.
        let events = resource_events(&ctx);
        let archive = events
            .iter()
            .position(|event| {
                *event
                    == (
                        ResourceAction::Archived,
                        ResourceKind::Branch,
                        "shprd/t1/T1".to_string(),
                    )
            })
            .unwrap_or_else(|| panic!("no archive of the failed branch: {events:?}"));
        let removed = events
            .iter()
            .position(|event| {
                *event
                    == (
                        ResourceAction::Removed,
                        ResourceKind::Branch,
                        "shprd/t1/T1".to_string(),
                    )
            })
            .unwrap_or_else(|| panic!("no removal of the failed branch: {events:?}"));
        assert!(
            archive < removed,
            "the branch was deleted unarchived: {events:?}"
        );
        assert!(
            events.contains(&(
                ResourceAction::Created,
                ResourceKind::ArchiveRef,
                "refs/shprd/archive/t1/T1-a1".to_string()
            )),
            "{events:?}"
        );
        assert!(
            events.contains(&(
                ResourceAction::Removed,
                ResourceKind::Worktree,
                first.worktree.display().to_string()
            )),
            "{events:?}"
        );
        let state = ctx.state().unwrap_or_else(|err| panic!("{err}"));
        let live = |kind: ResourceKind, name: &str| {
            state
                .resources
                .get(&(kind, name.to_string()))
                .map(|record| record.status)
        };
        assert_eq!(
            live(ResourceKind::ArchiveRef, "refs/shprd/archive/t1/T1-a1"),
            Some(ResourceStatus::Live)
        );
        assert_eq!(
            live(ResourceKind::Branch, "shprd/t1/T1"),
            Some(ResourceStatus::Live)
        );
        assert_eq!(
            live(
                ResourceKind::Worktree,
                &second.worktree.display().to_string()
            ),
            Some(ResourceStatus::Live)
        );
    }

    /// A retry works in the worktree the failed attempt had, build output and
    /// all, brought up to date with the integration branch.
    #[test]
    fn a_retry_reuses_the_worktree_and_its_build_and_brings_it_up_to_date() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.spawn_env(&ctx);
        let mux = host(&scratch);
        let first = spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));
        let tip = commit_in(&first.worktree, "retry.txt", "attempt one\n");
        // A build the new attempt should not have to repeat.
        std::fs::create_dir_all(first.worktree.join("target")).unwrap_or_default();
        std::fs::write(first.worktree.join("target/built"), "x").unwrap_or_default();
        // Work lands on the integration branch after the task was cut.
        git(scratch.root(), &["checkout", "-q", "integration/m4"]);
        std::fs::write(scratch.root().join("landed.txt"), "landed\n").unwrap_or_default();
        git(scratch.root(), &["add", "landed.txt"]);
        git(scratch.root(), &["commit", "-q", "-m", "landed"]);
        git(scratch.root(), &["checkout", "-q", "main"]);
        fail_review(&ctx, 1);

        let second = spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));

        assert_eq!(second.attempt, 2);
        assert_eq!(second.worktree, first.worktree, "the worktree is kept");
        assert!(
            second.worktree.join("target/built").exists(),
            "the build output is gone"
        );
        assert!(
            second.worktree.join("landed.txt").exists(),
            "not up to date"
        );
        assert!(second.worktree.join("retry.txt").exists(), "work was lost");
        // The earlier attempt's commits are still kept under its archive ref.
        assert_eq!(
            git_out(
                scratch.root(),
                &["rev-parse", "--verify", "refs/shprd/archive/t1/T1-a1"]
            ),
            tip
        );
        let events = resource_events(&ctx);
        assert!(
            !events
                .iter()
                .any(|event| event.0 == ResourceAction::Removed),
            "a kept worktree is not recorded as removed: {events:?}"
        );
        let state = ctx.state().unwrap_or_else(|err| panic!("{err}"));
        let worktree = state
            .resources
            .get(&(
                ResourceKind::Worktree,
                second.worktree.display().to_string(),
            ))
            .map(|record| record.status);
        assert_eq!(worktree, Some(ResourceStatus::Live));
    }

    /// A task redone after a clash starts from a clean base: its worktree is
    /// replaced, not reused.
    #[test]
    fn a_redo_after_a_clash_gets_a_fresh_worktree() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.spawn_env(&ctx);
        let mux = host(&scratch);
        let first = spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));
        commit_in(&first.worktree, "retry.txt", "attempt one\n");
        std::fs::create_dir_all(first.worktree.join("target")).unwrap_or_default();
        std::fs::write(first.worktree.join("target/built"), "x").unwrap_or_default();
        report_completed(&ctx, 1);
        Ctx::new(ctx.root.clone(), Actor::Engine, false, true)
            .commit(EventKind::GateFailed {
                task: "T1".to_string(),
                attempt: 1,
                gate: shprd_core::rules::LANDING_CONFLICT_GATE.to_string(),
                findings: Vec::new(),
                source: None,
            })
            .unwrap_or_else(|err| panic!("{err}"));

        let second = spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));

        let replaced = second
            .replaced
            .unwrap_or_else(|| panic!("a replaced attempt"));
        assert!(replaced.reused.is_none(), "a clash needs a clean base");
        assert!(!second.worktree.join("target/built").exists());
    }

    /// Starting past `tasks.max_running` waits as queued, on whichever path.
    #[test]
    fn a_start_beyond_max_running_waits_as_queued() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        ctx.commit(EventKind::TaskAdded {
            task: "T2".to_string(),
            title: "Second".to_string(),
            scope: Vec::new(),
            acceptance: vec!["it runs".to_string()],
            tags: Vec::new(),
            skills: Vec::new(),
            deps: Vec::new(),
        })
        .unwrap_or_else(|err| panic!("{err}"));
        let env = scratch.spawn_env_with(&ctx, "[tasks]\nmax_running = 1\n");
        let mux = host(&scratch);
        let state = ctx.state().unwrap_or_else(|err| panic!("{err}"));
        assert!(
            queued_for_slot(&env, &state, "T2").is_none(),
            "a slot is free"
        );
        spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));

        let state = ctx.state().unwrap_or_else(|err| panic!("{err}"));
        let queued = queued_for_slot(&env, &state, "T2")
            .unwrap_or_else(|| panic!("the second start must wait"));
        assert!(queued.starts_with("queued: 1 of 1"), "{queued}");
        // A caller that never asked still cannot start past the cap: spawn
        // itself refuses, and nothing is recorded.
        let refused = spawn(
            &mux,
            &env,
            &SpawnRequest {
                task: "T2".to_string(),
                agent: None,
                effort: None,
            },
        );
        match refused {
            Err(err) => assert!(err.to_string().contains("queued: 1 of 1"), "{err}"),
            Ok(_) => panic!("spawn must refuse a start past the cap"),
        }
        let after = ctx.state().unwrap_or_else(|err| panic!("{err}"));
        assert!(after.tasks["T2"].attempts.is_empty());
        // The running task's own retry is not counted against itself.
        assert!(queued_for_slot(&env, &state, "T1").is_none());
        // The answer is kept: the service's state directory now lists T2, so a
        // later pass starts it when a slot is free.
        let line = queue_start(&ctx, Some(scratch.home.path()), "T2", queued.clone());
        assert_eq!(line, queued);
        let dir = shprd_server::repo_state_dir(
            &crate::ctx::resolve_state_home(scratch.home.path()),
            &ctx.root,
        );
        assert_eq!(shprd_server::start_queue::waiting(&dir), ["T2"]);
    }

    /// A retry answers the open "review rounds spent" ask, so the next review
    /// never finds it unanswered.
    #[test]
    fn a_retry_closes_the_open_review_out_ask_with_the_retry_answer() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.spawn_env(&ctx);
        let mux = host(&scratch);
        spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));
        fail_review(&ctx, 1);
        let ask = shprd_core::gate::rounds_spent_ask(1);
        Ctx::new(ctx.root.clone(), Actor::Engine, false, true)
            .commit(EventKind::AskRaised {
                task: "T1".to_string(),
                ask: ask.clone(),
                ask_kind: shprd_core::ask::AskKind::Question,
                text: "review rounds spent".to_string(),
                options: shprd_core::gate::ROUNDS_SPENT_OPTIONS
                    .iter()
                    .map(|choice| (*choice).to_string())
                    .collect(),
                created_at_ms: 1_000,
                expires_at_ms: None,
            })
            .unwrap_or_else(|err| panic!("{err}"));

        spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));

        let state = ctx.state().unwrap_or_else(|err| panic!("{err}"));
        let record = state
            .asks
            .get(&("T1".to_string(), ask))
            .unwrap_or_else(|| panic!("the ask is in the log"));
        let answer = record.answer.as_ref().unwrap_or_else(|| panic!("closed"));
        assert_eq!(answer.text, "retry with another round");
    }

    /// A retry's brief, written by the spawn path itself, carries the ask the
    /// person answered in attempt 1, so the worker never asks it again.
    #[test]
    fn a_retry_brief_carries_an_ask_answered_in_the_earlier_attempt() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.spawn_env(&ctx);
        let mux = host(&scratch);
        let first = spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));
        let first_brief = std::fs::read_to_string(&first.brief).unwrap_or_default();
        assert!(!first_brief.contains("already answered"), "{first_brief}");
        ctx.commit(EventKind::AskRaised {
            task: "T1".to_string(),
            ask: "dictation".to_string(),
            ask_kind: shprd_core::ask::AskKind::Question,
            text: "which microphone?".to_string(),
            options: Vec::new(),
            created_at_ms: 1_000,
            expires_at_ms: None,
        })
        .unwrap_or_else(|err| panic!("{err}"));
        ctx.commit(EventKind::AskAnswered {
            task: "T1".to_string(),
            ask: "dictation".to_string(),
            answer: "use the built-in mic".to_string(),
        })
        .unwrap_or_else(|err| panic!("{err}"));
        fail_review(&ctx, 1);

        let second = spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));

        assert_eq!(second.attempt, 2);
        let brief = std::fs::read_to_string(&second.brief).unwrap_or_default();
        assert!(brief.contains("which microphone?"), "{brief}");
        assert!(brief.contains("use the built-in mic"), "{brief}");
    }

    /// Blocker 3: a retry carries the earlier attempt's commits forward, and a
    /// third attempt carries both, with each attempt's archive ref kept.
    #[test]
    fn a_retry_starts_from_the_previous_attempts_commits() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.spawn_env(&ctx);
        let mux = host(&scratch);
        let first = spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));
        commit_in(&first.worktree, "one.txt", "one\n");
        let tip = commit_in(&first.worktree, "two.txt", "two\n");
        fail_review(&ctx, 1);

        let second = spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));

        assert_eq!(git_out(&second.worktree, &["rev-parse", "HEAD"]), tip);
        assert_eq!(
            git_out(
                &second.worktree,
                &["rev-list", "--count", "integration/m4..HEAD"]
            ),
            "2"
        );
        let brief = std::fs::read_to_string(&second.brief).unwrap_or_default();
        assert!(brief.contains("starts from its last commit"), "{brief}");
        let third_tip = commit_in(&second.worktree, "three.txt", "three\n");
        fail_review(&ctx, 2);
        let third = spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(git_out(&third.worktree, &["rev-parse", "HEAD"]), third_tip);
        assert_eq!(
            git_out(
                scratch.root(),
                &["rev-parse", "--verify", "refs/shprd/archive/t1/T1-a1"]
            ),
            tip
        );
    }

    /// An attempt that committed nothing leaves nothing to carry, so the retry
    /// starts from the integration branch's current head, not an older point.
    #[test]
    fn a_retry_of_an_attempt_with_no_commits_starts_from_the_base() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.spawn_env(&ctx);
        let mux = host(&scratch);
        let first = spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));
        fail_review(&ctx, 1);
        // The base moves on while the attempt is down.
        git(scratch.root(), &["checkout", "-q", "integration/m4"]);
        commit_in(scratch.root(), "moved.txt", "moved\n");
        git(scratch.root(), &["checkout", "-q", "main"]);
        assert!(first.worktree.is_dir());

        let second = spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));

        assert!(
            second
                .replaced
                .as_ref()
                .is_some_and(|r| r.carried.is_none())
        );
        assert_eq!(
            git_out(&second.worktree, &["rev-parse", "HEAD"]),
            git_out(scratch.root(), &["rev-parse", "integration/m4"])
        );
    }

    /// Blocker 6: the retry finds the old worktree by what git registered, so a
    /// home spelled another way (a symlink, `/tmp` against `/private/tmp`) does
    /// not leave the branch pinned and the retry failing with git's own text.
    #[test]
    fn a_retry_replaces_a_worktree_registered_under_another_spelling_of_the_home() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.spawn_env(&ctx);
        let mux = host(&scratch);
        let first = spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));
        let tip = commit_in(&first.worktree, "kept.txt", "kept\n");
        fail_review(&ctx, 1);
        // The same home, reached through a symlink.
        let links = TempDir::new().unwrap_or_else(|err| panic!("{err}"));
        let link = links.path().join("home");
        std::os::unix::fs::symlink(scratch.home.path(), &link)
            .unwrap_or_else(|err| panic!("{err}"));
        let mut again = scratch.spawn_env(&ctx);
        again.layout = Layout::default_for_home(&link);

        let second = spawn(&mux, &again, &request()).unwrap_or_else(|err| panic!("{err}"));

        assert_eq!(second.attempt, 2);
        assert_eq!(git_out(&second.worktree, &["rev-parse", "HEAD"]), tip);
        assert_eq!(
            git_out(
                scratch.root(),
                &["rev-parse", "--verify", "refs/shprd/archive/t1/T1-a1"]
            ),
            tip
        );
    }

    /// The old worktree's folder is gone but git still lists it, so the branch
    /// stays pinned to it: the retry prunes that registration instead of failing
    /// with git's "cannot delete branch used by worktree".
    #[test]
    fn a_retry_clears_a_registration_whose_folder_is_gone() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.spawn_env(&ctx);
        let mux = host(&scratch);
        let first = spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));
        let tip = commit_in(&first.worktree, "kept.txt", "kept\n");
        fail_review(&ctx, 1);
        std::fs::remove_dir_all(&first.worktree).unwrap_or_else(|err| panic!("{err}"));

        let second = spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));

        assert_eq!(second.attempt, 2);
        assert_eq!(git_out(&second.worktree, &["rev-parse", "HEAD"]), tip);
    }

    /// Whatever git refuses, the person reads a plain sentence, not git's text.
    #[test]
    fn a_git_failure_on_retry_is_a_plain_sentence() {
        let err = plain_failure(
            "T1",
            "the old attempt's branch could not be cleared",
            &"git branch -D shprd/q1/T1 failed (exit Some(1)): error: cannot delete branch",
        );
        let text = err.to_string();
        assert!(
            text.contains("Could not start a new attempt of T1"),
            "{text}"
        );
        assert!(!text.contains("git branch"), "{text}");
        assert!(!text.contains("exit Some"), "{text}");
    }

    /// Every attempt reuses one branch name, so a third attempt must not
    /// overwrite the ref that keeps attempt 1's commits.
    #[test]
    fn a_third_attempt_keeps_each_earlier_attempts_archive_apart() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.spawn_env(&ctx);
        let mux = host(&scratch);
        let first = spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));
        let first_tip = commit_in(&first.worktree, "one.txt", "one\n");
        fail_review(&ctx, 1);
        let second = spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));
        let second_tip = commit_in(&second.worktree, "two.txt", "two\n");
        fail_review(&ctx, 2);

        let third = spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));

        assert_eq!(third.attempt, 3);
        let replaced = third
            .replaced
            .clone()
            .unwrap_or_else(|| panic!("the retry must name the ref it archived"));
        assert_eq!(replaced.attempt, 2);
        assert_eq!(replaced.ref_name, "refs/shprd/archive/t1/T1-a2");
        assert_eq!(
            git_out(
                scratch.root(),
                &["rev-parse", "--verify", "refs/shprd/archive/t1/T1-a1"]
            ),
            first_tip,
            "attempt 1's archive moved"
        );
        assert_eq!(
            git_out(
                scratch.root(),
                &["rev-parse", "--verify", "refs/shprd/archive/t1/T1-a2"]
            ),
            second_tip,
            "attempt 2's commits are not archived"
        );
    }

    /// An attempt that is still working, or reported and waiting on its gates,
    /// is work in flight: it is refused with the reason, not replaced.
    #[test]
    fn a_retry_refuses_an_attempt_that_is_still_in_flight() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.spawn_env(&ctx);
        let mux = host(&scratch);
        let first = spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));

        let err = match spawn(&mux, &env, &request()) {
            Ok(report) => panic!("a working attempt must not be replaced: {report:?}"),
            Err(err) => format!("{err}"),
        };
        assert!(err.contains("Working"), "{err}");
        assert!(err.contains("cannot be archived and replaced"), "{err}");
        assert!(first.worktree.is_dir(), "the working worktree was removed");
        assert!(
            git_try(
                scratch.root(),
                &["rev-parse", "--verify", "refs/shprd/archive/t1/T1-a1"]
            )
            .is_none(),
            "a working attempt must not be archived"
        );

        report_completed(&ctx, 1);
        let err = match spawn(&mux, &env, &request()) {
            Ok(report) => panic!("a reported attempt must not be replaced: {report:?}"),
            Err(err) => format!("{err}"),
        };
        assert!(err.contains("Reported"), "{err}");
        assert!(first.worktree.is_dir(), "the reported worktree was removed");
        assert_eq!(
            git_out(scratch.root(), &["rev-parse", "shprd/t1/T1"]),
            git_out(scratch.root(), &["rev-parse", "integration/m4"]),
            "the reported attempt's branch was replaced"
        );
    }

    /// The calls spawn made for one worker spec, whose `system_prompt_args` are
    /// the ones `--append-system-prompt` reads a file path from.
    fn calls_after_spawn(system_prompt_args: Option<Vec<&str>>) -> Vec<Call> {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let mut env = scratch.spawn_env(&ctx);
        env.programs[0].spec.launch.system_prompt_args =
            system_prompt_args.map(|args| args.into_iter().map(str::to_string).collect());
        let mux = FakeMux::default().with_ready_agent();
        mux.create_space(&repo_space_name(&scratch.repo_name()), scratch.root())
            .unwrap_or_else(|err| panic!("{err}"));
        spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));
        mux.calls()
    }

    /// The prompts spawn typed into the tab, in order.
    fn prompts(calls: &[Call]) -> Vec<String> {
        calls
            .iter()
            .filter_map(|call| match call {
                Call::Write(_, text, _) => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    /// An agent that took its brief as a system prompt starts idle, waiting for
    /// a first turn, so it gets one line telling it to begin, not the brief again.
    #[test]
    fn an_agent_given_the_brief_as_a_system_prompt_gets_a_kickoff() {
        let calls = calls_after_spawn(Some(vec!["--append-system-prompt", "{prompt_file}"]));
        let argv = calls
            .iter()
            .find_map(|call| match call {
                Call::Spawn(_, _, argv, _, _) => Some(argv.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("the CLI was not started: {calls:?}"));
        assert!(
            argv.iter().any(|arg| arg.ends_with(".md")),
            "the brief travels as a file path: {argv:?}"
        );
        assert_eq!(prompts(&calls), [crate::cmd::deliver::KICKOFF.to_string()]);
        assert!(
            !crate::cmd::deliver::KICKOFF.contains('\n'),
            "the kickoff is one line"
        );
    }

    /// An agent with no system-prompt flag still gets the whole brief typed in.
    #[test]
    fn an_agent_without_system_prompt_arguments_gets_the_whole_brief() {
        let prompt = prompts(&calls_after_spawn(None))
            .first()
            .cloned()
            .unwrap_or_else(|| panic!("the brief was never typed in"));
        assert!(prompt.contains("Make spawn real"), "{prompt}");
        assert!(prompt.contains("crates/shprd"), "{prompt}");
    }

    #[test]
    fn a_failed_tab_removes_the_worktree_and_records_nothing() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.spawn_env(&ctx);
        // A host that has the space but cannot start a program in it: the tab
        // shprd asked for never exists, so the worktree must go back.
        let mux = FakeMux::default()
            .with_ready_agent()
            .failing_spawn("the host refused");

        let failure = spawn(&mux, &env, &request());
        assert!(failure.is_err(), "spawn should have failed");
        assert_eq!(failure.map(|_| ()).map_err(|err| err.exit_code()), Err(1));

        let worktree = env.layout.path(&env.repo_name, "t1", "T1");
        assert!(
            !worktree.exists(),
            "worktree {} survived",
            worktree.display()
        );
        let state = ctx.state().unwrap_or_else(|err| panic!("{err}"));
        assert!(state.resources.is_empty(), "{:?}", state.resources);
        let task = state.tasks.get("T1").unwrap_or_else(|| panic!("no task"));
        assert_eq!(task.status, TaskStatus::Queued);
        assert!(task.attempts.is_empty());
    }

    #[test]
    fn a_brief_lands_once_the_host_detects_the_agent() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let mut env = scratch.spawn_env(&ctx);
        // The host needs two polls to call the pane an agent; the wait covers it.
        env.ready_wait = Duration::from_secs(5);
        let mux = FakeMux::default();
        let pane = shprd_mux::PaneId::new("pane-1");
        mux.set_state_after(&pane, 2, shprd_mux::AgentState::Idle);

        let report = match spawn(&mux, &env, &request()) {
            Ok(report) => report,
            Err(err) => panic!("{err}"),
        };
        assert!(report.worktree.exists());
        let prompted = mux
            .calls()
            .into_iter()
            .filter_map(|call| match call {
                Call::Write(pane, text, _) => Some((pane, text)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(prompted.len(), 1, "the brief is typed in exactly once");
        assert!(
            prompted[0].1.contains("Make spawn real"),
            "{}",
            prompted[0].1
        );
        let state = ctx.state().unwrap_or_else(|err| panic!("{err}"));
        assert!(state.escalations.is_empty(), "{:?}", state.escalations);
    }

    #[test]
    fn a_brief_no_agent_ever_takes_is_a_warning_and_an_escalation() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.spawn_env(&ctx);
        // No ready agent and no wait: the plain `sh` this fake hosts is never
        // detected, which is the real OpenCode and Kilo case.
        let mux = FakeMux::default();
        mux.create_space(&repo_space_name(&scratch.repo_name()), scratch.root())
            .unwrap_or_else(|err| panic!("{err}"));

        let report = match spawn(&mux, &env, &request()) {
            Ok(report) => report,
            Err(err) => panic!("an undelivered brief must not fail the spawn: {err}"),
        };
        assert!(report.worktree.exists(), "the worktree stays");
        assert!(
            mux.calls()
                .iter()
                .all(|call| !matches!(call, Call::Write(..))),
            "nothing may be typed into an undetected pane"
        );
        let state = ctx.state().unwrap_or_else(|err| panic!("{err}"));
        let escalation = state
            .escalations
            .first()
            .unwrap_or_else(|| panic!("no escalation: {:?}", state.escalations));
        assert_eq!(escalation.task.as_deref(), Some("T1"));
        assert_eq!(escalation.body, "brief not delivered");
        assert_eq!(escalation.default_action, "resend the brief");
    }

    /// A host with a core that cannot report a program listed in a spawn record
    /// as unreadable — the herdr-core 0.1.21 case — and one that can.
    fn no_plugin_reports() -> FakeMux {
        let mut caps = shprd_mux::Capabilities::all();
        caps.agent_reports = false;
        FakeMux::new(caps)
    }

    /// A spec that names its ready line and reports nothing of its own, the way
    /// the real opencode file does.
    fn plugin_only_spec(id: &str) -> HarnessSpec {
        let mut spec = fake_spec(id);
        spec.status.sources = vec![
            shprd_agents::spec::StatusSource::Hook,
            shprd_agents::spec::StatusSource::Screen,
        ];
        spec.ready.screen = vec!["Ask anything".to_string()];
        spec
    }

    /// `Scratch::spawn_env` with a program list of the caller's choosing.
    fn spawn_env_with_programs<'a>(
        scratch: &Scratch,
        ctx: &'a Ctx,
        programs: Vec<Program>,
    ) -> SpawnEnv<'a> {
        let mut env = scratch.spawn_env(ctx);
        env.workers = programs
            .iter()
            .map(|program| program.spec.id.clone())
            .collect();
        env.programs = programs;
        env
    }

    #[test]
    fn a_spawn_records_that_a_ready_line_was_what_said_the_program_was_up() {
        // The M10 gate's bug 3, from the spawn's own side: opencode's plugin
        // cannot report through a core without `pane.report_agent`, so the brief
        // goes in once the TUI's own line is drawn, and the record says so.
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = spawn_env_with_programs(
            &scratch,
            &ctx,
            vec![Program {
                spec: plugin_only_spec("sh"),
                installed: true,
            }],
        );
        let mux = no_plugin_reports();
        mux.create_space(&repo_space_name(&scratch.repo_name()), scratch.root())
            .unwrap_or_else(|err| panic!("{err}"));
        let report = match spawn(&mux, &env, &request()) {
            Ok(report) => report,
            Err(err) => panic!("{err}"),
        };
        // The screen is only painted after the spawn has read it once, so the
        // record cannot say `ready-line` from a *zero* wait: an empty screen and
        // a zero wait is a lost brief. Prove the record exists and names the
        // program and its sources either way.
        let path = spawn_record_path(&env.briefs_dir, "t1", "T1", report.attempt);
        let record = read_spawn_record(&path)
            .unwrap_or_else(|| panic!("no spawn record at {}", path.display()));
        assert_eq!(record.agent, "sh");
        assert_eq!(record.status, "hook, screen");
        assert_eq!(
            record.ready, "not-delivered",
            "a zero wait delivers nothing"
        );
        assert!(!record.host_cannot_report());
    }

    #[test]
    fn a_brief_read_off_the_ready_line_keeps_a_task_the_host_cannot_report_alive() {
        // A task whose brief arrived through opencode's ready line is one the
        // host cannot see afterwards either: its `agent_state` stays Unknown for
        // as long as it runs. Reading that as "no agent" is what would let the
        // tick land work still being written.
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = spawn_env_with_programs(
            &scratch,
            &ctx,
            vec![Program {
                spec: plugin_only_spec("sh"),
                installed: true,
            }],
        );
        // A TUI that paints its prompt: every pane the spawn makes shows it.
        let mux = no_plugin_reports().with_screen("┃  Ask anything… \"Fix broken tests\"");
        mux.create_space(&repo_space_name(&scratch.repo_name()), scratch.root())
            .unwrap_or_else(|err| panic!("{err}"));
        let mut env = env;
        env.ready_wait = Duration::from_secs(5);
        let report = match spawn(&mux, &env, &request()) {
            Ok(report) => report,
            Err(err) => panic!("{err}"),
        };
        let path = spawn_record_path(&env.briefs_dir, "t1", "T1", report.attempt);
        let record = read_spawn_record(&path)
            .unwrap_or_else(|| panic!("no spawn record at {}", path.display()));
        assert_eq!(record.ready, "ready-line", "{record:?}");
        assert!(record.host_cannot_report());

        // Now the liveness rule reads the same record: the task is live even
        // though the host has no opinion about its pane.
        let state = ctx.state().unwrap_or_else(|err| panic!("{err}"));
        assert!(
            live_tasks(&mux, &env.home, &state).contains("T1"),
            "a task whose program the host cannot report must stay live while its tab exists"
        );
    }

    #[test]
    fn a_task_the_host_does_report_goes_quiet_when_its_agent_does() {
        // The same rule, the other way: an ordinary launch that reported its
        // state is not kept alive by its tab alone.
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.spawn_env(&ctx);
        let mux = FakeMux::default().with_ready_agent();
        mux.create_space(&repo_space_name(&scratch.repo_name()), scratch.root())
            .unwrap_or_else(|err| panic!("{err}"));
        spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));
        let state = ctx.state().unwrap_or_else(|err| panic!("{err}"));
        assert!(live_tasks(&mux, &env.home, &state).contains("T1"));

        // Its agent ends: the tab is still there, and the task is not live.
        mux.set_exited(&PaneId::new("pane-1"), Some(0));
        assert!(
            !live_tasks(&mux, &env.home, &state).contains("T1"),
            "a reported agent that ended is not live"
        );
    }

    #[test]
    fn a_failing_run_closes_the_tab_and_removes_the_worktree() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.spawn_env(&ctx);
        let mux = FakeMux::default()
            .with_ready_agent()
            .failing_spawn("the host refused");

        let failure = spawn(&mux, &env, &request());
        assert!(failure.is_err(), "spawn should have failed");

        let worktree = env.layout.path(&env.repo_name, "t1", "T1");
        assert!(
            !worktree.exists(),
            "worktree {} survived",
            worktree.display()
        );
        // The space the spawn made is still there, with no tab in it: `spawn`
        // makes the tab and starts the program in one call, so a host that
        // refuses the program never leaves a tab behind.
        let space = agent_space(&mux, &env).unwrap_or_else(|err| panic!("{err}"));
        let _ = &space;
        let tabs = all_handles(&mux);
        assert!(tabs.is_empty(), "the tab it opened survived: {tabs:?}");
        assert!(
            !mux.calls()
                .iter()
                .any(|call| matches!(call, Call::CloseTab(_))),
            "there was no tab to close: {:?}",
            mux.calls()
        );
        let state = ctx.state().unwrap_or_else(|err| panic!("{err}"));
        assert!(state.resources.is_empty(), "{:?}", state.resources);
        let task = state.tasks.get("T1").unwrap_or_else(|| panic!("no task"));
        assert!(task.attempts.is_empty());
        assert!(state.escalations.is_empty(), "{:?}", state.escalations);
    }

    /// The repository's space is shprd's own to make, so a spawn does not
    /// depend on a person having opened anything: it is made at the repository
    /// root, and the worker in it has no tab and no space of its own.
    #[test]
    fn a_spawn_makes_the_repo_space_and_the_worker_has_no_tab() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.spawn_env(&ctx);
        let mux = FakeMux::default().with_ready_agent();

        let report = spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));

        let name = repo_space_name(&scratch.repo_name());
        let space = mux
            .find_space(&name)
            .unwrap_or_else(|err| panic!("{err}"))
            .unwrap_or_else(|| panic!("no space {name:?}"));
        assert!(
            mux.list_tabs(&space.id)
                .unwrap_or_else(|err| panic!("{err}"))
                .is_empty(),
            "no tab"
        );
        let agents = mux.list_agents().unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(agents.len(), 1, "{agents:?}");
        assert_eq!(agents[0].tab.title, report.tab);
        assert_eq!(agents[0].task.as_deref(), Some("T1"));
        assert_eq!(agents[0].role, shprd_mux::AgentRole::Worker);
        assert!(
            mux.calls().iter().any(|call| matches!(
                call,
                Call::CreateSpace(label, cwd)
                    if label == &name && cwd == scratch.root()
            )),
            "the repository's space is made at the repository root: {:?}",
            mux.calls()
        );
    }

    /// Writes the tab record a spawn would have written for one attempt.
    fn record_tab(scratch: &Scratch, run: &str, task: &str, attempt: u32, tab: &shprd_mux::TabId) {
        let path = tab_path(
            &scratch.home.path().join(".shprd").join("briefs"),
            run,
            task,
            attempt,
        );
        write_tab(&path, tab);
        assert!(path.exists(), "{} was not written", path.display());
    }

    #[test]
    fn prompt_reaches_the_tab_its_own_task_spawned() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let mux = FakeMux::default().with_ready_agent();
        // Both tasks' tabs are in the one repository space, and both are titled
        // after their own task: which one a prompt reaches is decided by the
        // record beside the brief, not by the title or the space alone.
        let first = "task-one";
        let second = "task-two";
        let other_space = mux
            .create_space(&repo_space_name(&scratch.repo_name()), scratch.root())
            .unwrap_or_else(|err| panic!("{err}"));
        let mine_space = other_space.clone();
        let other = mux
            .spawn(
                &other_space.id,
                &worker_title("sh", first),
                &[],
                &[],
                scratch.root(),
            )
            .unwrap_or_else(|err| panic!("{err}"));
        let mine = mux
            .spawn(
                &mine_space.id,
                &worker_title("sh", second),
                &[],
                &[],
                scratch.root(),
            )
            .unwrap_or_else(|err| panic!("{err}"));
        record_tab(&scratch, "t1", first, 1, &other.id);
        record_tab(&scratch, "t1", second, 1, &mine.id);

        prompt_in(&mux, scratch.home.path(), &ctx, second, "keep going")
            .unwrap_or_else(|err| panic!("{err}"));
        let prompted = mux
            .calls()
            .into_iter()
            .find_map(|call| match call {
                Call::Write(pane, text, _) => Some((pane, text)),
                _ => None,
            })
            .unwrap_or_else(|| panic!("nothing was prompted"));
        assert_eq!(prompted.0, mine.root_pane);
        assert_ne!(prompted.0, other.root_pane);
        assert_eq!(prompted.1, "keep going");

        // A task with no record, and a record whose tab is gone, both refuse
        // with the same message rather than prompting the wrong agent.
        let unrecorded = prompt_in(&mux, scratch.home.path(), &ctx, "T9", "hello");
        let message = match unrecorded {
            Ok(()) => panic!("a task with no tab record was prompted"),
            Err(err) => {
                assert_eq!(err.exit_code(), 2);
                err.to_string()
            }
        };
        assert!(message.contains("no open tab for task"), "{message}");

        // The latest attempt's tab is gone from the host: that is refused too,
        // never replaced by an older attempt's tab.
        write_tab(
            &tab_path(
                &scratch.home.path().join(".shprd").join("briefs"),
                "t1",
                first,
                2,
            ),
            &shprd_mux::TabId::new("tab-gone"),
        );
        let closed = prompt_in(&mux, scratch.home.path(), &ctx, first, "hello");
        assert_eq!(closed.map(|_| ()).map_err(|err| err.exit_code()), Err(2));
    }

    /// Two runs can each hold a tab record for the same task. A prompt belongs
    /// to the active run, even when an older run's record has the higher
    /// attempt number.
    #[test]
    fn prompt_ignores_an_older_runs_tab_record() {
        let scratch = Scratch::new();
        let ctx = Ctx::new(scratch.root().to_path_buf(), Actor::Human, false, true);
        ctx.commit(EventKind::RunStarted {
            run_id: "run-two".to_string(),
            repo: scratch.repo_name(),
            base_branch: "main".to_string(),
            integration_branch: "integration/m4".to_string(),
        })
        .unwrap_or_else(|err| panic!("{err}"));
        let mux = FakeMux::default().with_ready_agent();
        // The two records below name two tabs of the one space: the active
        // run's is the one that gets typed at.
        let stale_space = mux
            .create_space(&repo_space_name(&scratch.repo_name()), scratch.root())
            .unwrap_or_else(|err| panic!("{err}"));
        let alive_space = stale_space.clone();
        let stale = mux
            .spawn(&stale_space.id, "stale", &[], &[], scratch.root())
            .unwrap_or_else(|err| panic!("{err}"));
        let current = mux
            .spawn(&alive_space.id, "current", &[], &[], scratch.root())
            .unwrap_or_else(|err| panic!("{err}"));
        // The older run kept retrying, so its attempt number is the higher one.
        record_tab(&scratch, "run-one", "api", 7, &stale.id);
        record_tab(&scratch, "run-two", "api", 1, &current.id);

        prompt_in(&mux, scratch.home.path(), &ctx, "api", "keep going")
            .unwrap_or_else(|err| panic!("{err}"));
        let prompted = mux
            .calls()
            .into_iter()
            .find_map(|call| match call {
                Call::Write(pane, _, _) => Some(pane),
                _ => None,
            })
            .unwrap_or_else(|| panic!("nothing was prompted"));
        assert_eq!(prompted, current.root_pane);
        assert_ne!(prompted, stale.root_pane);
    }

    /// With no run in the folded log there is no current run to prompt in.
    #[test]
    fn prompt_without_a_run_says_so() {
        let scratch = Scratch::new();
        let ctx = Ctx::new(scratch.root().to_path_buf(), Actor::Human, false, true);
        let mux = FakeMux::default().with_ready_agent();
        let message = match prompt_in(&mux, scratch.home.path(), &ctx, "api", "hello") {
            Ok(()) => panic!("a prompt with no active run was sent"),
            Err(err) => {
                assert_eq!(err.exit_code(), 2);
                err.to_string()
            }
        };
        assert!(message.contains("no active run"), "{message}");
    }

    /// A finished run is over: its tab records are history, and a prompt after
    /// `shprd run finish` has nowhere to go.
    #[test]
    fn prompt_after_the_run_finished_says_there_is_no_active_run() {
        let scratch = Scratch::new();
        let ctx = Ctx::new(scratch.root().to_path_buf(), Actor::Human, false, true);
        ctx.commit(EventKind::RunStarted {
            run_id: "run-done".to_string(),
            repo: scratch.repo_name(),
            base_branch: "main".to_string(),
            integration_branch: "integration/m4".to_string(),
        })
        .unwrap_or_else(|err| panic!("{err}"));
        let mux = FakeMux::default().with_ready_agent();
        let workspace = mux
            .create_space(&repo_space_name(&scratch.repo_name()), scratch.root())
            .unwrap_or_else(|err| panic!("{err}"));
        let tab = mux
            .spawn(&workspace.id, "api", &[], &[], scratch.root())
            .unwrap_or_else(|err| panic!("{err}"));
        record_tab(&scratch, "run-done", "api", 1, &tab.id);
        ctx.commit(EventKind::RunFinished {
            abandoned: Vec::new(),
        })
        .unwrap_or_else(|err| panic!("{err}"));

        let message = match prompt_in(&mux, scratch.home.path(), &ctx, "api", "hello") {
            Ok(()) => panic!("a prompt after the run finished was sent"),
            Err(err) => {
                assert_eq!(err.exit_code(), 2);
                err.to_string()
            }
        };
        assert!(message.contains("no active run"), "{message}");
    }

    #[test]
    fn a_missing_repo_space_is_created_at_the_repository_root() {
        // Bug 6 of the M9 gate: the label is derived from the folder name and
        // the space was left to a person, so a run refused every pass until
        // someone read the refusal. `serve` creates it instead, with exactly that
        // label, at the repository root.
        let scratch = Scratch::new();
        let mux = FakeMux::default();
        let name = repo_space_name(&scratch.repo_name());

        let prepared =
            ensure_repo_space(&mux, scratch.root()).unwrap_or_else(|err| panic!("{err}"));

        assert!(prepared.created, "the space was missing, so it is made");
        assert_eq!(prepared.space.name, name);
        assert_eq!(
            mux.calls(),
            vec![
                Call::FindSpace(name.clone()),
                Call::CreateSpace(name, scratch.root().to_path_buf()),
            ],
            "one lookup, one creation, and nothing else touched"
        );
    }

    #[test]
    fn an_existing_repo_space_is_reused_and_no_other_is_created() {
        let scratch = Scratch::new();
        let mux = FakeMux::default();
        let human = mux
            .create_space("my own work", scratch.root())
            .unwrap_or_else(|err| panic!("{err}"));
        let ours = mux
            .create_space(&repo_space_name(&scratch.repo_name()), scratch.root())
            .unwrap_or_else(|err| panic!("{err}"));
        let _ = mux.take_calls();

        let prepared =
            ensure_repo_space(&mux, scratch.root()).unwrap_or_else(|err| panic!("{err}"));

        assert!(!prepared.created, "the space was already there");
        assert_eq!(prepared.space.id, ours.id, "the one with that label");
        assert_eq!(
            mux.calls(),
            vec![Call::FindSpace(repo_space_name(&scratch.repo_name()))],
            "a lookup only: nothing is created, renamed or focused"
        );
        let spaces = mux.list_spaces().unwrap_or_else(|err| panic!("{err}"));
        assert!(
            spaces.iter().any(|found| found.id == human.id),
            "the person's own space is untouched: {spaces:?}"
        );
    }

    #[test]
    fn the_refusal_names_the_expected_label_and_the_ones_the_host_has() {
        let scratch = Scratch::new();
        let mux = FakeMux::default();
        // The gate's attempt A: a space whose label is almost right.
        mux.create_space("shprd · gate repo", scratch.root())
            .unwrap_or_else(|err| panic!("{err}"));
        mux.create_space("my own work", scratch.root())
            .unwrap_or_else(|err| panic!("{err}"));

        let text = match space_named(&mux, &repo_space_name(&scratch.repo_name())) {
            Ok(space) => panic!("the space {} was accepted", space.id),
            Err(err) => err.to_string(),
        };

        let expected = repo_space_name(&scratch.repo_name());
        assert!(
            text.contains(&format!("{expected:?}")),
            "the expected label is named: {text}"
        );
        assert!(
            text.contains("\"shprd · gate repo\""),
            "the near miss is listed: {text}"
        );
        assert!(
            text.contains("\"my own work\""),
            "every space the host has is listed: {text}"
        );
    }

    #[test]
    fn the_refusal_says_when_the_host_has_no_space_at_all() {
        let scratch = Scratch::new();
        let mux = FakeMux::default();

        let text = match space_named(&mux, &repo_space_name(&scratch.repo_name())) {
            Ok(space) => panic!("the space {} was accepted", space.id),
            Err(err) => err.to_string(),
        };

        assert!(text.contains("no space at all"), "{text}");
        assert!(
            text.contains("shprd serve"),
            "the verb that fixes it: {text}"
        );
    }

    #[test]
    fn an_agent_is_started_for_the_repositorys_own_space() {
        // One space per repository, labelled from its folder: the task and the
        // run are not in it, so no task ever has a space of its own.
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.spawn_env(&ctx);
        let mux = FakeMux::default();

        let space = agent_space(&mux, &env).unwrap_or_else(|err| panic!("{err}"));

        assert_eq!(space.name, repo_space_name(&scratch.repo_name()));
    }

    #[test]
    fn the_repositorys_space_the_host_already_has_is_reused() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.spawn_env(&ctx);
        let mux = FakeMux::default();
        let first = agent_space(&mux, &env).unwrap_or_else(|err| panic!("{err}"));
        let _ = mux.take_calls();

        let again = agent_space(&mux, &env).unwrap_or_else(|err| panic!("{err}"));

        assert_eq!(first, again, "one repository, one space");
        assert!(
            !mux.calls()
                .iter()
                .any(|call| matches!(call, Call::CreateSpace(..))),
            "a second spawn does not make a second space: {:?}",
            mux.calls()
        );
    }

    /// The shipped harness table, as `DataDir` reads it.
    fn shipped_data() -> DataDir {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../data")
            .canonicalize()
            .unwrap_or_else(|err| panic!("{err}"));
        DataDir::resolve(Some(&path), None)
    }

    #[test]
    fn starting_an_agent_opens_its_plain_launch_line_in_the_repositorys_space() {
        let scratch = Scratch::new();
        let mux = FakeMux::default();
        let opened = open_program(&mux, &shipped_data(), scratch.root(), "claude", None)
            .unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(opened.program, "claude");
        assert_eq!(opened.name, "Claude Code");
        let calls = mux.calls();
        let (title, argv, env, cwd) = calls
            .iter()
            .find_map(|call| match call {
                Call::Spawn(_, title, argv, env, cwd) => {
                    Some((title.clone(), argv.clone(), env.clone(), cwd.clone()))
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("no spawn in {calls:?}"));
        assert_eq!(title, "Claude Code", "the tab is named after the program");
        assert_eq!(
            argv,
            ["claude"],
            "the plain launch line, with no mode, model or brief flag on it"
        );
        assert!(
            env.is_empty(),
            "a person's own agent gets no shprd variables: {env:?}"
        );
        assert_eq!(cwd, scratch.root());
        assert!(
            calls.iter().any(|call| matches!(call, Call::FindSpace(..))),
            "the repository's own space is looked for first: {calls:?}"
        );
        assert_eq!(
            calls
                .iter()
                .filter(|call| matches!(call, Call::CreateSpace(..)))
                .count(),
            1,
            "one space, made once: {calls:?}"
        );
    }

    #[test]
    fn starting_an_agent_in_a_named_space_opens_there_and_makes_no_space() {
        let scratch = Scratch::new();
        let mux = FakeMux::default();
        let other = mux
            .create_space("notes", scratch.root())
            .unwrap_or_else(|err| panic!("{err}"));
        open_program(
            &mux,
            &shipped_data(),
            scratch.root(),
            "claude",
            Some(other.id.as_str()),
        )
        .unwrap_or_else(|err| panic!("{err}"));
        let calls = mux.calls();
        assert!(
            calls
                .iter()
                .any(|call| matches!(call, Call::Spawn(space, ..) if *space == other.id)),
            "the tab opens in the space the window named: {calls:?}"
        );
        assert!(
            !calls.iter().any(|call| matches!(call, Call::FindSpace(..))),
            "the repository's space is not looked for: {calls:?}"
        );
        assert_eq!(
            calls
                .iter()
                .filter(|call| matches!(call, Call::CreateSpace(..)))
                .count(),
            1,
            "only the test's own space was made: {calls:?}"
        );
    }

    #[test]
    fn a_program_the_harness_table_lacks_is_refused_with_the_ones_it_has() {
        let scratch = Scratch::new();
        let mux = FakeMux::default();
        let refused = open_program(&mux, &shipped_data(), scratch.root(), "not-a-cli", None)
            .err()
            .unwrap_or_else(|| panic!("an unknown program was accepted"));
        let message = refused.to_string();
        assert!(message.contains("not-a-cli"), "{message}");
        assert!(
            message.contains("claude, codex, kilo, opencode, pi"),
            "{message}"
        );
        assert!(
            mux.calls().is_empty(),
            "nothing was opened for a program that does not exist: {:?}",
            mux.calls()
        );
    }

    /// The short run the SHPR-24 settings make: what a spawn names and cuts from.
    fn spawned_with(text: &str, scratch: &Scratch) -> (SpawnReport, String) {
        let ctx = scratch.ctx();
        let env = scratch.spawn_env_with(&ctx, text);
        let mux = FakeMux::default().with_ready_agent();
        let report = spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));
        let brief = std::fs::read_to_string(&report.brief).unwrap_or_default();
        (report, brief)
    }

    #[test]
    fn tasks_branch_and_archive_namespace_name_the_branch() {
        let scratch = Scratch::new();
        let (plain, _) = spawned_with("version = 1\n", &scratch);
        assert_eq!(
            plain.branch, "shprd/t1/T1",
            "unset, the name is what it was"
        );

        let scratch = Scratch::new();
        let (named, brief) = spawned_with(
            "version = 1\n[tasks]\nbranch = \"task/{n}-{slug}\"\narchive_namespace = \"work/\"\n",
            &scratch,
        );
        assert_eq!(named.branch, "work/task/1-t1");
        assert_eq!(
            git_out(&named.worktree, &["rev-parse", "--abbrev-ref", "HEAD"]),
            "work/task/1-t1"
        );
        assert!(brief.contains("branch `work/task/1-t1`"), "{brief}");
    }

    #[test]
    fn tasks_base_is_the_branch_a_worktree_is_cut_from() {
        let scratch = Scratch::new();
        git(scratch.root(), &["checkout", "-q", "-b", "trunk"]);
        let trunk = commit_in(scratch.root(), "trunk.txt", "trunk\n");
        git(scratch.root(), &["checkout", "-q", "main"]);
        let integration = git_out(scratch.root(), &["rev-parse", "integration/m4"]);
        assert_ne!(trunk, integration);

        let (report, _) = spawned_with("version = 1\n[tasks]\nbase = \"trunk\"\n", &scratch);
        assert_eq!(git_out(&report.worktree, &["rev-parse", "HEAD"]), trunk);

        let scratch = Scratch::new();
        let (report, _) = spawned_with("version = 1\n", &scratch);
        let integration = git_out(scratch.root(), &["rev-parse", "integration/m4"]);
        assert_eq!(
            git_out(&report.worktree, &["rev-parse", "HEAD"]),
            integration,
            "unset, the run's integration branch is the base"
        );
    }

    #[test]
    fn tasks_worktrees_off_works_in_the_repository_itself() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.spawn_env_with(&ctx, "version = 1\n[tasks]\nworktrees = false\n");
        let mux = FakeMux::default().with_ready_agent();
        let report = spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(report.worktree, scratch.root());
        assert_eq!(
            report.branch, "main",
            "the checked-out branch, not a new one"
        );
        assert!(
            !scratch.home.path().join(".shprd").join("wt").exists(),
            "no worktree was made"
        );
        let branches = git_out(scratch.root(), &["branch", "--list", "shprd/*"]);
        assert!(branches.is_empty(), "no task branch was made: {branches}");
        let state = ctx.state().unwrap_or_else(|err| panic!("{err}"));
        assert!(
            state.resources.is_empty(),
            "the repository is never a resource the janitor may remove: {:?}",
            state.resources
        );
    }

    #[test]
    fn tasks_worktrees_off_runs_one_task_at_a_time() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        ctx.commit(EventKind::TaskAdded {
            task: "T2".to_string(),
            title: "A second task".to_string(),
            scope: vec!["crates/shprd".to_string()],
            acceptance: vec!["it works".to_string()],
            tags: Vec::new(),
            skills: Vec::new(),
            deps: Vec::new(),
        })
        .unwrap_or_else(|err| panic!("{err}"));
        let env = scratch.spawn_env_with(&ctx, "version = 1\n[tasks]\nworktrees = false\n");
        let mux = FakeMux::default().with_ready_agent();
        spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));
        let second = SpawnRequest {
            task: "T2".to_string(),
            agent: None,
            effort: None,
        };
        let err = spawn(&mux, &env, &second).err();
        let text = err.map(|err| err.to_string()).unwrap_or_default();
        assert!(text.contains("one task at a time"), "{text}");
    }

    #[test]
    fn start_and_finish_notes_are_in_the_workers_brief() {
        let scratch = Scratch::new();
        let (_, brief) = spawned_with(
            "version = 1\n[tasks]\nstart_notes = \"READ-THE-DESIGN-FIRST\"\nfinish_notes = \"RUN-THE-LINTER-LAST\"\n",
            &scratch,
        );
        let start = brief.find("READ-THE-DESIGN-FIRST").unwrap_or(usize::MAX);
        let task = brief.find("# T1 ").unwrap_or(0);
        let rules = brief.find("## Rules").unwrap_or(0);
        let finish = brief.find("RUN-THE-LINTER-LAST").unwrap_or(0);
        assert!(
            start < task,
            "the start notes come before the task: {brief}"
        );
        assert!(
            finish > rules,
            "the finish notes come with the rules: {brief}"
        );

        let scratch = Scratch::new();
        let (_, brief) = spawned_with("version = 1\n", &scratch);
        assert!(!brief.contains("READ-THE-DESIGN-FIRST"), "{brief}");
        // Only the read-only checkout line fills the start notes.
        let start = brief.find("Before you start").map(|at| &brief[at..]);
        assert!(
            start.is_some_and(|rest| rest.contains("read-only: read from it")),
            "{brief}"
        );
        assert!(!brief.contains("Before you report"), "{brief}");
    }

    #[test]
    fn worktree_files_are_copied_and_linked_into_the_worktree() {
        let scratch = Scratch::new();
        std::fs::write(scratch.root().join(".env"), "KEY=1\n")
            .unwrap_or_else(|err| panic!("{err}"));
        std::fs::write(scratch.root().join("local.json"), "{}")
            .unwrap_or_else(|err| panic!("{err}"));
        let (report, _) = spawned_with(
            "version = 1\n[tasks]\nworktree_files = \".env\\nlink:local.json, absent.txt\"\n",
            &scratch,
        );
        let copied = report.worktree.join(".env");
        assert!(!copied.is_symlink());
        assert_eq!(
            std::fs::read_to_string(copied).unwrap_or_else(|err| panic!("{err}")),
            "KEY=1\n"
        );
        assert!(report.worktree.join("local.json").is_symlink());
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.contains("absent.txt") && w.contains("main checkout")),
            "{:?}",
            report.warnings
        );
    }

    #[test]
    fn untracked_files_the_scope_names_are_warned_about_and_the_checkout_is_named() {
        let scratch = Scratch::new();
        std::fs::create_dir_all(scratch.root().join("crates/shprd"))
            .unwrap_or_else(|err| panic!("{err}"));
        std::fs::write(scratch.root().join("crates/shprd/new.txt"), "x")
            .unwrap_or_else(|err| panic!("{err}"));
        let (report, brief) = spawned_with("version = 1\n", &scratch);
        let warning = report
            .warnings
            .iter()
            .find(|w| w.contains("will not see"))
            .unwrap_or_else(|| panic!("no warning: {:?}", report.warnings));
        assert!(warning.contains("crates/"), "{warning}");
        assert!(brief.contains("will not see"), "{brief}");
        let root = scratch.root().display().to_string();
        assert!(
            brief.contains(&format!("The main checkout is at {root} (read-only")),
            "{brief}"
        );

        // Listing the file in worktree_files provides it, so no warning.
        let scratch = Scratch::new();
        std::fs::create_dir_all(scratch.root().join("crates/shprd"))
            .unwrap_or_else(|err| panic!("{err}"));
        std::fs::write(scratch.root().join("crates/shprd/new.txt"), "x")
            .unwrap_or_else(|err| panic!("{err}"));
        let (report, _) = spawned_with(
            "version = 1\n[tasks]\nworktree_files = \"crates\"\n",
            &scratch,
        );
        assert!(
            !report.warnings.iter().any(|w| w.contains("will not see")),
            "{:?}",
            report.warnings
        );
        assert!(report.worktree.join("crates/shprd/new.txt").exists());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shprd_agents::skills::Role as SkillRole;
    use shprd_core::event::{Finding, Severity};
    use shprd_core::state::{GateResult, TaskStatus};

    fn live_agent(
        pane: &str,
        space: &str,
        role: shprd_mux::AgentRole,
        live: bool,
    ) -> shprd_mux::LiveAgent {
        shprd_mux::LiveAgent {
            tab: shprd_mux::Tab {
                id: shprd_mux::TabId::new(format!("tab-{pane}")),
                title: pane.to_string(),
                root_pane: PaneId::new(pane),
            },
            task: None,
            role,
            program: "claude".to_string(),
            space: shprd_mux::SpaceId::new(space),
            state: shprd_mux::AgentState::Idle,
            live,
            started_ms: None,
            ended_ms: None,
        }
    }

    #[test]
    fn only_running_agents_of_this_repositorys_closed_spaces_are_chosen() {
        use shprd_mux::AgentRole::{Coordinator, Worker};
        let recorded = vec!["spc1".to_string(), "spc2".to_string()];
        // spc2 was added again before the sweep ran, so it is open; spc9 is
        // another repository's.
        let open = vec!["spc2".to_string(), "spc9".to_string()];
        let agents = vec![
            live_agent("coordinator-1", "spc1", Coordinator, true),
            live_agent("worker-1", "vm/spc1", Worker, true),
            live_agent("coordinator-2", "spc2", Coordinator, true),
            live_agent("other", "spc9", Coordinator, true),
            live_agent("ended", "spc1", Worker, false),
            live_agent("worker-1", "spc1", Worker, true),
        ];
        let chosen: Vec<String> = agents_of_closed_spaces(&recorded, &open, agents)
            .iter()
            .map(|agent| agent.tab.root_pane.as_str().to_string())
            .collect();
        assert_eq!(chosen, ["coordinator-1", "worker-1"]);
    }

    #[test]
    fn a_task_tab_record_is_matched_by_its_whole_name() {
        // The attempt number is one or more digits and nothing else follows, so
        // a task whose id starts with another's keeps its own records.
        assert!(is_task_tab_record("T1-a1.tab", "T1"));
        assert!(is_task_tab_record("T1-a12-review.tab", "T1"));
        assert!(!is_task_tab_record("T1-audit-a1.tab", "T1"));
        assert!(!is_task_tab_record("T1-a.tab", "T1"));
        assert!(!is_task_tab_record("T1-a1x.tab", "T1"));
        assert!(!is_task_tab_record("T10-a1.tab", "T1"));
    }

    #[test]
    fn a_task_tab_record_needs_the_tab_extension() {
        assert!(!is_task_tab_record("T1-a1.md", "T1"));
        assert!(!is_task_tab_record("T1-a1-review.md", "T1"));
    }

    #[test]
    fn a_task_whose_name_ends_in_review_keeps_its_records() {
        // `-review` is only stripped when it sits between the attempt and the
        // extension, so a task called `T1-review` is not read as `T1`'s.
        assert!(is_task_tab_record("T1-review-a1.tab", "T1-review"));
        assert!(!is_task_tab_record("T1-review-a1.tab", "T1"));
    }

    #[test]
    fn a_worker_close_leaves_the_reviewers_record_alone() {
        let mux = shprd_mux::fake::FakeMux::default();
        let dir = tempfile::TempDir::new().unwrap_or_else(|err| panic!("{err}"));
        let run = "rrun1";
        std::fs::create_dir_all(dir.path().join(run)).unwrap_or_else(|err| panic!("{err}"));
        std::fs::write(dir.path().join(run).join("T1-a1.tab"), "tab-w")
            .unwrap_or_else(|err| panic!("{err}"));
        std::fs::write(dir.path().join(run).join("T1-a1-review.tab"), "tab-r")
            .unwrap_or_else(|err| panic!("{err}"));
        std::fs::write(dir.path().join(run).join("T1-audit-a1.tab"), "tab-a")
            .unwrap_or_else(|err| panic!("{err}"));

        close_worker(&mux, dir.path(), run, "T1").unwrap_or_else(|err| panic!("{err}"));

        let calls = mux.calls();
        assert!(
            calls.contains(&shprd_mux::fake::Call::CloseTab(shprd_mux::TabId::new(
                "tab-w"
            ))),
            "{calls:?}"
        );
        assert!(
            !calls.contains(&shprd_mux::fake::Call::CloseTab(shprd_mux::TabId::new(
                "tab-r"
            ))),
            "{calls:?}"
        );
        assert!(
            !calls.contains(&shprd_mux::fake::Call::CloseTab(shprd_mux::TabId::new(
                "tab-a"
            ))),
            "{calls:?}"
        );
    }

    /// A launch spec whose only model flag carries the `{model}` placeholder.
    fn launch() -> LaunchSpec {
        LaunchSpec {
            interactive: vec!["claude".to_string()],
            model_args: vec!["--model".to_string(), "{model}".to_string()],
            effort_args: None,
            auto_approve: None,
            reviewer_args: None,
            system_prompt_args: None,
            resume_args: None,
            verified: true,
        }
    }

    /// A harness spec with no access modes and no fast switch: the argv tests
    /// about the model and the brief must not carry flags they do not use.
    fn bare_spec() -> HarnessSpec {
        let text = r#"
id = "claude"
name = "Fake"
vendor = "pi"
verified_version = "1.0.0"
dry_patterns = []

[detect]
binary = "claude"
version_args = ["--version"]
signin_hint = "fake"
signin_args = []

[launch]
interactive = ["claude"]
model_args = ["--model", "{model}"]
verified = false

[models]
provider_qualified = false

[records]
kind = "jsonl"
location = "~/.fake/{session}.jsonl"
errors = "error"
verified = true

[quota]
source = "none"

[skills]
home_dirs = []
project_dirs = []
"#;
        toml::from_str(text).unwrap_or_else(|err| panic!("the test spec is invalid: {err}"))
    }

    /// A harness spec with two access modes and a fast switch.
    fn modes_spec() -> HarnessSpec {
        let text = r#"
id = "claude"
name = "Fake"
vendor = "pi"
verified_version = "1.0.0"
dry_patterns = []

[detect]
binary = "claude"
version_args = ["--version"]
signin_hint = "fake"
signin_args = []

[launch]
interactive = ["claude"]
model_args = ["--model", "{model}"]
verified = false

[models]
provider_qualified = false

[records]
kind = "jsonl"
location = "~/.fake/{session}.jsonl"
errors = "error"
verified = true

[quota]
source = "none"

[skills]
home_dirs = []
project_dirs = []

[permissions]
default = "plan"
full = "yolo"
modes = [
  { id = "plan", label = "Plan", args = ["--permission-mode", "plan"] },
  { id = "yolo", label = "Full", args = ["--yes"] },
]

[fast]
on = ["--fast"]
off = ["--no-fast"]
"#;
        toml::from_str(text).unwrap_or_else(|err| panic!("the test spec is invalid: {err}"))
    }

    #[test]
    fn a_task_without_a_model_gets_no_model_flags() {
        let brief = Path::new("/tmp/brief.md");
        let (argv, in_args) = launch_argv(
            &launch(),
            &bare_spec(),
            &Applied::default(),
            None,
            None,
            "brief",
            brief,
        )
        .unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(argv, ["claude".to_string()]);
        assert!(!in_args);
    }

    #[test]
    fn a_task_with_a_model_gets_its_flags() {
        let brief = Path::new("/tmp/brief.md");
        let (argv, _) = launch_argv(
            &launch(),
            &bare_spec(),
            &Applied::default(),
            Some("opus"),
            None,
            "brief",
            brief,
        )
        .unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(
            argv,
            [
                "claude".to_string(),
                "--model".to_string(),
                "opus".to_string()
            ]
        );
    }

    #[test]
    fn a_launch_carries_the_access_mode_and_fast_switch_it_applied() {
        let brief = Path::new("/tmp/brief.md");
        let spec = modes_spec();
        let chosen = applied(&spec, Some("plan"), true);
        assert_eq!(chosen.perm.as_deref(), Some("plan"));
        assert!(chosen.fast);
        let (argv, _) = launch_argv(&launch(), &spec, &chosen, None, None, "brief", brief)
            .unwrap_or_else(|err| panic!("{err}"));
        assert!(argv.contains(&"--permission-mode".to_string()), "{argv:?}");
        assert!(argv.contains(&"plan".to_string()), "{argv:?}");
        assert!(argv.contains(&"--fast".to_string()), "{argv:?}");
        assert!(
            !argv.contains(&"--no-fast".to_string()),
            "fast on is said once: {argv:?}"
        );
    }

    #[test]
    fn a_role_that_names_no_mode_gets_full_access() {
        let spec = modes_spec();
        let chosen = applied(&spec, None, false);
        assert_eq!(
            chosen.perm.as_deref(),
            Some("yolo"),
            "the full-access mode is the pre-M9 auto-approval"
        );
        assert!(!chosen.fast);
        let brief = Path::new("/tmp/brief.md");
        let (argv, _) = launch_argv(&launch(), &spec, &chosen, None, None, "brief", brief)
            .unwrap_or_else(|err| panic!("{err}"));
        assert!(argv.contains(&"--yes".to_string()), "{argv:?}");
        assert!(argv.contains(&"--no-fast".to_string()), "{argv:?}");
    }

    #[test]
    fn a_mode_the_program_does_not_declare_falls_back_to_full_access() {
        let spec = modes_spec();
        let chosen = applied(&spec, Some("read-only"), true);
        assert_eq!(
            chosen.perm.as_deref(),
            Some("yolo"),
            "a mode the program has no flags for is not claimed"
        );
    }

    #[test]
    fn fast_is_applied_only_where_the_program_has_the_switch() {
        let chosen = applied(&bare_spec(), None, true);
        assert!(
            !chosen.fast,
            "a program with no fast switch is not told to go fast"
        );
    }

    /// A task with the fields the brief and the skill selection read.
    fn task(id: &str, tags: &[&str]) -> Task {
        Task {
            id: id.to_string(),
            title: "Make spawn real".to_string(),
            scope: vec!["crates/shprd".to_string()],
            acceptance: vec!["spawn opens a tab".to_string()],
            tags: tags.iter().map(|tag| (*tag).to_string()).collect(),
            skills: Vec::new(),
            deps: Vec::new(),
            status: TaskStatus::Queued,
            attempts: Vec::new(),
            overrides: Vec::new(),
            dropped_reason: None,
            pr: None,
            approved_head: None,
            last_status: None,
            nothing_to_merge: None,
            fallbacks: Vec::new(),
            added_at_ms: 0,
            touched_at_ms: 0,
        }
    }

    /// One curated skill entry.
    fn compat(name: &str, verdict: Verdict, roles: &[SkillRole], tags: &[&str]) -> CompatEntry {
        CompatEntry {
            name: name.to_string(),
            source: "someone/skills".to_string(),
            verdict,
            roles: roles.to_vec(),
            tags: tags.iter().map(|tag| (*tag).to_string()).collect(),
            reason: String::new(),
            reviewed_hash: "abc".to_string(),
        }
    }

    #[test]
    fn skills_combine_config_and_tagged_allow_entries() {
        let skills = Skills {
            workers: vec!["tdd".to_string(), "simplify".to_string()],
            ..Skills::default()
        };
        let compat = [
            compat(
                "frontend-design",
                Verdict::Allow,
                &[SkillRole::Worker],
                &["ui"],
            ),
            compat(
                "writing-plans",
                Verdict::Allow,
                &[SkillRole::Orchestrator],
                &["ui"],
            ),
            compat(
                "rust-gates",
                Verdict::Allow,
                &[SkillRole::Worker],
                &["rust"],
            ),
            compat("tdd", Verdict::Allow, &[SkillRole::Worker], &["rust"]),
        ];
        let chosen = select_skills(&skills, &compat, &["ui".to_string()]);
        assert_eq!(chosen, vec!["tdd", "simplify", "frontend-design"]);
    }

    #[test]
    fn skills_never_include_a_denied_entry() {
        let skills = Skills {
            workers: vec!["brainstorming".to_string()],
            ..Skills::default()
        };
        let compat = [
            compat(
                "brainstorming",
                Verdict::Deny,
                &[SkillRole::Worker],
                &["ux"],
            ),
            compat("opt-in-only", Verdict::OptIn, &[SkillRole::Worker], &["ux"]),
        ];
        assert!(select_skills(&skills, &compat, &["ux".to_string()]).is_empty());
    }

    #[test]
    fn brief_states_scope_rules_and_skills() {
        let skills = vec!["tdd".to_string()];
        let plan = task("M4.4", &["rust"]);
        let text = render_brief(&BriefInput {
            task: &plan,
            run: "m4",
            attempt: 2,
            agent: "claude",
            model: Some("claude-opus-5-5"),
            branch: "shprd/m4/M4.4",
            worktree: "/wt/m4/M4.4",
            skills: &skills,
            gates: &["cargo test --workspace".to_string()],
            archive: None,
            prompt: None,
            ask: AskRoute::Hook,
            proof: true,
            settings: None,
            role: None,
            warnings: &[],
            program_line: None,
        });
        for needle in [
            "# M4.4 Make spawn real",
            "attempt 2",
            "claude-opus-5-5",
            "## Acceptance criteria",
            "spawn opens a tab",
            "## Where to start — a hint, not a limit",
            "crates/shprd",
            "## Skills to load first",
            "- tdd",
            "shprd task status M4.4 --msg",
            "shprd agent report --task M4.4 --attempt 2",
            "--check",
            "--test",
            "--changed",
            "--commit",
            "--skill-used",
            "shprd/m4/M4.4",
            "/wt/m4/M4.4",
        ] {
            assert!(
                text.contains(needle),
                "brief is missing {needle:?}:\n{text}"
            );
        }
    }

    /// The brief of a second attempt repeats what sent the first one back: the
    /// gate's name and each finding, so the new agent fixes those first.
    #[test]
    fn a_new_attempts_brief_carries_the_previous_findings() {
        let skills = Vec::new();
        let plan = task_sent_back();
        let text = render_brief(&BriefInput {
            task: &plan,
            run: "m4",
            attempt: 2,
            agent: "claude",
            model: None,
            branch: "shprd/m4/M4.4",
            worktree: "/wt/m4/M4.4",
            skills: &skills,
            gates: &[],
            archive: None,
            prompt: None,
            ask: AskRoute::Hook,
            proof: true,
            settings: None,
            role: None,
            warnings: &[],
            program_line: None,
        });
        for needle in [
            "## Why the previous attempt was sent back",
            "gate `review` failed",
            "- major: the unwrap panics on an empty log (crates/shprd/src/cmd/spawn.rs:120) — Return a CliError instead.",
            "Fix these first; keep what already works.",
        ] {
            assert!(
                text.contains(needle),
                "brief is missing {needle:?}:\n{text}"
            );
        }
    }

    /// A first attempt has nothing to answer, and a passing gate sends nothing
    /// back: the section appears only after a failed gate.
    #[test]
    fn a_brief_repeats_only_a_failed_earlier_gate() {
        let skills = Vec::new();
        let plan = task_sent_back();
        let brief_for = |attempt| {
            render_brief(&BriefInput {
                task: &plan,
                run: "m4",
                attempt,
                agent: "claude",
                model: None,
                branch: "shprd/m4/M4.4",
                worktree: "/wt/m4/M4.4",
                skills: &skills,
                gates: &[],
                archive: None,
                prompt: None,
                ask: AskRoute::Hook,
                proof: true,
                settings: None,
                role: None,
                warnings: &[],
                program_line: None,
            })
        };
        assert!(!brief_for(1).contains("sent back"));
        assert!(brief_for(2).contains("sent back"));
    }

    /// A task whose first attempt failed its review, with one major finding.
    fn task_sent_back() -> Task {
        let mut plan = task("M4.4", &[]);
        plan.attempts.push(shprd_core::state::Attempt {
            n: 1,
            agent: "claude".to_string(),
            model: None,
            worktree: "/wt/m4/M4.4".to_string(),
            branch: "shprd/m4/M4.4".to_string(),
            report: None,
            gates: vec![GateResult {
                gate: "review".to_string(),
                passed: false,
                findings: vec![Finding {
                    severity: Severity::Major,
                    title: "the unwrap panics on an empty log".to_string(),
                    location: Some("crates/shprd/src/cmd/spawn.rs:120".to_string()),
                    detail: "Return a CliError instead.".to_string(),
                }],
                source: None,
                seq: 7,
            }],
            reviews: Vec::new(),
            spawn_seq: None,
            stopped: false,
        });
        plan
    }
}

/// Test-only host behaviour: a [`FakeMux`] that fails one call kind.
///
/// A transient host error and a closed tab are the same value to a caller that
/// swallows errors, so the tests that must tell them apart make exactly one
/// call fail here.
#[cfg(test)]
pub(crate) mod fault {
    use std::path::Path;

    use shprd_mux::fake::FakeMux;
    use shprd_mux::{
        AgentState, Capabilities, Direction, Host, MuxError, Outcome, PaneId, Result, Space,
        SpaceId, Tab, TabId, Waited,
    };
    use std::time::Duration;

    /// The call a [`FlakyMux`] makes fail.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum Fault {
        /// [`Host::list_tabs`] answers [`MuxError::Command`].
        ListTabs,
        /// [`Host::close_tab`] answers [`MuxError::Command`].
        CloseTab,
        /// [`Host::list_spaces`] answers [`MuxError::Command`].
        ListSpaces,
    }

    /// A [`FakeMux`] whose `fault` call fails with a host error that is not
    /// [`MuxError::NotFound`], i.e. one a caller must not read as "gone".
    ///
    /// Only the verbs it changes are written out: every other one is the
    /// [`Host`] default delegating to the fake, through [`Fault::pass`]'s two
    /// helpers.
    pub(crate) struct FlakyMux {
        /// The fake host that answers every other call.
        inner: FakeMux,
        /// The call that fails.
        fault: Fault,
    }

    impl FlakyMux {
        /// Wraps `inner`, failing `fault`.
        pub(crate) fn new(inner: FakeMux, fault: Fault) -> Self {
            Self { inner, fault }
        }

        /// The fake underneath, so a test can script panes and read the calls.
        pub(crate) fn inner(&self) -> &FakeMux {
            &self.inner
        }

        /// The host error the failing call answers, if this is it.
        fn failure(&self, call: Fault) -> Option<MuxError> {
            if self.fault == call {
                return Some(MuxError::command("the fake host", "the host went away"));
            }
            None
        }
    }

    impl Host for FlakyMux {
        fn capabilities(&self) -> Capabilities {
            self.inner.capabilities()
        }

        fn find_space(&self, name: &str) -> Result<Option<Space>> {
            self.inner.find_space(name)
        }

        fn list_spaces(&self) -> Result<Vec<Space>> {
            match self.failure(Fault::ListSpaces) {
                Some(err) => Err(err),
                None => self.inner.list_spaces(),
            }
        }

        fn create_space(&self, name: &str, cwd: &Path) -> Result<Space> {
            self.inner.create_space(name, cwd)
        }

        fn focus_space(&self, space: &SpaceId) -> Result<()> {
            self.inner.focus_space(space)
        }

        fn list_tabs(&self, space: &SpaceId) -> Result<Vec<Tab>> {
            match self.failure(Fault::ListTabs) {
                Some(err) => Err(err),
                None => self.inner.list_tabs(space),
            }
        }

        fn rename_tab(&self, tab: &TabId, title: &str) -> Result<()> {
            self.inner.rename_tab(tab, title)
        }

        fn focus_tab(&self, tab: &TabId) -> Result<()> {
            self.inner.focus_tab(tab)
        }

        fn close_tab(&self, tab: &TabId) -> Result<()> {
            match self.failure(Fault::CloseTab) {
                Some(err) => Err(err),
                None => self.inner.close_tab(tab),
            }
        }

        fn spawn(
            &self,
            space: &SpaceId,
            title: &str,
            argv: &[String],
            env: &[(String, Option<String>)],
            cwd: &Path,
        ) -> Result<Tab> {
            self.inner.spawn(space, title, argv, env, cwd)
        }

        fn split(
            &self,
            pane: &PaneId,
            direction: Direction,
            ratio: f32,
            cwd: &Path,
        ) -> Result<PaneId> {
            self.inner.split(pane, direction, ratio, cwd)
        }

        fn write(&self, pane: &PaneId, text: &str, submit: bool) -> Result<()> {
            self.inner.write(pane, text, submit)
        }

        fn read(&self, pane: &PaneId, lines: usize) -> Result<String> {
            self.inner.read(pane, lines)
        }

        fn wait(&self, pane: &PaneId, wanted: &Waited, timeout: Duration) -> Result<Outcome> {
            self.inner.wait(pane, wanted, timeout)
        }

        fn agent_state(&self, pane: &PaneId) -> Result<AgentState> {
            self.inner.agent_state(pane)
        }

        fn subscribe(&self, timeout: Duration) -> Result<Vec<shprd_mux::MuxEvent>> {
            self.inner.subscribe(timeout)
        }
    }
}
