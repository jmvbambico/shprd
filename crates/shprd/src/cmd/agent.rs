//! `shprd agent …`: planning, spawning, reporting, gating and integrating.

use std::path::Path;

use anyhow::{Context, anyhow};
use shprd_core::event::{EventKind, Finding};

use crate::cli::{
    AgentCommand, EscalateArgs, GateArgs, GateOutcome, HandoffArgs, IntegrateArgs, OverrideArgs,
    PromptArgs, ReportArgs, SpawnArgs, TaskAddArgs, TaskCommand,
};
use crate::cmd::{emit, handoff, spawn};
use crate::ctx::{CliError, Ctx};

/// Dispatches one of the agent verbs.
///
/// `home` is the global `--home`, threaded through because spawning writes brief
/// files and worktrees under it.
pub fn dispatch(
    ctx: &Ctx,
    cmd: AgentCommand,
    home: Option<&std::path::Path>,
) -> Result<(), CliError> {
    match cmd {
        AgentCommand::Task { cmd } => match cmd {
            TaskCommand::Add(args) => task_add(ctx, &args),
        },
        AgentCommand::Spawn(args) => spawn_cmd(ctx, &args, home),
        AgentCommand::Prompt(args) => prompt_cmd(ctx, &args),
        AgentCommand::Report(args) => report(ctx, &args),
        AgentCommand::Gate(args) => gate(ctx, &args),
        AgentCommand::ReviewVerdict(args) => crate::cmd::verdict::verdict_verb(ctx, &args, home),
        AgentCommand::Escalate(args) => escalate(ctx, &args),
        AgentCommand::Override(args) => override_rule(ctx, &args),
        AgentCommand::Integrate(args) => integrate(ctx, &args),
        AgentCommand::Handoff(args) => handoff_cmd(ctx, &args, home),
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

/// Spawns an agent for a task attempt, or records one a human set up by hand.
fn spawn_cmd(ctx: &Ctx, args: &SpawnArgs, home: Option<&Path>) -> Result<(), CliError> {
    if args.record_only {
        return record_only(ctx, args);
    }
    let mux = spawn::real_mux();
    let env = spawn::build_env(ctx, home)?;
    let request = spawn::SpawnRequest {
        task: args.task.clone(),
        agent: args.agent.clone(),
        effort: args.effort.clone(),
    };
    let report = spawn::spawn(&mux, &env, &request)?;
    print_report(ctx, &report);
    sync_dagr(ctx);
    Ok(())
}

/// Hands a task from one agent to the next.
fn handoff_cmd(ctx: &Ctx, args: &HandoffArgs, home: Option<&Path>) -> Result<(), CliError> {
    handoff::handoff_verb(ctx, args, home)
}

/// Records an attempt whose worktree and tab a human set up, with no side
/// effects of its own.
fn record_only(ctx: &Ctx, args: &SpawnArgs) -> Result<(), CliError> {
    let (Some(attempt), Some(worktree), Some(branch)) =
        (args.attempt, args.worktree.as_ref(), args.branch.as_ref())
    else {
        return Err(CliError::other(anyhow!(
            "--record-only needs --attempt, --worktree and --branch"
        )));
    };
    let agent = args.agent.clone().ok_or_else(|| {
        CliError::other(anyhow!("--record-only needs --agent (<program>/<model>)"))
    })?;
    let (agent, model) = match agent.split_once('/') {
        Some((program, model)) if !model.is_empty() => {
            (program.to_string(), Some(model.to_string()))
        }
        _ => (agent, None),
    };
    let kind = EventKind::Spawned {
        task: args.task.clone(),
        attempt,
        agent,
        model,
        worktree: worktree.to_string_lossy().to_string(),
        branch: branch.clone(),
    };
    let event = ctx.commit(kind)?;
    emit(ctx, &event)
}

/// Prints what a spawn did: JSON when asked, else one short human line.
fn print_report(ctx: &Ctx, report: &spawn::SpawnReport) {
    if ctx.json {
        let value = serde_json::json!({
            "task": report.task,
            "attempt": report.attempt,
            "agent": report.agent,
            "model": report.model,
            "worktree": report.worktree.display().to_string(),
            "branch": report.branch,
            "tab": report.tab,
            "brief": report.brief.display().to_string(),
            "skills": report.skills,
        });
        println!("{value}");
        return;
    }
    println!(
        "ok spawned {} attempt {} with {} in {} (tab {:?})",
        report.task,
        report.attempt,
        report.agent,
        report.worktree.display(),
        report.tab
    );
}

/// Rewrites the dagr document, warning on stderr when that fails.
fn sync_dagr(ctx: &Ctx) {
    if ctx.no_dagr {
        return;
    }
    if let Err(err) = crate::cmd::dagr::sync(ctx) {
        eprintln!("warning: dagr sync failed: {err}");
    }
}

/// Types one turn of input into the tab a spawned agent is working in.
fn prompt_cmd(ctx: &Ctx, args: &PromptArgs) -> Result<(), CliError> {
    let mux = spawn::real_mux();
    spawn::prompt(&mux, &ctx.root, &args.task, &args.text)?;
    if ctx.json {
        let value = serde_json::json!({ "task": args.task, "prompted": true });
        println!("{value}");
    } else {
        println!("ok prompted {} in its tab", args.task);
    }
    Ok(())
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
