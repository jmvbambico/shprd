//! `shprd land`: the run's work leaves as one pull request.
//!
//! Landing is the only place shprd talks to a forge, and the only place it
//! pushes. Everything it decides comes from the log: the run must be active,
//! no task may still be in flight, at least one task must have landed, and the
//! run-wide gate must have passed on the very head being pushed. A refusal
//! writes nothing and exits 2.
//!
//! With `--merge` the same verb applies `[land] merge`: a human's approval is
//! always enough, and the `allow` policy merges an unapproved pull request only
//! when its base branch is not protected.

use std::path::{Path, PathBuf};

use anyhow::anyhow;
use shprd_core::config::{self, Config, MergePolicy};
use shprd_core::event::{Event, EventKind, ResourceAction, ResourceKind};
use shprd_core::gate::REVIEW_GATE;
use shprd_core::state::{RunState, Task, TaskStatus};
use shprd_git::forge::{FakeForge, Forge, GhForge};
use shprd_git::git::Git;
use shprd_git::integrate;

use crate::cli::LandArgs;
use crate::ctx::{CliError, Ctx, resolve_home, resolve_layout};

/// Environment variable naming a scripted forge, used by the integration tests.
///
/// Production runs always use `gh`; only a test sets this, and then every forge
/// call goes to the script named here instead of the network.
pub const FAKE_FORGE_ENV: &str = "SHPRD_FAKE_FORGE";

/// The line a body ends with, as the run's policy requires.
const REVIEW_PASSED: &str = "cross-vendor-review: passed";

/// The line a body ends with when no independent review could be had.
const REVIEW_DEGRADED: &str = "degraded-review";

/// The attribution line every generated pull request ends with.
const ATTRIBUTION: &str = "🤖 Generated with [Claude Code](https://claude.com/claude-code)";

/// How much of a sha a human line shows.
const SHORT_SHA: usize = 7;

/// Opens the run's pull request, or with `--merge` applies the merge policy.
pub fn land_verb(ctx: &Ctx, args: &LandArgs, home: Option<&Path>) -> Result<(), CliError> {
    let config = load_config(ctx, home)?;
    let (state, events) = ctx.load()?;
    let run = state.run.as_ref().ok_or_else(|| {
        CliError::Invalid(anyhow!(
            "no run yet: start one with `shprd run start` before landing"
        ))
    })?;
    let run_id = run.run_id.clone();
    let base_branch = run.base_branch.clone();
    if state.finished {
        return Err(CliError::Invalid(anyhow!(
            "the run {run_id} has finished: there is nothing left to land"
        )));
    }
    check_in_flight(&state)?;
    let integrated = integrated_tasks(&state);
    if integrated.is_empty() {
        return Err(CliError::Invalid(anyhow!(
            "no task is integrated: land a task with `shprd agent integrate` first"
        )));
    }
    let layout = resolve_layout(home, &config.janitor.namespace)?;
    let repo_name = repo_name(ctx)?;
    let worktree =
        integrate::open_integration(&ctx.root, &layout, &repo_name, &run_id, &base_branch)
            .map_err(|err| {
                CliError::other(anyhow!(err).context("opening the integration worktree"))
            })?;
    let git = Git::new(&worktree.path);
    let head = git
        .head_sha()
        .map_err(|err| CliError::other(anyhow!(err).context("reading the integration head")))?;
    check_full_gate(&state, &head)?;

    if args.merge {
        return merge(ctx, &config, &worktree, &head);
    }
    open(ctx, &state, &events, &worktree, &integrated)
}

/// Refuses while any task is still working or waiting to be gated.
fn check_in_flight(state: &RunState) -> Result<(), CliError> {
    let busy = state
        .tasks
        .values()
        .find(|task| matches!(task.status, TaskStatus::Working | TaskStatus::Reported));
    match busy {
        Some(task) => Err(CliError::Invalid(anyhow!(
            "task {:?} is {:?}: land once every task is integrated",
            task.id,
            task.status
        ))),
        None => Ok(()),
    }
}

/// Every task that landed, in plan order, which is at least one by the caller.
fn integrated_tasks(state: &RunState) -> Vec<&Task> {
    state
        .tasks
        .values()
        .filter(|task| task.status == TaskStatus::Integrated)
        .collect()
}

/// Refuses unless the run-wide gate passed on exactly this head.
fn check_full_gate(state: &RunState, head: &str) -> Result<(), CliError> {
    match &state.last_full_gate {
        None => Err(CliError::Invalid(anyhow!(
            "no full gate has run: run `shprd gate` before landing"
        ))),
        Some((gate_head, false, _)) => Err(CliError::Invalid(anyhow!(
            "the last full gate failed on head {gate_head}: fix it and run `shprd gate` again"
        ))),
        Some((gate_head, true, _)) if gate_head != head => Err(CliError::Invalid(anyhow!(
            "the last full gate passed on head {gate_head}, but the integration branch is at \
             {head}: run `shprd gate` again"
        ))),
        Some((_, true, _)) => Ok(()),
    }
}

/// Pushes the branch, refreshes or opens the pull request, records it.
fn open(
    ctx: &Ctx,
    state: &RunState,
    events: &[Event],
    worktree: &integrate::IntegrationWorktree,
    integrated: &[&Task],
) -> Result<(), CliError> {
    let run = state
        .run
        .as_ref()
        .ok_or_else(|| CliError::other(anyhow!("no run to land")))?;
    let body = render_body(state, events);
    let body_file = write_body(worktree, &body)?;
    let title = match integrated.first() {
        Some(task) => format!("{}: {}", run.run_id, task.title),
        None => run.run_id.clone(),
    };
    push(worktree, state, ctx)?;
    let forge = forge(&worktree.path)?;
    let branch = worktree.branch.clone();
    let pr = match forge.find_open(&branch).map_err(|err| {
        CliError::other(anyhow!(err).context("asking the forge for the pull request"))
    })? {
        Some(pr) => {
            forge.edit_body(pr.number, &body_file).map_err(|err| {
                CliError::other(anyhow!(err).context("refreshing the pull request"))
            })?;
            pr
        }
        None => forge
            .create(&run.base_branch, &branch, &title, &body_file)
            .map_err(|err| CliError::other(anyhow!(err).context("opening the pull request")))?,
    };
    let event = ctx.commit(EventKind::PrOpened {
        number: pr.number,
        url: pr.url.clone(),
        head: branch,
    })?;
    crate::cmd::emit(ctx, &event)?;
    if !ctx.json {
        println!("{}", pr.url);
    }
    Ok(())
}

/// Pushes the integration branch, recording the remote branch once.
fn push(
    worktree: &integrate::IntegrationWorktree,
    state: &RunState,
    ctx: &Ctx,
) -> Result<(), CliError> {
    let git = Git::new(&worktree.path);
    git.run(&["push", "-u", "origin", &worktree.branch])
        .map_err(|err| {
            CliError::other(anyhow!(err).context(format!(
                "pushing {} to origin from {}",
                worktree.branch,
                worktree.path.display()
            )))
        })?;
    let key = (ResourceKind::RemoteBranch, worktree.branch.clone());
    if matches!(
        state.resources.get(&key).map(|record| record.status),
        Some(shprd_core::state::ResourceStatus::Live)
    ) {
        return Ok(());
    }
    ctx.commit(EventKind::Resource {
        action: ResourceAction::Created,
        kind: ResourceKind::RemoteBranch,
        name: worktree.branch.clone(),
        task: None,
    })?;
    Ok(())
}

/// Where the body is written: beside the integration worktree, in shprd's own
/// state directory, so no file of the user's repository is touched.
fn write_body(worktree: &integrate::IntegrationWorktree, body: &str) -> Result<PathBuf, CliError> {
    let dir = worktree
        .path
        .parent()
        .ok_or_else(|| CliError::other(anyhow!("the integration worktree has no parent")))?;
    let path = dir.join("pr-body.md");
    std::fs::write(&path, body).map_err(|err| {
        CliError::other(anyhow!(err).context(format!("writing {}", path.display())))
    })?;
    Ok(path)
}

/// The pull request body: the run's facts, the review marker, the attribution.
fn render_body(state: &RunState, events: &[Event]) -> String {
    let mut body = shprd_views::pr::pr_body(state, events);
    if !body.ends_with('\n') {
        body.push('\n');
    }
    body.push('\n');
    body.push_str(&review_marker(state));
    body.push('\n');
    body.push('\n');
    body.push_str(ATTRIBUTION);
    body.push('\n');
    body
}

/// The review marker for the body, followed by one line per task that made it
/// degraded.
///
/// `cross-vendor-review: passed` is a claim about recorded facts: every
/// integrated task's latest attempt needs a review that was not degraded and a
/// passed `review` gate. Anything else — no review requested, only degraded
/// reviews, no passed review gate — is `degraded-review`, and the reasons say
/// which tasks caused it and why.
fn review_marker(state: &RunState) -> String {
    let gaps = review_gaps(state);
    if gaps.is_empty() {
        return REVIEW_PASSED.to_string();
    }
    let mut marker = String::from(REVIEW_DEGRADED);
    for gap in gaps {
        marker.push_str("\n- ");
        marker.push_str(&gap);
    }
    marker
}

/// One line per integrated task whose latest attempt lacks an independent,
/// passing review.
fn review_gaps(state: &RunState) -> Vec<String> {
    let mut gaps = Vec::new();
    for task in integrated_tasks(state) {
        let Some(attempt) = task.latest_attempt() else {
            gaps.push(format!("{}: no attempt to review", task.id));
            continue;
        };
        if attempt.reviews.is_empty() {
            gaps.push(format!(
                "{}: no review was requested for attempt {}",
                task.id, attempt.n
            ));
            continue;
        }
        if !attempt.reviews.iter().any(|review| !review.degraded) {
            let reviewers: Vec<&str> = attempt
                .reviews
                .iter()
                .map(|review| review.reviewer.as_str())
                .collect();
            gaps.push(format!(
                "{}: every review requested for attempt {} was degraded ({})",
                task.id,
                attempt.n,
                reviewers.join(", ")
            ));
            continue;
        }
        let verdict = attempt
            .gates
            .iter()
            .rev()
            .find(|gate| gate.gate == REVIEW_GATE);
        if !matches!(verdict, Some(gate) if gate.passed) {
            gaps.push(format!(
                "{}: attempt {} has no passed review gate",
                task.id, attempt.n
            ));
        }
    }
    gaps
}

/// The short form of a sha, for the human line.
pub(crate) fn short(sha: &str) -> &str {
    sha.get(..SHORT_SHA).unwrap_or(sha)
}

/// Merges the run's pull request when the policy allows it.
fn merge(
    ctx: &Ctx,
    config: &Config,
    worktree: &integrate::IntegrationWorktree,
    head: &str,
) -> Result<(), CliError> {
    let state = ctx.state()?;
    let run = state
        .run
        .as_ref()
        .ok_or_else(|| CliError::other(anyhow!("no run to merge")))?;
    let pr = state.pr.as_ref().ok_or_else(|| {
        CliError::Invalid(anyhow!(
            "no pull request yet: run `shprd land` before `shprd land --merge`"
        ))
    })?;
    let url = pr.url.clone();
    let number = pr.number;
    let base = run.base_branch.clone();
    // The ledger's head is the branch, not the sha: the janitor deletes a
    // remote branch only when the merged pull request names it as its head.
    let branch = worktree.branch.clone();
    let forge = forge(&worktree.path)?;
    let status = forge.status(number).map_err(|err| {
        CliError::other(anyhow!(err).context("asking the forge for the pull request"))
    })?;
    if status.merged {
        // The forge merged the pull request without shprd. That is a fact
        // about this branch only when the forge merged the very commit the
        // integration branch is at now: a merge at an older head leaves
        // commits the pull request never contained, and recording them as
        // merged would let the janitor delete them.
        if status.head_sha != head {
            return Err(CliError::Invalid(anyhow!(
                "the pull request was merged at {}, but the integration branch is now at {}: \
                 open a new pull request with `shprd land`",
                short(&status.head_sha),
                short(head)
            )));
        }
        // The merge is recorded once and never twice.
        if !pr.merged {
            let event = ctx.commit(EventKind::PrMerged {
                number,
                head: branch,
            })?;
            crate::cmd::emit(ctx, &event)?;
        }
        println!("merged {url}");
        return Ok(());
    }
    if status.head_sha != head {
        return Err(CliError::Invalid(anyhow!(
            "the pull request is at head {}, but the integration branch is at {head}: push and \
             run `shprd land` again",
            status.head_sha
        )));
    }
    if status.checks_passed != Some(true) {
        return Err(CliError::Invalid(anyhow!(
            "the pull request's checks have not passed on {head}: wait for CI and try again"
        )));
    }
    let protected = config.land.protected.contains(&base);
    // An approval recorded in shprd counts exactly like the forge's, as long as
    // it names this head: what a human said about older commits says nothing
    // about the ones pushed since.
    let approved_here = pr.approved_head.as_deref() == Some(head);
    let allowed =
        status.approved || approved_here || (config.land.merge == MergePolicy::Allow && !protected);
    if !allowed {
        return Err(CliError::Invalid(anyhow!(
            "waiting for your approval: run `shprd approve`, or approve {url} on GitHub"
        )));
    }
    forge
        .merge(number)
        .map_err(|err| CliError::other(anyhow!(err).context("merging the pull request")))?;
    let event = ctx.commit(EventKind::PrMerged {
        number,
        head: branch,
    })?;
    crate::cmd::emit(ctx, &event)?;
    if !ctx.json {
        println!("merged {url}");
    }
    Ok(())
}

/// Which forge a run drives.
///
/// The fake forge exists for tests only, so the choice is a pure function of
/// the environment and the build: a release binary always drives `gh`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ForgeChoice {
    /// The script `SHPRD_FAKE_FORGE` names; debug builds only.
    Fake(PathBuf),
    /// The real `gh`, run against the worktree it is asked about.
    Gh,
}

/// The forge the environment asks for, honoured only in a debug build.
///
/// `debug` is true in a build with debug assertions, which is every test
/// binary; a release build ignores the variable entirely, so nothing but a
/// real merge can be recorded as one.
fn choose_forge(env_value: Option<String>, debug: bool) -> ForgeChoice {
    match env_value {
        Some(path) if debug && !path.is_empty() => ForgeChoice::Fake(PathBuf::from(path)),
        _ => ForgeChoice::Gh,
    }
}

/// The forge to drive: `gh` here, or, in a debug build, the script
/// `SHPRD_FAKE_FORGE` names.
fn forge(dir: &Path) -> Result<Box<dyn Forge>, CliError> {
    let env_value =
        std::env::var_os(FAKE_FORGE_ENV).map(|value| value.to_string_lossy().into_owned());
    match choose_forge(env_value, cfg!(debug_assertions)) {
        ForgeChoice::Fake(script) => {
            eprintln!("warning: using a fake forge ({FAKE_FORGE_ENV}); nothing reaches GitHub");
            let fake = FakeForge::from_script(&script).map_err(|err| {
                CliError::other(anyhow!(err).context("reading the scripted forge"))
            })?;
            Ok(Box::new(fake))
        }
        ForgeChoice::Gh => Ok(Box::new(GhForge::new(dir))),
    }
}

/// The config's `[land]`, with the repo file winning over the global one.
pub(crate) fn load_config(ctx: &Ctx, home: Option<&Path>) -> Result<Config, CliError> {
    let home = resolve_home(home)?;
    let global = home.join(".config").join("shprd").join("config.toml");
    let repo = ctx.root.join(".shprd").join("config.toml");
    config::load(Some(&global), Some(&repo))
        .map_err(|err| CliError::other(anyhow!(err).context("loading the config")))
}

/// The name of the repository directory, which keeps repos apart on disk.
pub(crate) fn repo_name(ctx: &Ctx) -> Result<String, CliError> {
    ctx.root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| CliError::other(anyhow!("{} has no directory name", ctx.root.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use shprd_core::event::{Actor, ReportStatus, ReviewSource};

    /// An event with `kind` at position `seq`.
    fn ev(seq: u64, kind: EventKind) -> Event {
        Event {
            seq,
            at_ms: seq * 1000,
            actor: Actor::Engine,
            kind,
        }
    }

    /// A run with one task spawned and reported, ready to be reviewed.
    fn reported_log(task: &str) -> Vec<Event> {
        vec![
            ev(
                1,
                EventKind::RunStarted {
                    run_id: "r1".to_string(),
                    repo: "/tmp/repo".to_string(),
                    base_branch: "main".to_string(),
                    integration_branch: "integration/m1".to_string(),
                },
            ),
            ev(
                2,
                EventKind::TaskAdded {
                    task: task.to_string(),
                    title: "a task".to_string(),
                    scope: vec!["a.txt".to_string()],
                    acceptance: vec!["it works".to_string()],
                    tags: Vec::new(),
                    skills: Vec::new(),
                    deps: Vec::new(),
                },
            ),
            ev(
                3,
                EventKind::Spawned {
                    task: task.to_string(),
                    attempt: 1,
                    agent: "w1".to_string(),
                    model: None,
                    worktree: "/tmp/wt".to_string(),
                    branch: "feature/t1".to_string(),
                },
            ),
            ev(
                4,
                EventKind::Reported {
                    task: task.to_string(),
                    attempt: 1,
                    status: ReportStatus::Completed,
                    summary: "done".to_string(),
                    tests: Vec::new(),
                    changed_paths: vec!["a.txt".to_string()],
                    commit: Some("abc123".to_string()),
                    skills_used: Vec::new(),
                },
            ),
        ]
    }

    /// Records a review dispatch at `seq`.
    fn reviewed(seq: u64, task: &str, reviewer: &str, degraded: bool) -> Event {
        ev(
            seq,
            EventKind::ReviewRequested {
                task: task.to_string(),
                attempt: 1,
                reviewer: reviewer.to_string(),
                degraded,
            },
        )
    }

    /// The provenance a real verdict carries.
    fn provenance(reviewer: &str) -> ReviewSource {
        ReviewSource {
            reviewer: reviewer.to_string(),
            findings_sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
                .to_string(),
        }
    }

    /// Records a passed `review` gate at `seq`, with its provenance.
    fn review_passed(seq: u64, task: &str) -> Event {
        ev(
            seq,
            EventKind::GatePassed {
                task: task.to_string(),
                attempt: 1,
                gate: REVIEW_GATE.to_string(),
                findings: Vec::new(),
                source: Some(provenance("c1")),
            },
        )
    }

    /// Records the task landing at `seq`.
    fn integrated(seq: u64, task: &str) -> Event {
        ev(
            seq,
            EventKind::Integrated {
                task: task.to_string(),
                commits: vec!["abc123".to_string()],
            },
        )
    }

    /// Folds `events`, panicking on a rule error.
    fn folded(events: &[Event]) -> RunState {
        match RunState::fold(events) {
            Ok(state) => state,
            Err(err) => panic!("fold failed: {err}"),
        }
    }

    #[test]
    fn a_clean_review_reads_as_passed() {
        let mut log = reported_log("T1");
        log.push(reviewed(5, "T1", "c1", false));
        log.push(review_passed(6, "T1"));
        log.push(integrated(7, "T1"));
        assert_eq!(review_marker(&folded(&log)), REVIEW_PASSED);
    }

    #[test]
    fn a_restarted_review_still_reads_as_one_clean_review() {
        // A brief that never arrived is requested again for the same attempt:
        // both requests describe the one review, so the marker still reads a
        // pass rather than a task reviewed twice.
        let mut log = reported_log("T1");
        log.push(reviewed(5, "T1", "c1", false));
        log.push(reviewed(6, "T1", "c1", false));
        log.push(review_passed(7, "T1"));
        log.push(integrated(8, "T1"));
        assert_eq!(review_marker(&folded(&log)), REVIEW_PASSED);
    }

    #[test]
    fn one_degraded_review_names_the_task() {
        let mut log = reported_log("T1");
        log.push(reviewed(5, "T1", "w2", true));
        log.push(review_passed(6, "T1"));
        log.push(integrated(7, "T1"));
        let marker = review_marker(&folded(&log));
        assert!(marker.starts_with(REVIEW_DEGRADED), "{marker}");
        assert!(!marker.contains(REVIEW_PASSED), "{marker}");
        assert!(marker.contains("T1"), "{marker}");
        assert!(marker.contains("every review requested"), "{marker}");
        assert!(marker.contains("w2"), "{marker}");
    }

    #[test]
    fn a_passed_review_gate_without_a_recorded_review_is_degraded() {
        let mut log = reported_log("T1");
        log.push(review_passed(5, "T1"));
        log.push(integrated(6, "T1"));
        let marker = review_marker(&folded(&log));
        assert!(marker.starts_with(REVIEW_DEGRADED), "{marker}");
        assert!(marker.contains("T1"), "{marker}");
        assert!(marker.contains("no review was requested"), "{marker}");
    }

    #[test]
    fn an_independent_review_without_a_passed_gate_is_degraded() {
        let mut log = reported_log("T1");
        log.push(reviewed(5, "T1", "c1", false));
        // Its own gates passed; only the review gate is missing.
        log.push(ev(
            6,
            EventKind::GatePassed {
                task: "T1".to_string(),
                attempt: 1,
                gate: "lint".to_string(),
                findings: Vec::new(),
                source: None,
            },
        ));
        log.push(integrated(7, "T1"));
        let marker = review_marker(&folded(&log));
        assert!(marker.starts_with(REVIEW_DEGRADED), "{marker}");
        assert!(marker.contains("no passed review gate"), "{marker}");
    }

    #[test]
    fn only_a_debug_build_honours_the_fake_forge() {
        let script = "/tmp/forge.json".to_string();
        assert_eq!(
            choose_forge(Some(script.clone()), false),
            ForgeChoice::Gh,
            "a release build never trusts SHPRD_FAKE_FORGE"
        );
        assert_eq!(
            choose_forge(Some(script.clone()), true),
            ForgeChoice::Fake(PathBuf::from(&script))
        );
        assert_eq!(choose_forge(None, true), ForgeChoice::Gh);
        assert_eq!(choose_forge(Some(String::new()), true), ForgeChoice::Gh);
    }
}
