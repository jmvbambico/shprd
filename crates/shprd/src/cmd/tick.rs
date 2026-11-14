//! `shprd tick`: run the service's advance pass once, here and now.
//!
//! The service runs this same pass on an interval (`shprd serve`). The verb
//! exists for the two cases a loop does not cover: a human who wants to see what
//! the next pass would do without waiting for it, and a script that drives a run
//! where nothing is serving.
//!
//! With `--json` it prints the actions and their results as one document:
//!
//! ```json
//! {
//!   "steps": [
//!     {
//!       "action": "start",            // start | review | verdict | land
//!                                     // | mark_landed | needs_you | nudge
//!                                     // | backup | give_up
//!       "task": "T1",
//!       "ok": true,
//!       "detail": "started T1 attempt 1 with claude in ...",
//!       "seq": 42                     // the log line it appended, when it did
//!     }
//!   ]
//! }
//! ```
//!
//! A pass that told the coordinator something carries it too, one entry per
//! space, with the lines that were typed into its pane:
//!
//! ```json
//! { "steps": [], "news": [{ "space": "shprd", "lines": ["<shprd-news …>…"] }] }
//! ```
//!
//! A repository with no remote, or no forge, lands its tasks on the run's
//! integration branch in shprd's own worktree instead of through a pull
//! request; the policy is the same one a forge landing reads.
//!
//! Without JSON, one line per action and one per told line. A pass whose steps
//! all worked exits 0; one that could not read the run exits 1. A step that
//! failed is printed and stays in the report — it never stops the other steps,
//! so it is not the verb's own failure.

use std::path::Path;
use std::sync::Arc;

use anyhow::anyhow;
use shprd_core::config;
use shprd_core::event::Actor;
use shprd_git::forge::{Forge, GhForge};
use shprd_server::{NewsDesk, Starter, TickReport, Worker};

use crate::cli::TickArgs;
use crate::ctx::{CliError, Ctx, resolve_home};

/// Environment variable naming a scripted forge, used by the integration tests.
///
/// Production runs always use `gh`; only a test sets this, and then every forge
/// call goes to the script named here instead of the network. It is the same
/// variable `shprd land` honours, so a test drives one forge through both paths.
pub const FAKE_FORGE_ENV: &str = "SHPRD_FAKE_FORGE";

/// Runs one advance pass.
pub fn tick_verb(ctx: &Ctx, args: &TickArgs, home: Option<&Path>) -> Result<(), CliError> {
    let home = resolve_home(home)?;
    let config = load_config(ctx, &home)?;
    let worker = worker(ctx, &home, config)?;
    let report = worker
        .tick()
        .map_err(|err| CliError::other(anyhow!(err).context("running a tick pass")))?;
    print_report(ctx, args, &report);
    Ok(())
}

/// The worker one pass runs against: the repo's log, the real forge and host,
/// and the binary's own spawn path.
pub(crate) fn worker(ctx: &Ctx, home: &Path, config: config::Config) -> Result<Worker, CliError> {
    worker_with_hub(ctx, home, config, shprd_server::hub::Hub::new())
}

/// [`worker`], publishing its writes on `hub`.
///
/// `shprd serve` passes the service's own counter, so a pass's writes wake the
/// long polls and sockets an open page waits on. The `tick` verb has nobody
/// waiting on it and passes a fresh one.
pub(crate) fn worker_with_hub(
    ctx: &Ctx,
    home: &Path,
    config: config::Config,
    hub: shprd_server::hub::Hub,
) -> Result<Worker, CliError> {
    let repo = ctx.root.clone();
    let actor = shprd_core::event::Actor::Engine;
    let forge: shprd_server::SharedForge = std::sync::Arc::from(forge(&repo)?);
    // The live overlay is the host's word: a task with an agent in its tab is
    // at work, and a pass must not land a commit an agent is still writing. A
    // host that cannot be asked answers "nothing is live", which is what a
    // record-only start in a test also reports.
    let live_repo = repo.display().to_string();
    let live_home = home.to_path_buf();
    let live: shprd_server::LiveFn = std::sync::Arc::new(move |state| {
        if record_only() {
            return Default::default();
        }
        let Some(name) = Path::new(&live_repo)
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
        else {
            return Default::default();
        };
        crate::cmd::spawn::live_tasks(&crate::cmd::spawn::real_mux(), &live_home, &name, state)
    });
    let facts: std::sync::Arc<dyn shprd_server::FactsSource> =
        std::sync::Arc::new(shprd_server::LiveFacts {
            forge: forge.clone(),
            live,
        });
    let spawn_home = home.to_path_buf();
    let spawn_actor = actor.clone();
    let start: Starter = std::sync::Arc::new(move |repo: &Path, task: &str| {
        let spawn_ctx = Ctx::new(repo.to_path_buf(), spawn_actor.clone(), false, true);
        crate::cmd::agent::start_for_tick(&spawn_ctx, &spawn_home, task)
    });
    // The engine starts the reviewer on a hand-in, as `shprd agent review` does:
    // the closure is that verb's own `review()`, so the cross-vendor rule, the
    // packet, the brief and the kickoff cannot drift between the two callers. A
    // pass plans a review step only when the settings name a reviewer (the
    // service asks on every pass) and the caller is not record-only.
    let review_home = home.to_path_buf();
    let review_actor = actor.clone();
    let review: Option<shprd_server::Reviewer> = (!record_only()).then(|| {
        std::sync::Arc::new(move |repo: &Path, task: &str| {
            let review_ctx = Ctx::new(repo.to_path_buf(), review_actor.clone(), false, true);
            crate::cmd::review::review_for_tick(&review_ctx, &review_home, task)
        }) as shprd_server::Reviewer
    });
    // A verdict closes the reviewer's tab, and the tab belongs to the host, so a
    // pass that decides a review calls the same `review::close_reviewer` the
    // verb does. A record-only pass has no tabs at all.
    let close_home = home.to_path_buf();
    let close_reviewer: Option<shprd_server::ReviewCloser> = (!record_only()).then(|| {
        std::sync::Arc::new(move |repo: &Path, task: &str, attempt: u32| {
            let ctx = Ctx::new(repo.to_path_buf(), Actor::Engine, false, true);
            let run = ctx
                .state()
                .ok()
                .and_then(|state| state.run.map(|run| run.run_id))
                .ok_or_else(|| String::from("the run has not started"))?;
            let briefs = close_home.join(".shprd").join("briefs");
            crate::cmd::review::close_reviewer(
                &crate::cmd::spawn::real_mux(),
                &briefs,
                &run,
                task,
                attempt,
            )
            .map_err(|err| err.to_string())
        }) as shprd_server::ReviewCloser
    });
    // The fallback ladder's host: transcript facts, sign-in probes, and the two
    // effects it may have. A record-only pass has no tabs and no CLIs to probe,
    // so it is not wired there.
    let fallback: Option<shprd_server::FallbackSourceArc> = if record_only() {
        None
    } else {
        let name = repo
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        ctx.state()
            .ok()
            .and_then(|state| state.run.map(|run| run.run_id))
            .map(|run| {
                std::sync::Arc::new(crate::cmd::live_fallback::LiveFallback::new(
                    &repo, home, &run, &name,
                )) as shprd_server::FallbackSourceArc
            })
    };
    // The news' delivery: the coordinator's pane, through the host adapter. A
    // record-only pass has no tabs to type into, so it is not wired there.
    let news = if record_only() {
        None
    } else {
        Some(NewsDesk {
            coordinator: Arc::new(crate::cmd::live_news::LiveNews::new(Box::new(
                crate::cmd::spawn::real_mux(),
            ))),
            settle_ms: settle_ms(home, &repo),
            clock: Arc::new(shprd_core::log::SystemClock),
        })
    };
    let local = crate::cmd::land::local_landing(&shprd_git::git::Git::new(&repo));
    Ok(Worker {
        repo,
        home: home.to_path_buf(),
        config,
        facts,
        fallback,
        forge,
        local,
        start,
        review,
        close_reviewer,
        actor,
        after_write: None,
        hub,
        news,
    })
}

/// How long a change must hold before the coordinator is told, ms.
///
/// It is the settings model's `tasks.news_settle_secs`, whose built-in value is
/// Shepherdr's five seconds. A settings file that cannot be read is not a reason
/// to lose the news, so the built-in applies then.
fn settle_ms(home: &Path, repo: &Path) -> u64 {
    let seconds = shprd_core::settings::Loaded::read(home, Some(repo))
        .map(|loaded| loaded.resolve(None).tasks().news_settle_secs)
        .unwrap_or(shprd_core::settings::NEWS_SETTLE_SECS);
    seconds.saturating_mul(1_000)
}

/// True when the environment asks for a record-only start, in a debug build.
///
/// The same switch `start_for_tick` reads: a pass that records its starts has no
/// tabs to look for, so it reports nothing live rather than asking a host that
/// the test never made.
fn record_only() -> bool {
    cfg!(debug_assertions)
        && std::env::var_os(crate::cmd::agent::RECORD_ONLY_ENV).is_some_and(|value| value == "1")
}

/// The config in force: the global file, then the repo override.
pub(crate) fn load_config(ctx: &Ctx, home: &Path) -> Result<config::Config, CliError> {
    let global = home.join(".config").join("shprd").join("config.toml");
    let repo = ctx.root.join(".shprd").join("config.toml");
    config::load(Some(&global), Some(&repo))
        .map_err(|err| CliError::other(anyhow!(err).context("loading the shprd config")))
}

/// The forge one pass drives: `gh`, or the script a debug build is pointed at.
///
/// A release binary ignores the variable entirely, so nothing but a real merge
/// can ever be recorded as one.
fn forge(dir: &Path) -> Result<Box<dyn Forge + Send + Sync>, CliError> {
    let env_value =
        std::env::var_os(FAKE_FORGE_ENV).map(|value| value.to_string_lossy().into_owned());
    match env_value {
        Some(path) if cfg!(debug_assertions) && !path.is_empty() => {
            eprintln!("warning: using a fake forge ({FAKE_FORGE_ENV}); nothing reaches GitHub");
            let fake = ScriptedForge::new(Path::new(&path))?;
            Ok(Box::new(fake))
        }
        _ => Ok(Box::new(GhForge::new(dir))),
    }
}

/// A scripted forge, shared across a pass's threads.
///
/// `FakeForge` keeps its answers in a `RefCell`, which is right for one thread
/// and not for a worker thread; this holds one behind a mutex, and covers the
/// same script file and call journal so a test drives it exactly as it drives
/// the `land` verb's.
#[derive(Debug)]
struct ScriptedForge {
    /// Keeps the temp directory of a fresh script alive for the fixture's tests.
    inner: std::sync::Mutex<shprd_git::forge::FakeForge>,
}

impl ScriptedForge {
    /// Reads `path`, failing when the script cannot be read.
    fn new(path: &Path) -> Result<Self, CliError> {
        // The script is re-read for every pass: a test scripts the forge's
        // answer before each pass, exactly as it does before each `land`.
        let fake = shprd_git::forge::FakeForge::from_script(path)
            .map_err(|err| CliError::other(anyhow!(err).context("reading the scripted forge")))?;
        Ok(Self {
            inner: std::sync::Mutex::new(fake),
        })
    }

    /// Runs `f` against the scripted forge.
    fn with<T>(&self, f: impl FnOnce(&shprd_git::forge::FakeForge) -> T) -> T {
        let guard = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        f(&guard)
    }
}

impl Forge for ScriptedForge {
    fn find_open(
        &self,
        head: &str,
    ) -> Result<Option<shprd_git::forge::Pr>, shprd_git::forge::ForgeError> {
        self.with(|forge| forge.find_open(head))
    }

    fn create(
        &self,
        base: &str,
        head: &str,
        title: &str,
        body_file: &Path,
    ) -> Result<shprd_git::forge::Pr, shprd_git::forge::ForgeError> {
        self.with(|forge| forge.create(base, head, title, body_file))
    }

    fn edit_body(&self, number: u64, body_file: &Path) -> Result<(), shprd_git::forge::ForgeError> {
        self.with(|forge| forge.edit_body(number, body_file))
    }

    fn status(
        &self,
        number: u64,
    ) -> Result<shprd_git::forge::PrStatus, shprd_git::forge::ForgeError> {
        self.with(|forge| forge.status(number))
    }

    fn merge(&self, number: u64, head_sha: &str) -> Result<(), shprd_git::forge::ForgeError> {
        self.with(|forge| forge.merge(number, head_sha))
    }
}

/// Prints the pass: the JSON document when asked, else one line per action.
///
/// `args.once` changes nothing here: one invocation is one pass, and the flag
/// exists so a caller can say so out loud, as `shprd watch --once` does.
fn print_report(ctx: &Ctx, args: &TickArgs, report: &TickReport) {
    let _ = args.once;
    if ctx.json {
        match serde_json::to_string_pretty(report) {
            Ok(text) => println!("{text}"),
            Err(err) => eprintln!("error: serializing the tick report: {err}"),
        }
        return;
    }
    if report.steps.is_empty() && report.news.is_empty() {
        println!("ok nothing to advance");
        return;
    }
    for sent in &report.news {
        for line in &sent.lines {
            println!("ok news {}: {line}", sent.space);
        }
    }
    for step in &report.steps {
        let mark = if step.ok { "ok" } else { "failed" };
        match step.seq {
            Some(seq) => println!(
                "{mark} {} {} seq={seq}: {}",
                step.action, step.task, step.detail
            ),
            None => println!("{mark} {} {}: {}", step.action, step.task, step.detail),
        }
    }
}
