//! `shprd agent review`: hand a finished task's diff to a cross-vendor reviewer.
//!
//! A reviewer never gets a worktree. It gets one folder holding everything it may
//! read: the diff against the branch's merge base with the integration branch, the
//! contract the attempt was written against, and the schema its findings must
//! match. [`review`] takes the [`Mux`](shprd_mux::Mux) as a parameter, the way
//! [`spawn`] does, so a test drives a fake host and nothing here reads the
//! environment or starts a process of its own.

#![allow(dead_code)] // the next two commits wire these into the verb

use std::path::{Path, PathBuf};

use anyhow::anyhow;
use serde_json::{Value, json};
use shprd_core::event::ReportStatus;
use shprd_core::state::{Attempt, Task, TaskStatus};
use shprd_git::git::Git;

use crate::cmd::spawn::{self, SpawnEnv};
use crate::ctx::{CliError, Ctx};

/// The label a review carries when it is not cross-vendor, in the brief and on
/// the pull request.
pub const DEGRADED_LABEL: &str = "degraded-review";

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
        .join(format!("{task}-a{attempt}-review.tab"))
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
