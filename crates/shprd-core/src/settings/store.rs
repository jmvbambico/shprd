//! Reading, editing and writing the settings file.
//!
//! A write edits the document rather than rewriting it, so comments and keys the
//! verb does not own survive, and it validates the edited text before the file is
//! touched: a change that would leave the file unreadable, or that names a key
//! the file may not hold, writes nothing at all. The text goes to a fresh file
//! beside the target and is renamed over it, so the settings file is never seen
//! half-written.
//!
//! Only the person's own file is written. A repository's committed
//! `.shprd/settings.toml` is read and never edited from here: changing it is a
//! commit in that repository, which is what keeps a repo-committed file a
//! deliberate act rather than a side effect.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use toml_edit::{Array, DocumentMut, Item, Table, Value as EditValue};

use super::{File, Key, Scope, SettingsError, Value};

/// Apply one change to the settings file at `path` and write it back.
///
/// [`None`] removes the key. The file is created when it does not exist yet, and
/// stamped with `version = 1`.
pub(super) fn edit(
    path: &Path,
    scope: &Scope,
    key: &Key,
    change: Option<&Value>,
) -> Result<(), SettingsError> {
    let existing = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(source) => {
            return Err(SettingsError::Read {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let mut document = match existing.parse::<DocumentMut>() {
        Ok(document) => document,
        Err(source) => {
            return Err(SettingsError::InFile {
                path: path.to_path_buf(),
                source: Box::new(SettingsError::Document {
                    source: Box::new(source),
                }),
            });
        }
    };

    let segments = segments_of(scope, key);
    let (last, parents) = match segments.split_last() {
        Some(split) => split,
        None => {
            return Err(SettingsError::BadValue {
                key: key.name(),
                reason: "has no key to write".to_owned(),
            });
        }
    };
    let mut table = document.as_table_mut();
    for name in parents {
        table = child(table, name)?;
    }
    match change {
        Some(value) => {
            table[last.as_str()] = Item::Value(edit_value(value));
        }
        None => {
            table.remove(last.as_str());
        }
    }
    prune(document.as_table_mut());
    document["version"] = toml_edit::value(i64::from(super::VERSION));

    let text = document.to_string();
    // The edited text has to read back as a settings file before it is written:
    // a write must never be the reason a file stops loading.
    File::parse(&text).map_err(|source| SettingsError::InFile {
        path: path.to_path_buf(),
        source: Box::new(source),
    })?;
    write_atomic(path, &text)
}

/// The document path of a key: `[repos.<name>].` in front of the key's segments
/// when the scope is one repository's entry.
fn segments_of(scope: &Scope, key: &Key) -> Vec<String> {
    let mut segments = Vec::new();
    if let Scope::Repo(name) = scope {
        segments.push("repos".to_owned());
        segments.push(name.clone());
    }
    segments.extend(key.name().split('.').map(str::to_owned));
    segments
}

/// The table under `name`, created when it is missing.
///
/// A table this edit creates is implicit, so writing `roles.docs.agent` leaves
/// `[roles.docs]` behind rather than an empty `[roles]` header above it.
fn child<'a>(table: &'a mut Table, name: &str) -> Result<&'a mut Table, SettingsError> {
    table
        .entry(name)
        .or_insert_with(|| {
            let mut created = Table::new();
            created.set_implicit(true);
            Item::Table(created)
        })
        .as_table_mut()
        .ok_or_else(|| SettingsError::BadValue {
            key: name.to_owned(),
            reason: "is not a table, so it cannot be edited".to_owned(),
        })
}

/// Remove the tables the edit left empty, so an unset key leaves no trace.
fn prune(table: &mut Table) {
    let names: Vec<String> = table.iter().map(|(name, _)| name.to_owned()).collect();
    for name in names {
        let emptied = match table.get_mut(&name).and_then(Item::as_table_mut) {
            Some(child) => {
                prune(child);
                child.is_empty()
            }
            None => false,
        };
        if emptied {
            table.remove(&name);
        }
    }
}

/// One settings value as a TOML value.
fn edit_value(value: &Value) -> EditValue {
    match value {
        Value::Text(text) => EditValue::from(text.as_str()),
        Value::Number(number) => EditValue::from(*number),
        Value::Switch(on) => EditValue::from(*on),
        Value::List(items) => {
            let mut array = Array::new();
            for item in items {
                array.push(item.as_str());
            }
            EditValue::Array(array)
        }
    }
}

/// Write `text` to `path` through a fresh file beside it, then rename it in.
fn write_atomic(path: &Path, text: &str) -> Result<(), SettingsError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|source| SettingsError::Write {
        path: parent.to_path_buf(),
        source,
    })?;
    let temp: PathBuf = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "settings.toml".to_owned()),
        std::process::id()
    ));
    let written = fs::File::create(&temp).and_then(|mut file| {
        file.write_all(text.as_bytes())?;
        file.sync_all()
    });
    if let Err(source) = written {
        let _ = fs::remove_file(&temp);
        return Err(SettingsError::Write { path: temp, source });
    }
    if let Err(source) = fs::rename(&temp, path) {
        let _ = fs::remove_file(&temp);
        return Err(SettingsError::Write {
            path: path.to_path_buf(),
            source,
        });
    }
    Ok(())
}
