//! Harness table: the typed schema for the per-CLI TOML specs in `data/harness/`.
//!
//! Each spec records what shprd needs to detect, launch and watch one agent CLI.
//! Locations and formats live in data so supporting a new CLI version is a
//! table change rather than a code change (see `docs/agents.md`).

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// One supported agent CLI, as loaded from a TOML file in `data/harness/`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessSpec {
    /// Short CLI id, e.g. `claude`.
    pub id: String,
    /// Human-readable name, e.g. `Claude Code`.
    pub name: String,
    /// The vendor that ships the CLI.
    pub vendor: Vendor,
    /// The CLI version the facts in this spec were checked against.
    pub verified_version: String,
    /// How to find the CLI and tell whether it is signed in.
    pub detect: DetectSpec,
    /// How to start the CLI and inject model, effort and prompts.
    pub launch: LaunchSpec,
    /// How to list the models the CLI offers.
    pub models: ModelsSpec,
    /// Where the CLI records its conversations and provider errors.
    pub records: RecordsSpec,
    /// Where the CLI reports quota, if anywhere.
    pub quota: QuotaSpec,
    /// Skill directories the CLI reads.
    pub skills: SkillsSpec,
    /// Case-insensitive screen substrings that mean rate limited or out of quota.
    pub dry_patterns: Vec<String>,
}

/// The vendor that ships an agent CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Vendor {
    /// Anthropic (Claude Code).
    Anthropic,
    /// OpenAI (Codex).
    Openai,
    /// OpenCode.
    Opencode,
    /// Kilo, an OpenCode fork.
    Kilo,
    /// Pi.
    Pi,
}

/// CLI detection and sign-in probes.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DetectSpec {
    /// The binary to look for on `PATH`.
    pub binary: String,
    /// Arguments that print the CLI version.
    pub version_args: Vec<String>,
    /// The command a user runs to sign in.
    pub signin_hint: String,
}

/// Flags for starting the CLI and injecting session settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchSpec {
    /// The base interactive command.
    pub interactive: Vec<String>,
    /// The flag that selects a model.
    pub model_flag: String,
    /// The flag that selects an effort or thinking level, if the CLI has one.
    pub effort_flag: Option<String>,
    /// Extra arguments that suppress approval prompts for unattended work.
    pub auto_approve: Option<Vec<String>>,
    /// How to inject a system prompt, e.g. a flag name.
    pub system_prompt: Option<String>,
    /// The flag or subcommand that resumes a session.
    pub resume_flag: Option<String>,
    /// Whether these facts are stated in `docs/agents.md`.
    pub verified: bool,
}

/// How to list the CLI's models.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelsSpec {
    /// The command that lists available models, if the CLI has one.
    pub list_command: Option<Vec<String>>,
}

/// Where the CLI writes its conversation record and provider errors.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordsSpec {
    /// The record's on-disk format.
    pub kind: RecordKind,
    /// Path pattern; `~` and the `<cwd-slug>` / `<session>` placeholders are allowed.
    pub location: String,
    /// Where provider errors appear inside the record.
    pub errors: String,
    /// Whether these facts are stated in `docs/agents.md`.
    pub verified: bool,
}

/// The on-disk format of a CLI's conversation record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RecordKind {
    /// Newline-delimited JSON.
    Jsonl,
    /// A SQLite database.
    Sqlite,
}

/// Where the CLI exposes quota information.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuotaSpec {
    /// The quota source for this CLI.
    pub source: QuotaSource,
    /// Where live retry status appears, if the CLI reports it.
    pub retry_signal: Option<String>,
}

/// The quota source for an agent CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaSource {
    /// Anthropic's OAuth usage endpoint.
    AnthropicOauth,
    /// Codex's ChatGPT usage endpoint.
    CodexWham,
    /// Kilo's account balance from `kilo profile`.
    KiloProfile,
    /// The CLI cannot report quota up front.
    None,
}

/// Skill directories the CLI reads.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillsSpec {
    /// Paths the CLI loads skills from.
    pub dirs: Vec<String>,
}

/// An error while loading harness specs.
#[derive(Debug, thiserror::Error)]
pub enum SpecError {
    /// A harness directory could not be read.
    #[error("failed to read harness directory {path}: {source}")]
    ReadDir {
        /// The directory that failed.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// A harness spec file could not be read.
    #[error("failed to read harness spec {path}: {source}")]
    ReadFile {
        /// The file that failed.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// A harness spec file is not valid TOML or does not match the schema.
    #[error("failed to parse harness spec {path}: {source}")]
    Parse {
        /// The file that failed.
        path: PathBuf,
        /// The underlying TOML error.
        #[source]
        source: toml::de::Error,
    },
}

/// Load every `*.toml` harness spec in `path`, sorted by file name.
pub fn load_dir(path: &Path) -> Result<Vec<HarnessSpec>, SpecError> {
    let entries = fs::read_dir(path).map_err(|source| SpecError::ReadDir {
        path: path.to_owned(),
        source,
    })?;

    let mut files = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| SpecError::ReadDir {
            path: path.to_owned(),
            source,
        })?;
        let file = entry.path();
        if file.extension().and_then(OsStr::to_str) == Some("toml") {
            files.push(file);
        }
    }
    files.sort();

    files.iter().map(|file| load_file(file)).collect()
}

fn load_file(path: &Path) -> Result<HarnessSpec, SpecError> {
    let source = fs::read_to_string(path).map_err(|source| SpecError::ReadFile {
        path: path.to_owned(),
        source,
    })?;
    toml::from_str(&source).map_err(|source| SpecError::Parse {
        path: path.to_owned(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::{HarnessSpec, load_dir};
    use std::path::Path;

    fn harness_dir() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/harness")
    }

    const MINIMAL_SPEC: &str = r#"
id = "claude"
name = "Claude Code"
vendor = "anthropic"
verified_version = "2.1.311"
dry_patterns = ["rate limit"]

[detect]
binary = "claude"
version_args = ["--version"]
signin_hint = "claude auth login"

[launch]
interactive = ["claude"]
model_flag = "--model"
verified = false

[models]

[records]
kind = "jsonl"
location = "~/.claude/projects/<cwd-slug>/<session>.jsonl"
errors = "isApiErrorMessage"
verified = true

[quota]
source = "anthropic_oauth"

[skills]
dirs = ["~/.claude/skills"]
"#;

    #[test]
    fn loads_five_specs_with_unique_ids() {
        let specs = match load_dir(&harness_dir()) {
            Ok(specs) => specs,
            Err(error) => panic!("harness data should load: {error}"),
        };
        let mut ids: Vec<&str> = specs.iter().map(|spec| spec.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(specs.len(), 5);
        assert_eq!(ids, ["claude", "codex", "kilo", "opencode", "pi"]);
    }

    #[test]
    fn rejects_unknown_top_level_field() {
        let fixture =
            MINIMAL_SPEC.replacen("id = \"claude\"", "unexpected = true\nid = \"claude\"", 1);
        let error = match toml::from_str::<HarnessSpec>(&fixture) {
            Ok(_) => panic!("unknown field should be rejected"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn rejects_unknown_nested_field() {
        let fixture = MINIMAL_SPEC.replacen(
            "interactive = [\"claude\"]",
            "interactive = [\"claude\"]\nmystery = true",
            1,
        );
        let error = match toml::from_str::<HarnessSpec>(&fixture) {
            Ok(_) => panic!("unknown field should be rejected"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("unknown field"));
    }
}
