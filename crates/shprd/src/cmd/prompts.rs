//! `shprd prompts`: the standing instructions a role starts with.
//!
//! The verb is the readable half of [`shprd_core::prompts`]: it lists the
//! prompts this machine has, prints one's words or its file, and puts the
//! shipped words back over an edit. Reading one writes it on first use, so the
//! file always exists for a person to find after the first command.
//!
//! The prompts directory is `$XDG_CONFIG_HOME/shprd/prompts`, else
//! `~/.config/shprd/prompts`, so a person who moves their XDG directories keeps
//! their words. The root is resolved once, here (the binary's edge), and the
//! library takes it as a path. Nothing here reads a run log, so the verb works
//! outside a repository.

use std::path::Path;

use anyhow::anyhow;
use shprd_core::prompts::{self, Installed, Listed, PromptError};

use crate::cli::{PromptIdArgs, PromptsCommand};
use crate::ctx::{CliError, resolve_config_home, resolve_home};

/// Runs `shprd prompts`, whichever verb the command line asked for.
pub fn run(command: &PromptsCommand, json: bool, home: Option<&Path>) -> Result<(), CliError> {
    let home = resolve_home(home)?;
    let dir = prompts::prompts_dir(&resolve_config_home(&home));
    match command {
        PromptsCommand::List => list(&dir, json),
        PromptsCommand::Show(args) => show(&dir, args, json),
        PromptsCommand::Path(args) => path(&dir, args, json),
        PromptsCommand::Reset(args) => reset(&dir, args, json),
    }
}

/// Prints every prompt: its id, whether it is the shipped default or an edit,
/// and the file it lives in.
fn list(dir: &Path, json: bool) -> Result<(), CliError> {
    let prompts = prompts::list(dir).map_err(invalid)?;
    if json {
        let prompt: Vec<serde_json::Value> = prompts
            .iter()
            .map(|listed| match listed {
                Listed::Prompt(installed) => document(installed, false),
                Listed::Unreadable { id, path, reason } => serde_json::json!({
                    "id": id,
                    "path": path.display().to_string(),
                    "state": "unreadable",
                    "reason": reason,
                }),
            })
            .collect();
        let value = serde_json::json!({
            "dir": dir.display().to_string(),
            "prompts": prompt,
        });
        println!("{value}");
    } else {
        for listed in &prompts {
            match listed {
                Listed::Prompt(installed) => println!("{}", installed.line()),
                Listed::Unreadable { id, path, reason } => {
                    println!("{id:<10} {:<8} {} — {reason}", "unreadable", path.display())
                }
            }
        }
    }
    Ok(())
}

/// Prints one prompt's words, exactly as the agent reads them.
fn show(dir: &Path, args: &PromptIdArgs, json: bool) -> Result<(), CliError> {
    let installed = prompts::ensure(dir, &args.id).map_err(invalid)?;
    if json {
        println!("{}", document(&installed, true));
    } else {
        print!("{}", installed.text);
    }
    Ok(())
}

/// Prints the file one prompt is stored in, writing it on first use.
fn path(dir: &Path, args: &PromptIdArgs, json: bool) -> Result<(), CliError> {
    let installed = prompts::ensure(dir, &args.id).map_err(invalid)?;
    if json {
        println!("{}", document(&installed, true));
    } else {
        println!("{}", installed.path.display());
    }
    Ok(())
}

/// Puts the shipped words back over whatever the file holds.
fn reset(dir: &Path, args: &PromptIdArgs, json: bool) -> Result<(), CliError> {
    let installed = prompts::reset(dir, &args.id).map_err(invalid)?;
    if json {
        println!("{}", document(&installed, true));
    } else {
        println!(
            "ok {} {}: {}",
            installed.change.name(),
            installed.id,
            installed.path.display()
        );
    }
    Ok(())
}

/// One prompt as JSON: the file, its state and what the command did, with its
/// words when the caller asked for them.
///
/// `list` leaves the words out — four prompts of prose per line is noise — and
/// `show`, `path` and `reset` carry them, so one document describes one prompt
/// completely.
fn document(installed: &Installed, with_text: bool) -> serde_json::Value {
    let mut value = serde_json::json!({
        "id": installed.id,
        "path": installed.path.display().to_string(),
        "state": installed.state.name(),
        "change": installed.change.name(),
        "shipped": installed.shipped,
        "about": installed.about,
    });
    if with_text && let Some(object) = value.as_object_mut() {
        object.insert("text".to_owned(), installed.text.clone().into());
    }
    value
}

/// A prompt failure is the person's to fix, so it is a refusal (exit 2), not a
/// crash.
fn invalid(error: PromptError) -> CliError {
    CliError::Invalid(anyhow!(error))
}
