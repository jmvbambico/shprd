//! Herdr keybind block writer.
//!
//! Herdr's config is a TOML file a human also edits, so shprd owns only one
//! marked stretch of it: everything between [`BEGIN`] and [`END`] is generated
//! and everything outside is preserved byte for byte.

use std::fs;
use std::io::Write;
use std::ops::Range;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// The line that opens the block shprd owns.
pub const BEGIN: &str = "# >>> shprd (managed) >>>";
/// The line that closes the block shprd owns.
pub const END: &str = "# <<< shprd (managed) <<<";

/// How herdr runs a bound command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A shell command, run in the background.
    Shell,
    /// A command that opens a pane.
    Pane,
    /// A command that opens a popup.
    Popup,
}

impl Kind {
    /// The `type` herdr spells this kind as.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Shell => "shell",
            Self::Pane => "pane",
            Self::Popup => "popup",
        }
    }
}

/// One key herdr binds to a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    /// The key chord, in herdr spelling (`prefix+alt+u`).
    pub key: String,
    /// How herdr runs the command.
    pub kind: Kind,
    /// The command line herdr runs.
    pub command: String,
    /// The one-line label shown next to the chord.
    pub description: String,
}

/// The bindings shprd installs, in the order they render.
pub fn default_bindings() -> Vec<Binding> {
    vec![
        binding("prefix+alt+u", Kind::Shell, "shprd up", "shprd: up"),
        binding(
            "prefix+alt+c",
            Kind::Shell,
            "shprd focus chat",
            "shprd: chat",
        ),
        binding(
            "prefix+alt+o",
            Kind::Shell,
            "shprd focus orchestrator",
            "shprd: orchestrator",
        ),
        binding(
            "prefix+alt+n",
            Kind::Shell,
            "shprd focus next",
            "shprd: what needs me",
        ),
        binding("prefix+alt+r", Kind::Pane, "shprd remote", "shprd: remote"),
        binding("prefix+alt+s", Kind::Popup, "shprd status", "shprd: status"),
        binding(
            "prefix+alt+d",
            Kind::Popup,
            "shprd doctor",
            "shprd: doctor",
        ),
    ]
}

/// One binding, built from its four parts.
fn binding(key: &str, kind: Kind, command: &str, description: &str) -> Binding {
    Binding {
        key: key.to_owned(),
        kind,
        command: command.to_owned(),
        description: description.to_owned(),
    }
}

/// Renders the managed block, ending with a newline.
pub fn render(bindings: &[Binding]) -> String {
    let mut text = String::new();
    text.push_str(BEGIN);
    text.push('\n');
    for binding in bindings {
        text.push_str("\n[[keys.command]]\n");
        field(&mut text, "key", &binding.key);
        field(&mut text, "type", binding.kind.as_str());
        field(&mut text, "command", &binding.command);
        field(&mut text, "description", &binding.description);
    }
    text.push('\n');
    text.push_str(END);
    text.push('\n');
    text
}

/// Appends one `name = "value"` line, quoted with [`quote`].
fn field(text: &mut String, name: &str, value: &str) {
    text.push_str(name);
    text.push_str(" = ");
    text.push_str(&quote(value));
    text.push('\n');
}

/// Quotes `value` as a TOML basic string.
fn quote(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for character in value.chars() {
        match character {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            other => quoted.push(other),
        }
    }
    quoted.push('"');
    quoted
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A user's own config: comments, a table, and one unrelated key.
    const USER_CONFIG: &str = "# my herdr\n[terminal]\nshell = \"zsh\"\n\n\
        # mine\n[[keys.command]]\nkey = \"prefix+x\"\ntype = \"shell\"\n\
        command = \"mine\"\ndescription = \"mine\"\n";

    #[test]
    fn install_into_an_empty_file_installs_only_the_block() {
        let installed = install("", &default_bindings()).unwrap();
        assert_eq!(installed, render(&default_bindings()));
    }

    #[test]
    fn install_keeps_everything_outside_the_block_byte_for_byte() {
        let bindings = default_bindings();
        let installed = install(USER_CONFIG, &bindings).unwrap();
        assert_eq!(installed, format!("{USER_CONFIG}\n{}", render(&bindings)));
    }

    #[test]
    fn install_twice_changes_nothing_the_second_time() {
        let bindings = default_bindings();
        let once = install(USER_CONFIG, &bindings).unwrap();
        let twice = install(&once, &bindings).unwrap();
        assert_eq!(once, twice);
    }

    #[test]
    fn uninstall_restores_the_original_exactly() {
        let bindings = default_bindings();
        let installed = install(USER_CONFIG, &bindings).unwrap();
        assert_eq!(uninstall(&installed).unwrap(), USER_CONFIG);
    }

    #[test]
    fn uninstall_of_a_file_without_a_block_changes_nothing() {
        assert_eq!(uninstall(USER_CONFIG).unwrap(), USER_CONFIG);
    }

    #[test]
    fn install_refuses_a_key_the_user_already_bound() {
        let warned = "[[keys.command]]\nkey = \"prefix+alt+u\"\ntype = \"shell\"\n\
            command = \"vim\"\ndescription = \"mine\"\n";
        match install(warned, &default_bindings()) {
            Err(KeysError::Conflict { key, existing }) => {
                assert_eq!(key, "prefix+alt+u");
                assert_eq!(existing, "vim");
            }
            other => panic!("expected a conflict, got {other:?}"),
        }
    }

    #[test]
    fn install_refuses_missing_and_duplicated_markers() {
        let bindings = default_bindings();
        for broken in [
            format!("x = 1\n{BEGIN}\n"),
            format!("x = 1\n{END}\n"),
            format!("{END}\nx = 1\n{BEGIN}\n"),
            format!("{BEGIN}\n{END}\n{BEGIN}\n{END}\n"),
        ] {
            assert!(
                matches!(install(&broken, &bindings), Err(KeysError::Unbalanced)),
                "expected unbalanced for {broken:?}"
            );
        }
    }

    #[test]
    fn install_refuses_text_outside_the_block_that_is_not_toml() {
        let broken = "this is not toml = = =\n";
        assert!(matches!(
            install(broken, &default_bindings()),
            Err(KeysError::Parse(_))
        ));
    }

    #[test]
    fn render_parses_as_toml_with_one_table_per_binding() {
        let text = render(&default_bindings());
        let parsed: toml::Value = toml::from_str(&text).unwrap();
        let tables = parsed
            .get("keys")
            .and_then(|keys| keys.get("command"))
            .and_then(toml::Value::as_array)
            .unwrap();
        assert_eq!(tables.len(), 7);
        assert_eq!(
            tables[0].get("key").and_then(toml::Value::as_str),
            Some("prefix+alt+u")
        );
        assert_eq!(
            tables[4].get("type").and_then(toml::Value::as_str),
            Some("pane")
        );
    }

    #[test]
    fn render_escapes_quotes_and_backslashes() {
        let quoted = quote("a\"b\\c");
        assert_eq!(quoted, "\"a\\\"b\\\\c\"");
    }
}

/// What can go wrong while installing or removing the managed block.
#[derive(Debug, thiserror::Error)]
pub enum KeysError {
    /// The managed block is missing one of its two markers, has one twice, or
    /// has them in the wrong order.
    #[error("the shprd block markers are unbalanced, so shprd will not touch this file")]
    Unbalanced,
    /// A binding outside the block already owns one of shprd's keys.
    #[error("key `{key}` is already bound outside the shprd block, to `{existing}`")]
    Conflict {
        /// The key both sides want.
        key: String,
        /// The command the user's own config binds to it.
        existing: String,
    },
    /// The text outside the block is not valid TOML.
    #[error("the herdr config outside the shprd block is not valid TOML: {0}")]
    Parse(String),
    /// Reading or writing the config file failed.
    #[error("herdr config i/o failed: {0}")]
    Io(#[from] std::io::Error),
}

/// The byte span of the managed block: the [`BEGIN`] line through the end of
/// the [`END`] line.
fn block_span(text: &str) -> Result<Option<Range<usize>>, KeysError> {
    let mut begin = None;
    let mut end = None;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let marker = line.trim();
        if marker == BEGIN && begin.replace(offset).is_some() {
            return Err(KeysError::Unbalanced);
        }
        if marker == END && end.replace(offset + line.len()).is_some() {
            return Err(KeysError::Unbalanced);
        }
        offset += line.len();
    }
    match (begin, end) {
        (None, None) => Ok(None),
        (Some(start), Some(stop)) if start < stop => Ok(Some(start..stop)),
        _ => Err(KeysError::Unbalanced),
    }
}

/// Installs (or refreshes) the managed block in `text`.
///
/// An existing block is replaced in place, so everything outside it stays byte
/// for byte; a file without one gains `text`'s own block at the end, after a
/// blank line. Nothing is written to disk here: the caller decides.
///
/// A file whose last line had no newline gets one, because the block must start
/// on a line of its own.
pub fn install(text: &str, bindings: &[Binding]) -> Result<String, KeysError> {
    let block = render(bindings);
    let installed = match block_span(text)? {
        Some(span) => {
            let mut out = String::with_capacity(text.len() + block.len());
            out.push_str(&text[..span.start]);
            out.push_str(&block);
            out.push_str(&text[span.end..]);
            out
        }
        None => {
            let mut out = text.to_owned();
            if !out.is_empty() {
                if !out.ends_with('\n') {
                    out.push('\n');
                }
                out.push('\n');
            }
            out.push_str(&block);
            out
        }
    };
    ensure_compatible(&installed, bindings)?;
    Ok(installed)
}

/// Removes the managed block, and the blank line installing it added.
///
/// A file without a block comes back unchanged.
pub fn uninstall(text: &str) -> Result<String, KeysError> {
    let Some(span) = block_span(text)? else {
        return Ok(text.to_owned());
    };
    let mut start = span.start;
    if text[..start].ends_with("\n\n") {
        start -= 1;
    }
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..start]);
    out.push_str(&text[span.end..]);
    Ok(out)
}

/// Refuses text whose part outside the block is not TOML herdr could load, or
/// which binds one of shprd's keys outside the block.
fn ensure_compatible(text: &str, bindings: &[Binding]) -> Result<(), KeysError> {
    let outside = match block_span(text)? {
        Some(span) => {
            let mut outside = String::with_capacity(text.len());
            outside.push_str(&text[..span.start]);
            outside.push_str(&text[span.end..]);
            outside
        }
        None => text.to_owned(),
    };
    let parsed: toml::Value =
        toml::from_str(&outside).map_err(|err| KeysError::Parse(err.to_string()))?;
    let entries = parsed
        .get("keys")
        .and_then(|keys| keys.get("command"))
        .and_then(toml::Value::as_array);
    for entry in entries.into_iter().flatten() {
        let Some(key) = entry.get("key").and_then(toml::Value::as_str) else {
            continue;
        };
        if !bindings.iter().any(|binding| binding.key == key) {
            continue;
        }
        let existing = entry
            .get("command")
            .and_then(toml::Value::as_str)
            .unwrap_or_default();
        return Err(KeysError::Conflict {
            key: key.to_owned(),
            existing: existing.to_owned(),
        });
    }
    Ok(())
}

/// The process-wide counter that keeps temp names unique within one process.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Writes `text` to `path` atomically.
///
/// A uniquely named temp file in the same directory is written, flushed and
/// synced, then renamed over `path`, so a concurrent reader sees either the old
/// file or the new one and never a half-written config. The temp file is
/// removed when the write fails, so a crash leaves no debris behind.
pub fn write_atomic(path: &Path, text: &str) -> Result<(), KeysError> {
    let directory = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "config.toml".to_owned());
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp = directory.join(format!(
        ".{name}.shprd-{}-{counter}.tmp",
        std::process::id()
    ));
    if let Err(error) = write_synced(&temp, text) {
        let _ = fs::remove_file(&temp);
        return Err(KeysError::Io(error));
    }
    if let Err(error) = fs::rename(&temp, path) {
        let _ = fs::remove_file(&temp);
        return Err(KeysError::Io(error));
    }
    Ok(())
}

/// Writes `text` to `path` and flushes it all the way to the device.
fn write_synced(path: &Path, text: &str) -> std::io::Result<()> {
    let mut file = fs::File::create(path)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()
}

#[cfg(test)]
mod write_tests {
    use super::*;

    #[test]
    fn write_atomic_replaces_the_file_and_leaves_no_temp_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "old = true\n").unwrap();
        write_atomic(&path, "new = true\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "new = true\n");
        let left: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| {
                entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(left, vec!["config.toml".to_owned()]);
    }

    #[test]
    fn write_atomic_creates_the_file_when_it_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fresh.toml");
        write_atomic(&path, "a = 1\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "a = 1\n");
    }
}
