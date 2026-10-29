//! `shprd janitor` and `shprd restore`: the archive-aware cleanup verbs.
//!
//! The plan comes from [`shprd_git::janitor::plan`], which reads only the run's
//! own ledger. `--apply` runs it and appends one `resource` event per step that
//! succeeded, through the same check-then-append path every other verb uses.
//! A dry run writes nothing at all.
//!
//! A run's review packet folder is shprd's own directory, at
//! `<home>/.shprd/reviews/<run>`. `--apply` adopts it while the run can still
//! record anything, so a finished run's removal is also in the ledger; a run
//! finished before any adoption has no record to close, and its folder is still
//! archived and removed, because the path comes from the layout and the run id
//! rather than from the ledger.

use std::path::Path;

use anyhow::anyhow;
use serde_json::json;
use shprd_core::event::{EventKind, ResourceAction, ResourceKind};
use shprd_core::log::{Clock, SystemClock};
use shprd_core::state::{ResourceStatus, RunState};

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
    let (mut state, mut events) = ctx.load()?;
    // A plan without a run would be empty; say so instead of printing nothing.
    let _ = run_id(&state)?;
    // Adopting first means the plan sees the folder as the ledger's own, which
    // is what lets `--apply` record its removal later. A dry run never writes,
    // so it plans from whatever ledger is already there.
    if args.apply && adopt_review_packets(ctx, &state, &layout)? {
        let reloaded = ctx.load()?;
        state = reloaded.0;
        events = reloaded.1;
    }
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

/// Records the run's review packet folder while the run can still write.
///
/// `run finish` refuses every creation, so a run that is already finished has
/// nothing to adopt with. While it is open — its pull request merged, say —
/// `--apply` adopts the folder exactly as `up` adopts a worktree it finds in the
/// layout, and the plan's later removal then also closes that record. Adopting
/// records the folder and nothing else: no packet is read, moved or deleted
/// here.
///
/// Returns whether an event was appended, so the caller can reload the state it
/// plans from.
fn adopt_review_packets(ctx: &Ctx, state: &RunState, layout: &Layout) -> Result<bool, CliError> {
    if !janitor::review_packets_adoptable(state) {
        return Ok(false);
    }
    let Some(run) = &state.run else {
        return Ok(false);
    };
    let Some(dir) = janitor::review_packets_dir(layout, &run.run_id) else {
        return Ok(false);
    };
    if !matches!(std::fs::symlink_metadata(&dir), Ok(meta) if meta.is_dir()) {
        return Ok(false);
    }
    let name = dir.display().to_string();
    let held = state
        .resources
        .get(&(ResourceKind::Worktree, name.clone()))
        .is_some_and(|record| {
            matches!(
                record.status,
                ResourceStatus::Live | ResourceStatus::Archived
            )
        });
    if held {
        return Ok(false);
    }
    ctx.commit(EventKind::Resource {
        action: ResourceAction::Created,
        kind: ResourceKind::Worktree,
        name,
        task: None,
    })?;
    Ok(true)
}

/// The run id the plan is about.
fn run_id(state: &shprd_core::state::RunState) -> Result<String, CliError> {
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

        /// Records the review packet folder live, as an earlier adoption would.
        fn adopt(&self, dir: &std::path::Path) {
            self.commit(EventKind::Resource {
                action: ResourceAction::Created,
                kind: ResourceKind::Worktree,
                name: dir.display().to_string(),
                task: None,
            });
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

        /// The whole log, so a test can tell a dry run wrote nothing.
        fn log_text(&self) -> String {
            match std::fs::read_to_string(&self.ctx.log_path) {
                Ok(text) => text,
                Err(err) => panic!("read log: {err}"),
            }
        }
    }

    #[test]
    fn a_dry_run_lists_review_packets_and_writes_nothing() {
        let fixture = Fixture::new();
        fixture.start();
        let dir = fixture.packets();
        // Never adopted, as a finished run normally is: the dry run has to
        // list the folder from the layout alone, without recording it.
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
        fixture.adopt(&dir);
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
                    kind: ResourceKind::Worktree,
                    ref name,
                    task: None,
                } if name == &dir.display().to_string()
            )
        });
        assert!(removed, "the removal must be in the ledger");
    }

    #[test]
    fn apply_adopts_review_packets_once_the_pull_request_merged() {
        let fixture = Fixture::new();
        fixture.start();
        let dir = fixture.packets();
        fixture.merge();

        // A merge lets an apply adopt the folder, because a run that has not
        // finished can still record it; it does not remove packets the run may
        // still be reading.
        fixture.apply();
        assert!(
            dir.join("T1-a1/findings.json").is_file(),
            "an unfinished run keeps its packets"
        );
        assert!(!fixture.archived().exists(), "nothing may be archived yet");
        let resources = fixture.resource_events();
        assert!(
            resources.iter().any(|kind| matches!(
                kind,
                EventKind::Resource {
                    action: ResourceAction::Created,
                    kind: ResourceKind::Worktree,
                    name,
                    task: None,
                } if name == &dir.display().to_string()
            )),
            "the adopted folder is recorded: {resources:?}"
        );

        // Once the run finishes, the adopted folder is archived and removed,
        // and the removal is recorded after the adoption.
        fixture.finish();
        fixture.apply();
        assert!(!dir.exists(), "the folder must be gone");
        assert!(
            fixture.archived().join("T1-a1/findings.json").is_file(),
            "the packets must survive in the archive"
        );
        let resources = fixture.resource_events();
        let created = resources.iter().position(|kind| {
            matches!(
                kind,
                EventKind::Resource {
                    action: ResourceAction::Created,
                    kind: ResourceKind::Worktree,
                    name,
                    task: None,
                } if name == &dir.display().to_string()
            )
        });
        let removed = resources.iter().position(|kind| {
            matches!(
                kind,
                EventKind::Resource {
                    action: ResourceAction::Removed,
                    kind: ResourceKind::Worktree,
                    name,
                    task: None,
                } if name == &dir.display().to_string()
            )
        });
        assert!(
            created.is_some_and(|created| removed.is_some_and(|removed| created < removed)),
            "the adoption is recorded before the removal: {resources:?}"
        );
    }

    #[test]
    fn apply_cleans_the_review_packets_of_a_run_finished_before_any_adoption() {
        let fixture = Fixture::new();
        fixture.start();
        let dir = fixture.packets();
        // The run finished without an earlier apply, so the ledger never
        // adopted the folder and, once finished, it never can. The apply must
        // still archive it and then remove it.
        fixture.finish();

        fixture.apply();
        assert!(!dir.exists(), "the folder must be gone");
        assert!(
            fixture.archived().join("T1-a1/findings.json").is_file(),
            "the packets must survive in the archive"
        );
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
