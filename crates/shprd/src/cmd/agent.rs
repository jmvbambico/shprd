//! `shprd agent …`: planning, spawning, reporting, gating and integrating.

use std::path::Path;

use anyhow::{Context, anyhow};
use shprd_core::event::{CheckReport, EventKind, Finding, output_tail, status_line};

use crate::cli::{
    AgentCommand, EscalateArgs, GateArgs, GateOutcome, HandoffArgs, OverrideArgs, PromptArgs,
    ReportArgs, SpawnArgs, TaskAddArgs, TaskCommand, TaskStatusArgs,
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
            TaskCommand::Report(args) => report(ctx, &args),
        },
        AgentCommand::Spawn(args) => spawn_cmd(ctx, &args, home),
        AgentCommand::Prompt(args) => prompt_cmd(ctx, &args),
        AgentCommand::Report(args) => report(ctx, &args),
        AgentCommand::Gate(args) => gate(ctx, &args),
        AgentCommand::ReviewVerdict(args) => crate::cmd::verdict::verdict_verb(ctx, &args, home),
        AgentCommand::Escalate(args) => escalate(ctx, &args),
        AgentCommand::Override(args) => override_rule(ctx, &args),
        AgentCommand::Integrate(args) => {
            crate::cmd::integrate::verb(&spawn::real_mux(), ctx, &args, home)
        }
        AgentCommand::Handoff(args) => handoff_cmd(ctx, &args, home),
        AgentCommand::Review(args) => crate::cmd::review::review_verb(ctx, &args, home),
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
///
/// A retry first names the ref the attempt it replaced was archived under, so
/// the human can see the old work is still reachable — and, in JSON mode, that
/// line goes to stderr because stdout is the document.
fn print_report(ctx: &Ctx, report: &spawn::SpawnReport) {
    if ctx.json {
        let value = serde_json::json!({
            "archived": report.replaced.as_ref().map(|earlier| earlier.ref_name.clone()),
            "replaced_attempt": report.replaced.as_ref().map(|earlier| earlier.attempt),
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
    if let Some(replaced) = &report.replaced {
        let line = replaced.line(&report.task);
        println!("{line}");
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
///
/// Shared with `shprd serve`: a write made through the API is a write the same
/// way a CLI write is, so it leaves the same document behind.
pub(crate) fn sync_dagr(ctx: &Ctx) {
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

/// Starts an agent on a task, for the service's tick.
///
/// It is [`spawn_cmd`] without the printing: the tick runs the same spawn the
/// verb does — the same worktree, tab, brief and ledger events — and answers
/// with one line about what happened instead of writing to stdout, because a
/// service has no terminal to write to.
///
/// `SHPRD_TICK_RECORD_ONLY` makes it record the attempt without a worktree, a
/// tab or a CLI. Only a test sets it: the integration tests have no multiplexer
/// and no installed agent, and what they assert is that the tick started the
/// task, not that a terminal appeared.
pub(crate) fn start_for_tick(ctx: &Ctx, home: &Path, task: &str) -> Result<String, String> {
    if record_only_for_tick() {
        return record_attempt_only(ctx, home, task);
    }
    let mux = spawn::real_mux();
    let env = spawn::build_env(ctx, Some(home)).map_err(|err| err.to_string())?;
    let request = spawn::SpawnRequest {
        task: task.to_string(),
        agent: None,
        effort: None,
    };
    let report = spawn::spawn(&mux, &env, &request).map_err(|err| err.to_string())?;
    Ok(format!(
        "started {} attempt {} with {} in {} (tab {:?})",
        report.task,
        report.attempt,
        report.agent,
        report.worktree.display(),
        report.tab
    ))
}

/// Environment variable that makes a tick's start record the attempt only.
pub(crate) const RECORD_ONLY_ENV: &str = "SHPRD_TICK_RECORD_ONLY";

/// True when the environment asks for a record-only start, in a debug build.
fn record_only_for_tick() -> bool {
    cfg!(debug_assertions) && std::env::var_os(RECORD_ONLY_ENV).is_some_and(|value| value == "1")
}

/// Records the attempt a start would have made, and starts nothing.
///
/// The event is the one a spawn appends, so a task the tick started this way
/// reads as Working and everything downstream — the derived status, the next
/// pass, the landing — behaves exactly as it would with a live agent.
fn record_attempt_only(ctx: &Ctx, home: &Path, task: &str) -> Result<String, String> {
    let state = ctx.state().map_err(|err| err.to_string())?;
    let found = state
        .tasks
        .get(task)
        .ok_or_else(|| format!("no task {task:?} in this run"))?;
    let attempt = found.attempts.len() as u32 + 1;
    let run = state
        .run
        .as_ref()
        .ok_or_else(|| String::from("no run yet"))?;
    // The worktree is real, cut from the run's integration branch exactly as a
    // spawn cuts it: a test has to commit in it to make a landing possible, and
    // a landing reads the branch's head there.
    let layout = shprd_git::worktree::Layout::default_for_home(home);
    let created = shprd_git::worktree::create(
        &ctx.root,
        &layout,
        &run.run_id,
        task,
        &run.integration_branch,
    )
    .map_err(|err| err.to_string())?;
    ctx.commit(EventKind::Spawned {
        task: task.to_string(),
        attempt,
        agent: String::from("recorded"),
        model: None,
        worktree: created.path.display().to_string(),
        branch: created.branch.clone(),
    })
    .map_err(|err| err.to_string())?;
    Ok(format!(
        "recorded {task} attempt {attempt} on {} in {}",
        created.branch,
        created.path.display()
    ))
}

/// Records a short progress line for a task.
///
/// This is the verb a worker calls to say where it is. It appends under the
/// log's lock like every other write, so several agents can record one at once,
/// and it stores a capped one-line message rather than whatever was passed: a
/// pasted agent message must not become an event.
pub fn task_status(ctx: &Ctx, args: &TaskStatusArgs) -> Result<(), CliError> {
    let kind = EventKind::TaskStatus {
        task: args.task.clone(),
        message: status_line(&args.msg),
        progress: args.progress,
    };
    let event = ctx.commit(kind)?;
    emit(ctx, &event)
}

/// Records an agent's report for an attempt.
fn report(ctx: &Ctx, args: &ReportArgs) -> Result<(), CliError> {
    let checks = args
        .checks
        .iter()
        .map(|spec| parse_check(spec))
        .collect::<Result<Vec<CheckReport>, CliError>>()?;
    let kind = EventKind::Reported {
        task: args.task.clone(),
        attempt: args.attempt,
        status: args.status.into(),
        summary: args.summary.clone(),
        tests: args.tests.clone(),
        checks,
        changed_paths: args.changed.clone(),
        commit: args.commit.clone(),
        skills_used: args.skills_used.clone(),
    };
    let event = ctx.commit(kind)?;
    emit(ctx, &event)
}

/// Parses one `--check` value.
///
/// A value starting with `{` is a JSON [`CheckReport`], which is how a command
/// containing `|` is written. Everything else is `GATE|COMMAND|EXIT[|OUTPUT]`:
/// the four fields are split on the first three `|`s, so the output tail may
/// contain them. `EXIT` is the exit code, or `none` when a timeout killed the
/// command. Either way the output is capped to the last lines before it reaches
/// the log.
fn parse_check(spec: &str) -> Result<CheckReport, CliError> {
    if spec.trim_start().starts_with('{') {
        let mut check: CheckReport = serde_json::from_str(spec)
            .map_err(|err| CliError::other(anyhow!("--check is not a check object: {err}")))?;
        check.output_tail = output_tail(&check.output_tail);
        return Ok(check);
    }
    let parts: Vec<&str> = spec.splitn(4, '|').collect();
    let (Some(gate), Some(command), Some(exit)) = (parts.first(), parts.get(1), parts.get(2))
    else {
        return Err(bad_check(spec));
    };
    let gate = gate.trim();
    let command = command.trim();
    if gate.is_empty() || command.is_empty() {
        return Err(bad_check(spec));
    }
    let exit_code = match exit.trim() {
        "none" | "timeout" => None,
        raw => Some(raw.parse::<i32>().map_err(|_| bad_check(spec))?),
    };
    let output_tail = parts
        .get(3)
        .map(|text| output_tail(text))
        .unwrap_or_default();
    Ok(CheckReport {
        gate: gate.to_string(),
        command: command.to_string(),
        exit_code,
        output_tail,
    })
}

/// The error a malformed `--check` value gets.
fn bad_check(spec: &str) -> CliError {
    CliError::other(anyhow!(
        "--check needs GATE|COMMAND|EXIT[|OUTPUT] (exit is a number, or `none` for a timeout), got {spec:?}"
    ))
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
            source: None,
        },
        GateOutcome::Fail => EventKind::GateFailed {
            task: args.task.clone(),
            attempt: args.attempt,
            gate: args.gate.clone(),
            findings,
            source: None,
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
///
/// The hidden `agent override` alias of `shprd override`: it is the same verb,
/// so it asks the same question. The rule refuses it from anything but a
/// human either way.
fn override_rule(ctx: &Ctx, args: &OverrideArgs) -> Result<(), CliError> {
    crate::cmd::override_rule::override_verb(ctx, args)
}

#[cfg(test)]
mod tests {
    use super::parse_check;

    #[test]
    fn a_check_spec_carries_its_gate_command_and_exit_code() {
        let check = match parse_check("format|cargo fmt --all --check|0") {
            Ok(check) => check,
            Err(err) => panic!("a plain check must parse: {err}"),
        };
        assert_eq!(check.gate, "format");
        assert_eq!(check.command, "cargo fmt --all --check");
        assert_eq!(check.exit_code, Some(0));
        assert_eq!(check.output_tail, "");
    }

    #[test]
    fn a_check_spec_keeps_the_output_tail_capped() {
        let mut output = String::new();
        for line in 1..=30 {
            output.push_str(&format!("line {line}\n"));
        }
        let spec = format!("tests|cargo test|101|{output}");
        let check = match parse_check(&spec) {
            Ok(check) => check,
            Err(err) => panic!("a check with output must parse: {err}"),
        };
        assert_eq!(check.exit_code, Some(101));
        assert_eq!(check.output_tail.lines().count(), 20);
        assert!(check.output_tail.ends_with("line 30"));
    }

    #[test]
    fn a_timed_out_check_has_no_exit_code() {
        let check = match parse_check("tests|cargo test|none|timed out") {
            Ok(check) => check,
            Err(err) => panic!("a timeout must parse: {err}"),
        };
        assert_eq!(check.exit_code, None);
        assert_eq!(check.output_tail, "timed out");
    }

    #[test]
    fn a_check_may_be_a_json_object_for_a_command_with_a_pipe() {
        let spec = r#"{"gate":"tests","command":"cargo test --workspace | tail -20","exit_code":0,"output_tail":"ok"}"#;
        let check = match parse_check(spec) {
            Ok(check) => check,
            Err(err) => panic!("a JSON check must parse: {err}"),
        };
        assert_eq!(check.gate, "tests");
        assert_eq!(check.command, "cargo test --workspace | tail -20");
        assert_eq!(check.exit_code, Some(0));
        assert_eq!(check.output_tail, "ok");
        assert!(
            parse_check(r#"{"gate":"tests","command":"cargo test","exit_code":"0"}"#).is_err(),
            "a malformed object is refused"
        );
    }

    #[test]
    fn a_malformed_check_spec_is_an_error() {
        for spec in [
            "",
            "format",
            "format|cargo fmt",
            "|cargo fmt|0",
            "format|  |0",
        ] {
            assert!(parse_check(spec).is_err(), "{spec:?} must be refused");
        }
    }
}
