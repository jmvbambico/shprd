//! Agent strings (`<program>/<model>`), effort levels, and model listing.
//!
//! An agent string names a harness by its spec id and, optionally, the model
//! that harness should run: `claude/claude-opus-5-5`. A model may itself
//! contain slashes and spaces (`pi/Command Code/deepseek/deepseek-v4.1-flash`),
//! so the string splits at the *first* `/` only. Some CLIs (pi) need the
//! provider to be part of the model name; a spec says so with
//! [`crate::spec::ModelsSpec::provider_qualified`].
//!
//! A program lists its models one of two ways, as
//! [`crate::spec::CatalogSource`] says: asked of the program (its output read by
//! the spec's [`crate::spec::CatalogParser`]) or read from a static list in
//! `data/models/` ([`StaticCatalog`]). Both become a [`Catalog`]: the models,
//! the effort levels each one takes, and the program's levels overall.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use thiserror::Error;

use crate::data::{DataDir, Kind};
use crate::detect::{self, DetectError};
use crate::spec::{CatalogParser, EffortSource, HarnessSpec, ModelsSpec};

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
    /// A static model catalog could not be read.
    #[error("failed to read the static model catalog {path}: {source}")]
    CatalogRead {
        /// The file that failed.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// A static model catalog is not valid TOML or does not match the schema.
    #[error("failed to parse the static model catalog {path}: {source}")]
    CatalogParse {
        /// The file that failed.
        path: PathBuf,
        /// The underlying TOML error.
        #[source]
        source: toml::de::Error,
    },
    /// A catalog's output is not in the shape its parser reads.
    #[error("`{program}` lists its models in a shape {parser:?} cannot read: {reason}")]
    CatalogFormat {
        /// The program whose catalog could not be read.
        program: String,
        /// The parser that was asked to read it.
        parser: CatalogParser,
        /// What was wrong, in a few words.
        reason: String,
    },
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
/// The command runs from the `:`-separated `path_env` with `timeout`, and its
/// output is read by the spec's [`CatalogParser`] (see [`parse_catalog`]), so a
/// JSON catalog yields model names, not a line of JSON. Duplicates are dropped
/// and the CLI's order is kept. The output itself is never printed here, so a
/// caller decides whether a model it does not find is worth a warning.
pub fn list_models(
    spec: &HarnessSpec,
    path_env: &str,
    timeout: Duration,
) -> Result<Option<Vec<String>>, ModelError> {
    Ok(load_catalog(spec, path_env, timeout)?
        .map(|catalog| catalog.models.into_iter().map(|model| model.id).collect()))
}

/// Read a harness's whole catalog by asking its list command, or [`None`] when
/// its spec has no command.
///
/// Unlike [`list_models`], this keeps each model's effort levels, which is what
/// makes a model that cannot think (`pi`'s `thinking: no`) tellable apart from
/// one that can. A listing that cannot be read is [`ModelError`] rather than an
/// empty catalog, so a caller can decide whether to warn or stay silent.
pub fn load_catalog(
    spec: &HarnessSpec,
    path_env: &str,
    timeout: Duration,
) -> Result<Option<Catalog>, ModelError> {
    let models: &ModelsSpec = &spec.models;
    let Some(command) = &models.list_command else {
        return Ok(None);
    };
    let mut parts = command.iter();
    let Some(program) = parts.next() else {
        return Ok(Some(Catalog::default()));
    };
    let Some(binary) = detect::find_binary(program, path_env) else {
        return Err(ModelError::MissingBinary {
            program: program.clone(),
        });
    };
    let args: Vec<String> = parts.cloned().collect();
    let output = detect::run_with_timeout(&binary, &args, &[], timeout)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_catalog(spec, &stdout).map(Some)
}

/// One model a program offers, as its catalog describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogModel {
    /// The name the program's model flag takes, e.g. `Command Code/deepseek/x`.
    pub id: String,
    /// The words a person sees; the id when the catalog names no other.
    pub name: String,
    /// The effort levels this model takes, lowest first.
    ///
    /// Empty when the program takes none, or when the catalog does not say.
    pub efforts: Vec<String>,
    /// The level the model starts at when a role names none, if the catalog says.
    pub default_effort: Option<String>,
    /// Whether the model has a speed tier of its own, so a role may ask for fast.
    pub fast: bool,
}

/// A program's models and effort levels, as its catalog describes them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Catalog {
    /// The models, in the order the catalog returned them.
    pub models: Vec<CatalogModel>,
    /// The levels the program takes overall, lowest first.
    ///
    /// The picker offers a model's own levels where the catalog carries them
    /// and these where it does not.
    pub efforts: Vec<String>,
}

/// Whether an effort level can reach a model, as its catalog says.
///
/// Used to keep a level off a model that cannot use it: `pi --list-models` says
/// `thinking: no` for some models, and passing `--thinking low` to those looks
/// as if the setting applied when it does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffortFit {
    /// The catalog names no model, or has not heard of this one: nothing is known.
    Unknown,
    /// The model takes the level.
    Applies,
    /// The model takes no effort at all.
    TakesNone,
    /// The model takes effort, but not this level; the levels it takes.
    Lacks(Vec<String>),
}

impl Catalog {
    /// The model called `id`, if the catalog names one.
    pub fn model(&self, id: &str) -> Option<&CatalogModel> {
        self.models.iter().find(|model| model.id == id)
    }

    /// The model names, in the catalog's order.
    pub fn names(&self) -> Vec<&str> {
        self.models.iter().map(|model| model.id.as_str()).collect()
    }

    /// The levels `model` takes, empty when it takes none; a model the catalog
    /// does not name falls back to the program's own levels.
    pub fn efforts_for(&self, model: &str) -> &[String] {
        match self.model(model) {
            Some(found) => &found.efforts,
            None => &self.efforts,
        }
    }

    /// Whether `effort` can reach `model`, as far as this catalog knows.
    ///
    /// [`EffortFit::Unknown`] is the honest answer when the catalog names no
    /// model or has not heard of this one: a stale listing must not take a
    /// working level away.
    pub fn effort_fit(&self, model: Option<&str>, effort: &str) -> EffortFit {
        let Some(model) = model else {
            return EffortFit::Unknown;
        };
        let Some(found) = self.model(model) else {
            return EffortFit::Unknown;
        };
        if found.efforts.is_empty() {
            return EffortFit::TakesNone;
        }
        if found.efforts.iter().any(|level| level == effort) {
            EffortFit::Applies
        } else {
            EffortFit::Lacks(found.efforts.clone())
        }
    }

    /// The warning an effort level earns when it cannot reach `model`, if it
    /// cannot: `None` when the level applies or the catalog does not know.
    ///
    /// The words are the same wherever the check runs — settings validation,
    /// `shprd settings show` and the launch — so a person reads one sentence
    /// about one value, not three variants of it.
    pub fn effort_warning(
        &self,
        program: &str,
        model: Option<&str>,
        effort: &str,
    ) -> Option<String> {
        let model = model?;
        match self.effort_fit(Some(model), effort) {
            EffortFit::TakesNone => Some(format!(
                "`{program}` lists `{model}` as not thinking, so the effort `{effort}` has no \
                 effect: the flag is dropped"
            )),
            EffortFit::Lacks(levels) => Some(format!(
                "`{program}` does not list `{effort}` for `{model}` (it takes {}), so the flag is \
                 dropped",
                levels.join(", ")
            )),
            EffortFit::Applies | EffortFit::Unknown => None,
        }
    }

    /// Build a catalog from a program's static list, with `levels` as the
    /// program's own effort levels (they are not part of the file).
    pub fn from_static(list: &StaticCatalog, levels: &[String]) -> Catalog {
        let models = list
            .model
            .iter()
            .map(|model| CatalogModel {
                id: model.id.clone(),
                name: model.name.clone().unwrap_or_else(|| model.id.clone()),
                efforts: if model.efforts.is_empty() {
                    levels.to_vec()
                } else {
                    model.efforts.clone()
                },
                default_effort: model.default_effort.clone(),
                fast: model.fast,
            })
            .collect();
        Catalog {
            models,
            efforts: levels.to_vec(),
        }
    }
}

/// Read one catalog command's output into models and effort levels.
///
/// Which shape the output is in comes from the spec's
/// [`CatalogParser`](crate::spec::CatalogParser); the levels each model takes
/// come from the output where the program carries them (Codex does) and from
/// [`crate::spec::EffortSpec::levels`] otherwise. A spec with no parser and no
/// list command yields an empty catalog.
pub fn parse_catalog(spec: &HarnessSpec, text: &str) -> Result<Catalog, ModelError> {
    let levels = program_levels(spec);
    let models = match spec.models.parser {
        CatalogParser::FirstToken => first_token_models(text, &levels),
        CatalogParser::PiTable => pi_table_models(spec, text)?,
        CatalogParser::CodexCatalog => codex_catalog_models(spec, text)?,
    };
    Ok(Catalog {
        models,
        efforts: levels,
    })
}

/// The static catalog a harness names, read from the shipped data.
///
/// [`None`] when the spec names no static list; an error when it names one that
/// cannot be read or parsed.
pub fn static_catalog(spec: &HarnessSpec, data: &DataDir) -> Result<Option<Catalog>, ModelError> {
    let Some(name) = spec.models.static_list.as_deref() else {
        return Ok(None);
    };
    let rel = format!("{}/{name}", Kind::Models.dir());
    let path = data.path(&rel);
    let text = data.read(&rel).map_err(|source| ModelError::CatalogRead {
        path: path.clone(),
        source,
    })?;
    let list = StaticCatalog::parse(&path, &text)?;
    Ok(Some(Catalog::from_static(&list, &program_levels(spec))))
}

/// The program's own effort levels, from the spec.
///
/// A program that takes no level, or gets them only from its catalog, still
/// carries the fallback list here; an empty list means "none at all".
fn program_levels(spec: &HarnessSpec) -> Vec<String> {
    if spec.models.efforts.source == EffortSource::None {
        Vec::new()
    } else {
        spec.models.efforts.levels.clone()
    }
}

/// Models from a listing that puts one name per line, as its first word.
fn first_token_models(text: &str, levels: &[String]) -> Vec<CatalogModel> {
    model_names(text)
        .into_iter()
        .map(|id| CatalogModel {
            name: id.clone(),
            efforts: levels.to_vec(),
            default_effort: None,
            fast: false,
            id,
        })
        .collect()
}

/// The columns of pi's `--list-models` table, by where each one starts.
struct PiColumns {
    provider: usize,
    model: usize,
    context: usize,
    thinking: usize,
    images: usize,
}

impl PiColumns {
    /// Read the header line, or say which column is missing.
    fn parse(spec: &HarnessSpec, header: &str) -> Result<PiColumns, ModelError> {
        let column = |name: &str| {
            header.find(name).ok_or_else(|| ModelError::CatalogFormat {
                program: spec.id.clone(),
                parser: CatalogParser::PiTable,
                reason: format!("the header has no `{name}` column: {header:?}"),
            })
        };
        Ok(PiColumns {
            provider: column("provider")?,
            model: column("model")?,
            context: column("context")?,
            thinking: column("thinking")?,
            images: column("images")?,
        })
    }

    /// One column of one row, trimmed; empty when the row is too short.
    fn slice(line: &str, start: usize, end: usize) -> String {
        line.get(start..end).unwrap_or_default().trim().to_owned()
    }
}

/// Models from pi's `--list-models` table.
///
/// Each row is `provider  model  context  max-out  thinking  images`. The
/// provider may hold a space (`Command Code`), so the columns are read by the
/// header's offsets rather than by splitting on whitespace, and the model the
/// flag takes is `provider/model`, the reference pi's own resolver accepts
/// (`dist/core/model-resolver.js`, v1.4.0).
fn pi_table_models(spec: &HarnessSpec, text: &str) -> Result<Vec<CatalogModel>, ModelError> {
    let mut lines = text.lines().filter(|line| !line.trim().is_empty());
    let Some(header) = lines.next() else {
        return Err(ModelError::CatalogFormat {
            program: spec.id.clone(),
            parser: CatalogParser::PiTable,
            reason: "the listing is empty".to_owned(),
        });
    };
    let columns = PiColumns::parse(spec, header)?;
    let levels = program_levels(spec);
    let mut models = Vec::new();
    let mut seen = BTreeSet::new();
    for line in lines {
        let provider = PiColumns::slice(line, columns.provider, columns.model);
        let model = PiColumns::slice(line, columns.model, columns.context);
        let thinking = PiColumns::slice(line, columns.thinking, columns.images);
        if provider.is_empty() || model.is_empty() {
            continue;
        }
        let id = format!("{provider}/{model}");
        if !seen.insert(id.clone()) {
            continue;
        }
        models.push(CatalogModel {
            name: id.clone(),
            efforts: if thinking == "yes" {
                levels.clone()
            } else {
                Vec::new()
            },
            default_effort: None,
            fast: false,
            id,
        });
    }
    Ok(models)
}

/// Codex's catalog, as `codex debug models` and `~/.codex/models_cache.json`
/// both write it: a `models` array, one row per model.
#[derive(Debug, Deserialize)]
struct CodexCatalogFile {
    #[serde(default)]
    models: Vec<CodexModel>,
}

/// One row of Codex's catalog; the fields the picker needs, and no others.
#[derive(Debug, Deserialize)]
struct CodexModel {
    slug: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    visibility: Option<String>,
    #[serde(default)]
    default_reasoning_level: Option<String>,
    #[serde(default)]
    supported_reasoning_levels: Vec<CodexLevel>,
    #[serde(default)]
    service_tiers: Vec<serde_json::Value>,
}

/// One reasoning level in a Codex model row.
#[derive(Debug, Deserialize)]
struct CodexLevel {
    effort: String,
}

/// Models from Codex's JSON catalog, hidden rows left out.
fn codex_catalog_models(spec: &HarnessSpec, text: &str) -> Result<Vec<CatalogModel>, ModelError> {
    let file: CodexCatalogFile =
        serde_json::from_str(text).map_err(|error| ModelError::CatalogFormat {
            program: spec.id.clone(),
            parser: CatalogParser::CodexCatalog,
            reason: error.to_string(),
        })?;
    let fallback = program_levels(spec);
    Ok(file
        .models
        .into_iter()
        .filter(|model| model.visibility.as_deref() != Some("hide"))
        .map(|model| {
            let efforts: Vec<String> = model
                .supported_reasoning_levels
                .into_iter()
                .map(|level| level.effort)
                .collect();
            CatalogModel {
                name: model.display_name.unwrap_or_else(|| model.slug.clone()),
                efforts: if efforts.is_empty() {
                    fallback.clone()
                } else {
                    efforts
                },
                default_effort: model.default_reasoning_level,
                fast: !model.service_tiers.is_empty(),
                id: model.slug,
            }
        })
        .collect())
}

impl ModelsSpec {
    /// Where this spec's static catalog lives under a `data/models` directory,
    /// if it names one.
    ///
    /// The file name is validated when the harness loads: a bare `*.toml` name
    /// with no directory in it, so the catalogue can only sit in `data/models/`.
    /// The file a static list names, under `models_dir`.
    ///
    /// A path-shaped helper for callers holding a directory; the binary reads
    /// the shipped data through [`crate::data::DataDir`].
    pub fn static_list_path(&self, models_dir: &Path) -> Option<PathBuf> {
        self.static_list.as_ref().map(|name| models_dir.join(name))
    }
}

/// A program's static model list, as one `data/models/<program>.toml` file.
///
/// For a program with no model-list command (Claude Code), the harness names
/// this file with [`ModelsSpec::static_list`] and the picker reads the models
/// and their effort levels from it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StaticCatalog {
    /// The file's schema version; 1 is the only one this crate reads.
    pub schema: u32,
    /// The models, in the order the picker shows them.
    #[serde(default)]
    pub model: Vec<StaticModel>,
}

/// One model in a [`StaticCatalog`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StaticModel {
    /// The name the program's model flag takes.
    pub id: String,
    /// The words a person sees, when they differ from the id.
    #[serde(default)]
    pub name: Option<String>,
    /// The effort levels this model takes, lowest first.
    #[serde(default)]
    pub efforts: Vec<String>,
    /// The level this model starts at when a role names none, if it is known.
    #[serde(default)]
    pub default_effort: Option<String>,
    /// Whether the program's fast mode applies to this model.
    #[serde(default)]
    pub fast: bool,
}

impl StaticCatalog {
    /// Read one static catalog file.
    pub fn load(path: &Path) -> Result<StaticCatalog, ModelError> {
        let source = std::fs::read_to_string(path).map_err(|source| ModelError::CatalogRead {
            path: path.to_owned(),
            source,
        })?;
        StaticCatalog::parse(path, &source)
    }

    /// Parse one static catalog already read in, naming `path` in any error.
    ///
    /// This is [`StaticCatalog::load`] for a catalog that is not a file on this
    /// machine: the copy embedded in the binary, which
    /// [`crate::data::DataDir`] reads into text.
    pub(crate) fn parse(path: &Path, source: &str) -> Result<StaticCatalog, ModelError> {
        toml::from_str(source).map_err(|source| ModelError::CatalogParse {
            path: path.to_owned(),
            source,
        })
    }

    /// The model names, in the file's order.
    pub fn names(&self) -> Vec<&str> {
        self.model.iter().map(|model| model.id.as_str()).collect()
    }

    /// The model called `id`, if the list names one.
    pub fn model(&self, id: &str) -> Option<&StaticModel> {
        self.model.iter().find(|model| model.id == id)
    }
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
        harness_spec(id, &models)
    }

    /// One spec whose whole `[models]` section is `models`, as written.
    fn harness_spec(id: &str, models: &str) -> HarnessSpec {
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

    /// A captured command output the catalog tests read.
    fn fixture(name: &str) -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) => panic!("{} should be readable: {error}", path.display()),
        }
    }

    /// The model `id` in `catalog`, panicking when it is not listed.
    fn named<'a>(catalog: &'a Catalog, id: &str) -> &'a CatalogModel {
        match catalog.model(id) {
            Some(model) => model,
            None => panic!("{id} should be listed: {:?}", catalog.names()),
        }
    }

    /// The catalog of `spec`, from a captured command output.
    fn catalog(spec: &HarnessSpec, name: &str) -> Catalog {
        match parse_catalog(spec, &fixture(name)) {
            Ok(catalog) => catalog,
            Err(error) => panic!("the catalog should parse: {error}"),
        }
    }

    #[test]
    fn parses_a_first_token_listing() {
        let spec = spec("opencode", r#"["opencode", "models"]"#, false);
        let catalog = catalog(&spec, "opencode-models.txt");
        assert_eq!(
            catalog.names(),
            [
                "opencode/beam-free",
                "opencode/big-pickle",
                "opencode/claude-fable-5",
                "opencode/claude-fable-5-1",
                "opencode/claude-fable-5-2",
                "opencode/claude-haiku-4-5",
                "opencode/claude-haiku-5-5",
                "opencode/claude-opus-4-5",
                "opencode/claude-opus-4-6",
                "opencode/claude-opus-4-7",
                "opencode/claude-opus-4-8",
                "opencode/claude-opus-5",
            ]
        );
        assert!(catalog.efforts.is_empty(), "opencode takes no effort");
        let model = named(&catalog, "opencode/claude-opus-5");
        assert!(model.efforts.is_empty());
        assert!(!model.fast);
    }

    #[test]
    fn parses_pis_table_where_the_provider_holds_a_space() {
        let spec = harness_spec(
            "pi",
            r#"list_command = ["pi", "--list-models"]
parser = "pi_table"
provider_qualified = true

[models.efforts]
source = "static"
levels = ["off", "low", "high"]"#,
        );
        let catalog = catalog(&spec, "pi-models.txt");
        assert_eq!(
            catalog.names(),
            [
                "Command Code/deepseek/deepseek-v4.1-flash",
                "commandcode/deepseek/deepseek-v4.1-flash",
                "commandcode/z-ai/glm-5.3-flash",
            ]
        );
        assert_eq!(catalog.efforts, ["off", "low", "high"]);
        let model = named(&catalog, "Command Code/deepseek/deepseek-v4.1-flash");
        assert!(model.efforts.is_empty(), "this row says thinking: no");
    }

    #[test]
    fn a_pi_model_that_thinks_takes_the_programs_levels() {
        let spec = harness_spec(
            "pi",
            r#"list_command = ["pi", "--list-models"]
parser = "pi_table"

[models.efforts]
source = "static"
levels = ["off", "low", "high"]"#,
        );
        // The same padded columns pi prints, so the offsets are the ones a
        // captured listing has.
        let row = |provider: &str,
                   model: &str,
                   context: &str,
                   max_out: &str,
                   thinking: &str,
                   images: &str| {
            format!("{provider:<12}{model:<20}{context:<9}{max_out:<9}{thinking:<10}{images}")
        };
        let table = format!(
            "{}\n{}\n{}\n",
            row(
                "provider", "model", "context", "max-out", "thinking", "images"
            ),
            row(
                "anthropic",
                "claude-opus-4-8",
                "1.0M",
                "32.0K",
                "yes",
                "yes"
            ),
            row("local", "tiny", "8.0K", "1.0K", "no", "no"),
        );
        let catalog = match parse_catalog(&spec, &table) {
            Ok(catalog) => catalog,
            Err(error) => panic!("the table should parse: {error}"),
        };
        assert_eq!(catalog.names(), ["anthropic/claude-opus-4-8", "local/tiny"]);
        assert_eq!(
            catalog.efforts_for("anthropic/claude-opus-4-8"),
            ["off", "low", "high"]
        );
        assert!(catalog.efforts_for("local/tiny").is_empty());
    }

    #[test]
    fn a_no_thinking_model_has_no_effort_to_give() {
        let spec = harness_spec(
            "pi",
            r#"list_command = ["pi", "--list-models"]
parser = "pi_table"

[models.efforts]
source = "static"
levels = ["off", "low", "high"]"#,
        );
        let catalog = catalog(&spec, "pi-models-thinking.txt");
        let flat = "Command Code/deepseek/deepseek-v4.1-flash";
        let thinking = "anthropic/claude-opus-4-8";

        assert_eq!(catalog.effort_fit(Some(flat), "high"), EffortFit::TakesNone);
        let warning = match catalog.effort_warning("pi", Some(flat), "high") {
            Some(warning) => warning,
            None => panic!("a model that cannot think earns a warning"),
        };
        assert!(warning.contains("not thinking"), "{warning}");
        assert!(warning.contains("dropped"), "{warning}");

        assert_eq!(
            catalog.effort_fit(Some(thinking), "high"),
            EffortFit::Applies
        );
        assert_eq!(catalog.effort_warning("pi", Some(thinking), "high"), None);

        // A level the model does not take, a model the catalog never saw, and
        // a role that names no model are all told apart from these two.
        assert_eq!(
            catalog.effort_fit(Some(thinking), "max"),
            EffortFit::Lacks(vec!["off".to_owned(), "low".to_owned(), "high".to_owned()])
        );
        assert_eq!(
            catalog.effort_fit(Some("nobody/nothing"), "high"),
            EffortFit::Unknown
        );
        assert_eq!(catalog.effort_fit(None, "high"), EffortFit::Unknown);
        assert_eq!(catalog.effort_warning("pi", None, "high"), None);
    }

    #[test]
    fn parses_the_codex_catalog_and_leaves_hidden_models_out() {
        let spec = harness_spec(
            "codex",
            r#"list_command = ["codex", "debug", "models"]
parser = "codex_catalog"

[models.efforts]
source = "catalog"
levels = ["low", "medium", "high", "xhigh", "max", "ultra"]"#,
        );
        let catalog = catalog(&spec, "codex-models.json");
        assert_eq!(
            catalog.names(),
            ["gpt-6-luna", "gpt-5.6-terra", "gpt-5.6-luna"],
            "the hidden rows are not offered"
        );
        let terra = named(&catalog, "gpt-5.6-terra");
        assert_eq!(terra.name, "GPT-5.6-Terra");
        assert_eq!(
            terra.efforts,
            ["low", "medium", "high", "xhigh", "max", "ultra"]
        );
        assert_eq!(terra.default_effort.as_deref(), Some("medium"));
        assert!(!terra.fast, "no model on this account has a service tier");
    }

    #[test]
    fn a_codex_row_with_a_service_tier_may_ask_for_fast() {
        let spec = harness_spec(
            "codex",
            r#"list_command = ["codex", "debug", "models"]
parser = "codex_catalog""#,
        );
        let text = r#"{"models":[{"slug":"m","display_name":"M","visibility":"list",
            "default_reasoning_level":"high","supported_reasoning_levels":[{"effort":"high"}],
            "service_tiers":[{"id":"priority"}]}]}"#;
        let catalog = match parse_catalog(&spec, text) {
            Ok(catalog) => catalog,
            Err(error) => panic!("the catalog should parse: {error}"),
        };
        let model = named(&catalog, "m");
        assert!(model.fast);
        assert_eq!(model.default_effort.as_deref(), Some("high"));
    }

    #[test]
    fn a_listing_in_the_wrong_shape_is_an_error_naming_the_parser() {
        let spec = harness_spec(
            "codex",
            r#"list_command = ["codex", "debug", "models"]
parser = "codex_catalog""#,
        );
        let error = match parse_catalog(&spec, "not json") {
            Ok(catalog) => panic!("bad JSON should be refused, got {catalog:?}"),
            Err(error) => error,
        };
        assert!(matches!(error, ModelError::CatalogFormat { .. }), "{error}");
        assert!(error.to_string().contains("CodexCatalog"), "{error}");

        let pi = harness_spec(
            "pi",
            r#"parser = "pi_table"
list_command = ["pi", "--list-models"]"#,
        );
        let error = match parse_catalog(&pi, "nothing here\n") {
            Ok(catalog) => panic!("a headerless table should be refused, got {catalog:?}"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("provider"), "{error}");
    }

    #[test]
    fn a_listing_listed_through_the_command_is_parsed_by_the_parser() {
        let dir = match TempDir::new() {
            Ok(dir) => dir,
            Err(error) => panic!("tempdir failed: {error}"),
        };
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex-models.json");
        write_script(dir.path(), "codex", &format!("cat '{}'", fixture.display()));
        let spec = harness_spec(
            "codex",
            r#"list_command = ["codex", "debug", "models"]
parser = "codex_catalog""#,
        );
        let path_env = dir.path().display().to_string();
        assert_eq!(
            listed(&spec, &path_env),
            Some(vec![
                "gpt-6-luna".to_owned(),
                "gpt-5.6-terra".to_owned(),
                "gpt-5.6-luna".to_owned(),
            ])
        );
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
        crate::scripted::write_executable(&dir.join(name), &format!("#!/bin/sh\n{body}\n"));
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

    fn models_dir() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/models")
    }

    /// The shipped data, read through the embedded copy the binary carries.
    fn shipped() -> crate::data::DataDir {
        crate::data::DataDir::resolve(None, None)
    }

    #[test]
    fn loads_the_shipped_claude_catalog() {
        let path = models_dir().join("claude.toml");
        let catalog = match StaticCatalog::load(&path) {
            Ok(catalog) => catalog,
            Err(error) => panic!("data/models/claude.toml should load: {error}"),
        };
        assert_eq!(catalog.schema, 1);
        assert!(catalog.names().contains(&"opus"), "{:?}", catalog.names());
        let opus = match catalog.model("opus") {
            Some(model) => model,
            None => panic!("the catalog should name opus"),
        };
        assert_eq!(opus.name.as_deref(), Some("Opus 5.5"));
        assert!(opus.efforts.contains(&"xhigh".to_owned()));
        assert!(opus.fast, "fast mode is offered on Opus");
        // Haiku and the one-million-token variants are listed, in the ids the CLI takes.
        for id in [
            "haiku",
            "opus[1m]",
            "sonnet[1m]",
            "fable[1m]",
            "fable",
            "sonnet",
        ] {
            assert!(
                catalog.names().contains(&id),
                "{id} is missing: {:?}",
                catalog.names()
            );
        }
        assert_eq!(
            catalog
                .model("haiku")
                .and_then(|model| model.name.as_deref()),
            Some("Haiku 5.5")
        );
        assert_eq!(
            catalog
                .model("opus[1m]")
                .and_then(|model| model.name.as_deref()),
            Some("Opus 5.5 · 1M")
        );
    }

    #[test]
    fn a_spec_static_list_sits_in_the_models_dir() {
        let spec = spec("claude", "", false);
        assert_eq!(spec.models.static_list_path(&models_dir()), None);
        let mut spec = spec;
        spec.models.static_list = Some("claude.toml".to_owned());
        assert_eq!(
            spec.models.static_list_path(&models_dir()),
            Some(models_dir().join("claude.toml"))
        );
    }

    #[test]
    fn the_shipped_claude_spec_reads_its_static_catalog() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/harness/claude.toml");
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) => panic!("{} should be readable: {error}", path.display()),
        };
        let spec = match toml::from_str::<HarnessSpec>(&text) {
            Ok(spec) => spec,
            Err(error) => panic!("the shipped claude spec should parse: {error}"),
        };
        let catalog = match static_catalog(&spec, &shipped()) {
            Ok(Some(catalog)) => catalog,
            Ok(None) => panic!("the claude spec should name a static catalog"),
            Err(error) => panic!("the claude catalog should load: {error}"),
        };
        assert!(catalog.names().contains(&"opus"), "{:?}", catalog.names());
        assert_eq!(catalog.efforts, ["low", "medium", "high", "xhigh", "max"]);
        let opus = named(&catalog, "opus");
        assert_eq!(opus.name, "Opus 5.5");
        assert!(opus.fast);
        assert_eq!(opus.efforts, ["low", "medium", "high", "xhigh", "max"]);
    }

    #[test]
    fn a_spec_with_no_static_list_has_no_static_catalog() {
        let spec = spec("opencode", r#"["opencode", "models"]"#, false);
        match static_catalog(&spec, &shipped()) {
            Ok(None) => {}
            other => panic!("a spec with no static list should have no catalog: {other:?}"),
        }
    }

    #[test]
    fn an_unreadable_static_catalog_is_an_error() {
        let path = models_dir().join("nope.toml");
        let error = match StaticCatalog::load(&path) {
            Ok(_) => panic!("a missing catalog should be refused"),
            Err(error) => error,
        };
        assert!(matches!(error, ModelError::CatalogRead { .. }), "{error}");
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
