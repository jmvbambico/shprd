//! `shprd dagr sync`: project the run as a dagr v3 document and write it.
//!
//! The document is `<root>/.dagr/run.json`. It is checked with a `dagr` binary
//! when one is available, and written unchecked (with one note on stderr that
//! names the setting to fix) when it is not, so a run without dagr installed
//! still leaves a document behind.
//!
//! Where dagr is comes from [`crate::cmd::dagr::locate`], which is
//! `shprd-server`'s one answer to that question: `$SHPRD_DAGR_BIN`, then the
//! person's `graph.dagr` setting, then `PATH`, then the `herdr-dagr` plugin's
//! checkout. The same answer is what the Herdr window's graph block runs, so a
//! document is checked by the binary that draws it.

use std::path::{Path, PathBuf};

use anyhow::anyhow;
use shprd_core::log::{Clock, SystemClock};
use shprd_core::settings::Loaded;
use shprd_server::graph::{self, Dagr, Located, Missing};
use shprd_views::dagr::{ViewError, project, write_atomic};

use crate::ctx::{CliError, Ctx, resolve_config_home};

/// Environment variable naming the checker to use.
pub const CHECKER_ENV: &str = graph::ENV;

/// Where the document is written: `<root>/.dagr/run.json`.
pub fn run_json_path(root: &Path) -> PathBuf {
    graph::run_json(root)
}

/// The dagr this machine has, from the home the binary resolved.
///
/// `home` is `--home`, else `$SHPRD_HOME`, else `$HOME`, already read at the
/// edge. The settings are read from that home and the plugin folder is looked
/// for under its configuration root, so nothing here reads the environment for
/// itself and a test that names its own home can never find the developer's
/// install.
///
/// # Errors
///
/// [`Missing`] when no probe found a program, or when the one that won is not an
/// executable file.
pub fn locate(home: Option<&Path>) -> Result<Located, Missing> {
    let setting = home.and_then(setting_in);
    let config_home = home.map(resolve_config_home);
    graph::locate(
        config_home.as_deref().unwrap_or(Path::new("")),
        std::env::var_os("PATH").as_deref(),
        std::env::var_os(CHECKER_ENV).as_deref(),
        setting.as_deref(),
    )
}

/// `graph.dagr` as `home`'s settings resolve it, when it names anything.
///
/// A settings file that cannot be read is no setting: the person's file being
/// broken is `shprd settings show`'s to report, and a projection that refused
/// to write because of it would be a worse failure than an unchecked document.
fn setting_in(home: &Path) -> Option<std::ffi::OsString> {
    let resolved = Loaded::read(home, None).ok()?.resolve(None);
    graph::as_os(resolved.graph_dagr().map(Path::new))
}

/// What the graph block should run, and why it cannot when it cannot.
///
/// `Ctx` carries the home the binary resolved, so this is the same answer
/// [`locate`] gives, in the shape the service reports.
pub fn dagr(ctx: &Ctx) -> Dagr {
    Dagr::from(locate(ctx.home.as_deref()))
}

/// The document projected from the current log, or `None` when the run has
/// not started yet.
fn project_run(ctx: &Ctx) -> Result<Option<serde_json::Value>, CliError> {
    let (state, events) = ctx.load()?;
    let now_ms = SystemClock.now_ms();
    match project(&state, &events, now_ms) {
        Ok(doc) => Ok(Some(doc)),
        Err(ViewError::NoRun) => Ok(None),
        Err(err) => Err(CliError::other(anyhow!(err).context("projecting the run"))),
    }
}

/// Projects the run and writes `<root>/.dagr/run.json`, returning the path it
/// wrote, or `None` when the run has not started.
///
/// The document is checked when a program is found, and written unchecked with
/// one note on stderr when none is. The note names `graph.dagr`, because a
/// person who wants their projections checked needs to know which key fixes it.
/// A checker that rejects the document is an error carrying its findings, and
/// leaves the previous document in place.
///
/// # Errors
///
/// [`CliError::Other`] when the log cannot be read, the projection cannot be
/// written, or the checker rejects it.
pub fn sync(ctx: &Ctx) -> Result<Option<PathBuf>, CliError> {
    let Some(doc) = project_run(ctx)? else {
        return Ok(None);
    };
    let path = run_json_path(&ctx.root);
    let located = locate(ctx.home.as_deref());
    if let Err(missing) = &located {
        eprintln!("dagr not found; wrote unchecked: {missing}");
    }
    match write_atomic(
        &doc,
        &path,
        located.as_ref().ok().map(|found| found.path.as_path()),
    ) {
        Ok(()) => Ok(Some(path)),
        Err(err) => Err(CliError::other(
            anyhow!(err).context(format!("syncing {}", path.display())),
        )),
    }
}

/// The body of `shprd dagr sync`: sync, and report where the document went.
///
/// With no run yet there is nothing to project, so this says so on stderr and
/// succeeds.
pub fn sync_verb(ctx: &Ctx) -> Result<(), CliError> {
    let Some(path) = sync(ctx)? else {
        eprintln!("no run yet; nothing to project");
        return Ok(());
    };
    if ctx.json {
        let doc = serde_json::json!({
            "path": path.display().to_string(),
            "dagr": match locate(ctx.home.as_deref()) {
                Ok(found) => serde_json::json!({
                    "path": found.path.display().to_string(),
                    "origin": found.origin.name(),
                }),
                Err(missing) => serde_json::json!({ "missing": missing.message() }),
            },
        });
        let text = serde_json::to_string(&doc)
            .map_err(|err| CliError::other(anyhow!(err).context("serializing the result")))?;
        println!("{text}");
    } else {
        println!("wrote {}", path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn the_document_sits_where_the_graph_block_reads_it() {
        let root = Path::new("/repo");
        assert_eq!(
            run_json_path(root),
            PathBuf::from("/repo/.dagr/run.json"),
            "the block runs dagr view on exactly this path"
        );
    }

    #[test]
    fn a_home_with_no_settings_names_no_program() {
        // Pure enough for a unit test: the home is a temp dir, so nothing reads
        // the developer's own settings file.
        let home = tempfile::tempdir().expect("a temp home");
        assert_eq!(setting_in(home.path()), None);
        let ctx = Ctx::new(
            home.path().to_path_buf(),
            shprd_core::event::Actor::Human,
            false,
            true,
        );
        assert!(
            ctx.home.is_none(),
            "a context built without a home has none"
        );
    }

    #[test]
    fn the_graph_dagr_setting_is_what_the_wiring_hands_the_locator() {
        let home = tempfile::tempdir().expect("a temp home");
        let dir = home.path().join(".config").join("shprd");
        std::fs::create_dir_all(&dir).expect("the config folder");
        std::fs::write(
            dir.join("settings.toml"),
            "[graph]\ndagr = \"/opt/dagr/bin/dagr\"\n",
        )
        .expect("the settings file");
        assert_eq!(
            setting_in(home.path()),
            Some(std::ffi::OsString::from("/opt/dagr/bin/dagr"))
        );
    }

    #[test]
    fn a_context_carries_the_home_the_binary_resolved() {
        // What `mod.rs` hands every verb once, so the projection that follows a
        // write reads the person's settings instead of resolving `$HOME` again.
        let home = std::path::PathBuf::from("/home/example");
        let ctx = Ctx::new(
            std::path::PathBuf::from("/repo"),
            shprd_core::event::Actor::Human,
            false,
            false,
        )
        .with_home(Some(home.clone()));
        assert_eq!(ctx.home, Some(home));
        let plain = Ctx::new(
            std::path::PathBuf::from("/repo"),
            shprd_core::event::Actor::Human,
            false,
            false,
        );
        assert!(plain.home.is_none(), "a context built plainly has none");
    }
}
