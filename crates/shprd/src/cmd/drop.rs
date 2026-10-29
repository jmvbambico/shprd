//! `shprd drop <task> --reason <TEXT>`: a human decides a task will not be done.
//!
//! A run only ends when every task is integrated, so a task the human decides
//! not to do would block the run for ever: `land` has nothing to carry for it
//! and `down` refuses it as unfinished work. This verb is the decision that
//! unblocks the run. It is the human's alone — the same barrier `approve` and
//! the abandon of `down` use — and it is asked at a terminal before anything is
//! written, so a 'no' leaves the run exactly as it was.
//!
//! Nothing is thrown away here. The worktree and the branch stay where they
//! are, and the janitor archives the branch before it removes either, so
//! whatever the task did is still reachable with `shprd restore`. The one thing
//! the verb closes is the task's tabs, the worker's and any open reviewer's: an
//! agent cannot be left typing into a task nobody is waiting for, so a tab the
//! host refuses to close refuses the whole drop rather than warning about it.

use std::path::Path;

use anyhow::anyhow;
use shprd_core::event::EventKind;
use shprd_mux::Mux;

use crate::cli::DropArgs;
use crate::cmd::{confirm, spawn};
use crate::ctx::{CliError, Ctx};

/// Drops a task against the host the human is working in.
pub fn drop_verb(ctx: &Ctx, args: &DropArgs, home: Option<&Path>) -> Result<(), CliError> {
    drop_task(&spawn::real_mux(), ctx, args, home, &confirm::named_actor())
}

/// Drops a task, closing every tab the run recorded for it first.
///
/// Every rule refusal happens before the question, and the question before
/// anything is written: the event, the tabs and the log are one decision, and a
/// human who says no gets the run they had — with its agent still running.
pub fn drop_task(
    mux: &dyn Mux,
    ctx: &Ctx,
    args: &DropArgs,
    home: Option<&Path>,
    actor: &str,
) -> Result<(), CliError> {
    confirm::require_human(
        actor,
        "drops a task",
        "ask the maintainer to run `shprd drop`",
    )?;
    let state = ctx.state()?;
    let run = state.run.as_ref().ok_or_else(|| {
        CliError::Invalid(anyhow!(
            "no run yet: start one with `shprd run start` before dropping a task"
        ))
    })?;
    let task = state.tasks.get(&args.task).ok_or_else(|| {
        CliError::Invalid(anyhow!(
            "task {:?} is not in the run's plan: `shprd status` lists them",
            args.task
        ))
    })?;
    let kind = EventKind::TaskDropped {
        task: args.task.clone(),
        reason: args.reason.clone(),
    };
    // The rule decides which tasks may be dropped — not one that landed, not
    // one already dropped — and it is checked before the question, so a human
    // is never asked about a drop that would be refused.
    ctx.check(&kind)?;

    let question = format!(
        "drop {}? its branch is kept [y/N] ",
        dropped_phrase(&args.task, crate::summary::status_name(task.status), task)
    );
    if !confirm::answered(args.yes, "shprd drop", &question)? {
        return Err(CliError::Invalid(anyhow!(
            "not dropped: {} is still part of the run",
            args.task
        )));
    }

    // The agents go first. A tab left open is a worker that can keep typing
    // into a task the run has decided against — its branch may be archived and
    // its worktree gone by the time it commits — so a close that genuinely
    // fails refuses the drop, and nothing is written.
    stop_task_tabs(mux, home, &run.run_id, &args.task)?;
    let event = ctx.commit(kind)?;
    crate::cmd::emit(ctx, &event)?;
    if !ctx.json {
        println!(
            "dropped {} ({}); its worktree and branch are kept",
            args.task, args.reason
        );
    }
    Ok(())
}

/// The task, its status and the branch it sits on, as the question names them.
///
/// The branch is the handle on the work once the task is out of the run, so the
/// human is told which one they are keeping.
fn dropped_phrase(task: &str, status: &str, task_record: &shprd_core::state::Task) -> String {
    match task_record.attempts.last() {
        Some(attempt) => format!("task {task} ({status}, branch {})", attempt.branch),
        None => format!("task {task} ({status}, no branch: it never started)"),
    }
}

/// Closes every tab the run recorded for `task`, worker and reviewer alike.
///
/// The records a spawn and a review wrote beside the briefs are the only list of
/// tabs the task has, so this is where a drop finds them. A tab the host no
/// longer knows is closed already — a human shut it, say — which is the state
/// this verb wanted anyway; a tab the host keeps is a refusal, because the
/// agent in it is still able to act on work the run has taken away. The message
/// names the tab by id so the human can close it by hand and run the verb again.
fn stop_task_tabs(
    mux: &dyn Mux,
    home: Option<&Path>,
    run: &str,
    task: &str,
) -> Result<(), CliError> {
    let Ok(home) = crate::ctx::resolve_home(home) else {
        // No home means no briefs folder, so no tab was ever recorded for
        // anything: there is nothing to close.
        return Ok(());
    };
    let briefs = home.join(".shprd").join("briefs");
    spawn::close_task_tabs(mux, &briefs, run, task).map_err(|err| {
        CliError::Invalid(
            anyhow!(err).context(format!(
                "task {task} may still have an agent working in it: close the tab it names by hand, then run `shprd drop` again"
            )),
        )
    })
}

#[cfg(test)]
mod tests {
    use anyhow::{Context, Result, anyhow};
    use shprd_core::event::{Actor, ReportStatus};
    use shprd_core::state::TaskStatus;
    use shprd_mux::fake::{Call, FakeMux};
    use std::time::Duration;
    use tempfile::TempDir;

    use super::*;

    /// The run id every fixture starts, so a tab record finds its run folder.
    const RUN: &str = "rrun1";

    /// Turns a verb's failure into an [`anyhow::Error`], so an assertion prints
    /// the verb's own message.
    fn failed(err: CliError) -> anyhow::Error {
        anyhow!("{err}")
    }

    /// A temp home and a temp repository, with a run and one reported task.
    struct Fixture {
        /// Temp home: the run's briefs and tab records land here.
        home: TempDir,
        /// Temp repository, the run's root and log.
        repo: TempDir,
        /// The context the verb takes, writing as a human.
        ctx: Ctx,
    }

    impl Fixture {
        fn new() -> Result<Self> {
            let home = TempDir::new()?;
            let repo = TempDir::new()?;
            let ctx = Ctx::new(repo.path().to_path_buf(), Actor::Human, false, true);
            let fixture = Self { home, repo, ctx };
            fixture.commit(EventKind::RunStarted {
                run_id: RUN.to_string(),
                repo: fixture.repo.path().display().to_string(),
                base_branch: "main".to_string(),
                integration_branch: format!("shprd/{RUN}/int"),
            })?;
            Ok(fixture)
        }

        fn commit(&self, kind: EventKind) -> Result<()> {
            self.ctx
                .commit(kind)
                .map(|_| ())
                .map_err(|err| anyhow!("{err}"))
        }

        /// Adds `T1` and spawns it, leaving it Working.
        fn working(&self) -> Result<()> {
            self.commit(EventKind::TaskAdded {
                task: "T1".to_string(),
                title: "a task".to_string(),
                scope: vec!["a.txt".to_string()],
                acceptance: vec!["it works".to_string()],
                tags: Vec::new(),
                skills: Vec::new(),
                deps: Vec::new(),
            })?;
            self.commit(EventKind::Spawned {
                task: "T1".to_string(),
                attempt: 1,
                agent: "w1".to_string(),
                model: None,
                worktree: "/tmp/wt".to_string(),
                branch: "feature/t1".to_string(),
            })
        }

        /// Drives `T1` on to Reported.
        fn reported(&self) -> Result<()> {
            self.working()?;
            self.commit(EventKind::Reported {
                task: "T1".to_string(),
                attempt: 1,
                status: ReportStatus::Completed,
                summary: "done".to_string(),
                tests: Vec::new(),
                changed_paths: vec!["a.txt".to_string()],
                commit: Some("abc1234".to_string()),
                skills_used: Vec::new(),
            })
        }

        /// Drives `T1` on to Passed, with a passing `tests` gate.
        fn passed(&self) -> Result<()> {
            self.reported()?;
            self.commit(EventKind::GatePassed {
                task: "T1".to_string(),
                attempt: 1,
                gate: "tests".to_string(),
                findings: Vec::new(),
                source: None,
            })
        }

        /// Writes the tab record a spawn leaves beside the brief.
        fn record_tab(&self, tab: &shprd_mux::TabId) -> Result<()> {
            let dir = self.home.path().join(".shprd").join("briefs").join(RUN);
            std::fs::create_dir_all(&dir)?;
            std::fs::write(dir.join("T1-a1.tab"), tab.as_str())?;
            Ok(())
        }

        /// Writes the tab record a review leaves beside its brief.
        fn record_review_tab(&self, tab: &shprd_mux::TabId) -> Result<()> {
            let dir = self.home.path().join(".shprd").join("briefs").join(RUN);
            std::fs::create_dir_all(&dir)?;
            std::fs::write(dir.join("T1-a1-review.tab"), tab.as_str())?;
            Ok(())
        }

        fn home(&self) -> Option<&Path> {
            Some(self.home.path())
        }

        /// The log's text, so a test can tell a refusal wrote nothing.
        fn log_text(&self) -> Result<String> {
            std::fs::read_to_string(&self.ctx.log_path).context("reading the log")
        }

        /// The verb's arguments, with `--yes` as the tests provide it.
        fn args(&self) -> DropArgs {
            DropArgs {
                task: "T1".to_string(),
                reason: "not needed after all".to_string(),
                yes: true,
            }
        }
    }

    fn status(fixture: &Fixture) -> Result<TaskStatus> {
        let state = fixture.ctx.state().map_err(failed)?;
        let task = state.tasks.get("T1").context("the fixture added T1")?;
        Ok(task.status)
    }

    #[test]
    fn a_human_drops_a_reported_task_and_its_worker_tab_is_closed() -> Result<()> {
        let fixture = Fixture::new()?;
        fixture.reported()?;
        let fake = FakeMux::default();
        let tab = shprd_mux::TabId::new("tab-7");
        fixture.record_tab(&tab)?;

        drop_task(
            &fake,
            &fixture.ctx,
            &fixture.args(),
            fixture.home(),
            "human",
        )
        .map_err(failed)?;

        assert_eq!(status(&fixture)?, TaskStatus::Dropped);
        let state = fixture.ctx.state().map_err(failed)?;
        assert_eq!(
            state
                .tasks
                .get("T1")
                .and_then(|task| task.dropped_reason.clone()),
            Some("not needed after all".to_string())
        );
        assert!(
            fake.calls().contains(&Call::CloseTab(tab)),
            "the worker tab is closed: {:?}",
            fake.calls()
        );
        Ok(())
    }

    #[test]
    fn a_working_task_may_be_dropped() -> Result<()> {
        let fixture = Fixture::new()?;
        fixture.working()?;
        let fake = FakeMux::default();

        drop_task(
            &fake,
            &fixture.ctx,
            &fixture.args(),
            fixture.home(),
            "human",
        )
        .map_err(failed)?;

        assert_eq!(status(&fixture)?, TaskStatus::Dropped);
        Ok(())
    }

    #[test]
    fn an_orchestrator_may_not_drop_a_task() -> Result<()> {
        let fixture = Fixture::new()?;
        fixture.reported()?;
        let before = fixture.log_text()?;

        let err = drop_task(
            &FakeMux::default(),
            &fixture.ctx,
            &fixture.args(),
            fixture.home(),
            "orchestrator",
        )
        .err()
        .context("an orchestrator may not drop a task")?;

        assert_eq!(err.exit_code(), 2, "{err}");
        assert!(
            err.to_string().contains("SHPRD_ACTOR=orchestrator"),
            "{err}"
        );
        assert_eq!(fixture.log_text()?, before, "nothing was logged");
        Ok(())
    }

    #[test]
    fn a_drop_with_no_terminal_and_no_yes_is_refused_and_logs_nothing() -> Result<()> {
        let fixture = Fixture::new()?;
        fixture.reported()?;
        let before = fixture.log_text()?;
        let mut args = fixture.args();
        args.yes = false;

        // The test harness's stdin is not a terminal, which is exactly the case
        // an agent's shell tool is in.
        let err = drop_task(
            &FakeMux::default(),
            &fixture.ctx,
            &args,
            fixture.home(),
            "human",
        )
        .err()
        .context("a drop with no terminal and no --yes must be refused")?;

        assert_eq!(err.exit_code(), 2, "{err}");
        assert!(err.to_string().contains("shprd drop"), "{err}");
        assert_eq!(fixture.log_text()?, before, "nothing was logged");
        assert_eq!(status(&fixture)?, TaskStatus::Reported, "nothing changed");
        Ok(())
    }

    /// A host that keeps every tab it is asked to close.
    ///
    /// The real mux does this — a pane whose process will not die holds the tab
    /// open — and it is the case a drop must refuse rather than warn about: the
    /// agent in the tab is still able to act on the task.
    struct StuckTabMux {
        /// Where the calls are recorded, so a test can see the attempt.
        inner: FakeMux,
    }

    impl StuckTabMux {
        fn new() -> Self {
            Self {
                inner: FakeMux::default(),
            }
        }
    }

    impl Mux for StuckTabMux {
        fn capabilities(&self) -> shprd_mux::Capabilities {
            self.inner.capabilities()
        }
        fn find_workspace(&self, name: &str) -> shprd_mux::Result<Option<shprd_mux::Workspace>> {
            self.inner.find_workspace(name)
        }
        fn create_workspace(
            &self,
            name: &str,
            cwd: &Path,
        ) -> shprd_mux::Result<shprd_mux::Workspace> {
            self.inner.create_workspace(name, cwd)
        }
        fn focus_workspace(&self, workspace: &shprd_mux::WorkspaceId) -> shprd_mux::Result<()> {
            self.inner.focus_workspace(workspace)
        }
        fn list_tabs(
            &self,
            workspace: &shprd_mux::WorkspaceId,
        ) -> shprd_mux::Result<Vec<shprd_mux::Tab>> {
            self.inner.list_tabs(workspace)
        }
        fn create_tab(
            &self,
            workspace: &shprd_mux::WorkspaceId,
            title: &str,
            cwd: &Path,
        ) -> shprd_mux::Result<shprd_mux::Tab> {
            self.inner.create_tab(workspace, title, cwd)
        }
        fn rename_tab(&self, tab: &shprd_mux::TabId, title: &str) -> shprd_mux::Result<()> {
            self.inner.rename_tab(tab, title)
        }
        fn focus_tab(&self, tab: &shprd_mux::TabId) -> shprd_mux::Result<()> {
            self.inner.focus_tab(tab)
        }
        fn close_tab(&self, tab: &shprd_mux::TabId) -> shprd_mux::Result<()> {
            // The call is recorded either way, so a test can see shprd tried.
            let _ = self.inner.close_tab(tab);
            Err(shprd_mux::MuxError::command(
                "the stuck host",
                format!("tab {} is held open by a running process", tab.as_str()),
            ))
        }
        fn split(
            &self,
            pane: &shprd_mux::PaneId,
            direction: shprd_mux::Direction,
            ratio: f32,
            cwd: &Path,
        ) -> shprd_mux::Result<shprd_mux::PaneId> {
            self.inner.split(pane, direction, ratio, cwd)
        }
        fn run(
            &self,
            pane: &shprd_mux::PaneId,
            argv: &[String],
            env: &[(String, Option<String>)],
        ) -> shprd_mux::Result<()> {
            self.inner.run(pane, argv, env)
        }
        fn send_text(&self, pane: &shprd_mux::PaneId, text: &str) -> shprd_mux::Result<()> {
            self.inner.send_text(pane, text)
        }
        fn prompt(&self, pane: &shprd_mux::PaneId, text: &str) -> shprd_mux::Result<()> {
            self.inner.prompt(pane, text)
        }
        fn read(&self, pane: &shprd_mux::PaneId, lines: usize) -> shprd_mux::Result<String> {
            self.inner.read(pane, lines)
        }
        fn agent_state(
            &self,
            pane: &shprd_mux::PaneId,
        ) -> shprd_mux::Result<shprd_mux::AgentState> {
            self.inner.agent_state(pane)
        }
        fn next_events(&self, timeout: Duration) -> shprd_mux::Result<Vec<shprd_mux::MuxEvent>> {
            self.inner.next_events(timeout)
        }
    }

    #[test]
    fn a_worker_tab_the_host_keeps_refuses_the_drop_and_logs_nothing() -> Result<()> {
        let fixture = Fixture::new()?;
        fixture.reported()?;
        let fake = StuckTabMux::new();
        let tab = shprd_mux::TabId::new("tab-7");
        fixture.record_tab(&tab)?;
        let before = fixture.log_text()?;

        let err = drop_task(
            &fake,
            &fixture.ctx,
            &fixture.args(),
            fixture.home(),
            "human",
        )
        .err()
        .context("a tab the host keeps must refuse the drop")?;

        assert_eq!(err.exit_code(), 2, "{err}");
        let message = err.to_string();
        assert!(message.contains("tab-7"), "{message}");
        assert!(message.contains("shprd drop"), "{message}");
        assert_eq!(fixture.log_text()?, before, "nothing was logged");
        assert_eq!(status(&fixture)?, TaskStatus::Reported, "nothing changed");
        Ok(())
    }

    #[test]
    fn a_worker_tab_already_gone_counts_as_closed() -> Result<()> {
        // A human closed the tab before the drop: that is the state the verb
        // wants, not a refusal, so the drop goes ahead.
        let fixture = Fixture::new()?;
        fixture.reported()?;
        let fake = FakeMux::default();
        fixture.record_tab(&shprd_mux::TabId::new("tab-7"))?;

        drop_task(
            &fake,
            &fixture.ctx,
            &fixture.args(),
            fixture.home(),
            "human",
        )
        .map_err(failed)?;

        assert_eq!(status(&fixture)?, TaskStatus::Dropped);
        Ok(())
    }

    #[test]
    fn a_drop_closes_an_open_reviewer_tab_too() -> Result<()> {
        // A reviewer can be as talkative as a worker: its tab goes with the
        // worker's, or the review could still be written for a dropped task.
        let fixture = Fixture::new()?;
        fixture.reported()?;
        let fake = FakeMux::default();
        let worker = shprd_mux::TabId::new("tab-7");
        let reviewer = shprd_mux::TabId::new("tab-8");
        fixture.record_tab(&worker)?;
        fixture.record_review_tab(&reviewer)?;

        drop_task(
            &fake,
            &fixture.ctx,
            &fixture.args(),
            fixture.home(),
            "human",
        )
        .map_err(failed)?;

        assert!(
            fake.calls().contains(&Call::CloseTab(reviewer)),
            "the reviewer tab is closed: {:?}",
            fake.calls()
        );
        assert_eq!(status(&fixture)?, TaskStatus::Dropped);
        Ok(())
    }

    #[test]
    fn an_integrated_task_cannot_be_dropped() -> Result<()> {
        let fixture = Fixture::new()?;
        fixture.passed()?;
        fixture.commit(EventKind::Integrated {
            task: "T1".to_string(),
            commits: vec!["abc1234".to_string()],
        })?;
        let before = fixture.log_text()?;

        let err = drop_task(
            &FakeMux::default(),
            &fixture.ctx,
            &fixture.args(),
            fixture.home(),
            "human",
        )
        .err()
        .context("an integrated task may not be dropped")?;

        assert_eq!(err.exit_code(), 2, "{err}");
        assert_eq!(fixture.log_text()?, before, "nothing was logged");
        assert_eq!(status(&fixture)?, TaskStatus::Integrated);
        Ok(())
    }
}
