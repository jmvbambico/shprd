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
//! coordinator's; a task gets its own when it is spawned, and loses it when it
//! lands. The banner says whether it found the space, made it, or could not
//! reach the host at all.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::anyhow;
use serde_json::Value;
use shprd_core::state::RunInfo;
use shprd_mux::Host;
use shprd_server::{
    AfterWrite, Config, NO_RUN_HINT, PassNotice, Service, TickConfig, TickReport, WsLimits,
    shutdown_signal,
};

use shprd_server::ServerError::AlreadyRunning;

use crate::cli::ServeArgs;
use crate::ctx::{
    CliError, Ctx, resolve_config_home, resolve_data_dir, resolve_home, resolve_state_home,
};

/// The variable that sets how much the service logs.
///
/// It takes the same directives `RUST_LOG` does — `warn` (the default), `info`,
/// `debug`, or a per-target form such as `shprd_server=debug`.
const LOG_ENV: &str = "SHPRD_LOG";

/// Runs the service until the process is asked to stop.
pub fn run(ctx: &Ctx, args: &ServeArgs, home: Option<&Path>) -> Result<(), CliError> {
    let home = resolve_home(home)?;
    install_subscriber();
    // The host is resolved once, here, and passed down: every pass, every spawn
    // and every close of this service drives the same adapter, so a run cannot
    // half-happen on one multiplexer and half on another (M10.9).
    let (resolution, host) = crate::host::select();
    // The coordinator's space is part of the host setup a run needs, and its
    // label is derived rather than chosen, so the service makes it rather than
    // leaving a pass to refuse (M9 gate bug 6). A host that cannot be reached is
    // a warning and not a refusal to serve: the board is still what a person
    // reads, and the next start can make the space.
    let workspace = prepare_space(host.as_ref(), &ctx.root);
    if let SpaceNotice::Unavailable(reason) = &workspace {
        tracing::warn!(%reason, "the repository's space is not ready: a pass cannot start a task until it is");
    }
    // The banner says whether there is a run at all, and the passes say so again
    // if one is started after the service was: a service that came up too early
    // must not look like an empty run.
    let run_at_start = ctx.state()?.run;
    // Everything the service reads from the machine is resolved here, once: the
    // discovery files' state directory, the person's home, their prompts and the
    // harness table. The service itself reads no environment variable.
    let state_home = resolve_state_home(&home);
    let mut config = Config::new(ctx.root.clone(), shprd_server::state_dir(&state_home));
    config.port = args.port;
    config.actor = ctx.actor.clone();
    // The context the service's own writes run with: the same repository, actor
    // and flags, plus the home this start resolved. A write made through the
    // API reaches the same dagr projection a CLI write does.
    let write_ctx = ctx.clone().with_home(Some(home.clone()));
    config.after_write = Some(after_write(&write_ctx));
    // Where the graph's program is, found once here: the window's graph block
    // asks the service rather than probing this machine itself, and the answer
    // is the same one the projection is checked with.
    let dagr = crate::cmd::dagr::dagr(&write_ctx);
    // The window's graph block is a layout terminal block, and Herdr prunes such
    // a block when its terminal does not exist when the layout loads. The service
    // is up before the window is, so it starts the terminal; the mod in the
    // window keeps its own start as a fallback (M10.7).
    crate::cmd::graph::ensure(&resolution, &dagr, &ctx.root);
    config.dagr = Some(dagr);
    config.home = Some(home.clone());
    config.prompts_dir = Some(shprd_core::prompts::prompts_dir(&resolve_config_home(
        &home,
    )));
    config.data = Some(resolve_data_dir());
    let tick = tick_config(
        ctx,
        &home,
        config.hub.clone(),
        run_notice(
            ctx.json,
            run_at_start.as_ref().map(|run| run.run_id.clone()),
        ),
        std::sync::Arc::from(host),
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
        announce(
            ctx,
            &info,
            &announced,
            run_at_start.as_ref(),
            &workspace,
            &resolution,
        );
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

/// What the service did about the repository's own space when it started.
///
/// The label is derived ([`crate::cmd::spawn::repo_space_name`]) and a host
/// refuses a duplicate, so nothing here renames, adopts or removes a space: the
/// one shprd uses is the one whose label is exactly right, and everything else
/// on the host is left alone. Task spaces are not the service's business at
/// startup: they come and go with their tasks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SpaceNotice {
    /// The space was already there and is reused.
    Reused(String),
    /// The space was missing and is created at the repository root.
    Created(String),
    /// It could not be found or made; the reason names the label it wanted.
    Unavailable(String),
}

/// Finds the repository's own space, or creates it at the repository root.
///
/// This is the step that used to be left to a person (`docs/multiplexers.md`
/// states the convention, but the folder name is load-bearing and a label that
/// was almost right made every pass refuse). A host that cannot be asked at all
/// is reported rather than fatal, because the API is useful without one.
fn prepare_space(mux: &dyn Host, repo: &Path) -> SpaceNotice {
    match crate::cmd::spawn::ensure_repo_space(mux, repo) {
        Ok(prepared) if prepared.created => SpaceNotice::Created(prepared.space.name),
        Ok(prepared) => SpaceNotice::Reused(prepared.space.name),
        Err(err) => SpaceNotice::Unavailable(err.to_string()),
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
    let max_running = config.tick.max_running;
    let interval = Duration::from_secs(config.tick.interval_secs);
    let worker = crate::cmd::tick::worker_with_hub(ctx, home, config, hub, host)?;
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
/// in the state directory. The run is named when there is one, and the line
/// says what to do when there is not: a service that came up before
/// `shprd run start` must not look like a run with no tasks in it.
fn announce(
    ctx: &Ctx,
    info: &shprd_server::ServiceInfo,
    tick: &AnnouncedTick,
    run: Option<&RunInfo>,
    space: &SpaceNotice,
    host: &crate::host::Resolution,
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
            "space": space_json(space),
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
    println!("   space:        {}", space_line(space, &ctx.root));
    println!("   host:         {}", host.line());
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

/// The banner's space line: what the service found, made, or could not make.
///
/// It is one line either way, so a service whose host is missing still prints a
/// banner a person can read, and the failure to prepare the space is in the
/// place the rest of the setup is.
fn space_line(notice: &SpaceNotice, root: &Path) -> String {
    match notice {
        SpaceNotice::Created(label) => {
            format!(
                "created {label:?} at the repository root {}",
                root.display()
            )
        }
        SpaceNotice::Reused(label) => format!("{label:?} (already there, reused)"),
        SpaceNotice::Unavailable(reason) => format!("not ready: {reason}"),
    }
}

/// The banner's space line, as the one document's field.
///
/// A space that could not be prepared has no label to report, so it says so
/// and carries the reason: the label is what a person fixes, and the reason is
/// the only thing that says which label was wanted.
fn space_json(notice: &SpaceNotice) -> Value {
    match notice {
        SpaceNotice::Created(label) => {
            serde_json::json!({ "label": label, "state": "created" })
        }
        SpaceNotice::Reused(label) => {
            serde_json::json!({ "label": label, "state": "reused" })
        }
        SpaceNotice::Unavailable(reason) => {
            serde_json::json!({ "label": Value::Null, "state": "unavailable", "reason": reason })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shprd_mux::fake::{Call, FakeMux};
    use tempfile::TempDir;

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
        let mux = FakeMux::default();
        let label = label_of(repo.path());

        let notice = prepare_space(&mux, repo.path());

        assert_eq!(notice, SpaceNotice::Created(label.clone()));
        assert_eq!(
            mux.calls(),
            vec![
                Call::FindSpace(label.clone()),
                Call::CreateSpace(label.clone(), repo.path().to_path_buf()),
            ],
            "one lookup, one creation, and nothing else touched"
        );
        let line = space_line(&notice, repo.path());
        assert!(line.contains("created"), "{line}");
        assert!(line.contains(&label), "the label is named: {line}");
    }

    #[test]
    fn a_service_reuses_the_space_that_is_already_there() {
        let repo = TempDir::new().unwrap_or_else(|err| panic!("{err}"));
        let mux = FakeMux::default();
        let label = label_of(repo.path());
        let made = mux
            .create_space(&label, repo.path())
            .unwrap_or_else(|err| panic!("{err}"));
        let _ = mux.take_calls();

        let notice = prepare_space(&mux, repo.path());

        assert_eq!(notice, SpaceNotice::Reused(label.clone()));
        assert_eq!(
            mux.calls(),
            vec![Call::FindSpace(label)],
            "a lookup only: nothing is created, renamed or focused"
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
}
