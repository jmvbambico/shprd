//! `shprd settings`: read and write the settings, and say where each value came
//! from.
//!
//! The verb is the readable half of [`shprd_core::settings`]: it resolves the
//! layers for this machine and this repository, prints one line per key with the
//! layer that answered, and writes the person's own file (never a repository's
//! committed one) through an edit that leaves every other key and comment alone.
//!
//! A write is checked before it lands. The settings themselves are checked when
//! the file is parsed; the roles are checked against the harness table
//! (`shprd_agents::roles`), which is what refuses a reviewer that is not another
//! vendor and a role that names a program, mode or effort the program does not
//! have. A refusal exits 2 and writes nothing, and a write that *introduces* a
//! refusal is the one refused: a file someone else left broken does not block
//! every other key. Reading never refuses: a set of settings that would not work
//! is printed, with its refusals as warnings, because the person has to see the
//! broken line to fix it.
//!
//! The role checks need the harness table, and a machine without one cannot be
//! checked at all — so a role is never stored unchecked. Writing a role key
//! without the table is refused, naming the directory to set, and reading says
//! the checks were skipped instead of printing a clean bill of health.
//!
//! `--task` resolves for one task of the run: the agent its own newest attempt
//! ran is the nearest layer, which is the only per-task fact the log holds. A
//! task's own settings cannot be written from here, and the refusal says so.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use anyhow::anyhow;
use shprd_agents::roles::{Checked, Checker, Review};
use shprd_core::event::Actor;
use shprd_core::settings::{self, Key, Layer, Loaded, Resolved, RoleField, Scope, Source, Value};

use crate::cli::{
    SettingsCommand, SettingsGetArgs, SettingsSetArgs, SettingsShowArgs, SettingsUnsetArgs,
};
use crate::ctx::{CliError, Ctx, resolve_home};

/// How long one model listing may take while a read is answered.
///
/// The catalog only feeds a warning, so a slow or absent CLI must not hold the
/// verb: a listing that does not answer in time simply says nothing.
const CATALOG_TIMEOUT: Duration = Duration::from_secs(5);

/// Runs `shprd settings`, whichever verb the command line asked for.
///
/// `root` is the repository the settings are for: `--repo`, else the current
/// checkout, else none at all (outside a repository only the person's own file
/// applies).
pub fn run(
    command: &SettingsCommand,
    json: bool,
    home: Option<&Path>,
    root: Option<&Path>,
) -> Result<(), CliError> {
    let home = resolve_home(home)?;
    let loaded = load(&home, root)?;
    let checker = Checker::load(crate::ctx::resolve_data_dir(root).ok().as_deref());
    match command {
        SettingsCommand::Show(args) => show(&loaded, root, args, json, &checker),
        SettingsCommand::Get(args) => get(&loaded, root, args, json, &checker),
        SettingsCommand::Set(args) => set(&loaded, &home, args, json, &checker),
        // Unsetting is the way out of a file that will not check, so it is
        // never held back by a check.
        SettingsCommand::Unset(args) => unset(&loaded, &home, args, json),
    }
}

/// Read the settings files, turning a refusal into the verb's invalid exit.
fn load(home: &Path, root: Option<&Path>) -> Result<Loaded, CliError> {
    Loaded::read(home, root).map_err(invalid)
}

/// The layer one repository's files add, as the resolution wants it.
fn scope(loaded: &Loaded, for_repo: bool) -> Result<Scope, CliError> {
    if !for_repo {
        return Ok(Scope::You);
    }
    match &loaded.repo {
        Some(name) => Ok(Scope::Repo(name.clone())),
        None => Err(invalid(anyhow!(
            "`--for-repo` writes a repository's entry, so run it inside a repository or pass `--repo`"
        ))),
    }
}

/// The nearest layer for a task: what its own newest attempt ran.
fn task_layer(root: Option<&Path>, task: Option<&str>) -> Result<Option<Layer>, CliError> {
    let Some(task) = task else {
        return Ok(None);
    };
    let root = root.ok_or_else(|| {
        invalid(anyhow!(
            "`--task {task}` reads the run log, so run it inside a repository or pass `--repo`"
        ))
    })?;
    let ctx = Ctx::new(root.to_path_buf(), Actor::Human, false, true);
    let state = ctx.state()?;
    let found = state.tasks.get(task).ok_or_else(|| {
        invalid(anyhow!(
            "no task `{task}` in this run, so nothing is known to resolve for it"
        ))
    })?;
    let Some(attempt) = found.attempts.last() else {
        return Ok(None);
    };
    let mut layer = Layer::new();
    let (program, model) = match attempt.agent.split_once('/') {
        Some((program, model)) if !model.is_empty() => (program.to_owned(), Some(model.to_owned())),
        _ => (attempt.agent.clone(), None),
    };
    layer.set(
        Key::Role {
            id: "worker".to_owned(),
            field: RoleField::Agent,
        },
        Value::Text(program),
    );
    if let Some(model) = attempt.model.clone().or(model) {
        layer.set(
            Key::Role {
                id: "worker".to_owned(),
                field: RoleField::Model,
            },
            Value::Text(model),
        );
    }
    Ok(Some(layer))
}

/// Runs `settings show`.
fn show(
    loaded: &Loaded,
    root: Option<&Path>,
    args: &SettingsShowArgs,
    json: bool,
    checker: &Checker,
) -> Result<(), CliError> {
    let task = task_layer(root, args.task.as_deref())?;
    let resolved = loaded.resolve(task.as_ref());
    let checker = with_catalogs(checker.clone(), &resolved, root);
    let checked = check(&resolved, &checker);
    let skipped = skipped_note(&checked, &resolved);
    if json {
        let values: Vec<serde_json::Value> = resolved
            .rows()
            .into_iter()
            .map(|row| row_json(&row))
            .collect();
        let document = serde_json::json!({
            "repo": loaded.repo,
            "values": values,
            "problems": checked.messages(),
            "warnings": checked.warnings(),
            "not_checked": skipped,
        });
        print_json(&document)?;
        return Ok(());
    }
    for row in resolved.rows() {
        println!("{}", line(&row));
    }
    for problem in checked.messages() {
        eprintln!("warning: {problem}");
    }
    for warning in checked.warnings() {
        eprintln!("warning: {warning}");
    }
    if let Some(note) = skipped {
        eprintln!("warning: {note}");
    }
    Ok(())
}

/// Runs `settings get`.
fn get(
    loaded: &Loaded,
    root: Option<&Path>,
    args: &SettingsGetArgs,
    json: bool,
    checker: &Checker,
) -> Result<(), CliError> {
    let key = Key::parse(&args.key).map_err(invalid)?;
    let task = task_layer(root, args.task.as_deref())?;
    let resolved = loaded.resolve(task.as_ref());
    let row = row_of(&resolved, &key);
    let checker = with_catalogs(checker.clone(), &resolved, root);
    let checked = check(&resolved, &checker);
    let skipped = skipped_note(&checked, &resolved);
    if json {
        let mut document = row_json(&row);
        document["not_checked"] = serde_json::json!(skipped);
        document["warnings"] = serde_json::json!(checked.warnings());
        print_json(&document)?;
        return Ok(());
    }
    println!("{}", line(&row));
    for problem in checked.messages() {
        eprintln!("warning: {problem}");
    }
    for warning in checked.warnings() {
        eprintln!("warning: {warning}");
    }
    if let Some(note) = skipped {
        eprintln!("warning: {note}");
    }
    Ok(())
}

/// Runs `settings set`.
fn set(
    loaded: &Loaded,
    home: &Path,
    args: &SettingsSetArgs,
    json: bool,
    checker: &Checker,
) -> Result<(), CliError> {
    refuse_a_task_write(args.task.as_deref())?;
    let key = Key::parse(&args.key).map_err(invalid)?;
    let value = key.value_from_text(&args.value).map_err(invalid)?;
    let scope = scope(loaded, args.for_repo)?;
    // A role is only stored once the harness table has been asked: storing an
    // unchecked one would let a same-vendor reviewer in with no warning at all.
    if key.decides_a_review()
        && let Some(reason) = checker.reason()
    {
        return Err(invalid(anyhow!(
            "{} cannot be stored: {reason}. Nothing was written",
            key.name()
        )));
    }
    // Check the settings the write would leave behind, before it leaves
    // anything: a refused change never reaches the disk. A refusal that is
    // already in the file is not this write's to answer, so only the new ones
    // count — otherwise a broken pair someone left would block every other key.
    let before = check(&loaded.resolve(None), checker);
    let candidate = loaded.preview(&scope, &key, Some(&value), None);
    let after = check(&candidate, checker);
    let introduced = after.introduced(&before);
    if !introduced.is_empty() {
        return Err(invalid(anyhow!(
            "{} would be refused: {}",
            key.name(),
            introduced.join("; ")
        )));
    }
    let written = loaded
        .write(home, &scope, &key, Some(value), None)
        .map_err(invalid)?;
    let row = row_of(&written.resolved, &key);
    if json {
        print_json(&serde_json::json!({
            "outcome": written.outcome.name(),
            "key": row.key,
            "value": value_json(&row.value),
            "source": row.source.map(Source::name),
        }))?;
        return Ok(());
    }
    println!("{}", written.outcome.message(&key));
    println!("{}", line(&row));
    Ok(())
}

/// Runs `settings unset`.
fn unset(
    loaded: &Loaded,
    home: &Path,
    args: &SettingsUnsetArgs,
    json: bool,
) -> Result<(), CliError> {
    refuse_a_task_write(args.task.as_deref())?;
    let key = Key::parse(&args.key).map_err(invalid)?;
    let scope = scope(loaded, args.for_repo)?;
    let written = loaded
        .write(home, &scope, &key, None, None)
        .map_err(invalid)?;
    let row = row_of(&written.resolved, &key);
    if json {
        print_json(&serde_json::json!({
            "outcome": written.outcome.name(),
            "key": row.key,
            "value": value_json(&row.value),
            "source": row.source.map(Source::name),
        }))?;
        return Ok(());
    }
    println!("{}", written.outcome.message(&key));
    println!("{}", line(&row));
    Ok(())
}

/// Refuse a write aimed at a task, and say where a task's settings come from.
fn refuse_a_task_write(task: Option<&str>) -> Result<(), CliError> {
    match task {
        None => Ok(()),
        Some(task) => Err(invalid(anyhow!(
            "a task's own settings cannot be written from here (`--task {task}`): the engine records \
             what each attempt ran, and a task's overrides belong to its own record. Write to your \
             defaults, or to this repository's entry with `--for-repo`"
        ))),
    }
}

/// The warning a read prints when the roles could not be checked at all.
///
/// Nothing is said when there is nothing to check: a person who has picked no
/// program yet should not be warned about a table they do not need.
fn skipped_note(checked: &Checked, resolved: &Resolved) -> Option<String> {
    let reason = checked.skipped()?;
    resolved
        .names_a_pick()
        .then(|| format!("the roles were not checked: {reason}"))
}

/// The refusals these settings have, or the fact that they could not be
/// checked.
fn check(resolved: &Resolved, checker: &Checker) -> Checked {
    checker.check(resolved, Review::default())
}

/// The checker with the catalogs of the programs some role names an effort for.
///
/// Only those are read: a program no role gives an effort is never asked, so a
/// read of the settings stays a read. A listing that cannot be read is skipped —
/// the effort check then says nothing rather than guessing.
fn with_catalogs(checker: Checker, resolved: &Resolved, root: Option<&Path>) -> Checker {
    let roles = resolved.roles();
    let programs: Vec<&str> = roles
        .iter()
        .filter(|role| role.effort.is_some())
        .filter_map(|role| role.agent.as_ref().map(|agent| agent.value.as_str()))
        .collect();
    if programs.is_empty() {
        return checker;
    }
    let Ok(data) = crate::ctx::resolve_data_dir(root) else {
        return checker;
    };
    let Some(specs) = checker.specs() else {
        return checker;
    };
    let path_env = std::env::var("PATH").unwrap_or_default();
    let models_dir = data.join("models");
    let mut catalogs = BTreeMap::new();
    for program in programs {
        if catalogs.contains_key(program) {
            continue;
        }
        let Some(spec) = specs.iter().find(|spec| spec.id == program) else {
            continue;
        };
        if let Some(catalog) =
            shprd_agents::adapter::catalog(spec, &models_dir, &path_env, CATALOG_TIMEOUT)
        {
            catalogs.insert(program.to_owned(), catalog);
        }
    }
    checker.with_catalogs(catalogs)
}

/// One key's line as the verb prints it.
fn line(row: &settings::Row) -> String {
    match (&row.value, row.source) {
        (Some(value), Some(source)) => {
            format!("{} = {}  ({})", row.key, value.display(), source.label())
        }
        _ => format!("{} = (unset)", row.key),
    }
}

/// One key's JSON line.
fn row_json(row: &settings::Row) -> serde_json::Value {
    serde_json::json!({
        "key": row.key,
        "value": value_json(&row.value),
        "source": row.source.map(Source::name),
    })
}

/// One value as JSON: `null` when nothing sets it.
fn value_json(value: &Option<Value>) -> serde_json::Value {
    value
        .as_ref()
        .map_or(serde_json::Value::Null, Value::to_json)
}

/// The row one key resolves to, set or not.
fn row_of(resolved: &Resolved, key: &Key) -> settings::Row {
    resolved
        .rows()
        .into_iter()
        .find(|row| row.key == key.name())
        .unwrap_or(settings::Row {
            key: key.name(),
            value: resolved.get(key).map(|sourced| sourced.value.clone()),
            source: resolved.source_of(key),
        })
}

/// Print one JSON document, then a newline.
fn print_json(document: &serde_json::Value) -> Result<(), CliError> {
    let text = serde_json::to_string_pretty(document)
        .map_err(|err| CliError::other(anyhow!(err).context("printing JSON")))?;
    println!("{text}");
    Ok(())
}

/// Turn a settings refusal into the invalid exit the other verbs use.
///
/// The message is flattened rather than chained: a [`SettingsError`] already
/// names the file, the key and the reason in its own words, and printing the
/// same reason again under "caused by" only makes the line longer.
fn invalid(error: impl std::fmt::Display) -> CliError {
    CliError::Invalid(anyhow!("{error}"))
}
