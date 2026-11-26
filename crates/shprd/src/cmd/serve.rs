//! `shprd serve`: the loopback API every shprd UI calls, and the tick.
//!
//! The service reads and writes this repository's run log through
//! `shprd-server`, which uses the same fold, rules and append the CLI does. It
//! writes its discovery files under the XDG state directory, binds `127.0.0.1`
//! only, and stops on `SIGINT` or `SIGTERM` — on a clean stop it removes
//! `service.json` so no client is left pointing at a dead port.
//!
//! Beside the API it runs the tick: one advance pass every
//! `[tick] interval_secs` (15 by default), so a run moves on its own rather than
//! waiting for a human to run a verb. A pass is the same one `shprd tick` runs
//! once on demand — a queued task starts, a task the policy allows lands, a
//! merge made outside shprd is recorded — and its writes are published on the
//! service's change channel like any other.
//!
//! A pass that fails a step carries on with the others, so the failure has to be
//! said out loud or the operator never hears it: `serve` installs a `tracing`
//! subscriber at `SHPRD_LOG`'s level (default `warn`, to stderr), which is where
//! every step's `tracing::warn!` goes, and it says in its banner whether the
//! repository has a run at all.
//!
//! It also prepares the repository's own space before the first pass: the label
//! is derived from the folder name, so a space nobody made is a run that cannot
//! start a task until someone reads a refusal (M9 gate bug 6). That space is the
//! coordinator's, and the service starts the coordinator agent in it (or finds
//! it running after a restart): the one space shprd makes. A task gets no space
//! and no tab: its worker and reviewer are headless agents the host lists. The
//! banner says whether it found the space, made it, or could not reach the host
//! at all.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::anyhow;
use serde_json::Value;
use shprd_core::event::Actor;
use shprd_core::state::RunInfo;
use shprd_mux::Host;
use shprd_server::{
    AfterWrite, Config, Engine, NO_RUN_HINT, PassNotice, Service, TickConfig, TickReport, WsLimits,
    shutdown_signal,
};

use shprd_server::ServerError::{AlreadyRunning, ShuttingDown};

use crate::cli::ServeArgs;
use crate::ctx::{
    CliError, Ctx, resolve_agent_data_home, resolve_agent_home, resolve_config_home,
    resolve_data_dir, resolve_home, resolve_state_home,
};

/// The variable that sets how much the service logs.
///
/// It takes the same directives `RUST_LOG` does — `warn` (the default), `info`,
/// `debug`, or a per-target form such as `shprd_server=debug`.
const LOG_ENV: &str = "SHPRD_LOG";

/// Runs the service until the process is asked to stop.
///
/// There is one service per user (SHPR-77): it serves every repository its
/// window registers, each with its own run, event log, tick, news and
/// coordinator. `named` says the caller named a repository (`--repo`,
/// `$SHPRD_REPO`, or a working directory inside one): that repository is
/// registered before the banner, so a CLI start still shows its space and run;
/// a start from the window's own launch names none and waits to be told.
pub fn run(ctx: &Ctx, args: &ServeArgs, home: Option<&Path>, named: bool) -> Result<(), CliError> {
    let home = resolve_home(home)?;
    install_subscriber();
    // The host is resolved once, here, and passed down: every pass, every spawn
    // and every close of this service drives the same adapter, so a run cannot
    // half-happen on one multiplexer and half on another (M10.9).
    let (resolution, host) = crate::host::select();
    let host: Arc<dyn Host> = Arc::from(host);
    // `select` keeps this resolution for the whole process, and a review or a
    // spawn the service starts later (`spawn::real_mux`) goes through the same
    // `select`, so it gets this answer back without probing under load. It is
    // asked afresh only if the host stops opening and comes back.
    // Everything the service reads from the machine is resolved here, once: the
    // discovery files' state directory, the person's home, their prompts and the
    // harness table. The service itself reads no environment variable.
    let state_home = resolve_state_home(&home);
    let state_dir = shprd_server::state_dir(&state_home);
    // The per-repository services an earlier version ran are stopped and their
    // records removed before anything is registered, so nothing doubles up.
    let migrated = crate::cmd::serve_migrate::migrate(&state_dir);
    for note in &migrated {
        eprintln!("   migrated:     {note}");
    }
    install_self_update(&home, &state_dir);
    let data = resolve_data_dir();
    let factory = Arc::new(Factory {
        actor: ctx.actor.clone(),
        json: ctx.json,
        no_dagr: ctx.no_dagr,
        home: home.clone(),
        host: host.clone(),
        data: data.clone(),
        built: Arc::new(Mutex::new(std::collections::HashMap::new())),
    });
    let mut config = Config::service(state_dir, factory.clone());
    config.port = args.port;
    config.actor = ctx.actor.clone();
    let write_ctx = ctx.clone().with_home(Some(home.clone()));
    // Where the graph's program is, found once here: the web app's graph reads
    // it from the service (`GET /api/graph`), and the answer is the same one the
    // projection is checked with. The desktop draws no graph terminal any more.
    config.dagr = Some(crate::cmd::dagr::dagr(&write_ctx));
    config.home = Some(home.clone());
    config.prompts_dir = Some(shprd_core::prompts::prompts_dir(&resolve_config_home(
        &home,
    )));
    config.data = Some(data);
    // A chat reads an agent CLI's own session record, and that file is written
    // by the CLI under the home and data home *it* resolves — the `HOME` it
    // inherits (shprd never rewrites it: it is a protected variable) and the
    // `$XDG_DATA_HOME` this process was started with. The engine's own home is
    // `SHPRD_HOME`, which no CLI writes a session under, so it is not the root a
    // transcript is looked for in (M10 gate bug 2). A process with no `$HOME`
    // leaves this unset rather than guessing, and the chat's route then says it
    // was not told.
    config.transcript_roots = resolve_agent_home().map(|agent_home| {
        shprd_agents::transcript::Roots::new(&agent_home, resolve_agent_data_home().as_deref())
    });
    // The roster's pickers ask a program for the models it runs, so the service
    // is given the `PATH` this process was started with rather than reading one
    // of its own: what a request may start is the binary's decision, resolved
    // once, here.
    config.path_env = Some(std::env::var("PATH").unwrap_or_default());

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|err| CliError::other(anyhow!(err).context("starting the async runtime")))?;

    // Stop handling is installed before the service announces itself: the
    // record `Service::start` writes is what a supervisor waits for, and a
    // signal sent right after it must stop the service quietly.
    let shutdown = {
        let _in_runtime = runtime.enter();
        shutdown_signal()
    };
    // A second service for the same state directory would take the record over
    // and leave the running one undiscoverable, so it is refused with the exit
    // code a refused verb uses rather than an error's.
    let service = runtime
        .block_on(Service::start(config))
        .map_err(|err| match err {
            // A service that is up, or one whose stop is still in flight, is a
            // person's verb that did not happen rather than a fault: the message
            // says which, and the retry fixes the second.
            AlreadyRunning { .. } | ShuttingDown { .. } => CliError::Invalid(anyhow!(err)),
            other => CliError::other(anyhow!(other).context("starting the service")),
        })?;
    // The repository the caller named is registered here, outside the async
    // context and with its space prepared before the banner: the host it asks
    // may own a runtime of its own, which must never be dropped inside this
    // one's.
    let registry = service.registry();
    let mut first = None;
    if named {
        match registry.register_folder_wait(&ctx.root, true, true) {
            Ok(found) => first = Some(found),
            Err(why) => tracing::warn!(%why, "the named repository could not be served"),
        }
    }
    let info = service.info().clone();
    let banner = Banner {
        first: first.map(|found| PathBuf::from(found.root)),
        factory: factory.clone(),
        migrated: migrated.len(),
    };
    announce(ctx, &info, &banner, &resolution);
    runtime.block_on(async move {
        service
            .run(shutdown)
            .await
            .map_err(|err| CliError::other(anyhow!(err).context("serving the run")))
    })
}

/// Lets a landing start the self-update helper, when the person turned it on.
///
/// The binary's own file is captured here, once, before anything renames it: the
/// helper swaps that file and keeps the previous one beside it as `shprd.prev`.
/// No path is built in; a service whose own file cannot be found updates nothing.
fn install_self_update(home: &Path, state_dir: &Path) {
    let Ok(exe) = std::env::current_exe().and_then(|path| path.canonicalize()) else {
        tracing::warn!("the running binary's file could not be found; self-update is off");
        return;
    };
    let (home_dir, state) = (home.to_path_buf(), state_dir.to_path_buf());
    let spawn: shprd_server::self_update::Spawn =
        Arc::new(move |job| crate::cmd::self_update::spawn_for(&exe, &home_dir, &state, job));
    shprd_server::self_update::install(shprd_server::self_update::Trigger::new(home, spawn));
}

/// What the banner says about the service and the repository it was started for.
struct Banner {
    /// The repository registered at start, when the caller named one.
    first: Option<PathBuf>,
    /// Where that repository's setup notices were kept.
    factory: Arc<Factory>,
    /// How many per-repository services were stopped and removed.
    migrated: usize,
}

/// What `build` found out about one repository, for the banner.
#[derive(Clone)]
struct Built {
    /// The tick the banner announces.
    tick: AnnouncedTick,
    /// The run at registration.
    run: Option<RunInfo>,
    /// What preparing the space did, once it has run.
    space: Option<SpaceNotice>,
}

/// What makes one repository's slot: the binary's half of the registry.
struct Factory {
    actor: Actor,
    json: bool,
    no_dagr: bool,
    home: PathBuf,
    host: Arc<dyn Host>,
    data: shprd_agents::data::DataDir,
    /// What each registered repository's build and preparation reported.
    built: Arc<Mutex<std::collections::HashMap<PathBuf, Built>>>,
}

impl std::fmt::Debug for Factory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Factory").finish_non_exhaustive()
    }
}

impl Factory {
    fn ctx_for(&self, root: &Path) -> Ctx {
        Ctx::new(
            root.to_path_buf(),
            self.actor.clone(),
            self.json,
            self.no_dagr,
        )
        .with_home(Some(self.home.clone()))
    }

    fn built_of(&self, root: &Path) -> Option<Built> {
        self.built.lock().ok()?.get(root).cloned()
    }
}

impl shprd_server::RepoFactory for Factory {
    fn root_of(&self, folder: &Path) -> PathBuf {
        crate::ctx::serve_root_of(folder)
    }

    fn build(
        &self,
        root: &Path,
        folder: &Path,
        hub: shprd_server::hub::Hub,
        _dir: &Path,
    ) -> Result<shprd_server::RepoParts, String> {
        let ctx = self.ctx_for(root);
        let host = self.host.clone();
        let home = self.home.clone();
        // The banner says whether there is a run at all, and the passes say so
        // again if one is started after the service was: a service that came up
        // too early must not look like an empty run.
        let run_at_start = ctx.state().map_err(|err| err.to_string())?.run;
        let start_failure: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let live = Arc::new(crate::cmd::live_agents::HostLive::new(
            root.to_path_buf(),
            home.clone(),
            host.clone(),
        ));
        let tick = tick_config(
            &ctx,
            &home,
            hub,
            run_notice(
                self.json,
                run_at_start.as_ref().map(|run| run.run_id.clone()),
            ),
            host.clone(),
        )
        .map_err(|err| err.to_string())?;
        let announced = AnnouncedTick {
            interval_secs: tick.interval.as_secs(),
            max_running: tick.max_running,
        };
        // The person's start, retry and stop actions are the same two closures
        // the tick's steps run: one spawn path, one way of closing a task's
        // space, so a board's Start and the engine's own start cannot drift.
        let engine = engine_of(&tick.worker)
            .with_resend(resender(host.clone(), &home))
            .with_review_again(review_again(&home, tick.worker.actor.clone()));
        if let Ok(mut built) = self.built.lock() {
            built.insert(
                root.to_path_buf(),
                Built {
                    tick: announced,
                    run: run_at_start,
                    space: None,
                },
            );
        }
        // The coordinator's space is part of the host setup a run needs, and its
        // label is derived rather than chosen, so the service makes it rather
        // than leaving a pass to refuse (M9 gate bug 6). It is prepared after the
        // slot answers: it asks the host, which can take seconds, and a window
        // that switches to this repository must not wait for it.
        // Set when the repository's space closes and the service drops it: a
        // preparation that has not started yet and a coordinator retry stop there,
        // so nothing they would start outlives the space.
        let retired = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let retire = {
            let (host, ctx, home) = (host.clone(), ctx.clone(), home.clone());
            let (root, retired) = (root.to_path_buf(), retired.clone());
            Arc::new(move || {
                retired.store(true, std::sync::atomic::Ordering::SeqCst);
                let done = crate::cmd::spawn::stop_repo_agents(host.as_ref(), &ctx, &home, &root);
                tracing::info!(repo = %root.display(), stopped = done.stopped, kept = done.kept.len(), tabs_kept = done.tabs_kept.len(), "the space closed: its agents were stopped");
            }) as shprd_server::Prepare
        };
        let prepare = {
            let retired = retired.clone();
            let (host, ctx, home) = (host.clone(), ctx.clone(), home.clone());
            let (root, folder) = (root.to_path_buf(), folder.to_path_buf());
            let (failure, live, every) = (start_failure.clone(), live.clone(), tick.interval);
            let built = self.built.clone();
            Arc::new(move || {
                if retired.load(std::sync::atomic::Ordering::SeqCst) {
                    return;
                }
                let retired = retired.clone();
                let (host, ctx, home) = (host.clone(), ctx.clone(), home.clone());
                let (root, folder, failure) = (root.clone(), folder.clone(), failure.clone());
                // The guard is asked before anything touches the host (M11 fix
                // Y): a live check run from a temporary checkout inherits the
                // pane's `HERDR_SOCKET_PATH`, and a service that prepared its
                // space would make one in the person's own window.
                let leakage = crate::guard::leakage(&crate::guard::Edge::from_environment(&root));
                let workspace = match &leakage {
                    Some(reason) => SpaceNotice::Withheld(reason.clone()),
                    None => prepare_space(
                        host.as_ref(),
                        &root,
                        Some(&folder),
                        coordinator_start(host.clone(), ctx.clone(), home.clone()),
                    ),
                };
                if let SpaceNotice::Unavailable(reason) | SpaceNotice::Withheld(reason) = &workspace
                {
                    tracing::warn!(%reason, "the repository's space is not ready: a pass cannot start a task until it is");
                }
                live.set_space(matches!(
                    workspace,
                    SpaceNotice::Created(..) | SpaceNotice::Reused(..)
                ));
                let failed = match &workspace {
                    SpaceNotice::Created(_, None, why) | SpaceNotice::Reused(_, None, why) => {
                        why.clone()
                    }
                    SpaceNotice::Unavailable(reason) | SpaceNotice::Withheld(reason) => {
                        Some(format!("the space was not ready: {reason}"))
                    }
                    _ => None,
                };
                let withheld = matches!(workspace, SpaceNotice::Withheld(_));
                if let Ok(mut kept) = failure.lock() {
                    *kept = failed.clone();
                }
                if let Ok(mut built) = built.lock()
                    && let Some(entry) = built.get_mut(&root)
                {
                    entry.space = Some(workspace);
                }
                // A coordinator that could not start (its program missing, or
                // ended as it started) is tried again every pass, so it comes up
                // once the program is back, and the chat's reason follows each try.
                if failed.is_some() && !withheld {
                    std::thread::spawn(move || {
                        retry_coordinator(
                            host,
                            ctx,
                            home,
                            (root, folder),
                            failure,
                            every,
                            &retired,
                        );
                    });
                }
            }) as shprd_server::Prepare
        };
        Ok(shprd_server::RepoParts {
            after_write: Some(after_write(&ctx)),
            engine: Some(engine),
            // The chat's writes go through the host, the same adapter every
            // other verb drives: one code path types into a pane, so a chat line
            // lands where a prompt does and never in a tab shprd did not open.
            chat: shprd_server::ChatHost::with(Arc::new(HostChat {
                repo: root.to_path_buf(),
                home: home.clone(),
                host: host.clone(),
                start_failure,
                notice: Mutex::new(None),
            })),
            terminal: shprd_server::TerminalPort::with(Arc::new(HostTerminal {
                repo: root.to_path_buf(),
                home: home.clone(),
                host: host.clone(),
                live: live.clone(),
            })),
            // The tab bar's "+" menu's *Start an agent* is the same host and the
            // same harness table: a program is a tab with its plain launch line
            // in it, never a task's attempt, so it goes through the service and
            // not through the mod.
            agents: shprd_server::Agents::with(Arc::new(HostAgents {
                repo: root.to_path_buf(),
                data: self.data.clone(),
                host: host.clone(),
                ctx: ctx.clone(),
                home: home.clone(),
            })),
            // The agents the host lists, headless: `live_agents` on the state
            // answer, the output and attach routes, and the terminal socket's
            // `agent:<id>` space all read this one list, so a client can only
            // reach an agent that was listed.
            live: shprd_server::LiveAgents::with(live),
            tick: Some(tick),
            prepare: Some(prepare),
            retire: Some(retire),
            retired: Some(retired),
        })
    }
}

/// The step that starts the coordinator in a prepared space: the same one at
/// start and on every retry, so a retry reads the settings and the installed
/// programs afresh.
fn coordinator_start(
    host: Arc<dyn Host>,
    ctx: Ctx,
    home: PathBuf,
) -> impl FnOnce(
    &crate::cmd::spawn::PreparedSpace,
) -> Result<crate::cmd::spawn::CoordinatorStart, CliError> {
    move |prepared| {
        let data = crate::cmd::roster::resolve_data_dir();
        let env = crate::cmd::spawn::build_repo_env(&ctx, Some(&home), &data)?;
        let delivering = host.clone();
        crate::cmd::spawn::ensure_coordinator(host.as_ref(), &env, prepared, move |pending| {
            // The wait for the program to take a turn runs beside the
            // service: the banner and the API do not wait for a program.
            std::thread::spawn(move || pending.deliver(delivering.as_ref()));
        })
    }
}

/// One try at starting a coordinator that failed to: `true` once it runs (the
/// chat's reason is cleared), `false` with the reason kept up to date.
fn retry_coordinator_once(
    host: &dyn Host,
    repo: &Path,
    folder: &Path,
    failure: &Mutex<Option<String>>,
    start: impl FnOnce(
        &crate::cmd::spawn::PreparedSpace,
    ) -> Result<crate::cmd::spawn::CoordinatorStart, CliError>,
) -> bool {
    let notice = prepare_space(host, repo, Some(folder), start);
    let (started, why) = match notice {
        SpaceNotice::Created(_, pane, why) | SpaceNotice::Reused(_, pane, why) => {
            (pane.is_some(), why)
        }
        SpaceNotice::Unavailable(reason) | SpaceNotice::Withheld(reason) => {
            (false, Some(format!("the space was not ready: {reason}")))
        }
    };
    if let Ok(mut kept) = failure.lock() {
        if started {
            *kept = None;
        } else if *kept != why {
            if let Some(why) = &why {
                eprintln!("   coordinator:  still not started: {why}");
            }
            *kept = why;
        }
    }
    started
}

/// Tries again every `every` until the coordinator runs.
fn retry_coordinator(
    host: Arc<dyn Host>,
    ctx: Ctx,
    home: PathBuf,
    (repo, folder): (PathBuf, PathBuf),
    failure: Arc<Mutex<Option<String>>>,
    every: Duration,
    retired: &std::sync::atomic::AtomicBool,
) {
    loop {
        std::thread::sleep(every);
        if retired.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let start = coordinator_start(host.clone(), ctx.clone(), home.clone());
        if retry_coordinator_once(host.as_ref(), &repo, &folder, &failure, start) {
            println!("   coordinator:  started (it could not be at first)");
            return;
        }
    }
}

/// Installs the subscriber every `tracing` line in the service goes through.
///
/// The binary is the only place that decides where log lines go, so it is the
/// only place that installs one: the level is `SHPRD_LOG`'s (default `warn`),
/// and the lines go to stderr so `serve`'s banner on stdout stays the one thing
/// a person reads on purpose. A subscriber that is already installed — a test
/// that captured one, a process that set one up — is left alone.
fn install_subscriber() {
    use std::io::IsTerminal;
    use tracing_subscriber::filter::LevelFilter;
    let filter = tracing_subscriber::EnvFilter::try_from_env(LOG_ENV)
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(LevelFilter::WARN.to_string()));
    // Colour only for a person watching: a log a supervisor redirects to a file
    // is read by something that would keep the escape codes.
    let ansi = std::io::stderr().is_terminal();
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(ansi)
        .with_target(false)
        .try_init();
}

/// The host as the chat uses it: one turn of the person's input, into a pane.
///
/// It holds nothing but the repository, because the host is made per call: a
/// `shprd serve` that runs for days must pick up a herdr that was restarted
/// under it, and the adapter rediscovers its socket every time it is built. The
/// pane a space's agent runs in is resolved by the same path `shprd agent
/// prompt` uses ([`crate::cmd::spawn::prompt_in`]), so a task's chat and a
/// prompt can never disagree about which tab is the task's.
struct HostChat {
    /// The repository whose run log names each space's pane.
    repo: std::path::PathBuf,
    /// The home directory the pane record and the tab records live under,
    /// resolved once by the binary rather than read from the environment on
    /// every request.
    home: std::path::PathBuf,
    /// The host this service drives — the same adapter the tick spawns and
    /// closes with, so a chat line and a prompt can never go to two hosts.
    host: Arc<dyn Host>,
    /// Why the coordinator could not be started when the service started, so a
    /// chat that finds none says why and not only that there is none.
    start_failure: Arc<Mutex<Option<String>>>,
    /// What the last send did first (made the folder a repository), until the
    /// reply carries it out.
    notice: Mutex<Option<String>>,
}

impl shprd_server::ChatWriter for HostChat {
    fn take_notice(&self) -> Option<String> {
        self.notice.lock().ok().and_then(|mut notice| notice.take())
    }

    fn coordinator_running(&self) -> Option<bool> {
        // A host that cannot say counts as "none running": the chat then shows
        // nothing rather than risk another product's conversation in the folder.
        let Ok(name) = crate::cmd::spawn::repo_name(&self.repo)
            .map(|name| crate::cmd::spawn::repo_space_name(&name))
        else {
            return Some(false);
        };
        let space = match shprd_mux::find_marked_space(self.host.as_ref(), &name) {
            Ok(Some(space)) => space,
            Ok(None) | Err(_) => return Some(false),
        };
        Some(
            crate::cmd::spawn::find_coordinator(&self.home, &self.repo, self.host.as_ref(), &space)
                .is_some(),
        )
    }

    fn started_ms(&self, space: Option<&str>) -> Option<u64> {
        // Only the coordinator is pinned: a task's conversation is in its own
        // worktree, where nobody else's lives.
        if space.is_some() {
            return None;
        }
        let name = crate::cmd::spawn::repo_name(&self.repo)
            .map(|name| crate::cmd::spawn::repo_space_name(&name))
            .ok()?;
        let space = shprd_mux::find_marked_space(self.host.as_ref(), &name)
            .ok()
            .flatten()?;
        crate::cmd::spawn::find_coordinator(&self.home, &self.repo, self.host.as_ref(), &space)?
            .started_ms
    }

    fn interrupt(&self, space: Option<&str>) -> Result<(), String> {
        let ctx = Ctx::new(self.repo.clone(), Actor::Human, false, true);
        let pane = match space {
            None => crate::cmd::spawn::coordinator_pane(self.host.as_ref(), &self.home, &ctx),
            Some(task) => crate::cmd::spawn::task_pane(self.host.as_ref(), &self.home, &ctx, task),
        }
        .map_err(|err| err.to_string())?;
        self.host
            .keys(&pane, &[shprd_mux::Key::Esc])
            .map_err(|err| err.to_string())
    }

    fn send(&self, space: Option<&str>, text: &str) -> Result<(), String> {
        // A folder that is not a repository yet becomes one now, at the person's
        // first message and not when the window merely showed it: the work
        // needs a repository, and nobody types a command to get one.
        if space.is_none() {
            match crate::cmd::adopt::ensure_repo(&self.repo) {
                Ok(Some(made)) => {
                    if let Ok(mut notice) = self.notice.lock() {
                        *notice = Some(made.note);
                    }
                }
                Ok(None) => {}
                Err(why) => return Err(why.trim_end_matches('.').to_string()),
            }
        }
        // Reading the log needs the root; the actor is the person, because the
        // line is theirs — the API never writes to the ledger for a chat turn,
        // so nothing here can be mistaken for an engine decision.
        let ctx = Ctx::new(self.repo.clone(), Actor::Human, false, true);
        match space {
            None => {
                crate::cmd::spawn::prompt_coordinator(self.host.as_ref(), &self.home, &ctx, text)
            }
            Some(task) => {
                crate::cmd::spawn::prompt_in(self.host.as_ref(), &self.home, &ctx, task, text)
            }
        }
        .map_err(|err| {
            let why = self.start_failure.lock().ok().and_then(|why| why.clone());
            match (why, space) {
                (Some(why), None) if matches!(err, CliError::Invalid(_)) => {
                    format!("{err} (it could not be started: {why}; the service tries again)")
                }
                _ => err.to_string(),
            }
        })
    }
}

/// The host as the terminal socket uses it: watch a pane, type into it.
///
/// The pane is resolved by the chat's own path ([`crate::cmd::spawn::task_pane`]
/// and [`crate::cmd::spawn::coordinator_pane`]), so a terminal can only ever be
/// attached to a pane a chat line could reach, and a `pane` the client names is
/// checked against it rather than used.
struct HostTerminal {
    /// The repository whose run log names each space's pane.
    repo: std::path::PathBuf,
    /// The home directory the pane records live under.
    home: std::path::PathBuf,
    /// The host this service drives.
    host: Arc<dyn Host>,
    /// The agents the host lists, for the `agent:<id>` space.
    live: Arc<crate::cmd::live_agents::HostLive>,
}

impl shprd_server::TerminalHost for HostTerminal {
    fn resolve(&self, space: &str, pane: Option<&str>) -> Result<String, String> {
        // `agent:<id>` is a headless agent the service listed: its terminal and
        // no other, never an address the client chose.
        if let Some(id) = space.strip_prefix("agent:") {
            return self
                .live
                .terminal_of(id)
                .filter(|terminal| pane.is_none_or(|wanted| wanted == terminal))
                .ok_or_else(|| format!("no agent {id} is listed"));
        }
        let ctx = Ctx::new(self.repo.clone(), Actor::Human, false, true);
        let found = match shprd_server::ChatSpace::parse(Some(space)) {
            shprd_server::ChatSpace::Coordinator => {
                crate::cmd::spawn::coordinator_pane(self.host.as_ref(), &self.home, &ctx)
            }
            shprd_server::ChatSpace::Task(task) => {
                crate::cmd::spawn::task_pane(self.host.as_ref(), &self.home, &ctx, &task)
            }
        }
        .map_err(|err| err.to_string())?;
        match pane {
            Some(wanted) if wanted != found.as_str() => Err(format!(
                "{wanted:?} is not the pane {space} runs in: attach by space and let the service choose"
            )),
            _ => Ok(found.as_str().to_owned()),
        }
    }

    fn read(&self, pane: &str, lines: usize) -> Result<String, String> {
        self.host
            .read(&shprd_mux::PaneId::new(pane.to_owned()), lines)
            .map_err(|err| err.to_string())
    }

    fn write(&self, pane: &str, text: &str) -> Result<(), String> {
        self.host
            .write(&shprd_mux::PaneId::new(pane.to_owned()), text, false)
            .map_err(|err| err.to_string())
    }

    fn keys(&self, pane: &str, keys: &[shprd_server::TermKey]) -> Result<(), String> {
        use shprd_mux::Key;
        use shprd_server::TermKey as K;
        let keys: Vec<Key> = keys
            .iter()
            .map(|key| match key {
                K::Enter => Key::Enter,
                K::Esc => Key::Esc,
                K::Tab => Key::Tab,
                K::Up => Key::Up,
                K::Down => Key::Down,
                K::Left => Key::Left,
                K::Right => Key::Right,
                K::PageUp => Key::PageUp,
                K::PageDown => Key::PageDown,
                K::Backspace => Key::Backspace,
                K::Delete => Key::Delete,
                K::Home => Key::Home,
                K::End => Key::End,
                K::CtrlC => Key::CtrlC,
            })
            .collect();
        self.host
            .keys(&shprd_mux::PaneId::new(pane.to_owned()), &keys)
            .map_err(|err| err.to_string())
    }
}

/// The host as the "+" menu's *Start an agent* uses it: one program, one tab.
///
/// It holds the repository, the shipped harness table and the same adapter every
/// other verb drives. Opening is [`crate::cmd::spawn::open_program`], so the
/// tab's argv, its name and the space it opens in are the spawn path's decisions
/// and not the route's.
struct HostAgents {
    /// The repository whose space the tab opens in.
    repo: std::path::PathBuf,
    /// The harness table a program id is looked up in.
    data: shprd_agents::data::DataDir,
    /// The host this service drives.
    host: Arc<dyn Host>,
    /// The context a coordinator is started with.
    ctx: Ctx,
    /// The person's home, where the settings the coordinator starts from live.
    home: std::path::PathBuf,
}

impl shprd_server::AgentOpener for HostAgents {
    fn name_coordinator(&self, title: &str) -> Result<(), String> {
        crate::cmd::spawn::rename_coordinator_tab(self.host.as_ref(), &self.repo, title)
            .map(|_| ())
            .map_err(|err| err.to_string())
    }

    fn restart_coordinator(&self) -> Result<(), String> {
        let delivering = self.host.clone();
        crate::cmd::spawn::restart_coordinator(
            self.host.as_ref(),
            &self.ctx,
            &self.home,
            move |pending| {
                std::thread::spawn(move || pending.deliver(delivering.as_ref()));
            },
        )
        .map_err(|err| err.to_string())
    }

    fn open(&self, program: &str, space: Option<&str>) -> Result<shprd_server::OpenedTab, String> {
        let opened = crate::cmd::spawn::open_program(
            self.host.as_ref(),
            &self.data,
            &self.repo,
            program,
            space,
        )
        .map_err(|err| err.to_string())?;
        Ok(shprd_server::OpenedTab {
            program: opened.program,
            name: opened.name,
            tab: opened.tab,
        })
    }
}

/// What the service did about the repository's own space when it started.
///
/// The label is derived ([`crate::cmd::spawn::repo_space_name`]) and a host
/// refuses a duplicate, so nothing here renames, adopts or removes a space: the
/// one shprd uses is the one whose label is exactly right, and everything else
/// on the host is left alone. Task spaces are not the service's business at
/// startup: they come and go with their tasks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SpaceNotice {
    /// The space was already there and is reused, with the pane recorded for the
    /// coordinator's chat when the space has a tab, and the reason the
    /// coordinator could not be started when it could not.
    Reused(String, Option<String>, Option<String>),
    /// The space was missing and is created at the repository root, with that
    /// pane.
    Created(String, Option<String>, Option<String>),
    /// It could not be found or made; the reason names the label it wanted.
    Unavailable(String),
    /// The host was not touched at all: the checkout and the socket name two
    /// different machines ([`crate::guard`]). The reason says which two.
    Withheld(String),
}

/// Finds the repository's own space, or creates it at the repository root.
///
/// This is the step that used to be left to a person (`docs/multiplexers.md`
/// states the convention, but the folder name is load-bearing and a label that
/// was almost right made every pass refuse). A host that cannot be asked at all
/// is reported rather than fatal, because the API is useful without one.
fn prepare_space(
    mux: &dyn Host,
    repo: &Path,
    folder: Option<&Path>,
    start: impl FnOnce(
        &crate::cmd::spawn::PreparedSpace,
    ) -> Result<crate::cmd::spawn::CoordinatorStart, CliError>,
) -> SpaceNotice {
    // The folder the person added may be inside the repository: its plain space is
    // adopted rather than a second one made at the root.
    let also: Vec<&Path> = folder.into_iter().collect();
    let prepared = match crate::cmd::spawn::ensure_repo_space_from(mux, repo, &also) {
        Ok(prepared) => prepared,
        Err(err) => return SpaceNotice::Unavailable(err.to_string()),
    };
    // The space holds the coordinator, which is an agent shprd starts: a chat
    // line or the news is typed into it and nothing else, so it is started here
    // (or found running again after a restart) and its pane recorded. A
    // coordinator that cannot start (no CLI installed, a prompt that cannot be
    // read) is a warning and not a refusal to serve: the board is still what a
    // person reads, and the chat then says why it has nowhere to type.
    let (pane, why) = match start(&prepared) {
        Ok(start) => (Some(start.pane().as_str().to_string()), None),
        Err(err) => {
            tracing::warn!(%err, "the coordinator could not be started");
            (None, Some(err.to_string()))
        }
    };
    match prepared.created {
        true => SpaceNotice::Created(prepared.space.name, pane, why),
        false => SpaceNotice::Reused(prepared.space.name, pane, why),
    }
}

/// What the service says when a pass first sees a run the banner did not name.
///
/// The banner is printed once, before the first pass, so a service started in a
/// repository whose run does not exist yet would otherwise look like an empty
/// run for ever. The notice fires once per run: the passes after the first have
/// nothing new to say.
///
/// With `--json` the notice goes to stderr: the flag promises one document on
/// stdout, and a line appended to it would make the banner unparseable. The
/// board picks the run up by itself either way; this line is news for a person.
fn run_notice(json: bool, announced: Option<String>) -> PassNotice {
    let seen = Arc::new(Mutex::new(announced));
    Arc::new(move |report: &TickReport| {
        let Some(run) = report.run.as_deref() else {
            return;
        };
        let Ok(mut seen) = seen.lock() else {
            return;
        };
        if seen.as_deref() == Some(run) {
            return;
        }
        *seen = Some(run.to_string());
        let line = format!("   run:          {run} (started after serve: the passes advance it)");
        if json {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    })
}

/// The tick the service runs beside its API.
///
/// It is built here, where the binary already knows the home directory, the
/// config and the spawn path, so the service itself stays free of the
/// environment: it runs the worker it is handed on the interval it is handed.
fn tick_config(
    ctx: &Ctx,
    home: &Path,
    hub: shprd_server::hub::Hub,
    notice: PassNotice,
    host: Arc<dyn Host>,
) -> Result<TickConfig, CliError> {
    let config = crate::cmd::tick::load_config(ctx, home)?;
    // The interval the banner prints is the one the first pass will act on, not
    // only the one the config file named: `tasks.tick_secs` is read per pass, and
    // a banner that disagreed with the first pass would be the very confusion
    // M10 gate bug 4 was.
    let (config, settings_interval) = shprd_server::tick::pass_config(home, &ctx.root, &config);
    let max_running = config.tick.max_running;
    let interval = Duration::from_secs(settings_interval.unwrap_or(config.tick.interval_secs));
    let worker = crate::cmd::tick::worker_with_hub(ctx, home, config, hub, host)?;
    Ok(TickConfig {
        worker: std::sync::Arc::new(worker),
        interval,
        max_running,
        notice: Some(notice),
    })
}

/// The binary's spawn and close, as the API's actions call them.
///
/// Both are the tick worker's own: it was built with the same closures a pass
/// uses, so a person's start leaves exactly what the engine's start leaves. A
/// worker built without a host's close (a record-only pass) gets an engine that
/// says so when a stop is asked for.
fn engine_of(worker: &std::sync::Arc<shprd_server::Worker>) -> Engine {
    let engine = Engine::new(worker.start.clone());
    match worker.close_worker.clone() {
        Some(close) => engine.with_stop(close),
        None => engine,
    }
}

/// The board's Resend: types a task's latest brief into its agent again.
fn resender(host: Arc<dyn Host>, home: &Path) -> shprd_server::Resender {
    let home = home.to_path_buf();
    Arc::new(move |repo: &Path, task: &str| {
        let ctx = crate::ctx::Ctx::new(repo.to_path_buf(), Actor::Human, false, true);
        crate::cmd::spawn::resend_brief(&*host, &home, &ctx, task).map_err(|err| err.to_string())
    })
}

/// The board's `review again` on a stuck review: the verb's own `--again`.
fn review_again(home: &Path, actor: Actor) -> shprd_server::Resender {
    let home = home.to_path_buf();
    Arc::new(move |repo: &Path, task: &str| {
        let ctx = crate::ctx::Ctx::new(repo.to_path_buf(), actor.clone(), false, true);
        crate::cmd::review::review_again_for_tick(&ctx, &home, task)
    })
}

/// What the startup line says about the tick.
#[derive(Clone)]
struct AnnouncedTick {
    /// Seconds between two passes.
    interval_secs: u64,
    /// How many tasks of this repository may run at once.
    max_running: u32,
}

/// The CLI's own post-write step, for the service's writes.
///
/// It is `shprd`'s dagr projection, not a second copy of it: a task added or a
/// progress line recorded through the API leaves `<root>/.dagr/run.json` fresh,
/// exactly as the same write through the CLI does.
fn after_write(ctx: &Ctx) -> AfterWrite {
    let actor = ctx.actor.clone();
    let no_dagr = ctx.no_dagr;
    let home = ctx.home.clone();
    AfterWrite::new(move |repo| {
        if no_dagr {
            return;
        }
        let write_ctx =
            Ctx::new(repo.to_path_buf(), actor.clone(), false, false).with_home(home.clone());
        crate::cmd::agent::sync_dagr(&write_ctx);
    })
}

/// Says where the service is and how to reach it.
///
/// The token itself is never printed — only the file it is in, which is `0600`
/// in the state directory. When the caller named a repository, the run, the
/// space and the tick are its own; the service itself serves every repository
/// its window registers, so a start from the window names none.
fn announce(
    ctx: &Ctx,
    info: &shprd_server::ServiceInfo,
    banner: &Banner,
    host: &crate::host::Resolution,
) {
    let limits = WsLimits::default();
    let first = banner
        .first
        .as_deref()
        .and_then(|root| banner.factory.built_of(root).map(|built| (root, built)));
    let run = first.as_ref().and_then(|(_, built)| built.run.as_ref());
    let space = first.as_ref().and_then(|(_, built)| built.space.as_ref());
    let tick = first.as_ref().map(|(_, built)| &built.tick);
    let hint = (first.is_some() && run.is_none()).then_some(NO_RUN_HINT);
    if ctx.json {
        let value = serde_json::json!({
            "serving": first.as_ref().map(|(root, _)| root.display().to_string()),
            "port": info.port,
            "url": format!("http://127.0.0.1:{}", info.port),
            "id": info.id,
            "version": info.version,
            "pid": info.pid,
            "token": info.token_path.display().to_string(),
            "service_json": info.service_json.display().to_string(),
            "migrated": banner.migrated,
            "run": run.map(|run| serde_json::json!({
                "id": run.run_id,
                "base": run.base_branch,
                "integration": run.integration_branch,
            })),
            "space": space.map(space_json),
            "host": {
                "kind": host.kind.name(),
                "binary": host.kind.binary(),
                "setting": host.setting.name(),
                "why": host.why,
                "fell_back": host.fell_back,
                "herdr_core": host.probe.core,
                "window": host.probe.window,
            },
            "hint": hint,
            "ws": {
                "ping_secs": limits.ping.as_secs(),
                "idle_secs": limits.idle.as_secs(),
                "session_secs": limits.session.as_secs(),
            },
            "tick": tick.map(|tick| serde_json::json!({
                "interval_secs": tick.interval_secs,
                "max_running": tick.max_running,
            })),
        });
        println!("{value}");
        return;
    }
    match &first {
        Some((root, _)) => println!(
            "ok serving {} (and every repository the window registers) on http://127.0.0.1:{} (id {}, version {})",
            root.display(),
            info.port,
            info.id,
            info.version
        ),
        None => println!(
            "ok serving every repository the window registers on http://127.0.0.1:{} (id {}, version {})",
            info.port, info.id, info.version
        ),
    }
    if let Some((root, _)) = &first {
        match run {
            Some(run) => println!(
                "   run:          {} · {} → {}",
                run.run_id, run.base_branch, run.integration_branch
            ),
            None => println!("   run:          {NO_RUN_HINT}"),
        }
        if let Some(space) = space {
            println!("   space:        {}", space_line(space, root));
        }
    }
    println!("   host:         {}", host.line());
    println!("   token:        {}", info.token_path.display());
    println!("   service.json: {}", info.service_json.display());
    println!(
        "   sockets:      ping every {}s, closed after {}s of silence, {}m at most",
        limits.ping.as_secs(),
        limits.idle.as_secs(),
        limits.session.as_secs() / 60
    );
    if let Some(tick) = tick {
        println!(
            "   tick:         every {}s, at most {} task(s) running",
            tick.interval_secs, tick.max_running
        );
    }
    println!("   stop with Ctrl-C or SIGTERM");
}

/// The banner's space line: what the service found, made, or could not make.
///
/// It is one line either way, so a service whose host is missing still prints a
/// banner a person can read, and the failure to prepare the space is in the
/// place the rest of the setup is.
fn space_line(notice: &SpaceNotice, root: &Path) -> String {
    let coordinator = |pane: &Option<String>, why: &Option<String>| match (pane, why) {
        (Some(pane), _) => format!(", the coordinator's pane {pane}"),
        (None, Some(why)) => format!(", the coordinator could not be started: {why}"),
        (None, None) => String::from(", no tab for the coordinator yet"),
    };
    match notice {
        SpaceNotice::Created(label, pane, why) => format!(
            "created {label:?} at the repository root {}{}",
            root.display(),
            coordinator(pane, why)
        ),
        SpaceNotice::Reused(label, pane, why) => {
            format!(
                "{label:?} (already there, reused){}",
                coordinator(pane, why)
            )
        }
        SpaceNotice::Unavailable(reason) => format!("not ready: {reason}"),
        // A withheld space has no label to report: nothing was looked up, and
        // saying which label it would have used would suggest it had one.
        SpaceNotice::Withheld(reason) => format!("not attempted: {reason}"),
    }
}

/// The banner's space line, as the one document's field.
///
/// A space that could not be prepared has no label to report, so it says so
/// and carries the reason: the label is what a person fixes, and the reason is
/// the only thing that says which label was wanted.
fn space_json(notice: &SpaceNotice) -> Value {
    match notice {
        SpaceNotice::Created(label, pane, why) => {
            serde_json::json!({ "label": label, "state": "created", "pane": pane, "coordinator_error": why })
        }
        SpaceNotice::Reused(label, pane, why) => {
            serde_json::json!({ "label": label, "state": "reused", "pane": pane, "coordinator_error": why })
        }
        SpaceNotice::Unavailable(reason) => {
            serde_json::json!({ "label": Value::Null, "state": "unavailable", "reason": reason })
        }
        SpaceNotice::Withheld(reason) => {
            serde_json::json!({ "label": Value::Null, "state": "withheld", "reason": reason })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shprd_mux::fake::{Call, FakeMux};
    use tempfile::TempDir;

    /// A coordinator start that spawns a program tab and records its pane, as
    /// [`crate::cmd::spawn::ensure_coordinator`] does, without a harness table.
    fn start_in<'a>(
        mux: &'a FakeMux,
        home: &'a Path,
        repo: &'a Path,
    ) -> impl FnOnce(
        &crate::cmd::spawn::PreparedSpace,
    ) -> Result<crate::cmd::spawn::CoordinatorStart, CliError>
    + 'a {
        move |prepared| {
            let tab = mux
                .spawn_agent(&shprd_mux::AgentLaunch {
                    space: &prepared.space.id,
                    title: "coordinator",
                    argv: &["sh".to_string()],
                    env: &[],
                    cwd: repo,
                    role: shprd_mux::AgentRole::Coordinator,
                    task: None,
                    program: "sh",
                    parent: None,
                })
                .map_err(|err| CliError::other(anyhow!(err)))?;
            crate::cmd::spawn::record_coordinator_pane(
                home,
                repo,
                &prepared.space.id,
                &tab.root_pane,
                "sh",
            )?;
            Ok(crate::cmd::spawn::CoordinatorStart::Started(tab.root_pane))
        }
    }

    /// The label a repository's own space carries, as the banner reports it.
    fn label_of(repo: &Path) -> String {
        match crate::cmd::spawn::repo_name(repo) {
            Ok(name) => crate::cmd::spawn::repo_space_name(&name),
            Err(err) => panic!("{err}"),
        }
    }

    #[test]
    fn a_service_makes_the_missing_space_at_the_repository_root() {
        // Bug 6 of the M9 gate: the label is derived from the folder name, and
        // the space was left to a person, so a run refused every pass. The
        // service creates it, with exactly that label, where the repository is.
        let repo = TempDir::new().unwrap_or_else(|err| panic!("{err}"));
        let mux = FakeMux::default().with_starter_tab();
        let label = label_of(repo.path());

        let home = TempDir::new().unwrap_or_else(|err| panic!("{err}"));
        let notice = prepare_space(
            &mux,
            repo.path(),
            None,
            start_in(&mux, home.path(), repo.path()),
        );

        let SpaceNotice::Created(reported, pane, _) = &notice else {
            panic!("the space is created, not {notice:?}");
        };
        assert_eq!(reported, &label);
        // The space's own first tab is the pane the chat may reach, and it is
        // recorded once, here.
        let space = mux
            .find_space(&label)
            .ok()
            .flatten()
            .unwrap_or_else(|| panic!("the space exists"));
        let recorded = std::fs::read_to_string(crate::cmd::spawn::coordinator_pane_path(
            home.path(),
            repo.path(),
            &space.id,
        ))
        .unwrap_or_else(|err| panic!("the pane is recorded: {err}"));
        assert_eq!(
            recorded.lines().next().unwrap_or_default(),
            pane.clone().unwrap_or_default()
        );
        let calls = mux.calls();
        assert_eq!(calls[0], Call::FindSpace(label.clone()));
        assert_eq!(
            calls[1],
            Call::CreateSpace(label.clone(), repo.path().to_path_buf())
        );
        assert!(
            calls.iter().any(|call| matches!(
                call,
                Call::SpawnAgent(shprd_mux::AgentRole::Coordinator, None, None)
            )),
            "the coordinator agent is started in it: {calls:?}"
        );
        let line = space_line(&notice, repo.path());
        assert!(line.contains("created"), "{line}");
        assert!(line.contains(&label), "the label is named: {line}");
    }

    #[test]
    fn agents_of_a_space_the_host_still_has_open_are_never_stopped() {
        // SHPR-86: a folder added again before the retire ran has a new space and
        // a new coordinator; the sweep must not take it, nor a transient empty
        // list from the window take every repository's agents.
        let repo = TempDir::new().unwrap_or_else(|err| panic!("{err}"));
        let home = TempDir::new().unwrap_or_else(|err| panic!("{err}"));
        let mux = FakeMux::default().with_starter_tab();
        prepare_space(
            &mux,
            repo.path(),
            None,
            start_in(&mux, home.path(), repo.path()),
        );
        let label = label_of(repo.path());
        let space = mux
            .find_space(&label)
            .ok()
            .flatten()
            .unwrap_or_else(|| panic!("the space exists"));
        let ctx = crate::ctx::Ctx::new(repo.path().to_path_buf(), Actor::Human, false, true);
        let record = crate::cmd::spawn::coordinator_pane_path(home.path(), repo.path(), &space.id);
        assert!(record.exists(), "the coordinator is recorded");

        let done = crate::cmd::spawn::stop_repo_agents(&mux, &ctx, home.path(), repo.path());

        assert_eq!(done, crate::cmd::spawn::Retired::default());
        assert!(
            mux.list_coordinators()
                .unwrap_or_default()
                .iter()
                .all(|agent| agent.live),
            "the coordinator of the open space runs on"
        );
        assert!(record.exists(), "its record stays");
    }

    #[test]
    fn a_closed_spaces_surviving_agent_is_stopped_and_its_record_dropped() {
        // SHPR-86: a headless worker has no tab, so closing its space leaves it
        // running; the sweep stops it and drops the closed space's record.
        let repo = TempDir::new().unwrap_or_else(|err| panic!("{err}"));
        let home = TempDir::new().unwrap_or_else(|err| panic!("{err}"));
        let mut caps = FakeMux::default().caps();
        caps.headless_agents = true;
        let mux = FakeMux::new(caps).with_starter_tab();
        prepare_space(
            &mux,
            repo.path(),
            None,
            start_in(&mux, home.path(), repo.path()),
        );
        let space = mux
            .find_space(&label_of(repo.path()))
            .ok()
            .flatten()
            .unwrap_or_else(|| panic!("the space exists"));
        let worker = mux
            .spawn_agent(&shprd_mux::AgentLaunch {
                space: &space.id,
                title: "worker",
                argv: &["sh".to_string()],
                env: &[],
                cwd: repo.path(),
                role: shprd_mux::AgentRole::Worker,
                task: Some("T1"),
                program: "sh",
                parent: None,
            })
            .unwrap_or_else(|err| panic!("{err}"));
        let record = crate::cmd::spawn::coordinator_pane_path(home.path(), repo.path(), &space.id);
        assert!(record.exists());
        mux.close_space(&space.id)
            .unwrap_or_else(|err| panic!("{err}"));
        let ctx = crate::ctx::Ctx::new(repo.path().to_path_buf(), Actor::Human, false, true);

        let done = crate::cmd::spawn::stop_repo_agents(&mux, &ctx, home.path(), repo.path());

        assert_eq!(done.stopped, 1, "the worker the window left: {done:?}");
        assert!(
            mux.list_agents()
                .unwrap_or_default()
                .iter()
                .any(|agent| agent.tab.id == worker.id && !agent.live),
            "the worker is no longer running"
        );
        assert!(!record.exists(), "the closed space's record is dropped");
    }

    #[test]
    fn a_space_an_earlier_run_left_for_the_folder_is_reused_and_given_the_label() {
        // SHPR-65 bug 1: the person's repo already had a space (made before the
        // mark existed, named after its folder). The banner said "created" and the
        // sidebar showed a second, raw-id entry beside it.
        let repo = TempDir::new().unwrap_or_else(|err| panic!("{err}"));
        let mux = FakeMux::default().with_starter_tab();
        let label = label_of(repo.path());
        let folder = repo
            .path()
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        let old = mux.add_space(&folder, repo.path());

        let home = TempDir::new().unwrap_or_else(|err| panic!("{err}"));
        let notice = prepare_space(
            &mux,
            repo.path(),
            None,
            start_in(&mux, home.path(), repo.path()),
        );

        let SpaceNotice::Reused(reported, _, _) = &notice else {
            panic!("the older space is reused, not {notice:?}");
        };
        assert_eq!(reported, &label);
        assert!(space_line(&notice, repo.path()).contains("reused"));
        let spaces = mux.list_spaces().unwrap_or_default();
        assert_eq!(spaces.len(), 1, "one repository, one space: {spaces:?}");
        assert_eq!(spaces[0].id, old);
        assert_eq!(spaces[0].name, label, "labelled, not the raw id");
    }

    #[test]
    fn a_coordinator_that_cannot_start_says_why_in_the_banner() {
        let repo = TempDir::new().unwrap_or_else(|err| panic!("{err}"));
        let mux = FakeMux::default();
        let notice = prepare_space(&mux, repo.path(), None, |_| {
            Err(CliError::Invalid(anyhow!("no installed worker CLI")))
        });
        let line = space_line(&notice, repo.path());
        assert!(line.contains("could not be started"), "{line}");
        assert!(line.contains("no installed worker CLI"), "{line}");
        assert_eq!(
            space_json(&notice)["coordinator_error"],
            "invalid: no installed worker CLI"
        );
    }

    /// SHPR-69: a coordinator that could not start is tried again, the chat's
    /// reason follows each try, and it is cleared once the coordinator runs.
    #[test]
    fn a_failed_coordinator_start_is_retried_until_it_runs() {
        let repo = TempDir::new().unwrap_or_else(|err| panic!("{err}"));
        let home = TempDir::new().unwrap_or_else(|err| panic!("{err}"));
        let mux = FakeMux::default();
        let failure = Mutex::new(Some("claude is not installed".to_owned()));

        let still = retry_coordinator_once(&mux, repo.path(), repo.path(), &failure, |_| {
            Err(CliError::other(anyhow!("claude exited as it started")))
        });
        assert!(!still, "the program is still missing");
        let why = failure.lock().map(|why| why.clone()).unwrap_or_default();
        assert_eq!(why.as_deref(), Some("claude exited as it started"));

        let up = retry_coordinator_once(
            &mux,
            repo.path(),
            repo.path(),
            &failure,
            start_in(&mux, home.path(), repo.path()),
        );
        assert!(up, "the program is back, so the coordinator starts");
        assert!(failure.lock().is_ok_and(|why| why.is_none()));
    }

    #[test]
    fn a_service_reuses_the_space_that_is_already_there() {
        let repo = TempDir::new().unwrap_or_else(|err| panic!("{err}"));
        let mux = FakeMux::default().with_starter_tab();
        let label = label_of(repo.path());
        let made = mux
            .create_space(&label, repo.path())
            .unwrap_or_else(|err| panic!("{err}"));
        let _ = mux.take_calls();

        let home = TempDir::new().unwrap_or_else(|err| panic!("{err}"));
        let notice = prepare_space(
            &mux,
            repo.path(),
            None,
            start_in(&mux, home.path(), repo.path()),
        );

        let SpaceNotice::Reused(reported, pane, _) = &notice else {
            panic!("the space is reused, not {notice:?}");
        };
        assert_eq!(reported, &label);
        assert!(
            pane.is_some(),
            "the pane the chat may reach is recorded on a reused space too"
        );
        let calls = mux.calls();
        assert_eq!(calls[0], Call::FindSpace(label.clone()));
        assert!(
            !calls
                .iter()
                .any(|call| matches!(call, Call::CreateSpace(..) | Call::CloseSpace(_))),
            "nothing created or closed: {calls:?}"
        );
        let found = mux
            .find_space(&made.name)
            .unwrap_or_else(|err| panic!("{err}"))
            .unwrap_or_else(|| panic!("the space is gone"));
        assert_eq!(found.id, made.id);
    }

    #[test]
    fn a_space_that_could_not_be_made_is_reported_not_hidden() {
        // A host that is not there must not stop the service: the API is still
        // the board a person reads. The banner says the one thing a pass will
        // fail on, so it is not silent.
        let repo = TempDir::new().unwrap_or_else(|err| panic!("{err}"));
        let message = crate::cmd::spawn::missing_space_message(
            &label_of(repo.path()),
            &[String::from("shprd · gate repo")],
        );
        let notice = SpaceNotice::Unavailable(message);

        let line = space_line(&notice, repo.path());
        assert!(line.starts_with("not ready:"), "{line}");
        assert!(
            line.contains(&label_of(repo.path())),
            "the label the host wanted is named: {line}"
        );
        assert!(
            line.contains("shprd · gate repo"),
            "and the one it has: {line}"
        );
        assert_eq!(space_json(&notice)["state"], "unavailable");
        assert_eq!(space_json(&notice)["label"], Value::Null);
    }

    #[test]
    fn a_withheld_space_says_so_and_names_no_label() {
        // M11 fix Y: the guard refused the host, so nothing was looked up. The
        // banner must not name a label it never wanted, and the reason has to
        // carry the two paths a person needs to see the disagreement.
        let repo = TempDir::new().unwrap_or_else(|err| panic!("{err}"));
        let reason = crate::guard::leakage(&crate::guard::Edge {
            repo: repo.path().to_path_buf(),
            temp: repo.path().to_path_buf(),
            socket: None,
            allowed: false,
        })
        .unwrap_or_else(|| panic!("the guard fires for a checkout in the temp dir"));
        let notice = SpaceNotice::Withheld(reason.clone());

        let line = space_line(&notice, repo.path());
        assert!(
            line.starts_with("not attempted:"),
            "a withheld space is not the same news as a failed one: {line}"
        );
        assert!(
            !line.contains("shprd ·"),
            "no label is named for a space that was never wanted: {line}"
        );
        assert!(line.contains(&repo.path().display().to_string()), "{line}");

        let document = space_json(&notice);
        assert_eq!(document["state"], "withheld");
        assert_eq!(document["label"], Value::Null);
        assert_eq!(document["reason"].as_str(), Some(reason.as_str()));
    }

    #[test]
    fn the_host_is_not_asked_at_all_when_the_guard_refused() {
        // The one thing the guard is for: not a workspace in the wrong window,
        // and not a lookup either. `prepare_space` is the only caller that
        // touches the host at startup, so a test that skips it is a test that
        // left the host alone — and this one names the calls that would have
        // happened if it had not.
        let repo = TempDir::new().unwrap_or_else(|err| panic!("{err}"));
        let mux = FakeMux::default().with_starter_tab();
        let guard = crate::guard::leakage(&crate::guard::Edge {
            repo: repo.path().to_path_buf(),
            temp: repo.path().to_path_buf(),
            socket: None,
            allowed: false,
        });
        let notice = match &guard {
            Some(reason) => SpaceNotice::Withheld(reason.clone()),
            None => prepare_space(
                &mux,
                repo.path(),
                None,
                start_in(&mux, repo.path(), repo.path()),
            ),
        };
        assert!(matches!(notice, SpaceNotice::Withheld(_)), "{notice:?}");
        assert_eq!(
            mux.calls(),
            Vec::new(),
            "a refused start makes no workspace, looks none up and lists no tabs"
        );
    }
}
