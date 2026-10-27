//! `shprd roster`: what agent CLIs this machine has, and whether they work.
//!
//! Detection is read-only: the verb runs each CLI's version and sign-in
//! commands and prints a line for each. It needs no run log, so it works
//! outside a repository.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::anyhow;
use shprd_roster::detect::{Detection, SignIn, detect_all, vendor_name};
use shprd_roster::spec::{HarnessSpec, load_dir};

use crate::cli::RosterArgs;
use crate::ctx::CliError;

/// Environment variable pointing shprd at the data directory, for a checkout
/// or a packaged install.
const DATA_ENV: &str = "SHPRD_DATA_DIR";

/// Environment variable giving the model to probe the pi sign-in with.
const PI_MODEL_ENV: &str = "SHPRD_PI_MODEL";

/// The spec id whose sign-in probe needs a model.
const PI_ID: &str = "pi";

/// Runs `shprd roster`, printing one line per CLI or a JSON array.
pub fn run(args: &RosterArgs, json: bool) -> Result<(), CliError> {
    let dir = resolve_data_dir()?;
    let specs = load_dir(&dir.join("harness")).map_err(|err| {
        CliError::other(anyhow!(err).context(format!("loading {}", dir.display())))
    })?;
    let path_env = std::env::var("PATH").unwrap_or_default();
    let detections = detect_all(&specs, &path_env, &[], &models(&specs));
    if json || args.json {
        print_json(&detections)?;
    } else {
        for detection in &detections {
            println!("{}", describe(detection));
        }
    }
    Ok(())
}

/// The model each spec's probes should use, from the environment.
fn models(specs: &[HarnessSpec]) -> BTreeMap<String, String> {
    let mut models = BTreeMap::new();
    if specs.iter().any(|spec| spec.id == PI_ID)
        && let Ok(model) = std::env::var(PI_MODEL_ENV)
        && !model.trim().is_empty()
    {
        models.insert(PI_ID.to_owned(), model);
    }
    models
}

/// The data directory: `$SHPRD_DATA_DIR`, else `<git root>/data`, else the
/// `../share/shprd/data` a packaged install puts next to the binary.
pub(crate) fn resolve_data_dir() -> Result<PathBuf, CliError> {
    if let Some(dir) = std::env::var_os(DATA_ENV).filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    let root = crate::ctx::resolve_root(None).ok();
    let packaged = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .map(|dir| dir.join("../share/shprd/data"));
    let candidates = [root.map(|root| root.join("data")), packaged];
    for candidate in candidates.into_iter().flatten() {
        if candidate.is_dir() {
            return Ok(candidate);
        }
    }
    Err(CliError::other(anyhow!(
        "no harness data directory: set {DATA_ENV} to a directory holding `harness/`"
    )))
}

/// One detection as the aligned-ish human line the verb prints.
fn describe(detection: &Detection) -> String {
    let binary = match &detection.binary {
        Some(path) => path.display().to_string(),
        None => "not installed".to_owned(),
    };
    let version = detection.version.as_deref().unwrap_or("-");
    format!(
        "{}  {}  {}  {}  {}",
        detection.id,
        binary,
        version,
        signin_text(&detection.signin),
        vendor_name(detection.vendor)
    )
}

/// The sign-in state as it reads in a line.
fn signin_text(signin: &SignIn) -> String {
    match signin {
        SignIn::SignedIn => "signed in".to_owned(),
        SignIn::SignedOut => "signed out".to_owned(),
        SignIn::Unknown(reason) => format!("unknown ({reason})"),
    }
}

/// Prints the detections as a JSON array.
fn print_json(detections: &[Detection]) -> Result<(), CliError> {
    let rows: Vec<serde_json::Value> = detections
        .iter()
        .map(|detection| {
            let (state, reason) = match &detection.signin {
                SignIn::SignedIn => ("signed_in", None),
                SignIn::SignedOut => ("signed_out", None),
                SignIn::Unknown(reason) => ("unknown", Some(reason.clone())),
            };
            serde_json::json!({
                "id": detection.id,
                "name": detection.name,
                "vendor": vendor_name(detection.vendor),
                "binary": detection.binary.as_ref().map(|path| path.display().to_string()),
                "version": detection.version,
                "signin": state,
                "reason": reason,
            })
        })
        .collect();
    let text = serde_json::to_string_pretty(&rows)
        .map_err(|err| CliError::other(anyhow!(err).context("serializing the roster")))?;
    println!("{text}");
    Ok(())
}
