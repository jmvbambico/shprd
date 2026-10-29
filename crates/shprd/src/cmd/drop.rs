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
//! the verb closes is the worker's tab: an agent cannot be left typing into a
//! task nobody is waiting for.

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

/// Drops a task, closing the worker tab the run recorded for it.
///
/// Every refusal happens before the question, and the question before anything
/// is written: the event, the tab and the log are one decision, and a human who
/// says no gets the run they had.
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

    let event = ctx.commit(kind)?;
    crate::cmd::emit(ctx, &event)?;
    close_worker_tab(mux, home, &run.run_id, &args.task);
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

/// Closes the tab the run recorded for `task`'s attempts.
///
/// A host that keeps the tab is warned about, not fatal: the event is written
/// and the task is out of the run, so a tab left open is a thing to tidy, not a
/// reason to pretend the drop did not happen.
fn close_worker_tab(mux: &dyn Mux, home: Option<&Path>, run: &str, task: &str) {
    let Ok(home) = crate::ctx::resolve_home(home) else {
        return;
    };
    let briefs = home.join(".shprd").join("briefs");
    if let Err(err) = spawn::close_worker(mux, &briefs, run, task) {
        eprintln!("warning: {err}");
    }
}

#[cfg(test)]
mod tests {
    use anyhow::{Context, Result, anyhow};
    use shprd_core::event::{Actor, ReportStatus};
    use shprd_core::state::TaskStatus;
    use shprd_mux::fake::{Call, FakeMux};
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
