//! `shprd agent …`: planning, spawning, reporting, gating and integrating.

use anyhow::Context;
use shprd_core::event::{EventKind, Finding};

use crate::cli::{
    AgentCommand, EscalateArgs, GateArgs, GateOutcome, IntegrateArgs, OverrideArgs, ReportArgs,
    SpawnArgs, TaskAddArgs, TaskCommand,
};
use crate::cmd::emit;
use crate::ctx::{CliError, Ctx};

/// Dispatches one of the agent verbs.
pub fn dispatch(ctx: &Ctx, cmd: AgentCommand) -> Result<(), CliError> {
    match cmd {
        AgentCommand::Task { cmd } => match cmd {
            TaskCommand::Add(args) => task_add(ctx, &args),
        },
        AgentCommand::Spawn(args) => spawn(ctx, &args),
        AgentCommand::Report(args) => report(ctx, &args),
        AgentCommand::Gate(args) => gate(ctx, &args),
        AgentCommand::Escalate(args) => escalate(ctx, &args),
        AgentCommand::Override(args) => override_rule(ctx, &args),
        AgentCommand::Integrate(args) => integrate(ctx, &args),
    }
}

/// Adds a task to the run plan.
fn task_add(ctx: &Ctx, args: &TaskAddArgs) -> Result<(), CliError> {
    let kind = EventKind::TaskAdded {
        task: args.id.clone(),
        title: args.title.clone(),
        scope: args.scope.clone(),
        acceptance: args.acceptance.clone(),
        tags: args.tags.clone(),
        skills: args.skills.clone(),
        deps: args.deps.clone(),
    };
    let event = ctx.commit(kind)?;
    emit(ctx, &event)
}

/// Records that an agent took a task attempt. No worktree is created here.
fn spawn(ctx: &Ctx, args: &SpawnArgs) -> Result<(), CliError> {
    let kind = EventKind::Spawned {
        task: args.task.clone(),
        attempt: args.attempt,
        agent: args.agent.clone(),
        model: args.model.clone(),
        worktree: args.worktree.to_string_lossy().to_string(),
        branch: args.branch.clone(),
    };
    let event = ctx.commit(kind)?;
    emit(ctx, &event)
}

/// Records an agent's report for an attempt.
fn report(ctx: &Ctx, args: &ReportArgs) -> Result<(), CliError> {
    let kind = EventKind::Reported {
        task: args.task.clone(),
        attempt: args.attempt,
        status: args.status.into(),
        summary: args.summary.clone(),
        tests: args.tests.clone(),
        changed_paths: args.changed.clone(),
        commit: args.commit.clone(),
        skills_used: args.skills_used.clone(),
    };
    let event = ctx.commit(kind)?;
    emit(ctx, &event)
}

/// Records a gate result, reading its findings from a JSON file when given.
fn gate(ctx: &Ctx, args: &GateArgs) -> Result<(), CliError> {
    let findings = match &args.findings {
        Some(path) => read_findings(path)?,
        None => Vec::new(),
    };
    let kind = match args.outcome {
        GateOutcome::Pass => EventKind::GatePassed {
            task: args.task.clone(),
            attempt: args.attempt,
            gate: args.gate.clone(),
            findings,
        },
        GateOutcome::Fail => EventKind::GateFailed {
            task: args.task.clone(),
            attempt: args.attempt,
            gate: args.gate.clone(),
            findings,
        },
    };
    let event = ctx.commit(kind)?;
    emit(ctx, &event)
}

/// Reads a JSON array of findings.
fn read_findings(path: &std::path::Path) -> Result<Vec<Finding>, CliError> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading findings from {}", path.display()))
        .map_err(CliError::other)?;
    serde_json::from_str(&text)
        .with_context(|| format!("parsing findings in {}", path.display()))
        .map_err(CliError::other)
}

/// Records an escalation to a human.
fn escalate(ctx: &Ctx, args: &EscalateArgs) -> Result<(), CliError> {
    let task = args.task.clone().filter(|task| !task.trim().is_empty());
    let kind = EventKind::Escalated {
        task,
        body: args.body.clone(),
        default_action: args.default_action.clone(),
    };
    let event = ctx.commit(kind)?;
    emit(ctx, &event)
}

/// Records a human overriding a rule for one task, or run-wide.
fn override_rule(ctx: &Ctx, args: &OverrideArgs) -> Result<(), CliError> {
    let kind = EventKind::Override {
        task: args.task.clone().unwrap_or_default(),
        rule: args.rule.clone(),
        reason: args.reason.clone(),
    };
    let event = ctx.commit(kind)?;
    emit(ctx, &event)
}

/// Records that a task's commits landed on the integration branch.
///
/// In M1 nothing is merged; this only records what the caller reports.
fn integrate(ctx: &Ctx, args: &IntegrateArgs) -> Result<(), CliError> {
    let kind = EventKind::Integrated {
        task: args.task.clone(),
        commits: args.commits.clone(),
    };
    let event = ctx.commit(kind)?;
    emit(ctx, &event)
}
