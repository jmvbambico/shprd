//! `shprd agent review`: hand a finished task's diff to a cross-vendor reviewer.
//!
//! A reviewer never gets a worktree. It gets one folder holding everything it may
//! read: the diff against the branch's merge base with the integration branch, the
//! contract the attempt was written against, and the schema its findings must
//! match. [`review`] takes the [`Mux`](shprd_mux::Mux) as a parameter, the way
//! [`spawn`] does, so a test drives a fake host and nothing here reads the
//! environment or starts a process of its own.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::anyhow;
use serde_json::{Value, json};
use shprd_core::event::{EventKind, ReportStatus};
use shprd_core::state::{Attempt, Task, TaskStatus};
use shprd_git::git::Git;
use shprd_mux::{Mux, naming};
use shprd_roster::env::{EnvContext, LaunchEnv, Role as EnvRole, launch_env};
use shprd_roster::families::{Families, agent_vendor};
use shprd_roster::models::AgentRef;
use shprd_roster::spec::{BlockingPrompt, HarnessSpec, LaunchSpec, Values, render_args};

use crate::cli::ReviewArgs;
use crate::cmd::launch;
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
    /// `roster.reviewer`: the agent a review runs on, when one is configured.
    pub reviewer: Option<String>,
    /// How long to wait before retrying a brief the reviewer did not take.
    ///
    /// It is data rather than a constant so a test can set it to
    /// [`Duration::ZERO`] and still exercise the retry: the wait is production's
    /// patience, not the retry rule.
    pub brief_retry_wait: Duration,
}

impl ReviewEnv<'_> {
    /// Builds the environment from the process environment, the config and the
    /// run log.
    pub fn build<'a>(ctx: &'a Ctx, home: Option<&Path>) -> Result<ReviewEnv<'a>, CliError> {
        let home = crate::ctx::resolve_home(home)?;
        let spawn = spawn::build_env(ctx, Some(&home))?;
        // `build_env` keeps what a spawn needs; the reviewer is the one roster
        // entry only a review reads, so it is read here.
        let global = home.join(".config").join("shprd").join("config.toml");
        let repo_config = ctx.root.join(".shprd").join("config.toml");
        let config = shprd_core::config::load(Some(&global), Some(&repo_config))
            .map_err(|err| CliError::other(anyhow!(err).context("loading the shprd config")))?;
        Ok(ReviewEnv {
            spawn,
            reviews_dir: home.join(".shprd").join("reviews"),
            reviewer: config.roster.reviewer,
            brief_retry_wait: BRIEF_RETRY_WAIT,
        })
    }
}

/// What the caller asked for.
#[derive(Debug, Clone)]
pub struct ReviewRequest {
    /// The task whose latest attempt is reviewed.
    pub task: String,
    /// Review even when the reviewer shares the author's vendor.
    pub degraded: bool,
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
    reviews.join(run).join(format!("{task}-a{attempt}"))
}

/// The file a reviewer writes its findings to, inside its packet folder.
///
/// It is the one file read back out of a packet: the watchdog watches it, so a
/// finished review is picked up without the reviewer running any command.
pub fn findings_path(reviews: &Path, run: &str, task: &str, attempt: u32) -> PathBuf {
    packet_dir(reviews, run, task, attempt).join("findings.json")
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
    mux: &dyn Mux,
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
}

/// Renders the contract a reviewer checks the diff against.
///
/// It carries what the diff cannot say: what the task was for, what "done"
/// meant, what the attempt was allowed to touch, and what the agent claimed.
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
    out.push_str("\n## Scope — the only paths the attempt could change\n\n");
    for path in &task.scope {
        out.push_str(&format!("- {path}\n"));
    }
    out.push_str("\n## What the agent reported\n\n");
    let report = attempt.and_then(|attempt| attempt.report.as_ref());
    let Some(report) = report else {
        out.push_str("- no report\n");
        return out;
    };
    out.push_str(&format!("- summary: {}\n", report.summary));
    out.push_str(&listed("tests", &report.tests));
    out.push_str(&listed("changed paths", &report.changed_paths));
    if let Some(commit) = &report.commit {
        out.push_str(&format!("- commit: {commit}\n"));
    }
    out
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
/// since this one forked.
fn diff_against_merge_base(repo: &Path, base: &str, branch: &str) -> Result<String, CliError> {
    let git = Git::new(repo);
    let merge_base = git.run(&["merge-base", base, branch]).map_err(|err| {
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
    std::fs::create_dir_all(&dir).map_err(|err| {
        CliError::other(anyhow!(err).context(format!("creating {}", dir.display())))
    })?;
    let diff =
        diff_against_merge_base(&env.spawn.ctx.root, &env.spawn.base_branch, &attempt.branch)?;
    write_file(&dir.join("diff.patch"), &diff)?;
    write_file(
        &dir.join("contract.md"),
        &render_contract(&ContractInput { task }),
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

/// The agent a review runs on, with everything needed to start it.
#[derive(Debug, Clone)]
pub struct Reviewer {
    /// The program name, which is the harness spec id.
    pub agent: String,
    /// The model to select, when the configured string carried one.
    pub model: Option<String>,
    /// How to start the program.
    pub launch: LaunchSpec,
    /// The prompts the CLI waits on a human to answer before it reads anything.
    pub blocking_prompts: Vec<BlockingPrompt>,
    /// Whether this review is cross-vendor or degraded.
    pub degraded: bool,
    /// A one-line note to show the human when a flag had no effect.
    pub note: Option<String>,
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
pub fn choose_reviewer(
    configured: Option<&str>,
    author: &str,
    author_model: Option<&str>,
    programs: &[Program],
    degraded: bool,
) -> Result<Reviewer, CliError> {
    let raw = configured.ok_or_else(|| {
        CliError::Invalid(anyhow!(
            "no reviewer is configured: set one with `shprd set reviewer <program>/<model>`"
        ))
    })?;
    let specs: Vec<HarnessSpec> = spawn::spec_table(programs);
    let parsed = AgentRef::parse(raw, &specs).map_err(|err| {
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
        launch: program.spec.launch.clone(),
        blocking_prompts: program.spec.blocking_prompts.clone(),
        // `--degraded` is only recorded where the review needs the label: the
        // branches below set it, and it stays false for an independent review.
        degraded: false,
        note: None,
    };
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
        (_, None) => Some(raw),
    };
    if let Some(agent) = unknown {
        if !degraded {
            return Err(CliError::Invalid(anyhow!(
                "cannot prove the reviewer is a different vendor: {agent} has no known model \
                 family: set a reviewer with a known family with `shprd set reviewer`, or pass \
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
                 models from the {vendor} family: set a reviewer from another vendor with \
                 `shprd set reviewer`, or pass --degraded to review anyway"
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
    let Ok(dir) = crate::cmd::roster::resolve_data_dir() else {
        return Families::default();
    };
    Families::load(&dir.join("models").join("families.toml")).unwrap_or_default()
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
}

/// Renders the brief a reviewer reads before its first turn.
///
/// It names the three files the reviewer may read, says plainly that the
/// repository is off limits, and ends by telling the reviewer to write its
/// findings and stop: the reviewer runs no `shprd` command, because its folder
/// is outside the repository and the orchestration submits the verdict instead.
pub fn render_review_brief(input: &ReviewBrief<'_>) -> String {
    let mut out = String::new();
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
    out.push_str("You are a REVIEWER. You do not write code and you do not fix anything.\n\n");
    out.push_str("## What you may read\n\n");
    out.push_str(
        "- `diff.patch` — the whole change, against its merge base with the integration branch\n",
    );
    out.push_str("- `contract.md` — the task, what \"done\" meant, and what the agent reported\n");
    out.push_str("- `findings.schema.json` — the shape of what you write\n\n");
    out.push_str("- Read those three files, in this folder, and nothing else.\n");
    out.push_str("- Never open or edit the repository or any worktree; the diff is the truth you judge. If the packet does not show something you need, record that as a finding.\n");
    out.push_str("- Write nothing outside this folder.\n\n");
    out.push_str("## What to write\n\n");
    out.push_str("Write `findings.json` in this folder, matching `findings.schema.json`: a JSON array of `{severity, title, location, detail}`, where severity is one of `critical`, `major`, `minor` or `trivial`. Write an empty array — `[]` — when nothing is wrong: an empty array is a clean review, and it is the answer to aim for.\n\n");
    out.push_str("Judge the diff against the acceptance criteria and the scope: what it does wrong, what it misses, what it changes that it may not touch. A critical or major finding blocks integration, so spend them on things that matter.\n\n");
    out.push_str("## Then\n\n");
    out.push_str(
        "Write `findings.json` and stop. Do not run any `shprd` command: the folder you are in is \
         not a repository, so no command of yours would reach the run log. The orchestrator reads \
         your file and records the verdict itself.\n",
    );
    out
}

/// Opens the reviewer's tab in `dir` and starts its CLI there.
///
/// The tab's cwd is the packet folder, never the worktree: where a reviewer's
/// shell starts is the difference between reading a diff and editing the code.
/// Returns the tab's id and title.
fn open_reviewer(
    mux: &dyn Mux,
    env: &ReviewEnv<'_>,
    task: &str,
    reviewer: &Reviewer,
    dir: &Path,
    brief: &ReviewBrief<'_>,
    brief_path: &Path,
) -> Result<(shprd_mux::TabId, String), CliError> {
    let workspace = spawn::workspace_of(mux, &env.spawn)?;
    let wanted = naming::tab_title(naming::Role::Reviewer, &reviewer.agent, task, false);
    let existing = mux
        .list_tabs(&workspace.id)
        .map_err(|err| CliError::other(anyhow!(err).context("listing tabs")))?
        .into_iter()
        .map(|tab| tab.title)
        .collect::<Vec<String>>();
    let title = naming::disambiguate(&existing, &wanted, task);
    let tab = mux
        .create_tab(&workspace.id, &title, dir)
        .map_err(|err| CliError::other(anyhow!(err).context("creating the reviewer tab")))?;
    // The tab exists from here on, so every failure closes it again.
    let started = start_in_tab(
        mux, env, task, reviewer, dir, brief, brief_path, &title, &tab,
    );
    match started {
        Ok(()) => Ok((tab.id, title)),
        Err(err) => {
            spawn::close_tab(mux, &tab.id);
            Err(err)
        }
    }
}

/// Starts the reviewer's CLI in `tab` and hands it the brief.
///
/// A brief that does not arrive is reported, never fatal: the CLI is running,
/// and the human can resend the brief into the tab. A lost first delivery is
/// retried once by [`deliver_review_brief`] before it is reported.
#[expect(clippy::too_many_arguments, reason = "one launch's whole input")]
fn start_in_tab(
    mux: &dyn Mux,
    env: &ReviewEnv<'_>,
    task: &str,
    reviewer: &Reviewer,
    dir: &Path,
    brief: &ReviewBrief<'_>,
    brief_path: &Path,
    title: &str,
    tab: &shprd_mux::Tab,
) -> Result<(), CliError> {
    let text = render_review_brief(brief);
    let (argv, brief_in_args) = reviewer_argv(
        &reviewer.launch,
        reviewer.model.as_deref(),
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
            repo: dir.to_path_buf(),
        },
    ));
    mux.run(&tab.root_pane, &argv, &pairs)
        .map_err(|err| CliError::other(anyhow!(err).context("starting the reviewer CLI")))?;
    if brief_in_args {
        return Ok(());
    }
    let delivery = deliver_review_brief(mux, env, reviewer, tab, &text);
    let Some(reason) = delivery.reason() else {
        return Ok(());
    };
    eprintln!(
        "warning: review brief not delivered to {title}: {reason}; it is at {}",
        brief_path.display()
    );
    let escalation = EventKind::Escalated {
        task: Some(task.to_string()),
        body: "review brief not delivered".to_string(),
        default_action: "resend the review brief into the reviewer tab".to_string(),
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
    mux: &dyn Mux,
    env: &ReviewEnv<'_>,
    reviewer: &Reviewer,
    tab: &shprd_mux::Tab,
    text: &str,
) -> launch::Delivery {
    let wait = env.spawn.ready_wait;
    let first = launch::deliver_brief(mux, &tab.root_pane, text, &reviewer.blocking_prompts, wait);
    if matches!(first, launch::Delivery::Delivered) {
        return first;
    }
    std::thread::sleep(env.brief_retry_wait);
    launch::deliver_brief(mux, &tab.root_pane, text, &reviewer.blocking_prompts, wait)
}

/// The argv a reviewer's CLI starts with.
///
/// The order is the base command, the flags that confine a reviewer's writes
/// ([`LaunchSpec::reviewer_args`]), the model and finally the brief as the
/// system prompt. A reviewer never gets [`LaunchSpec::auto_approve`]: it runs
/// beside a human, so its tools must prompt rather than act unprompted, and a
/// reviewer that could silently write could read or change the worktree the
/// brief told it to leave alone. The second element says whether the brief went
/// in as an argument, which is also what says whether it has to be typed in
/// instead.
fn reviewer_argv(
    launch_spec: &LaunchSpec,
    model: Option<&str>,
    brief: &str,
    brief_path: &Path,
) -> Result<(Vec<String>, bool), CliError> {
    let mut argv = launch_spec.interactive.clone();
    if let Some(confine) = &launch_spec.reviewer_args {
        argv.extend(confine.iter().cloned());
    }
    let values = Values {
        model: model.map(str::to_string),
        // A reviewer has no effort level of its own: `roster.effort` is keyed by
        // worker agent strings, and a reviewer is chosen for its vendor, not its
        // depth.
        effort: None,
        prompt: Some(brief.to_string()),
        prompt_file: Some(brief_path.display().to_string()),
        session: None,
    };
    argv.extend(render(
        launch_spec,
        launch::model_args(launch_spec, model),
        &values,
    )?);
    let mut brief_in_args = false;
    if let Some(system_prompt_args) = &launch_spec.system_prompt_args {
        argv.extend(render(launch_spec, system_prompt_args, &values)?);
        brief_in_args = true;
    }
    Ok((argv, brief_in_args))
}

/// Renders one argument template, naming the template when it cannot be filled.
fn render(
    launch_spec: &LaunchSpec,
    template: &[String],
    values: &Values,
) -> Result<Vec<String>, CliError> {
    render_args(template, values).map_err(|err| {
        CliError::other(anyhow!(err).context(format!(
            "filling the launch arguments {template:?} of {:?}",
            launch_spec.interactive
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

/// Opens a cross-vendor reviewer on a task's latest completed attempt.
///
/// The order is the one [`spawn`] uses: every rule is checked and every decision
/// is made before the first side effect, so a refused review leaves nothing
/// behind. The effects then run in the order the packet needs: the files the
/// reviewer reads, the brief that points at them, and only then the tab.
pub fn review(
    mux: &dyn Mux,
    env: &ReviewEnv<'_>,
    req: &ReviewRequest,
) -> Result<ReviewReport, CliError> {
    let state = env.spawn.ctx.state()?;
    let task = spawn::task_of(&state, &req.task)?;
    let attempt = reviewed_attempt(task)?;
    let reviewer = choose_reviewer(
        env.reviewer.as_deref(),
        &attempt.agent,
        attempt.model.as_deref(),
        &env.spawn.programs,
        req.degraded,
    )?;
    if let Some(note) = &reviewer.note {
        eprintln!("{note}");
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::collections::BTreeMap;
    use std::process::Command;
    use std::time::Duration;

    use shprd_core::config::Skills;
    use shprd_core::event::Actor;
    use shprd_git::worktree::Layout;
    use shprd_mux::fake::{Call, FakeMux};
    use shprd_mux::naming;
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

    /// A fake CLI that runs other vendors' models, when `multi_model` says so,
    /// and wants the provider in the model name when `provider_qualified` does.
    fn fake_model_spec(
        id: &str,
        vendor: &str,
        multi_model: bool,
        provider_qualified: bool,
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
verified = false

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
                changed_paths: vec!["README.md".to_string()],
                commit: None,
                skills_used: Vec::new(),
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
            ReviewEnv {
                spawn: SpawnEnv {
                    ctx,
                    layout: Layout::default_for_home(self.home.path()),
                    briefs_dir: self.home.path().join(".shprd").join("briefs"),
                    run: "t1".to_string(),
                    base_branch: "integration/m4".to_string(),
                    repo_name: self.repo_name(),
                    programs,
                    workers: vec!["sh".to_string()],
                    skills: Skills::default(),
                    compat: Vec::new(),
                    base_env,
                    // No wait: a fake pane that never reports an agent would
                    // otherwise stall every test.
                    ready_wait: Duration::ZERO,
                },
                reviews_dir: self.home.path().join(".shprd").join("reviews"),
                reviewer: reviewer.map(str::to_string),
                // No wait before the retry either, so the two delivery tests run
                // through the same path without sleeping.
                brief_retry_wait: Duration::ZERO,
            }
        }

        /// A fake host that already has this repository's workspace.
        fn mux(&self) -> FakeMux {
            self.with_workspace(FakeMux::default().with_ready_agent())
        }

        /// A fake host with this repository's workspace that never detects an
        /// agent, so a brief typed into it cannot arrive until the test
        /// scripts the pane as ready.
        fn unready_mux(&self) -> FakeMux {
            self.with_workspace(FakeMux::default())
        }

        /// Records this repository's workspace in `mux`, as `shprd up` would.
        fn with_workspace(&self, mux: FakeMux) -> FakeMux {
            mux.create_workspace(&naming::workspace_name(&self.repo_name()), self.root())
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
                Call::Prompt(_, text) => Some(text),
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
        // The reviewer writes and stops: no shprd verb is asked of it, because
        // its folder is outside the repository and the orchestrator submits.
        assert!(!brief.contains("shprd agent"), "{brief}");
        assert!(brief.contains("findings.json` and stop"), "{brief}");
        assert!(brief.contains("REVIEWER"), "{brief}");
        assert!(!brief.contains(DEGRADED_LABEL), "{brief}");
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
                .all(|call| !matches!(call, Call::CreateTab(..))),
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
                Call::CreateTab(_, title, _) if title == &report.tab
            )),
            "no tab titled {:?} was opened",
            report.tab
        );
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
                Call::CreateTab(_, title, cwd) => Some((title.clone(), cwd.clone())),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no tab was created: {calls:?}"));
        assert_eq!(cwd, report.dir, "the tab must start in the packet");
        assert_eq!(cwd, packet_dir(&env.reviews_dir, "t1", "T1", 1));
        assert_eq!(
            cwd,
            scratch
                .home
                .path()
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
        // A reviewer is prefixed with its own glyph, so it never reads as a worker.
        assert_eq!(
            title,
            naming::tab_title(naming::Role::Reviewer, "codex", "T1", false)
        );
        assert!(title.starts_with('\u{25c7}'), "{title}");

        let pairs = calls
            .iter()
            .find_map(|call| match call {
                Call::Run(_, _, pairs) => Some(pairs.clone()),
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
                Some(report.dir.display().to_string())
            )),
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
                .any(|call| matches!(call, Call::Prompt(_, text) if text.contains("REVIEWER"))),
            "the brief was never typed in: {calls:?}"
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
        assert!(delivered[0].contains("REVIEWER"), "{delivered:?}");
        let landed = mux.calls().into_iter().find_map(|call| match call {
            Call::Prompt(pane, _) => Some(pane),
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
        assert_eq!(
            default_action,
            "resend the review brief into the reviewer tab"
        );
        assert_eq!(
            detection_polls(&mux),
            2,
            "the brief was retried exactly once before the escalation"
        );
        assert!(prompts(&mux).is_empty(), "nothing may be typed in");
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
            let (argv, _) = reviewer_argv(&spec.launch, Some("m1"), "brief", Path::new("b.md"))
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
        let (argv, _) = reviewer_argv(&spec.launch, Some("m1"), "brief", Path::new("b.md"))
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

    /// Every tab the fake host still has open in this repository's workspace.
    fn open_tabs(mux: &FakeMux, env: &ReviewEnv<'_>) -> Vec<String> {
        let workspace = spawn::workspace_of(mux, &env.spawn).unwrap_or_else(|err| panic!("{err}"));
        mux.list_tabs(&workspace.id)
            .unwrap_or_else(|err| panic!("{err}"))
            .into_iter()
            .map(|tab| tab.id.to_string())
            .collect()
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

        let outcome = crate::cmd::verdict::submit_verdict(
            &mux,
            &ctx,
            Some(scratch.home.path()),
            "T1",
            1,
            &findings,
        )
        .unwrap_or_else(|err| panic!("{err}"));

        assert_eq!(outcome, crate::cmd::verdict::Outcome::Passed);
        assert!(
            !open_tabs(&mux, &env).contains(&tab),
            "the reviewer tab {tab} outlived its verdict"
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
        let workspace = spawn::workspace_of(&mux, &env.spawn).unwrap_or_else(|err| panic!("{err}"));
        let worker = mux
            .create_tab(&workspace.id, "worker", scratch.root())
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
        crate::cmd::up::down(&mux, &ctx, Some(scratch.home.path()))
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
    fn degraded_is_ignored_when_the_reviewer_is_independent() {
        // `--degraded` is a permission, not a claim. When the reviewer's family
        // is known and differs from the author's, the review is independent no
        // matter what the flag said, so the label would be a lie.
        let scratch = Scratch::new();
        let ctx = scratch.ctx();
        scratch.reported_with(&ctx, "pi", Some("deepseek/deepseek-v4.1-flash"));
        let programs = family_programs();
        let chosen = choose_reviewer(
            Some("pi/claude/claude-opus-5-5"),
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
}
