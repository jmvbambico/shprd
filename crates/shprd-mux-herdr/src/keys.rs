//! Herdr keybind block writer.
//!
//! Herdr's config is a TOML file a human also edits, so shprd owns only one
//! marked stretch of it: everything between [`BEGIN`] and [`END`] is generated
//! and everything outside is preserved byte for byte.

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
