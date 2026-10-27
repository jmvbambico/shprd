//! `shprd keys`: show, install and remove herdr's shprd keybinds.
//!
//! The herdr config is a file a human also edits, so shprd writes only the
//! marked block `shprd-mux-herdr::keys` owns and leaves the rest untouched.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::anyhow;
use shprd_mux_herdr::keys::{self, KeysError};

use crate::cli::{KeysArgs, KeysCommand};
use crate::ctx::{CliError, resolve_home};

/// The env var that overrides the herdr config path.
const CONFIG_ENV: &str = "HERDR_CONFIG_PATH";

/// Runs `keys show|install|uninstall`.
pub fn run(args: &KeysArgs, home: Option<&Path>) -> Result<(), CliError> {
    let path = config_path(args.config.as_deref(), home)?;
    match args.cmd {
        KeysCommand::Show => {
            print!("{}", keys::render(&keys::default_bindings()));
            Ok(())
        }
        KeysCommand::Install => {
            let text = read(&path)?;
            let installed = keys::install(&text, &keys::default_bindings()).map_err(refused)?;
            written(&path, &installed)
        }
        KeysCommand::Uninstall => {
            let text = read(&path)?;
            let removed = keys::uninstall(&text).map_err(refused)?;
            written(&path, &removed)
        }
    }
}

/// The file to edit: `--config`, then `$HERDR_CONFIG_PATH`, then the global
/// herdr file under [`home`].
fn config_path(explicit: Option<&Path>, home: Option<&Path>) -> Result<PathBuf, CliError> {
    if let Some(path) = explicit {
        return Ok(path.to_path_buf());
    }
    let from_env = std::env::var_os(CONFIG_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    if let Some(path) = from_env {
        return Ok(path);
    }
    let home = resolve_home(home)?;
    Ok(home.join(".config").join("herdr").join("config.toml"))
}

/// The file's current text; a file that does not exist yet reads as empty.
fn read(path: &Path) -> Result<String, CliError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(CliError::other(
            anyhow!(error).context(format!("reading {}", path.display())),
        )),
    }
}

/// Writes the new text atomically, then says how to make herdr pick it up.
fn written(path: &Path, text: &str) -> Result<(), CliError> {
    keys::write_atomic(path, text).map_err(refused)?;
    println!("run: herdr server reload-config");
    println!("wrote: {}", path.display());
    Ok(())
}

/// Maps a refusal (unbalanced block, conflict, unparsable text) to exit 2.
fn refused(error: KeysError) -> CliError {
    match error {
        KeysError::Io(source) => CliError::other(anyhow!(source)),
        other => CliError::Invalid(anyhow!(other).context("checking the herdr config")),
    }
}
