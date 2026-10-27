//! Agent strings (`<program>/<model>`), effort levels, and model listing.
//!
//! An agent string names a harness by its spec id and, optionally, the model
//! that harness should run: `claude/claude-opus-5-5`. A model may itself
//! contain slashes and spaces (`pi/Command Code/deepseek/deepseek-v4.1-flash`),
//! so the string splits at the *first* `/` only. Some CLIs (pi) need the
//! provider to be part of the model name; a spec says so with
//! [`crate::spec::ModelsSpec::provider_qualified`].

use std::collections::BTreeSet;
use std::time::Duration;

use thiserror::Error;

use crate::detect::{self, DetectError};
use crate::spec::{HarnessSpec, ModelsSpec};

/// Why an agent string, an effort level or a model listing was rejected.
#[derive(Debug, Error)]
pub enum ModelError {
    /// The part before the first `/` names no spec in the harness table.
    #[error("unknown program `{program}`: known programs are {known}")]
    UnknownProgram {
        /// The id that was written.
        program: String,
        /// The ids the harness table does define, comma separated.
        known: String,
    },
    /// The string ends in a `/` with no model after it.
    #[error("`{agent}` names no model: write `<program>/<model>`")]
    MissingModel {
        /// The agent string as written.
        agent: String,
    },
    /// The harness needs the provider in the model name and none was given.
    #[error("`{program}` needs a provider: write `{hint}`")]
    NeedsProvider {
        /// The harness id, the part before the first `/`.
        program: String,
        /// The shape to write instead, naming `<provider>/<model>`.
        hint: String,
    },
    /// The effort level is not one the roster accepts.
    #[error("unknown effort `{effort}`: use one of {allowed}")]
    UnknownEffort {
        /// The word that was written.
        effort: String,
        /// The accepted words, comma separated.
        allowed: String,
    },
    /// The spec's list command names a binary that is not on the path.
    #[error("`{program}` is not on the path, so its models cannot be listed")]
    MissingBinary {
        /// The binary the list command starts with.
        program: String,
    },
    /// The list command failed to run or timed out.
    #[error(transparent)]
    Detect(#[from] DetectError),
}

/// The effort levels a roster accepts, lowest to highest.
pub const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// A harness plus the model it should run, parsed from an agent string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRef {
    /// The harness spec id, e.g. `claude`.
    pub program: String,
    /// The model to pass to that harness, if the string named one.
    pub model: Option<String>,
}

impl AgentRef {
    /// Parse `<program>` or `<program>/<model>` against the harness table.
    ///
    /// The string splits at its first `/`, because a model may contain more
    /// slashes and spaces. With a model given, a spec that is
    /// [`provider_qualified`](crate::spec::ModelsSpec::provider_qualified)
    /// requires the model to name its provider too, with neither the provider
    /// nor the rest empty or blank.
    pub fn parse(s: &str, specs: &[HarnessSpec]) -> Result<AgentRef, ModelError> {
        let (program, model) = match s.find('/') {
            Some(index) => (&s[..index], Some(&s[index + 1..])),
            None => (s, None),
        };
        let spec = specs
            .iter()
            .find(|spec| spec.id == program)
            .ok_or_else(|| {
                let known: Vec<&str> = specs.iter().map(|spec| spec.id.as_str()).collect();
                ModelError::UnknownProgram {
                    program: program.to_owned(),
                    known: known.join(", "),
                }
            })?;
        let Some(model) = model else {
            return Ok(AgentRef {
                program: program.to_owned(),
                model: None,
            });
        };
        if model.trim().is_empty() {
            return Err(ModelError::MissingModel {
                agent: s.to_owned(),
            });
        }
        if spec.models.provider_qualified {
            let (provider, rest) = model.split_once('/').unwrap_or_default();
            if provider.trim().is_empty() || rest.trim().is_empty() {
                // A bare model is shown back with its name, so the fix is
                // obvious; an empty half keeps the generic shape.
                let hint = if model.contains('/') {
                    format!("{program}/<provider>/<model>")
                } else {
                    format!("{program}/<provider>/{model}")
                };
                return Err(ModelError::NeedsProvider {
                    program: program.to_owned(),
                    hint,
                });
            }
        }
        Ok(AgentRef {
            program: program.to_owned(),
            model: Some(model.to_owned()),
        })
    }

    /// The spec this ref points at, given the same table it was parsed with.
    pub fn spec<'a>(&self, specs: &'a [HarnessSpec]) -> Option<&'a HarnessSpec> {
        specs.iter().find(|spec| spec.id == self.program)
    }
}

impl std::fmt::Display for AgentRef {
    /// Writes the string [`AgentRef::parse`] accepts back out.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.model {
            Some(model) => write!(f, "{}/{}", self.program, model),
            None => write!(f, "{}", self.program),
        }
    }
}

/// Check an effort level: one of [`EFFORTS`], lowercase.
pub fn parse_effort(s: &str) -> Result<String, ModelError> {
    if EFFORTS.contains(&s) {
        return Ok(s.to_owned());
    }
    Err(ModelError::UnknownEffort {
        effort: s.to_owned(),
        allowed: EFFORTS.join(", "),
    })
}

/// List the models a harness offers, or [`None`] when its spec has no command.
///
/// The command runs from the `:`-separated `path_env` with `timeout`. Each
/// non-empty output line contributes its first whitespace token: CLIs decorate
/// the name (`* fast`) and the first token is the name. Duplicates are dropped
/// and the CLI's order is kept. The output itself is never printed here, so a
/// caller decides whether a model it does not find is worth a warning.
pub fn list_models(
    spec: &HarnessSpec,
    path_env: &str,
    timeout: Duration,
) -> Result<Option<Vec<String>>, ModelError> {
    let models: &ModelsSpec = &spec.models;
    let Some(command) = &models.list_command else {
        return Ok(None);
    };
    let mut parts = command.iter();
    let Some(program) = parts.next() else {
        return Ok(Some(Vec::new()));
    };
    let Some(binary) = detect::find_binary(program, path_env) else {
        return Err(ModelError::MissingBinary {
            program: program.clone(),
        });
    };
    let args: Vec<String> = parts.cloned().collect();
    let output = detect::run_with_timeout(&binary, &args, &[], timeout)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(Some(model_names(&stdout)))
}

/// The model names in `text`: the first token of each line, de-duplicated.
fn model_names(text: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut names = Vec::new();
    for line in text.lines() {
        let Some(name) = line.split_whitespace().next() else {
            continue;
        };
        if seen.insert(name.to_owned()) {
            names.push(name.to_owned());
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use tempfile::TempDir;

    use super::*;

    /// A harness table with the three specs the parse cases use.
    fn specs() -> Vec<HarnessSpec> {
        vec![
            spec("claude", "", false),
            spec("opencode", "", false),
            spec("pi", r#"["pi", "--list-models"]"#, true),
        ]
    }

    /// One spec, with a `[models]` block the caller chooses.
    fn spec(id: &str, list_command: &str, provider_qualified: bool) -> HarnessSpec {
        let models = if list_command.is_empty() {
            format!("provider_qualified = {provider_qualified}")
        } else {
            format!("list_command = {list_command}\nprovider_qualified = {provider_qualified}")
        };
        let text = format!(
            r#"
id = "{id}"
name = "Fake {id}"
vendor = "pi"
verified_version = "1.0.0"
dry_patterns = []

[detect]
binary = "{id}"
version_args = ["--version"]
signin_hint = "fake"
signin_args = ["auth", "check"]

[launch]
interactive = ["{id}"]
model_args = ["--model", "{{model}}"]
verified = false

[models]
{models}

[records]
kind = "jsonl"
location = "~/.fake/{{session}}.jsonl"
errors = "error"
verified = true

[quota]
source = "none"

[skills]
home_dirs = []
project_dirs = []
"#
        );
        match toml::from_str::<HarnessSpec>(&text) {
            Ok(spec) => spec,
            Err(error) => panic!("fixture should parse: {error}"),
        }
    }

    /// The ref `s` parses to, panicking with the error when it is refused.
    fn parse_ok(s: &str, specs: &[HarnessSpec]) -> AgentRef {
        match AgentRef::parse(s, specs) {
            Ok(agent) => agent,
            Err(error) => panic!("`{s}` should parse: {error}"),
        }
    }

    /// A script interactive shell can run, standing in for a real agent CLI.
    fn write_script(dir: &Path, name: &str, body: &str) {
        let path = dir.join(name);
        if let Err(error) = fs::write(&path, format!("#!/bin/sh\n{body}\n")) {
            panic!("writing {name} failed: {error}");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::Permissions::from_mode(0o755);
            if let Err(error) = fs::set_permissions(&path, mode) {
                panic!("chmod {name} failed: {error}");
            }
        }
    }

    #[test]
    fn parses_a_plain_agent_string() {
        let specs = specs();
        let parsed = parse_ok("claude/claude-opus-5-5", &specs);
        assert_eq!(parsed.program, "claude");
        assert_eq!(parsed.model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(parsed.to_string(), "claude/claude-opus-5-5");
    }

    #[test]
    fn parses_a_model_that_holds_slashes() {
        let specs = specs();
        let parsed = parse_ok("opencode/opencode/deepseek-v4.1-flash-free", &specs);
        assert_eq!(parsed.program, "opencode");
        assert_eq!(
            parsed.model.as_deref(),
            Some("opencode/deepseek-v4.1-flash-free")
        );
    }

    #[test]
    fn parses_a_provider_qualified_model_with_a_space() {
        let specs = specs();
        let agent = "pi/Command Code/deepseek/deepseek-v4.1-flash";
        let parsed = parse_ok(agent, &specs);
        assert_eq!(parsed.program, "pi");
        assert_eq!(
            parsed.model.as_deref(),
            Some("Command Code/deepseek/deepseek-v4.1-flash")
        );
        assert_eq!(parsed.to_string(), agent);
    }

    #[test]
    fn a_program_with_no_model_is_allowed() {
        let specs = specs();
        let parsed = parse_ok("claude", &specs);
        assert_eq!(parsed.program, "claude");
        assert_eq!(parsed.model, None);
        assert_eq!(parsed.to_string(), "claude");
    }

    #[test]
    fn refuses_a_model_without_a_provider() {
        let specs = specs();
        let parsed = AgentRef::parse("pi/deepseek-v4.1-flash", &specs);
        match parsed {
            Err(ModelError::NeedsProvider { program, hint }) => {
                assert_eq!(program, "pi");
                assert!(hint.contains("/<provider>/deepseek-v4.1-flash"), "{hint}");
            }
            other => panic!("expected NeedsProvider, got {other:?}"),
        }
    }

    #[test]
    fn refuses_a_provider_qualified_model_with_an_empty_part() {
        let specs = specs();
        for bad in ["pi//m", "pi/ /m", "pi/p/", "pi/p/ "] {
            match AgentRef::parse(bad, &specs) {
                Err(ModelError::NeedsProvider { program, .. }) => assert_eq!(program, "pi"),
                other => panic!("`{bad}` should need a provider, got {other:?}"),
            }
        }
    }

    #[test]
    fn accepts_a_provider_qualified_model_holding_slashes() {
        let specs = specs();
        let parsed = parse_ok("pi/Command Code/deepseek/x", &specs);
        assert_eq!(parsed.model.as_deref(), Some("Command Code/deepseek/x"));
    }

    #[test]
    fn refuses_an_unknown_program() {
        let specs = specs();
        match AgentRef::parse("gemini/pro", &specs) {
            Err(ModelError::UnknownProgram { program, known }) => {
                assert_eq!(program, "gemini");
                assert!(known.contains("claude"), "{known}");
            }
            other => panic!("expected UnknownProgram, got {other:?}"),
        }
    }

    #[test]
    fn refuses_a_trailing_slash() {
        let specs = specs();
        match AgentRef::parse("claude/", &specs) {
            Err(ModelError::MissingModel { agent }) => assert_eq!(agent, "claude/"),
            other => panic!("expected MissingModel, got {other:?}"),
        }
    }

    /// The models a spec lists, panicking with the error when listing fails.
    fn listed(spec: &HarnessSpec, path_env: &str) -> Option<Vec<String>> {
        match list_models(spec, path_env, Duration::from_secs(5)) {
            Ok(models) => models,
            Err(error) => panic!("listing should succeed: {error}"),
        }
    }

    #[test]
    fn accepts_the_documented_efforts_only() {
        for effort in EFFORTS {
            assert_eq!(parse_effort(effort).ok().as_deref(), Some(effort));
        }
        assert!(parse_effort("HIGH").is_err());
        assert!(parse_effort("turbo").is_err());
    }

    #[test]
    fn lists_models_from_the_spec_command() {
        let dir = match TempDir::new() {
            Ok(dir) => dir,
            Err(error) => panic!("tempdir failed: {error}"),
        };
        write_script(
            dir.path(),
            "fake",
            "printf '%s\\n' 'fast  (default)' 'slow' 'fast' '' 'last'",
        );
        let spec = spec("fake", r#"["fake", "--list-models"]"#, false);
        let path_env = dir.path().display().to_string();
        assert_eq!(
            listed(&spec, &path_env),
            Some(vec![
                "fast".to_owned(),
                "slow".to_owned(),
                "last".to_owned()
            ])
        );
    }

    #[test]
    fn a_spec_without_a_list_command_lists_nothing() {
        let spec = spec("fake", "", false);
        assert_eq!(listed(&spec, ""), None);
    }

    #[test]
    fn a_missing_listing_binary_is_an_error() {
        let spec = spec("fake", r#"["fake-missing", "--list"]"#, false);
        let listed = list_models(&spec, "", Duration::from_secs(1));
        match listed {
            Err(ModelError::MissingBinary { program }) => assert_eq!(program, "fake-missing"),
            other => panic!("expected MissingBinary, got {other:?}"),
        }
    }
}
