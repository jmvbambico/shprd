//! The command-line surface: global options and every subcommand.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use shprd_core::event::ReportStatus;

/// The `shprd` command line.
#[derive(Debug, Parser)]
#[command(
    name = "shprd",
    version,
    about = "Orchestrate coding agents inside a terminal multiplexer"
)]
pub struct Cli {
    /// Options shared by every subcommand.
    #[command(flatten)]
    pub global: Global,
    /// What to do; `shprd` with no subcommand prints the status.
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Options every subcommand accepts.
#[derive(Debug, Args)]
pub struct Global {
    /// Repository root; defaults to `$SHPRD_REPO`, then the git top level of the
    /// current directory.
    ///
    /// A linked git worktree resolves to the repository it was cut from, so a
    /// worker's own verbs find the run from its worktree; `$SHPRD_REPO` keeps
    /// that meaning, and a verb that writes into the checkout itself (ask
    /// install) still uses the worktree it names.
    #[arg(long, global = true, value_name = "PATH")]
    pub repo: Option<PathBuf>,
    /// Who is acting: `engine`, `orchestrator`, `human` or `agent:<ID>`.
    ///
    /// Defaults to `$SHPRD_ACTOR`, or `human` when that is unset.
    #[arg(long = "as", global = true, value_name = "ACTOR")]
    pub actor: Option<String>,
    /// Print machine-readable JSON on stdout instead of a human line.
    #[arg(long, global = true)]
    pub json: bool,
    /// Skip the dagr sync that normally follows every write.
    #[arg(long = "no-dagr", global = true)]
    pub no_dagr: bool,
    /// Home directory whose `.shprd` directory holds shprd's git state:
    /// worktrees, archive refs.
    ///
    /// Defaults to `$SHPRD_HOME`, then `$HOME`.
    #[arg(long, global = true, value_name = "PATH")]
    pub home: Option<PathBuf>,
}

/// The top-level verbs.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run lifecycle: start and finish a run.
    Run {
        /// Which lifecycle verb.
        #[command(subcommand)]
        cmd: RunCommand,
    },
    /// Agent-facing verbs: plan, spawn, report, gate, escalate, integrate.
    Agent {
        /// Which agent verb.
        #[command(subcommand)]
        cmd: AgentCommand,
    },
    /// Agent-callable task verbs: record where a task stands.
    Task {
        /// Which task verb.
        #[command(subcommand)]
        cmd: TaskVerb,
    },
    /// Asks: the hook a program runs when its agent waits for an answer.
    Ask {
        /// Which ask verb.
        #[command(subcommand)]
        cmd: AskCommand,
    },
    /// Print the folded run state.
    State,
    /// Print the rehydration brief: what an agent needs to resume.
    Brief,
    /// Print where every task sits, one line each.
    Status,
    /// Print the pull-request body for the run.
    PrBody,
    /// dagr document verbs: project the run as dagr v3 and write it.
    Dagr {
        /// Which dagr verb.
        #[command(subcommand)]
        cmd: DagrCommand,
    },
    /// Print the cleanup plan for this run, or apply it with `--apply`.
    Janitor(JanitorArgs),
    /// Recreate a task's branch from its newest attempt's archive ref.
    Restore(RestoreArgs),
    /// Detect the agent CLIs installed here, their version and sign-in state.
    Roster(RosterArgs),
    /// Write a roster role into the global config, or with `--repo <PATH>`
    /// into that repository's `.shprd/config.toml`.
    Set(SetArgs),
    /// Read and write the settings: roles, the workflow keys, their layers.
    ///
    /// The layers are resolved nearest first: the task's own record, your entry
    /// for this repository, your defaults, the repository's committed
    /// `.shprd/settings.toml` (allow-listed keys only) and then the values shprd
    /// ships. Every value is printed with the layer it came from.
    Settings {
        /// Which settings verb.
        #[command(subcommand)]
        cmd: SettingsCommand,
    },
    /// Read and write the prompts the roles start with.
    ///
    /// The shipped words are written to `~/.config/shprd/prompts/<id>.md`
    /// (`$XDG_CONFIG_HOME` wins) on first use. An installed file is replaced
    /// with today's words only when it is byte-identical to a retired default,
    /// so an unedited default never goes stale and an edit is never lost;
    /// `reset` is the explicit way back to the shipped words.
    Prompts {
        /// Which prompts verb.
        #[command(subcommand)]
        cmd: PromptsCommand,
    },
    /// Check this machine's setup and print what to fix.
    Doctor(DoctorArgs),
    /// Watch every working agent and hand work over when one goes dry.
    Watch(WatchArgs),
    /// Run the integration branch's gate commands once, in its worktree.
    Gate(FullGateArgs),
    /// Merge the base branch into the run's integration branch, so a run that
    /// outlived a merged pull request lands cleanly. Merges, never rebases; a
    /// conflict is undone and escalated. Needs no terminal.
    Sync,
    /// Land a task through its own pull request, as `[land] merge` says; with
    /// no task, push the run's integration branch and open its pull request,
    /// merging it with `--merge` when `[land]` allows.
    Land(LandArgs),
    /// Approve a pull request in shprd, for a maintainer whose forge will not
    /// let them approve their own: the run's, or with a task, that task's. It
    /// asks on the terminal, which an agent's shell tool never has.
    Approve(ApproveArgs),
    /// Re-admit a dry agent: record that the run may use it again, so a spawn
    /// or a handoff stops refusing it. A human's verb, needing no terminal.
    Readmit(ReadmitArgs),
    /// Override a rule for a task, e.g. let it integrate although a gate
    /// failed. It asks on the terminal, which an agent's shell tool never has.
    Override(OverrideArgs),
    /// Drop a task a human decided not to do: it stops blocking the run, and
    /// its branch is kept. It asks on the terminal, which an agent's shell tool
    /// never has.
    Drop(DropArgs),
    /// Serve this run over a loopback HTTP API, for every UI that calls it.
    Serve(ServeArgs),
    /// Advance the run once, now: start what may start, land what may land.
    ///
    /// This is the same pass `shprd serve` runs every `tasks.tick_secs`
    /// (15 by default, also settable as `[tick] interval_secs`). It starts a
    /// queued task whose `deps` have all landed, up to `tasks.max_running`
    /// (4 by default) tasks running at once, starts a reviewer on a hand-in,
    /// records the verdict when the reviewer's findings have settled, lands a
    /// task `tasks.merge` allows — asked about the branch the landing writes —
    /// records a pull request merged outside shprd, and says once when a ready
    /// task's policy is `ask`.
    ///
    /// `--once` spells out that this invocation is one pass; the verb never
    /// loops. With `--json` it prints one document, `{"steps": [...]}`, each step being
    /// `{action, task, ok, detail, seq?}` where `action` is one of `start`,
    /// `review`, `verdict`, `land`, `mark_landed`, `needs_you`, `nudge`,
    /// `backup` or `give_up`, and `seq` is the log line it appended
    /// when it appended one. Without `--json` it prints one line per action, or
    /// `ok nothing to advance` when the pass found nothing to do.
    ///
    /// A step that fails is printed and left in the report; the pass carries on
    /// with the next task.
    Tick(TickArgs),
}

/// Arguments for `tick`.
#[derive(Debug, Args)]
pub struct TickArgs {
    /// Run one pass and exit. This is what the verb does; the flag spells it
    /// out, as `shprd watch --once` does for its own loop.
    #[arg(long)]
    pub once: bool,
}

/// Arguments for `gate`, the run-wide gate. The per-attempt gate is
/// `shprd agent gate`, whose arguments are [`GateArgs`].
#[derive(Debug, Args)]
pub struct FullGateArgs {
    /// How long one command may run before it is killed, in seconds.
    #[arg(long = "timeout-secs", value_name = "SECS", default_value_t = 1800)]
    pub timeout_secs: u64,
}

/// Arguments for `run finish`, the verb that ends a run.
#[derive(Debug, Args)]
pub struct EndRunArgs {
    /// End the run even though tasks are unfinished, keeping their branches.
    ///
    /// A run whose every task landed and whose pull request merged needs no
    /// flag. This is the explicit choice for the rest: the event records which
    /// tasks were left behind, and their branches are kept. It asks a human at
    /// a terminal first, naming the run and the tasks it leaves.
    #[arg(long)]
    pub abandon: bool,
    /// Answer the abandon's question for the record without asking, in a debug
    /// build only: a release binary ignores it and still needs a terminal.
    /// `SHPRD_ACTOR` still has to be unset or `human`.
    #[arg(long, hide = !cfg!(debug_assertions))]
    pub yes: bool,
}

/// Arguments for `land`.
#[derive(Debug, Args)]
pub struct LandArgs {
    /// The task to land through its own pull request. Without one, `land`
    /// pushes the run's integration branch instead (the pre-M8.3 run-level
    /// pull request), where `--merge` applies.
    #[arg(value_name = "TASK")]
    pub task: Option<String>,
    /// Merge the run's pull request instead of only opening it.
    ///
    /// A task's landing merges as `[land] merge` says, so this flag belongs to
    /// the run-level pull request and is refused with a task.
    #[arg(long)]
    pub merge: bool,
}

/// Arguments for `approve`.
#[derive(Debug, Args)]
pub struct ApproveArgs {
    /// Approve this task's own pull request instead of the run's.
    #[arg(value_name = "TASK")]
    pub task: Option<String>,
    /// Answer the question for the record without asking, in a debug build
    /// only: a release binary ignores it and still needs a terminal.
    /// `SHPRD_ACTOR` still has to be unset or `human`.
    #[arg(long, hide = !cfg!(debug_assertions))]
    pub yes: bool,
}

/// Arguments for `drop`.
#[derive(Debug, Args)]
pub struct DropArgs {
    /// The task to drop.
    #[arg(value_name = "TASK")]
    pub task: String,
    /// Why it is dropped, in the human's words. The run's pull request lists it,
    /// so a reader sees what is missing and why.
    #[arg(long, value_name = "TEXT")]
    pub reason: String,
    /// Answer the question for the record without asking, in a debug build only:
    /// a release binary ignores it and still needs a terminal. `SHPRD_ACTOR`
    /// still has to be unset or `human`.
    #[arg(long, hide = !cfg!(debug_assertions))]
    pub yes: bool,
}

/// Arguments for `serve`.
#[derive(Debug, Args)]
pub struct ServeArgs {
    /// Port to listen on, on 127.0.0.1 only. `0` picks a free one.
    ///
    /// The port a service really bound is written to `service.json` in the
    /// state directory, so a client reads it there rather than guessing.
    ///
    /// Every pass's failed step is logged on stderr at the level `SHPRD_LOG`
    /// sets — `warn` by default, or `info`, `debug`, or a per-target form such
    /// as `shprd_server=debug`.
    #[arg(long, value_name = "PORT", default_value_t = 0)]
    pub port: u16,
}

/// Arguments for `readmit`.
#[derive(Debug, Args)]
pub struct ReadmitArgs {
    /// The agent to re-admit: the program shprd marked dry (`pi`), or the
    /// `program/model` form the roster holds.
    #[arg(value_name = "AGENT")]
    pub agent: String,
    /// Why the agent is usable again, recorded in the log.
    #[arg(long, value_name = "TEXT", default_value = "re-admitted by a human")]
    pub reason: String,
}

/// Arguments for `watch`.
#[derive(Debug, Args)]
pub struct WatchArgs {
    /// Run one polling pass and exit, instead of watching until stopped.
    #[arg(long)]
    pub once: bool,
    /// How long to wait between polling passes, in milliseconds.
    #[arg(long = "interval-ms", value_name = "MS", default_value_t = 10_000)]
    pub interval_ms: u64,
}

/// Arguments for `janitor`.
#[derive(Debug, Args)]
pub struct JanitorArgs {
    /// Execute the plan instead of only printing it.
    #[arg(long)]
    pub apply: bool,
    /// Days an archive ref is kept before it is expired.
    ///
    /// Defaults to `[janitor] archive_days` from the config, or 14.
    #[arg(long = "archive-days", value_name = "DAYS")]
    pub archive_days: Option<u32>,
}

/// Arguments for `restore`.
#[derive(Debug, Args)]
pub struct RestoreArgs {
    /// The task whose branch comes back.
    #[arg(value_name = "TASK")]
    pub task: String,
    /// The run, when the log holds more than one; defaults to the log's run.
    #[arg(long, value_name = "ID")]
    pub run: Option<String>,
}

/// Arguments for `roster`.
#[derive(Debug, Args)]
pub struct RosterArgs {
    /// Print the detections as a JSON array instead of one line per CLI.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `set`.
#[derive(Debug, Args)]
pub struct SetArgs {
    /// Which role to write.
    #[command(subcommand)]
    pub role: SetRole,
}

/// The roster role `set` writes.
#[derive(Debug, Subcommand)]
pub enum SetRole {
    /// The agent that plans the run.
    Orchestrator(OneAgent),
    /// The agent that reviews, which must differ from the orchestrator.
    Reviewer(OneAgent),
    /// The agents workers fall back through, in order; replaces the list.
    Workers(ManyAgents),
    /// Whether the orchestrator launches without permission prompts.
    OrchestratorAutoApprove(Switch),
}

/// The `on|off` positional of a boolean setting.
#[derive(Debug, Args)]
pub struct Switch {
    /// `on` turns the setting on, `off` turns it off.
    #[arg(value_name = "on|off")]
    pub state: Toggle,
}

/// The two states a [`Switch`] accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Toggle {
    /// Turn the setting on.
    On,
    /// Turn the setting off.
    Off,
}

/// One agent string with the effort it runs at.
#[derive(Debug, Args)]
pub struct OneAgent {
    /// `<program>/<model>`, e.g. `claude/claude-opus-5-5`.
    #[arg(value_name = "AGENT")]
    pub agent: String,
    /// Effort for this agent: low, medium, high, xhigh or max.
    #[arg(long, value_name = "EFFORT")]
    pub effort: Option<String>,
}

/// The agent strings for `set workers`, in fallback order.
#[derive(Debug, Args)]
pub struct ManyAgents {
    /// `<program>/<model>`, repeated in fallback order.
    #[arg(value_name = "AGENT", required = true)]
    pub agents: Vec<String>,
}

/// Settings verbs.
#[derive(Debug, Subcommand)]
pub enum SettingsCommand {
    /// Print every key, its value and the layer it came from.
    Show(SettingsShowArgs),
    /// Print one key's value and where it came from.
    Get(SettingsGetArgs),
    /// Set one key: your own default, or this repository's entry with `--for-repo`.
    Set(SettingsSetArgs),
    /// Remove one key, so it inherits again.
    Unset(SettingsUnsetArgs),
}

/// Arguments for `settings show`.
#[derive(Debug, Args)]
pub struct SettingsShowArgs {
    /// Resolve for this task: the agent its own newest attempt ran is the
    /// nearest layer.
    #[arg(long, value_name = "TASK")]
    pub task: Option<String>,
}

/// Arguments for `settings get`.
#[derive(Debug, Args)]
pub struct SettingsGetArgs {
    /// The key, e.g. `tasks.merge` or `roles.worker.agent`. The names the old
    /// config file used (`tick.max_running`, `janitor.archive_days`, ...) are
    /// accepted too.
    #[arg(value_name = "KEY")]
    pub key: String,
    /// Resolve for this task.
    #[arg(long, value_name = "TASK")]
    pub task: Option<String>,
}

/// Arguments for `settings set`.
#[derive(Debug, Args)]
pub struct SettingsSetArgs {
    /// The key, e.g. `roles.reviewer.agent` or `tasks.max_running`.
    #[arg(value_name = "KEY")]
    pub key: String,
    /// The value. Lists are items separated by commas, switches are
    /// `true`/`false` (or `on`/`off`), and everything else is taken as written.
    #[arg(value_name = "VALUE")]
    pub value: String,
    /// Write this repository's entry instead of your own defaults.
    ///
    /// The entry is keyed by the repository folder's name; a repository's
    /// committed `.shprd/settings.toml` is not written from here, because
    /// changing it is a commit in that repository.
    #[arg(long = "for-repo")]
    pub for_repo: bool,
    /// Refused: a task's own settings come from its attempts in the run log.
    #[arg(long, value_name = "TASK")]
    pub task: Option<String>,
}

/// Arguments for `settings unset`.
#[derive(Debug, Args)]
pub struct SettingsUnsetArgs {
    /// The key to remove.
    #[arg(value_name = "KEY")]
    pub key: String,
    /// Remove it from this repository's entry instead of your own defaults.
    #[arg(long = "for-repo")]
    pub for_repo: bool,
    /// Refused: a task's own settings come from its attempts in the run log.
    #[arg(long, value_name = "TASK")]
    pub task: Option<String>,
}

/// Prompts verbs.
#[derive(Debug, Subcommand)]
pub enum PromptsCommand {
    /// Print every prompt, its state and the file it is stored in.
    List,
    /// Print one prompt's words.
    Show(PromptIdArgs),
    /// Print the file one prompt is stored in, writing it first on first use.
    Path(PromptIdArgs),
    /// Put the shipped words back over whatever the file holds.
    Reset(PromptIdArgs),
}

/// Arguments naming one prompt.
#[derive(Debug, Args)]
pub struct PromptIdArgs {
    /// The prompt: `assistant` (the coordinator's, also `coordinator`),
    /// `worker`, `reviewer`, `backup`, or the id of a prompt of your own.
    #[arg(value_name = "ID")]
    pub id: String,
}

/// Arguments for `doctor`.
#[derive(Debug, Args)]
pub struct DoctorArgs {
    /// Print the report as JSON instead of one indented page.
    #[arg(long)]
    pub json: bool,
    /// Skip the quota probes, which touch the network.
    #[arg(long = "no-quota")]
    pub no_quota: bool,
}

/// dagr verbs.
#[derive(Debug, Subcommand)]
pub enum DagrCommand {
    /// Project the run as `<root>/.dagr/run.json`, checking it when dagr is
    /// available.
    Sync,
}

/// Run lifecycle verbs.
#[derive(Debug, Subcommand)]
pub enum RunCommand {
    /// Start a run: the first event in the log.
    Start(RunStartArgs),
    /// Finish the run; nothing may follow.
    Finish(EndRunArgs),
}

/// Arguments for `run start`.
#[derive(Debug, Args)]
pub struct RunStartArgs {
    /// Run identifier, e.g. `m1`.
    #[arg(long, value_name = "ID")]
    pub run_id: String,
    /// Branch the run was cut from.
    #[arg(long, value_name = "BRANCH")]
    pub base: String,
    /// Branch landed work is integrated on.
    #[arg(long, value_name = "BRANCH")]
    pub integration: String,
}

/// Agent-facing verbs.
#[derive(Debug, Subcommand)]
pub enum AgentCommand {
    /// Plan verbs: add a task to the run plan.
    Task {
        /// Which plan verb.
        #[command(subcommand)]
        cmd: TaskCommand,
    },
    /// Spawn an agent for a task attempt: worktree, tab and launched CLI.
    Spawn(SpawnArgs),
    /// Send one turn of input to the tab a spawned agent is working in.
    Prompt(PromptArgs),
    /// Record an agent's report for a task attempt.
    Report(ReportArgs),
    /// Record a gate result for a task attempt.
    Gate(GateArgs),
    /// Decide a review from the reviewer's packet and record what follows.
    ///
    /// Engine-only: the findings are read from the packet folder the reviewer
    /// wrote, and the verdict carries that reviewer's provenance.
    ReviewVerdict(ReviewVerdictArgs),
    /// Escalate something to a human.
    Escalate(EscalateArgs),
    /// Record a human overriding a rule.
    ///
    /// Kept as a hidden alias of `shprd override`, which is the verb a human
    /// runs: the rule refuses an override from anything but a human anyway.
    #[command(hide = true)]
    Override(OverrideArgs),
    /// Record that a task's commits landed.
    Integrate(IntegrateArgs),
    /// Hand a task from one agent to the next, keeping its worktree.
    Handoff(HandoffArgs),
    /// Open a cross-vendor reviewer on a finished attempt's diff.
    Review(ReviewArgs),
}

/// Plan verbs.
#[derive(Debug, Subcommand)]
pub enum TaskCommand {
    /// Add a task to the run plan.
    Add(TaskAddArgs),
    /// Hand the attempt in: what it did, and the checks it ran.
    ///
    /// The same verb as `shprd agent report`; this is the spelling a worker's
    /// brief uses.
    Report(ReportArgs),
}

/// The ask verbs.
#[derive(Debug, Subcommand)]
pub enum AskCommand {
    /// Write one ask from a program's hook payload and wait for its answer.
    ///
    /// The payload is JSON on stdin, in the shape Claude Code and Codex send to
    /// a `PermissionRequest` hook. Nothing is printed on timeout or on any
    /// error: the hook never decides by itself.
    Hook(AskHookArgs),
    /// Raise one ask yourself, for a program whose CLI has no ask hook.
    ///
    /// This is the agent-facing form: it writes the card into the run's queue,
    /// records it in the ledger and blocks until the person answers, then
    /// prints the answer on stdout for the agent to carry on with. A timeout
    /// prints nothing, leaves the card open and exits non-zero, because nobody
    /// answered. A worker's brief names this verb only for a program whose
    /// harness says it has no ask hook (`[asks] supported = false`).
    Raise(AskRaiseArgs),
    /// Write a program's ask hook into a task's worktree-local config file.
    ///
    /// The target is the program's own project file — Claude Code's
    /// `.claude/settings.local.json`, Codex's `.codex/hooks.json` plus
    /// `.codex/config.toml` — never the person's global config. A program with
    /// no hook of its own (pi, opencode) is left untouched, and the board falls
    /// back to reading its screen.
    Install(AskInstallArgs),
}

/// Arguments for `ask raise`.
#[derive(Debug, Args)]
pub struct AskRaiseArgs {
    /// What kind of ask this is: a question, a permission or a plan.
    #[arg(long, value_enum, value_name = "KIND", default_value = "question")]
    pub kind: AskKindArg,
    /// What the person is asked, in one sentence naming what you need and why.
    #[arg(long, value_name = "TEXT")]
    pub text: String,
    /// A choice the card offers. Repeat for two to four, your recommendation
    /// first. A permission or plan with no choices offers Allow and Deny.
    #[arg(long = "option", value_name = "LABEL")]
    pub options: Vec<String>,
    /// How long to wait for an answer, in seconds.
    ///
    /// Defaults to 1800 seconds. A timeout prints nothing on stdout, leaves the
    /// card open and exits non-zero: nothing is decided on the person's behalf.
    #[arg(long = "timeout-secs", value_name = "SECS")]
    pub timeout_secs: Option<u64>,
    /// The task the ask belongs to; normally `$SHPRD_TASK` inside a worker's
    /// worktree.
    #[arg(long, value_name = "TASK")]
    pub task: Option<String>,
    /// The ask's id; one is made when it is omitted.
    #[arg(long, value_name = "ID")]
    pub id: Option<String>,
}

/// Arguments for `ask install`.
#[derive(Debug, Args)]
pub struct AskInstallArgs {
    /// The program whose hook is installed: `claude`, `codex`, and so on.
    #[arg(long, value_name = "PROGRAM", default_value = "claude")]
    pub program: String,
    /// The task's worktree, whose config file gets the hook. Defaults to the
    /// working directory.
    #[arg(long, value_name = "DIR")]
    pub worktree: Option<PathBuf>,
    /// Where the hook script goes. Defaults to `<worktree>/.shprd/ask-hook.sh`.
    #[arg(long, value_name = "PATH")]
    pub hook: Option<PathBuf>,
}

/// What kind of ask a flag names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum AskKindArg {
    /// A question the agent asks and waits on.
    Question,
    /// Permission to run a tool.
    Permission,
    /// A plan to approve.
    Plan,
}

/// Arguments for `ask hook`.
#[derive(Debug, Args)]
pub struct AskHookArgs {
    /// The program whose hook is calling: `claude`, `codex`, and so on.
    #[arg(long, value_name = "PROGRAM", default_value = "claude")]
    pub program: String,
    /// How long to wait for an answer, in seconds.
    ///
    /// Defaults to the harness's ask timeout, or 1800 seconds. A test uses a
    /// short one; a timeout prints nothing, as the program's own prompt stays.
    #[arg(long = "timeout-secs", value_name = "SECS")]
    pub timeout_secs: Option<u64>,
    /// The task the ask belongs to; normally `$SHPRD_TASK` inside the worktree.
    #[arg(long, value_name = "TASK")]
    pub task: Option<String>,
    /// The ask's kind, when the payload is not a program's hook shape.
    #[arg(long, value_enum, value_name = "KIND")]
    pub kind: Option<AskKindArg>,
    /// What the person is asked, when the payload is not a program's hook shape.
    #[arg(long, value_name = "TEXT")]
    pub text: Option<String>,
    /// A choice a card offers. Repeat for more than one.
    #[arg(long = "option", value_name = "LABEL")]
    pub options: Vec<String>,
    /// The ask's id; one is made when it is omitted.
    #[arg(long, value_name = "ID")]
    pub id: Option<String>,
}

/// Agent-callable task verbs.
#[derive(Debug, Subcommand)]
pub enum TaskVerb {
    /// Record a short progress line for a task.
    Status(TaskStatusArgs),
}

/// Arguments for `task status`.
#[derive(Debug, Args)]
pub struct TaskStatusArgs {
    /// The task.
    #[arg(value_name = "TASK")]
    pub task: String,
    /// One short line saying where the work stands; capped at 40 characters.
    #[arg(long, value_name = "TEXT")]
    pub msg: String,
    /// How far along the task is, from 0 to 100.
    #[arg(long, value_name = "PERCENT", value_parser = clap::value_parser!(u8).range(0..=100))]
    pub progress: Option<u8>,
}

/// Arguments for `agent task add`.
#[derive(Debug, Args)]
pub struct TaskAddArgs {
    /// Task id, e.g. `M1.6`.
    #[arg(long, value_name = "ID")]
    pub id: String,
    /// One-line title.
    #[arg(long, value_name = "TITLE")]
    pub title: String,
    /// Repo-relative path the task may change; repeatable.
    #[arg(long = "scope", value_name = "PATH", required = true)]
    pub scope: Vec<String>,
    /// What must be true when the task is done; repeatable.
    #[arg(long = "acceptance", value_name = "TEXT", required = true)]
    pub acceptance: Vec<String>,
    /// Free-form tag; repeatable.
    #[arg(long = "tag", value_name = "TAG")]
    pub tags: Vec<String>,
    /// Skill the agent must load; repeatable.
    #[arg(long = "skill", value_name = "SKILL")]
    pub skills: Vec<String>,
    /// Task that must land first; repeatable.
    #[arg(long = "dep", value_name = "ID")]
    pub deps: Vec<String>,
}

/// Arguments for `agent spawn`.
///
/// The run and the task decide where the agent works: the worktree, the branch
/// and the attempt number all come from the log and the layout, so the agent is
/// the only thing left to choose.
#[derive(Debug, Args)]
pub struct SpawnArgs {
    /// The task.
    #[arg(long, value_name = "ID")]
    pub task: String,
    /// The agent taking the attempt, as `<program>/<model>`.
    ///
    /// Defaults to the first configured worker whose CLI is installed.
    #[arg(long, value_name = "PROGRAM/MODEL")]
    pub agent: Option<String>,
    /// Effort level for this attempt, e.g. `high`.
    #[arg(long, value_name = "EFFORT")]
    pub effort: Option<String>,
    /// Only record the attempt in the log: no worktree, no tab, no CLI.
    ///
    /// This keeps the record-only behaviour for callers whose worktree a human
    /// made by hand; with it `--attempt`, `--worktree` and `--branch` are
    /// required.
    #[arg(long = "record-only", hide = true)]
    pub record_only: bool,
    /// Attempt number, for `--record-only` only.
    #[arg(long, value_name = "N")]
    pub attempt: Option<u32>,
    /// Worktree the agent works in, for `--record-only` only.
    #[arg(long, value_name = "PATH")]
    pub worktree: Option<PathBuf>,
    /// Branch the agent commits on, for `--record-only` only.
    #[arg(long, value_name = "BRANCH")]
    pub branch: Option<String>,
}

/// Arguments for `agent handoff`.
///
/// The task keeps its attempt, its worktree and its branch; only the agent in
/// the tab changes, so the hand-over is a checkpoint plus a new brief.
#[derive(Debug, Args)]
pub struct HandoffArgs {
    /// The task to hand over.
    #[arg(long, value_name = "ID")]
    pub task: String,
    /// Why it is being handed over; the next agent reads this.
    #[arg(long, value_name = "TEXT")]
    pub reason: String,
    /// The agent taking it over, as `<program>/<model>`.
    ///
    /// Defaults to the next configured worker after the current one.
    #[arg(long, value_name = "PROGRAM/MODEL")]
    pub to: Option<String>,
}

/// Arguments for `agent review`.
///
/// The task and the log decide the rest: the attempt, the branch, the diff and
/// the reviewer all come from the run, so the caller only chooses what to review
/// and whether to accept a review that is not cross-vendor.
#[derive(Debug, Args)]
pub struct ReviewArgs {
    /// The task whose latest attempt is reviewed.
    #[arg(long, value_name = "ID")]
    pub task: String,
    /// Review anyway when the reviewer shares the author's vendor.
    #[arg(long)]
    pub degraded: bool,
    /// Restart a review that is stuck: close its reviewer tab and hand the
    /// brief to a fresh one, for the same attempt and only before a verdict.
    #[arg(long)]
    pub again: bool,
}

/// Arguments for `agent prompt`.
#[derive(Debug, Args)]
pub struct PromptArgs {
    /// The task whose agent tab is prompted.
    #[arg(long, value_name = "ID")]
    pub task: String,
    /// The text to submit as one turn of input; quote it.
    #[arg(value_name = "TEXT")]
    pub text: String,
}

/// Arguments for `agent report`.
#[derive(Debug, Args)]
pub struct ReportArgs {
    /// The task.
    #[arg(long, value_name = "ID")]
    pub task: String,
    /// Attempt number.
    #[arg(long, value_name = "N")]
    pub attempt: u32,
    /// Outcome of the attempt.
    #[arg(long, value_name = "STATUS")]
    pub status: StatusArg,
    /// What the agent says it did.
    #[arg(long, value_name = "TEXT")]
    pub summary: String,
    /// A gate the worker ran, as `GATE|COMMAND|EXIT[|OUTPUT]`; repeatable.
    ///
    /// `EXIT` is the exit code, or `none` when a timeout killed the command.
    /// `OUTPUT` is the tail of what it printed; the last 20 lines are kept.
    /// A command containing `|` cannot be written this way; such a check can
    /// be given as a JSON object instead, e.g.
    /// `{"gate":"tests","command":"cargo test | tail -20","exit_code":0}`.
    /// A completed hand-in needs at least one, and every one must have exited
    /// 0, or the hand-in is refused.
    #[arg(
        long = "check",
        value_name = "GATE|COMMAND|EXIT[|OUTPUT]",
        action = clap::ArgAction::Append
    )]
    pub checks: Vec<String>,
    /// Test command and its result; repeatable.
    #[arg(long = "test", value_name = "CMD")]
    pub tests: Vec<String>,
    /// Path the agent changed; repeatable.
    #[arg(long = "changed", value_name = "PATH")]
    pub changed: Vec<String>,
    /// Commit the agent made.
    #[arg(long, value_name = "SHA")]
    pub commit: Option<String>,
    /// Skill the agent used; repeatable.
    #[arg(long = "skill-used", value_name = "SKILL")]
    pub skills_used: Vec<String>,
    /// Where the attempt left its proof — a screenshot, a command's real
    /// output, a test run; repeatable.
    ///
    /// The proof path belongs here, never in `--summary`: the summary is one
    /// plain line for the person, and a path there reads as jargon. The
    /// reviewer sees every proof in the packet it checks the work against.
    #[arg(long = "proof", value_name = "PATH")]
    pub proofs: Vec<String>,
}

/// Arguments for `agent gate`.
#[derive(Debug, Args)]
pub struct GateArgs {
    /// Whether the gate passed or failed.
    #[arg(value_enum)]
    pub outcome: GateOutcome,
    /// The task.
    #[arg(long, value_name = "ID")]
    pub task: String,
    /// Attempt number.
    #[arg(long, value_name = "N")]
    pub attempt: u32,
    /// Gate name, e.g. `lint`.
    #[arg(long, value_name = "NAME")]
    pub gate: String,
    /// JSON file holding an array of findings.
    #[arg(long, value_name = "FILE")]
    pub findings: Option<PathBuf>,
}

/// Arguments for `agent review-verdict`.
#[derive(Debug, Args)]
pub struct ReviewVerdictArgs {
    /// The task under review.
    #[arg(long, value_name = "ID")]
    pub task: String,
    /// Attempt number.
    #[arg(long, value_name = "N")]
    pub attempt: u32,
}

/// Arguments for `agent escalate`.
#[derive(Debug, Args)]
pub struct EscalateArgs {
    /// The task, when the escalation is not run-wide.
    #[arg(long, value_name = "ID")]
    pub task: Option<String>,
    /// What needs deciding.
    #[arg(long, value_name = "TEXT")]
    pub body: String,
    /// The action taken if nobody answers.
    #[arg(long = "default", value_name = "TEXT")]
    pub default_action: String,
}

/// Arguments for `override`, and its hidden `agent override` alias.
#[derive(Debug, Args)]
pub struct OverrideArgs {
    /// The task, or omitted for a run-wide rule.
    #[arg(long, value_name = "ID")]
    pub task: Option<String>,
    /// The rule being overridden.
    #[arg(long, value_name = "RULE")]
    pub rule: String,
    /// Why it was overridden.
    #[arg(long, value_name = "TEXT")]
    pub reason: String,
    /// Answer the terminal's question for the record.
    ///
    /// A test affordance, ignored by a release build: `--yes` must not be able
    /// to turn a pane into a human.
    #[arg(long, hide = true)]
    pub yes: bool,
}

/// Arguments for `agent integrate`.
#[derive(Debug, Args)]
pub struct IntegrateArgs {
    /// The task.
    #[arg(long, value_name = "ID")]
    pub task: String,
    /// Record that the task landed without running git.
    ///
    /// This is the behaviour M1 shipped, kept for callers that land work by
    /// hand and only need the ledger to know.
    #[arg(long = "record-only", hide = true)]
    pub record_only: bool,
}

/// The `--status` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum StatusArg {
    /// The attempt finished its scope.
    Completed,
    /// The attempt is stuck.
    Blocked,
    /// The attempt failed.
    Failed,
}

impl From<StatusArg> for ReportStatus {
    fn from(value: StatusArg) -> Self {
        match value {
            StatusArg::Completed => Self::Completed,
            StatusArg::Blocked => Self::Blocked,
            StatusArg::Failed => Self::Failed,
        }
    }
}

/// The `<pass|fail>` positional of `agent gate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum GateOutcome {
    /// The gate passed.
    Pass,
    /// The gate failed.
    Fail,
}
