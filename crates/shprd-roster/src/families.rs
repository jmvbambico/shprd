//! Model families: who made the model a harness runs, not who ships the CLI.
//!
//! The harness table says which vendor ships a CLI. That is not the vendor of
//! the model: pi, opencode and kilo run other vendors' models. The table in
//! `data/models/families.toml` maps a model id to the family that produced it,
//! so cross-vendor review can compare model families instead of CLI names.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

/// The only `schema` value this build understands.
pub const SCHEMA: u32 = 1;

/// One model family: the vendor name and the prefixes that name its models.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Family {
    /// The vendor name, e.g. `anthropic`, as [`Families::family_of`] returns it.
    pub name: String,
    /// Lowercase prefixes a model id segment starts with, e.g. `claude`.
    pub prefixes: Vec<String>,
}

/// Every model family, as loaded from `data/models/families.toml`.
///
/// The [`Default`] value knows no family at all: a caller that cannot read the
/// table treats every model as unknown rather than as independent.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Families {
    /// The table format version, [`SCHEMA`] when loaded from a file.
    #[serde(default)]
    pub schema: u32,
    /// One entry per family, in file order.
    #[serde(default, rename = "family")]
    pub families: Vec<Family>,
}

/// Why a model family table could not be loaded.
#[derive(Debug, Error)]
pub enum FamilyError {
    /// The file could not be read.
    #[error("reading `{}`: {source}", path.display())]
    ReadFile {
        /// The file that was read.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// The file is not valid TOML for the schema below.
    #[error("parsing `{}`: {source}", path.display())]
    Parse {
        /// The file that was read.
        path: PathBuf,
        /// The underlying error.
        source: Box<toml::de::Error>,
    },
    /// The file declares a format this build does not read.
    #[error("`{}` has schema {found}, but this build reads schema {expected}", path.display())]
    Schema {
        /// The file that was read.
        path: PathBuf,
        /// The schema it declares.
        found: u32,
        /// The schema this build reads.
        expected: u32,
    },
    /// The file parses but breaks a rule the type system cannot express.
    #[error("`{}`: {reason}", path.display())]
    Invalid {
        /// The file that was read.
        path: PathBuf,
        /// What is wrong with it.
        reason: String,
    },
}

impl Families {
    /// Loads and validates the table at `path`.
    ///
    /// A table is valid when it declares [`SCHEMA`], names every family with a
    /// non-empty lowercase name, gives each family at least one lowercase
    /// prefix, and repeats neither a family name nor a prefix, so a model id
    /// can never match two families.
    pub fn load(path: &Path) -> Result<Families, FamilyError> {
        let source = fs::read_to_string(path).map_err(|source| FamilyError::ReadFile {
            path: path.to_owned(),
            source,
        })?;
        let table: Families = toml::from_str(&source).map_err(|source| FamilyError::Parse {
            path: path.to_owned(),
            source: Box::new(source),
        })?;
        if table.schema != SCHEMA {
            return Err(FamilyError::Schema {
                path: path.to_owned(),
                found: table.schema,
                expected: SCHEMA,
            });
        }
        table.validate().map_err(|reason| FamilyError::Invalid {
            path: path.to_owned(),
            reason,
        })?;
        Ok(table)
    }

    /// Checks the rules the type system cannot express; see [`Self::load`].
    fn validate(&self) -> Result<(), String> {
        if self.families.is_empty() {
            return Err("no `[[family]]` entry: the table would know no model".to_owned());
        }
        let mut names: Vec<&str> = Vec::new();
        let mut prefixes: Vec<&str> = Vec::new();
        for family in &self.families {
            if family.name.trim().is_empty() {
                return Err("a family has an empty name".to_owned());
            }
            if !is_word(&family.name) {
                return Err(format!(
                    "the family name `{}` must be lowercase letters, digits or `-`",
                    family.name
                ));
            }
            if names.contains(&family.name.as_str()) {
                return Err(format!("the family `{}` is declared twice", family.name));
            }
            if family.prefixes.is_empty() {
                return Err(format!("the family `{}` has no prefixes", family.name));
            }
            for prefix in &family.prefixes {
                if !is_word(prefix) {
                    return Err(format!(
                        "the prefix `{prefix}` must be lowercase letters, digits or `-`"
                    ));
                }
                if prefixes.contains(&prefix.as_str()) {
                    return Err(format!("the prefix `{prefix}` is declared twice"));
                }
                prefixes.push(prefix);
            }
            names.push(&family.name);
        }
        Ok(())
    }
}

/// Whether `text` is a lowercase word: letters, digits or `-`, not empty.
fn is_word(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

impl Families {
    /// The family of `model`, which is a model id with no program in front.
    ///
    /// The id is split on `/`: its last segment is the model's own name, and its
    /// first segment is often a provider, so both are tried, last first. A match
    /// is a case-insensitive prefix, which is why `claude-opus-5-5` reads as
    /// anthropic and `gpt-5-codex` as openai. [`None`] means the table knows the
    /// model no better than chance.
    pub fn family_of(&self, model: &str) -> Option<&str> {
        let segments: Vec<&str> = model
            .split('/')
            .map(str::trim)
            .filter(|segment| !segment.is_empty())
            .collect();
        let last = segments.last()?;
        let first = segments.first()?;
        self.match_segment(last)
            .or_else(|| self.match_segment(first))
    }

    /// The first family one of whose prefixes `segment` starts with.
    fn match_segment(&self, segment: &str) -> Option<&str> {
        let lowered = segment.to_ascii_lowercase();
        self.families
            .iter()
            .find(|family| {
                family
                    .prefixes
                    .iter()
                    .any(|prefix| lowered.starts_with(prefix.as_str()))
            })
            .map(|family| family.name.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::{Families, FamilyError, SCHEMA, is_word};
    use std::path::Path;

    /// The shipped table, which names every family.
    fn shipped() -> Families {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../data/models")
            .join("families.toml");
        match Families::load(&path) {
            Ok(table) => table,
            Err(error) => panic!("the shipped family table must load: {error}"),
        }
    }

    #[test]
    fn the_shipped_table_loads_and_declares_the_schema() {
        let table = shipped();
        assert_eq!(table.schema, SCHEMA);
        // Every family the task named is present, named as the cross-vendor
        // rule reports it.
        for name in [
            "anthropic",
            "openai",
            "deepseek",
            "google",
            "zhipu",
            "moonshot",
            "alibaba",
            "xai",
            "minimax",
            "mistral",
            "meta",
        ] {
            assert!(
                table.families.iter().any(|family| family.name == name),
                "the shipped table has no `{name}`"
            );
        }
    }

    #[test]
    fn a_model_reads_as_the_family_that_made_it() {
        let table = shipped();
        let cases = [
            ("claude-opus-5-5", Some("anthropic")),
            ("Claude-Sonnet-4", Some("anthropic")),
            ("openrouter/anthropic/claude-opus-5-5", Some("anthropic")),
            ("gpt-5-codex", Some("openai")),
            ("o3-mini", Some("openai")),
            ("deepseek-v4.1-flash", Some("deepseek")),
            ("deepseek/deepseek-v4.1-flash", Some("deepseek")),
            ("gemini-3-pro", Some("google")),
            ("z-ai/glm-4.6", Some("zhipu")),
            ("glm-4.6", Some("zhipu")),
            ("kimi-k2", Some("moonshot")),
            ("qwen3-coder", Some("alibaba")),
            ("grok-4", Some("xai")),
            ("MiniMax-M2", Some("minimax")),
            ("codestral-2501", Some("mistral")),
            ("meta/llama-4-70b", Some("meta")),
            // A router names no family, and neither does a free tier.
            ("kilo-auto/free", None),
            ("free", None),
            ("", None),
        ];
        for (model, expected) in cases {
            assert_eq!(table.family_of(model), expected, "for `{model}`");
        }
    }

    /// A temp file holding `text`, and the table it loads to.
    fn load_text(text: &str) -> Result<Families, FamilyError> {
        let dir = match tempfile::TempDir::new() {
            Ok(dir) => dir,
            Err(error) => panic!("{error}"),
        };
        let path = dir.path().join("families.toml");
        if let Err(error) = std::fs::write(&path, text) {
            panic!("{error}");
        }
        Families::load(&path)
    }

    #[test]
    fn a_table_with_an_unknown_schema_is_refused() {
        let text = r#"
schema = 2

[[family]]
name = "anthropic"
prefixes = ["claude"]
"#;
        assert!(matches!(
            load_text(text),
            Err(FamilyError::Schema { found: 2, .. })
        ));
    }

    #[test]
    fn a_table_that_would_match_one_model_twice_is_refused() {
        let repeated = r#"
schema = 1

[[family]]
name = "anthropic"
prefixes = ["claude"]

[[family]]
name = "openai"
prefixes = ["claude", "gpt"]
"#;
        match load_text(repeated) {
            Err(FamilyError::Invalid { reason, .. }) => {
                assert!(reason.contains("`claude` is declared twice"), "{reason}");
            }
            other => panic!("a repeated prefix must be refused, got {other:?}"),
        }
    }

    #[test]
    fn a_table_with_an_uppercase_prefix_or_no_prefix_is_refused() {
        let uppercase = r#"
schema = 1

[[family]]
name = "anthropic"
prefixes = ["Claude"]
"#;
        assert!(matches!(
            load_text(uppercase),
            Err(FamilyError::Invalid { .. })
        ));

        let empty = r#"
schema = 1

[[family]]
name = "anthropic"
prefixes = []
"#;
        assert!(matches!(load_text(empty), Err(FamilyError::Invalid { .. })));

        let no_entries = "schema = 1\n";
        assert!(matches!(
            load_text(no_entries),
            Err(FamilyError::Invalid { .. })
        ));
    }

    #[test]
    fn a_word_is_lowercase_letters_digits_and_dashes() {
        assert!(is_word("glm-4"));
        assert!(is_word("z-ai"));
        assert!(!is_word(""));
        assert!(!is_word("Claude"));
        assert!(!is_word("gpt 5"));
    }

    #[test]
    fn an_unreadable_file_names_itself() {
        let path = Path::new("/nonexistent/shprd/families.toml");
        match Families::load(path) {
            Err(FamilyError::ReadFile { path: named, .. }) => assert_eq!(named, path),
            other => panic!("a missing file must be a read error, got {other:?}"),
        }
    }
}
