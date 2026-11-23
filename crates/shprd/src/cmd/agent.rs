//! `shprd agent …`: planning, spawning, reporting, gating and integrating.

use std::path::Path;

use anyhow::{Context, anyhow};
use shprd_core::event::{CheckReport, EventKind, Finding, output_tail, status_line};

use crate::cli::{
    AgentCommand, EscalateArgs, GateArgs, GateOutcome, HandoffArgs, OverrideArgs, PromptArgs,
    ReportArgs, SpawnArgs, TaskAddArgs, TaskCommand, TaskStatusArgs, TaskWidenArgs,
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
            TaskCommand::Widen(args) => task_widen(ctx, &args),
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
        AgentCommand::Propose(args) => crate::cmd::propose::propose_verb(ctx, &args),
    }
}

/// Adds a task to the run plan.
fn task_add(ctx: &Ctx, args: &TaskAddArgs) -> Result<(), CliError> {
    // The first task in a repository with no run starts one with the defaults, as
    // the service's own first task does: the coordinator adds tasks with this
    // verb, and nobody has typed `shprd run start`.
    if ctx.state()?.run.is_none() {
        for event in
            shprd_server::default_run(&ctx.root).map_err(|why| CliError::Invalid(anyhow!(why)))?
        {
            ctx.commit(event)?;
        }
    }
    for line in scope_warnings(&ctx.root, &args.scope) {
        eprintln!("{line}");
    }
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

/// One warning per scope entry that matches nothing in the repository, with
/// the nearest real path when there is one.
fn scope_warnings(root: &Path, scope: &[String]) -> Vec<String> {
    scope
        .iter()
        .filter_map(|raw| {
            let entry = shprd_core::scope::normalize(raw).ok()?;
            let near = shprd_core::scope::nearest_existing(root, &entry)?;
            Some(if near.is_empty() || near == entry {
                format!("warning: scope path {entry:?} matches nothing in the repository")
            } else {
                format!(
                    "warning: scope path {entry:?} matches nothing in the repository; \
                     did you mean {near:?}?"
                )
            })
        })
        .collect()
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
            "warnings": report.warnings,
        });
        println!("{value}");
        return;
    }
    for warning in &report.warnings {
        eprintln!("warning: {warning}");
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
    deliver_worker_notes(ctx);
    if ctx.no_dagr {
        return;
    }
    if let Err(err) = crate::cmd::dagr::sync(ctx) {
        eprintln!("warning: dagr sync failed: {err}");
    }
}

/// Raises the card that proposes a wider scope for a task.
///
/// It writes no scope: the card's `widen` answer, given by the person, does.
fn task_widen(ctx: &Ctx, args: &TaskWidenArgs) -> Result<(), CliError> {
    let state = ctx.state()?;
    if !state.tasks.contains_key(&args.task) {
        return Err(CliError::Invalid(anyhow!(
            "no task {:?} in this run: name one `shprd status` lists",
            args.task
        )));
    }
    let mut paths = Vec::new();
    for path in &args.scope {
        let path = shprd_core::scope::normalize(path)
            .map_err(|err| CliError::Invalid(anyhow!("scope path {path:?}: {err}")))?;
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    if args.reason.trim().is_empty() {
        return Err(CliError::Invalid(anyhow!(
            "`--reason` is empty: tell the person why the task needs the paths"
        )));
    }
    // An open scope card that already offers these paths is the one question:
    // say which, and raise nothing.
    let task = &state.tasks[&args.task];
    if let Some(open) = state.asks.values().find(|ask| {
        ask.task == args.task
            && ask.answer.is_none()
            && shprd_core::ask::scope_key_of(&ask.task, &ask.id).is_some()
            && {
                let offered = shprd_core::ask::offered_paths(&ask.options);
                paths.iter().all(|path| offered.contains(path))
            }
    }) {
        println!(
            "ask {} already covers {} ({}): nothing new was raised",
            open.id,
            paths.join(", "),
            shprd_core::ask::scope_key(&args.task)
        );
        return Ok(());
    }
    if paths
        .iter()
        .all(|path| shprd_core::scope::within(path, &task.scope))
    {
        println!(
            "{} is already in {}'s scope: nothing was raised",
            paths.join(", "),
            args.task
        );
        return Ok(());
    }
    let earlier = state
        .asks
        .values()
        .filter(|ask| {
            ask.task == args.task && ask.id.starts_with(shprd_core::ask::SCOPE_WIDEN_ASK_PREFIX)
        })
        .count();
    let id = shprd_core::ask::scope_widen_ask(earlier + 1);
    let text = shprd_core::ask::scope_widen_body(&args.task, &paths, &args.reason);
    let options = shprd_core::ask::scope_widen_options(&paths);
    let now_ms = shprd_core::log::Clock::now_ms(&shprd_core::log::SystemClock);
    let ask = shprd_core::ask::Ask::new(
        id.clone(),
        args.task.clone(),
        shprd_core::ask::AskKind::Question,
        text.clone(),
        now_ms,
    )
    .with_options(options.clone());
    let queue = shprd_core::ask::AskQueue::for_repo(&ctx.root);
    queue
        .raise(&ask)
        .map_err(|err| CliError::other(anyhow!(err).context("writing the ask")))?;
    let raised = EventKind::AskRaised {
        task: args.task.clone(),
        ask: id.clone(),
        ask_kind: shprd_core::ask::AskKind::Question,
        text,
        options,
        created_at_ms: now_ms,
        expires_at_ms: None,
    };
    let event = match ctx.commit(raised) {
        Ok(event) => event,
        Err(err) => {
            let _ = queue.remove(&args.task, &id);
            return Err(err);
        }
    };
    emit(ctx, &event)
}

/// How many sends a note gets before the person is told it never arrived.
const NOTE_SEND_CAP: u32 = 5;

/// What one try at typing a note into its task's worker came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NoteSend {
    /// The worker's terminal took the note.
    Sent,
    /// The task has no live worker: its next brief lists the paths itself.
    NoWorker,
    /// A live worker's terminal refused the write; worth trying again.
    Failed,
}

/// Types every queued worker note into its task's live worker.
///
/// A note is skipped only when the task has no live worker: the next attempt's
/// brief is built from the widened scope and lists the paths itself. A failed
/// write to a live worker leaves the note pending for the next pass, up to
/// [`NOTE_SEND_CAP`] tries, after which the person is told in plain words. A
/// note is settled as sent only when it was delivered, so nothing is typed twice.
pub(crate) fn deliver_worker_notes(ctx: &Ctx) {
    let notes = shprd_core::ask::WorkerNotes::for_repo(&ctx.root);
    let pending = notes.pending();
    if pending.is_empty() {
        return;
    }
    let mux = spawn::real_mux();
    let home = crate::ctx::resolve_home(None).ok();
    let reader = Ctx::new(
        ctx.root.clone(),
        shprd_core::event::Actor::Human,
        false,
        true,
    );
    let told = deliver_notes_with(&notes, &pending, |note| {
        let Some(home) = &home else {
            return NoteSend::NoWorker;
        };
        let Ok(pane) = spawn::task_pane(&*mux, home, &reader, &note.task) else {
            return NoteSend::NoWorker;
        };
        match mux.write(&pane, &note.text, true) {
            Ok(()) => NoteSend::Sent,
            Err(_) => NoteSend::Failed,
        }
    });
    for task in told {
        let status = EventKind::TaskStatus {
            task,
            message: status_line("could not tell the worker its scope was widened"),
            progress: None,
        };
        let _ = ctx.commit(status);
    }
}

/// Runs one delivery pass over `pending` with `send`, settling what is final.
///
/// Returns the tasks whose note ran out of tries, for the caller to tell the
/// person about. The try count lives beside the note, so it survives passes.
pub(crate) fn deliver_notes_with(
    notes: &shprd_core::ask::WorkerNotes,
    pending: &[shprd_core::ask::WorkerNote],
    mut send: impl FnMut(&shprd_core::ask::WorkerNote) -> NoteSend,
) -> Vec<String> {
    let mut gave_up = Vec::new();
    for note in pending {
        let tries_file = note.path.with_extension("tries");
        match send(note) {
            NoteSend::Sent => {
                notes.settle(note, true);
                let _ = std::fs::remove_file(&tries_file);
            }
            NoteSend::NoWorker => {
                notes.settle(note, false);
                let _ = std::fs::remove_file(&tries_file);
            }
            NoteSend::Failed => {
                let tries = std::fs::read_to_string(&tries_file)
                    .ok()
                    .and_then(|text| text.trim().parse::<u32>().ok())
                    .unwrap_or(0)
                    + 1;
                if tries >= NOTE_SEND_CAP {
                    notes.settle(note, false);
                    let _ = std::fs::remove_file(&tries_file);
                    gave_up.push(note.task.clone());
                } else {
                    let _ = std::fs::write(&tries_file, tries.to_string());
                }
            }
        }
    }
    gave_up
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
    let warnings = match report.warnings.is_empty() {
        true => String::new(),
        false => format!(" ({})", report.warnings.join("; ")),
    };
    Ok(format!(
        "started {} attempt {} with {} in {} (tab {:?}){warnings}",
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
    let layout = shprd_git::worktree::Layout::from_settings(home, &ctx.root);
    // A retry takes the same path and the same branch as the attempt it
    // replaces, so the failed one is archived and removed first, exactly as a
    // real spawn does it (`spawn::replace_previous`). This is what lets a test
    // of a send-back record the second attempt at all.
    if let Some(previous) = found.latest_attempt() {
        let path = layout.path(
            &ctx.root
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            &run.run_id,
            task,
        );
        let archive = shprd_git::archive::archive_ref_for_attempt(&run.run_id, task, previous.n);
        if shprd_git::git::Git::new(&ctx.root)
            .branch_exists(&previous.branch)
            .unwrap_or(false)
        {
            let _ = shprd_git::archive::archive_to(&ctx.root, &previous.branch, &archive);
        }
        let _ = shprd_git::worktree::remove_worktree_only(&ctx.root, &layout, &path, true);
        let _ = shprd_git::worktree::delete_branch(&ctx.root, &layout, &previous.branch);
    }
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
///
/// A proof is an artifact a person and the reviewer can look at, and the
/// reviewer reads the packet from outside the worktree: a path outside the
/// attempt's own worktree is refused, before anything is written, so a proof is
/// always something the reviewer can open.
fn report(ctx: &Ctx, args: &ReportArgs) -> Result<(), CliError> {
    let checks = args
        .checks
        .iter()
        .map(|spec| parse_check(spec))
        .collect::<Result<Vec<CheckReport>, CliError>>()?;
    let proofs = proof_paths(ctx, args)?;
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
        proofs,
    };
    let event = match ctx.commit(kind) {
        Ok(event) => event,
        Err(CliError::Refused(refused)) => {
            // A refused hand-in records nothing, so the board would read
            // `Working` for ever with the worker idle. A card says why instead.
            raise_refused_hand_in(ctx, args, &refused);
            // The refusal names the event it was judged against and when, so a
            // worker can tell a stale refusal from a new one (a later
            // `scope_widened` means: run the report again).
            let seq = ctx.state().map(|state| state.last_seq).unwrap_or_default();
            let now = shprd_core::log::Clock::now_ms(&shprd_core::log::SystemClock);
            eprintln!("{}", refusal_note(seq, now));
            return Err(CliError::Refused(refused));
        }
        Err(other) => return Err(other),
    };
    emit(ctx, &event)
}

/// The line printed with a refusal: the event seq and time it was judged at.
fn refusal_note(seq: u64, now_ms: u64) -> String {
    let secs = now_ms / 1000;
    let (h, m, sec) = (secs / 3600 % 24, secs / 60 % 60, secs % 60);
    format!(
        "[refused at event seq {seq}, {h:02}:{m:02}:{sec:02} UTC: a scope_widened event with a \
         higher seq means this refusal is stale, so run the report again]"
    )
}

/// Puts a card in front of the person for a hand-in the engine refused.
///
/// Only a path outside the scope gets one, because only a person can settle it
/// (by widening the scope); a hand-in without passing checks is the worker's to
/// fix and records nothing. It is best effort: the worker still gets the refusal as its exit
/// status, and a card that could not be written must not hide it.
fn raise_refused_hand_in(ctx: &Ctx, args: &ReportArgs, refused: &shprd_core::rules::RuleError) {
    use shprd_core::rules::RuleError;
    if !matches!(refused, RuleError::PathOutOfScope { .. }) {
        return;
    }
    let Ok(state) = ctx.state() else { return };
    let Some(task) = state.tasks.get(&args.task) else {
        return;
    };
    let prefix = format!(
        "{}{}-",
        shprd_core::gate::HAND_IN_REFUSED_ASK_PREFIX,
        args.attempt
    );
    let earlier = state
        .asks
        .values()
        .filter(|ask| ask.task == args.task && ask.id.starts_with(&prefix))
        .count();
    // One open card per attempt: a worker that retries the same hand-in does
    // not stack them.
    if state
        .asks
        .values()
        .any(|ask| ask.task == args.task && ask.id.starts_with(&prefix) && ask.answer.is_none())
    {
        return;
    }
    let outside: Vec<String> = args
        .changed
        .iter()
        .filter_map(|path| shprd_core::scope::normalize(path).ok())
        .filter(|path| !shprd_core::scope::within(path, &task.scope))
        .collect();
    // The coordinator's widen card may already ask for these paths: one scope
    // question, one card.
    if state.asks.values().any(|ask| {
        ask.task == args.task && ask.answer.is_none() && {
            let offered = shprd_core::ask::offered_paths(&ask.options);
            !outside.is_empty()
                && shprd_core::ask::scope_key_of(&ask.task, &ask.id).is_some()
                && outside.iter().all(|path| offered.contains(path))
        }
    }) {
        return;
    }
    let id = shprd_core::gate::hand_in_refused_ask(args.attempt, earlier + 1);
    let text =
        shprd_core::gate::hand_in_refused_body(&args.task, args.attempt, &refused.to_string());
    let options = shprd_core::gate::hand_in_refused_options(&outside);
    let now_ms = shprd_core::log::Clock::now_ms(&shprd_core::log::SystemClock);
    let ask = shprd_core::ask::Ask::new(
        id.clone(),
        args.task.clone(),
        shprd_core::ask::AskKind::Question,
        text.clone(),
        now_ms,
    )
    .with_options(options.clone());
    let queue = shprd_core::ask::AskQueue::for_repo(&ctx.root);
    if queue.raise(&ask).is_err() {
        return;
    }
    let raised = ctx.commit(EventKind::AskRaised {
        task: args.task.clone(),
        ask: id.clone(),
        ask_kind: shprd_core::ask::AskKind::Question,
        text,
        options,
        created_at_ms: now_ms,
        expires_at_ms: None,
    });
    if raised.is_err() {
        let _ = queue.remove(&args.task, &id);
        return;
    }
    let _ = ctx.commit(EventKind::TaskStatus {
        task: args.task.clone(),
        message: status_line("hand-in refused: waiting for a decision"),
        progress: None,
    });
}

/// The proofs a hand-in records, refused when one is outside the worktree.
///
/// The attempt's worktree is the root a worker's relative paths are taken from
/// and the one place the reviewer is told to look, so a proof has to be inside
/// it. A path outside it — `/tmp/…`, another checkout, a home folder — is not
/// one the reviewer can open, and recording it would hand the reviewer a proof
/// it cannot check.
fn proof_paths(ctx: &Ctx, args: &ReportArgs) -> Result<Vec<String>, CliError> {
    if args.proofs.is_empty() {
        return Ok(Vec::new());
    }
    let root = proof_root(ctx, args)?;
    for proof in &args.proofs {
        let path = Path::new(proof);
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            root.join(path)
        };
        if !inside(&root, &absolute) {
            return Err(CliError::Invalid(anyhow!(
                "refusing to record the proof {proof:?}: it is not inside the task's worktree {}\n\
                 leave the artifact in the worktree and pass its path from there (e.g. \
                 `--proof .shprd/proof/tests.txt`), because that is the folder the reviewer is given",
                root.display()
            )));
        }
        if is_tracked(&root, &absolute) {
            return Err(CliError::Invalid(anyhow!(
                "refusing to record the proof {proof:?}: it is a tracked file on the task's branch, \
                 and proof notes never live in the repository\n\
                 write the artifact under `.shprd/proof/` in the worktree (e.g. \
                 `--proof .shprd/proof/tests.txt`), which git ignores, and pass that path"
            )));
        }
    }
    Ok(args.proofs.clone())
}

/// Whether git tracks `path` in the worktree at `root`. A directory that is not
/// a git checkout tracks nothing; a git that cannot be run counts as tracked.
fn is_tracked(root: &Path, path: &Path) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "--error-unmatch", "--"])
        .arg(path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        // git itself could not run: fail closed rather than accept a tracked proof.
        .unwrap_or(true)
}

/// The worktree the hand-in's proofs are read from.
///
/// It is the attempt's own worktree as the log recorded it, which is the folder
/// the reviewer's packet names. A log that cannot say — or a report recorded
/// without a worktree, as the record-only paths do — falls back to the
/// repository root, so a plain directory still checks its proofs against the
/// root a worker runs in.
fn proof_root(ctx: &Ctx, args: &ReportArgs) -> Result<std::path::PathBuf, CliError> {
    let state = ctx.state()?;
    Ok(state
        .tasks
        .get(&args.task)
        .and_then(|task| task.attempts.iter().find(|one| one.n == args.attempt))
        .map(|attempt| std::path::PathBuf::from(&attempt.worktree))
        .filter(|worktree| !worktree.as_os_str().is_empty() && worktree.is_dir())
        .unwrap_or_else(|| ctx.root.clone()))
}

/// Whether `path` is `root` or inside it, once both are resolved.
///
/// Both sides are resolved through any symlink that exists, so a home reached
/// by one — macOS keeps `/var` as a link to `/private/var`, and a worktree can
/// be reached either way — cannot make a file look like it is outside the
/// worktree it is in. A proof file the worker has not written yet still
/// resolves: its deepest existing ancestor is, and the rest is appended.
fn inside(root: &Path, path: &Path) -> bool {
    let root = resolve(root);
    let path = resolve(path);
    path.starts_with(&root)
}

/// `path` with every symlink that leads to it resolved and the components that
/// do not exist yet appended as written.
fn resolve(path: &Path) -> std::path::PathBuf {
    let normal = lexical_normal(path);
    let mut missing: Vec<std::ffi::OsString> = Vec::new();
    let mut here = normal.clone();
    loop {
        if let Ok(resolved) = here.canonicalize() {
            let mut out = resolved;
            for part in missing.iter().rev() {
                out.push(part);
            }
            return out;
        }
        match (
            here.file_name().map(|name| name.to_os_string()),
            here.parent(),
        ) {
            (Some(name), Some(parent)) => {
                missing.push(name);
                here = parent.to_path_buf();
            }
            _ => return normal,
        }
    }
}

/// `path` with its `.` and `..` components resolved, for a file that is not
/// there yet.
fn lexical_normal(path: &Path) -> std::path::PathBuf {
    let mut out = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
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
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{NoteSend, deliver_notes_with, parse_check, refusal_note, scope_warnings};

    #[test]
    fn a_scope_that_matches_nothing_warns_with_the_nearest_path() {
        let dir = std::env::temp_dir().join(format!("shprd-scope-warn-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("client/src/components")).unwrap();
        std::fs::write(dir.join("client/src/components/icons.tsx"), "").unwrap();
        let scope = vec![
            "client/src/components/icons".to_string(),
            "client/src/components".to_string(),
        ];
        let warnings = scope_warnings(&dir, &scope);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("icons.tsx"), "{warnings:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_refusal_carries_its_seq_and_time() {
        let text = refusal_note(99, 3_723_000);
        assert!(
            text.contains("seq 99") && text.contains("01:02:03 UTC"),
            "{text}"
        );
    }

    #[test]
    fn a_failed_send_stays_pending_then_delivers_once() {
        let dir = tempfile::tempdir().expect("temp dir");
        let notes = shprd_core::ask::WorkerNotes::for_repo(dir.path());
        notes.queue("T1", "widened").expect("queue");
        let mut sends = 0;
        let mut script = vec![NoteSend::Failed, NoteSend::Sent].into_iter();
        for _ in 0..3 {
            let pending = notes.pending();
            deliver_notes_with(&notes, &pending, |_| {
                sends += 1;
                script.next().expect("no third send")
            });
        }
        assert_eq!(sends, 2, "one failure, one delivery, then nothing pending");
        assert!(notes.pending().is_empty());
        let settled: Vec<_> = std::fs::read_dir(dir.path().join(".shprd/worker-notes/T1"))
            .expect("dir")
            .flatten()
            .map(|e| {
                e.path()
                    .extension()
                    .and_then(|x| x.to_str())
                    .unwrap_or("")
                    .to_string()
            })
            .collect();
        assert_eq!(settled, ["sent"]);
    }

    #[test]
    fn no_live_worker_skips_the_note() {
        let dir = tempfile::tempdir().expect("temp dir");
        let notes = shprd_core::ask::WorkerNotes::for_repo(dir.path());
        notes.queue("T1", "widened").expect("queue");
        let pending = notes.pending();
        let told = deliver_notes_with(&notes, &pending, |_| NoteSend::NoWorker);
        assert!(told.is_empty());
        assert!(notes.pending().is_empty());
        let settled: Vec<_> = std::fs::read_dir(dir.path().join(".shprd/worker-notes/T1"))
            .expect("dir")
            .flatten()
            .map(|e| {
                e.path()
                    .extension()
                    .and_then(|x| x.to_str())
                    .unwrap_or("")
                    .to_string()
            })
            .collect();
        assert_eq!(settled, ["skipped"]);
    }

    #[test]
    fn a_note_that_keeps_failing_gives_up_and_names_the_task() {
        let dir = tempfile::tempdir().expect("temp dir");
        let notes = shprd_core::ask::WorkerNotes::for_repo(dir.path());
        notes.queue("T1", "widened").expect("queue");
        let mut told = Vec::new();
        for _ in 0..super::NOTE_SEND_CAP {
            let pending = notes.pending();
            told = deliver_notes_with(&notes, &pending, |_| NoteSend::Failed);
        }
        assert_eq!(told, ["T1"]);
        assert!(notes.pending().is_empty());
    }

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
