//! `shprd serve`: the loopback API every shprd UI calls.
//!
//! The service reads and writes this repository's run log through
//! `shprd-server`, which uses the same fold, rules and append the CLI does. It
//! writes its discovery files under the XDG state directory, binds `127.0.0.1`
//! only, and stops on `SIGINT` or `SIGTERM` — on a clean stop it removes
//! `service.json` so no client is left pointing at a dead port.

use std::path::Path;

use anyhow::anyhow;
use shprd_server::{AfterWrite, Config, Service, shutdown_signal, state_dir};

use crate::cli::ServeArgs;
use crate::ctx::{CliError, Ctx, resolve_home};

/// Runs the service until the process is asked to stop.
pub fn run(ctx: &Ctx, args: &ServeArgs, home: Option<&Path>) -> Result<(), CliError> {
    let home = resolve_home(home)?;
    let mut config = Config::new(ctx.root.clone(), state_dir(&home));
    config.port = args.port;
    config.actor = ctx.actor.clone();
    config.after_write = Some(after_write(ctx));

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|err| CliError::other(anyhow!(err).context("starting the async runtime")))?;

    runtime.block_on(async move {
        let service = Service::start(config)
            .await
            .map_err(|err| CliError::other(anyhow!(err).context("starting the service")))?;
        let info = service.info().clone();
        announce(ctx, &info);
        service
            .run(shutdown_signal())
            .await
            .map_err(|err| CliError::other(anyhow!(err).context("serving the run")))
    })
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
fn announce(ctx: &Ctx, info: &shprd_server::ServiceInfo) {
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
    println!("   stop with Ctrl-C or SIGTERM");
}
