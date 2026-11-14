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

use std::path::Path;
use std::time::Duration;

use anyhow::anyhow;
use shprd_server::{AfterWrite, Config, Service, TickConfig, WsLimits, shutdown_signal, state_dir};

use shprd_server::ServerError::AlreadyRunning;

use crate::cli::ServeArgs;
use crate::ctx::{CliError, Ctx, resolve_home};

/// Runs the service until the process is asked to stop.
pub fn run(ctx: &Ctx, args: &ServeArgs, home: Option<&Path>) -> Result<(), CliError> {
    let home = resolve_home(home)?;
    let mut config = Config::new(ctx.root.clone(), state_dir(&home));
    config.port = args.port;
    config.actor = ctx.actor.clone();
    config.after_write = Some(after_write(ctx));
    let tick = tick_config(ctx, &home, config.hub.clone())?;
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
        announce(ctx, &info, &announced);
        service
            .run(shutdown_signal())
            .await
            .map_err(|err| CliError::other(anyhow!(err).context("serving the run")))
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
) -> Result<TickConfig, CliError> {
    let config = crate::cmd::tick::load_config(ctx, home)?;
    let max_running = config.tick.max_running;
    let interval = Duration::from_secs(config.tick.interval_secs);
    let worker = crate::cmd::tick::worker_with_hub(ctx, home, config, hub)?;
    Ok(TickConfig {
        worker: std::sync::Arc::new(worker),
        interval,
        max_running,
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
/// in the state directory.
fn announce(ctx: &Ctx, info: &shprd_server::ServiceInfo, tick: &AnnouncedTick) {
    let limits = WsLimits::default();
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
