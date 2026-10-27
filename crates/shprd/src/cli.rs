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
    /// Repository root; defaults to the git top level of the current directory.
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
    /// Recreate a task's branch from its archive ref.
    Restore(RestoreArgs),
    /// Detect the agent CLIs installed here, their version and sign-in state.
    Roster(RosterArgs),
    /// Write a roster role into the global config, or with `--repo <PATH>`
    /// into that repository's `.shprd/config.toml`.
    Set(SetArgs),
    /// Check this machine's setup and print what to fix.
    Doctor(DoctorArgs),
    /// Print, install or remove herdr's shprd keybinds.
    Keys(KeysArgs),
    /// Start or resume the run and lay out this repository's workspace.
    Up,
    /// Finish the run; refuses while a task is still working.
    Down,
    /// Watch every working agent and hand work over when one goes dry.
    Watch(WatchArgs),
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
    Finish,
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
    /// Escalate something to a human.
    Escalate(EscalateArgs),
    /// Record a human overriding a rule.
    Override(OverrideArgs),
    /// Record that a task's commits landed.
    Integrate(IntegrateArgs),
    /// Hand a task from one agent to the next, keeping its worktree.
    Handoff(HandoffArgs),
}

/// Plan verbs.
#[derive(Debug, Subcommand)]
pub enum TaskCommand {
    /// Add a task to the run plan.
    Add(TaskAddArgs),
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

/// Arguments for `agent override`.
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
}

/// Arguments for `agent integrate`.
#[derive(Debug, Args)]
pub struct IntegrateArgs {
    /// The task.
    #[arg(long, value_name = "ID")]
    pub task: String,
    /// Commit that landed; repeatable, at least one.
    #[arg(long = "commit", value_name = "SHA", required = true)]
    pub commits: Vec<String>,
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

/// Arguments for `keys`.
#[derive(Debug, Args)]
pub struct KeysArgs {
    /// Which keybind verb.
    #[command(subcommand)]
    pub cmd: KeysCommand,
    /// The herdr config file to read and write.
    ///
    /// Defaults to `$HERDR_CONFIG_PATH`, then
    /// `$HOME/.config/herdr/config.toml`.
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,
}

/// The keybind verbs.
#[derive(Debug, Subcommand)]
pub enum KeysCommand {
    /// Print the block shprd installs.
    Show,
    /// Install the block, or refresh an existing one in place.
    Install,
    /// Remove the block.
    Uninstall,
}
