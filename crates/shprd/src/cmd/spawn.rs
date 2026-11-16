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
use shprd_agents::data::DataDir;
use shprd_agents::env::{EnvContext, LaunchEnv, Role as EnvRole, launch_env};
use shprd_agents::models::{AgentRef, Catalog};
use shprd_agents::skills::{CompatEntry, Role as SkillRole, Verdict};
use shprd_agents::spec::{BlockingPrompt, HarnessSpec, LaunchSpec, Values, render_args};
use shprd_core::config::Skills;
use shprd_core::event::{Actor, EventKind, ResourceAction, ResourceKind};
use shprd_core::prompts::{self, PromptRef};
use shprd_core::settings::{
    Key, Layer, Loaded, Resolved, ResolvedRole, RoleField, Source, Sourced, Value,
};
use shprd_core::state::{ResourceStatus, RunState, Task, TaskStatus};
use shprd_git::archive;
use shprd_git::git::Git;
use shprd_git::worktree::{self, Layout, Worktree};
use shprd_mux::{AgentState, Host, PaneId};

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
    let role = env.settings.resolve(None).role(BRIEF_ROLE);
    // The worker role's choice comes first, then the configured workers: a
    // person who set `roles.worker.agent` gets that program, and `--agent`
    // still wins over both.
    let candidates = role_candidates(role.as_ref(), &env.workers);
    let mut pick = choose_agent(req.agent.as_deref(), &candidates, &env.programs)?;
    if req.agent.is_none()
        && let Some(model) = role
            .as_ref()
            .and_then(|role| role.model.as_ref())
            .filter(|model| !model.value.trim().is_empty())
    {
        pick.model = Some(model.value.clone());
    }
    let effort = req
        .effort
        .clone()
        .or_else(|| {
            role.as_ref()
                .and_then(|role| role.effort.as_ref())
                .map(|effort| effort.value.clone())
                .filter(|effort| !effort.trim().is_empty())
        })
        .filter(|effort| !effort.trim().is_empty());
    // What the catalog says about the level decides whether the flag is passed;
    // the setting itself stays as it was written, so the brief can say both what
    // was asked for and why it does nothing.
    let (launch_effort, effort_warning) =
        effective_effort(env, &pick.agent, pick.model.as_deref(), effort.clone());
    let warnings: Vec<String> = effort_warning.into_iter().collect();
    let attempt = task.attempts.len() as u32 + 1;
    let worktree = env.layout.path(&env.repo_name, &env.run, &task.id);
    let branch = env.layout.branch_name(&env.run, &task.id);
    let skills = select_skills(&env.skills, &env.compat, &task.tags);
    // What the launch applies is decided before the brief is written, because
    // the Settings block names it.
    let program = program_of(env.programs.as_slice(), &pick.agent)?;
    let chosen = applied(
        &program.spec,
        named(role.as_ref().and_then(|role| role.perm.as_ref())),
        role.as_ref().is_some_and(|role| role.fast.value),
    );
    // How this program raises a needs-you card is the harness's fact alone
    // (`[asks] supported`), so the brief never has to know a program's name.
    let ask = AskRoute::of_harness(program.spec.asks.supported);
    // The role's words and the Settings block are read before anything is
    // touched: an agent whose prompt cannot be read must cost nothing.
    let context = brief_context(env, BRIEF_ROLE, &pick, effort.as_deref(), &chosen)?;
    let brief_path = brief_path(&env.briefs_dir, &env.run, &task.id, attempt);
    // A retry replaces what the failed attempt left behind, so the fresh
    // worktree below can take the same path and branch name. A task still in
    // flight is refused here, before anything is removed.
    let replaced = replace_previous(mux, env, &state, task, attempt)?;
    let kind = spawned_event(&task.id, attempt, &pick, &worktree, &branch);
    // Refuse before any effect: a refused spawn must leave nothing behind.
    env.ctx.check(&kind)?;
    for resource in resource_kinds(&task.id, &worktree, &branch) {
        env.ctx.check(&resource)?;
    }
    let brief = render_brief(&BriefInput {
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
    });
    write_brief(&brief_path, &brief)?;
    // The task's own space comes first, and it is shprd's to make rather than a
    // person's to open: a spawn needs somewhere to put the tab it is about to
    // start, and the label is derived from the task. A host that refuses (tuios
    // without the `admin` grant, say) refuses here, before the worktree exists.
    let space = task_space(mux, env, &task.id)?;
    let created = match create_worktree(env, &task.id) {
        Ok(created) => created,
        Err(err) => {
            discard_empty_space(mux, &space);
            return Err(err);
        }
    };
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
    let (tab_id, tab) = match opened {
        Ok(opened) => opened,
        Err(err) => {
            discard_worktree(env, &created);
            // The space is shprd's own, made for this spawn: an empty one left
            // on the host's bar is a task a person sees and no agent works in.
            discard_empty_space(mux, &space);
            return Err(err);
        }
    };
    // The tab id goes next to the brief: a label is only 16 characters, so two
    // tasks can share one, and the id cannot. With a space per task the tab is
    // found inside the task's own space, and the record is what says which tab
    // of it is the worker's.
    write_tab(
        &tab_path(&env.briefs_dir, &env.run, &task.id, attempt),
        &tab_id,
    );
    if let Err(err) = record_attempt(env, &task.id, attempt, &pick, &created) {
        close_tab(mux, &tab_id);
        discard_worktree(env, &created);
        discard_empty_space(mux, &space);
        return Err(err);
    }
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
/// One repository, one space for the coordinator: a human reads the name on
/// their host's bar and knows which checkout they are looking at. Every task
/// gets a space of its own ([`task_space_name`]), so this one outlives them.
pub(crate) fn repo_space_name(repo_name: &str) -> String {
    shprd_mux::repo_space_name(repo_name)
}

/// The name of the space one task's tabs live in: worker and reviewer together.
///
/// The task and the run are both in the label, so a human reading the host's bar
/// can tell which run a task belongs to when the same task id comes round again.
pub(crate) fn task_space_name(repo_name: &str, run: &str, task: &str) -> String {
    shprd_mux::task_space_name(repo_name, run, task)
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
/// It is the input to [`repo_space_name`] and [`task_space_name`], so this is
/// where the labels' one derivation gets its input from, for a caller holding a
/// path rather than a [`SpawnEnv`] (`shprd serve` does, before there is a run to
/// spawn for).
pub(crate) fn repo_name(repo: &Path) -> Result<String, CliError> {
    repo.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .ok_or_else(|| CliError::other(anyhow!("{} has no directory name", repo.display())))
}

/// A task's space, found or made at the repository root.
///
/// One task, one space: its worker, its reviewer and its backup all open tabs
/// here, so a person sees one group per task and a landing has one thing to
/// close. The label is derived ([`task_space_name`]), so a race with another
/// caller is resolved by reading back what the winner made rather than by
/// adopting whatever happens to be there.
pub(crate) fn task_space(
    mux: &dyn Host,
    env: &SpawnEnv<'_>,
    task: &str,
) -> Result<shprd_mux::Space, CliError> {
    let name = task_space_name(&env.repo_name, &env.run, task);
    mux.ensure_space(&name, &env.ctx.root).map_err(|err| {
        CliError::other(anyhow!(err).context(format!("opening the space {name:?} for {task}")))
    })
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
/// anything else on the host is left alone. Task spaces are made by
/// [`task_space`] at spawn and closed when the task lands.
pub(crate) struct PreparedSpace {
    /// The space the coordinator's tabs open in.
    pub(crate) space: shprd_mux::Space,
    /// True when this call created it, false when it was already there.
    pub(crate) created: bool,
}

/// [`PreparedSpace`]: find the coordinator's space, or create it.
pub(crate) fn ensure_repo_space(mux: &dyn Host, repo: &Path) -> Result<PreparedSpace, CliError> {
    let name = repo_space_name(&repo_name(repo)?);
    let found = mux.find_space(&name).map_err(|err| {
        CliError::other(anyhow!(err).context(format!("looking for the space {name:?}")))
    })?;
    if let Some(space) = found {
        return Ok(PreparedSpace {
            space,
            created: false,
        });
    }
    match mux.create_space(&name, repo) {
        Ok(space) => Ok(PreparedSpace {
            space,
            created: true,
        }),
        // Two services can start at once; the loser of the race finds what the
        // winner made rather than reporting a failure it can do nothing about.
        Err(err) => match mux.find_space(&name) {
            Ok(Some(space)) => Ok(PreparedSpace {
                space,
                created: false,
            }),
            _ => Err(CliError::other(anyhow!(err).context(format!(
                "creating the space {name:?} at {}",
                repo.display()
            )))),
        },
    }
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

/// Opens the agent's tab and starts its CLI in it, handing it the brief.
pub(crate) fn open_agent(
    mux: &dyn Host,
    env: &SpawnEnv<'_>,
    space: &shprd_mux::Space,
    launching: &Launching<'_>,
) -> Result<(shprd_mux::TabId, String), CliError> {
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
    let (argv, brief_in_args) = launch_argv(
        &pick.launch,
        &program.spec,
        applied,
        pick.model.as_deref(),
        effort,
        brief,
        brief_path,
    )?;
    let pairs = env_pairs(launch_env(
        &env.base_env,
        EnvRole::Worker,
        &EnvContext {
            actor: format!("agent:{}", pick.agent),
            task: Some(task.to_string()),
            run: Some(env.run.clone()),
            repo: worktree.path.clone(),
            home: Some(env.home.clone()),
            data: env.data_dir.env_value().map(Path::to_path_buf),
        },
    ));
    let tab = mux
        .spawn(&space.id, &title, &argv, &pairs, &worktree.path)
        .map_err(|err| CliError::other(anyhow!(err).context("starting the agent tab")))?;
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
        Ok(()) => Ok((tab.id, title)),
        Err(err) => {
            close_tab(mux, &tab.id);
            Err(err)
        }
    }
}

/// Hands the agent that is already running in `tab` its brief.
///
/// A brief that does not arrive is reported, never fatal: the CLI is running,
/// and the human can resend the brief into the tab.
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
) -> Result<(), CliError> {
    // A brief that arrived as a system prompt leaves the CLI idle, waiting for a
    // first turn, so it gets a one-line kickoff; an agent with no system-prompt
    // flag gets the whole brief typed in, as before.
    let (text, what) = if brief_in_args {
        (super::deliver::KICKOFF, "kickoff")
    } else {
        (brief, "brief")
    };
    let delivery = super::deliver::deliver_brief(
        mux,
        &tab.root_pane,
        text,
        &pick.blocking_prompts,
        env.ready_wait,
    );
    let Some(reason) = delivery.reason() else {
        return Ok(());
    };
    eprintln!(
        "warning: {what} not delivered to {title}: {reason}; resend with: shprd agent prompt --task {task} \"$(cat {})\"",
        brief_path.display()
    );
    let escalation = EventKind::Escalated {
        task: Some(task.to_string()),
        body: "brief not delivered".to_string(),
        default_action: "resend the brief".to_string(),
    };
    if let Err(err) = env.ctx.commit(escalation) {
        eprintln!("warning: could not record the escalation: {err}");
    }
    Ok(())
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

/// Closes the space a landed task's terminals lived in.
///
/// One task, one space: its worker, its reviewer and any backup are tabs of it,
/// so the space is what goes when the task lands — one call instead of one per
/// tab, and nothing on the host outside that space is touched. A space the host
/// has already forgotten (a human closed it, or a second landing raced this one)
/// is in the state this wanted, so [`MuxError::NotFound`] is success.
///
/// # The tab records always follow
///
/// [`Capabilities::close_space`] is what says whether the one call is possible.
/// Either way the records the run wrote beside the briefs are closed after it:
/// they name every tab shprd opened for the task, a tab the space already took
/// answers `NotFound` (which counts as closed), and a host that cannot close a
/// space — or one whose space a human closed by hand — is left with neither a
/// tab running an agent nor a space nobody can account for.
pub fn close_task_space(
    mux: &dyn Host,
    briefs: &Path,
    run: &str,
    repo_name: &str,
    task: &str,
) -> Result<(), CliError> {
    if mux.capabilities().close_space {
        let name = task_space_name(repo_name, run, task);
        // A host that cannot answer is not a host that kept the space: the tab
        // records below are the evidence shprd itself wrote, so an unreachable
        // host degrades to them rather than refusing the caller's verb.
        match mux.find_space(&name) {
            Ok(found) => {
                if let Some(space) = found {
                    match mux.close_space(&space.id) {
                        Ok(()) | Err(shprd_mux::MuxError::NotFound { .. }) => {}
                        // The host promised the capability and then could not do
                        // it: the records still say what was opened, so the tabs
                        // are closed one by one below.
                        Err(
                            shprd_mux::MuxError::Unsupported(_)
                            | shprd_mux::MuxError::Command { .. },
                        ) => {}
                        Err(err) => {
                            return Err(CliError::other(
                                anyhow!(err)
                                    .context(format!("closing the space {name:?} of {task}")),
                            ));
                        }
                    }
                }
            }
            Err(err) => eprintln!("warning: could not look for the space {name:?}: {err}"),
        }
    }
    // Then the records, always: a host that could not close the space, or one
    // whose space a human had already closed, leaves whatever shprd opened. A
    // tab the space already took answers `NotFound`, which counts as closed, so
    // this second sweep costs one call per record and can never fail on the
    // normal path.
    close_task_tabs(mux, briefs, run, task)
}

/// Closes every task space a run opened, as the run's last act.
///
/// Run end has no task in hand, so every task the log still knows is asked
/// about; a task whose space a landing already closed is simply not there. A
/// host that keeps a space comes back as a [`Leftover`], the way a tab it keeps
/// does, so the one escalation names everything still open.
pub(crate) fn close_run_spaces(
    mux: &dyn Host,
    state: &RunState,
    repo_name: &str,
    briefs: &Path,
    run: &str,
) -> Vec<Leftover> {
    let mut leftovers = Vec::new();
    for task in state.tasks.keys() {
        if let Err(err) = close_task_space(mux, briefs, run, repo_name, task) {
            eprintln!("warning: {err}");
            leftovers.push(Leftover::new(
                "task space",
                task_space_name(repo_name, run, task),
            ));
        }
    }
    leftovers
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
    // A task's own space goes first: closing it takes its tabs, so the sweep
    // below then finds the records already satisfied. Sweeping tab by tab first
    // would work too, but it would leave an empty space behind for every task.
    if let Ok(name) = repo_name(&ctx.root) {
        leftovers.extend(close_run_spaces(mux, &state, &name, &briefs, &run));
    }
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

/// Creates the attempt's worktree from the integration branch.
fn create_worktree(env: &SpawnEnv<'_>, task: &str) -> Result<Worktree, CliError> {
    worktree::create(&env.ctx.root, &env.layout, &env.run, task, &env.base_branch).map_err(|err| {
        CliError::other(anyhow!(err).context(format!("creating a worktree for {task}")))
    })
}

/// Closes a task's space this spawn made and then could not use.
///
/// Only an empty one goes: a space with a tab in it belongs to an attempt that
/// is running (or to a reviewer), and this cleanup runs only on the path where
/// the spawn is about to return an error. A host that refuses is a warning, not
/// a second failure.
fn discard_empty_space(mux: &dyn Host, space: &shprd_mux::Space) {
    let empty = match mux.list_tabs(&space.id) {
        Ok(tabs) => tabs.is_empty(),
        Err(_) => false,
    };
    if !empty {
        return;
    }
    if let Err(err) = mux.close_space(&space.id)
        && !matches!(err, shprd_mux::MuxError::Unsupported(_))
    {
        eprintln!(
            "warning: could not close the empty space {:?}: {err}",
            space.name
        );
    }
}

/// Removes a worktree this spawn created, ignoring the failure: the caller is
/// already returning a more useful error.
fn discard_worktree(env: &SpawnEnv<'_>, created: &Worktree) {
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
    let branch_exists = branch_exists(env, &branch)?;
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
    if branch_exists {
        archive::archive_to(&env.ctx.root, &branch, &ref_name).map_err(|err| {
            CliError::other(
                anyhow!(err).context(format!("archiving attempt {previous} of {:?}", task.id)),
            )
        })?;
        for event in archive_events {
            env.ctx.commit(event)?;
        }
    }
    // The previous attempt's tab points at a worktree that is about to go, and
    // its worker has already been sent back: leaving the tab open would leave a
    // pane typing into a directory that no longer exists.
    if let Err(err) = close_worker(mux, &env.briefs_dir, &env.run, &task.id) {
        eprintln!("warning: {err}");
    }
    if worktree_event.is_some() {
        worktree::remove_worktree_only(&env.ctx.root, &env.layout, &path, true).map_err(|err| {
            CliError::other(anyhow!(err).context(format!(
                "removing the failed attempt's worktree {}",
                path.display()
            )))
        })?;
        if let Some(event) = worktree_event {
            env.ctx.commit(event)?;
        }
    }
    if branch_exists {
        worktree::delete_branch(&env.ctx.root, &env.layout, &branch).map_err(|err| {
            CliError::other(anyhow!(err).context(format!("deleting the branch {branch:?}")))
        })?;
        if let Some(event) = branch_event {
            env.ctx.commit(event)?;
        }
    }
    Ok(Some(Replaced {
        attempt: previous,
        ref_name,
    }))
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
fn attempt_events(task: &str, attempt: u32, pick: &Pick, created: &Worktree) -> Vec<EventKind> {
    let mut events = resource_kinds(task, &created.path, created.branch.as_str()).to_vec();
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
) -> Result<(), CliError> {
    let events = attempt_events(task, attempt, pick, created);
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

/// The agent strings a spawn tries, in order: the worker role's program and
/// model when it names a program, then the configured workers.
///
/// A role's program that is not installed falls through to the configured
/// workers, exactly as an uninstalled worker does, so a machine without it
/// still starts the task.
fn role_candidates(role: Option<&ResolvedRole>, workers: &[String]) -> Vec<String> {
    let mut candidates = Vec::new();
    if let Some(role) = role
        && let Some(agent) = &role.agent
    {
        let model = role
            .model
            .as_ref()
            .map(|model| model.value.as_str())
            .filter(|model| !model.is_empty());
        candidates.push(match model {
            Some(model) => format!("{}/{model}", agent.value),
            None => agent.value.clone(),
        });
    }
    candidates.extend(workers.iter().cloned());
    candidates
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
    let prompt = role_prompt(env, role_id)?;
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
    let run = state
        .run
        .as_ref()
        .ok_or_else(|| CliError::other(anyhow!("no run yet: start one with `shprd run start`")))?;
    let repo_name = repo_name(&ctx.root)?;
    let base_env: BTreeMap<String, String> = std::env::vars().collect();
    // The roots the agent CLIs write under are read from the same environment
    // they will inherit, so a launch that carries `HOME` or `$XDG_DATA_HOME`
    // reads its own records and not the service's.
    let transcript_roots = shprd_agents::transcript::Roots::for_launch(&base_env);
    Ok(SpawnEnv {
        ctx,
        layout: Layout::default_for_home(&home),
        briefs_dir: home.join(".shprd").join("briefs"),
        prompts_dir: prompts::prompts_dir(&crate::ctx::resolve_config_home(&home)),
        home,
        settings,
        run: run.run_id.clone(),
        base_branch: run.integration_branch.clone(),
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
    let run = active_run(ctx)?;
    // A task's tabs live in the task's own space, so the prompt goes to the tab
    // the record names, looked up in that space only. A space that is not there
    // is the same answer as a tab that is not there: nothing to type into, and
    // the person is told how to spawn it.
    let name = task_space_name(&repo_name(&ctx.root)?, &run, task);
    let Some(space) = mux.find_space(&name).map_err(|err| {
        CliError::other(anyhow!(err).context(format!("looking for the space {name:?}")))
    })?
    else {
        return Err(no_open_tab(task));
    };
    let tabs = mux
        .list_tabs(&space.id)
        .map_err(|err| CliError::other(anyhow!(err).context("listing tabs")))?;
    let record = latest_tab_record(&home.join(".shprd").join("briefs"), &run, task)
        .ok_or_else(|| no_open_tab(task))?;
    let wanted = std::fs::read_to_string(&record).map_err(|err| {
        CliError::other(anyhow!(err).context(format!("reading {}", record.display())))
    })?;
    let tab = tabs
        .iter()
        .find(|tab| tab.id.as_str() == wanted.trim())
        .ok_or_else(|| no_open_tab(task))?;
    mux.write(&tab.root_pane, text, true)
        .map_err(|err| CliError::other(anyhow!(err).context("prompting the agent")))
}

/// Where the pane shprd prepared for the coordinator is written.
///
/// It is `<home>/.shprd/coordinator.pane`, beside the tab records: the service
/// writes the pane it prepared when it made or found the repository's space,
/// and a chat line is typed there. One line, the host's own pane id.
pub(crate) fn coordinator_pane_path(home: &Path) -> PathBuf {
    home.join(".shprd").join("coordinator.pane")
}

/// Records the pane the repository's own space holds for the coordinator.
///
/// `shprd serve` calls this once it has made or found the space: the pane is
/// the space's own first tab, which is where a person starts the coordinator, so
/// it is the one pane a chat line may reach. Returns the pane it recorded, or
/// `None` when the space has no tab to name.
pub(crate) fn record_coordinator_pane(
    home: &Path,
    mux: &dyn Host,
    space: &shprd_mux::Space,
) -> Result<Option<PaneId>, CliError> {
    let tabs = mux
        .list_tabs(&space.id)
        .map_err(|err| CliError::other(anyhow!(err).context("listing the space's tabs")))?;
    let Some(tab) = tabs.first() else {
        return Ok(None);
    };
    let path = coordinator_pane_path(home);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|err| {
            CliError::other(anyhow!(err).context(format!("making {}", dir.display())))
        })?;
    }
    std::fs::write(&path, format!("{}\n", tab.root_pane.as_str())).map_err(|err| {
        CliError::other(anyhow!(err).context(format!("writing {}", path.display())))
    })?;
    Ok(Some(tab.root_pane.clone()))
}

/// The pane recorded for the coordinator, when it is still one of the space's.
///
/// The record is only trusted while the pane is still a tab of the repository's
/// own space: a person who closed that tab and left a shell in another one must
/// not be typed into, so a stale record is a refusal rather than a fallback.
fn recorded_coordinator_pane(
    home: &Path,
    mux: &dyn Host,
    space: &shprd_mux::Space,
) -> Option<PaneId> {
    let path = coordinator_pane_path(home);
    let wanted = std::fs::read_to_string(path).ok()?;
    let wanted = wanted.trim();
    if wanted.is_empty() {
        return None;
    }
    let tabs = mux.list_tabs(&space.id).ok()?;
    tabs.into_iter()
        .map(|tab| tab.root_pane)
        .find(|pane| pane.as_str() == wanted)
}

/// Sends one turn of input to the run's coordinator, as the person.
///
/// Two panes are reachable, and both are named rather than guessed:
///
/// 1. the pane the run recorded when it launched a coordinator
///    (`OrchestratorLaunched`), and
/// 2. the pane `shprd serve` recorded for the repository's own space
///    ([`record_coordinator_pane`]), which is where a person starts the
///    coordinator in the M10 flow.
///
/// A pane that is neither is a refusal: shprd never types into a tab it cannot
/// name, so a chat line cannot land in whatever happens to be first on the
/// host's bar.
pub(crate) fn prompt_coordinator(
    mux: &dyn Host,
    home: &Path,
    ctx: &Ctx,
    text: &str,
) -> Result<(), CliError> {
    let state = ctx.state()?;
    if let Some(orchestrator) = state.orchestrator.as_ref() {
        let pane = PaneId::new(orchestrator.pane.clone());
        return mux
            .write(&pane, text, true)
            .map_err(|err| CliError::other(anyhow!(err).context("prompting the coordinator")));
    }
    let name = repo_space_name(&repo_name(&ctx.root)?);
    let space = mux.find_space(&name).map_err(|err| {
        CliError::other(anyhow!(err).context(format!("looking for the space {name:?}")))
    })?;
    let Some(space) = space else {
        return Err(no_coordinator(&name));
    };
    let Some(pane) = recorded_coordinator_pane(home, mux, &space) else {
        return Err(no_coordinator(&name));
    };
    mux.write(&pane, text, true)
        .map_err(|err| CliError::other(anyhow!(err).context("prompting the coordinator")))
}

/// The refusal a coordinator with no recorded pane earns.
fn no_coordinator(space: &str) -> CliError {
    CliError::Invalid(anyhow!(
        "no pane is recorded for the coordinator: `shprd serve` records the first tab of the \
         space {space:?} when it starts, so run the coordinator there and start the service \
         (or restart it), then send again — nothing was typed"
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
/// A host that cannot be asked answers "nothing is live", which is the safe
/// direction for the tick: a task it cannot see is a task it will not start a
/// second attempt on.
pub fn live_tasks(mux: &dyn Host, home: &Path, state: &RunState) -> BTreeSet<String> {
    let mut live = BTreeSet::new();
    let Some(run) = state.run.as_ref() else {
        return live;
    };
    // One tab per task, and each task's tab is in that task's own space, so
    // every space the host lists is asked for its tabs.
    let Ok(spaces) = mux.list_spaces() else {
        return live;
    };
    let tabs: Vec<shprd_mux::Tab> = spaces
        .iter()
        .flat_map(|space| mux.list_tabs(&space.id).unwrap_or_default())
        .collect();
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
        // Done, Unknown or a host error all mean the same thing here: no agent
        // is at work in that pane, or nobody can say — and either way the task
        // is not "live".
        if let Ok(AgentState::Working | AgentState::Idle | AgentState::Blocked) =
            mux.agent_state(&tab.root_pane)
        {
            live.insert(id.clone());
        }
    }
    live
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
        let created = create_worktree(&env, "T1").unwrap_or_else(|err| panic!("{err}"));
        // The third append is the `Spawned` event; the two resources land.
        let mut commits = 0;
        let outcome = append_in_order(attempt_events("T1", 1, &pick(), &created), |event| {
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

    #[test]
    fn live_tasks_reads_the_host_state_of_each_attempts_own_tab() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.spawn_env(&ctx);
        let mux = FakeMux::default().with_ready_agent();
        let report = spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));
        // The agent's tab is in the task's own space, which the spawn made.
        let pane = mux
            .list_spaces()
            .unwrap_or_else(|err| panic!("{err}"))
            .iter()
            .flat_map(|space| mux.list_tabs(&space.id).unwrap_or_default())
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
            brief.contains("- Permissions: bypassPermissions (built in)"),
            "the block names the mode in force, not the one that was set: {brief}"
        );
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

    /// The pane the service records is the pane the chat types into: this is the
    /// path every coordinator line in the M10 flow takes, since nothing launches
    /// a coordinator for the run to record.
    #[test]
    fn a_coordinator_line_goes_to_the_pane_the_service_recorded() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let mux = FakeMux::default().with_starter_tab();
        let space = repo_space(&mux, &scratch);
        let pane = record_coordinator_pane(scratch.home.path(), &mux, &space)
            .unwrap_or_else(|err| panic!("{err}"))
            .unwrap_or_else(|| panic!("the space has a pane to record"));
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
                err.to_string()
                    .contains("no pane is recorded for the coordinator"),
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
        let path = coordinator_pane_path(scratch.home.path());
        let dir = path.parent().unwrap_or_else(|| panic!("a parent"));
        std::fs::create_dir_all(dir).unwrap_or_else(|err| panic!("{err}"));
        std::fs::write(&path, "p-closed\n").unwrap_or_else(|err| panic!("{err}"));
        let _ = mux.take_calls();

        let refused = prompt_coordinator(&mux, scratch.home.path(), &ctx, "hello").is_err();
        assert!(refused, "a pane the space no longer has is refused");
        assert!(writes(&mux).is_empty(), "nothing was typed");

        // And the space's own tab is still fine once it is the one recorded.
        let pane = record_coordinator_pane(scratch.home.path(), &mux, &space)
            .unwrap_or_else(|err| panic!("{err}"))
            .unwrap_or_else(|| panic!("a pane"));
        let _ = mux.take_calls();
        prompt_coordinator(&mux, scratch.home.path(), &ctx, "hello")
            .unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(
            writes(&mux),
            vec![(pane.as_str().to_string(), "hello".to_string())]
        );
    }

    /// A run that recorded its own coordinator pane wins over the service's
    /// record: it is the pane a launcher owns.
    #[test]
    fn the_runs_recorded_coordinator_pane_wins() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let mux = FakeMux::default().with_starter_tab();
        let space = repo_space(&mux, &scratch);
        let _ = record_coordinator_pane(scratch.home.path(), &mux, &space).unwrap_or_default();
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
            brief.starts_with("<!--"),
            "the shipped worker prompt leads the brief: {brief}"
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
        let space = task_space(&mux, &env, "T1").unwrap_or_else(|err| panic!("{err}"));
        let tabs = mux
            .list_tabs(&space.id)
            .unwrap_or_else(|err| panic!("{err}"));
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
        let name = task_space_name(&scratch.repo_name(), &env.run, "T1");
        let space = mux
            .find_space(&name)
            .unwrap_or_else(|err| panic!("{err}"))
            .unwrap_or_else(|| panic!("no space {name:?}"));
        assert_eq!(
            tab.0, space.id,
            "the agent's tab is in the task's own space"
        );
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
        fail_review(&ctx, 1);

        let second = spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));

        assert_eq!(second.attempt, 2);
        let replaced = second
            .replaced
            .clone()
            .unwrap_or_else(|| panic!("the retry must name the ref it archived"));
        assert_eq!(replaced.attempt, 1);
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
        // Every attempt works at the same path, so what proves the old worktree
        // was replaced rather than reused is what is not in it: the fresh one is
        // cut from the integration branch's current head, and the replaced
        // attempt's commit is only reachable through the archive ref.
        assert!(second.worktree.is_dir(), "the new worktree is missing");
        assert!(
            !second.worktree.join("retry.txt").exists(),
            "the fresh worktree still carries the replaced attempt's work"
        );
        assert_eq!(
            git_out(scratch.root(), &["rev-parse", &second.branch]),
            git_out(scratch.root(), &["rev-parse", "integration/m4"])
        );
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
        let space = task_space(&mux, &env, "T1").unwrap_or_else(|err| panic!("{err}"));
        let tabs = mux
            .list_tabs(&space.id)
            .unwrap_or_else(|err| panic!("{err}"));
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

    /// A task's space is shprd's own to make, so a spawn does not depend on a
    /// person having opened anything: the space it works in is labelled from the
    /// task and made at the repository root.
    #[test]
    fn a_spawn_makes_the_task_space_and_works_in_it() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.spawn_env(&ctx);
        let mux = FakeMux::default().with_ready_agent();

        let report = spawn(&mux, &env, &request()).unwrap_or_else(|err| panic!("{err}"));

        let name = task_space_name(&scratch.repo_name(), &env.run, "T1");
        let space = mux
            .find_space(&name)
            .unwrap_or_else(|err| panic!("{err}"))
            .unwrap_or_else(|| panic!("no space {name:?}"));
        let tabs = mux
            .list_tabs(&space.id)
            .unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(tabs.len(), 1, "{tabs:?}");
        assert_eq!(tabs[0].title, report.tab);
        assert!(
            mux.calls().iter().any(|call| matches!(
                call,
                Call::CreateSpace(label, cwd)
                    if label == &name && cwd == scratch.root()
            )),
            "the task's space is made at the repository root: {:?}",
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
        // Each task's tab is in that task's own space, and both are titled
        // after their own task: which one a prompt reaches is decided by the
        // record beside the brief, not by the title or the space alone.
        let first = "task-one";
        let second = "task-two";
        let other_space = mux
            .create_space(
                &task_space_name(&scratch.repo_name(), "t1", first),
                scratch.root(),
            )
            .unwrap_or_else(|err| panic!("{err}"));
        let mine_space = mux
            .create_space(
                &task_space_name(&scratch.repo_name(), "t1", second),
                scratch.root(),
            )
            .unwrap_or_else(|err| panic!("{err}"));
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
        // One space per run-and-task, so the two records below name tabs in
        // two different spaces: the active run's is the one that gets typed at.
        let stale_space = mux
            .create_space(
                &task_space_name(&scratch.repo_name(), "run-one", "api"),
                scratch.root(),
            )
            .unwrap_or_else(|err| panic!("{err}"));
        let alive_space = mux
            .create_space(
                &task_space_name(&scratch.repo_name(), "run-two", "api"),
                scratch.root(),
            )
            .unwrap_or_else(|err| panic!("{err}"));
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
    fn a_task_gets_a_space_labelled_from_the_task() {
        // The space-per-task layout: the label names the repository, the run
        // and the task, so two runs of one task never share a space and a human
        // reading the host's bar knows whose terminals these are.
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.spawn_env(&ctx);
        let mux = FakeMux::default();

        let space = task_space(&mux, &env, "T1").unwrap_or_else(|err| panic!("{err}"));

        assert_eq!(
            space.name,
            task_space_name(&scratch.repo_name(), &env.run, "T1")
        );
        assert_ne!(space.name, repo_space_name(&scratch.repo_name()));
        // Ensured, not merely found: the first spawn is what makes it.
        assert!(
            mux.calls()
                .iter()
                .any(|call| matches!(call, Call::EnsureSpace(..))),
            "the task's space is ensured: {:?}",
            mux.calls()
        );
    }

    #[test]
    fn a_task_space_the_host_already_has_is_reused() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        let env = scratch.spawn_env(&ctx);
        let mux = FakeMux::default();
        let first = task_space(&mux, &env, "T1").unwrap_or_else(|err| panic!("{err}"));
        let _ = mux.take_calls();

        let again = task_space(&mux, &env, "T1").unwrap_or_else(|err| panic!("{err}"));

        assert_eq!(first, again, "one task, one space");
        assert!(
            !mux.calls()
                .iter()
                .any(|call| matches!(call, Call::CreateSpace(..))),
            "a second spawn does not make a second space: {:?}",
            mux.calls()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shprd_agents::skills::Role as SkillRole;
    use shprd_core::event::{Finding, Severity};
    use shprd_core::state::{GateResult, TaskStatus};

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
        });
        for needle in [
            "# M4.4 Make spawn real",
            "attempt 2",
            "claude-opus-5-5",
            "## Acceptance criteria",
            "spawn opens a tab",
            "## Scope — change only these paths",
            "crates/shprd",
            "## Skills to load first",
            "- tdd",
            "never push",
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
