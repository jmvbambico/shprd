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
    /// What to do.
    #[command(subcommand)]
    pub command: Command,
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
    /// Record that an agent was spawned for a task attempt.
    Spawn(SpawnArgs),
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
#[derive(Debug, Args)]
pub struct SpawnArgs {
    /// The task.
    #[arg(long, value_name = "ID")]
    pub task: String,
    /// Attempt number, starting at 1.
    #[arg(long, value_name = "N")]
    pub attempt: u32,
    /// The agent taking the attempt.
    #[arg(long, value_name = "AGENT")]
    pub agent: String,
    /// Model the agent runs on.
    #[arg(long, value_name = "MODEL")]
    pub model: Option<String>,
    /// Worktree the agent works in.
    #[arg(long, value_name = "PATH")]
    pub worktree: PathBuf,
    /// Branch the agent commits on.
    #[arg(long, value_name = "BRANCH")]
    pub branch: String,
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
