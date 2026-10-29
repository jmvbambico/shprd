//! `shprd janitor` and `shprd restore`: the archive-aware cleanup verbs.
//!
//! The plan comes from [`shprd_git::janitor::plan`], which reads only the run's
//! own ledger. `--apply` runs it and appends one `resource` event per step that
//! succeeded, through the same check-then-append path every other verb uses.
//! A dry run writes nothing at all.
//!
//! A run's review packet folder is shprd's own directory, at
//! `<home>/.shprd/reviews/<run>`, and it is spent once the run has finished.
//! `--apply` copies it aside, deletes it, and records the removal: the folder's
//! path comes from the layout and the run id rather than from a ledger record,
//! so a folder no earlier record ever held is cleaned all the same, and its
//! removal is the audit trail the log keeps.

use std::path::Path;

use anyhow::anyhow;
use serde_json::json;
use shprd_core::event::{EventKind, ResourceAction, ResourceKind};
use shprd_core::log::{Clock, SystemClock};
use shprd_core::state::RunState;

use crate::cli::{JanitorArgs, RestoreArgs};
use crate::cmd::emit;
use crate::ctx::{CliError, Ctx, resolve_home, resolve_layout};
use shprd_core::config::{self as config, Config};
use shprd_git::archive;
use shprd_git::janitor;
use shprd_git::worktree::{self, Layout};

/// Runs `shprd janitor`: print the plan, or apply it.
pub fn plan_verb(
    ctx: &Ctx,
    args: &JanitorArgs,
    home: Option<&std::path::Path>,
) -> Result<(), CliError> {
    let (loaded, layout) = config_and_layout(ctx, home)?;
    let archive_days = args.archive_days.unwrap_or(loaded.janitor.archive_days);
    let (state, events) = ctx.load()?;
    // A plan without a run would be empty; say so instead of printing nothing.
    let _ = run_id(&state)?;
    let now_ms = SystemClock.now_ms();
    let actions = janitor::plan(&state, &events, &layout, now_ms, archive_days);

    if !args.apply {
        print_plan(ctx, &actions);
        return Ok(());
    }

    let outcomes = janitor::execute(&ctx.root, &layout, &actions);
    let mut failures = 0u32;
    for outcome in &outcomes {
        match &outcome.result {
            Ok(kinds) => {
                for kind in kinds {
                    let event = ctx.commit(kind.clone())?;
                    emit(ctx, &event)?;
                }
            }
            Err(err) => {
                failures = failures.saturating_add(1);
                eprintln!("error: {}: {err}", outcome.action.describe());
            }
        }
    }
    if failures > 0 {
        return Err(CliError::other(anyhow!(
            "{failures} janitor action(s) failed; the rest were applied"
        )));
    }
    Ok(())
}

/// Runs `shprd restore <task>`: recreate the branch from its archive ref.
///
/// The ledger is checked before git: a restore that would not be recorded must
/// not create a branch at all. If appending the record then fails, the branch
/// this call just made is deleted again, so the branch and the ledger never
/// disagree.
pub fn restore_verb(
    ctx: &Ctx,
    args: &RestoreArgs,
    home: Option<&std::path::Path>,
) -> Result<(), CliError> {
    let (_config, layout) = config_and_layout(ctx, home)?;
    let state = ctx.state()?;
    let run = match &args.run {
        Some(run) => run.clone(),
        None => run_id(&state)?,
    };
    let kind = EventKind::Resource {
        action: ResourceAction::Created,
        kind: ResourceKind::Branch,
        name: layout.branch_name(&run, &args.task),
        task: Some(args.task.clone()),
    };
    ctx.check(&kind)?;
    let branch = archive::restore(&ctx.root, &layout, &run, &args.task)
        .map_err(|err| CliError::other(anyhow!(err).context("restoring the archived branch")))?;
    let event = match ctx.commit(kind) {
        Ok(event) => event,
        Err(err) => {
            let name = &branch;
            if let Err(undo) = worktree::delete_branch(&ctx.root, &layout, name) {
                return Err(CliError::other(anyhow!(err).context(format!(
                    "and the branch it created could not be removed: {undo}"
                ))));
            }
            return Err(err);
        }
    };
    emit(ctx, &event)
}

/// The resolved config and the layout its janitor settings describe.
///
/// The global file is `<home>/.config/shprd/config.toml`, the repo file is
/// `<repo>/.shprd/config.toml`, and the repo file wins.
fn config_and_layout(ctx: &Ctx, home: Option<&Path>) -> Result<(Config, Layout), CliError> {
    let home = resolve_home(home)?;
    let global = home.join(".config").join("shprd").join("config.toml");
    let repo = ctx.root.join(".shprd").join("config.toml");
    let loaded = config::load(Some(&global), Some(&repo))
        .map_err(|err| CliError::other(anyhow!(err).context("loading the config")))?;
    let layout = resolve_layout(Some(&home), &loaded.janitor.namespace)?;
    Ok((loaded, layout))
}

/// The run id the plan is about.
fn run_id(state: &RunState) -> Result<String, CliError> {
    match &state.run {
        Some(info) => Ok(info.run_id.clone()),
        None => Err(CliError::other(anyhow!(
            "no run in the log: start one with `shprd run start`"
        ))),
    }
}

/// Prints the plan, one line per step, and nothing else.
fn print_plan(ctx: &Ctx, actions: &[janitor::Action]) {
    if ctx.json {
        let lines: Vec<String> = actions.iter().map(|action| action.describe()).collect();
        println!("{}", json!({ "plan": lines }));
        return;
    }
    for action in actions {
        println!("would {}", action.describe());
    }
    if actions.is_empty() {
        println!("nothing to clean up");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use shprd_core::event::Actor;

    /// A temporary repo root and home, with a log the janitor can read.
    struct Fixture {
        _temp: tempfile::TempDir,
        ctx: Ctx,
        home: PathBuf,
    }

    impl Fixture {
        fn new() -> Fixture {
            let temp = match tempfile::tempdir() {
                Ok(temp) => temp,
                Err(err) => panic!("tempdir: {err}"),
            };
            let root = temp.path().join("repo");
            if let Err(err) = std::fs::create_dir_all(&root) {
                panic!("mkdir {}: {err}", root.display());
            }
            let ctx = Ctx::new(root, Actor::Engine, false, true);
            Fixture {
                home: temp.path().to_path_buf(),
                _temp: temp,
                ctx,
            }
        }

        /// Appends one event the way every writing verb does.
        fn commit(&self, kind: EventKind) {
            if let Err(err) = self.ctx.commit(kind) {
                panic!("commit: {err}");
            }
        }

        fn start(&self) {
            self.commit(EventKind::RunStarted {
                run_id: "r1".to_string(),
                repo: self.ctx.root.display().to_string(),
                base_branch: "main".to_string(),
                integration_branch: "integration/m1".to_string(),
            });
        }

        fn merge(&self) {
            self.commit(EventKind::PrOpened {
                number: 7,
                url: "https://example.invalid/pr/7".to_string(),
                head: "shprd/r1/int".to_string(),
            });
            self.commit(EventKind::PrMerged {
                number: 7,
                head: "abc1234".to_string(),
            });
        }

        /// Ends the run as a human: the engine may not finish one.
        fn finish(&self) {
            let human = Ctx::new(self.ctx.root.clone(), Actor::Human, false, true);
            if let Err(err) = human.commit(EventKind::RunFinished {
                abandoned: Vec::new(),
            }) {
                panic!("commit: {err}");
            }
        }

        /// The folder holding run `r1`'s packets, with one packet in it.
        fn packets(&self) -> PathBuf {
            let dir = self.home.join(".shprd").join("reviews").join("r1");
            let packet = dir.join("T1-a1");
            if let Err(err) = std::fs::create_dir_all(&packet) {
                panic!("mkdir {}: {err}", packet.display());
            }
            if let Err(err) = std::fs::write(packet.join("findings.json"), "{}") {
                panic!("write findings: {err}");
            }
            dir
        }

        fn archived(&self) -> PathBuf {
            self.home
                .join(".shprd")
                .join("archive")
                .join("reviews")
                .join("r1")
        }

        fn apply(&self) {
            let args = JanitorArgs {
                apply: true,
                archive_days: None,
            };
            if let Err(err) = plan_verb(&self.ctx, &args, Some(&self.home)) {
                panic!("apply: {err}");
            }
        }

        fn dry_run(&self) {
            let args = JanitorArgs {
                apply: false,
                archive_days: None,
            };
            if let Err(err) = plan_verb(&self.ctx, &args, Some(&self.home)) {
                panic!("dry run: {err}");
            }
        }

        /// The resource events the log currently holds.
        fn resource_events(&self) -> Vec<EventKind> {
            let events = match self.ctx.load() {
                Ok((_, events)) => events,
                Err(err) => panic!("load: {err}"),
            };
            events
                .into_iter()
                .map(|event| event.kind)
                .filter(|kind| matches!(kind, EventKind::Resource { .. }))
                .collect()
        }

        /// Adds `T1`, spawns it, records the worktree and branch it holds, then
        /// drops it — the state a dropped task leaves behind.
        fn dropped_task(&self) -> (PathBuf, String) {
            let layout = Layout::default_for_home(&self.home);
            let worktree = layout.path("repo", "r1", "T1");
            let branch = "shprd/r1/T1".to_string();
            self.commit(EventKind::TaskAdded {
                task: "T1".to_string(),
                title: "a task".to_string(),
                scope: vec!["a.txt".to_string()],
                acceptance: vec!["it works".to_string()],
                tags: Vec::new(),
                skills: Vec::new(),
                deps: Vec::new(),
            });
            self.commit(EventKind::Spawned {
                task: "T1".to_string(),
                attempt: 1,
                agent: "w1".to_string(),
                model: None,
                worktree: worktree.display().to_string(),
                branch: branch.clone(),
            });
            self.commit(EventKind::Resource {
                action: ResourceAction::Created,
                kind: ResourceKind::Worktree,
                name: worktree.display().to_string(),
                task: Some("T1".to_string()),
            });
            self.commit(EventKind::Resource {
                action: ResourceAction::Created,
                kind: ResourceKind::Branch,
                name: branch.clone(),
                task: Some("T1".to_string()),
            });
            let human = Ctx::new(self.ctx.root.clone(), Actor::Human, false, true);
            if let Err(err) = human.commit(EventKind::TaskDropped {
                task: "T1".to_string(),
                reason: "the human changed their mind".to_string(),
            }) {
                panic!("commit: {err}");
            }
            (worktree, branch)
        }

        /// The whole log, so a test can tell a dry run wrote nothing.
        fn log_text(&self) -> String {
            match std::fs::read_to_string(&self.ctx.log_path) {
                Ok(text) => text,
                Err(err) => panic!("read log: {err}"),
            }
        }
    }

    #[test]
    fn a_dropped_task_is_archived_before_its_worktree_and_branch_go() {
        // A human dropped T1: nobody will push its branch, so it is spent like
        // an integrated one. The archive comes first, so whatever the task did
        // is still recoverable with `shprd restore`.
        let fixture = Fixture::new();
        fixture.start();
        let (worktree, branch) = fixture.dropped_task();
        let (state, events) = match fixture.ctx.load() {
            Ok(loaded) => loaded,
            Err(err) => panic!("load: {err}"),
        };
        let layout = Layout::default_for_home(&fixture.home);
        let described: Vec<String> = janitor::plan(&state, &events, &layout, 0, 30)
            .iter()
            .map(janitor::Action::describe)
            .collect();

        let archive = format!("archive T1: {branch} -> refs/shprd/archive/");
        let remove = format!("remove worktree T1: {}", worktree.display());
        let delete = format!("delete branch T1: {branch}");
        assert_eq!(
            described,
            vec![archive.clone(), remove.clone(), delete.clone()],
            "one dropped task: archive, then its worktree, then its branch"
        );
    }

    #[test]
    fn a_dry_run_lists_review_packets_and_writes_nothing() {
        let fixture = Fixture::new();
        fixture.start();
        let dir = fixture.packets();
        // Never adopted: the run finished without an earlier apply, so nothing
        // recorded the folder, and a finished run can record no creation. The
        // dry run has to list the folder from the layout alone, without
        // recording it.
        fixture.finish();
        let before = fixture.log_text();

        fixture.dry_run();
        assert_eq!(fixture.log_text(), before, "a dry run writes nothing");
        assert!(
            dir.join("T1-a1/findings.json").is_file(),
            "the folder stays"
        );
        assert!(!fixture.archived().exists(), "nothing is archived");

        // The same plan the dry run printed names the folder it would archive.
        let (state, events) = match fixture.ctx.load() {
            Ok(loaded) => loaded,
            Err(err) => panic!("load: {err}"),
        };
        let layout = Layout::default_for_home(&fixture.home);
        let actions = janitor::plan(&state, &events, &layout, 0, 30);
        assert!(
            actions.iter().any(|action| matches!(
                action,
                janitor::Action::ArchiveReviewPackets { dir: planned, .. } if planned == &dir
            )),
            "the dry run must list the folder: {actions:?}"
        );
        assert!(
            actions
                .iter()
                .map(|action| action.describe())
                .any(|line| line == format!("archive review packets r1: {}", dir.display())),
            "the dry run must print the folder: {actions:?}"
        );
    }

    #[test]
    fn apply_archives_then_removes_review_packets_and_records_the_removal() {
        let fixture = Fixture::new();
        fixture.start();
        let dir = fixture.packets();
        fixture.finish();

        fixture.apply();
        assert!(!dir.exists(), "the folder must be gone");
        assert!(
            fixture.archived().join("T1-a1/findings.json").is_file(),
            "the packets must survive in the archive"
        );
        let removed = fixture.resource_events().into_iter().any(|kind| {
            matches!(
                kind,
                EventKind::Resource {
                    action: ResourceAction::Removed,
                    kind: ResourceKind::ReviewPackets,
                    ref name,
                    task: None,
                } if name == &dir.display().to_string()
            )
        });
        assert!(removed, "the removal must be in the ledger");
    }

    #[test]
    fn apply_keeps_and_records_nothing_for_a_run_that_has_not_finished() {
        let fixture = Fixture::new();
        fixture.start();
        let dir = fixture.packets();
        fixture.merge();

        // Packets feed the pull request body and the review gate, so a run
        // that has not finished keeps them — merged or not — and an apply
        // records no removal for them.
        fixture.apply();
        assert!(
            dir.join("T1-a1/findings.json").is_file(),
            "an unfinished run keeps its packets"
        );
        assert!(!fixture.archived().exists(), "nothing may be archived yet");
        assert!(
            fixture.resource_events().is_empty(),
            "nothing was removed, so nothing is recorded: {:?}",
            fixture.resource_events()
        );
    }

    #[test]
    fn apply_cleans_the_review_packets_of_a_run_finished_before_any_earlier_apply() {
        let fixture = Fixture::new();
        fixture.start();
        let dir = fixture.packets();
        // The run finished without an earlier apply, so nothing recorded the
        // folder and, once finished, it never can. The apply must still archive
        // the folder, remove it, and record that removal: a cleanup the log
        // cannot show is a cleanup nobody can audit.
        fixture.finish();

        fixture.apply();
        assert!(!dir.exists(), "the folder must be gone");
        assert!(
            fixture.archived().join("T1-a1/findings.json").is_file(),
            "the packets must survive in the archive"
        );
        let removed = fixture.resource_events().into_iter().any(|kind| {
            matches!(
                kind,
                EventKind::Resource {
                    action: ResourceAction::Removed,
                    kind: ResourceKind::ReviewPackets,
                    ref name,
                    task: None,
                } if name == &dir.display().to_string()
            )
        });
        assert!(
            removed,
            "the removal of a folder the ledger never held must still be recorded"
        );

        // The log still folds, which is what makes the record usable at all.
        let state = match fixture.ctx.state() {
            Ok(state) => state,
            Err(err) => panic!("the recorded removal must fold: {err}"),
        };
        assert!(state.finished, "the run is still finished");
    }

    #[test]
    fn a_failed_archive_leaves_the_review_packets_and_counts_as_a_failure() {
        let fixture = Fixture::new();
        fixture.start();
        let dir = fixture.packets();
        fixture.finish();
        // A file where the archive folder belongs, so the copy cannot start.
        let blocker = fixture.home.join(".shprd").join("archive");
        if let Err(err) = std::fs::write(&blocker, "not a folder") {
            panic!("write {}: {err}", blocker.display());
        }

        let args = JanitorArgs {
            apply: true,
            archive_days: None,
        };
        assert!(
            plan_verb(&fixture.ctx, &args, Some(&fixture.home)).is_err(),
            "the step is reported as a failure"
        );
        assert!(
            dir.join("T1-a1/findings.json").is_file(),
            "a failed archive must not lose the folder"
        );
    }

    #[test]
    fn a_run_that_is_still_working_keeps_its_review_packets() {
        let fixture = Fixture::new();
        fixture.start();
        let dir = fixture.packets();

        fixture.apply();
        assert!(
            dir.join("T1-a1/findings.json").is_file(),
            "the folder stays"
        );
        assert!(!fixture.archived().exists(), "nothing may be archived yet");
        assert!(
            fixture.resource_events().is_empty(),
            "an unfinished run's packets are never recorded or removed"
        );
    }
}
