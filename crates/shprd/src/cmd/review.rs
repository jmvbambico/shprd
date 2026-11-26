//! `shprd agent review`: hand a finished task's diff to a cross-vendor reviewer.
//!
//! A reviewer never gets a worktree. It gets one folder holding everything it may
//! read: the diff against the branch's merge base with the integration branch, the
//! contract the attempt was written against, and the schema its findings must
//! match. [`review`] takes the [`Host`](shprd_mux::Host) as a parameter, the way
//! [`spawn`] does, so a test drives a fake host and nothing here reads the
//! environment or starts a process of its own.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::anyhow;
use serde_json::{Value, json};
use shprd_agents::env::{EnvContext, LaunchEnv, Role as EnvRole, launch_env};
use shprd_agents::families::{Families, agent_vendor};
use shprd_agents::models::AgentRef;
use shprd_agents::spec::{BlockingPrompt, HarnessSpec, Values, render_args};
use shprd_core::ask::AskId;
use shprd_core::event::{EventKind, ReportStatus, TaskId};
use shprd_core::gate::REVIEW_GATE;
use shprd_core::settings::{ResolvedRole, Source, Sourced, WorkerLine};
use shprd_core::state::{AskRecord, Attempt, Task, TaskStatus};
use shprd_git::git::Git;
use shprd_mux::Host;

use crate::cli::ReviewArgs;
use crate::cmd::deliver;
use crate::cmd::spawn::{self, Program, SpawnEnv};
use crate::ctx::{CliError, Ctx};

/// The label a review carries when it is not cross-vendor, in the brief and on
/// the pull request.
pub const DEGRADED_LABEL: &str = "degraded-review";

/// How long a review waits before offering a lost brief a second time.
///
/// A CLI that has just started can miss its first prompt while the host has not
/// detected it as an agent yet, so one retry after a few seconds is worth making.
/// A reviewer that is genuinely stuck still warns and escalates once, just later.
const BRIEF_RETRY_WAIT: Duration = Duration::from_secs(3);

/// Everything [`review`] needs from the caller's environment.
///
/// It is plain data plus the run log, so a test builds one without touching a
/// real home directory, a real config file or a real CLI.
pub struct ReviewEnv<'a> {
    /// The spawn environment: the log, the layout, the briefs dir, the run, the
    /// integration branch, the harness specs and the base environment.
    pub spawn: SpawnEnv<'a>,
    /// Where review packets are written: `<home>/.shprd/reviews`.
    pub reviews_dir: PathBuf,
    /// The `roles.reviewer` row the settings resolve: the agent a review runs
    /// on, or a role that names no program yet.
    pub reviewer: ReviewerRole,
    /// How long to wait before retrying a brief the reviewer did not take.
    ///
    /// It is data rather than a constant so a test can set it to
    /// [`Duration::ZERO`] and still exercise the retry: the wait is production's
    /// patience, not the retry rule.
    pub brief_retry_wait: Duration,
}

impl ReviewEnv<'_> {
    /// Builds the environment from the process environment, the settings and the
    /// run log.
    pub fn build<'a>(ctx: &'a Ctx, home: Option<&Path>) -> Result<ReviewEnv<'a>, CliError> {
        let home = crate::ctx::resolve_home(home)?;
        let spawn = spawn::build_env(ctx, Some(&home))?;
        // The reviewer is the `roles.reviewer` row, resolved the way every other
        // setting is: the person's settings file, then their old `config.toml`,
        // then this repository's committed file. A reviewer named the pre-M9 way
        // therefore still resolves, with the legacy layer as its source.
        let resolved = spawn.settings.resolve(None);
        let mut reviewer = resolved
            .role(REVIEWER_ROLE)
            .map(|role| ReviewerRole::resolved(&role))
            .unwrap_or_default();
        // The rule is read here, on every review, from the same settings layer
        // the validation reads, so turning it off takes effect on the next one.
        reviewer.same_vendor_ok = !resolved.cross_vendor();
        if reviewer.agent().is_none() {
            // A repository's own `.shprd/config.toml` predates the settings model
            // and is not a settings layer, so its `[roster] reviewer` is the last
            // fallback: a repository that named its reviewer the old way keeps it.
            let global = home.join(".config").join("shprd").join("config.toml");
            let repo_config = ctx.root.join(".shprd").join("config.toml");
            let config = shprd_core::config::load(Some(&global), Some(&repo_config))
                .map_err(|err| CliError::other(anyhow!(err).context("loading the shprd config")))?;
            if let Some(written) = config.roster.reviewer.as_deref() {
                let effort = config.roster.effort.get(written).map(String::as_str);
                let same_vendor_ok = reviewer.same_vendor_ok;
                reviewer = ReviewerRole::written(written, effort);
                reviewer.same_vendor_ok = same_vendor_ok;
            }
        }
        // A program the run recorded as out of quota is skipped for as long as
        // the CLI said it would be (or the bounded cooldown the switch chose).
        let now_ms = shprd_core::log::Clock::now_ms(&shprd_core::log::SystemClock);
        if let Ok(state) = spawn.ctx.state() {
            reviewer.dry = dry_programs(&state, now_ms);
        }
        Ok(ReviewEnv {
            spawn,
            reviews_dir: real_dir(&home).join(".shprd").join("reviews"),
            reviewer,
            brief_retry_wait: BRIEF_RETRY_WAIT,
        })
    }
}

/// The programs `state` has as dry at `now_ms`: dry, and not past the time
/// they were expected back.
pub fn dry_programs(state: &shprd_core::state::RunState, now_ms: u64) -> Vec<DryProgram> {
    state
        .agents
        .iter()
        .filter(|(_, info)| info.dry && info.dry_until_ms.is_none_or(|until| until > now_ms))
        .map(|(agent, info)| DryProgram {
            agent: agent.clone(),
            reason: info.dry_reason.clone().unwrap_or_default(),
            until_ms: info.dry_until_ms,
        })
        .collect()
}

/// `dir` with its symlinks resolved, or `dir` itself when it cannot be.
///
/// A packet's path is handed to a reviewer that reads it from inside a sandbox,
/// and Codex's refuses a path that goes through a symlink (macOS's `/tmp` and
/// `/var` are both links), so the folder is named by where it really is.
pub(crate) fn real_dir(dir: &Path) -> PathBuf {
    dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf())
}

/// What the caller asked for.
#[derive(Debug, Clone)]
pub struct ReviewRequest {
    /// The task whose latest attempt is reviewed.
    pub task: String,
    /// Review even when the reviewer shares the author's vendor.
    pub degraded: bool,
    /// Restart a review that is already requested and not yet decided.
    pub again: bool,
}

/// What a review did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewReport {
    /// The task that was reviewed.
    pub task: String,
    /// The attempt that was reviewed.
    pub attempt: u32,
    /// The program the reviewer runs.
    pub agent: String,
    /// The model it runs, when one was chosen.
    pub model: Option<String>,
    /// Whether the review is cross-vendor or degraded.
    pub degraded: bool,
    /// The folder the reviewer reads and writes in.
    pub dir: PathBuf,
    /// The brief file the reviewer was pointed at.
    pub brief: PathBuf,
    /// The title of the tab the reviewer runs in.
    pub tab: String,
}

/// Where a review packet lives: `<reviews>/<run>/<task>-a<attempt>/`.
pub fn packet_dir(reviews: &Path, run: &str, task: &str, attempt: u32) -> PathBuf {
    shprd_core::review::packet_dir(reviews, run, task, attempt)
}

/// The file a reviewer writes its findings to, inside its packet folder.
///
/// It is the one file read back out of a packet: the watchdog watches it, so a
/// finished review is picked up without the reviewer running any command.
pub fn findings_path(reviews: &Path, run: &str, task: &str, attempt: u32) -> PathBuf {
    shprd_core::review::findings_path(reviews, run, task, attempt)
}

/// Where a reviewer's brief lives: `<briefs>/<run>/<task>-a<attempt>-review.md`.
///
/// The `-review` suffix keeps it clear of the worker's own brief for the same
/// attempt, which sits beside it.
pub fn brief_path(briefs: &Path, run: &str, task: &str, attempt: u32) -> PathBuf {
    briefs
        .join(run)
        .join(format!("{task}-a{attempt}-review.md"))
}

/// Where an attempt records the tab its reviewer opened:
/// `<briefs>/<run>/<task>-a<attempt>-review.tab`.
pub fn tab_record_path(briefs: &Path, run: &str, task: &str, attempt: u32) -> PathBuf {
    briefs
        .join(run)
        .join(format!("{task}-a{attempt}{}", spawn::REVIEW_TAB_SUFFIX))
}

/// Closes the reviewer tab an attempt's review left open.
///
/// The record the review wrote is the only thing that names the tab, so a
/// missing record means there is nothing to close. A host that no longer knows
/// the tab counts as closed: that is the state the caller wanted anyway, the
/// way [`handoff`](super::handoff) treats a vanished old tab.
pub fn close_reviewer(
    mux: &dyn Host,
    briefs: &Path,
    run: &str,
    task: &str,
    attempt: u32,
) -> Result<(), CliError> {
    // A reviewer that never opened a tab is a review that never got that far:
    // no record is nothing to close, not a failure.
    spawn::close_recorded(
        mux,
        "reviewer's",
        &tab_record_path(briefs, run, task, attempt),
    )
}

/// What a `contract.md` says, as data, so the rendering is testable on its own.
pub struct ContractInput<'a> {
    /// The task that was attempted.
    pub task: &'a Task,
    /// Every ask the run folded, from which this attempt's own are picked.
    ///
    /// The fold keeps one record per ask, task-wide, so the attempt whose spawn
    /// came before a card and whose report came after is the attempt that
    /// raised it. An attempt built by hand, with no `spawn_seq`, gets every ask
    /// the task raised: there is nothing to tell them apart by, and showing too
    /// much is never what hides an acceptance criterion.
    pub asks: &'a [(TaskId, AskId, AskRecord)],
    /// The proof files copied into the packet, as `(path the worker named,
    /// path inside the packet)`. A proof the worker named that is not here
    /// could not be copied, and the contract says so.
    pub proof_copies: &'a [(String, Option<String>)],
    /// The protected paths the attempt's real diff touches that the task did
    /// not declare ([`shprd_git::changed::protected_touches`]). Flagged for the
    /// reviewer, never refused.
    pub protected: &'a [String],
}

/// Renders the contract a reviewer checks the diff against.
///
/// It carries what the diff cannot say: what the task was for, what "done"
/// meant, where it was expected to work, what it asked the person and
/// what they answered, and what the agent claimed — including where its
/// proof is. An acceptance criterion that says "ask first, then use the answer"
/// is unverifiable from a diff alone, so the asks belong in the packet rather
/// than only in the agent's summary.
pub fn render_contract(input: &ContractInput<'_>) -> String {
    let task = input.task;
    let mut out = String::new();
    out.push_str(&format!("# {} {}\n\n", task.id, task.title));
    let attempt = task.attempts.last();
    if let Some(attempt) = attempt {
        out.push_str(&format!(
            "attempt {}, agent `{}`{}, branch `{}`\n\n",
            attempt.n,
            attempt.agent,
            match attempt.model.as_deref() {
                Some(model) => format!(" ({model})"),
                None => String::new(),
            },
            attempt.branch
        ));
    }
    out.push_str("## Acceptance criteria\n\n");
    for item in &task.acceptance {
        out.push_str(&format!("- {item}\n"));
    }
    out.push_str("\n## Where the task was expected to work (a hint, not a limit)\n\n");
    for path in &task.scope {
        out.push_str(&format!("- {path}\n"));
    }
    out.push_str(
        "\nA change elsewhere is fine when the task needs it: judge whether each change serves \
         the task, and name one it did not need in a finding.\n",
    );
    if let Some(line) = shprd_core::gate::protected_line(input.protected) {
        out.push_str(&format!(
            "\n{line}. The task did not name them: say in a finding whether it needed them.\n"
        ));
    }
    out.push_str(&asks_section(&task.id, attempt, input.asks));
    out.push_str("\n## What the agent reported\n\n");
    let report = attempt.and_then(|attempt| attempt.report.as_ref());
    let Some(report) = report else {
        out.push_str("- no report\n");
        return out;
    };
    out.push_str(&format!("- summary: {}\n", report.summary));
    out.push_str(&listed("tests", &report.tests));
    out.push_str(&checks_section(&report.checks));
    out.push_str(&listed("changed paths", &report.changed_paths));
    out.push_str(&listed("proof", &report.proofs));
    out.push_str(&proof_section(input.proof_copies));
    if let Some(commit) = &report.commit {
        out.push_str(&format!("- commit: {commit}\n"));
    }
    out
}

/// The checks the hand-in named: gate, command, exit code and, when one was
/// given, the output tail in a fenced block.
///
/// The reviewer judges "gates not named" from this list, so every check of the
/// attempt's hand-in appears here, not only a count of them.
fn checks_section(checks: &[shprd_core::event::CheckReport]) -> String {
    let mut out = String::from("- checks:\n");
    if checks.is_empty() {
        out.push_str("  - none claimed\n");
    }
    for check in checks {
        let outcome = match check.exit_code {
            Some(code) => format!("exited {code}"),
            None => "killed by timeout (no exit code)".to_string(),
        };
        out.push_str(&format!(
            "  - {}: `{}` {outcome}\n",
            check.gate, check.command
        ));
        let tail = check.output_tail.trim_end();
        if !tail.is_empty() {
            out.push_str("    ```\n");
            for line in tail.lines() {
                out.push_str(&format!("    {line}\n"));
            }
            out.push_str("    ```\n");
        }
    }
    out
}

/// Where each proof the worker named sits in the packet, or that it is missing.
///
/// Empty when the worker named none: the "proof" list above already says so.
fn proof_section(copies: &[(String, Option<String>)]) -> String {
    if copies.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n## Proof files in this packet\n\n");
    for (named, copy) in copies {
        match copy {
            Some(copy) => out.push_str(&format!("- `{named}` is at `{copy}`\n")),
            None => out.push_str(&format!(
                "- `{named}` could not be copied into the packet: it is missing, outside the worktree or too large\n"
            )),
        }
    }
    out
}

/// The most a single proof file may weigh to be copied into a packet.
const PROOF_COPY_LIMIT: u64 = 2 * 1024 * 1024;

/// Copies the proof files an attempt named from its worktree into
/// `<packet>/proof/`, and returns where each one went.
///
/// Proof is usually left untracked (`.shprd/proof/…`), so the diff never holds
/// it and a reviewer reading only the packet could not check a claim about it.
/// A path that resolves outside the worktree, a missing file, a directory and a
/// file over [`PROOF_COPY_LIMIT`] are reported as not copied rather than
/// failing the review. The folder is rebuilt each time so a re-run does not
/// keep a stale proof.
fn copy_proofs(worktree: &Path, proofs: &[String], packet: &Path) -> Vec<(String, Option<String>)> {
    let target = packet.join("proof");
    let _ = std::fs::remove_dir_all(&target);
    if proofs.is_empty() {
        return Vec::new();
    }
    let root = real_dir(worktree);
    let mut out = Vec::new();
    for (index, named) in proofs.iter().enumerate() {
        let path = Path::new(named);
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            worktree.join(path)
        };
        let copied = absolute
            .canonicalize()
            .ok()
            .filter(|real| real.starts_with(&root))
            .filter(|real| {
                std::fs::metadata(real)
                    .map(|meta| meta.is_file() && meta.len() <= PROOF_COPY_LIMIT)
                    .unwrap_or(false)
            })
            .and_then(|real| {
                let name = real.file_name()?.to_string_lossy().into_owned();
                std::fs::create_dir_all(&target).ok()?;
                let relative = format!("proof/{index}-{name}");
                std::fs::copy(&real, packet.join(&relative)).ok()?;
                Some(relative)
            });
        out.push((named.clone(), copied));
    }
    out
}

/// Whether an ask is a card the engine raised (a refused hand-in or a scope
/// widening) rather than one the worker raised itself.
fn engine_card(record: &AskRecord) -> bool {
    record
        .id
        .starts_with(shprd_core::ask::SCOPE_WIDEN_ASK_PREFIX)
        || record
            .id
            .starts_with(shprd_core::gate::HAND_IN_REFUSED_ASK_PREFIX)
}

/// One ask as the contract lists it: text, options and the answer.
fn push_ask(out: &mut String, record: &AskRecord) {
    out.push_str(&format!(
        "- {} `{}`: {}\n",
        record.kind.word(),
        record.id,
        record.text
    ));
    out.push_str(&format!(
        "  - options: {}\n",
        if record.options.is_empty() {
            "none".to_owned()
        } else {
            record.options.join(", ")
        }
    ));
    out.push_str(&format!(
        "  - answer: {}\n",
        match &record.answer {
            Some(answer) => answer.text.as_str(),
            None => "unanswered",
        }
    ));
}

/// The asks of the task, in three groups: the worker's own asks in this
/// attempt, the worker's asks in earlier attempts (their answers still stand),
/// and the cards the engine raised, listed apart.
///
/// The first section is written even when the attempt asked nothing: a
/// reviewer reading "none" knows a criterion about asking was never met, while
/// a missing section reads like a packet that lost something. An ask answered
/// in an earlier attempt is shown too, so a later attempt is never judged for
/// "never asking" when the person already answered.
fn asks_section(
    task: &str,
    attempt: Option<&Attempt>,
    asks: &[(TaskId, AskId, AskRecord)],
) -> String {
    let mut records: Vec<&AskRecord> = asks
        .iter()
        .filter(|(owner, _, _)| owner == task)
        .map(|(_, _, record)| record)
        .collect();
    records.sort_by_key(|record| record.raised_seq);
    let worker = records.iter().copied().filter(|r| !engine_card(r));
    let (this, earlier): (Vec<&AskRecord>, Vec<&AskRecord>) =
        worker.partition(|record| raised_in(attempt, record));
    // An earlier ask still standing is one the person answered.
    let earlier: Vec<&AskRecord> = earlier
        .into_iter()
        .filter(|record| record.answer.is_some())
        .collect();
    let cards: Vec<&AskRecord> = records.iter().copied().filter(|r| engine_card(r)).collect();
    let mut out = String::from("\n## Asks raised during this attempt\n\n");
    if this.is_empty() {
        out.push_str("- none: this attempt asked the person nothing\n");
    }
    for record in this {
        push_ask(&mut out, record);
    }
    if !earlier.is_empty() {
        out.push_str("\n## Asks raised in earlier attempts (their answers still stand)\n\n");
        for record in earlier {
            push_ask(&mut out, record);
        }
    }
    if !cards.is_empty() {
        out.push_str("\n## Cards the engine raised (not the worker's own asks)\n\n");
        for record in cards {
            push_ask(&mut out, record);
        }
    }
    out
}

/// Whether `record` was raised while `attempt` was the live one.
///
/// The attempt's spawn came first and its report came after, so an ask raised
/// in that window belongs to this attempt. Without a recorded spawn — an
/// attempt a test built, or one folded from a log written before the field
/// existed — every ask counts, because a packet that shows too much is
/// never the one that hides a criterion.
fn raised_in(attempt: Option<&Attempt>, record: &AskRecord) -> bool {
    let Some(attempt) = attempt else {
        return true;
    };
    let Some(spawn) = attempt.spawn_seq else {
        return true;
    };
    record.raised_seq > spawn
}

/// One reported list, saying so plainly when the agent claimed none.
fn listed(name: &str, items: &[String]) -> String {
    let mut out = format!("- {name}:\n");
    if items.is_empty() {
        out.push_str("  - none claimed\n");
    }
    for item in items {
        out.push_str(&format!("  - {item}\n"));
    }
    out
}

/// The JSON Schema a `findings.json` must match.
///
/// It describes exactly what [`shprd_core::event::Finding`] deserializes, so
/// findings that satisfy the schema are findings a gate can read unchanged. An
/// empty array is valid: a review that found nothing is a passing review.
pub fn findings_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "shprd review findings",
        "type": "array",
        "items": {
            "type": "object",
            "additionalProperties": false,
            "required": ["severity", "title", "detail"],
            "properties": {
                "severity": {
                    "type": "string",
                    "enum": ["critical", "major", "minor", "trivial"]
                },
                "title": { "type": "string", "minLength": 1 },
                "location": { "type": "string" },
                "detail": { "type": "string", "minLength": 1 }
            }
        }
    })
}

/// The diff between `branch` and its merge base with `base`, run in `repo`.
///
/// The merge base is what makes the packet the task's own change: diffing
/// against the integration branch's tip would show every other task that landed
/// since this one forked. It is taken against the freshest of the local base
/// and `origin/<base>` (fetched first), so a landing merged on the forge, which
/// moves only the remote ref, is not read as the task's work.
///
/// The task's declared scope is a hint, not a limit: a diff that leaves it is
/// reviewed like any other, and the reviewer judges whether each change serves
/// the task.
fn diff_against_merge_base(repo: &Path, base: &str, branch: &str) -> Result<String, CliError> {
    let git = Git::new(repo);
    let merge_base = shprd_git::sync::attempt_base(&git, base, branch, true).map_err(|err| {
        CliError::other(anyhow!(err).context(format!("finding where {branch} forked from {base}")))
    })?;
    git.run(&["diff", &format!("{merge_base}..{branch}")])
        .map_err(|err| {
            CliError::other(anyhow!(err).context(format!("diffing {branch} against {base}")))
        })
}

/// Writes the packet a reviewer reads, and returns its folder.
///
/// The attempt's branch is read from the log, never from the filesystem, so the
/// packet describes what the run recorded rather than whatever a checkout
/// happens to hold. Every file is overwritten, so re-running a review on one
/// attempt refreshes the packet instead of appending to it.
fn write_packet(env: &ReviewEnv<'_>, task: &Task, attempt: &Attempt) -> Result<PathBuf, CliError> {
    let dir = packet_dir(&env.reviews_dir, &env.spawn.run, &task.id, attempt.n);
    // The diff is read before the packet's folder exists: a review that cannot
    // read it leaves nothing behind.
    let diff =
        diff_against_merge_base(&env.spawn.ctx.root, &env.spawn.base_branch, &attempt.branch)?;
    std::fs::create_dir_all(&dir).map_err(|err| {
        CliError::other(anyhow!(err).context(format!("creating {}", dir.display())))
    })?;
    write_file(&dir.join("diff.patch"), &diff)?;
    // The protected flag reads the same real diff, against the current base. A
    // flag git could not compute is left off: the person still sees the diff.
    let protected = shprd_git::changed::protected_touches(
        &env.spawn.ctx.root,
        &env.spawn.base_branch,
        &attempt.branch,
        &task.scope,
    )
    .unwrap_or_default();
    // The attempt's own asks come from the fold, which is the only place an
    // answer is recorded: the reviewer reads the same answer the worker did.
    let state = env.spawn.ctx.state()?;
    let asks: Vec<(TaskId, AskId, AskRecord)> = state
        .asks
        .iter()
        .filter(|((ask_task, _), _)| *ask_task == task.id)
        .map(|((ask_task, ask), record)| (ask_task.clone(), ask.clone(), record.clone()))
        .collect();
    let proof_copies = match attempt.report.as_ref() {
        Some(report) => copy_proofs(Path::new(&attempt.worktree), &report.proofs, &dir),
        None => Vec::new(),
    };
    write_file(
        &dir.join("contract.md"),
        &render_contract(&ContractInput {
            task,
            asks: &asks,
            proof_copies: &proof_copies,
            protected: &protected,
        }),
    )?;
    write_file(&dir.join("findings.schema.json"), &schema_text()?)?;
    Ok(dir)
}

/// The findings schema, rendered as the file's text.
fn schema_text() -> Result<String, CliError> {
    let text = serde_json::to_string_pretty(&findings_schema())
        .map_err(|err| CliError::other(anyhow!(err).context("rendering the findings schema")))?;
    Ok(format!("{text}\n"))
}

/// Writes one packet file, so its name and its failure context sit together.
fn write_file(path: &Path, text: &str) -> Result<(), CliError> {
    std::fs::write(path, text)
        .map_err(|err| CliError::other(anyhow!(err).context(format!("writing {}", path.display()))))
}

/// The attempt a review may look at, or the refusal saying why not.
///
/// Only a task whose latest attempt reported completion is reviewable: a blocked
/// or failed attempt has no finished work to review, and reviewing an older
/// attempt would review work the run has already moved past.
fn reviewed_attempt(task: &Task) -> Result<&Attempt, CliError> {
    if task.status != TaskStatus::Reported {
        return Err(CliError::Invalid(anyhow!(
            "task {:?} is {status:?}, not Reported: a review needs a completed report",
            task.id,
            status = task.status
        )));
    }
    let attempt = task
        .attempts
        .last()
        .ok_or_else(|| CliError::Invalid(anyhow!("task {:?} has no attempt to review", task.id)))?;
    match attempt.report.as_ref() {
        Some(report) if report.status == ReportStatus::Completed => Ok(attempt),
        Some(report) => Err(CliError::Invalid(anyhow!(
            "task {:?} attempt {} reported {status:?}, not Completed: there is nothing to review",
            task.id,
            attempt.n,
            status = report.status
        ))),
        None => Err(CliError::Invalid(anyhow!(
            "task {:?} attempt {} has not reported: run `shprd agent report` first",
            task.id,
            attempt.n
        ))),
    }
}

/// The attempt a review may restart, or the refusal saying why not.
///
/// A restart is for a review that exists and is not decided: the attempt needs
/// a recorded request and no verdict, because once the review gate has passed or
/// failed, the review is over and the way forward is the next attempt, not a
/// second reviewer on this one.
fn restartable_attempt(task: &Task) -> Result<&Attempt, CliError> {
    // The request and the verdict are read before the status, because a verdict
    // is what moved the task to Passed or Failed: the caller needs to hear that
    // the review is decided, not that the task is no longer Reported.
    let attempt = task
        .attempts
        .last()
        .ok_or_else(|| CliError::Invalid(anyhow!("task {:?} has no attempt to review", task.id)))?;
    if attempt.reviews.is_empty() {
        return Err(CliError::Invalid(anyhow!(
            "task {:?} attempt {} has no review to restart: run `shprd agent review --task {}` first",
            task.id,
            attempt.n,
            task.id
        )));
    }
    if let Some(gate) = attempt
        .gates
        .iter()
        .rev()
        .find(|gate| gate.gate == REVIEW_GATE)
    {
        let decided = if gate.passed { "passed" } else { "failed" };
        return Err(CliError::Invalid(anyhow!(
            "task {:?} attempt {} already has a review verdict that {decided}: there is nothing to \
             restart, so start the next attempt instead",
            task.id,
            attempt.n
        )));
    }
    reviewed_attempt(task)
}

/// Refuses a review while one is already out for the same attempt.
///
/// A task is still `Reported` between its request and the verdict, so a second
/// plain review would look legal. It is not: the first reviewer is still
/// running, and recording a second request would overwrite the only record of
/// its tab. The way forward is `--again`, which closes that reviewer and starts
/// a fresh one.
fn refuse_pending_review(task: &Task, attempt: &Attempt) -> Result<(), CliError> {
    if attempt.reviews.is_empty() {
        return Ok(());
    }
    Err(CliError::Invalid(anyhow!(
        "task {:?} attempt {} already has a review requested and no verdict yet: pass --again to \
         replace it, or wait for the reviewer to write its findings",
        task.id,
        attempt.n
    )))
}

/// Refuses a restart the findings file says is a mistake.
///
/// The watchdog reads the reviewer's `findings.json` once it settles, so a
/// restart that deletes the file destroys finished work when the reviewer was
/// only just ahead of the watch. Two states are therefore refused, and both
/// leave the packet exactly as it was:
///
/// - findings that parse: the review is decided, and the next tick records the
///   verdict, so there is nothing to restart;
/// - a file younger than the watchdog's settle window: the reviewer may still
///   be writing, so what is on disk is not yet the review's last word.
///
/// A settled file that does not parse is the one case a restart may clear: the
/// watchdog escalates an invalid packet instead of deciding from it, so the
/// review genuinely has to be started over.
fn check_restartable_findings(
    reviews: &Path,
    run: &str,
    task: &str,
    attempt: u32,
) -> Result<(), CliError> {
    let path = findings_path(reviews, run, task, attempt);
    if !path.exists() {
        return Ok(());
    }
    if crate::cmd::verdict::read_findings(&path).is_ok() {
        return Err(CliError::Invalid(anyhow!(
            "the reviewer already wrote its findings: the watchdog will record the verdict"
        )));
    }
    if crate::cmd::watch::settled_mtime(&path).is_none() {
        return Err(CliError::Invalid(anyhow!(
            "the reviewer is still writing its findings; try again shortly"
        )));
    }
    Ok(())
}

/// Removes the findings file a previous reviewer may have left behind.
///
/// A reviewer writes `findings.json` when it is done, and the watchdog acts on
/// that file. A restart must not let a half-written file from the review it
/// replaces be read as the new review's verdict, so the packet is cleared of it
/// before the fresh reviewer starts. No file is the normal case, not a failure;
/// a file worth keeping never reaches here, because
/// [`check_restartable_findings`] refuses the restart first.
fn clear_findings(reviews: &Path, run: &str, task: &str, attempt: u32) -> Result<(), CliError> {
    let path = findings_path(reviews, run, task, attempt);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(CliError::other(
            anyhow!(err).context(format!("removing {}", path.display())),
        )),
    }
}

/// The role id a review resolves its reviewer from.
pub const REVIEWER_ROLE: &str = "reviewer";

/// The reviewer the settings name: the resolved `roles.reviewer` row.
///
/// It is plain data so the pick and the argv can be tested without a settings
/// file, and every field keeps the layer that supplied it: a value the old
/// `config.toml` still sets reads as [`Source::Legacy`], not as the person's own
/// settings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReviewerRole {
    /// The program, in the words of the settings role.
    pub agent: Option<Sourced<String>>,
    /// The model, in the program's own words.
    pub model: Option<Sourced<String>>,
    /// The effort level, in the program's own words.
    pub effort: Option<Sourced<String>>,
    /// The access mode, one of the program's `[permissions]` ids.
    pub perm: Option<Sourced<String>>,
    /// Whether the role only reads, as the settings resolve it.
    pub readonly: bool,
    /// Whether the role may review: its kind is `reviewer`, or it only reads.
    pub may_review: bool,
    /// True when the person turned `review.cross_vendor` off: a reviewer of the
    /// author's own vendor, or of an unknown one, is then no reason to refuse or
    /// to label the review degraded. False (the default) holds the rule.
    pub same_vendor_ok: bool,
    /// The reviewer lines after the first, in order: the next one is used when
    /// the one before is not installed here or cannot review this change (it
    /// shares the author's vendor).
    pub fallbacks: Vec<WorkerLine>,
    /// The programs the run knows are out of quota or rate-limited right now:
    /// a review skips them for the next line instead of starting there.
    pub dry: Vec<DryProgram>,
    /// True when `perm` came from a program line's own mode rather than the
    /// role's: a line's read-only mode cannot write the packet's `findings.json`,
    /// so a review never runs under it (see [`choose_main_reviewer`]).
    pub line_perm: bool,
}

/// A program the run recorded as out of quota, until a time or until someone
/// re-admits it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DryProgram {
    /// The program's harness id.
    pub agent: String,
    /// Why, as the `Dry` event recorded it: `out of quota` or `rate-limited`,
    /// then the CLI's own phrase.
    pub reason: String,
    /// When it is expected back, ms since the epoch, when the CLI said.
    pub until_ms: Option<u64>,
}

impl DryProgram {
    /// The plain words for this program's state: `codex is out of quota until
    /// Dec 18`.
    pub fn sentence(&self) -> String {
        let state = if self.reason.starts_with("rate-limited") {
            "rate-limited"
        } else {
            "out of quota"
        };
        match self.until_ms.and_then(shprd_agents::exhausted::day_of) {
            Some(day) => format!("{} is {state} until {day}", self.agent),
            None => format!("{} is {state}", self.agent),
        }
    }
}

impl ReviewerRole {
    /// The role as one settings resolution answers it.
    ///
    /// The reviewer lines are the settings' own list, in its order: the role's
    /// `agent` first when it names one, else the first of `agents`. A role that
    /// lists its programs under `agents` alone is reviewed by the first of them,
    /// never by a program the old `config.toml` named.
    pub fn resolved(role: &ResolvedRole) -> Self {
        let lines = role.lines();
        let own_first = role
            .agent
            .as_ref()
            .is_some_and(|agent| lines.first().is_some_and(|line| line.agent == agent.value));
        let mut this = Self {
            agent: role.agent.clone(),
            model: role.model.clone(),
            effort: role.effort.clone(),
            perm: role.perm.clone(),
            readonly: role.readonly.value,
            may_review: role.may_review(),
            same_vendor_ok: false,
            fallbacks: lines.iter().skip(1).cloned().collect(),
            dry: Vec::new(),
            line_perm: false,
        };
        if !own_first && let Some(first) = lines.first() {
            let from = |value: &Option<String>| {
                value
                    .clone()
                    .map(|value| Sourced::new(value, role.agents.source))
            };
            this.agent = Some(Sourced::new(first.agent.clone(), role.agents.source));
            this.model = from(&first.model);
            this.effort = from(&first.effort);
            // The line's own access mode, else the role's.
            if first.perm.is_some() {
                this.perm = from(&first.perm);
                this.line_perm = true;
            }
        }
        this
    }

    /// A reviewer written the pre-M9 way, `program/model`, as the legacy layer
    /// names it.
    ///
    /// The model keeps every `/` after the program's, because a multi-model CLI
    /// names its provider there (`pi/deepseek/deepseek-v4.1-flash`).
    pub fn written(written: &str, effort: Option<&str>) -> Self {
        let trimmed = written.trim();
        let (agent, model) = match trimmed.split_once('/') {
            Some((program, model)) if !model.trim().is_empty() => {
                (program.trim(), Some(model.trim()))
            }
            _ => (trimmed, None),
        };
        let text = |value: &str| Sourced::new(value.to_owned(), Source::Legacy);
        Self {
            agent: Some(text(agent)).filter(|agent| !agent.value.is_empty()),
            model: model.map(text),
            effort: effort
                .map(str::trim)
                .filter(|effort| !effort.is_empty())
                .map(text),
            perm: None,
            // The legacy key named a reviewer, so the role reviews: the
            // built-in reviewer is a `reviewer` that only reads.
            readonly: true,
            may_review: true,
            same_vendor_ok: false,
            fallbacks: Vec::new(),
            dry: Vec::new(),
            line_perm: false,
        }
    }

    /// The recorded dry state of `agent`, when it is dry.
    fn dry_entry(&self, agent: &str) -> Option<&DryProgram> {
        self.dry.iter().find(|dry| dry.agent == agent)
    }

    /// The program, when the role names one.
    pub fn agent(&self) -> Option<&str> {
        self.agent
            .as_ref()
            .map(|agent| agent.value.trim())
            .filter(|agent| !agent.is_empty())
    }

    /// The model, when the role names one.
    pub fn model(&self) -> Option<&str> {
        self.model
            .as_ref()
            .map(|model| model.value.trim())
            .filter(|model| !model.is_empty())
    }

    /// The effort, when the role names one.
    pub fn effort(&self) -> Option<&str> {
        self.effort
            .as_ref()
            .map(|effort| effort.value.trim())
            .filter(|effort| !effort.is_empty())
    }

    /// The access mode, when the role names one.
    pub fn perm(&self) -> Option<&str> {
        self.perm
            .as_ref()
            .map(|perm| perm.value.trim())
            .filter(|perm| !perm.is_empty())
    }

    /// The program and model as the old one-word agent string would write them.
    fn written_agent(&self) -> Option<String> {
        let agent = self.agent()?;
        Some(match self.model() {
            Some(model) => format!("{agent}/{model}"),
            None => agent.to_owned(),
        })
    }
}

/// The agent a review runs on, with everything needed to start it.
#[derive(Debug, Clone)]
pub struct Reviewer {
    /// The program name, which is the harness spec id.
    pub agent: String,
    /// The model to select, when the configured string carried one.
    pub model: Option<String>,
    /// The effort level, when the role names one.
    pub effort: Option<String>,
    /// The access mode the role names, when the program offers it.
    pub perm: Option<String>,
    /// The harness spec the program runs from: how to start it, and how a
    /// reviewer is confined.
    pub spec: HarnessSpec,
    /// The prompts the CLI waits on a human to answer before it reads anything.
    pub blocking_prompts: Vec<BlockingPrompt>,
    /// The lines the program's TUI prints once it can take its turn.
    pub ready_lines: Vec<String>,
    /// Whether the host can be expected to report this program's status.
    pub own_status: bool,
    /// Whether this review is cross-vendor or degraded.
    pub degraded: bool,
    /// A one-line note to show the human when a flag had no effect.
    pub note: Option<String>,
    /// The plain sentence for the task's card when this reviewer is not the
    /// first choice because the first is out of quota.
    pub card_note: Option<String>,
}

impl Reviewer {
    /// The readiness this reviewer's program has, as the delivery needs it.
    fn readiness(&self) -> deliver::Readiness<'_> {
        deliver::Readiness {
            lines: &self.ready_lines,
            prompts: &self.blocking_prompts,
            own_status: self.own_status,
        }
    }
}

/// Picks the reviewer for one attempt.
///
/// The rule is the one AGENTS.md states: a change is reviewed by an agent from a
/// different vendor than the one that wrote it, and a review that cannot meet
/// that rule is labelled rather than pretended. The vendor compared here is the
/// vendor of the *model* each side runs, never the program name: a `pi` worker
/// on DeepSeek reviewed by a `pi` reviewer on Claude is cross-vendor, and a `pi`
/// worker on Claude reviewed by Claude Code is not. An agent whose family cannot
/// be proven, such as one running a router, is never assumed independent.
///
/// The reviewer is the `roles.reviewer` row the settings resolve, so the program,
/// model, effort, access mode and the read-only rule all come from one place;
/// the pre-M9 `[roster] reviewer` reaches the same answer through the settings'
/// legacy layer. The cross-vendor check is made against this resolved reviewer,
/// never against a separate config value.
pub fn choose_reviewer(
    role: &ReviewerRole,
    author: &str,
    author_model: Option<&str>,
    programs: &[Program],
    degraded: bool,
) -> Result<Reviewer, CliError> {
    // A program the run knows is out of quota is not tried: the next line is.
    let main_dry = role.agent().and_then(|agent| role.dry_entry(agent));
    let main = match main_dry {
        Some(dry) => Err(CliError::Invalid(anyhow!(dry.sentence()))),
        None => choose_main_reviewer(role, author, author_model, programs, degraded),
    };
    if role.fallbacks.is_empty() && main_dry.is_none() {
        return main;
    }
    // The main reviewer stands when it can start here: the next lines are for a
    // program that is not installed, one that is out of quota, or one that
    // cannot review this change.
    let why = match &main {
        Ok(chosen) if installed(programs, &chosen.agent) => return main,
        Ok(chosen) => format!("`{}` is not installed here", chosen.agent),
        Err(error) => error.to_string(),
    };
    let mut spent: Vec<String> = main_dry.iter().map(|dry| dry.sentence()).collect();
    for line in &role.fallbacks {
        if let Some(dry) = role.dry_entry(&line.agent) {
            spent.push(dry.sentence());
            continue;
        }
        let fallback = line.agent_ref();
        let mut next = ReviewerRole::written(&fallback, line.effort.as_deref().or(role.effort()));
        next.perm = line
            .perm
            .as_deref()
            .map(|perm| Sourced::new(perm.to_owned(), Source::You));
        next.line_perm = next.perm.is_some();
        next.readonly = role.readonly;
        next.may_review = role.may_review;
        next.same_vendor_ok = role.same_vendor_ok;
        if let Ok(mut chosen) =
            choose_main_reviewer(&next, author, author_model, programs, degraded)
            && installed(programs, &chosen.agent)
        {
            chosen.note = Some(format!(
                "note: the fallback reviewer `{fallback}` reviews, because {why}"
            ));
            if !spent.is_empty() {
                chosen.card_note = Some(format!(
                    "{}; {} reviews instead",
                    spent.join("; "),
                    reviewer_label(&chosen.agent, chosen.model.as_deref())
                ));
            }
            return Ok(chosen);
        }
    }
    if !spent.is_empty() {
        // Every program that could review is out of quota: the one plain
        // sentence the person's card is made of.
        return Err(CliError::Invalid(anyhow!(
            "{NO_REVIEWER_LEFT}: {}",
            spent.join("; ")
        )));
    }
    // No line can do better: the main reviewer's own answer stands.
    main
}

pub use shprd_core::review::NO_REVIEWER_LEFT;

/// A reviewer as a person reads it: the model's family word, then the program,
/// `GLM (pi)`; the bare program when no model is named.
pub fn reviewer_label(agent: &str, model: Option<&str>) -> String {
    let family = model
        .and_then(|model| model.rsplit('/').next())
        .map(|name| {
            name.chars()
                .take_while(|c| c.is_ascii_alphabetic())
                .collect::<String>()
        })
        .filter(|word| !word.is_empty());
    match family {
        Some(word) if word.len() <= 4 => format!("{} ({agent})", word.to_ascii_uppercase()),
        Some(word) => {
            let mut chars = word.chars();
            let head = chars.next().map(|c| c.to_ascii_uppercase());
            let rest: String = chars.collect();
            format!("{}{rest} ({agent})", head.unwrap_or_default())
        }
        None => agent.to_owned(),
    }
}

/// Whether the program is installed on this machine.
fn installed(programs: &[Program], id: &str) -> bool {
    programs
        .iter()
        .any(|program| program.spec.id == id && program.installed)
}

/// Picks the one reviewer a [`ReviewerRole`] names, with no fallback.
fn choose_main_reviewer(
    role: &ReviewerRole,
    author: &str,
    author_model: Option<&str>,
    programs: &[Program],
    degraded: bool,
) -> Result<Reviewer, CliError> {
    let raw = role.written_agent().ok_or_else(|| {
        CliError::Invalid(anyhow!(
            "no reviewer is configured: set one with `shprd settings set roles.reviewer.agent \
             <program>` and `roles.reviewer.model <model>`, or the old way with `shprd set \
             reviewer <program>/<model>`"
        ))
    })?;
    if !role.may_review {
        return Err(CliError::Invalid(anyhow!(
            "the role `reviewer` may not review: only a role of kind `reviewer`, or one marked \
             readonly, may review; set `roles.reviewer.kind = \"reviewer\"` or \
             `roles.reviewer.readonly = true`"
        )));
    }
    let specs: Vec<HarnessSpec> = spawn::spec_table(programs);
    let parsed = AgentRef::parse(&raw, &specs).map_err(|err| {
        CliError::Invalid(anyhow!(err).context(format!(
            "the configured reviewer `{raw}` is not a usable agent"
        )))
    })?;
    let program = programs
        .iter()
        .find(|program| program.spec.id == parsed.program)
        .ok_or_else(|| {
            CliError::Invalid(anyhow!(
                "no harness spec for the reviewer {:?} in the data directory",
                parsed.program
            ))
        })?;
    let families = families_table();
    let author_vendor = author_vendor(&specs, author, author_model, &families);
    let reviewer_vendor = agent_vendor(&parsed, &specs, &families);
    let mut reviewer = Reviewer {
        agent: parsed.program,
        model: parsed.model,
        // The role's effort is applied where the CLI takes one; a program with
        // no effort flag runs at its own default.
        effort: role.effort().map(str::to_owned),
        // The mode is kept only when the program offers it, and never when it is
        // the program's full-access one: a reviewer may not act unprompted.
        perm: role
            .perm()
            .filter(|id| program.spec.permissions.mode(id).is_some())
            .filter(|id| program.spec.permissions.full.as_deref() != Some(*id))
            // A program line's read-only mode (pi's `--tools read,grep,find,ls`)
            // has no tool that writes `findings.json`: the reviewer would read
            // the diff, say its findings on the screen and never land a verdict.
            // The role's own explicit mode is still honoured.
            .filter(|id| {
                !(role.line_perm && program.spec.permissions.readonly.as_deref() == Some(*id))
            })
            .map(str::to_owned),
        spec: program.spec.clone(),
        blocking_prompts: program.spec.blocking_prompts.clone(),
        ready_lines: program.spec.ready.screen.clone(),
        own_status: !program.spec.status.needs_the_host(),
        // `--degraded` is only recorded where the review needs the label: the
        // branches below set it, and it stays false for an independent review.
        degraded: false,
        note: None,
        card_note: None,
    };
    // `review.cross_vendor = false` is the person's own answer: any reviewer will
    // do, and nothing about this review is labelled degraded.
    if role.same_vendor_ok {
        return Ok(reviewer);
    }
    // The run log keeps the author's program and model apart; both name the
    // agent in a message.
    let author_label = match author_model {
        Some(model) => format!("{author}/{model}"),
        None => author.to_owned(),
    };
    // An agent whose family nothing proves is never independent: assuming
    // otherwise would pass a review that may share the author's vendor.
    let unknown = match (&author_vendor, &reviewer_vendor) {
        (Some(_), Some(_)) => None,
        (None, _) => Some(author_label.as_str()),
        (_, None) => Some(raw.as_str()),
    };
    if let Some(agent) = unknown {
        if !degraded {
            return Err(CliError::Invalid(anyhow!(
                "cannot prove the reviewer is a different vendor: {agent} has no known model \
                 family: set a reviewer with a known family in `roles.reviewer`, or pass \
                 --degraded to review anyway"
            )));
        }
        eprintln!(
            "warning: cannot prove the reviewer is a different vendor: {agent} has no known \
             model family: this review is {DEGRADED_LABEL}"
        );
        reviewer.degraded = true;
    } else if author_vendor == reviewer_vendor {
        let vendor = reviewer_vendor.as_deref().unwrap_or_default().to_owned();
        if !degraded {
            return Err(CliError::Invalid(anyhow!(
                "the reviewer `{raw}` and `{author_label}`, which wrote this change, both run \
                 models from the {vendor} family: set a reviewer from another vendor in \
                 `roles.reviewer`, or pass --degraded to review anyway"
            )));
        }
        eprintln!(
            "warning: the reviewer `{raw}` and `{author_label}` both run models from the \
             {vendor} family: this review is {DEGRADED_LABEL}"
        );
        reviewer.degraded = true;
    } else if degraded {
        // The reviewer is provably from another family, so the flag was not
        // needed: say so once rather than labelling a sound review.
        reviewer.note = Some(format!(
            "note: --degraded was ignored: the reviewer `{raw}` uses the {} family, which differs \
             from the author's",
            reviewer_vendor.as_deref().unwrap_or_default()
        ));
    }
    Ok(reviewer)
}

/// The model family table the data directory ships, or an empty one.
///
/// An empty table knows no family, so every vendor is unknown and every review
/// must say so: a table that cannot be read may never make a reviewer look
/// independent.
fn families_table() -> Families {
    crate::cmd::roster::resolve_data_dir()
        .families()
        .unwrap_or_default()
}

/// The family of the model the author wrote the change with.
///
/// The run log records the author's program and its model separately, and both
/// are needed: `pi` on DeepSeek and `pi` on Claude are different families.
/// [`None`] when the program names no harness, no model was recorded, or the
/// model is one no family claims.
fn author_vendor(
    specs: &[HarnessSpec],
    agent: &str,
    model: Option<&str>,
    families: &Families,
) -> Option<String> {
    let parsed = AgentRef {
        program: agent.to_owned(),
        model: model.map(str::to_owned),
    };
    agent_vendor(&parsed, specs, families)
}

/// What a reviewer's brief says, as data, so its rendering is testable alone.
pub struct ReviewBrief<'a> {
    /// The task being reviewed.
    pub task: &'a Task,
    /// The run id.
    pub run: &'a str,
    /// The attempt number.
    pub attempt: u32,
    /// The program the reviewer runs.
    pub agent: &'a str,
    /// The model it runs, when one was chosen.
    pub model: Option<&'a str>,
    /// The folder the reviewer reads and writes in.
    pub dir: &'a str,
    /// Whether the review is degraded, and so must say so.
    pub degraded: bool,
    /// The reviewer role's standing words, when its prompt can be read.
    pub prompt: Option<&'a str>,
}

/// Renders the brief a reviewer reads before its first turn.
///
/// It names the three files the reviewer may read, says plainly that the
/// repository is off limits, and ends by telling the reviewer to write its
/// findings and stop: the reviewer runs no `shprd` command, because its folder
/// is outside the repository and the engine records the verdict instead.
pub fn render_review_brief(input: &ReviewBrief<'_>) -> String {
    let mut out = String::new();
    if let Some(prompt) = input.prompt {
        out.push_str(prompt.trim_end());
        out.push_str("\n\n");
    }
    out.push_str(&format!(
        "# REVIEW {} {}\n\n",
        input.task.id, input.task.title
    ));
    out.push_str(&format!(
        "run `{}`, attempt {}, reviewer `{}`{}\nyour folder: `{}`\n\n",
        input.run,
        input.attempt,
        input.agent,
        match input.model {
            Some(model) => format!(" ({model})"),
            None => String::new(),
        },
        input.dir
    ));
    if input.degraded {
        out.push_str(&format!(
            "this review is labelled **{DEGRADED_LABEL}**: the reviewer shares a vendor with \
             the agent that wrote the change, so it is not an independent review. Say so in \
             your findings.\n\n"
        ));
    }
    out.push_str("## Files\n\n");
    out.push_str("Read these, in this folder, in this order, and nothing else:\n\n");
    out.push_str("1. `contract.md` — the task, what \"done\" meant, what the agent reported, and the asks it raised with the person's answers\n");
    out.push_str(
        "2. `diff.patch` — the whole change, against its merge base with the integration branch\n",
    );
    out.push_str("3. `proof/` — copies of the proof files the worker named, when it named any; `contract.md` says where each one is\n");
    out.push_str("4. `findings.schema.json` — the shape of what you write\n\n");
    out.push_str("Write `findings.json` in this folder once: a JSON array of `{severity, title, location, detail}`, where severity is one of `critical`, `major`, `minor` or `trivial`. Write nothing else, and nothing outside this folder.\n");
    out
}

/// Opens the reviewer's tab in `dir` and starts its CLI there.
///
/// The tab's cwd is the packet folder, never the worktree: where a reviewer's
/// shell starts is the difference between reading a diff and editing the code.
/// Returns the tab's id and title.
fn open_reviewer(
    mux: &dyn Host,
    env: &ReviewEnv<'_>,
    task: &str,
    reviewer: &Reviewer,
    dir: &Path,
    brief: &ReviewBrief<'_>,
    brief_path: &Path,
) -> Result<(shprd_mux::TabId, String), CliError> {
    // The reviewer is a second agent of the task and starts the way the worker
    // did: headless where the host can be, a tab of the repository's one space
    // where it cannot. It is the coordinator's child, like the worker.
    let space = spawn::agent_space(mux, &env.spawn)?;
    let title = reviewer_title(&reviewer.agent, task);
    let text = render_review_brief(brief);
    let (mut argv, brief_in_args) = reviewer_argv(
        &reviewer.spec,
        reviewer.model.as_deref(),
        reviewer.effort.as_deref(),
        reviewer.perm.as_deref(),
        &text,
        brief_path,
    )?;
    let pairs = env_pairs(launch_env(
        &env.spawn.base_env,
        EnvRole::Reviewer,
        &EnvContext {
            actor: format!("agent:{}", reviewer.agent),
            task: Some(task.to_string()),
            run: Some(env.spawn.run.clone()),
            repo: real_dir(dir),
            home: Some(env.spawn.home.clone()),
            bin: shprd_agents::env::service_bin(),
            data: env.spawn.data_dir.env_value().map(Path::to_path_buf),
        },
    ));
    let parent = spawn::coordinator_pane_of(&env.spawn.home, &env.spawn.ctx.root, mux, &space);
    // Codex refuses to start a shell when a writable root goes through a
    // symlink, and the reviewer's cwd is its writable root: always hand it the
    // folder's real path, whatever spelling the packet was reached by.
    let real_cwd = real_dir(dir);
    // The packet folder is one shprd made, so no first-run dialog may ask about
    // it: a Claude reviewer sat on its folder-trust question and never got its
    // kickoff.
    spawn::trust_launch_folder(&reviewer.agent, &real_cwd, &env.spawn.base_env, &mut argv);
    let tab = mux
        .spawn_agent(&shprd_mux::AgentLaunch {
            space: &space.id,
            title: &title,
            argv: &argv,
            env: &pairs,
            cwd: &real_cwd,
            role: shprd_mux::AgentRole::Reviewer,
            task: Some(task),
            program: &reviewer.agent,
            parent: parent.as_ref(),
        })
        .map_err(|err| CliError::other(anyhow!(err).context("starting the reviewer")))?;
    // The tab exists from here on, so every failure closes it again.
    let started = deliver_review_turn(mux, env, reviewer, task, &text, &tab, brief_in_args);
    match started {
        Ok(()) => Ok((tab.id, title)),
        Err(err) => {
            spawn::close_tab(mux, &tab.id);
            Err(err)
        }
    }
}

/// The title of a reviewer's tab: `<agent>·<task>·review`.
///
/// The `review` suffix is what keeps a reviewer's tab from reading as the
/// worker's, now that the role glyphs are gone: the two sit in the same space,
/// one tab apart, so the titles are what tells them apart.
fn reviewer_title(agent: &str, task: &str) -> String {
    format!("{agent}·{task}·review")
}

/// Hands the reviewer's brief to the CLI that is already running in `tab`.
///
/// A brief that does not arrive is reported, never fatal: the CLI is running,
/// and the human can resend the brief into the tab. A lost first delivery is
/// retried once by [`deliver_review_brief`] before it is reported.
fn deliver_review_turn(
    mux: &dyn Host,
    env: &ReviewEnv<'_>,
    reviewer: &Reviewer,
    task: &str,
    rendered: &str,
    tab: &shprd_mux::Tab,
    brief_in_args: bool,
) -> Result<(), CliError> {
    // A brief that arrived as a system prompt leaves the CLI idle, waiting for a
    // first turn, so it gets the same one-line kickoff a worker gets; a reviewer
    // whose CLI has no system-prompt flag gets the whole brief typed in.
    let (text, what) = if brief_in_args {
        (deliver::KICKOFF, "review kickoff")
    } else {
        (rendered, "review brief")
    };
    let started = std::time::Instant::now();
    let delivery = deliver_review_brief(mux, env, reviewer, tab, text);
    let Some(reason) = delivery.reason() else {
        return Ok(());
    };
    let waited_ms = started.elapsed().as_millis();
    eprintln!("warning: {what} not delivered: {reason}; the brief is beside the review packet");
    let state = env.spawn.ctx.state().ok();
    let reviews = state
        .as_ref()
        .and_then(|state| state.tasks.get(task))
        .and_then(|entry| entry.latest_attempt())
        .map(|attempt| attempt.reviews.as_slice())
        .unwrap_or_default();
    let request_seq = reviews.last().map(|request| request.seq);
    // A reviewer that is already working, or whose review already has a result,
    // got its turn after all: the detection was late, not the delivery lost.
    let answered = state
        .as_ref()
        .and_then(|state| state.tasks.get(task))
        .and_then(|entry| entry.latest_attempt())
        .is_some_and(|attempt| {
            attempt.gates.iter().any(|gate| {
                gate.gate == shprd_core::gate::REVIEW_GATE
                    && request_seq.is_some_and(|seq| gate.seq > seq)
            })
        });
    let working = matches!(
        mux.agent_state(&tab.root_pane),
        Ok(shprd_mux::AgentState::Working)
    );
    if answered || working {
        eprintln!("note: the reviewer is already working, so no escalation is raised");
        return Ok(());
    }
    // With more than one review request on the attempt the card says which it is about.
    let which = match request_seq {
        Some(seq) if reviews.len() > 1 => format!(" (review request #{seq})"),
        _ => String::new(),
    };
    let escalation = EventKind::Escalated {
        task: Some(task.to_string()),
        // The ledger says which turn was lost, the way the warning does: a
        // kickoff that never arrived and a brief that never arrived are fixed
        // the same way but are different causes to read.
        body: match delivery.blocked_by(&reviewer.agent) {
            Some(cause) => format!("{what} not delivered: {cause}{which}"),
            None => format!("{what} not delivered{which}"),
        },
        default_action: format!(
            "press Retry review on the task in the Tasks pane, which closes the reviewer \
             tab and delivers the {what} again ({reason}; tried for {waited_ms} ms in all); \
             never type into the reviewer's tab"
        ),
    };
    if let Err(err) = env.spawn.ctx.commit(escalation) {
        eprintln!("warning: could not record the escalation: {err}");
    }
    Ok(())
}

/// Hands the reviewer its brief, giving a lost first delivery one more chance.
///
/// A freshly started CLI is not always ready for its first prompt: the host may
/// not have detected the agent yet, or the pane may still be settling. Neither
/// means the review is lost, so the brief is offered a second time after
/// [`ReviewEnv::brief_retry_wait`] before the human is told. One retry covers a
/// start-up race; a reviewer that is genuinely stuck still produces exactly one
/// warning and one escalation, and they carry the reason the last attempt gave.
fn deliver_review_brief(
    mux: &dyn Host,
    env: &ReviewEnv<'_>,
    reviewer: &Reviewer,
    tab: &shprd_mux::Tab,
    text: &str,
) -> deliver::Delivery {
    let wait = env.spawn.ready_wait;
    let readiness = reviewer.readiness();
    let first = deliver::deliver_brief(mux, &tab.root_pane, text, &readiness, wait);
    if first.ready().is_some() {
        return first;
    }
    // A prompt waiting on a human is not a start-up race: the second attempt
    // would type the brief over the question the person has to answer.
    if matches!(first, deliver::Delivery::Blocked { .. }) {
        return first;
    }
    std::thread::sleep(env.brief_retry_wait);
    deliver::deliver_brief(mux, &tab.root_pane, text, &readiness, wait)
}

/// The argv a reviewer's CLI starts with.
///
/// The order is the base command, the flags that confine a reviewer's tools
/// ([`review_confinement`]), the model, the effort, and finally the brief as the
/// system prompt. A reviewer never gets [`LaunchSpec::auto_approve`]: it runs
/// beside a human, so its tools must prompt rather than act unprompted, and a
/// reviewer that could silently write could read or change the worktree the
/// brief told it to leave alone. The second element says whether the brief went
/// in as an argument, which is also what says whether it has to be typed in
/// instead.
fn reviewer_argv(
    spec: &HarnessSpec,
    model: Option<&str>,
    effort: Option<&str>,
    perm: Option<&str>,
    brief: &str,
    brief_path: &Path,
) -> Result<(Vec<String>, bool), CliError> {
    let launch = &spec.launch;
    let mut argv = launch.interactive.clone();
    argv.extend(review_confinement(spec, perm));
    let values = Values {
        model: model.map(str::to_string),
        effort: effort.map(str::to_string),
        prompt: Some(brief.to_string()),
        prompt_file: Some(brief_path.display().to_string()),
        session: None,
    };
    argv.extend(render(spec, deliver::model_args(launch, model), &values)?);
    if let (Some(effort_args), Some(_)) = (&launch.effort_args, effort) {
        argv.extend(render(spec, effort_args, &values)?);
    }
    let mut brief_in_args = false;
    if let Some(system_prompt_args) = &launch.system_prompt_args {
        argv.extend(render(spec, system_prompt_args, &values)?);
        brief_in_args = true;
    }
    Ok((argv, brief_in_args))
}

/// The flags that confine a reviewer's tools, before the model.
///
/// A role that names an access mode the program offers gets exactly that mode,
/// unless it is the program's full-access one: a reviewer may not act
/// unprompted, so a full-access mode is refused here as the settings check
/// refuses it, and the program's own reviewer confinement applies instead.
/// A role that names no mode keeps [`LaunchSpec::reviewer_args`], which is the
/// program's own answer to "how does a reviewer stay out of the worktree and
/// still write its findings": codex's workspace-write sandbox is that answer.
///
/// A role marked read-only is not handed the program's read-only *tool set*
/// when it names no mode: for codex and pi those modes (`-s read-only`,
/// `--tools read,grep,find,ls`) cannot write the packet's `findings.json`, so
/// the review would never produce a verdict. The program's reviewer confinement
/// is what keeps such a reviewer out of the worktree instead. A role that names
/// a read-only mode explicitly still gets it, as does one that names any other
/// mode the program offers.
fn review_confinement(spec: &HarnessSpec, perm: Option<&str>) -> Vec<String> {
    if let Some(mode) = perm.and_then(|id| spec.permissions.mode(id))
        && spec.permissions.full.as_deref() != Some(mode.id.as_str())
    {
        return mode.args.clone();
    }
    spec.launch.reviewer_args.clone().unwrap_or_default()
}

/// Renders one argument template, naming the template when it cannot be filled.
fn render(
    spec: &HarnessSpec,
    template: &[String],
    values: &Values,
) -> Result<Vec<String>, CliError> {
    render_args(template, values).map_err(|err| {
        CliError::other(anyhow!(err).context(format!(
            "filling the launch arguments {template:?} of {:?}",
            spec.launch.interactive
        )))
    })
}

/// Flattens a launch environment into the pairs [`Host::spawn`] takes: set
/// entries
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

/// Opens a cross-vendor reviewer on a task's latest completed attempt.
///
/// The order is the one [`spawn`] uses: every rule is checked and every decision
/// is made before the first side effect, so a refused review leaves nothing
/// behind. The effects then run in the order the packet needs: the files the
/// reviewer reads, the brief that points at them, and only then the tab.
pub fn review(
    mux: &dyn Host,
    env: &ReviewEnv<'_>,
    req: &ReviewRequest,
) -> Result<ReviewReport, CliError> {
    let state = env.spawn.ctx.state()?;
    let task = spawn::task_of(&state, &req.task)?;
    let attempt = if req.again {
        restartable_attempt(task)?
    } else {
        let attempt = reviewed_attempt(task)?;
        // One reviewer at a time on one attempt: a second plain review would
        // append a second request, overwrite the tab record and leave the first
        // reviewer running with nothing naming it. Only `--again` replaces a
        // review that is still out.
        refuse_pending_review(task, attempt)?;
        attempt
    };
    let reviewer = choose_reviewer(
        &env.reviewer,
        &attempt.agent,
        attempt.model.as_deref(),
        &env.spawn.programs,
        req.degraded,
    )?;
    if let Some(note) = &reviewer.note {
        eprintln!("{note}");
    }
    // The reviewer's standing words are read before any effect: a prompt that
    // cannot be read must leave no packet, tab or event behind.
    let prompt = spawn::role_prompt(&env.spawn, "reviewer")?;
    // A restart replaces the review it restarts: the old reviewer's tab is
    // closed and any findings it half-wrote are removed before the packet is
    // rebuilt, so nothing reads a stale verdict as this review's.
    if req.again {
        // Refused before the first side effect, because a closed tab and a
        // removed file are not undone by an error on the next line.
        check_restartable_findings(&env.reviews_dir, &env.spawn.run, &task.id, attempt.n)?;
        close_reviewer(
            mux,
            &env.spawn.briefs_dir,
            &env.spawn.run,
            &task.id,
            attempt.n,
        )?;
        clear_findings(&env.reviews_dir, &env.spawn.run, &task.id, attempt.n)?;
    }
    let dir = write_packet(env, task, attempt)?;
    let brief_path = brief_path(&env.spawn.briefs_dir, &env.spawn.run, &task.id, attempt.n);
    let brief = ReviewBrief {
        task,
        run: &env.spawn.run,
        attempt: attempt.n,
        agent: &reviewer.agent,
        model: reviewer.model.as_deref(),
        dir: &dir.display().to_string(),
        degraded: reviewer.degraded,
        prompt: Some(&prompt),
    };
    spawn::write_brief(&brief_path, &render_review_brief(&brief))?;
    // Who reviews, and whether the review can be independent, is a recorded
    // fact before the reviewer exists: a refused event opens no tab, and
    // `shprd land` reads this instead of guessing it out of prose.
    env.spawn.ctx.commit(EventKind::ReviewRequested {
        task: task.id.clone(),
        attempt: attempt.n,
        reviewer: reviewer.agent.clone(),
        degraded: reviewer.degraded,
        note: reviewer.card_note.clone(),
    })?;
    let (tab_id, tab) = open_reviewer(mux, env, &task.id, &reviewer, &dir, &brief, &brief_path)?;
    spawn::write_tab(
        &tab_record_path(&env.spawn.briefs_dir, &env.spawn.run, &task.id, attempt.n),
        &tab_id,
    );
    Ok(ReviewReport {
        task: task.id.clone(),
        attempt: attempt.n,
        agent: reviewer.agent,
        model: reviewer.model,
        degraded: reviewer.degraded,
        dir,
        brief: brief_path,
        tab,
    })
}

/// `shprd agent review`: opens a reviewer on a task's latest attempt.
pub fn review_verb(ctx: &Ctx, args: &ReviewArgs, home: Option<&Path>) -> Result<(), CliError> {
    let mux = spawn::real_mux();
    let env = ReviewEnv::build(ctx, home)?;
    let request = ReviewRequest {
        task: args.task.clone(),
        degraded: args.degraded,
        again: args.again,
    };
    let report = review(&mux, &env, &request)?;
    if ctx.json {
        let value = json!({
            "task": report.task,
            "attempt": report.attempt,
            "agent": report.agent,
            "model": report.model,
            "degraded": report.degraded,
            "dir": report.dir.display().to_string(),
            "brief": report.brief.display().to_string(),
            "tab": report.tab,
        });
        println!("{value}");
        return Ok(());
    }
    println!("{}", report.dir.display());
    Ok(())
}

/// Starts the review of one task on the engine's behalf, as the tick does.
///
/// It is [`review`] with the verb's own arguments, so the service runs exactly
/// the code a person's `shprd agent review` runs. It returns one line for the
/// pass's report: which reviewer was started, and where.
pub fn review_for_tick(ctx: &Ctx, home: &Path, task: &str) -> Result<String, String> {
    start_review(ctx, home, task, false)
}

/// Closes the task's reviewer and starts a new review, as `--again` does.
///
/// It is what a stuck review's card runs when the person answers `review again`.
pub fn review_again_for_tick(ctx: &Ctx, home: &Path, task: &str) -> Result<String, String> {
    start_review(ctx, home, task, true)
}

/// One review start for the service: [`review`] with the verb's own arguments.
fn start_review(ctx: &Ctx, home: &Path, task: &str, again: bool) -> Result<String, String> {
    let mux = spawn::real_mux();
    let env = ReviewEnv::build(ctx, Some(home)).map_err(|err| err.to_string())?;
    // `review again` on a task whose review never started (every reviewer was
    // out of quota) is a plain review: there is no reviewer to close.
    let started = env
        .spawn
        .ctx
        .state()
        .ok()
        .and_then(|state| {
            state
                .tasks
                .get(task)
                .and_then(|found| found.attempts.last())
                .map(|attempt| !attempt.reviews.is_empty())
        })
        .unwrap_or(true);
    let request = ReviewRequest {
        task: task.to_string(),
        degraded: false,
        again: again && started,
    };
    let report = review(&mux, &env, &request).map_err(|err| err.to_string())?;
    Ok(format!(
        "started {} on {} attempt {} in tab {}",
        report.agent, report.task, report.attempt, report.tab
    ))
}

/// How long a program stays skipped when it is out of quota and printed no
/// time to come back: an hour for an allowance, ten minutes for a rate limit.
const QUOTA_COOLDOWN_MS: u64 = 60 * 60 * 1000;
/// The cooldown of a plain rate limit with no time printed.
const RATE_COOLDOWN_MS: u64 = 10 * 60 * 1000;

/// What a reviewer's pane says when its account cannot answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spent {
    /// `out of quota` or `rate-limited`, then the CLI's own phrase.
    pub reason: String,
    /// When the CLI said it comes back, ms since the epoch.
    pub reset_at_ms: Option<u64>,
}

/// Reads a reviewer pane's bottom for an out-of-quota or rate-limit message.
///
/// Only the last few non-blank lines count, and never a line shprd or the
/// person typed: the short phrases the table holds (`quota`, `credit`) are
/// ordinary words in a diff, and a reviewer reads diffs. A pane whose agent is
/// still working is not read at all: a CLI that hit its limit has stopped.
pub fn spent_pane(patterns: &[String], screen: &str, working: bool, now_ms: u64) -> Option<Spent> {
    if working {
        return None;
    }
    let lines: Vec<&str> = screen
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let tail: Vec<&str> = lines[lines
        .len()
        .saturating_sub(shprd_watch::screen::VERDICT_LINES)..]
        .iter()
        .copied()
        .filter(|line| !line.starts_with("$ ") && !line.starts_with("> "))
        .collect();
    let text = tail.join("\n");
    let phrase = shprd_agents::exhausted::exhausted_pane(patterns, &text)?;
    let lowered = phrase.to_ascii_lowercase();
    let rate = lowered.contains("rate") || lowered.contains("429") || lowered.contains("many");
    // A message whose printed reset has passed is a stale screen, not a spent
    // account: it must not start a new cooldown.
    let printed = shprd_agents::exhausted::reset_at_ms(&text);
    if printed.is_some_and(|at| at <= now_ms) {
        return None;
    }
    let reset_at_ms = printed;
    let (state, cooldown) = if rate {
        ("rate-limited", RATE_COOLDOWN_MS)
    } else {
        ("out of quota", QUOTA_COOLDOWN_MS)
    };
    Some(Spent {
        reason: format!("{state}: {phrase}"),
        reset_at_ms: reset_at_ms.or(Some(now_ms + cooldown)),
    })
}

/// One pass over the reviewers that are out: a reviewer whose account is spent
/// is stopped, its program is remembered as dry, and the review starts again on
/// the next reviewer in the settings list, with no question to anyone.
///
/// A task with no reviewer left is reported so the caller puts one plain card in
/// front of the person; everything else a pass finds is left alone.
pub fn switch_exhausted(
    mux: &dyn Host,
    ctx: &Ctx,
    home: &Path,
) -> Vec<shprd_core::review::ReviewSwitch> {
    use shprd_core::review::ReviewSwitch;
    let mut out = Vec::new();
    let Ok(state) = ctx.state() else {
        return out;
    };
    let now_ms = shprd_core::log::Clock::now_ms(&shprd_core::log::SystemClock);
    let waiting: Vec<(String, u32, String)> = state
        .tasks
        .values()
        .filter_map(|task| {
            let attempt = shprd_core::tick::awaiting_verdict(task)?;
            let request = task.latest_attempt()?.reviews.last()?;
            Some((task.id.clone(), attempt, request.reviewer.clone()))
        })
        .collect();
    if waiting.is_empty() {
        return out;
    }
    let Ok(env) = ReviewEnv::build(ctx, Some(home)) else {
        return out;
    };
    for (task, attempt, reviewer) in waiting {
        // A verdict on its way is not a reviewer to move.
        let findings = findings_path(&env.reviews_dir, &env.spawn.run, &task, attempt);
        if findings.exists() {
            continue;
        }
        let Ok(tab) = std::fs::read_to_string(tab_record_path(
            &env.spawn.briefs_dir,
            &env.spawn.run,
            &task,
            attempt,
        )) else {
            continue;
        };
        let Ok(Some(pane)) = crate::cmd::watch::pane_for(mux, &env.spawn, tab.trim()) else {
            continue;
        };
        let (Ok(screen), Ok(agent_state)) = (
            mux.read(&pane, shprd_watch::screen::WINDOW_LINES),
            mux.agent_state(&pane),
        ) else {
            continue;
        };
        let patterns = env
            .spawn
            .programs
            .iter()
            .find(|program| program.spec.id == reviewer)
            .map(|program| program.spec.dry_patterns.clone())
            .unwrap_or_default();
        let working = agent_state == shprd_mux::AgentState::Working;
        let Some(spent) = spent_pane(&patterns, &screen, working, now_ms) else {
            continue;
        };
        let already = state
            .agents
            .get(&reviewer)
            .is_some_and(|info| info.dry && info.dry_until_ms.is_none_or(|until| until > now_ms));
        if !already
            && let Err(err) = ctx.commit(EventKind::Dry {
                agent: reviewer.clone(),
                reason: spent.reason.clone(),
                reset_at_ms: spent.reset_at_ms,
            })
        {
            eprintln!("warning: could not record that {reviewer} is dry: {err}");
            continue;
        }
        match start_review(ctx, home, &task, true) {
            Ok(_) => {
                let line = ctx
                    .state()
                    .ok()
                    .and_then(|state| {
                        state
                            .tasks
                            .get(&task)?
                            .latest_attempt()?
                            .reviews
                            .last()?
                            .note
                            .clone()
                    })
                    .unwrap_or_else(|| format!("{reviewer} is out of quota; the review moved on"));
                out.push(ReviewSwitch::Switched { task, line });
            }
            Err(message) if message.contains(NO_REVIEWER_LEFT) => {
                let why = message
                    .split_once(": ")
                    .map_or(message.as_str(), |(_, rest)| rest)
                    .to_owned();
                out.push(ReviewSwitch::NoneLeft { task, attempt, why });
            }
            Err(message) => eprintln!("warning: could not move {task}'s review: {message}"),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    #[test]
    fn proof_files_are_copied_into_the_packet() {
        let dir = TempDir::new().unwrap();
        let tree = dir.path().join("wt");
        std::fs::create_dir_all(tree.join(".shprd/proof")).unwrap();
        std::fs::write(tree.join(".shprd/proof/run.txt"), "ok 9 passed").unwrap();
        let packet = dir.path().join("packet");
        std::fs::create_dir(&packet).unwrap();
        let copies = copy_proofs(
            &tree,
            &[
                ".shprd/proof/run.txt".to_string(),
                ".shprd/proof/missing.txt".to_string(),
                "../outside.txt".to_string(),
            ],
            &packet,
        );
        let first = copies[0].1.clone().expect("the first proof is copied");
        assert_eq!(
            std::fs::read_to_string(packet.join(&first)).unwrap(),
            "ok 9 passed"
        );
        assert_eq!(copies[1].1, None, "a missing file is reported");
        assert_eq!(copies[2].1, None, "a path outside the worktree is refused");
        let text = proof_section(&copies);
        assert!(text.contains(&format!("`.shprd/proof/run.txt` is at `{first}`")));
        assert!(text.contains("`.shprd/proof/missing.txt` could not be copied"));
    }

    #[test]
    fn a_packet_folder_is_named_by_where_it_really_is() {
        let dir = TempDir::new().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(real_dir(&link), real.canonicalize().unwrap());
        // A folder that is not there yet is left as it was named.
        let missing = dir.path().join("missing");
        assert_eq!(real_dir(&missing), missing);
    }

    use std::collections::BTreeMap;
    use std::process::Command;
    use std::time::{Duration, SystemTime};

    use shprd_core::config::{ConfigLayer, Skills};
    use shprd_core::event::{Actor, ReviewSource};
    use shprd_core::settings::{File, Loaded, Source as LayerSource};
    use shprd_git::worktree::Layout;
    use shprd_mux::fake::{Call, FakeMux};
    use shprd_mux::{AgentState, PaneId};
    use tempfile::TempDir;

    use super::*;

    /// Runs one git command in `dir`, failing the test when it does.
    fn git(dir: &Path, args: &[&str]) {
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
    }

    /// A harness spec for a fake CLI from `vendor`: the tests record the launch,
    /// never run it, so `interactive` is the harmless `sh`.
    fn fake_spec(id: &str, vendor: &str) -> HarnessSpec {
        fake_model_spec(id, vendor, false, false)
    }

    /// A fake CLI that takes its brief as a system prompt, the way pi and
    /// claude do: the brief arrives as a file, so a review must still deliver a
    /// first turn or the CLI sits idle.
    fn fake_prompt_spec(id: &str, vendor: &str) -> HarnessSpec {
        fake_spec_text(
            id,
            vendor,
            false,
            false,
            "system_prompt_args = [\"--system-prompt\", \"{prompt_file}\"]\n",
        )
    }

    /// The reviewer role a test builds from an old one-word agent string.
    fn reviewer_role(reviewer: Option<&str>) -> ReviewerRole {
        reviewer
            .map(|written| ReviewerRole::written(written, None))
            .unwrap_or_default()
    }

    /// The reviewer role a settings document resolves to.
    fn settings_reviewer(text: &str) -> ReviewerRole {
        let mut loaded = Loaded::default();
        loaded.file = File::parse(text).unwrap_or_else(|err| panic!("{err}"));
        ReviewerRole::resolved(
            &loaded
                .resolve(None)
                .role(REVIEWER_ROLE)
                .unwrap_or_else(|| panic!("the reviewer role is missing")),
        )
    }

    /// The reviewer role a pre-M9 `config.toml` resolves to.
    fn legacy_reviewer(config: &str) -> ReviewerRole {
        let layer: ConfigLayer = toml::from_str(config).unwrap_or_else(|err| panic!("{err}"));
        let mut loaded = Loaded::default();
        loaded.file.legacy = layer.settings_projection();
        ReviewerRole::resolved(
            &loaded
                .resolve(None)
                .role(REVIEWER_ROLE)
                .unwrap_or_else(|| panic!("the reviewer role is missing")),
        )
    }

    /// A fake CLI that runs other vendors' models, when `multi_model` says so,
    /// and wants the provider in the model name when `provider_qualified` does.
    fn fake_model_spec(
        id: &str,
        vendor: &str,
        multi_model: bool,
        provider_qualified: bool,
    ) -> HarnessSpec {
        fake_spec_text(id, vendor, multi_model, provider_qualified, "")
    }

    /// One fake CLI spec, with `launch_extra` added to its `[launch]` block.
    fn fake_spec_text(
        id: &str,
        vendor: &str,
        multi_model: bool,
        provider_qualified: bool,
        launch_extra: &str,
    ) -> HarnessSpec {
        let text = format!(
            r#"
id = "{id}"
name = "Fake {id}"
vendor = "{vendor}"
multi_model = {multi_model}
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
effort_args = ["--effort", "{{effort}}"]
{launch_extra}verified = false

[permissions]
default = "manual"
full = "full"
readonly = "read-only"
modes = [
  {{ id = "manual", label = "Manual", args = [] }},
  {{ id = "read-only", label = "Read only", args = ["--mode", "read-only"] }},
  {{ id = "full", label = "Full", args = ["--mode", "full"] }},
]

[models]
provider_qualified = {provider_qualified}

[records]
kind = "jsonl"
location = "~/.fake/{{session}}.jsonl"
errors = "fake"
verified = true

[quota]
source = "none"

[skills]
home_dirs = []
project_dirs = []
"#
        );
        toml::from_str::<HarnessSpec>(&text)
            .unwrap_or_else(|err| panic!("the test spec is invalid: {err}"))
    }

    /// A repository with the integration branch, a task branch one commit ahead
    /// of it, a started run and one completed attempt, plus a home directory.
    struct Scratch {
        repo: TempDir,
        home: TempDir,
    }

    impl Scratch {
        fn new() -> Scratch {
            let repo = TempDir::new().unwrap_or_else(|err| panic!("{err}"));
            let home = TempDir::new().unwrap_or_else(|err| panic!("{err}"));
            let root = repo.path();
            git(root, &["init", "-q", "-b", "main"]);
            std::fs::write(root.join("README.md"), "scratch\n").unwrap();
            git(root, &["add", "."]);
            git(root, &["commit", "-q", "-m", "first"]);
            git(root, &["branch", "integration/m4"]);
            git(root, &["checkout", "-q", "-b", "feature/t1"]);
            std::fs::write(root.join("README.md"), "scratch\nand the change\n").unwrap();
            git(root, &["commit", "-q", "-a", "-m", "t1 work"]);
            git(root, &["checkout", "-q", "main"]);
            Scratch { repo, home }
        }

        fn root(&self) -> &Path {
            self.repo.path()
        }

        /// The repo's directory name, which the workspace name uses.
        fn repo_name(&self) -> String {
            self.root()
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_default()
        }

        /// A context with a started run and one completed attempt at `T1`.
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
                title: "Make review real".to_string(),
                scope: vec!["README.md".to_string()],
                acceptance: vec!["the diff is reviewed".to_string()],
                tags: Vec::new(),
                skills: Vec::new(),
                deps: Vec::new(),
            })
            .unwrap_or_else(|err| panic!("{err}"));
            ctx
        }

        /// Records the attempt `git` describes: `sh` worked on `feature/t1`.
        fn reported(&self, ctx: &Ctx) {
            self.reported_as(ctx, "sh");
        }

        /// Records one completed attempt whose worker was `agent`.
        fn reported_as(&self, ctx: &Ctx, agent: &str) {
            self.reported_with(ctx, agent, None);
        }

        /// Records one completed attempt whose worker ran `model`.
        fn reported_with(&self, ctx: &Ctx, agent: &str, model: Option<&str>) {
            ctx.commit(EventKind::Spawned {
                task: "T1".to_string(),
                attempt: 1,
                agent: agent.to_string(),
                model: model.map(str::to_owned),
                worktree: "/tmp/wt/T1".to_string(),
                branch: "feature/t1".to_string(),
            })
            .unwrap_or_else(|err| panic!("{err}"));
            ctx.commit(EventKind::Reported {
                task: "T1".to_string(),
                attempt: 1,
                status: ReportStatus::Completed,
                summary: "made it real".to_string(),
                tests: vec!["cargo test".to_string()],
                checks: vec![shprd_core::event::CheckReport::passed(
                    "tests",
                    "cargo test",
                )],
                changed_paths: vec!["README.md".to_string()],
                commit: None,
                skills_used: Vec::new(),
                proofs: Vec::new(),
            })
            .unwrap_or_else(|err| panic!("{err}"));
        }

        /// The environment a review runs in, with three fake CLIs: two from one
        /// vendor and one from another.
        fn review_env<'a>(&self, ctx: &'a Ctx, reviewer: Option<&str>) -> ReviewEnv<'a> {
            self.review_env_with(
                ctx,
                reviewer,
                vec![
                    Program {
                        spec: fake_spec("sh", "pi"),
                        installed: true,
                    },
                    Program {
                        spec: fake_spec("pi", "pi"),
                        installed: true,
                    },
                    Program {
                        spec: fake_spec("codex", "openai"),
                        installed: true,
                    },
                ],
            )
        }

        /// The environment a review runs in, with the programs it should see.
        fn review_env_with<'a>(
            &self,
            ctx: &'a Ctx,
            reviewer: Option<&str>,
            programs: Vec<Program>,
        ) -> ReviewEnv<'a> {
            let mut base_env = BTreeMap::new();
            base_env.insert("GH_TOKEN".to_string(), "secret".to_string());
            base_env.insert("PATH".to_string(), "/usr/bin".to_string());
            let transcript_roots = shprd_agents::transcript::Roots::for_launch(&base_env);
            ReviewEnv {
                spawn: SpawnEnv {
                    ctx,
                    layout: Layout::default_for_home(self.home.path()),
                    briefs_dir: self.home.path().join(".shprd").join("briefs"),
                    prompts_dir: shprd_core::prompts::prompts_dir(
                        &self.home.path().join(".config"),
                    ),
                    home: self.home.path().to_path_buf(),
                    settings: shprd_core::settings::Loaded::default(),
                    run: "t1".to_string(),
                    base_branch: "integration/m4".to_string(),
                    repo_name: self.repo_name(),
                    programs,
                    workers: vec!["sh".to_string()],
                    skills: Skills::default(),
                    gates: Vec::new(),
                    catalogs: std::collections::BTreeMap::new(),
                    data_dir: shprd_agents::data::DataDir::resolve(Some(self.home.path()), None),
                    compat: Vec::new(),
                    base_env,
                    // No wait: a fake pane that never reports an agent would
                    // otherwise stall every test.
                    ready_wait: Duration::ZERO,
                    transcript_roots,
                },
                reviews_dir: self.home.path().join(".shprd").join("reviews"),
                reviewer: reviewer_role(reviewer),
                // No wait before the retry either, so the two delivery tests run
                // through the same path without sleeping.
                brief_retry_wait: Duration::ZERO,
            }
        }

        /// A fake host that already has this repository's coordinator space.
        ///
        /// The reviewer is headless (no tab, no space of its own), so the fixture
        /// only seeds what a person would have opened.
        fn mux(&self) -> FakeMux {
            self.with_workspace(FakeMux::default().with_ready_agent())
        }

        /// A fake host with this repository's workspace that never detects an
        /// agent, so a brief typed into it cannot arrive until the test
        /// scripts the pane as ready.
        fn unready_mux(&self) -> FakeMux {
            self.with_workspace(FakeMux::default())
        }

        /// Records this repository's workspace in `mux`, as a launcher would.
        fn with_workspace(&self, mux: FakeMux) -> FakeMux {
            mux.create_space(&spawn::repo_space_name(&self.repo_name()), self.root())
                .unwrap_or_else(|err| panic!("{err}"));
            mux
        }
    }

    /// The pane id a review's reviewer tab gets as its root pane.
    ///
    /// Fake host ids are deterministic and the tests' workspace already exists,
    /// so the first tab a review creates is the reviewer's and its root pane is
    /// `pane-1`. Tests script state for this pane, and then assert the brief
    /// landed in it, so a wrong guess fails loudly rather than silently.
    fn reviewer_pane() -> PaneId {
        PaneId::new("pane-1")
    }

    /// Every `Escalated` event in the run log, as (task, body, default action).
    fn escalations(ctx: &Ctx) -> Vec<(Option<String>, String, String)> {
        let (_, events) = ctx.load().unwrap_or_else(|err| panic!("{err}"));
        events
            .into_iter()
            .filter_map(|event| match event.kind {
                EventKind::Escalated {
                    task,
                    body,
                    default_action,
                } => Some((task, body, default_action)),
                _ => None,
            })
            .collect()
    }

    /// The text of every prompt submitted to any pane, oldest first.
    fn prompts(mux: &FakeMux) -> Vec<String> {
        mux.calls()
            .into_iter()
            .filter_map(|call| match call {
                Call::Write(_, text, _) => Some(text),
                _ => None,
            })
            .collect()
    }

    /// How often the host was asked whether the reviewer's pane is an agent,
    /// which is one question per brief delivery attempt.
    fn detection_polls(mux: &FakeMux) -> usize {
        mux.calls()
            .into_iter()
            .filter(|call| matches!(call, Call::AgentState(_)))
            .count()
    }

    /// A review request for `T1`.
    fn request(degraded: bool) -> ReviewRequest {
        ReviewRequest {
            task: "T1".to_string(),
            degraded,
            again: false,
        }
    }

    #[test]
    fn the_packet_carries_the_real_diff_and_the_contract() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        scratch.reported(&ctx);
        let env = scratch.review_env(&ctx, Some("codex/m1"));
        let mux = scratch.mux();

        let report = review(&mux, &env, &request(false)).unwrap_or_else(|err| panic!("{err}"));

        assert_eq!(report.task, "T1");
        assert_eq!(report.attempt, 1);
        assert_eq!(report.agent, "codex");
        assert_eq!(report.model.as_deref(), Some("m1"));
        assert!(!report.degraded);
        assert_eq!(report.dir, packet_dir(&env.reviews_dir, "t1", "T1", 1));
        assert_eq!(
            report.brief,
            brief_path(&env.spawn.briefs_dir, "t1", "T1", 1)
        );

        // The diff is the task branch's own change, taken from its merge base.
        let diff = std::fs::read_to_string(report.dir.join("diff.patch"))
            .unwrap_or_else(|err| panic!("{err}"));
        assert!(diff.starts_with("diff --git"), "{diff}");
        assert!(diff.contains("+and the change"), "{diff}");

        let contract = std::fs::read_to_string(report.dir.join("contract.md"))
            .unwrap_or_else(|err| panic!("{err}"));
        assert!(contract.contains("Make review real"), "{contract}");
        assert!(contract.contains("the diff is reviewed"), "{contract}");
        assert!(contract.contains("README.md"), "{contract}");
        assert!(contract.contains("made it real"), "{contract}");

        let schema: Value = serde_json::from_str(
            &std::fs::read_to_string(report.dir.join("findings.schema.json"))
                .unwrap_or_else(|err| panic!("{err}")),
        )
        .unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(
            schema["items"]["properties"]["severity"]["enum"][0],
            "critical"
        );

        let brief = std::fs::read_to_string(&report.brief).unwrap_or_else(|err| panic!("{err}"));
        // The reviewer's own words lead the brief: the shipped reviewer prompt
        // is installed on first use and read in.
        assert!(
            brief.starts_with("You are a shprd reviewer") && !brief.contains("<!--"),
            "{brief}"
        );
        assert!(
            brief.contains("You review one finished attempt's work"),
            "{brief}"
        );
        // The reviewer writes and stops: no shprd verb is asked of it, because
        // its folder is outside the repository and the engine records the
        // verdict.
        assert!(!brief.contains("shprd agent"), "{brief}");
        assert!(
            brief.contains("Write `findings.json` in this folder once"),
            "{brief}"
        );
        assert!(brief.contains("# REVIEW "), "{brief}");
        assert!(!brief.contains(DEGRADED_LABEL), "{brief}");
    }

    /// The contract for a task with one completed attempt, as text.
    ///
    /// It goes through the real log: the asks are folded events, so a test that
    /// built them by hand would not prove the packet reads what the run
    /// recorded.
    fn contract_text(scratch: &Scratch, answers: &[(&str, Option<&str>)]) -> String {
        let ctx = scratch.ctx();
        ctx.commit(EventKind::Spawned {
            task: "T1".to_string(),
            attempt: 1,
            agent: "sh".to_string(),
            model: None,
            worktree: "/tmp/wt/T1".to_string(),
            branch: "feature/t1".to_string(),
        })
        .unwrap_or_else(|err| panic!("{err}"));
        for (n, (ask, answer)) in answers.iter().enumerate() {
            ctx.commit(EventKind::AskRaised {
                task: "T1".to_string(),
                ask: (*ask).to_string(),
                ask_kind: shprd_core::ask::AskKind::Question,
                text: format!("question {n}"),
                options: vec!["add".to_string(), "sum".to_string()],
                created_at_ms: 1_000,
                expires_at_ms: None,
            })
            .unwrap_or_else(|err| panic!("{err}"));
            if let Some(answer) = answer {
                ctx.commit(EventKind::AskAnswered {
                    task: "T1".to_string(),
                    ask: (*ask).to_string(),
                    answer: (*answer).to_string(),
                })
                .unwrap_or_else(|err| panic!("{err}"));
            }
        }
        ctx.commit(EventKind::Reported {
            task: "T1".to_string(),
            attempt: 1,
            status: ReportStatus::Completed,
            summary: "made it real".to_string(),
            tests: vec!["cargo test".to_string()],
            checks: vec![shprd_core::event::CheckReport::passed(
                "tests",
                "cargo test",
            )],
            changed_paths: vec!["README.md".to_string()],
            commit: None,
            skills_used: Vec::new(),
            proofs: vec![".shprd/proof/test-run.txt".to_string()],
        })
        .unwrap_or_else(|err| panic!("{err}"));
        let state = ctx.state().unwrap_or_else(|err| panic!("{err}"));
        let task = spawn::task_of(&state, "T1").unwrap_or_else(|err| panic!("{err}"));
        let asks: Vec<(TaskId, AskId, AskRecord)> = state
            .asks
            .iter()
            .map(|((task, ask), record)| (task.clone(), ask.clone(), record.clone()))
            .collect();
        render_contract(&ContractInput {
            task,
            asks: &asks,
            proof_copies: &[],
            protected: &[],
        })
    }

    #[test]
    fn a_contract_for_work_outside_the_declared_paths_calls_them_a_hint() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        scratch.reported(&ctx);
        let state = ctx.state().unwrap_or_else(|err| panic!("{err}"));
        let mut task = state.tasks["T1"].clone();
        // The attempt changed README.md; the task declared only docs.
        task.scope = vec!["docs".to_string()];
        let contract = render_contract(&ContractInput {
            task: &task,
            asks: &[],
            proof_copies: &[],
            protected: &[],
        });
        assert!(
            contract.contains("## Where the task was expected to work (a hint, not a limit)"),
            "{contract}"
        );
        assert!(contract.contains("README.md"), "{contract}");
        for word in [
            "outside scope",
            "outside the scope",
            "only paths",
            "Also allowed",
        ] {
            assert!(!contract.contains(word), "{word:?} in {contract}");
        }
        assert!(!contract.contains("Touches protected paths"), "{contract}");
        let flagged = render_contract(&ContractInput {
            task: &task,
            asks: &[],
            proof_copies: &[],
            protected: &["AGENTS.md".to_string()],
        });
        assert!(
            flagged.contains("Touches protected paths: AGENTS.md. The task did not name them"),
            "{flagged}"
        );
    }

    #[test]
    fn the_contract_carries_the_attempts_asks_and_their_answers() {
        let scratch = Scratch::new();
        let contract = contract_text(&scratch, &[("c1", Some("add"))]);
        let snapshot = "\n\
## Asks raised during this attempt

- question `c1`: question 0
  - options: add, sum
  - answer: add

## What the agent reported
";
        let start = contract
            .find("\n## Asks raised during this attempt")
            .unwrap_or_else(|| panic!("the packet carries the asks: {contract}"));
        assert!(
            contract[start..].starts_with(snapshot),
            "the ask section is the answered card: {contract}"
        );
        assert!(
            contract.contains("- proof:\n  - .shprd/proof/test-run.txt\n"),
            "the proof is in the packet: {contract}"
        );
    }

    /// An ask record the way the fold holds it.
    fn ask_record(id: &str, seq: u64, answer: Option<&str>) -> AskRecord {
        AskRecord {
            id: id.to_string(),
            task: "T1".to_string(),
            kind: shprd_core::ask::AskKind::Question,
            text: format!("text of {id}"),
            options: vec!["yes".to_string()],
            created_at_ms: 1_000,
            expires_at_ms: None,
            raised_seq: seq,
            answer: answer.map(|text| shprd_core::state::AskAnswerRecord {
                text: text.to_string(),
                seq: seq + 1,
            }),
        }
    }

    #[test]
    fn a_later_attempts_contract_carries_asks_answered_in_an_earlier_one() {
        let attempt = Attempt {
            n: 2,
            agent: "claude".to_string(),
            model: None,
            worktree: "/wt".to_string(),
            branch: "b".to_string(),
            report: None,
            gates: Vec::new(),
            reviews: Vec::new(),
            spawn_seq: Some(50),
            stopped: false,
        };
        let asks: Vec<(TaskId, AskId, AskRecord)> = vec![
            ("dictation", Some("use mic")),
            ("scope-widen-1", Some("keep")),
            ("hand-in-refused-2-1", None),
        ]
        .into_iter()
        .zip([10u64, 60, 70])
        .map(|((id, answer), seq)| {
            (
                "T1".to_string(),
                id.to_string(),
                ask_record(id, seq, answer),
            )
        })
        .collect();
        let mut asks = asks;
        asks.push((
            "T2".to_string(),
            "foreign".to_string(),
            ask_record("foreign", 11, Some("no")),
        ));
        asks.push((
            "T1".to_string(),
            "never-answered".to_string(),
            ask_record("never-answered", 12, None),
        ));
        let text = asks_section("T1", Some(&attempt), &asks);
        let earlier = text
            .find("## Asks raised in earlier attempts")
            .unwrap_or_else(|| panic!("an earlier group: {text}"));
        let cards = text
            .find("## Cards the engine raised")
            .unwrap_or_else(|| panic!("a cards group: {text}"));
        assert!(
            text.contains("- none: this attempt asked the person nothing"),
            "{text}"
        );
        let earlier_part = &text[earlier..cards];
        assert!(
            earlier_part.contains("`dictation`") && earlier_part.contains("answer: use mic"),
            "{text}"
        );
        let cards_part = &text[cards..];
        assert!(
            cards_part.contains("`scope-widen-1`") && cards_part.contains("`hand-in-refused-2-1`"),
            "{text}"
        );
        assert!(!earlier_part.contains("scope-widen"), "{text}");
        assert!(!text.contains("foreign"), "{text}");
        assert!(!text.contains("never-answered"), "{text}");
    }

    #[test]
    fn the_contract_says_plainly_when_an_ask_is_unanswered() {
        let scratch = Scratch::new();
        let contract = contract_text(&scratch, &[("c1", None)]);
        assert!(
            contract.contains(
                "- question `c1`: question 0\n  - options: add, sum\n  - answer: unanswered\n"
            ),
            "{contract}"
        );
    }

    #[test]
    fn the_contract_says_so_when_the_attempt_asked_nothing() {
        let scratch = Scratch::new();
        let contract = contract_text(&scratch, &[]);
        assert!(
            contract.contains(
                "## Asks raised during this attempt\n\n- none: this attempt asked the person nothing\n"
            ),
            "{contract}"
        );
    }

    /// The packet on disk, not just the renderer: `write_packet` reads the fold,
    /// so a card raised and answered between the attempt's spawn and its report
    /// reaches the reviewer.
    #[test]
    fn the_written_packet_carries_the_attempts_own_ask() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        ctx.commit(EventKind::Spawned {
            task: "T1".to_string(),
            attempt: 1,
            agent: "sh".to_string(),
            model: None,
            worktree: "/tmp/wt/T1".to_string(),
            branch: "feature/t1".to_string(),
        })
        .unwrap_or_else(|err| panic!("{err}"));
        let raised = ctx
            .commit(EventKind::AskRaised {
                task: "T1".to_string(),
                ask: "c1".to_string(),
                ask_kind: shprd_core::ask::AskKind::Question,
                text: "which name?".to_string(),
                options: vec!["add".to_string(), "sum".to_string()],
                created_at_ms: 1_000,
                expires_at_ms: None,
            })
            .unwrap_or_else(|err| panic!("{err}"));
        assert!(raised.seq > 0, "the ask is in the log");
        ctx.commit(EventKind::Reported {
            task: "T1".to_string(),
            attempt: 1,
            status: ReportStatus::Completed,
            summary: "made it real".to_string(),
            tests: vec!["cargo test".to_string()],
            checks: vec![shprd_core::event::CheckReport::passed(
                "tests",
                "cargo test",
            )],
            changed_paths: vec!["README.md".to_string()],
            commit: None,
            skills_used: Vec::new(),
            proofs: Vec::new(),
        })
        .unwrap_or_else(|err| panic!("{err}"));
        let env = scratch.review_env(&ctx, Some("codex/m1"));

        let report =
            review(&scratch.mux(), &env, &request(false)).unwrap_or_else(|err| panic!("{err}"));
        let contract = std::fs::read_to_string(report.dir.join("contract.md"))
            .unwrap_or_else(|err| panic!("{err}"));
        assert!(
            contract.contains(
                "## Asks raised during this attempt\n\n- question `c1`: which name?\n  - options: add, sum\n  - answer: unanswered\n"
            ),
            "the packet on disk carries the ask: {contract}"
        );
    }

    #[test]
    fn a_reviewer_from_the_authors_vendor_is_refused() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        scratch.reported(&ctx);
        // `pi` and `sh` are both single-vendor here and both read as `pi`, so
        // neither can review the other's work: the rule is about the model's
        // family, not the program.
        let env = scratch.review_env(&ctx, Some("pi/m1"));
        let mux = scratch.mux();

        let err = review(&mux, &env, &request(false)).unwrap_err();

        assert_eq!(err.exit_code(), 2);
        let message = format!("{err}");
        assert!(
            message.contains("both run models from the pi family"),
            "{message}"
        );
        assert!(message.contains("--degraded"), "{message}");
        // Nothing was written and no tab was opened.
        assert!(!env.reviews_dir.exists(), "no packet may be written");
        assert!(
            mux.calls()
                .iter()
                .all(|call| !matches!(call, Call::Spawn(..))),
            "no tab may be opened"
        );
    }

    /// The programs a family-aware review sees: pi and opencode run other
    /// vendors' models, kilo is a free router, claude and codex are
    /// single-vendor.
    fn family_programs() -> Vec<Program> {
        vec![
            Program {
                spec: fake_model_spec("pi", "pi", true, true),
                installed: true,
            },
            Program {
                spec: fake_model_spec("kilo", "kilo", true, false),
                installed: true,
            },
            Program {
                spec: fake_spec("opencode", "opencode"),
                installed: true,
            },
            Program {
                spec: fake_spec("claude", "anthropic"),
                installed: true,
            },
            Program {
                spec: fake_spec("codex", "openai"),
                installed: true,
            },
        ]
    }

    #[test]
    fn a_pi_worker_on_deepseek_is_reviewed_by_a_pi_reviewer_on_claude() {
        // Same CLI, two model families: this is cross-vendor review, and must
        // not be refused just because `pi` wrote it and `pi` reviews it.
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        scratch.reported_with(&ctx, "pi", Some("deepseek/deepseek-v4.1-flash"));
        let env =
            scratch.review_env_with(&ctx, Some("pi/claude/claude-opus-5-5"), family_programs());
        let mux = scratch.mux();

        let report = review(&mux, &env, &request(false)).unwrap_or_else(|err| panic!("{err}"));

        assert!(!report.degraded, "the review is cross-vendor");
        assert_eq!(report.agent, "pi");
        assert!(report.dir.is_dir(), "the packet was written");
    }

    #[test]
    fn a_pi_worker_on_claude_cannot_be_reviewed_by_claude_code() {
        // Two CLIs, one model family: this is not cross-vendor review, so it is
        // refused rather than passed.
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        scratch.reported_with(&ctx, "pi", Some("claude/claude-opus-5-5"));
        let env = scratch.review_env_with(&ctx, Some("claude/claude-opus-5-5"), family_programs());
        let mux = scratch.mux();

        let err = review(&mux, &env, &request(false)).unwrap_err();

        assert_eq!(err.exit_code(), 2);
        let message = format!("{err}");
        assert!(
            message.contains("both run models from the anthropic family"),
            "{message}"
        );
        assert!(message.contains("--degraded"), "{message}");
        assert!(!env.reviews_dir.exists(), "no packet may be written");
    }

    #[test]
    fn turning_cross_vendor_off_lets_a_same_vendor_review_run_unlabelled() {
        // The same pair `a_pi_worker_on_claude_cannot_be_reviewed_by_claude_code`
        // refuses: with `review.cross_vendor = false` it runs, and is not labelled.
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        scratch.reported_with(&ctx, "pi", Some("claude/claude-opus-5-5"));
        let mut env =
            scratch.review_env_with(&ctx, Some("claude/claude-opus-5-5"), family_programs());
        env.reviewer.same_vendor_ok = true;
        let mux = scratch.mux();

        let report = review(&mux, &env, &request(false)).unwrap_or_else(|err| panic!("{err}"));

        assert!(!report.degraded, "nothing is labelled when the rule is off");
        assert!(report.dir.is_dir(), "the packet was written");
    }

    #[test]
    fn the_cross_vendor_setting_reaches_the_review_environment() {
        // `ReviewEnv::build` reads the switch from the settings, so the verb that
        // runs the review honours it without a restart.
        let home = tempfile::tempdir().unwrap_or_else(|err| panic!("{err}"));
        let repo = tempfile::tempdir().unwrap_or_else(|err| panic!("{err}"));
        let write = |text: &str| {
            let dir = home.path().join(".config").join("shprd");
            std::fs::create_dir_all(&dir).unwrap_or_else(|err| panic!("{err}"));
            std::fs::write(dir.join("settings.toml"), text).unwrap_or_else(|err| panic!("{err}"));
        };
        let ctx = Ctx::new(repo.path().to_path_buf(), Actor::Human, false, true);
        ctx.commit(EventKind::RunStarted {
            run_id: "t1".to_string(),
            repo: "r".to_string(),
            base_branch: "main".to_string(),
            integration_branch: "main".to_string(),
        })
        .unwrap_or_else(|err| panic!("{err}"));
        write("version = 1\n");
        let on = ReviewEnv::build(&ctx, Some(home.path())).unwrap_or_else(|err| panic!("{err}"));
        assert!(!on.reviewer.same_vendor_ok);
        write("version = 1\n[review]\ncross_vendor = false\n");
        let off = ReviewEnv::build(&ctx, Some(home.path())).unwrap_or_else(|err| panic!("{err}"));
        assert!(off.reviewer.same_vendor_ok);
    }

    #[test]
    fn a_worker_on_a_router_is_never_assumed_independent() {
        // `kilo/kilo-auto/free` names no model family, so nothing proves the
        // reviewer differs. A wrong-but-passing review is worse than a
        // degraded one, so the review is refused unless it may be labelled.
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        scratch.reported_with(&ctx, "kilo", Some("kilo-auto/free"));
        let env = scratch.review_env_with(&ctx, Some("codex/gpt-5-codex"), family_programs());
        let mux = scratch.mux();

        let err = review(&mux, &env, &request(false)).unwrap_err();

        assert_eq!(err.exit_code(), 2);
        let message = format!("{err}");
        assert!(
            message.contains(
                "cannot prove the reviewer is a different vendor: kilo/kilo-auto/free \
                 has no known model family"
            ),
            "{message}"
        );

        let degraded = review(&mux, &env, &request(true)).unwrap_or_else(|err| panic!("{err}"));
        assert!(degraded.degraded, "a labelled review is allowed");
    }

    #[test]
    fn a_degraded_review_is_allowed_and_says_so() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        scratch.reported(&ctx);
        let env = scratch.review_env(&ctx, Some("pi/m1"));
        let mux = scratch.mux();

        let report = review(&mux, &env, &request(true)).unwrap_or_else(|err| panic!("{err}"));

        assert!(report.degraded);
        let brief = std::fs::read_to_string(&report.brief).unwrap_or_else(|err| panic!("{err}"));
        assert!(brief.contains(DEGRADED_LABEL), "{brief}");
        assert!(report.dir.is_dir(), "the packet was written anyway");
        // The tab record names the tab the review opened, beside the brief.
        let recorded = tab_record_path(&env.spawn.briefs_dir, "t1", "T1", 1);
        let tab_id = std::fs::read_to_string(&recorded).unwrap_or_else(|err| panic!("{err}"));
        assert!(!tab_id.trim().is_empty(), "the record names no tab");
        assert!(
            mux.calls().iter().any(|call| matches!(
                call,
                Call::Spawn(_, title, _, _, _) if title == &report.tab
            )),
            "no tab titled {:?} was opened",
            report.tab
        );
    }

    #[test]
    fn the_reviewer_starts_in_the_real_path_of_a_symlinked_packet() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        scratch.reported(&ctx);
        let mut env = scratch.review_env(&ctx, Some("codex/m1"));
        let real = scratch.home.path().join("real-reviews");
        std::fs::create_dir_all(&real).unwrap();
        let link = scratch.home.path().join("link-reviews");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        env.reviews_dir = link;
        let mux = scratch.mux();

        let report = review(&mux, &env, &request(false)).unwrap_or_else(|err| panic!("{err}"));

        let cwd = mux
            .calls()
            .iter()
            .find_map(|call| match call {
                Call::Spawn(_, title, _, _, cwd) if title == &report.tab => Some(cwd.clone()),
                _ => None,
            })
            .expect("the reviewer was spawned");
        assert_eq!(cwd, report.dir.canonicalize().unwrap());
        assert!(cwd.starts_with(real.canonicalize().unwrap()), "{cwd:?}");
    }

    #[test]
    fn the_reviewer_tab_starts_in_the_packet_not_the_worktree() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        scratch.reported(&ctx);
        let env = scratch.review_env(&ctx, Some("codex/m1"));
        let mux = scratch.mux();

        let report = review(&mux, &env, &request(false)).unwrap_or_else(|err| panic!("{err}"));

        let calls = mux.calls();
        let (title, cwd) = calls
            .iter()
            .find_map(|call| match call {
                Call::Spawn(_, title, _, _, cwd) => Some((title.clone(), cwd.clone())),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no tab was created: {calls:?}"));
        assert_eq!(
            cwd,
            real_dir(&report.dir),
            "the tab must start in the packet"
        );
        assert_eq!(cwd, real_dir(&packet_dir(&env.reviews_dir, "t1", "T1", 1)));
        assert_eq!(
            cwd,
            real_dir(scratch.home.path())
                .join(".shprd")
                .join("reviews")
                .join("t1")
                .join("T1-a1")
        );
        assert_ne!(
            cwd,
            std::path::PathBuf::from("/tmp/wt/T1"),
            "the tab must never start in the worktree"
        );
        // A reviewer's title says so, so it never reads as the worker's.
        assert_eq!(title, reviewer_title("codex", "T1"));
        assert_ne!(title, spawn::worker_title("codex", "T1"), "{title}");

        let pairs = calls
            .iter()
            .find_map(|call| match call {
                Call::Spawn(_, _, _, pairs, _) => Some(pairs.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("the reviewer CLI was not started: {calls:?}"));
        assert!(
            pairs.contains(&("SHPRD_ACTOR".to_string(), Some("agent:codex".to_string()))),
            "{pairs:?}"
        );
        assert!(
            pairs.contains(&(
                "SHPRD_REPO".to_string(),
                Some(real_dir(&report.dir).display().to_string())
            )),
            "{pairs:?}"
        );
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
        // A reviewer may not push, so the remote credentials are dropped.
        assert!(pairs.contains(&("GH_TOKEN".to_string(), None)), "{pairs:?}");
        assert!(
            !pairs.contains(&("GH_TOKEN".to_string(), Some("secret".to_string()))),
            "{pairs:?}"
        );
        // The spec has no system-prompt arguments, so the brief is typed in.
        assert!(
            calls
                .iter()
                .any(|call| matches!(call, Call::Write(_, text, _) if text.contains("# REVIEW "))),
            "the brief was never typed in: {calls:?}"
        );
    }

    /// The argv the first reviewer tab's CLI was started with.
    fn launched_argv(mux: &FakeMux) -> Vec<String> {
        mux.calls()
            .into_iter()
            .find_map(|call| match call {
                Call::Spawn(_, _, argv, _, _) => Some(argv),
                _ => None,
            })
            .unwrap_or_else(|| panic!("the reviewer CLI was not started"))
    }

    #[test]
    fn the_reviewer_comes_from_the_settings_role() {
        // The program, model, effort and access mode are the reviewer role's
        // own, resolved through the settings, not a separate config value.
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        scratch.reported(&ctx);
        let mut env = scratch.review_env_with(
            &ctx,
            None,
            vec![
                Program {
                    spec: fake_spec("sh", "pi"),
                    installed: true,
                },
                Program {
                    spec: fake_spec("codex", "openai"),
                    installed: true,
                },
            ],
        );
        env.reviewer = settings_reviewer(
            "[roles.reviewer]\nagent = \"codex\"\nmodel = \"m1\"\neffort = \"high\"\nperm = \"read-only\"\n",
        );
        let reviewer = env
            .reviewer
            .agent
            .as_ref()
            .unwrap_or_else(|| panic!("no program"));
        assert_eq!(reviewer.value, "codex");
        assert_eq!(
            reviewer.source,
            LayerSource::You,
            "the reviewer comes from the person's settings"
        );
        let mux = scratch.mux();

        let report = review(&mux, &env, &request(false)).unwrap_or_else(|err| panic!("{err}"));

        assert_eq!(report.agent, "codex");
        assert_eq!(report.model.as_deref(), Some("m1"));
        let argv = launched_argv(&mux);
        assert!(
            argv.windows(2)
                .any(|pair| pair == ["--model".to_string(), "m1".to_string()]),
            "the role's model reaches the CLI: {argv:?}"
        );
        assert!(
            argv.windows(2)
                .any(|pair| pair == ["--effort".to_string(), "high".to_string()]),
            "the role's effort reaches the CLI: {argv:?}"
        );
        assert!(
            argv.windows(2)
                .any(|pair| pair == ["--mode".to_string(), "read-only".to_string()]),
            "the role's read-only mode reaches the CLI: {argv:?}"
        );
    }

    #[test]
    fn the_legacy_roster_reviewer_still_resolves_and_says_where_it_came_from() {
        // A person who named the reviewer the pre-M9 way keeps it: the old
        // config file is the lower settings layer, and the role says so.
        let role = legacy_reviewer(
            "[roster]\nreviewer = \"codex/gpt-5-codex\"\n\n[roster.effort]\n\"codex/gpt-5-codex\" = \"high\"\n",
        );
        assert_eq!(role.agent(), Some("codex"));
        assert_eq!(role.model(), Some("gpt-5-codex"));
        assert_eq!(role.effort(), Some("high"));
        assert_eq!(
            role.agent.as_ref().map(|agent| agent.source),
            Some(LayerSource::Legacy),
            "the reviewer's program comes from the old config file"
        );
    }

    #[test]
    fn the_cross_vendor_check_uses_the_settings_reviewer() {
        // The same family on both sides is refused, and the message names the
        // programme the settings chose: the check reads the resolved role.
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        scratch.reported_with(&ctx, "pi", Some("deepseek/deepseek-v4.1-flash"));
        let mut env = scratch.review_env_with(&ctx, None, family_programs());
        env.reviewer = settings_reviewer(
            "[roles.reviewer]\nagent = \"pi\"\nmodel = \"deepseek/deepseek-v4.1-flash\"\n",
        );
        let mux = scratch.mux();

        let err = review(&mux, &env, &request(false)).expect_err("the families match");
        assert_eq!(err.exit_code(), 2);
        let message = format!("{err}");
        assert!(
            message.contains("both run models from the deepseek family"),
            "{message}"
        );
        assert!(!env.reviews_dir.exists(), "a refused review writes nothing");
    }

    #[test]
    fn a_settings_role_that_may_not_review_is_refused() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        scratch.reported(&ctx);
        let mut env = scratch.review_env(&ctx, None);
        env.reviewer = settings_reviewer(
            "[roles.reviewer]\nagent = \"codex\"\nmodel = \"m1\"\nkind = \"worker\"\nreadonly = false\n",
        );
        let mux = scratch.mux();

        let err = review(&mux, &env, &request(false)).expect_err("a worker role may not review");
        let message = format!("{err}");
        assert!(message.contains("may not review"), "{message}");
        assert!(message.contains("readonly"), "{message}");
        assert_eq!(opened_tabs(&mux).len(), 0, "no reviewer tab opened");
    }

    #[test]
    fn a_system_prompt_reviewer_is_given_the_kickoff() {
        // A CLI whose brief arrives as a system prompt starts idle: without the
        // kickoff turn it never reads the packet, and the review sits for ever.
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        scratch.reported(&ctx);
        let env = scratch.review_env_with(
            &ctx,
            Some("codex/m1"),
            vec![
                Program {
                    spec: fake_spec("sh", "pi"),
                    installed: true,
                },
                Program {
                    spec: fake_prompt_spec("codex", "openai"),
                    installed: true,
                },
            ],
        );
        let mux = scratch.mux();

        review(&mux, &env, &request(false)).unwrap_or_else(|err| panic!("{err}"));

        let argv = launched_argv(&mux);
        assert!(
            argv.contains(&"--system-prompt".to_string()),
            "the brief travels as a system prompt: {argv:?}"
        );
        assert_eq!(
            prompts(&mux),
            vec![deliver::KICKOFF.to_string()],
            "the reviewer gets the same first turn a worker gets"
        );
        assert!(
            escalations(&ctx).is_empty(),
            "a delivered kickoff escalates nothing"
        );
    }

    #[test]
    fn a_kickoff_that_never_arrives_is_escalated() {
        // The reviewer's pane never becomes an agent, so the kickoff cannot be
        // typed in: the review must not sit silently idle.
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        scratch.reported(&ctx);
        let env = scratch.review_env_with(
            &ctx,
            Some("codex/m1"),
            vec![
                Program {
                    spec: fake_spec("sh", "pi"),
                    installed: true,
                },
                Program {
                    spec: fake_prompt_spec("codex", "openai"),
                    installed: true,
                },
            ],
        );
        let mux = scratch.unready_mux();

        review(&mux, &env, &request(false)).unwrap_or_else(|err| panic!("{err}"));

        let escalations = escalations(&ctx);
        assert_eq!(escalations.len(), 1, "{escalations:?}");
        assert_eq!(
            escalations[0].1, "review kickoff not delivered",
            "the ledger says which turn was lost"
        );
        assert!(
            escalations[0]
                .2
                .contains("delivers the review kickoff again"),
            "the default action names the same turn: {:?}",
            escalations[0].2
        );
        assert!(prompts(&mux).is_empty(), "nothing may be typed in");
    }

    fn kickoff_env<'a>(scratch: &Scratch, ctx: &'a Ctx) -> ReviewEnv<'a> {
        scratch.review_env_with(
            ctx,
            Some("codex/m1"),
            vec![
                Program {
                    spec: fake_spec("sh", "pi"),
                    installed: true,
                },
                Program {
                    spec: fake_prompt_spec("codex", "openai"),
                    installed: true,
                },
            ],
        )
    }

    #[test]
    fn a_reviewer_that_is_already_working_raises_no_kickoff_card() {
        // Detection came late (sweatbrain seq 206-209): the reviewer was producing
        // output by the time the wait ran out, so there is nothing to retry.
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        scratch.reported(&ctx);
        let env = kickoff_env(&scratch, &ctx);
        let mux = scratch.unready_mux();
        mux.set_state_after(&reviewer_pane(), 2, AgentState::Working);

        review(&mux, &env, &request(false)).unwrap_or_else(|err| panic!("{err}"));

        assert!(escalations(&ctx).is_empty(), "{:?}", escalations(&ctx));
    }

    #[test]
    fn a_kickoff_card_names_its_request_when_the_task_has_more_than_one() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        scratch.reported(&ctx);
        let env = kickoff_env(&scratch, &ctx);
        let mux = scratch.unready_mux();
        review(&mux, &env, &request(false)).unwrap_or_else(|err| panic!("{err}"));
        let again = ReviewRequest {
            again: true,
            ..request(false)
        };
        review(&mux, &env, &again).unwrap_or_else(|err| panic!("{err}"));

        let escalations = escalations(&ctx);
        assert_eq!(escalations.len(), 2, "{escalations:?}");
        assert_eq!(escalations[0].1, "review kickoff not delivered");
        assert!(
            escalations[1]
                .1
                .starts_with("review kickoff not delivered (review request #"),
            "{:?}",
            escalations[1].1
        );
        assert!(
            escalations[1].2.contains("ms in all"),
            "the card states how long it waited: {:?}",
            escalations[1].2
        );
    }

    /// A claude reviewer's environment with a private HOME, so the trust entry
    /// goes to a temp file and never the person's `~/.claude.json`.
    fn claude_review_env<'a>(scratch: &Scratch, ctx: &'a Ctx) -> ReviewEnv<'a> {
        let mut claude = fake_prompt_spec("claude", "anthropic");
        claude.blocking_prompts = vec![BlockingPrompt {
            name: "folder trust".to_string(),
            pattern: "Is this a project you created or one you trust".to_string(),
            answer: None,
        }];
        let mut env = scratch.review_env_with(
            ctx,
            Some("claude/m1"),
            vec![
                Program {
                    spec: fake_spec("sh", "pi"),
                    installed: true,
                },
                Program {
                    spec: claude,
                    installed: true,
                },
            ],
        );
        env.spawn.base_env.insert(
            "HOME".to_string(),
            scratch.home.path().display().to_string(),
        );
        env
    }

    #[test]
    fn a_claude_reviewers_packet_folder_is_trusted_before_it_starts() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        scratch.reported(&ctx);
        let env = claude_review_env(&scratch, &ctx);
        let mux = scratch.mux();

        let report = review(&mux, &env, &request(false)).unwrap_or_else(|err| panic!("{err}"));

        let config = scratch.home.path().join(".claude.json");
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
        let key = real_dir(&report.dir).display().to_string();
        assert_eq!(
            doc["projects"][&key]["hasTrustDialogAccepted"], true,
            "{doc}"
        );
    }

    #[test]
    fn a_kickoff_stuck_on_the_trust_prompt_says_so_plainly() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        scratch.reported(&ctx);
        let env = claude_review_env(&scratch, &ctx);
        let mux = scratch.unready_mux();
        mux.set_screen(
            &reviewer_pane(),
            "Is this a project you created or one you trust?\n  > 1. Yes, I trust",
        );

        review(&mux, &env, &request(false)).unwrap_or_else(|err| panic!("{err}"));

        let escalations = escalations(&ctx);
        assert_eq!(escalations.len(), 1, "{escalations:?}");
        assert_eq!(
            escalations[0].1,
            "review kickoff not delivered: claude is asking to trust the folder"
        );
        assert!(prompts(&mux).is_empty(), "nothing may be typed over it");
    }

    #[test]
    fn a_second_review_of_the_same_attempt_is_refused() {
        // One attempt has one reviewer at a time: a second plain review would
        // record a second request and lose the tab the first one opened.
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        scratch.reported(&ctx);
        let env = scratch.review_env(&ctx, Some("codex/m1"));
        let mux = scratch.mux();
        review(&mux, &env, &request(false)).unwrap_or_else(|err| panic!("{err}"));

        let err = review(&mux, &env, &request(false)).expect_err("the review is already out");
        let message = format!("{err}");
        assert!(
            message.contains("already has a review requested"),
            "{message}"
        );
        assert!(message.contains("--again"), "{message}");
        assert_eq!(opened_tabs(&mux).len(), 1, "no second reviewer is opened");
        assert_eq!(
            recorded_reviews(&ctx).len(),
            1,
            "the first request is the only one recorded"
        );
    }

    #[test]
    fn a_brief_whose_first_delivery_fails_is_retried_once() {
        // A CLI that is not yet detected as an agent misses the first brief:
        // the second delivery, a moment later, finds it ready and lands. A lost
        // first delivery is a start-up race, not something to escalate.
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        scratch.reported(&ctx);
        let env = scratch.review_env(&ctx, Some("codex/m1"));
        let mux = scratch.unready_mux();
        let pane = reviewer_pane();
        // Unknown for the first poll, an idle agent from the retry on.
        mux.set_state_after(&pane, 1, AgentState::Idle);

        review(&mux, &env, &request(false)).unwrap_or_else(|err| panic!("{err}"));

        let delivered = prompts(&mux);
        assert_eq!(
            delivered.len(),
            1,
            "the retried brief lands once: {delivered:?}"
        );
        assert!(delivered[0].contains("# REVIEW "), "{delivered:?}");
        let landed = mux.calls().into_iter().find_map(|call| match call {
            Call::Write(pane, _, _) => Some(pane),
            _ => None,
        });
        assert_eq!(
            landed,
            Some(reviewer_pane()),
            "the brief landed in the reviewer's tab, so the scripted pane is its root pane"
        );
        assert_eq!(
            detection_polls(&mux),
            2,
            "one detection attempt for the first delivery and one for the retry"
        );
        assert!(
            escalations(&ctx).is_empty(),
            "a brief the retry delivered escalates nothing: {:?}",
            escalations(&ctx)
        );
    }

    #[test]
    fn a_brief_that_fails_twice_is_escalated_once() {
        // The reviewer's pane never becomes an agent, so neither the first
        // delivery nor the retry lands: the human hears about it once, with the
        // reason the last attempt gave.
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        scratch.reported(&ctx);
        let env = scratch.review_env(&ctx, Some("codex/m1"));
        let mux = scratch.unready_mux();

        review(&mux, &env, &request(false)).unwrap_or_else(|err| panic!("{err}"));

        let escalations = escalations(&ctx);
        assert_eq!(
            escalations.len(),
            1,
            "exactly one escalation: {escalations:?}"
        );
        let (task, body, default_action) = &escalations[0];
        assert_eq!(task.as_deref(), Some("T1"));
        assert_eq!(body, "review brief not delivered");
        assert!(
            default_action.starts_with(
                "press Retry review on the task in the Tasks pane, which closes the reviewer tab \
                 and delivers the review brief again (the agent was not detected within 0 ms; \
                 tried for "
            ) && default_action.ends_with(" ms in all); never type into the reviewer's tab"),
            "{default_action}"
        );
        assert_eq!(
            detection_polls(&mux),
            3,
            "the brief was retried exactly once, then the escalation looked once more at the pane"
        );
        assert!(prompts(&mux).is_empty(), "nothing may be typed in");
    }

    /// A review request that restarts the review it replaces.
    fn restart() -> ReviewRequest {
        ReviewRequest {
            task: "T1".to_string(),
            degraded: false,
            again: true,
        }
    }

    /// Writes `T1`'s `findings.json` and ages it by `age`, so a test says
    /// whether the watch would already have read it.
    fn wrote_findings(env: &ReviewEnv<'_>, text: &str, age: Duration) -> PathBuf {
        let path = findings_path(&env.reviews_dir, &env.spawn.run, "T1", 1);
        std::fs::create_dir_all(path.parent().unwrap_or(&env.reviews_dir))
            .unwrap_or_else(|err| panic!("{err}"));
        std::fs::write(&path, text).unwrap_or_else(|err| panic!("{err}"));
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap_or_else(|err| panic!("{err}"));
        file.set_modified(SystemTime::now() - age)
            .unwrap_or_else(|err| panic!("{err}"));
        path
    }

    /// A findings file a reviewer wrote and finished: one minor finding.
    const WRITTEN_FINDINGS: &str = r#"[{"severity":"minor","title":"a nit","location":null,
        "detail":"worth a look"}]"#;

    /// Every tab the host was asked to close, oldest first.
    fn closed_tabs(mux: &FakeMux) -> Vec<String> {
        mux.calls()
            .into_iter()
            .filter_map(|call| match call {
                Call::CloseTab(tab) => Some(tab.as_str().to_string()),
                _ => None,
            })
            .collect()
    }

    /// Every tab the host was asked to open, oldest first.
    fn opened_tabs(mux: &FakeMux) -> Vec<String> {
        mux.calls()
            .into_iter()
            .filter_map(|call| match call {
                Call::Spawn(_, title, _, _, _) => Some(title),
                _ => None,
            })
            .collect()
    }

    /// The review requests the run log holds for `T1` attempt 1.
    fn recorded_reviews(ctx: &Ctx) -> Vec<shprd_core::state::ReviewRequest> {
        let state = ctx.state().unwrap_or_else(|err| panic!("{err}"));
        match state.tasks.get("T1").and_then(|task| task.latest_attempt()) {
            Some(attempt) => attempt.reviews.clone(),
            None => panic!("the attempt is missing"),
        }
    }

    #[test]
    fn a_restart_replaces_an_undelivered_reviewer() {
        // The first review's brief never lands, so the human would see the
        // escalation. A restart closes that reviewer, clears whatever it left
        // behind and hands the brief to a fresh tab, for the same attempt.
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        scratch.reported(&ctx);
        let env = scratch.review_env(&ctx, Some("codex/m1"));
        let mux = scratch.unready_mux();
        review(&mux, &env, &request(false)).unwrap_or_else(|err| panic!("{err}"));
        let first = recorded_tab(&env, "T1", 1);
        assert_eq!(
            open_tabs(&mux, &env),
            vec![first.clone()],
            "the record names the tab the first review opened"
        );
        // A reviewer that got as far as writing a findings file leaves it in
        // the packet; a restart must not let the watchdog read it. The file is
        // aged past the settle window, because a fresh one may still be being
        // written: a restart refuses that case instead of clearing it.
        let findings = wrote_findings(&env, "[", Duration::from_secs(60));
        // The fresh reviewer's pane is an agent from its first poll.
        let fresh = PaneId::new("pane-2");
        mux.set_state_after(&fresh, 0, AgentState::Idle);

        let report = review(&mux, &env, &restart()).unwrap_or_else(|err| panic!("{err}"));

        assert_eq!(report.attempt, 1, "a restart keeps the attempt");
        assert_eq!(
            closed_tabs(&mux),
            vec![first.clone()],
            "the old reviewer's tab is the one closed"
        );
        let fresh_tab = recorded_tab(&env, "T1", 1);
        assert_ne!(fresh_tab, first, "the record names the new tab");
        assert_eq!(
            open_tabs(&mux, &env),
            vec![fresh_tab],
            "only the fresh reviewer is open, and the record names it"
        );
        assert_eq!(opened_tabs(&mux).len(), 2, "one reviewer tab per request");
        assert!(
            !findings.exists(),
            "a restart clears a partial findings file"
        );
        let brief = std::fs::read_to_string(&report.brief).unwrap_or_else(|err| panic!("{err}"));
        assert!(
            brief.contains("# REVIEW "),
            "the brief still points at the packet"
        );
        assert!(
            mux.calls()
                .into_iter()
                .any(|call| matches!(call, Call::Write(pane, text, _) if pane == fresh && text.contains("# REVIEW "))),
            "the brief is delivered to the fresh reviewer"
        );
        assert_eq!(
            recorded_reviews(&ctx).len(),
            2,
            "the restart records a second request for the same attempt"
        );
    }

    #[test]
    fn a_restart_after_a_verdict_is_refused() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        scratch.reported(&ctx);
        let env = scratch.review_env(&ctx, Some("codex/m1"));
        let mux = scratch.mux();
        review(&mux, &env, &request(false)).unwrap_or_else(|err| panic!("{err}"));
        let request = recorded_reviews(&ctx)
            .last()
            .cloned()
            .unwrap_or_else(|| panic!("the review request is missing"));
        // A verdict is the engine's to record, from the packet the reviewer
        // wrote: exactly what `verdict` records, which is what the restart sees.
        ctx.as_actor(Actor::Engine)
            .commit(EventKind::GatePassed {
                task: "T1".to_string(),
                attempt: 1,
                gate: REVIEW_GATE.to_string(),
                findings: Vec::new(),
                source: Some(ReviewSource {
                    reviewer: request.reviewer.clone(),
                    findings_sha256: "ba7816bf".to_string(),
                    request_seq: request.seq,
                }),
            })
            .unwrap_or_else(|err| panic!("{err}"));

        let err = review(&mux, &env, &restart()).expect_err("a decided review cannot restart");
        let message = format!("{err}");
        assert!(
            message.contains("already has a review verdict"),
            "{message}"
        );
        assert_eq!(opened_tabs(&mux).len(), 1, "no second reviewer tab opened");
        assert!(
            closed_tabs(&mux).is_empty(),
            "the decided reviewer stays open"
        );
    }

    /// A restart never deletes findings the reviewer finished writing.
    ///
    /// The reviewer can be ahead of the watch: the file is written and valid,
    /// and the next tick will record its verdict. Deleting it would throw away
    /// a finished review, so the restart is refused and the file stays.
    #[test]
    fn a_restart_never_discards_written_findings() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        scratch.reported(&ctx);
        let env = scratch.review_env(&ctx, Some("codex/m1"));
        let mux = scratch.mux();
        review(&mux, &env, &request(false)).unwrap_or_else(|err| panic!("{err}"));
        let findings = wrote_findings(&env, WRITTEN_FINDINGS, Duration::ZERO);

        let err = review(&mux, &env, &restart()).expect_err("finished findings are not stale");

        assert_eq!(err.exit_code(), 2, "a refusal exits 2: {err}");
        let message = format!("{err}");
        assert!(
            message.contains("the reviewer already wrote its findings"),
            "{message}"
        );
        assert!(
            message.contains("the watchdog will record the verdict"),
            "{message}"
        );
        assert!(
            findings.exists(),
            "written findings must survive the refusal"
        );
        assert_eq!(opened_tabs(&mux).len(), 1, "no fresh reviewer is opened");
        assert!(
            closed_tabs(&mux).is_empty(),
            "the reviewer that wrote the findings stays open"
        );
    }

    /// A restart never clears a findings file the reviewer may still be writing.
    ///
    /// Inside the watch's settle window the file on disk is not yet the
    /// review's last word, so it is neither readable nor deletable: the restart
    /// is refused and the human is told to come back.
    #[test]
    fn a_restart_never_clears_a_file_still_being_written() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        scratch.reported(&ctx);
        let env = scratch.review_env(&ctx, Some("codex/m1"));
        let mux = scratch.mux();
        review(&mux, &env, &request(false)).unwrap_or_else(|err| panic!("{err}"));
        // Half a JSON array: a reviewer caught mid-write.
        let findings = wrote_findings(&env, "[", Duration::ZERO);

        let err = review(&mux, &env, &restart()).expect_err("a fresh file is still being written");

        assert_eq!(err.exit_code(), 2, "a refusal exits 2: {err}");
        let message = format!("{err}");
        assert!(
            message.contains("the reviewer is still writing its findings"),
            "{message}"
        );
        assert!(message.contains("try again shortly"), "{message}");
        assert!(findings.exists(), "a file mid-write must not be deleted");
        assert_eq!(opened_tabs(&mux).len(), 1, "no fresh reviewer is opened");
        assert!(
            closed_tabs(&mux).is_empty(),
            "the writing reviewer's tab stays open"
        );
    }

    /// A settled findings file that does not parse is the one a restart clears.
    ///
    /// The watch escalates that packet instead of deciding from it, so the
    /// review really has to be started over, and the file left by the reviewer
    /// it replaces must not be read as the new review's verdict.
    #[test]
    fn a_restart_clears_a_settled_invalid_packet() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        scratch.reported(&ctx);
        let env = scratch.review_env(&ctx, Some("codex/m1"));
        let mux = scratch.mux();
        review(&mux, &env, &request(false)).unwrap_or_else(|err| panic!("{err}"));
        let findings = wrote_findings(&env, "not json at all", Duration::from_secs(60));

        let report = review(&mux, &env, &restart()).unwrap_or_else(|err| panic!("{err}"));

        assert_eq!(report.attempt, 1, "a restart keeps the attempt");
        assert!(
            !findings.exists(),
            "a settled invalid file is cleared for the fresh reviewer"
        );
        assert_eq!(opened_tabs(&mux).len(), 2, "the fresh reviewer is opened");
    }

    #[test]
    fn a_restart_without_a_review_is_refused() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        scratch.reported(&ctx);
        let env = scratch.review_env(&ctx, Some("codex/m1"));
        let mux = scratch.mux();

        let err = review(&mux, &env, &restart()).expect_err("there is no review to restart");
        let message = format!("{err}");
        assert!(message.contains("has no review to restart"), "{message}");
        assert!(opened_tabs(&mux).is_empty(), "no reviewer tab opened");
        assert!(!env.reviews_dir.exists(), "no packet may be written");
    }

    /// The shipped `data/harness` directory.
    fn harness_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/harness")
    }

    /// Every harness spec the data directory ships, parsed from its TOML.
    fn shipped_specs() -> Vec<HarnessSpec> {
        let mut specs = Vec::new();
        for entry in std::fs::read_dir(harness_dir()).unwrap_or_else(|err| panic!("{err}")) {
            let path = entry.unwrap_or_else(|err| panic!("{err}")).path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{err}"));
            let spec = toml::from_str::<HarnessSpec>(&text)
                .unwrap_or_else(|err| panic!("{}: {err}", path.display()));
            specs.push(spec);
        }
        assert!(!specs.is_empty(), "no harness specs to check");
        specs
    }

    /// One shipped spec by harness id.
    fn shipped_spec(id: &str) -> HarnessSpec {
        shipped_specs()
            .into_iter()
            .find(|spec| spec.id == id)
            .unwrap_or_else(|| panic!("no shipped spec for {id}"))
    }

    #[test]
    fn no_reviewer_inherits_the_specs_auto_approval_flags() {
        // Auto-approval exists for unattended workers. A reviewer runs beside a
        // human and must never act unprompted, so no shipped spec's flag reaches
        // its argv, not even the one that only suppresses prompts.
        let mut checked = 0;
        for spec in shipped_specs() {
            let some = spec.launch.auto_approve.clone();
            let Some(auto) = some else { continue };
            if auto.is_empty() {
                continue;
            }
            checked += 1;
            let (argv, _) =
                reviewer_argv(&spec, Some("m1"), None, None, "brief", Path::new("b.md"))
                    .unwrap_or_else(|err| panic!("{}: {err}", spec.id));
            for flag in auto {
                assert!(
                    !argv.contains(&flag),
                    "the {} reviewer inherited {flag}: {argv:?}",
                    spec.id
                );
            }
        }
        assert!(checked > 0, "no shipped spec has auto-approval to check");
    }

    #[test]
    fn the_codex_reviewer_is_confined_to_its_packet() {
        // The packet directory is the reviewer's cwd, so a workspace-write
        // sandbox is what keeps a codex reviewer's writes out of the worktree.
        let spec = shipped_spec("codex");
        let (argv, _) = reviewer_argv(&spec, Some("m1"), None, None, "brief", Path::new("b.md"))
            .unwrap_or_else(|err| panic!("{err}"));
        assert!(
            argv.windows(2)
                .any(|pair| pair == ["--sandbox".to_string(), "workspace-write".to_string()]),
            "the codex reviewer is not sandboxed: {argv:?}"
        );
    }

    /// The tab a review recorded, so a test names the tab it asserts about.
    fn recorded_tab(env: &ReviewEnv<'_>, task: &str, attempt: u32) -> String {
        let path = tab_record_path(&env.spawn.briefs_dir, &env.spawn.run, task, attempt);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("{err}"))
            .trim()
            .to_string()
    }

    /// Every tab the fake host still has open, across every space.
    ///
    /// A task's worker and reviewer share its own space, so "the space this
    /// suite means" is whichever one the task's labels made; listing them all
    /// is the simplest way to be right about it.
    fn open_tabs(mux: &FakeMux, env: &ReviewEnv<'_>) -> Vec<String> {
        let _ = env;
        let mut ids: Vec<String> = spawn::all_handles(mux)
            .into_iter()
            .map(|tab| tab.id.to_string())
            .collect();
        ids.sort();
        ids
    }

    /// Opens a review of `T1` on `ctx`'s run: its environment, host and tab.
    fn reviewed<'a>(scratch: &Scratch, ctx: &'a Ctx) -> (ReviewEnv<'a>, FakeMux, String) {
        let env = scratch.review_env(ctx, Some("codex/m1"));
        let mux = scratch.mux();
        review(&mux, &env, &request(false)).unwrap_or_else(|err| panic!("{err}"));
        let tab = recorded_tab(&env, "T1", 1);
        (env, mux, tab)
    }

    #[test]
    fn a_verdict_closes_the_reviewer_tab() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        scratch.reported(&ctx);
        let (env, mux, tab) = reviewed(&scratch, &ctx);
        let findings = packet_dir(&env.reviews_dir, "t1", "T1", 1).join("findings.json");
        std::fs::write(&findings, "[]").unwrap_or_else(|err| panic!("{err}"));
        assert!(
            open_tabs(&mux, &env).contains(&tab),
            "the review left no tab for the verdict to close"
        );

        // The engine records the verdict, not the human default: only the
        // engine may turn a reviewer's packet into a gate result.
        let engine = Ctx::new(ctx.root.clone(), Actor::Engine, ctx.json, ctx.no_dagr);
        let outcome =
            crate::cmd::verdict::submit_verdict(&mux, &engine, Some(scratch.home.path()), "T1", 1)
                .unwrap_or_else(|err| panic!("{err}"));

        assert_eq!(outcome, crate::cmd::verdict::Outcome::Passed);
        assert!(
            !open_tabs(&mux, &env).contains(&tab),
            "the reviewer tab {tab} outlived its verdict"
        );
    }

    /// SHPR-23's contract on the fake host: a task's worker and its reviewer
    /// start with no space and no tab of their own, both are listed as agents
    /// (what `live_agents` is built from), and the one close a landing and a
    /// drop share stops both.
    #[test]
    fn a_worker_and_a_reviewer_have_no_tab_and_stop_on_land_or_drop() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        scratch.reported(&ctx);
        let (env, mux, reviewer_handle) = reviewed(&scratch, &ctx);
        let space = spawn::agent_space(&mux, &env.spawn).unwrap_or_else(|err| panic!("{err}"));
        let worker = mux
            .spawn_agent(&shprd_mux::AgentLaunch {
                space: &space.id,
                title: "sh·T1",
                argv: &[],
                env: &[],
                cwd: scratch.root(),
                role: shprd_mux::AgentRole::Worker,
                task: Some("T1"),
                program: "sh",
                parent: None,
            })
            .unwrap_or_else(|err| panic!("{err}"));
        std::fs::write(
            spawn::tab_path(&env.spawn.briefs_dir, &env.spawn.run, "T1", 1),
            worker.id.to_string(),
        )
        .unwrap_or_else(|err| panic!("{err}"));

        // No space but the repository's, and no tab in it.
        let spaces = mux.list_spaces().unwrap_or_default();
        assert_eq!(spaces.len(), 1, "{spaces:?}");
        assert_eq!(spaces[0].name, spawn::repo_space_name(&scratch.repo_name()));
        assert!(mux.list_tabs(&spaces[0].id).unwrap_or_default().is_empty());
        // Both are listed, with their roles and their task.
        let listed = mux.list_agents().unwrap_or_default();
        let roles: Vec<(&str, Option<&str>)> = listed
            .iter()
            .map(|agent| (agent.role.as_str(), agent.task.as_deref()))
            .collect();
        assert!(roles.contains(&("worker", Some("T1"))), "{roles:?}");
        assert!(roles.contains(&("reviewer", Some("T1"))), "{roles:?}");
        assert!(open_tabs(&mux, &env).contains(&reviewer_handle));

        // A landing and a drop both close through this one function.
        spawn::stop_task_agents(&mux, &env.spawn.briefs_dir, &env.spawn.run, "T1")
            .unwrap_or_else(|err| panic!("{err}"));
        assert!(
            mux.list_agents().unwrap_or_default().is_empty(),
            "both agents were stopped"
        );
    }

    #[test]
    fn a_vanished_reviewer_tab_is_already_closed() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        scratch.reported(&ctx);
        let (env, mux, tab) = reviewed(&scratch, &ctx);
        // A human closed the tab before the verdict: the state the close wants.
        mux.close_tab(&shprd_mux::TabId::new(&tab))
            .unwrap_or_else(|err| panic!("{err}"));
        close_reviewer(&mux, &env.spawn.briefs_dir, &env.spawn.run, "T1", 1)
            .unwrap_or_else(|err| panic!("{err}"));
        // An attempt with no record at all has nothing to close either.
        close_reviewer(&mux, &env.spawn.briefs_dir, &env.spawn.run, "T1", 2)
            .unwrap_or_else(|err| panic!("{err}"));
    }

    #[test]
    fn run_end_closes_the_reviewer_and_worker_tabs() {
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        scratch.reported(&ctx);
        let (env, mux, reviewer) = reviewed(&scratch, &ctx);
        // A worker's record sits beside the reviewer's in the same run folder:
        // both are tabs this run opened, so run end closes both.
        let space = spawn::agent_space(&mux, &env.spawn).unwrap_or_else(|err| panic!("{err}"));
        let worker = mux
            .spawn(&space.id, "worker", &[], &[], scratch.root())
            .unwrap_or_else(|err| panic!("{err}"));
        std::fs::write(
            spawn::tab_path(&env.spawn.briefs_dir, &env.spawn.run, "T1", 1),
            worker.id.to_string(),
        )
        .unwrap_or_else(|err| panic!("{err}"));

        assert!(
            open_tabs(&mux, &env).contains(&reviewer),
            "the run has no reviewer tab to end"
        );
        // The task is only reported, so the run ends the one way it can with
        // work still in it: abandoned, which is what closes its tabs.
        crate::cmd::run::finish(
            &mux,
            &ctx,
            Some(scratch.home.path()),
            crate::cmd::run::EndRun::finish(true, true),
        )
        .unwrap_or_else(|err| panic!("{err}"));

        let open = open_tabs(&mux, &env);
        assert!(
            !open.contains(&reviewer),
            "a reviewer tab outlived the run: {open:?}"
        );
        assert!(
            !open.contains(&worker.id.to_string()),
            "a worker tab outlived the run: {open:?}"
        );
    }

    #[test]
    fn the_reviewer_fallback_reviews_when_the_main_reviewer_cannot() {
        let mut role = settings_reviewer(
            "[roles.reviewer]\nagent = \"claude\"\nfallback = [\"codex/gpt-5\"]\n",
        );
        assert_eq!(role.fallbacks.len(), 1);
        assert_eq!(role.fallbacks[0].agent_ref(), "codex/gpt-5");
        // Claude Code wrote it: the Claude reviewer cannot, so the fallback does.
        let programs = family_programs();
        let chosen = choose_reviewer(&role, "claude", None, &programs, false)
            .unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(chosen.agent, "codex");
        assert_eq!(chosen.model.as_deref(), Some("gpt-5"));
        let note = chosen.note.unwrap_or_default();
        assert!(note.contains("fallback reviewer `codex/gpt-5`"), "{note}");

        // A different author: the main reviewer stands, the fallback is unused.
        let chosen = choose_reviewer(&role, "codex", None, &programs, false)
            .unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(chosen.agent, "claude");
        assert!(chosen.note.is_none());

        // A main reviewer that is not installed here gives way too.
        let mut missing = family_programs();
        for program in &mut missing {
            if program.spec.id == "claude" {
                program.installed = false;
            }
        }
        let chosen = choose_reviewer(
            &role,
            "pi",
            Some("deepseek/deepseek-v4.1-flash"),
            &missing,
            false,
        )
        .unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(
            chosen.agent, "codex",
            "the main reviewer's CLI is not installed"
        );

        // No fallback: the refusal is the main reviewer's own.
        role.fallbacks.clear();
        assert!(choose_reviewer(&role, "claude", None, &programs, false).is_err());
    }

    #[test]
    fn reviewer_lines_are_tried_in_order() {
        // The new shape: `agents` holds the lines after the first.
        let role = settings_reviewer(
            "[roles.reviewer]\nagent = \"claude\"\nagents = [\"claude|||\", \"codex|gpt-5||\", \"pi||\"]\n",
        );
        assert_eq!(role.fallbacks.len(), 2, "a repeated program is one line");
        assert_eq!(role.fallbacks[0].agent_ref(), "codex/gpt-5");
        let programs = family_programs();
        let chosen = choose_reviewer(&role, "claude", None, &programs, false)
            .unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(chosen.agent, "codex");
    }

    /// Codex's own words when its account is spent, as its pane shows them.
    const CODEX_PANE: &str = "\u{2022} Working on the review\n\nERROR: You've hit your usage limit. \
         Upgrade to Plus to continue using Codex, or try again at Dec 18th, 2026 6:50 AM.\n\n\
         gpt-6-luna \u{b7} 100% context left";

    /// The person's own settings: a reviewer list of one `pi` GLM line and no
    /// `agent`, over an old `config.toml` that still names `codex`.
    fn glm_only_reviewer() -> ReviewerRole {
        let layer: ConfigLayer = toml::from_str("[roster]\nreviewer = \"codex/gpt-6-luna\"\n")
            .unwrap_or_else(|e| panic!("{e}"));
        let mut loaded = Loaded::default();
        loaded.file = File::parse(
            "[roles.reviewer]\nperm = \"auto\"\nagents = [\"pi|Command Code/z-ai/glm-5.3-flash|||read-only\"]\n",
        )
        .unwrap_or_else(|err| panic!("{err}"));
        loaded.file.legacy = layer.settings_projection();
        ReviewerRole::resolved(
            &loaded
                .resolve(None)
                .role(REVIEWER_ROLE)
                .unwrap_or_else(|| panic!("the reviewer role is missing")),
        )
    }

    #[test]
    fn a_reviewer_list_under_agents_alone_never_reviews_on_an_old_roster_program() {
        let role = glm_only_reviewer();
        assert_eq!(
            role.agent(),
            Some("pi"),
            "the settings list, not the old key"
        );
        assert_eq!(role.model(), Some("Command Code/z-ai/glm-5.3-flash"));
        assert!(role.fallbacks.is_empty(), "{:?}", role.fallbacks);
        let programs = family_programs();
        let chosen = choose_reviewer(
            &role,
            "pi",
            Some("deepseek/deepseek-v4.1-flash"),
            &programs,
            false,
        )
        .unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(chosen.agent, "pi");
        assert_eq!(
            chosen.model.as_deref(),
            Some("Command Code/z-ai/glm-5.3-flash")
        );
        // Not even when everything else is out: codex is not in the list.
        let mut spent = role.clone();
        spent.dry = vec![DryProgram {
            agent: "pi".into(),
            reason: "out of quota: usage limit".into(),
            until_ms: None,
        }];
        let err = choose_reviewer(&spent, "claude", None, &programs, false)
            .err()
            .map(|err| err.to_string())
            .unwrap_or_default();
        assert!(err.contains(NO_REVIEWER_LEFT), "{err}");
    }

    #[test]
    fn the_first_of_agents_is_the_reviewer_when_no_agent_is_set() {
        let role = settings_reviewer(
            "[roles.reviewer]\nagents = [\"codex|gpt-6-luna||\", \"pi|Command Code/z-ai/glm-5.3-flash||\"]\n",
        );
        assert_eq!(role.agent(), Some("codex"));
        assert_eq!(role.fallbacks.len(), 1);
        assert_eq!(role.fallbacks[0].agent, "pi");
    }

    #[test]
    fn a_reviewer_out_of_quota_is_skipped_for_the_next_line_with_a_plain_note() {
        let mut role = settings_reviewer(
            "[roles.reviewer]\nagents = [\"codex|gpt-6-luna||\", \"pi|Command Code/z-ai/glm-5.3-flash||\"]\n",
        );
        let reset = shprd_agents::exhausted::reset_at_ms(CODEX_PANE).expect("a reset time");
        role.dry = vec![DryProgram {
            agent: "codex".into(),
            reason: "out of quota: usage limit".into(),
            until_ms: Some(reset),
        }];
        let programs = family_programs();
        let chosen = choose_reviewer(
            &role,
            "pi",
            Some("deepseek/deepseek-v4.1-flash"),
            &programs,
            false,
        )
        .unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(chosen.agent, "pi");
        assert_eq!(
            chosen.card_note.as_deref(),
            Some("codex is out of quota until Dec 18; GLM (pi) reviews instead")
        );
        // Nobody else in the list can review: one plain refusal.
        role.fallbacks.clear();
        let err = choose_reviewer(
            &role,
            "pi",
            Some("deepseek/deepseek-v4.1-flash"),
            &programs,
            false,
        )
        .err()
        .map(|err| err.to_string())
        .unwrap_or_default();
        assert!(
            err.contains(NO_REVIEWER_LEFT) && err.contains("codex is out of quota until Dec 18"),
            "{err}"
        );
    }

    #[test]
    fn a_dry_mark_ends_at_the_time_the_cli_gave() {
        let mut state = shprd_core::state::RunState::default();
        state.agents.insert(
            "codex".into(),
            shprd_core::state::AgentInfo {
                dry: true,
                dry_until_ms: Some(1_000),
                dry_reason: Some("out of quota: usage limit".into()),
            },
        );
        assert_eq!(dry_programs(&state, 999).len(), 1);
        assert!(dry_programs(&state, 1_001).is_empty(), "past the reset");
    }

    #[test]
    fn codexs_usage_limit_pane_is_spent_and_a_diff_that_says_quota_is_not() {
        let now = 0;
        let spent = spent_pane(&[], CODEX_PANE, false, now).expect("codex is spent");
        assert!(spent.reason.starts_with("out of quota"), "{}", spent.reason);
        let day = spent.reset_at_ms.and_then(shprd_agents::exhausted::day_of);
        assert_eq!(day.as_deref(), Some("Dec 18"));
        // Still working: nothing is read.
        assert!(spent_pane(&[], CODEX_PANE, true, now).is_none());
        // A reviewer reading code that mentions a quota, far above the bottom.
        let diff = format!(
            "+ // the quota check: usage limit and credit\n{}\ndone reading\nthinking about it\nwriting findings\nstill here\nmore\nlast line",
            "x\n".repeat(3)
        );
        assert!(
            spent_pane(&[], &diff, false, now).is_none(),
            "content is not a verdict"
        );
        // A prompt echoed at the bottom is never a verdict.
        assert!(spent_pane(&[], "> check the usage limit wording", false, now).is_none());
        // A stale message (its printed reset has passed) is not a spent account.
        assert!(spent_pane(&[], CODEX_PANE, false, u64::MAX / 2).is_none());
        // Short words that are ordinary text are never a verdict.
        assert!(
            spent_pane(
                &[],
                "reviewing the billing quota code: 429 credit",
                false,
                now
            )
            .is_none()
        );
        // No time printed: a bounded cooldown, not for ever.
        let bare =
            spent_pane(&[], "ERROR: rate limit reached, slow down", false, 100).expect("rate");
        assert!(bare.reason.starts_with("rate-limited"));
        assert_eq!(bare.reset_at_ms, Some(100 + RATE_COOLDOWN_MS));
    }

    #[test]
    fn degraded_is_ignored_when_the_reviewer_is_independent() {
        // `--degraded` is a permission, not a claim. When the reviewer's family
        // is known and differs from the author's, the review is independent no
        // matter what the flag said, so the label would be a lie.
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        scratch.reported_with(&ctx, "pi", Some("deepseek/deepseek-v4.1-flash"));
        let programs = family_programs();
        let chosen = choose_reviewer(
            &ReviewerRole::written("pi/claude/claude-opus-5-5", None),
            "pi",
            Some("deepseek/deepseek-v4.1-flash"),
            &programs,
            true,
        )
        .unwrap_or_else(|err| panic!("{err}"));
        assert!(
            !chosen.degraded,
            "--degraded must not mark an independent review"
        );
        let note = chosen
            .note
            .clone()
            .unwrap_or_else(|| panic!("the ignored flag is never mentioned"));
        assert!(note.contains("--degraded"), "{note}");

        let env = scratch.review_env_with(&ctx, Some("pi/claude/claude-opus-5-5"), programs);
        let mux = scratch.mux();
        let report = review(&mux, &env, &request(true)).unwrap_or_else(|err| panic!("{err}"));
        assert!(!report.degraded, "the report labels an independent review");
        let brief = std::fs::read_to_string(&report.brief).unwrap_or_else(|err| panic!("{err}"));
        assert!(!brief.contains(DEGRADED_LABEL), "{brief}");
    }

    /// Runs git in `dir`, asserting success, and returns trimmed stdout.
    fn git_ok(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// Commits a file in `dir`.
    fn commit_file(dir: &Path, file: &str) {
        std::fs::write(dir.join(file), format!("{file}\n")).unwrap();
        git_ok(dir, &["add", "--", file]);
        git_ok(dir, &["commit", "-m", file]);
    }

    #[test]
    fn the_packet_diff_holds_only_the_tasks_own_commits_when_origin_is_ahead() {
        let temp = TempDir::new().unwrap();
        let repo = temp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        git_ok(temp.path(), &["init", "--bare", "origin.git"]);
        git_ok(&repo, &["init", "-b", "integration/m11"]);
        git_ok(&repo, &["config", "user.name", "tests"]);
        git_ok(&repo, &["config", "user.email", "tests@example.invalid"]);
        git_ok(&repo, &["config", "commit.gpgsign", "false"]);
        git_ok(&repo, &["remote", "add", "origin", "../origin.git"]);
        commit_file(&repo, "seed.txt");
        let seed = git_ok(&repo, &["rev-parse", "HEAD"]);
        // A pull request landed this on the forge only.
        commit_file(&repo, "landed.txt");
        git_ok(&repo, &["push", "-u", "origin", "integration/m11"]);
        git_ok(&repo, &["checkout", "-b", "task"]);
        commit_file(&repo, "own.txt");
        git_ok(&repo, &["branch", "-f", "integration/m11", &seed]);

        let diff = diff_against_merge_base(&repo, "integration/m11", "task").unwrap();
        assert!(diff.contains("own.txt"), "{diff}");
        assert!(!diff.contains("landed.txt"), "{diff}");
    }

    #[test]
    fn the_packet_is_written_for_a_diff_outside_the_declared_paths() {
        let temp = TempDir::new().unwrap();
        let repo = temp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        git_ok(&repo, &["init", "-b", "integration/m11"]);
        git_ok(&repo, &["config", "user.name", "tests"]);
        git_ok(&repo, &["config", "user.email", "tests@example.invalid"]);
        git_ok(&repo, &["config", "commit.gpgsign", "false"]);
        commit_file(&repo, "seed.txt");
        git_ok(&repo, &["checkout", "-b", "task"]);
        // A task that declared some other folder changes a file outside it.
        commit_file(&repo, "elsewhere.txt");
        let diff = diff_against_merge_base(&repo, "integration/m11", "task").unwrap();
        assert!(diff.contains("elsewhere.txt"), "{diff}");
    }
}
