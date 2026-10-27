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

impl HarnessSpec {
    /// Check the rules the type system cannot express: the record location
    /// pattern and the skill directory roots.
    ///
    /// A spec that passes this has a [`RecordsSpec::location`] that only uses
    /// the documented placeholders, `home_dirs` entries that are home-relative
    /// (each starting with `~/`) and `project_dirs` entries that are relative
    /// to the repository root.
    pub fn validate(&self) -> Result<(), SpecError> {
        validate_location(&self.records.location)?;

        for dir in &self.skills.home_dirs {
            match dir.strip_prefix("~/") {
                Some(rest) if !rest.is_empty() => {}
                _ => {
                    return Err(SpecError::InvalidSkillDir { dir: dir.clone() });
                }
            }
        }
        for dir in &self.skills.project_dirs {
            if dir.is_empty() || dir.starts_with('/') || dir.starts_with('~') {
                return Err(SpecError::InvalidSkillDir { dir: dir.clone() });
            }
        }
        Ok(())
    }
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

/// Argument templates for starting the CLI and injecting session settings.
///
/// Every template is a list of argv tokens: each element reaches the CLI as one
/// argument, so a value with spaces stays one argument without quoting. A token
/// may contain at most one placeholder from [`Values`], e.g.
/// `model_reasoning_effort={effort}` or `{prompt_file}`. Use [`render_args`] to
/// fill a template in.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchSpec {
    /// The base interactive command.
    pub interactive: Vec<String>,
    /// Arguments that select a model, using `{model}`.
    pub model_args: Vec<String>,
    /// Arguments that select an effort or thinking level, using `{effort}`.
    pub effort_args: Option<Vec<String>>,
    /// Extra arguments that suppress approval prompts for unattended work.
    pub auto_approve: Option<Vec<String>>,
    /// Arguments that inject a system prompt, using `{prompt}` or `{prompt_file}`.
    pub system_prompt_args: Option<Vec<String>>,
    /// Arguments that resume a session, using `{session}`.
    pub resume_args: Option<Vec<String>>,
    /// Whether these facts are stated in `docs/agents.md`.
    pub verified: bool,
}

/// The values substituted into [`LaunchSpec`] argument templates.
///
/// A [`None`] field means the caller has no value for that placeholder, so any
/// template that uses it fails to render rather than silently dropping an
/// argument.
#[derive(Debug, Clone, Default)]
pub struct Values {
    /// The model id to select.
    pub model: Option<String>,
    /// The effort or thinking level to select.
    pub effort: Option<String>,
    /// A system prompt passed inline.
    pub prompt: Option<String>,
    /// A path to a file holding the system prompt.
    pub prompt_file: Option<String>,
    /// The session id to resume.
    pub session: Option<String>,
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
    /// Where the record lives, as a path pattern.
    ///
    /// The allowed placeholders are `{cwd_slug}` (the working directory written
    /// as one safe path component), `{session}` (the CLI's session id),
    /// `{yyyy}`, `{mm}` and `{dd}` (the record's date). `~` is allowed only as
    /// the first component, where it means the user's home directory, and `*`
    /// stands for any run of characters within one path component. Every other
    /// `{...}` is rejected by [`validate_location`].
    pub location: String,
    /// Where provider errors appear inside the record.
    pub errors: String,
    /// Whether these facts are stated in `docs/agents.md`.
    pub verified: bool,
}

/// Placeholders allowed in a [`RecordsSpec::location`] pattern.
const LOCATION_PLACEHOLDERS: [&str; 5] = ["cwd_slug", "session", "yyyy", "mm", "dd"];

/// Check a record location pattern against the placeholder contract.
///
/// Allowed placeholders are `{cwd_slug}`, `{session}`, `{yyyy}`, `{mm}` and
/// `{dd}`; `*` may stand for any run of characters within a single path
/// component; and `~` may appear only as the first component, where it means the
/// user's home directory.
pub fn validate_location(location: &str) -> Result<(), SpecError> {
    if location.trim().is_empty() {
        return Err(SpecError::EmptyLocation);
    }
    if location
        .char_indices()
        .any(|(index, ch)| ch == '~' && index != 0)
    {
        return Err(SpecError::TildeNotLeading {
            location: location.to_owned(),
        });
    }
    if location.contains("**") {
        return Err(SpecError::DoubleStar {
            location: location.to_owned(),
        });
    }

    // A leading `/` is the root; every component after it must be non-empty.
    let body = location.strip_prefix('/').unwrap_or(location);
    for (index, component) in body.split('/').enumerate() {
        if component.is_empty() {
            return Err(SpecError::EmptyPathComponent {
                location: location.to_owned(),
            });
        }
        if index == 0 && component.starts_with('*') {
            return Err(SpecError::LeadingStar {
                location: location.to_owned(),
            });
        }
    }

    let mut rest = location;
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            return Err(SpecError::UnknownPlaceholder {
                token: rest[start..].to_owned(),
            });
        };
        let name = &after[..end];
        if !LOCATION_PLACEHOLDERS.contains(&name) {
            return Err(SpecError::UnknownPlaceholder {
                token: format!("{{{name}}}"),
            });
        }
        rest = &after[end + 1..];
    }
    Ok(())
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
    /// Directories under the user's home, each written with a leading `~/`.
    pub home_dirs: Vec<String>,
    /// Directories relative to the repository root.
    pub project_dirs: Vec<String>,
}

/// An error while loading or checking harness specs.
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
    /// A template uses a placeholder that no [`Values`] field provides.
    #[error("unknown placeholder in {token}")]
    UnknownPlaceholder {
        /// The token or location pattern holding the placeholder.
        token: String,
    },
    /// A template needs a placeholder value the caller did not supply.
    #[error("missing value for placeholder {placeholder}")]
    MissingValue {
        /// The placeholder, e.g. `{model}`.
        placeholder: String,
    },
    /// A record location uses `~` anywhere but its first path component.
    #[error("location {location} uses `~` outside its first path component")]
    TildeNotLeading {
        /// The location pattern that failed.
        location: String,
    },
    /// An argument template has an unmatched `{` or `}`.
    #[error("unmatched brace in template token {token}")]
    UnmatchedBrace {
        /// The template token that failed.
        token: String,
    },
    /// A record location is empty or whitespace only.
    #[error("location pattern is empty")]
    EmptyLocation,
    /// A record location has an empty path component, e.g. `a//b` or a trailing `/`.
    #[error("location {location} has an empty path component")]
    EmptyPathComponent {
        /// The location pattern that failed.
        location: String,
    },
    /// A record location uses `**`, which no supported CLI needs.
    #[error("location {location} uses `**`: use a single `*` inside one path component")]
    DoubleStar {
        /// The location pattern that failed.
        location: String,
    },
    /// A record location starts its first path component with `*`.
    #[error("location {location} starts a path component with `*`")]
    LeadingStar {
        /// The location pattern that failed.
        location: String,
    },
    /// A skill directory is neither home-relative nor repo-relative.
    #[error(
        "invalid skill directory {dir}: use a leading `~/` for home dirs and a relative path for project dirs"
    )]
    InvalidSkillDir {
        /// The directory that failed.
        dir: String,
    },
}

/// Fill the placeholders in an argument template with values.
///
/// Returns one token per template token, so the result can be appended to a
/// command line as arguments. A template token with no placeholder is copied
/// through unchanged; `{model}`, `{effort}`, `{prompt}`, `{prompt_file}` and
/// `{session}` are replaced from `values`.
pub fn render_args(template: &[String], values: &Values) -> Result<Vec<String>, SpecError> {
    let mut rendered = Vec::with_capacity(template.len());
    for token in template {
        rendered.push(render_token(token, values)?);
    }
    Ok(rendered)
}

/// Fill the placeholders in one argument token.
fn render_token(token: &str, values: &Values) -> Result<String, SpecError> {
    let mut out = String::with_capacity(token.len());
    let mut rest = token;
    while let Some(start) = rest.find('{') {
        let prefix = &rest[..start];
        if prefix.contains('}') {
            return Err(SpecError::UnmatchedBrace {
                token: token.to_owned(),
            });
        }
        out.push_str(prefix);
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            return Err(SpecError::UnmatchedBrace {
                token: token.to_owned(),
            });
        };
        let name = &after[..end];
        let value = match name {
            "model" => &values.model,
            "effort" => &values.effort,
            "prompt" => &values.prompt,
            "prompt_file" => &values.prompt_file,
            "session" => &values.session,
            _ => {
                return Err(SpecError::UnknownPlaceholder {
                    token: token.to_owned(),
                });
            }
        };
        let Some(value) = value else {
            return Err(SpecError::MissingValue {
                placeholder: format!("{{{name}}}"),
            });
        };
        out.push_str(value);
        rest = &after[end + 1..];
    }
    if rest.contains('}') {
        return Err(SpecError::UnmatchedBrace {
            token: token.to_owned(),
        });
    }
    out.push_str(rest);
    Ok(out)
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
    use super::{HarnessSpec, SpecError, Values, load_dir, render_args, validate_location};
    use std::path::Path;

    fn harness_dir() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/harness")
    }

    fn tokens(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
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
model_args = ["--model", "{model}"]
verified = false

[models]

[records]
kind = "jsonl"
location = "~/.claude/projects/{cwd_slug}/{session}.jsonl"
errors = "isApiErrorMessage"
verified = true

[quota]
source = "anthropic_oauth"

[skills]
home_dirs = ["~/.claude/skills"]
project_dirs = []
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
    fn every_shipped_spec_validates() {
        let specs = match load_dir(&harness_dir()) {
            Ok(specs) => specs,
            Err(error) => panic!("harness data should load: {error}"),
        };
        assert_eq!(specs.len(), 5);
        for spec in &specs {
            if let Err(error) = spec.validate() {
                panic!("{} should validate: {error}", spec.id);
            }
        }
    }

    fn parse_fixture(fixture: &str) -> HarnessSpec {
        match toml::from_str::<HarnessSpec>(fixture) {
            Ok(spec) => spec,
            Err(error) => panic!("fixture should parse: {error}"),
        }
    }

    #[test]
    fn renders_codex_effort_arguments() {
        let template = tokens(&["-c", "model_reasoning_effort={effort}"]);
        let values = Values {
            effort: Some("high".to_owned()),
            ..Values::default()
        };
        let rendered = match render_args(&template, &values) {
            Ok(rendered) => rendered,
            Err(error) => panic!("template should render: {error}"),
        };
        assert_eq!(rendered, tokens(&["-c", "model_reasoning_effort=high"]));
    }

    #[test]
    fn renders_claude_prompt_file_arguments() {
        let template = tokens(&["--append-system-prompt-file", "{prompt_file}"]);
        let values = Values {
            prompt_file: Some("/tmp/p.md".to_owned()),
            ..Values::default()
        };
        let rendered = match render_args(&template, &values) {
            Ok(rendered) => rendered,
            Err(error) => panic!("template should render: {error}"),
        };
        assert_eq!(
            rendered,
            tokens(&["--append-system-prompt-file", "/tmp/p.md"])
        );
    }

    #[test]
    fn rejects_unknown_placeholder() {
        let template = tokens(&["--model", "{nope}"]);
        let error = match render_args(&template, &Values::default()) {
            Ok(rendered) => panic!("unknown placeholder should be rejected, got {rendered:?}"),
            Err(error) => error,
        };
        assert!(matches!(error, SpecError::UnknownPlaceholder { .. }));
    }

    #[test]
    fn rejects_missing_value() {
        let template = tokens(&["--model", "{model}"]);
        let error = match render_args(&template, &Values::default()) {
            Ok(rendered) => panic!("missing value should be rejected, got {rendered:?}"),
            Err(error) => error,
        };
        assert!(matches!(error, SpecError::MissingValue { .. }));
    }

    #[test]
    fn accepts_shipped_location_patterns() {
        for location in [
            "~/.claude/projects/{cwd_slug}/{session}.jsonl",
            "~/.codex/sessions/{yyyy}/{mm}/{dd}/rollout-*.jsonl",
            "~/.pi/agent/sessions/{cwd_slug}/*_{session}.jsonl",
            "~/.local/share/kilo/kilo.db",
        ] {
            if let Err(error) = validate_location(location) {
                panic!("{location} should validate: {error}");
            }
        }
    }

    #[test]
    fn rejects_unknown_location_placeholder() {
        let error = match validate_location("~/.pi/agent/sessions/{cwd_slug}/{ts}.jsonl") {
            Ok(()) => panic!("unknown location placeholder should be rejected"),
            Err(error) => error,
        };
        assert!(matches!(error, SpecError::UnknownPlaceholder { .. }));
    }

    #[test]
    fn rejects_tilde_outside_first_component() {
        let error = match validate_location("/home/me/~cache/{session}.jsonl") {
            Ok(()) => panic!("misplaced `~` should be rejected"),
            Err(error) => error,
        };
        assert!(matches!(error, SpecError::TildeNotLeading { .. }));
    }

    #[test]
    fn rejects_bad_skill_dirs() {
        let absolute = MINIMAL_SPEC.replacen(
            r#"home_dirs = ["~/.claude/skills"]"#,
            r#"home_dirs = ["/opt/skills"]"#,
            1,
        );
        let spec = parse_fixture(&absolute);
        assert!(matches!(
            spec.validate(),
            Err(SpecError::InvalidSkillDir { .. })
        ));

        let home_relative_project =
            MINIMAL_SPEC.replacen("project_dirs = []", r#"project_dirs = ["~/skills"]"#, 1);
        let spec = parse_fixture(&home_relative_project);
        assert!(matches!(
            spec.validate(),
            Err(SpecError::InvalidSkillDir { .. })
        ));
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

    #[test]
    fn rejects_unmatched_open_brace() {
        let template = tokens(&["--model={model"]);
        let error = match render_args(&template, &Values::default()) {
            Ok(rendered) => panic!("unmatched `{{` should be rejected, got {rendered:?}"),
            Err(error) => error,
        };
        assert!(matches!(error, SpecError::UnmatchedBrace { .. }));
    }

    #[test]
    fn rejects_unmatched_close_brace() {
        let template = tokens(&["--model}"]);
        let error = match render_args(&template, &Values::default()) {
            Ok(rendered) => panic!("unmatched `}}` should be rejected, got {rendered:?}"),
            Err(error) => error,
        };
        assert!(matches!(error, SpecError::UnmatchedBrace { .. }));
    }

    fn location_error(location: &str) -> SpecError {
        match validate_location(location) {
            Ok(()) => panic!("{location} should be rejected"),
            Err(error) => error,
        }
    }

    #[test]
    fn rejects_empty_location() {
        assert!(matches!(location_error(""), SpecError::EmptyLocation));
        assert!(matches!(location_error("   "), SpecError::EmptyLocation));
    }

    #[test]
    fn rejects_empty_location_component() {
        let error = location_error("~/.claude//{session}.jsonl");
        assert!(matches!(error, SpecError::EmptyPathComponent { .. }));
    }

    #[test]
    fn rejects_trailing_slash_location() {
        let error = location_error("~/.claude/projects/");
        assert!(matches!(error, SpecError::EmptyPathComponent { .. }));
    }

    #[test]
    fn rejects_double_star_location() {
        let error = location_error("~/.codex/sessions/**/*.jsonl");
        assert!(matches!(error, SpecError::DoubleStar { .. }));
    }

    #[test]
    fn rejects_leading_star_component() {
        let error = location_error("*.jsonl/{session}");
        assert!(matches!(error, SpecError::LeadingStar { .. }));
    }
}
