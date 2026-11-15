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

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::anyhow;
use shprd_core::state::RunInfo;
use shprd_server::{
    AfterWrite, Config, NO_RUN_HINT, PassNotice, Service, TickConfig, TickReport, WsLimits,
    shutdown_signal, state_dir,
};

use shprd_server::ServerError::AlreadyRunning;

use crate::cli::ServeArgs;
use crate::ctx::{CliError, Ctx, resolve_home};

/// The variable that sets how much the service logs.
///
/// It takes the same directives `RUST_LOG` does — `warn` (the default), `info`,
/// `debug`, or a per-target form such as `shprd_server=debug`.
const LOG_ENV: &str = "SHPRD_LOG";

/// Runs the service until the process is asked to stop.
pub fn run(ctx: &Ctx, args: &ServeArgs, home: Option<&Path>) -> Result<(), CliError> {
    let home = resolve_home(home)?;
    install_subscriber();
    // The banner says whether there is a run at all, and the passes say so again
    // if one is started after the service was: a service that came up too early
    // must not look like an empty run.
    let run_at_start = ctx.state()?.run;
    let mut config = Config::new(ctx.root.clone(), state_dir(&home));
    config.port = args.port;
    config.actor = ctx.actor.clone();
    config.after_write = Some(after_write(ctx));
    let tick = tick_config(
        ctx,
        &home,
        config.hub.clone(),
        run_notice(
            ctx.json,
            run_at_start.as_ref().map(|run| run.run_id.clone()),
        ),
    )?;
    let announced = AnnouncedTick {
        interval_secs: tick.interval.as_secs(),
        max_running: tick.max_running,
    };
    config.tick = Some(tick);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|err| CliError::other(anyhow!(err).context("starting the async runtime")))?;

    runtime.block_on(async move {
        // A second service for the same state directory would take the record
        // over and leave the running one undiscoverable, so it is refused with
        // the exit code a refused verb uses rather than an error's.
        let service = Service::start(config).await.map_err(|err| match err {
            AlreadyRunning { .. } => CliError::Invalid(anyhow!(err)),
            other => CliError::other(anyhow!(other).context("starting the service")),
        })?;
        let info = service.info().clone();
        announce(ctx, &info, &announced, run_at_start.as_ref());
        service
            .run(shutdown_signal())
            .await
            .map_err(|err| CliError::other(anyhow!(err).context("serving the run")))
    })
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
) -> Result<TickConfig, CliError> {
    let config = crate::cmd::tick::load_config(ctx, home)?;
    let max_running = config.tick.max_running;
    let interval = Duration::from_secs(config.tick.interval_secs);
    let worker = crate::cmd::tick::worker_with_hub(ctx, home, config, hub)?;
    Ok(TickConfig {
        worker: std::sync::Arc::new(worker),
        interval,
        max_running,
        notice: Some(notice),
    })
}

/// What the startup line says about the tick.
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
    AfterWrite::new(move |repo| {
        if no_dagr {
            return;
        }
        let write_ctx = Ctx::new(repo.to_path_buf(), actor.clone(), false, false);
        crate::cmd::agent::sync_dagr(&write_ctx);
    })
}

/// Says where the service is and how to reach it.
///
/// The token itself is never printed — only the file it is in, which is `0600`
/// in the state directory. The run is named when there is one, and the line
/// says what to do when there is not: a service that came up before
/// `shprd run start` must not look like a run with no tasks in it.
fn announce(
    ctx: &Ctx,
    info: &shprd_server::ServiceInfo,
    tick: &AnnouncedTick,
    run: Option<&RunInfo>,
) {
    let limits = WsLimits::default();
    let hint = run.is_none().then_some(NO_RUN_HINT);
    if ctx.json {
        let value = serde_json::json!({
            "serving": ctx.root.display().to_string(),
            "port": info.port,
            "url": format!("http://127.0.0.1:{}", info.port),
            "id": info.id,
            "version": info.version,
            "pid": info.pid,
            "token": info.token_path.display().to_string(),
            "service_json": info.service_json.display().to_string(),
            "run": run.map(|run| serde_json::json!({
                "id": run.run_id,
                "base": run.base_branch,
                "integration": run.integration_branch,
            })),
            "hint": hint,
            "ws": {
                "ping_secs": limits.ping.as_secs(),
                "idle_secs": limits.idle.as_secs(),
                "session_secs": limits.session.as_secs(),
            },
            "tick": {
                "interval_secs": tick.interval_secs,
                "max_running": tick.max_running,
            },
        });
        println!("{value}");
        return;
    }
    println!(
        "ok serving {} on http://127.0.0.1:{} (id {}, version {})",
        ctx.root.display(),
        info.port,
        info.id,
        info.version
    );
    match run {
        Some(run) => println!(
            "   run:          {} · {} → {}",
            run.run_id, run.base_branch, run.integration_branch
        ),
        None => println!("   run:          {NO_RUN_HINT}"),
    }
    println!("   token:        {}", info.token_path.display());
    println!("   service.json: {}", info.service_json.display());
    println!(
        "   sockets:      ping every {}s, closed after {}s of silence, {}m at most",
        limits.ping.as_secs(),
        limits.idle.as_secs(),
        limits.session.as_secs() / 60
    );
    println!(
        "   tick:         every {}s, at most {} task(s) running",
        tick.interval_secs, tick.max_running
    );
    println!("   stop with Ctrl-C or SIGTERM");
}
