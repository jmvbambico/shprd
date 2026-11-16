//! `shprd run …`: the run's lifecycle verbs.
//!
//! `run start` records the run's first event; `run finish` records its last,
//! closing the tabs the run opened on the way. Both are the only verbs that
//! open and close a run: `up`, the launcher that used to do it from the
//! workspace, is gone, and the service calls these instead.

use std::path::Path;

use anyhow::anyhow;
use shprd_core::event::{Event, EventKind, ResourceAction, ResourceKind, TaskId};
use shprd_core::rules::{abandonable_tasks, unfinished_tasks, unlanded};
use shprd_core::state::RunState;
use shprd_git::git::Git;
use shprd_mux::{Host, PaneId};

use crate::cli::{EndRunArgs, RunStartArgs};
use crate::cmd::{confirm, emit, spawn};
use crate::ctx::{CliError, Ctx};

/// Records the run's first event, and makes the branch it integrates on.
///
/// A run whose integration branch does not exist cannot move: every start cuts
/// its worktree from that branch, so the first pass fails — every pass — and
/// nothing says why. So the branch is made here, from `--base`, and recorded in
/// the resource ledger like every other branch shprd creates. A base branch
/// that does not exist either is the person's mistake, and is refused (exit 2)
/// before anything is written.
///
/// A directory that is not a git repository is left alone: a run there is a
/// log-only run, which is what the record-only paths and the tests use.
pub fn start(ctx: &Ctx, args: &RunStartArgs) -> Result<(), CliError> {
    let repo = ctx.root.to_string_lossy().to_string();
    let kind = EventKind::RunStarted {
        run_id: args.run_id.clone(),
        repo,
        base_branch: args.base.clone(),
        integration_branch: args.integration.clone(),
    };
    // A refused start must leave nothing behind, so the run's own rule is asked
    // before the branch is made.
    ctx.check(&kind)?;
    let made = ensure_branch(&ctx.root, &args.base, &args.integration)?;
    let event = ctx.commit(kind)?;
    if made {
        ctx.commit(EventKind::Resource {
            action: ResourceAction::Created,
            kind: ResourceKind::Branch,
            name: args.integration.clone(),
            task: None,
        })?;
    }
    emit(ctx, &event)
}

/// Makes sure `branch` exists, cutting it from `base` when it does not.
///
/// Answers whether it had to create it, so the caller can record that in the
/// ledger exactly once. Every way it can fail is a refusal rather than a plain
/// error: a person who names a branch that is not there has to be told which
/// one, and the run must not start half made.
fn ensure_branch(repo: &Path, base: &str, branch: &str) -> Result<bool, CliError> {
    let git = Git::new(repo);
    // Not a repository at all: a log-only run, with nothing to cut from.
    if git.run(&["rev-parse", "--git-dir"]).is_err() {
        return Ok(false);
    }
    if git.branch_exists(branch).unwrap_or(false) {
        return Ok(false);
    }
    if git.rev_parse(base).is_err() {
        return Err(CliError::Invalid(anyhow!(
            "refusing to start run: branch {base:?} does not exist in {}, so {branch:?} cannot be \
             cut from it\nname an existing base with --base, or make {base:?} first (`git branch \
             {base}`)",
            repo.display()
        )));
    }
    git.run(&["branch", branch, base]).map_err(|err| {
        CliError::Invalid(anyhow!(
            "refusing to start run: could not create the integration branch {branch:?} from \
             {base:?}: {err}"
        ))
    })?;
    Ok(true)
}

/// Records that the run is over, closing the tabs it left open.
///
/// A run with unfinished work is a human's decision, and `--abandon` is how
/// they make it. That abandon asks a human at a terminal, and asks before
/// anything is written. The event is then checked before the first close and
/// committed after the last, so the tabs and panes the host keeps are
/// collected, reported and escalated while the ledger still accepts an
/// escalation.
pub fn finish(mux: &dyn Host, ctx: &Ctx, home: Option<&Path>, end: EndRun) -> Result<(), CliError> {
    let state = ctx.state()?;
    let kind = end_kind(&state, end)?;
    ctx.check(&kind)?;
    let watch = watch_pane(ctx);
    spawn::close_at_run_end(mux, ctx, home, watch.as_ref());
    let event = ctx.commit(kind)?;
    print_abandoned(&state, &event);
    emit(ctx, &event)
}

/// `shprd run finish` against the host the human is working in.
pub fn finish_verb(ctx: &Ctx, args: &EndRunArgs, home: Option<&Path>) -> Result<(), CliError> {
    finish(
        &spawn::real_mux(),
        ctx,
        home,
        EndRun::finish(args.abandon, args.yes).acting_as(confirm::named_actor()),
    )
}

/// The event that ends the run, or the refusal that says why it cannot.
///
/// A run is over only when every task reached a terminal state and the work
/// that landed is merged: the pull request the run opened has to have merged,
/// or the work sits on a branch nothing will pick up. Anything else is work a
/// human would lose, so it is refused with the tasks named one per line.
/// `abandon` is the explicit choice to end it anyway: the event then carries
/// the tasks that were left, which is what the log keeps for the human.
fn finish_kind(state: &RunState, abandon: bool) -> Result<EventKind, CliError> {
    let unfinished = unfinished_tasks(state);
    // What an abandon names: the unfinished work, plus the integrated work
    // while the pull request that would carry it has not merged. An abandon
    // with nothing to abandon is a normal finish, and goes through the checks
    // that carry no list.
    let abandoned = abandonable_tasks(state);
    if abandon && !abandoned.is_empty() {
        return Ok(EventKind::RunFinished { abandoned });
    }
    if !unfinished.is_empty() {
        return Err(CliError::Invalid(anyhow!(
            "refusing to end run {}\n{}\n{}",
            run_id(state),
            unfinished_lines(state, &unfinished),
            RESTART_ADVICE
        )));
    }
    if unlanded(state) {
        return Err(CliError::Invalid(anyhow!(
            "refusing to end run {}: every task is integrated, but the run's pull \
             request has not merged, so nothing carries the work yet\nrun `shprd \
             land` to open it and `shprd land --merge` to merge it, or\n{}",
            run_id(state),
            RESTART_ADVICE
        )));
    }
    Ok(EventKind::RunFinished {
        abandoned: Vec::new(),
    })
}

/// The event that ends the run, once a human has answered for an abandon.
///
/// [`finish_kind`] decides what the event is, and an abandon that leaves work
/// behind is the human's, so it is asked at a terminal before anything is
/// recorded.
fn end_kind(state: &RunState, end: EndRun) -> Result<EventKind, CliError> {
    let kind = finish_kind(state, end.abandon)?;
    let EventKind::RunFinished { abandoned } = &kind else {
        return Ok(kind);
    };
    // A finish with nothing to abandon is a normal finish: there is no work to
    // throw away, so there is nothing to ask.
    if abandoned.is_empty() {
        return Ok(kind);
    }
    confirm::require_human(
        &end.actor,
        "abandons a run",
        &format!("ask the maintainer to run `{}`", end.verb),
    )?;
    let question = abandon_question(state, abandoned);
    let answer = confirm::answered(end.yes, end.verb, &question)?;
    decided(state, abandoned, answer)?;
    Ok(kind)
}

/// The question an abandon asks, naming the run and everything it would leave.
///
/// Each task carries its status and its branch: the branch is the handle on the
/// work once the run is over, and the thing the human is agreeing to keep.
fn abandon_question(state: &RunState, abandoned: &[TaskId]) -> String {
    format!(
        "abandon run {} and leave: {}? their branches are kept [y/N] ",
        run_id(state),
        left_list(state, abandoned)
    )
}

/// One phrase per task being left: its status, and the branch that keeps it.
fn left_list(state: &RunState, abandoned: &[TaskId]) -> String {
    abandoned
        .iter()
        .map(|id| {
            let status = state
                .tasks
                .get(id)
                .map_or("unknown", |task| crate::summary::status_name(task.status));
            match task_branch(state, id) {
                Some(branch) => format!("{id} ({status}, branch {branch})"),
                None => format!("{id} ({status}, no branch: it never started)"),
            }
        })
        .collect::<Vec<String>>()
        .join(", ")
}

/// What a 'no' to the abandon question means: the run stays exactly as it was.
///
/// The event is never written, so nothing is logged, no tab is closed and the
/// branches stay put. A 'yes' hands the caller the event it already has.
fn decided(state: &RunState, abandoned: &[TaskId], answer: bool) -> Result<(), CliError> {
    if answer {
        return Ok(());
    }
    Err(CliError::Invalid(anyhow!(
        "not abandoned: run {} is still in flight, so nothing was logged and no tab was \
         closed\nkept: {}\nthe run is as it was: end it with `shprd run finish --abandon` \
         when you mean it",
        run_id(state),
        left_list(state, abandoned)
    )))
}

/// How the verb that ends a run was asked to end it.
///
/// The flags travel together with the command a refusal names, so a message
/// never points at a verb the caller did not run.
#[derive(Debug, Clone)]
pub(crate) struct EndRun {
    /// End the run even though tasks are unfinished, keeping their branches.
    pub abandon: bool,
    /// Answer the abandon's question for the record: a debug build only.
    pub yes: bool,
    /// The command a refusal names, e.g. `shprd run finish --abandon`.
    pub verb: &'static str,
    /// The actor the environment named, read at the verb's boundary.
    ///
    /// Empty is "nothing was named", which is a human. The constructors leave
    /// it empty, so the tests that end a run are not at the mercy of the
    /// `SHPRD_ACTOR` the test process inherited; a test that needs a named
    /// actor injects one with [`EndRun::acting_as`].
    actor: String,
}

impl EndRun {
    /// `shprd run finish` as the human typed it.
    pub const fn finish(abandon: bool, yes: bool) -> Self {
        Self {
            abandon,
            yes,
            verb: FINISH_VERB,
            actor: String::new(),
        }
    }

    /// The actor the environment named, for the abandon's human check.
    #[must_use]
    pub fn acting_as(mut self, named: impl Into<String>) -> Self {
        self.actor = named.into();
        self
    }

    /// A plain finish: it abandons nothing, so it asks nothing.
    ///
    /// The verb itself always carries the flags a human typed, so this exists
    /// for the tests that end a run without abandoning anything.
    #[cfg(test)]
    pub const fn plain() -> Self {
        Self::finish(false, false)
    }
}

/// The command that abandons through `shprd run finish`.
const FINISH_VERB: &str = "shprd run finish --abandon";

/// What a human can do instead of ending a run with work still in it.
///
/// The escape hatch says what it keeps: the branches their work sits on.
const RESTART_ADVICE: &str = "to end the run anyway and keep the branches, run \
     `shprd run finish --abandon`";

/// One line per unfinished task, naming the task and where it sits.
fn unfinished_lines(state: &RunState, unfinished: &[TaskId]) -> String {
    let lines: Vec<String> = unfinished
        .iter()
        .map(|id| {
            let status = state
                .tasks
                .get(id)
                .map_or("unknown", |task| crate::summary::status_name(task.status));
            format!("  {id}  {status}")
        })
        .collect();
    format!(
        "{} task(s) unfinished:\n{}",
        unfinished.len(),
        lines.join("\n")
    )
}

/// The run's identifier, for a message about it.
fn run_id(state: &RunState) -> &str {
    state.run.as_ref().map_or("?", |run| run.run_id.as_str())
}

/// Tells a human which branches keep the work an abandoned run left behind.
///
/// Nothing is printed for a run that abandoned nothing: a run that ended with
/// its work integrated and merged has no loose ends to name.
fn print_abandoned(state: &RunState, event: &Event) {
    let EventKind::RunFinished { abandoned } = &event.kind else {
        return;
    };
    for line in abandoned_lines(state, abandoned) {
        println!("{line}");
    }
}

/// The lines a human reads when a run is abandoned: what was left, and where.
///
/// The branch is the point: the run is over and its log is about to be
/// archived, so the branch name is the only handle on the work that survived.
fn abandoned_lines(state: &RunState, abandoned: &[TaskId]) -> Vec<String> {
    if abandoned.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![format!(
        "ended the run with unfinished tasks: {}",
        abandoned.join(", ")
    )];
    for id in abandoned {
        let status = state
            .tasks
            .get(id)
            .map_or("unknown", |task| crate::summary::status_name(task.status));
        lines.push(match task_branch(state, id) {
            Some(branch) => format!("  {id}  {status}  branch {branch}"),
            None => format!("  {id}  {status}  no branch: it never started"),
        });
    }
    lines
}

/// The branch a task's work sits on, as the attempt that made it recorded it.
///
/// A spawn names it from [`shprd_git::worktree::Layout::branch_name`] and the
/// log keeps it on the attempt, so this is the branch a human goes to and the
/// one the janitor would archive. A task nothing ever spawned has none: the
/// name a spawn would have made names nothing on disk.
fn task_branch<'a>(state: &'a RunState, id: &TaskId) -> Option<&'a str> {
    state
        .tasks
        .get(id)?
        .attempts
        .last()
        .map(|attempt| attempt.branch.as_str())
}

/// The watchdog's pane, which run end closes as its last act.
///
/// The watchdog watches a run that is over, so it has nothing left to do. The
/// pane is named here and closed by [`spawn::close_at_run_end`], so a pane the
/// host keeps is listed with everything else the run left open.
fn watch_pane(ctx: &Ctx) -> Option<PaneId> {
    let watch = ctx.state().ok().and_then(|state| state.watch)?;
    Some(PaneId::new(watch.pane))
}

#[cfg(test)]
mod tests {
    use anyhow::{Context, Result, anyhow};
    use shprd_core::event::{Actor, ReportStatus};
    use shprd_mux::fake::{Call, FakeMux};
    use tempfile::TempDir;

    use super::*;
    use crate::cmd::spawn::fault::{Fault, FlakyMux};

    /// The run id every fixture starts, so a tab record finds its run folder.
    const RUN: &str = "rrun1";

    /// Turns a verb's failure into an [`anyhow::Error`], which is what a test
    /// returns so a failing assertion prints the verb's own message.
    fn failed(err: CliError) -> anyhow::Error {
        anyhow!("{err}")
    }

    /// A temp home and a temp repository, with a run in flight.
    struct Fixture {
        /// Temp home: the run's briefs and tab records land here.
        home: TempDir,
        /// Temp repository, the run's root and log.
        repo: TempDir,
    }

    impl Fixture {
        /// A temp home and a temp repository, with no run in the log yet.
        fn new() -> Result<Self> {
            Ok(Self {
                home: TempDir::new()?,
                repo: TempDir::new()?,
            })
        }

        /// A context with no run in its log, for a test that records its own.
        fn empty(&self) -> Ctx {
            Ctx::new(self.repo.path().to_path_buf(), Actor::Human, false, true)
        }

        /// The context `finish` takes, with a run started.
        fn ctx(&self) -> Result<Ctx> {
            let ctx = self.empty();
            ctx.commit(EventKind::RunStarted {
                run_id: RUN.to_string(),
                repo: self.repo.path().display().to_string(),
                base_branch: "main".to_string(),
                integration_branch: format!("shprd/{RUN}/int"),
            })
            .map_err(|err| anyhow!("{err}"))?;
            Ok(ctx)
        }

        /// Where the run's tab records belong.
        fn briefs(&self) -> std::path::PathBuf {
            self.home.path().join(".shprd").join("briefs").join(RUN)
        }

        /// Writes a tab record, the way a spawn leaves one beside the brief.
        fn record(&self, name: &str, tab: &shprd_mux::TabId) -> Result<()> {
            std::fs::create_dir_all(self.briefs())?;
            std::fs::write(self.briefs().join(name), tab.as_str())?;
            Ok(())
        }
    }

    /// The log's escalations, as their task and body.
    fn escalations(ctx: &Ctx) -> Result<Vec<(Option<String>, String)>> {
        let (_, events) = ctx.load().map_err(|err| anyhow!("{err}"))?;
        Ok(events
            .into_iter()
            .filter_map(|event| match event.kind {
                EventKind::Escalated { task, body, .. } => Some((task, body)),
                _ => None,
            })
            .collect())
    }

    /// A host with one workspace, and a tab in it.
    fn a_tab(mux: &FakeMux, fixture: &Fixture) -> Result<shprd_mux::Tab> {
        let workspace = mux.find_space("ws").map_err(|err| anyhow!("{err}"))?;
        let workspace = match workspace {
            Some(workspace) => workspace,
            None => mux
                .create_space("ws", fixture.repo.path())
                .map_err(|err| anyhow!("{err}"))?,
        };
        mux.spawn(&workspace.id, "worker", &[], &[], fixture.repo.path())
            .map_err(|err| anyhow!("{err}"))
    }

    /// Starts a run with the id the end-of-run tests expect.
    fn started(ctx: &Ctx) -> Result<()> {
        ctx.commit(EventKind::RunStarted {
            run_id: "rabc123".to_string(),
            repo: "/tmp/repo".to_string(),
            base_branch: "main".to_string(),
            integration_branch: "shprd/rabc123/int".to_string(),
        })
        .map_err(failed)?;
        Ok(())
    }

    /// Starts a run, then adds one task and spawns an agent for it.
    fn working_task(ctx: &Ctx) -> Result<()> {
        started(ctx)?;
        ctx.commit(EventKind::TaskAdded {
            task: "M4.1".to_string(),
            title: "the run's lifecycle".to_string(),
            scope: vec!["crates/shprd".to_string()],
            acceptance: vec!["the run ends".to_string()],
            tags: Vec::new(),
            skills: Vec::new(),
            deps: Vec::new(),
        })
        .map_err(failed)?;
        ctx.commit(EventKind::Spawned {
            task: "M4.1".to_string(),
            attempt: 1,
            agent: "claude/claude-opus-5-5".to_string(),
            model: None,
            worktree: "/tmp/repo/wt".to_string(),
            branch: "feature/m4-up".to_string(),
        })
        .map_err(failed)?;
        Ok(())
    }

    /// Reports the working task's attempt completed: it waits on its gates.
    fn reported_task(ctx: &Ctx) -> Result<()> {
        working_task(ctx)?;
        ctx.commit(EventKind::Reported {
            task: "M4.1".to_string(),
            attempt: 1,
            status: ReportStatus::Completed,
            summary: "done".to_string(),
            tests: Vec::new(),
            checks: vec![shprd_core::event::CheckReport::passed(
                "tests",
                "cargo test",
            )],
            changed_paths: Vec::new(),
            commit: None,
            skills_used: Vec::new(),
            proofs: Vec::new(),
        })
        .map_err(failed)?;
        Ok(())
    }

    /// The reported task's first gate passed, so it waits to be integrated.
    fn passed_task(ctx: &Ctx) -> Result<()> {
        reported_task(ctx)?;
        ctx.commit(EventKind::GatePassed {
            task: "M4.1".to_string(),
            attempt: 1,
            gate: "tests".to_string(),
            findings: Vec::new(),
            source: None,
        })
        .map_err(failed)?;
        Ok(())
    }

    /// The passed task integrated, with the pull request carrying it merged.
    ///
    /// This is the one shape of run that ends without a human's say-so: every
    /// task landed and the pull request carrying that work merged.
    fn landed_task(ctx: &Ctx) -> Result<()> {
        passed_task(ctx)?;
        ctx.commit(EventKind::Integrated {
            task: "M4.1".to_string(),
            commits: vec!["abc1234".to_string()],
        })
        .map_err(failed)?;
        ctx.commit(EventKind::PrOpened {
            number: 7,
            url: "https://example.invalid/pr/7".to_string(),
            head: "shprd/rabc123/int".to_string(),
        })
        .map_err(failed)?;
        ctx.commit(EventKind::PrMerged {
            number: 7,
            head: "abc1234".to_string(),
        })
        .map_err(failed)?;
        Ok(())
    }

    /// What an end-of-run test needs: a run in a temp repository, and a host.
    fn end_fixture() -> Result<(Fixture, Ctx, FakeMux)> {
        let fixture = Fixture::new()?;
        let ctx = fixture.empty();
        Ok((fixture, ctx, FakeMux::default()))
    }

    #[test]
    fn finish_escalates_the_worker_tab_the_host_will_not_close() -> Result<()> {
        let fixture = Fixture::new()?;
        let ctx = fixture.ctx()?;
        let fake = FakeMux::default();
        let tab = a_tab(&fake, &fixture)?;
        fixture.record("T2-a1.tab", &tab.id)?;
        let mux = FlakyMux::new(fake, Fault::CloseTab);

        finish(&mux, &ctx, Some(fixture.home.path()), EndRun::plain()).map_err(failed)?;

        let recorded = escalations(&ctx)?;
        assert_eq!(recorded.len(), 1, "one escalation: {recorded:?}");
        let body = &recorded[0].1;
        assert!(
            body.contains("worker tab for T2"),
            "names what it is: {body}"
        );
        assert!(body.contains(tab.id.as_str()), "names its id: {body}");
        assert!(
            ctx.state().map_err(failed)?.finished,
            "the run ended anyway"
        );
        Ok(())
    }

    #[test]
    fn finish_escalates_the_watchdog_pane_the_host_will_not_close() -> Result<()> {
        let fixture = Fixture::new()?;
        let ctx = fixture.ctx()?;
        ctx.commit(EventKind::WatchLaunched {
            pane: "pane-99".to_string(),
            process_group: None,
        })
        .map_err(|err| anyhow!("{err}"))?;
        let mux = FakeMux::default().failing_close_pane("the pane is busy");

        finish(&mux, &ctx, Some(fixture.home.path()), EndRun::plain()).map_err(failed)?;

        let recorded = escalations(&ctx)?;
        assert_eq!(recorded.len(), 1, "one escalation: {recorded:?}");
        let body = &recorded[0].1;
        assert!(body.contains("watchdog pane"), "names what it is: {body}");
        assert!(body.contains("pane-99"), "names its id: {body}");
        Ok(())
    }

    #[test]
    fn finish_records_no_escalation_when_every_tab_and_pane_closes() -> Result<()> {
        let fixture = Fixture::new()?;
        let ctx = fixture.ctx()?;
        ctx.commit(EventKind::WatchLaunched {
            pane: "pane-99".to_string(),
            process_group: None,
        })
        .map_err(|err| anyhow!("{err}"))?;
        let mux = FakeMux::default();
        let tab = a_tab(&mux, &fixture)?;
        fixture.record("T2-a1.tab", &tab.id)?;

        finish(&mux, &ctx, Some(fixture.home.path()), EndRun::plain()).map_err(failed)?;

        assert!(
            escalations(&ctx)?.is_empty(),
            "nothing was left over, so there is nothing to escalate"
        );
        let closed = mux
            .calls()
            .into_iter()
            .any(|call| matches!(call, Call::CloseTab(closed) if closed == tab.id));
        assert!(closed, "the recorded tab is closed: {:?}", mux.calls());
        assert!(ctx.state().map_err(failed)?.finished, "the run ended");
        Ok(())
    }

    /// Run end takes every task space and leaves the coordinator's alone.
    ///
    /// A run with no task in flight still has the spaces its tasks were given
    /// (a dropped or abandoned one, say); ending the run is what removes them,
    /// and the repository-level space is where a person's coordinator lives, so
    /// it is never shprd's to close.
    #[test]
    fn finish_closes_the_tasks_spaces_and_keeps_the_coordinators() -> Result<()> {
        let fixture = Fixture::new()?;
        let ctx = fixture.ctx()?;
        ctx.commit(EventKind::TaskAdded {
            task: "T2".to_string(),
            title: "abandoned".to_string(),
            scope: vec!["crates/shprd".to_string()],
            acceptance: vec!["it works".to_string()],
            tags: Vec::new(),
            skills: Vec::new(),
            deps: Vec::new(),
        })
        .map_err(|err| anyhow!("{err}"))?;
        let mux = FakeMux::default();
        let repo_name = fixture
            .repo
            .path()
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        let coordinator = mux
            .create_space(&spawn::repo_space_name(&repo_name), fixture.repo.path())
            .map_err(|err| anyhow!("{err}"))?;
        let task = mux
            .create_space(
                &spawn::task_space_name(&repo_name, RUN, "T2"),
                fixture.repo.path(),
            )
            .map_err(|err| anyhow!("{err}"))?;

        finish(
            &mux,
            &ctx,
            Some(fixture.home.path()),
            EndRun::finish(true, true),
        )
        .map_err(failed)?;

        let spaces: Vec<shprd_mux::SpaceId> = mux
            .calls()
            .into_iter()
            .filter_map(|call| match call {
                Call::CloseSpace(closed) => Some(closed),
                _ => None,
            })
            .collect();
        assert_eq!(spaces, vec![task.id.clone()], "only the task's space goes");
        assert!(
            mux.find_space(&spawn::repo_space_name(&repo_name))
                .map_err(|err| anyhow!("{err}"))?
                .is_some(),
            "the coordinator's space stayed"
        );
        assert!(
            mux.find_space(&spawn::task_space_name(&repo_name, RUN, "T2"))
                .map_err(|err| anyhow!("{err}"))?
                .is_none(),
            "the task's space stayed"
        );
        assert!(coordinator.id != task.id);
        Ok(())
    }

    #[test]
    fn finish_counts_a_pane_and_tab_already_gone_as_closed() -> Result<()> {
        let fixture = Fixture::new()?;
        let ctx = fixture.ctx()?;
        ctx.commit(EventKind::WatchLaunched {
            pane: "pane-404".to_string(),
            process_group: None,
        })
        .map_err(|err| anyhow!("{err}"))?;
        let fake = FakeMux::default();
        let workspace = fake
            .create_space("ws", fixture.repo.path())
            .map_err(|err| anyhow!("{err}"))?;
        let tab = fake
            .spawn(&workspace.id, "worker", &[], &[], fixture.repo.path())
            .map_err(|err| anyhow!("{err}"))?;
        fixture.record("T2-a1.tab", &tab.id)?;
        fake.close_tab(&tab.id).map_err(|err| anyhow!("{err}"))?;

        finish(&fake, &ctx, Some(fixture.home.path()), EndRun::plain()).map_err(failed)?;

        assert!(
            escalations(&ctx)?.is_empty(),
            "the host already closed both, so nothing is left over"
        );
        Ok(())
    }

    #[test]
    fn a_refused_finish_closes_no_tab() -> Result<()> {
        let fixture = Fixture::new()?;
        let ctx = fixture.ctx()?;
        ctx.commit(EventKind::TaskAdded {
            task: "T1".to_string(),
            title: "working".to_string(),
            scope: vec!["crates/shprd".to_string()],
            acceptance: vec!["it works".to_string()],
            tags: Vec::new(),
            skills: Vec::new(),
            deps: Vec::new(),
        })
        .map_err(|err| anyhow!("{err}"))?;
        ctx.commit(EventKind::Spawned {
            task: "T1".to_string(),
            attempt: 1,
            agent: "claude/claude-opus-5-5".to_string(),
            model: None,
            worktree: "/tmp/wt".to_string(),
            branch: "shprd/r/T1".to_string(),
        })
        .map_err(|err| anyhow!("{err}"))?;
        let mux = FakeMux::default();
        let tab = a_tab(&mux, &fixture)?;
        fixture.record("T1-a1.tab", &tab.id)?;

        let err = finish(&mux, &ctx, Some(fixture.home.path()), EndRun::plain())
            .err()
            .context("finish must refuse a working task")?;

        assert_eq!(err.exit_code(), 2, "{err}");
        assert!(
            !mux.calls()
                .iter()
                .any(|call| matches!(call, Call::CloseTab(_))),
            "a refused finish closes no tab: {:?}",
            mux.calls()
        );
        assert!(!ctx.state().map_err(failed)?.finished);
        Ok(())
    }

    #[test]
    fn finish_refuses_while_a_task_is_working() -> Result<()> {
        let (fixture, ctx, mux) = end_fixture()?;
        working_task(&ctx)?;
        let err = finish(&mux, &ctx, Some(fixture.home.path()), EndRun::plain())
            .err()
            .context("finish must refuse")?;
        assert_eq!(err.exit_code(), 2, "{err}");
        let message = err.to_string();
        assert!(message.contains("working"), "{message}");
        assert!(message.contains("shprd run finish --abandon"), "{message}");
        assert!(
            !ctx.state().map_err(failed)?.finished,
            "a refused finish writes nothing"
        );
        Ok(())
    }

    #[test]
    fn finish_refuses_a_reported_task() -> Result<()> {
        let (fixture, ctx, mux) = end_fixture()?;
        reported_task(&ctx)?;
        let before = ctx.state().map_err(failed)?.last_seq;
        let err = finish(&mux, &ctx, Some(fixture.home.path()), EndRun::plain())
            .err()
            .context("finish must refuse a reported task")?;

        assert_eq!(err.exit_code(), 2, "{err}");
        let message = err.to_string();
        assert!(message.contains("M4.1"), "{message}");
        assert!(message.contains("reported"), "{message}");
        assert!(message.contains("1 task(s) unfinished"), "{message}");
        assert!(message.contains("shprd run finish --abandon"), "{message}");
        let state = ctx.state().map_err(failed)?;
        assert!(!state.finished, "a refused finish writes nothing");
        assert_eq!(state.last_seq, before, "a refusal appends no event");
        Ok(())
    }

    #[test]
    fn finish_refuses_a_passed_task_that_was_never_integrated() -> Result<()> {
        let (fixture, ctx, mux) = end_fixture()?;
        passed_task(&ctx)?;
        let err = finish(&mux, &ctx, Some(fixture.home.path()), EndRun::plain())
            .err()
            .context("finish must refuse work that was never integrated")?;

        assert_eq!(err.exit_code(), 2, "{err}");
        assert!(err.to_string().contains("M4.1"), "{err}");
        // The word a human needs is the status the task is actually in.
        assert!(err.to_string().contains("passed"), "{err}");
        assert!(!ctx.state().map_err(failed)?.finished);
        Ok(())
    }

    #[test]
    fn finish_refuses_landed_work_whose_pull_request_has_not_merged() -> Result<()> {
        let (fixture, ctx, mux) = end_fixture()?;
        passed_task(&ctx)?;
        ctx.commit(EventKind::Integrated {
            task: "M4.1".to_string(),
            commits: vec!["abc1234".to_string()],
        })
        .map_err(failed)?;
        let err = finish(&mux, &ctx, Some(fixture.home.path()), EndRun::plain())
            .err()
            .context("finish must refuse work nothing carries")?;

        assert_eq!(err.exit_code(), 2, "{err}");
        assert!(err.to_string().contains("shprd land"), "{err}");
        assert!(
            err.to_string().contains("shprd run finish --abandon"),
            "{err}"
        );
        assert!(!ctx.state().map_err(failed)?.finished);
        Ok(())
    }

    #[test]
    fn finish_abandon_ends_a_run_whose_pull_request_never_merged() -> Result<()> {
        // The refusal above names `--abandon` as the way out, so it has to work
        // even though nothing is unfinished: it ends the run naming the landed
        // task whose work the unmerged pull request would have carried.
        let (fixture, ctx, mux) = end_fixture()?;
        passed_task(&ctx)?;
        ctx.commit(EventKind::Integrated {
            task: "M4.1".to_string(),
            commits: vec!["abc1234".to_string()],
        })
        .map_err(failed)?;
        let before = ctx.state().map_err(failed)?;
        finish(
            &mux,
            &ctx,
            Some(fixture.home.path()),
            EndRun::finish(true, true),
        )
        .map_err(failed)?;

        let state = ctx.state().map_err(failed)?;
        assert!(state.finished, "the abandon ends the run");
        assert_eq!(
            state.abandoned,
            vec!["M4.1".to_string()],
            "the landed task is what the unmerged pull request left behind"
        );
        let lines = abandoned_lines(&before, &state.abandoned);
        assert!(
            lines.iter().any(|line| line.contains("integrated")),
            "each abandoned task is named with its status: {lines:?}"
        );
        Ok(())
    }

    #[test]
    fn finish_finishes_a_run_whose_work_landed_and_merged() -> Result<()> {
        let (fixture, ctx, mux) = end_fixture()?;
        landed_task(&ctx)?;
        finish(&mux, &ctx, Some(fixture.home.path()), EndRun::plain()).map_err(failed)?;

        let state = ctx.state().map_err(failed)?;
        assert!(state.finished);
        assert!(
            state.abandoned.is_empty(),
            "a run that landed everything abandoned nothing"
        );
        Ok(())
    }

    #[test]
    fn finish_abandon_records_the_tasks_it_left_and_their_branches() -> Result<()> {
        let (fixture, ctx, mux) = end_fixture()?;
        reported_task(&ctx)?;
        let before = ctx.state().map_err(failed)?;
        finish(
            &mux,
            &ctx,
            Some(fixture.home.path()),
            EndRun::finish(true, true),
        )
        .map_err(failed)?;

        let state = ctx.state().map_err(failed)?;
        assert!(state.finished, "abandon is the explicit way to end the run");
        assert_eq!(state.abandoned, vec!["M4.1".to_string()]);
        let lines = abandoned_lines(&before, &state.abandoned);
        assert!(
            lines.iter().any(|line| line.contains("feature/m4-up")),
            "a human is told the branch that keeps the work: {lines:?}"
        );
        assert!(
            lines.iter().any(|line| line.contains("reported")),
            "each abandoned task is named with its status: {lines:?}"
        );
        Ok(())
    }

    #[test]
    fn finish_refuses_and_abandons_its_unfinished_work() -> Result<()> {
        let (fixture, ctx, mux) = end_fixture()?;
        reported_task(&ctx)?;
        let err = finish(&mux, &ctx, Some(fixture.home.path()), EndRun::plain())
            .err()
            .context("finish must refuse unfinished work")?;
        assert_eq!(err.exit_code(), 2, "{err}");
        assert!(!ctx.state().map_err(failed)?.finished);

        finish(
            &mux,
            &ctx,
            Some(fixture.home.path()),
            EndRun::finish(true, true),
        )
        .map_err(failed)?;
        let state = ctx.state().map_err(failed)?;
        assert!(state.finished);
        assert_eq!(state.abandoned, vec!["M4.1".to_string()]);
        Ok(())
    }

    #[test]
    fn an_abandon_refuses_an_actor_that_is_not_a_human() -> Result<()> {
        let (fixture, ctx, mux) = end_fixture()?;
        reported_task(&ctx)?;
        let before = ctx.state().map_err(failed)?;

        // The actor is injected, not inherited: the process may run with any
        // `SHPRD_ACTOR` and this still tests the refusal itself.
        let err = finish(
            &mux,
            &ctx,
            Some(fixture.home.path()),
            EndRun::finish(true, true).acting_as("engine"),
        )
        .err()
        .context("an abandon must refuse a named non-human actor")?;

        assert_eq!(err.exit_code(), 2, "{err}");
        assert!(err.to_string().contains("SHPRD_ACTOR=engine"), "{err}");
        let after = ctx.state().map_err(failed)?;
        assert!(!after.finished, "the refused abandon wrote nothing");
        assert_eq!(
            after.last_seq, before.last_seq,
            "a refusal appends no event"
        );
        Ok(())
    }

    #[test]
    fn the_abandon_question_names_the_run_the_tasks_and_their_branches() -> Result<()> {
        let (_fixture, ctx, _mux) = end_fixture()?;
        reported_task(&ctx)?;
        let state = ctx.state().map_err(failed)?;
        let question = abandon_question(&state, &abandonable_tasks(&state));

        let run_id = state
            .run
            .as_ref()
            .context("the run started")?
            .run_id
            .clone();
        assert!(question.contains(&run_id), "{question}");
        assert!(question.contains("M4.1"), "{question}");
        assert!(question.contains("reported"), "{question}");
        assert!(
            question.contains("feature/m4-up"),
            "the branch that keeps the work is named: {question}"
        );
        assert!(
            question.contains("[y/N]"),
            "the question says what an answer looks like: {question}"
        );
        Ok(())
    }

    /// A 'no' at the question writes nothing: no event, so no closed tab and no
    /// abandoned list. The decision is taken from the answer alone, so the test
    /// needs no terminal to make it.
    #[test]
    fn answering_no_to_the_abandon_question_leaves_the_run_as_it_was() -> Result<()> {
        let (_fixture, ctx, _mux) = end_fixture()?;
        reported_task(&ctx)?;
        let before = ctx.state().map_err(failed)?;
        let abandoned = abandonable_tasks(&before);
        assert!(!abandoned.is_empty(), "there is work to leave behind");

        let err = decided(&before, &abandoned, false)
            .err()
            .context("a no refuses")?;
        assert_eq!(err.exit_code(), 2, "{err}");
        assert!(err.to_string().contains("not abandoned"), "{err}");

        let after = ctx.state().map_err(failed)?;
        assert!(!after.finished, "the run is still in flight");
        assert!(after.abandoned.is_empty(), "nothing was abandoned");
        Ok(())
    }

    #[test]
    fn finish_ends_a_run_with_nothing_in_flight() -> Result<()> {
        let fixture = Fixture::new()?;
        let ctx = fixture.ctx()?;
        finish(
            &FakeMux::default(),
            &ctx,
            Some(fixture.home.path()),
            EndRun::plain(),
        )
        .map_err(failed)?;
        assert!(ctx.state().map_err(failed)?.finished);
        Ok(())
    }
}
