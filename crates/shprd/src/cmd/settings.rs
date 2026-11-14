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
//! have. A refusal exits 2 and writes nothing. Reading never refuses: a set of
//! settings that would not work is printed, with its refusals as warnings, because
//! the person has to see the broken line to fix it.
//!
//! `--task` resolves for one task of the run: the agent its own newest attempt
//! ran is the nearest layer, which is the only per-task fact the log holds. A
//! task's own settings cannot be written from here, and the refusal says so.

use std::path::Path;

use anyhow::anyhow;
use shprd_agents::roles::{self, Problems, Review, Table};
use shprd_core::event::Actor;
use shprd_core::settings::{self, Key, Layer, Loaded, Resolved, RoleField, Scope, Source, Value};

use crate::cli::{
    SettingsCommand, SettingsGetArgs, SettingsSetArgs, SettingsShowArgs, SettingsUnsetArgs,
};
use crate::ctx::{CliError, Ctx, resolve_home};

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
    let table = harness_table(root);
    match command {
        SettingsCommand::Show(args) => show(&loaded, &home, root, args, json, table.as_ref()),
        SettingsCommand::Get(args) => get(&loaded, &home, root, args, json, table.as_ref()),
        SettingsCommand::Set(args) => set(&loaded, &home, root, args, json, table.as_ref()),
        SettingsCommand::Unset(args) => unset(&loaded, &home, root, args, json, table.as_ref()),
    }
}

/// Read the settings files, turning a refusal into the verb's invalid exit.
fn load(home: &Path, root: Option<&Path>) -> Result<Loaded, CliError> {
    Loaded::read(home, root).map_err(invalid)
}

/// The harness table and the model families, when this machine has them.
///
/// A machine without a data directory can still read and write the workflow
/// keys; only the role checks need the table, and [`check`] says when it could
/// not run.
fn harness_table(root: Option<&Path>) -> Option<Table> {
    let dir = shprd_agents::harness::data_dir(root)?;
    roles::Table::load(&dir).ok()
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
    _home: &Path,
    root: Option<&Path>,
    args: &SettingsShowArgs,
    json: bool,
    table: Option<&Table>,
) -> Result<(), CliError> {
    let task = task_layer(root, args.task.as_deref())?;
    let resolved = loaded.resolve(task.as_ref());
    let problems = check(&resolved, table);
    if json {
        let values: Vec<serde_json::Value> = resolved
            .rows()
            .into_iter()
            .map(|row| row_json(&row))
            .collect();
        let document = serde_json::json!({
            "repo": loaded.repo,
            "values": values,
            "problems": problems.messages(),
        });
        print_json(&document)?;
        return Ok(());
    }
    for row in resolved.rows() {
        println!("{}", line(&row));
    }
    for problem in problems.messages() {
        eprintln!("warning: {problem}");
    }
    Ok(())
}

/// Runs `settings get`.
fn get(
    loaded: &Loaded,
    _home: &Path,
    root: Option<&Path>,
    args: &SettingsGetArgs,
    json: bool,
    table: Option<&Table>,
) -> Result<(), CliError> {
    let key = Key::parse(&args.key).map_err(invalid)?;
    let task = task_layer(root, args.task.as_deref())?;
    let resolved = loaded.resolve(task.as_ref());
    let row = row_of(&resolved, &key);
    if json {
        print_json(&row_json(&row))?;
        return Ok(());
    }
    println!("{}", line(&row));
    let problems = check(&resolved, table);
    for problem in problems.messages() {
        eprintln!("warning: {problem}");
    }
    Ok(())
}

/// Runs `settings set`.
fn set(
    loaded: &Loaded,
    home: &Path,
    _root: Option<&Path>,
    args: &SettingsSetArgs,
    json: bool,
    table: Option<&Table>,
) -> Result<(), CliError> {
    refuse_a_task_write(args.task.as_deref())?;
    let key = Key::parse(&args.key).map_err(invalid)?;
    let value = key.value_from_text(&args.value).map_err(invalid)?;
    let scope = scope(loaded, args.for_repo)?;
    // Check the settings the write would leave behind, before it leaves
    // anything: a refused change never reaches the disk.
    let candidate = loaded.preview(&scope, &key, Some(&value), None);
    let problems = check(&candidate, table);
    if !problems.is_empty() {
        return Err(invalid(anyhow!(
            "{} would be refused: {problems}",
            key.name()
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
    _root: Option<&Path>,
    args: &SettingsUnsetArgs,
    json: bool,
    _table: Option<&Table>,
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

/// The refusals the harness table has for these settings.
///
/// Empty when the settings check out, or when this machine has no harness table
/// to check them against: a missing data directory is `shprd doctor`'s to report,
/// not a reason to refuse every write.
fn check(resolved: &Resolved, table: Option<&Table>) -> Problems {
    let Some(table) = table else {
        return Problems::default();
    };
    match roles::check(resolved, Review::default(), table) {
        Ok(()) => Problems::default(),
        Err(problems) => problems,
    }
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
