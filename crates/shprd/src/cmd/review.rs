//! `shprd agent review`: hand a finished task's diff to a cross-vendor reviewer.
//!
//! A reviewer never gets a worktree. It gets one folder holding everything it may
//! read: the diff against the branch's merge base with the integration branch, the
//! contract the attempt was written against, and the schema its findings must
//! match. [`review`] takes the [`Mux`](shprd_mux::Mux) as a parameter, the way
//! [`spawn`] does, so a test drives a fake host and nothing here reads the
//! environment or starts a process of its own.

use std::path::{Path, PathBuf};

use anyhow::anyhow;
use serde_json::{Value, json};
use shprd_core::event::{EventKind, ReportStatus};
use shprd_core::state::{Attempt, Task, TaskStatus};
use shprd_git::git::Git;
use shprd_mux::{Mux, naming};
use shprd_roster::env::{EnvContext, LaunchEnv, Role as EnvRole, launch_env};
use shprd_roster::models::AgentRef;
use shprd_roster::spec::{HarnessSpec, LaunchSpec, Values, Vendor, render_args};

use crate::cli::ReviewArgs;
use crate::cmd::launch;
use crate::cmd::spawn::{self, Program, SpawnEnv};
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

/// The agent a review runs on, with everything needed to start it.
#[derive(Debug, Clone)]
pub struct Reviewer {
    /// The program name, which is the harness spec id.
    pub agent: String,
    /// The model to select, when the configured string carried one.
    pub model: Option<String>,
    /// How to start the program.
    pub launch: LaunchSpec,
    /// Whether this review is cross-vendor or degraded.
    pub degraded: bool,
}

/// Picks the reviewer for one attempt.
///
/// The rule is the one AGENTS.md states: a change is reviewed by an agent from a
/// different vendor than the one that wrote it, and a review that cannot meet
/// that rule is labelled rather than pretended. So the vendor comes from the two
/// harness specs, never from the program names, because two CLIs from one vendor
/// share a vendor even when they share no binary.
pub fn choose_reviewer(
    configured: Option<&str>,
    author: &str,
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
    let author_vendor = author_vendor(programs, author);
    let mut reviewer = Reviewer {
        agent: parsed.program,
        model: parsed.model,
        launch: program.spec.launch.clone(),
        degraded,
    };
    match author_vendor {
        None => {
            // Without the author's spec there is no way to prove the two differ,
            // so the review is only degraded when the caller says it may be.
            if !degraded {
                return Err(CliError::Invalid(anyhow!(
                    "no harness spec for `{author}`, the agent that wrote this change, so \
                     cross-vendor review cannot be checked: pass --degraded to review anyway"
                )));
            }
            eprintln!("warning: no harness spec for `{author}`: this review is {DEGRADED_LABEL}");
            reviewer.degraded = true;
        }
        Some(vendor) if vendor == program.spec.vendor => {
            if !degraded {
                return Err(CliError::Invalid(anyhow!(
                    "the reviewer `{raw}` and `{author}`, which wrote this change, are both \
                     {vendor:?}: set a reviewer from another vendor with `shprd set reviewer`, \
                     or pass --degraded to review anyway"
                )));
            }
            eprintln!(
                "warning: the reviewer `{raw}` shares the vendor {vendor:?} with `{author}`: \
                 this review is {DEGRADED_LABEL}"
            );
            reviewer.degraded = true;
        }
        Some(_) => {}
    }
    Ok(reviewer)
}

/// The vendor of `agent`'s harness spec, or `None` when the data directory has
/// no spec for it.
fn author_vendor(programs: &[Program], agent: &str) -> Option<Vendor> {
    programs
        .iter()
        .find(|program| program.spec.id == agent)
        .map(|program| program.spec.vendor)
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
    /// The absolute path the reviewer writes its findings to.
    pub findings: &'a str,
    /// Whether the review is degraded, and so must say so.
    pub degraded: bool,
}

/// Renders the brief a reviewer reads before its first turn.
///
/// It names the three files the reviewer may read, says plainly that the
/// repository is off limits, and ends with the command that records the verdict,
/// so the reviewer never has to guess how its work gets back into the run.
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
    out.push_str("Run this once `findings.json` is written:\n\n");
    out.push_str(&format!(
        "    shprd agent review-verdict --task {} --attempt {} --findings {}\n",
        input.task.id, input.attempt, input.findings
    ));
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
/// and the human can resend the brief into the tab.
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
    let delivery = launch::deliver_brief(mux, &tab.root_pane, &text, env.spawn.ready_wait);
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

/// The argv a reviewer's CLI starts with.
///
/// The order is [`spawn`]'s: the base command, the flags that suppress approval
/// prompts, then the model and finally the brief as the system prompt. The
/// second element says whether the brief went in as an argument, which is also
/// what says whether it has to be typed in instead.
fn reviewer_argv(
    launch_spec: &LaunchSpec,
    model: Option<&str>,
    brief: &str,
    brief_path: &Path,
) -> Result<(Vec<String>, bool), CliError> {
    let mut argv = launch_spec.interactive.clone();
    if let Some(auto) = &launch_spec.auto_approve {
        argv.extend(auto.iter().cloned());
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
        &env.spawn.programs,
        req.degraded,
    )?;
    let dir = write_packet(env, task, attempt)?;
    let findings = dir.join("findings.json");
    let brief_path = brief_path(&env.spawn.briefs_dir, &env.spawn.run, &task.id, attempt.n);
    let brief = ReviewBrief {
        task,
        run: &env.spawn.run,
        attempt: attempt.n,
        agent: &reviewer.agent,
        model: reviewer.model.as_deref(),
        dir: &dir.display().to_string(),
        findings: &findings.display().to_string(),
        degraded: reviewer.degraded,
    };
    spawn::write_brief(&brief_path, &render_review_brief(&brief))?;
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
