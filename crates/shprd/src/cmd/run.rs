//! `shprd run …`: the run's lifecycle verbs.

use std::path::Path;

use shprd_core::event::EventKind;
use shprd_mux::Mux;

use crate::cli::{EndRunArgs, RunStartArgs};
use crate::cmd::up::EndRun;
use crate::cmd::{confirm, emit, spawn};
use crate::ctx::{CliError, Ctx};

/// Records the run's first event.
pub fn start(ctx: &Ctx, args: &RunStartArgs) -> Result<(), CliError> {
    let repo = ctx.root.to_string_lossy().to_string();
    let kind = EventKind::RunStarted {
        run_id: args.run_id.clone(),
        repo,
        base_branch: args.base.clone(),
        integration_branch: args.integration.clone(),
    };
    let event = ctx.commit(kind)?;
    emit(ctx, &event)
}

/// Records that the run is over, closing the tabs it left open.
///
/// It refuses exactly as `down` does: a run with unfinished work is a human's
/// decision, and `--abandon` is how they make it. That abandon asks a human at
/// a terminal, and asks before anything is written. The event is then checked
/// before the first close and committed after the last, so the tabs and panes
/// the host keeps are collected, reported and escalated while the ledger still
/// accepts an escalation.
pub fn finish(mux: &dyn Mux, ctx: &Ctx, home: Option<&Path>, end: EndRun) -> Result<(), CliError> {
    let state = ctx.state()?;
    let kind = crate::cmd::up::end_kind(&state, end)?;
    ctx.check(&kind)?;
    let watch = crate::cmd::up::watch_pane(ctx);
    spawn::close_at_run_end(mux, ctx, home, watch.as_ref());
    let event = ctx.commit(kind)?;
    crate::cmd::up::print_abandoned(&state, &event);
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

#[cfg(test)]
mod tests {
    use anyhow::{Context, Result, anyhow};
    use shprd_core::event::Actor;
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

        /// The context `finish` takes, with a run started.
        fn ctx(&self) -> Result<Ctx> {
            let ctx = Ctx::new(self.repo.path().to_path_buf(), Actor::Human, false, true);
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
        let workspace = mux.find_workspace("ws").map_err(|err| anyhow!("{err}"))?;
        let workspace = match workspace {
            Some(workspace) => workspace,
            None => mux
                .create_workspace("ws", fixture.repo.path())
                .map_err(|err| anyhow!("{err}"))?,
        };
        mux.create_tab(&workspace.id, "worker", fixture.repo.path())
            .map_err(|err| anyhow!("{err}"))
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
            .create_workspace("ws", fixture.repo.path())
            .map_err(|err| anyhow!("{err}"))?;
        let tab = fake
            .create_tab(&workspace.id, "worker", fixture.repo.path())
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
}
