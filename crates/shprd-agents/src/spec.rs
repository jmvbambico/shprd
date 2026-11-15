//! Harness table: the typed schema for the per-CLI TOML specs in `data/harness/`.
//!
//! Each spec records what shprd needs to detect, launch and watch one agent CLI,
//! and — since M9 — what the per-program adapter needs on top: the access modes
//! a role may pick, the fast switch, where the model catalog and its effort
//! levels come from, how a brief reaches the program, where its status is read,
//! and whether it takes a hook for its asks. Locations and formats live in data
//! so supporting a new CLI version is a table change rather than a code change
//! (see `docs/agents.md`).
//!
//! Every M9 section is optional and has a typed default, so a harness written
//! before M9 still loads: no modes, no fast switch, a first-message brief, the
//! agent's own report and then its screen, and no ask hook.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One supported agent CLI, as loaded from a TOML file in `data/harness/`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessSpec {
    /// Short CLI id, e.g. `claude`.
    pub id: String,
    /// Human-readable name, e.g. `Claude Code`.
    pub name: String,
    /// The vendor that ships the CLI.
    ///
    /// For a CLI that runs its vendor's own models this is also the vendor of
    /// the model. A CLI that runs other vendors' models says so with
    /// [`HarnessSpec::multi_model`], and then only the model names a vendor.
    pub vendor: Vendor,
    /// Whether the CLI runs models from vendors other than its own.
    ///
    /// pi, opencode and kilo all run other vendors' models, so the spec's
    /// [`HarnessSpec::vendor`] says who ships the CLI and says nothing about
    /// the model. Defaults to `false`, the single-vendor CLI.
    #[serde(default)]
    pub multi_model: bool,
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
    /// The access modes the program offers, and which one is the default.
    ///
    /// Absent means the harness names no modes, and a role may not pick one.
    #[serde(default)]
    pub permissions: PermissionsSpec,
    /// How a brief reaches the program.
    ///
    /// Absent means the brief is the program's first message, which every
    /// supported program accepts.
    #[serde(default)]
    pub brief: BriefSpec,
    /// The arguments that switch the program's fast (higher-priority) mode.
    ///
    /// Absent means the program has no such switch, and a role may not ask for
    /// fast on it.
    #[serde(default)]
    pub fast: FastSpec,
    /// Where the program's status can be read, most trusted source first.
    ///
    /// Absent means the agent's own OSC 7501 report and then its screen.
    #[serde(default)]
    pub status: StatusSpec,
    /// Whether and how the program accepts a hook for its asks.
    ///
    /// Absent means the program takes no ask hook.
    #[serde(default)]
    pub asks: AskHookSpec,
    /// Case-insensitive screen substrings that mean rate limited or out of quota.
    ///
    /// A fallback, for CLIs that print an error without recording it. The
    /// watchdog prefers the record and the live retry signal, and reads these
    /// only from the bottom lines of a stopped agent's pane, so each entry is a
    /// whole phrase the CLI itself prints: a bare word like "quota" matches any
    /// file the agent happens to be reading.
    pub dry_patterns: Vec<String>,
    /// Prompts the CLI raises on its own and waits for a human to answer.
    ///
    /// Until one is answered the agent never reads a brief, so a launch that
    /// meets one says so instead of waiting forever. An absent table means the
    /// CLI raises no such prompt.
    #[serde(default)]
    pub blocking_prompts: Vec<BlockingPrompt>,
    /// Where one session's record is read as a chat transcript.
    ///
    /// Absent means shprd shows no conversation for this program: the chat
    /// panel says so rather than reading a record whose shape it does not know.
    #[serde(default)]
    pub transcript: TranscriptSpec,
}

/// One question the CLI asks its own user and blocks on until they answer.
///
/// Claude Code asks whether a folder is trusted the first time it starts in
/// it; shprd cannot answer that, so it only reports that the agent is waiting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockingPrompt {
    /// Short human-readable name, e.g. `folder trust`.
    pub name: String,
    /// Case-insensitive screen substring that means the prompt is showing.
    pub pattern: String,
}

impl HarnessSpec {
    /// Check the rules the type system cannot express.
    ///
    /// A spec that passes this has a [`RecordsSpec::location`] that only uses
    /// the documented placeholders, `home_dirs` entries that are home-relative
    /// (each starting with `~/`) and `project_dirs` entries that are relative
    /// to the repository root, launch templates that use the placeholders their
    /// field allows, blocking prompts with a name and a pattern, permission
    /// modes with unique ids and one of them the default, one model catalog
    /// source, effort levels a source uses, a brief delivery that matches the
    /// flags present, at least one status source, and ask-hook facts that say
    /// how a hook is installed or say the program takes none.
    pub fn validate(&self) -> Result<(), SpecError> {
        validate_location(&self.records.location)?;
        self.transcript.validate()?;

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

        for prompt in &self.blocking_prompts {
            if prompt.name.trim().is_empty() {
                return Err(SpecError::EmptyBlockingPromptName);
            }
            if prompt.pattern.trim().is_empty() {
                return Err(SpecError::EmptyBlockingPromptPattern {
                    name: prompt.name.clone(),
                });
            }
        }

        let launch = &self.launch;
        check_template("launch.interactive", &launch.interactive, &[], None)?;
        check_template(
            "launch.model_args",
            &launch.model_args,
            &["model"],
            Some("model"),
        )?;
        if let Some(tokens) = &launch.effort_args {
            check_template("launch.effort_args", tokens, &["effort"], Some("effort"))?;
        }
        check_optional_template("launch.auto_approve", &launch.auto_approve, &[])?;
        check_optional_template("launch.reviewer_args", &launch.reviewer_args, &[])?;
        check_optional_template(
            "launch.system_prompt_args",
            &launch.system_prompt_args,
            &["prompt", "prompt_file"],
        )?;
        check_optional_template("launch.resume_args", &launch.resume_args, &["session"])?;
        check_optional_template("detect.signin_args", &self.detect.signin_args, &["model"])?;
        self.permissions.validate()?;
        self.models.validate()?;
        // A level the launch cannot send is a level a role may pick and never
        // get, so the two facts have to agree: levels need a flag to carry them.
        if launch.effort_args.is_none() && !self.models.efforts.levels.is_empty() {
            return Err(SpecError::EffortsWithoutFlag);
        }
        self.brief.validate(launch)?;
        self.fast.validate()?;
        self.status.validate()?;
        self.asks.validate()?;
        Ok(())
    }
}

/// The vendor that ships an agent CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DetectSpec {
    /// The binary to look for on `PATH`.
    pub binary: String,
    /// Arguments that print the CLI version.
    pub version_args: Vec<String>,
    /// The command a user runs to sign in.
    pub signin_hint: String,
    /// Arguments that probe whether the CLI is signed in.
    ///
    /// They run after the binary and exit 0 when the user is signed in. A
    /// probe may use `{model}` when the answer depends on the model. It must
    /// never print a credential: the output is not shown.
    pub signin_args: Option<Vec<String>>,
}

/// Argument templates for starting the CLI and injecting session settings.
///
/// Every template is a list of argv tokens: each element reaches the CLI as one
/// argument, so a value with spaces stays one argument without quoting. A token
/// may contain at most one placeholder from [`Values`], e.g.
/// `model_reasoning_effort={effort}` or `{prompt_file}`. Use [`render_args`] to
/// fill a template in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Extra arguments that confine a reviewer's writes, never approve them.
    ///
    /// A reviewer runs beside a human and must not act unprompted, so these
    /// arguments may only narrow what its tools can reach — e.g. a sandbox that
    /// keeps writes inside its working directory. They never suppress prompts
    /// and, like [`Self::auto_approve`], may hold no placeholder.
    pub reviewer_args: Option<Vec<String>>,
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
///
/// The catalog has one source: asked of the program with [`Self::list_command`],
/// or read from a static list in `data/models/` named by [`Self::static_list`].
/// A spec that names neither has no catalog, and a model name it is given is
/// passed to the program as written.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelsSpec {
    /// The command that lists available models, if the CLI has one.
    #[serde(default)]
    pub list_command: Option<Vec<String>>,
    /// How to read that command's output.
    #[serde(default)]
    pub parser: CatalogParser,
    /// A static catalog in `data/models/`, as a bare file name, e.g. `claude.toml`.
    ///
    /// For a program with no list command (Claude Code), or one whose list the
    /// person cannot read at that moment.
    #[serde(default)]
    pub static_list: Option<String>,
    /// Whether a model name must also name its provider, as `pi` requires.
    #[serde(default)]
    pub provider_qualified: bool,
    /// Where the program's effort levels come from.
    #[serde(default)]
    pub efforts: EffortSpec,
}

impl ModelsSpec {
    /// What the catalog is read from.
    pub fn catalog_source(&self) -> CatalogSource {
        if self.list_command.is_some() {
            CatalogSource::Command
        } else if self.static_list.is_some() {
            CatalogSource::Static
        } else {
            CatalogSource::None
        }
    }

    /// Check the catalog: one source only, a list command for a parser that
    /// reads one, and a static list that is a bare file name in `data/models/`.
    fn validate(&self) -> Result<(), SpecError> {
        if self.list_command.is_some() && self.static_list.is_some() {
            return Err(SpecError::TwoCatalogSources);
        }
        if self.parser != CatalogParser::FirstToken && self.list_command.is_none() {
            return Err(SpecError::ParserWithoutListCommand {
                parser: self.parser,
            });
        }
        if let Some(name) = &self.static_list {
            let bare = !name.is_empty()
                && name.ends_with(".toml")
                && !name.contains('/')
                && !name.contains('\\')
                && !name.starts_with('~');
            if !bare {
                return Err(SpecError::InvalidStaticList { name: name.clone() });
            }
        }
        if self.efforts.source == EffortSource::None && !self.efforts.levels.is_empty() {
            return Err(SpecError::EffortsWithoutSource);
        }
        for level in &self.efforts.levels {
            if level.is_empty() || level.trim() != level {
                return Err(SpecError::InvalidEffortLevel {
                    level: level.clone(),
                });
            }
        }
        Ok(())
    }
}

/// What a model catalog is read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogSource {
    /// Asked of the program with a list command.
    Command,
    /// Read from a static list in `data/models/`.
    Static,
    /// The program has no catalog.
    None,
}

/// How a program's model list command prints its models.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogParser {
    /// One model per line, its name as the line's first word.
    #[default]
    FirstToken,
    /// Codex's catalog: JSON with a `models` array whose rows hold `slug`,
    /// `display_name`, `default_reasoning_level`, `supported_reasoning_levels`
    /// and `visibility`.
    CodexCatalog,
    /// pi's catalog: a table whose first line names the columns (`provider`,
    /// `model`, `context`, `max-out`, `thinking`, `images`) and whose rows are
    /// padded to those columns, so a provider name may hold a space.
    PiTable,
}

/// Where a program's effort levels come from.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffortSpec {
    /// The source of the levels.
    #[serde(default)]
    pub source: EffortSource,
    /// The levels this program takes, when the source is `static`, and the
    /// fallback when a catalog cannot be read.
    ///
    /// An empty list with [`EffortSource::Static`] means the program takes no
    /// effort level at all, which the settings UI must not offer.
    #[serde(default)]
    pub levels: Vec<String>,
}

/// Where the effort levels a program accepts are listed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffortSource {
    /// The program takes no effort level.
    #[default]
    None,
    /// The model catalog itself carries them, per model, as Codex's does.
    Catalog,
    /// The program's `--help` lists them, next to its effort flag.
    Help,
    /// [`EffortSpec::levels`] is the list.
    Static,
}

/// Where the CLI writes its conversation record and provider errors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

/// Where one session's record is read as a conversation, and how.
///
/// [`RecordsSpec`] says where a CLI writes its record so a reader can find it
/// again; this says how to turn that record into messages, which is a different
/// question: the watchdog only needs the newest error out of a record, while a
/// chat needs the person's words, the agent's words and a summary of its tools.
///
/// The location is a pattern with the same placeholders [`RecordsSpec`] allows,
/// plus `*` for any run of characters inside one component, so one session's
/// file can be named by the search rather than by a pid the service does not
/// have. A CLI that changes where it writes is a table change, not a code
/// change.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptSpec {
    /// How the record is read; absent means no reader, and no chat.
    pub format: Option<TranscriptFormat>,
    /// Where one session lives, as a path pattern.
    ///
    /// A file format names the session's file
    /// (`~/.claude/projects/{cwd_slug}/*.jsonl`); a store format names the
    /// directory the session's parts are kept under
    /// (`~/.local/share/opencode/storage`).
    pub location: Option<String>,
}

impl TranscriptSpec {
    /// Whether this program's conversation can be read at all.
    pub fn readable(&self) -> bool {
        self.format.is_some()
    }

    /// Check that a format and its location are stated together, and that the
    /// location uses only the placeholders [`RecordsSpec::location`] allows.
    pub fn validate(&self) -> Result<(), SpecError> {
        match (&self.format, &self.location) {
            (Some(_), Some(location)) => validate_location(location),
            (Some(_), None) => Err(SpecError::TranscriptWithoutLocation),
            (None, Some(_)) => Err(SpecError::LocationWithoutTranscript),
            (None, None) => Ok(()),
        }
    }
}

/// The on-disk shape of one session's record, as a chat reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptFormat {
    /// Claude Code's `~/.claude/projects/<slug>/<session>.jsonl`.
    ClaudeJsonl,
    /// Codex's `~/.codex/sessions/<date>/rollout-*.jsonl`.
    CodexJsonl,
    /// Pi's `~/.pi/agent/sessions/<slug>/<date>_<session>.jsonl`.
    PiJsonl,
    /// OpenCode's storage folder: one JSON file a session, one a message, and a
    /// folder of parts for each message. OpenCode up to 1.18 wrote this; from
    /// 1.18 on the same tables live in [`Self::OpencodeSqlite`].
    OpencodeStorage,
    /// OpenCode's database: `session`, `message` and `part` tables in one SQLite
    /// file, which is what 1.18.35 writes and what a chat reads.
    OpencodeSqlite,
}

/// Placeholders allowed in a [`RecordsSpec::location`] pattern.
const LOCATION_PLACEHOLDERS: [&str; 6] = ["cwd_slug", "session", "yyyy", "mm", "dd", "data_home"];

/// Check a record location pattern against the placeholder contract.
///
/// Allowed placeholders are `{cwd_slug}`, `{session}`, `{yyyy}`, `{mm}`,
/// `{dd}` and `{data_home}`; `*` may stand for any run of characters within a
/// single path component; `~` may appear only as the first component, where it
/// means the user's home directory; and `{data_home}` may only be the first
/// component, where it means the XDG data home.
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
    // `{data_home}` is a root, like `~`: anywhere else it would name a
    // directory under another one, which is not a thing a CLI has.
    if location
        .match_indices("{data_home}")
        .any(|(index, _)| index != 0)
    {
        return Err(SpecError::DataHomeNotLeading {
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RecordKind {
    /// Newline-delimited JSON.
    Jsonl,
    /// A SQLite database.
    Sqlite,
}

/// Where the CLI exposes quota information.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuotaSpec {
    /// The quota source for this CLI.
    pub source: QuotaSource,
    /// Where live retry status appears, if the CLI reports it.
    pub retry_signal: Option<String>,
}

/// The quota source for an agent CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillsSpec {
    /// Directories under the user's home, each written with a leading `~/`.
    pub home_dirs: Vec<String>,
    /// Directories relative to the repository root.
    pub project_dirs: Vec<String>,
}

/// The access modes a program offers, and which one a role gets.
///
/// The ids are the program's own words for the modes, mapped from Shepherdr's
/// `perm` values (M7c 2.2): `manual`, `auto`, `acceptEdits`, `plan` and
/// `bypassPermissions` for Claude Code, `read-only`, `auto` and `full` for
/// Codex. Keeping each program's own id lets the settings UI show the words a
/// person already knows, and [`Self::across`] carries a choice over when a role
/// switches program.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionsSpec {
    /// The mode a role gets when it names none; one of [`Self::modes`].
    #[serde(default)]
    pub default: Option<String>,
    /// The modes, in the order the settings UI lists them.
    #[serde(default)]
    pub modes: Vec<PermissionMode>,
    /// The mode that counts as full access, for the cross-program carry.
    #[serde(default)]
    pub full: Option<String>,
    /// The mode a reviewer runs in: nothing is approved on its own.
    #[serde(default)]
    pub readonly: Option<String>,
    /// This program's nearest mode to one of another program's.
    ///
    /// Each key is `<program>:<mode>`, e.g. `claude:bypassPermissions`, and each
    /// value is one of this program's own [`Self::modes`] ids. It is what keeps
    /// a role on "auto" when it moves from Claude Code to Codex, instead of
    /// silently dropping to ask (Shepherdr's `PERM_ACROSS`).
    #[serde(default)]
    pub across: BTreeMap<String, String>,
}

impl PermissionsSpec {
    /// The mode called `id`, if this program has one.
    pub fn mode(&self, id: &str) -> Option<&PermissionMode> {
        self.modes.iter().find(|mode| mode.id == id)
    }

    /// The mode a role gets when it names none.
    pub fn default_mode(&self) -> Option<&PermissionMode> {
        self.default.as_deref().and_then(|id| self.mode(id))
    }

    /// The mode that counts as full access.
    pub fn full_mode(&self) -> Option<&PermissionMode> {
        self.full.as_deref().and_then(|id| self.mode(id))
    }

    /// The mode a reviewer runs in.
    pub fn readonly_mode(&self) -> Option<&PermissionMode> {
        self.readonly.as_deref().and_then(|id| self.mode(id))
    }

    /// This program's mode for `<program>:<mode>`, if it names one.
    pub fn across_mode(&self, from: &str) -> Option<&PermissionMode> {
        self.across.get(from).and_then(|id| self.mode(id))
    }

    /// Check the modes, and every field that names one.
    fn validate(&self) -> Result<(), SpecError> {
        for (index, mode) in self.modes.iter().enumerate() {
            if mode.id.trim().is_empty() {
                return Err(SpecError::EmptyPermissionId);
            }
            if mode.label.trim().is_empty() {
                return Err(SpecError::EmptyPermissionLabel {
                    id: mode.id.clone(),
                });
            }
            check_template("permissions.modes.args", &mode.args, &[], None)?;
            if self.modes[..index]
                .iter()
                .any(|earlier| earlier.id == mode.id)
            {
                return Err(SpecError::DuplicatePermissionMode {
                    id: mode.id.clone(),
                });
            }
        }
        for (field, value) in [
            ("permissions.default", &self.default),
            ("permissions.full", &self.full),
            ("permissions.readonly", &self.readonly),
        ] {
            if let Some(id) = value
                && self.mode(id).is_none()
            {
                return Err(SpecError::UnknownPermissionMode {
                    field,
                    id: id.clone(),
                });
            }
        }
        if !self.modes.is_empty() && self.default.is_none() {
            return Err(SpecError::PermissionDefaultMissing);
        }
        for (from, id) in &self.across {
            if from.starts_with(':') || from.ends_with(':') || !from.contains(':') {
                return Err(SpecError::InvalidAcrossKey { key: from.clone() });
            }
            if self.mode(id).is_none() {
                return Err(SpecError::UnknownPermissionMode {
                    field: "permissions.across",
                    id: id.clone(),
                });
            }
        }
        Ok(())
    }
}

/// One access mode a program offers, with the arguments that select it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionMode {
    /// The program's own word for the mode, e.g. `auto` or `read-only`.
    pub id: String,
    /// The words a person sees, e.g. `Auto` or `Read only`.
    pub label: String,
    /// The argv tokens that select the mode, e.g. `["--sandbox", "read-only"]`.
    ///
    /// Tokens are appended to the launch argv as they are: no placeholder.
    pub args: Vec<String>,
}

/// How a brief reaches the program.
///
/// Every supported program takes the brief as its first turn; a program with a
/// system-prompt file flag takes it there instead, so a long brief is never one
/// argv argument that a terminal has to hold.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BriefSpec {
    /// Which of the two ways this program takes its brief.
    #[serde(default)]
    pub delivery: BriefDelivery,
    /// The flag that carries the brief, when it is a system-prompt file.
    ///
    /// Defaults to [`LaunchSpec::system_prompt_args`], which is where the flag
    /// has always lived; set this only when the brief flag is not that one. One
    /// token must hold `{prompt_file}`.
    #[serde(default)]
    pub args: Option<Vec<String>>,
    /// Whether the flag takes the file's path or its contents.
    #[serde(default)]
    pub file: BriefFileMode,
}

/// How a brief reaches the program: first message or system-prompt file.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BriefDelivery {
    /// The brief is the program's first turn, typed into its pane.
    ///
    /// Every program that takes a prompt does; this is the default. It is not an
    /// argv argument, because a brief is multi-line and a terminal run cannot
    /// carry a newline in one argument.
    #[default]
    FirstMessage,
    /// The brief is written to a file the program is pointed at.
    SystemPromptFile,
}

/// How a brief's file reaches the program's argument.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BriefFileMode {
    /// The argument is the file's path, which the program reads itself.
    #[default]
    Path,
    /// The argument is the file's own contents, which the adapter reads in.
    Contents,
}

impl BriefSpec {
    /// The template that carries this program's brief, if it carries a file.
    ///
    /// [`Self::args`] wins when it is set; otherwise the flag is
    /// [`LaunchSpec::system_prompt_args`], where it has always lived.
    pub fn args<'a>(&'a self, launch: &'a LaunchSpec) -> Option<&'a [String]> {
        self.args
            .as_deref()
            .or(launch.system_prompt_args.as_deref())
    }

    /// Check that the delivery and the flags agree: a first-message brief has
    /// no flag, and a system-prompt brief names exactly one placeholder, the one
    /// its [`Self::file`] mode carries (`{prompt_file}` for a path, `{prompt}`
    /// for the contents).
    fn validate(&self, launch: &LaunchSpec) -> Result<(), SpecError> {
        match self.delivery {
            BriefDelivery::FirstMessage => {
                let stray = if self.args.is_some() {
                    Some("brief.args")
                } else if launch.system_prompt_args.is_some() {
                    Some("launch.system_prompt_args")
                } else {
                    None
                };
                match stray {
                    Some(field) => Err(SpecError::BriefFlagWithoutFile { field }),
                    None => Ok(()),
                }
            }
            BriefDelivery::SystemPromptFile => {
                let Some(template) = self.args(launch) else {
                    return Err(SpecError::BriefFileWithoutFlag);
                };
                let required = match self.file {
                    BriefFileMode::Path => "prompt_file",
                    BriefFileMode::Contents => "prompt",
                };
                check_template(
                    "brief.args",
                    template,
                    &["prompt", "prompt_file"],
                    Some(required),
                )
            }
        }
    }
}

/// The arguments that switch a program's fast (higher-priority) mode.
///
/// Fast is a property of the service tier a program asks its provider for, not
/// of the model or the effort, so each program that has one says how to turn it
/// on and how to turn it off again: a role that names `fast = false` must be
/// able to win over a person's own config that leaves it on (Shepherdr's
/// `argvFor` always writes both, M7c 2.2). A program that has no such switch
/// leaves the section out.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FastSpec {
    /// The arguments that turn fast mode on.
    #[serde(default)]
    pub on: Vec<String>,
    /// The arguments that turn it off again, when the program has to be told.
    #[serde(default)]
    pub off: Vec<String>,
}

impl FastSpec {
    /// Whether the program has a fast switch at all.
    pub fn available(&self) -> bool {
        !self.on.is_empty()
    }

    /// The arguments for a role that asked for `fast` (or did not).
    ///
    /// A program that is never told to turn fast off has no `off` arguments and
    /// gets none, so turning fast on is the only thing it ever hears about.
    pub fn args(&self, fast: bool) -> &[String] {
        if fast { &self.on } else { &self.off }
    }

    /// Check that a program that can be told to turn fast off can be told to
    /// turn it on too.
    ///
    /// The arguments are literal tokens, not templates: no placeholder is
    /// filled into them, because what a program wants is a whole value of its
    /// own (`{"fastMode":true}` for Claude Code), braces and all.
    fn validate(&self) -> Result<(), SpecError> {
        if self.on.is_empty() && !self.off.is_empty() {
            return Err(SpecError::FastOffWithoutOn);
        }
        Ok(())
    }
}

/// Where a program's status can be read, most trusted source first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatusSpec {
    /// The sources, in order: the first one that answers wins.
    ///
    /// The default is the agent's own report and then its screen, which every
    /// program has; `hook` is for a program whose hook reports its state.
    #[serde(default = "default_status_sources")]
    pub sources: Vec<StatusSource>,
}

impl Default for StatusSpec {
    fn default() -> Self {
        Self {
            sources: default_status_sources(),
        }
    }
}

/// The status sources a program that says nothing about it is read from.
fn default_status_sources() -> Vec<StatusSource> {
    vec![StatusSource::Osc7501, StatusSource::Screen]
}

impl StatusSpec {
    /// Check that there is at least one source and no source twice.
    fn validate(&self) -> Result<(), SpecError> {
        if self.sources.is_empty() {
            return Err(SpecError::EmptyStatusSources);
        }
        for (index, source) in self.sources.iter().enumerate() {
            if self.sources[..index].contains(source) {
                return Err(SpecError::DuplicateStatusSource {
                    name: source.as_str().to_owned(),
                });
            }
        }
        Ok(())
    }
}

/// One place a program's status can be read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusSource {
    /// The status the agent reports itself, as OSC 7501 on its terminal.
    Osc7501,
    /// A hook or plugin the program runs for shprd.
    Hook,
    /// The bottom lines of the agent's pane.
    Screen,
}

impl StatusSource {
    /// The source's name, as a harness file writes it.
    pub fn as_str(self) -> &'static str {
        match self {
            StatusSource::Osc7501 => "osc7501",
            StatusSource::Hook => "hook",
            StatusSource::Screen => "screen",
        }
    }
}

/// Whether and how a program accepts a hook for its asks.
///
/// An ask is a question, a permission or a plan the program waits on: while it
/// waits the agent reads no brief and answers nothing, so the person has to be
/// shown it (M9.5). Only the schema lives here; the installers are adapters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AskHookSpec {
    /// Whether the program has a hook that can surface an ask at all.
    #[serde(default)]
    pub supported: bool,
    /// The config file the adapter writes the hook into, relative to the task's
    /// worktree (`.claude/settings.local.json`, `.codex/hooks.json`).
    ///
    /// Never a `~/...` path: a task's hook belongs to the task, and shprd never
    /// edits the person's global config.
    #[serde(default)]
    pub config: Option<String>,
    /// The file's shape, which picks the installer.
    #[serde(default)]
    pub format: AskHookConfig,
    /// The hook command, with `{hook}` where the hook executable's path goes.
    ///
    /// The adapter answers the rest of the command, e.g. `sh {hook}` or
    /// `bash '{hook}' perm`.
    #[serde(default)]
    pub command: Option<String>,
    /// The program's events that fire an ask, one per [`AskKind`].
    #[serde(default)]
    pub events: Vec<AskEvent>,
    /// How long the program lets the hook wait for an answer, if it says.
    #[serde(default)]
    pub timeout_seconds: Option<u32>,
}

impl AskHookSpec {
    /// The program's event for `kind`, if it fires one.
    pub fn event_for(&self, kind: AskKind) -> Option<&AskEvent> {
        self.events.iter().find(|event| event.kind == kind)
    }

    /// Check the installer facts: a supported program says where its hook goes
    /// and how it is written there; an unsupported one says nothing else.
    fn validate(&self) -> Result<(), SpecError> {
        if !self.supported {
            let stray = if self.config.is_some() {
                Some("config")
            } else if self.format != AskHookConfig::Unsupported {
                Some("format")
            } else if self.command.is_some() {
                Some("command")
            } else if !self.events.is_empty() {
                Some("events")
            } else if self.timeout_seconds.is_some() {
                Some("timeout_seconds")
            } else {
                None
            };
            return match stray {
                Some(field) => Err(SpecError::AskHookWithoutSupport { field }),
                None => Ok(()),
            };
        }

        let Some(config) = &self.config else {
            return Err(SpecError::IncompleteAskHook { missing: "config" });
        };
        let relative = Path::new(config);
        let outside = config.is_empty()
            || config.starts_with('~')
            || relative.is_absolute()
            || relative.components().any(|part| {
                matches!(
                    part,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            });
        if outside {
            return Err(SpecError::InvalidAskConfig {
                config: config.clone(),
            });
        }
        if self.format == AskHookConfig::Unsupported {
            return Err(SpecError::IncompleteAskHook { missing: "format" });
        }
        let Some(command) = &self.command else {
            return Err(SpecError::IncompleteAskHook { missing: "command" });
        };
        if command.matches("{hook}").count() != 1 {
            return Err(SpecError::InvalidAskCommand {
                command: command.clone(),
            });
        }
        if self.events.is_empty() {
            return Err(SpecError::IncompleteAskHook { missing: "events" });
        }
        for (index, event) in self.events.iter().enumerate() {
            if event.event.trim().is_empty() {
                return Err(SpecError::EmptyAskEvent { kind: event.kind });
            }
            if self.events[..index]
                .iter()
                .any(|earlier| earlier.kind == event.kind)
            {
                return Err(SpecError::DuplicateAskKind { kind: event.kind });
            }
        }
        Ok(())
    }
}

/// The shape of the config file an ask hook is installed into.
///
/// The adapter owns the exact JSON (or TOML) it writes; the harness only says
/// which program's file and layout to use.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AskHookConfig {
    /// The program takes no ask hook.
    #[default]
    Unsupported,
    /// Claude Code's worktree-local `.claude/settings.local.json`:
    /// `hooks.<Event>[].hooks[]`.
    ClaudeSettings,
    /// Codex's worktree-local `.codex/hooks.json`: the same shape in its own
    /// file, with `.codex/config.toml` turning hooks on.
    CodexHooks,
}

/// One ask a program's hook fires for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AskEvent {
    /// The ask this event fires for.
    pub kind: AskKind,
    /// The program's own event name, e.g. `PermissionRequest`.
    pub event: String,
    /// The matcher the program applies to the event, if it takes one.
    #[serde(default)]
    pub matcher: Option<String>,
}

/// The asks shprd shows a person, each as its own card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AskKind {
    /// The agent asks a question and waits for an answer.
    Question,
    /// The agent asks permission to run a tool.
    Permission,
    /// The agent offers a plan and waits for approval.
    Plan,
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
    /// A record location uses `{data_home}` anywhere but its first component.
    #[error("location {location} uses `{{data_home}}` outside its first path component")]
    DataHomeNotLeading {
        /// The location pattern that failed.
        location: String,
    },
    /// A loaded harness spec failed the checks in [`HarnessSpec::validate`].
    #[error("invalid harness spec {path}: {source}")]
    Invalid {
        /// The file that failed.
        path: PathBuf,
        /// The validation error.
        #[source]
        source: Box<SpecError>,
    },
    /// Two harness spec files declare the same `id`.
    #[error("duplicate harness id {id} in {path}: already defined in {first}")]
    DuplicateId {
        /// The repeated id.
        id: String,
        /// The file that repeats it.
        path: PathBuf,
        /// The file that declared it first.
        first: PathBuf,
    },
    /// A launch argument template uses a placeholder it may not or must use.
    #[error("invalid {field} template: {reason}")]
    InvalidTemplate {
        /// The launch field that failed, e.g. `launch.model_args`.
        field: &'static str,
        /// What is wrong with it.
        reason: String,
    },
    /// An argument template has an unmatched `{` or `}`.
    #[error("unmatched brace in template token {token}")]
    UnmatchedBrace {
        /// The template token that failed.
        token: String,
    },
    /// A transcript format is stated without a location to read.
    #[error("transcript.format is set but transcript.location is not")]
    TranscriptWithoutLocation,
    /// A transcript location is stated without a format to read it with.
    #[error("transcript.location is set but transcript.format is not")]
    LocationWithoutTranscript,
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
    /// A [`BlockingPrompt`] has an empty name.
    #[error("blocking prompt name is empty")]
    EmptyBlockingPromptName,
    /// A [`BlockingPrompt`] has an empty screen pattern.
    #[error("blocking prompt {name} has an empty pattern")]
    EmptyBlockingPromptPattern {
        /// The prompt whose pattern is empty.
        name: String,
    },
    /// A permission mode has an empty id.
    #[error("permission mode id is empty")]
    EmptyPermissionId,
    /// A permission mode has an empty label.
    #[error("permission mode {id} has an empty label")]
    EmptyPermissionLabel {
        /// The mode whose label is empty.
        id: String,
    },
    /// The same permission mode id is declared twice.
    #[error("duplicate permission mode {id}")]
    DuplicatePermissionMode {
        /// The repeated id.
        id: String,
    },
    /// A permission field names a mode the program does not declare.
    #[error("{field} names permission mode {id}, which the harness does not declare")]
    UnknownPermissionMode {
        /// The field that names the mode, e.g. `permissions.default`.
        field: &'static str,
        /// The mode id that was named.
        id: String,
    },
    /// A harness declares modes but does not say which one is the default.
    #[error("permissions.modes is set but permissions.default is not")]
    PermissionDefaultMissing,
    /// A cross-program permission key is not `<program>:<mode>`.
    #[error("permissions.across key {key} is not `<program>:<mode>`")]
    InvalidAcrossKey {
        /// The key that is shaped wrong.
        key: String,
    },
    /// A harness names both a list command and a static model catalog.
    #[error("models names both a list command and a static list: pick one source")]
    TwoCatalogSources,
    /// A catalog parser is set without a list command to parse.
    #[error("models.parser {parser:?} needs models.list_command")]
    ParserWithoutListCommand {
        /// The parser that was set.
        parser: CatalogParser,
    },
    /// A static model catalog is not a bare file name in `data/models/`.
    #[error(
        "invalid static model list {name}: use a bare file name in data/models/, e.g. `claude.toml`"
    )]
    InvalidStaticList {
        /// The name that was written.
        name: String,
    },
    /// Effort levels are listed without a source that uses them.
    #[error("models.efforts.levels is set but models.efforts.source is `none`")]
    EffortsWithoutSource,
    /// An effort level is empty or padded with whitespace.
    #[error("invalid effort level `{level}`")]
    InvalidEffortLevel {
        /// The level that was written.
        level: String,
    },
    /// A program is told how to turn fast off but not how to turn it on.
    #[error("fast.off is set but fast.on is empty")]
    FastOffWithoutOn,
    /// A program lists effort levels but has no flag that sends one.
    #[error("models.efforts lists levels but launch.effort_args is not set")]
    EffortsWithoutFlag,
    /// A program takes its brief as its first message but names a brief flag.
    #[error("brief.delivery is first-message but {field} names a system-prompt flag")]
    BriefFlagWithoutFile {
        /// The field that holds the stray flag.
        field: &'static str,
    },
    /// A program takes its brief as a system-prompt file but names no flag.
    #[error(
        "brief.delivery is system-prompt-file but neither brief.args nor launch.system_prompt_args names the flag"
    )]
    BriefFileWithoutFlag,
    /// A harness names no status source at all.
    #[error("status.sources is empty: name at least one source")]
    EmptyStatusSources,
    /// The same status source is listed twice.
    #[error("duplicate status source {name}")]
    DuplicateStatusSource {
        /// The repeated source, by its serde name.
        name: String,
    },
    /// An unsupported ask hook carries installer details anyway.
    #[error("asks.supported is false but asks.{field} is set")]
    AskHookWithoutSupport {
        /// The field a supported program would need.
        field: &'static str,
    },
    /// A supported ask hook leaves out a fact its installer needs.
    #[error("asks.supported is true but asks.{missing} is missing")]
    IncompleteAskHook {
        /// The field that is missing.
        missing: &'static str,
    },
    /// The ask hook's config file is not home-relative.
    #[error("invalid ask hook config {config}: use a leading `~/` path")]
    InvalidAskConfig {
        /// The path that was written.
        config: String,
    },
    /// The ask hook command does not hold exactly one `{hook}`.
    #[error("invalid ask hook command {command}: it must hold `{{hook}}` exactly once")]
    InvalidAskCommand {
        /// The command that was written.
        command: String,
    },
    /// An ask event has an empty program event name.
    #[error("the {kind:?} ask event has an empty event name")]
    EmptyAskEvent {
        /// The ask kind whose event is empty.
        kind: AskKind,
    },
    /// The same ask kind is declared twice.
    #[error("the {kind:?} ask kind is declared twice")]
    DuplicateAskKind {
        /// The repeated kind.
        kind: AskKind,
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

/// The placeholder names in one template token, rejecting malformed braces.
fn template_placeholders(token: &str) -> Result<Vec<&str>, SpecError> {
    let mut names = Vec::new();
    let mut rest = token;
    loop {
        let Some(start) = rest.find('{') else {
            if rest.contains('}') {
                return Err(SpecError::UnmatchedBrace {
                    token: token.to_owned(),
                });
            }
            return Ok(names);
        };
        if rest[..start].contains('}') {
            return Err(SpecError::UnmatchedBrace {
                token: token.to_owned(),
            });
        }
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            return Err(SpecError::UnmatchedBrace {
                token: token.to_owned(),
            });
        };
        names.push(&after[..end]);
        rest = &after[end + 1..];
    }
}

/// Check one launch template: at most one placeholder per token, only the
/// placeholders in `allowed`, and `required` exactly once across the template.
fn check_template(
    field: &'static str,
    tokens: &[String],
    allowed: &[&str],
    required: Option<&str>,
) -> Result<(), SpecError> {
    let mut required_count = 0usize;
    for token in tokens {
        let names = template_placeholders(token)?;
        if names.len() > 1 {
            return Err(SpecError::InvalidTemplate {
                field,
                reason: format!(
                    "token `{token}` has {} placeholders; use at most one",
                    names.len()
                ),
            });
        }
        for name in names {
            if !allowed.contains(&name) {
                return Err(SpecError::InvalidTemplate {
                    field,
                    reason: format!("token `{token}` uses {{{name}}}, which is not allowed here"),
                });
            }
            if Some(name) == required {
                required_count += 1;
            }
        }
    }
    if let Some(required) = required
        && required_count != 1
    {
        return Err(SpecError::InvalidTemplate {
            field,
            reason: format!("must use {{{required}}} exactly once, found {required_count}"),
        });
    }
    Ok(())
}

/// Check an optional launch template, ignoring a field the spec leaves out.
fn check_optional_template(
    field: &'static str,
    tokens: &Option<Vec<String>>,
    allowed: &[&str],
) -> Result<(), SpecError> {
    match tokens {
        Some(tokens) => check_template(field, tokens, allowed, None),
        None => Ok(()),
    }
}

/// Load every `*.toml` harness spec in `path`, sorted by file name.
///
/// Every spec is checked with [`HarnessSpec::validate`]; a spec that fails, or
/// two specs that declare the same [`HarnessSpec::id`], are rejected with an
/// error naming the offending file.
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

    let mut loaded = Vec::with_capacity(files.len());
    for file in files {
        let text = fs::read_to_string(&file).map_err(|source| SpecError::ReadFile {
            path: file.clone(),
            source,
        })?;
        loaded.push((file, text));
    }
    load_entries(loaded)
}

/// Check harness files already read in, each with the path it came from.
///
/// This is [`load_dir`] for data that is not a directory on this machine: the
/// copy embedded in the binary, or a person's overrides, both of which
/// [`crate::data::DataDir`] reads into text first. The entries are checked in
/// the order given, so a caller that lists them sorted keeps the same order of
/// refusals a directory read would give.
pub(crate) fn load_entries(entries: Vec<(PathBuf, String)>) -> Result<Vec<HarnessSpec>, SpecError> {
    let mut specs = Vec::with_capacity(entries.len());
    let mut seen: BTreeMap<String, PathBuf> = BTreeMap::new();
    for (path, text) in entries {
        let spec = parse(&path, &text)?;
        spec.validate().map_err(|source| SpecError::Invalid {
            path: path.clone(),
            source: Box::new(source),
        })?;
        if let Some(first) = seen.get(&spec.id) {
            return Err(SpecError::DuplicateId {
                id: spec.id.clone(),
                path: path.clone(),
                first: first.clone(),
            });
        }
        seen.insert(spec.id.clone(), path.clone());
        specs.push(spec);
    }
    Ok(specs)
}

/// Parse one harness file's text, naming `path` in any error.
pub(crate) fn parse(path: &Path, text: &str) -> Result<HarnessSpec, SpecError> {
    toml::from_str(text).map_err(|source| SpecError::Parse {
        path: path.to_owned(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        AskHookSpec, AskKind, BriefDelivery, BriefFileMode, CatalogSource, EffortSource, FastSpec,
        HarnessSpec, PermissionsSpec, SpecError, StatusSource, Values, load_dir, render_args,
        validate_location,
    };
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
effort_args = ["--effort", "{effort}"]
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

    /// A dry pattern is read off an agent's screen, which is mostly the content
    /// it is reading: a bare word like "quota" or a status code like "429"
    /// matches shprd's own source, so every shipped pattern must be a phrase
    /// (two or more words) or an API error identifier.
    #[test]
    fn every_dry_pattern_is_a_phrase_not_a_bare_word() {
        let specs = match load_dir(&harness_dir()) {
            Ok(specs) => specs,
            Err(error) => panic!("harness data should load: {error}"),
        };
        for spec in &specs {
            for pattern in &spec.dry_patterns {
                let words = pattern.split_whitespace().count();
                assert!(
                    words >= 2 || pattern.contains('_'),
                    "{}: the dry pattern `{pattern}` is a bare word, which any \
                     file the agent reads can match",
                    spec.id
                );
            }
        }
    }

    /// herdr refuses any argument containing a newline, so a brief may only
    /// reach a CLI as a file path: no shipped spec may pass `{prompt}`.
    #[test]
    fn no_shipped_spec_passes_a_multi_line_brief_as_an_argument() {
        let specs = match load_dir(&harness_dir()) {
            Ok(specs) => specs,
            Err(error) => panic!("harness data should load: {error}"),
        };
        let values = Values {
            prompt: Some("line one\nline two\n".to_owned()),
            prompt_file: Some("/tmp/brief.md".to_owned()),
            ..Values::default()
        };
        for spec in &specs {
            let Some(template) = &spec.launch.system_prompt_args else {
                continue;
            };
            let rendered = match render_args(template, &values) {
                Ok(rendered) => rendered,
                Err(error) => panic!("{} should render: {error}", spec.id),
            };
            for arg in rendered {
                assert!(
                    !arg.contains('\n'),
                    "{} passes a multi-line brief as an argument: {arg:?}",
                    spec.id
                );
            }
        }
    }

    /// Codex 0.164 opens an "Update available!" screen before it accepts a
    /// turn, so every codex launch — worker and reviewer, both of which start
    /// from `launch.interactive` — has to switch the update check off.
    #[test]
    fn every_codex_launch_skips_the_update_check() {
        let specs = match load_dir(&harness_dir()) {
            Ok(specs) => specs,
            Err(error) => panic!("harness data should load: {error}"),
        };
        let Some(codex) = specs.iter().find(|spec| spec.id == "codex") else {
            panic!("the codex spec should load");
        };
        let wanted = tokens(&["-c", "check_for_update_on_startup=false"]);
        assert!(
            codex
                .launch
                .interactive
                .windows(2)
                .any(|pair| pair == wanted.as_slice()),
            "codex still checks for updates at startup: {:?}",
            codex.launch.interactive
        );
    }

    /// If a codex build ignores the flag, the update screen still has to be
    /// reported as a blocking prompt rather than swallowing the brief.
    #[test]
    fn codex_declares_its_update_screen_as_a_blocking_prompt() {
        let specs = match load_dir(&harness_dir()) {
            Ok(specs) => specs,
            Err(error) => panic!("harness data should load: {error}"),
        };
        let Some(codex) = specs.iter().find(|spec| spec.id == "codex") else {
            panic!("the codex spec should load");
        };
        let found = codex.blocking_prompts.iter().any(|prompt| {
            prompt.name == "update available" && prompt.pattern == "Update available!"
        });
        assert!(
            found,
            "codex does not declare its update screen: {:?}",
            codex.blocking_prompts
        );
    }

    fn parse_fixture(fixture: &str) -> HarnessSpec {
        match toml::from_str::<HarnessSpec>(fixture) {
            Ok(spec) => spec,
            Err(error) => panic!("fixture should parse: {error}"),
        }
    }

    /// `MINIMAL_SPEC` plus each of the sections M9 adds, filled in.
    fn full_spec_text() -> String {
        format!(
            "{MINIMAL_SPEC}
[permissions]
default = \"auto\"
full = \"full\"
readonly = \"read-only\"
modes = [
  {{ id = \"read-only\", label = \"Read only\", args = [\"-s\", \"read-only\"] }},
  {{ id = \"auto\", label = \"Auto\", args = [\"-s\", \"workspace-write\"] }},
  {{ id = \"full\", label = \"Full access\", args = [\"--yolo\"] }},
]

[permissions.across]
\"claude:bypassPermissions\" = \"full\"
\"claude:plan\" = \"read-only\"

[brief]
delivery = \"system-prompt-file\"
args = [\"--sys\", \"{{prompt_file}}\"]
file = \"path\"

[status]
sources = [\"hook\", \"osc7501\", \"screen\"]

[fast]
on = [\"--fast\"]
off = [\"--no-fast\"]

[asks]
supported = true
config = \".fake/hooks.json\"
format = \"codex_hooks\"
command = \"sh '{{hook}}'\"
timeout_seconds = 60

[[asks.events]]
kind = \"permission\"
event = \"PermissionRequest\"

[[asks.events]]
kind = \"plan\"
event = \"PlanRequest\"
matcher = \"^plan$\"

[models.efforts]
source = \"static\"
levels = [\"low\", \"high\"]
"
        )
    }

    fn full_spec_error(fixture: &str) -> SpecError {
        let spec = parse_fixture(fixture);
        match spec.validate() {
            Ok(()) => panic!("the spec should be refused"),
            Err(error) => error,
        }
    }

    #[test]
    fn the_new_sections_default_when_the_file_leaves_them_out() {
        let spec = parse_fixture(MINIMAL_SPEC);
        assert!(spec.validate().is_ok(), "the defaults should validate");
        assert_eq!(spec.permissions, PermissionsSpec::default());
        assert!(spec.permissions.modes.is_empty());
        assert_eq!(spec.permissions.default_mode(), None);
        assert_eq!(spec.brief.delivery, BriefDelivery::FirstMessage);
        assert_eq!(spec.brief.file, BriefFileMode::Path);
        assert_eq!(spec.brief.args(&spec.launch), None);
        assert_eq!(
            spec.status.sources,
            [StatusSource::Osc7501, StatusSource::Screen]
        );
        assert_eq!(spec.asks, AskHookSpec::default());
        assert!(!spec.asks.supported);
        assert_eq!(spec.fast, FastSpec::default());
        assert!(!spec.fast.available());
        assert!(spec.fast.args(true).is_empty());
        assert!(spec.fast.args(false).is_empty());
        assert_eq!(spec.models.catalog_source(), CatalogSource::None);
        assert_eq!(spec.models.efforts.source, EffortSource::None);
    }

    #[test]
    fn every_new_section_refuses_an_unknown_key_by_name() {
        for section in ["permissions", "brief", "status", "asks", "fast"] {
            let fixture = MINIMAL_SPEC.replacen(
                "[models]",
                &format!("[{section}]\nmystery = true\n\n[models]"),
                1,
            );
            let error = match toml::from_str::<HarnessSpec>(&fixture) {
                Ok(_) => panic!("an unknown {section} key should be refused"),
                Err(error) => error,
            };
            assert!(
                error.to_string().contains("mystery"),
                "{section}: the error should name the key: {error}"
            );
        }
    }

    #[test]
    fn every_new_section_round_trips() {
        let spec = parse_fixture(&full_spec_text());
        assert!(spec.validate().is_ok(), "the full fixture should validate");
        let text = match toml::to_string(&spec) {
            Ok(text) => text,
            Err(error) => panic!("the spec should serialize: {error}"),
        };
        let again = parse_fixture(&text);
        assert_eq!(again, spec, "re-parsing the written spec should match");
        assert_eq!(again.permissions.modes.len(), 3);
        assert_eq!(
            again
                .permissions
                .across_mode("claude:plan")
                .map(|mode| &mode.id),
            Some(&"read-only".to_owned())
        );
        assert_eq!(again.brief.delivery, BriefDelivery::SystemPromptFile);
        assert_eq!(again.brief.file, BriefFileMode::Path);
        assert_eq!(
            again.status.sources,
            [
                StatusSource::Hook,
                StatusSource::Osc7501,
                StatusSource::Screen
            ]
        );
        assert_eq!(again.asks.events.len(), 2);
        assert_eq!(
            again
                .asks
                .event_for(AskKind::Plan)
                .map(|event| &event.matcher),
            Some(&Some("^plan$".to_owned()))
        );
        assert_eq!(again.models.efforts.levels, ["low", "high"]);
        assert_eq!(
            again.fast,
            FastSpec {
                on: tokens(&["--fast"]),
                off: tokens(&["--no-fast"]),
            }
        );
        assert_eq!(again.fast.args(true), tokens(&["--fast"]));
        assert_eq!(again.fast.args(false), tokens(&["--no-fast"]));
    }

    #[test]
    fn fast_off_args_need_fast_on_args() {
        let fixture = full_spec_text().replace("on = [\"--fast\"]\n", "");
        assert!(matches!(
            full_spec_error(&fixture),
            SpecError::FastOffWithoutOn
        ));
    }

    #[test]
    fn a_permission_field_must_name_a_declared_mode() {
        let fixture = full_spec_text().replace("default = \"auto\"", "default = \"ask\"");
        assert!(matches!(
            full_spec_error(&fixture),
            SpecError::UnknownPermissionMode {
                field: "permissions.default",
                ..
            }
        ));

        let fixture = full_spec_text().replace("full = \"full\"", "full = \"everything\"");
        assert!(matches!(
            full_spec_error(&fixture),
            SpecError::UnknownPermissionMode {
                field: "permissions.full",
                ..
            }
        ));

        let fixture = full_spec_text().replace(
            "\"claude:plan\" = \"read-only\"",
            "\"claude:plan\" = \"nope\"",
        );
        assert!(matches!(
            full_spec_error(&fixture),
            SpecError::UnknownPermissionMode {
                field: "permissions.across",
                ..
            }
        ));
    }

    #[test]
    fn permissions_without_a_default_are_refused() {
        let fixture = full_spec_text().replace("default = \"auto\"\n", "");
        assert!(matches!(
            full_spec_error(&fixture),
            SpecError::PermissionDefaultMissing
        ));
    }

    #[test]
    fn duplicate_permission_modes_are_refused() {
        let fixture = full_spec_text().replacen(
            "  { id = \"auto\", label = \"Auto\"",
            "  { id = \"read-only\", label = \"Auto\"",
            1,
        );
        assert!(matches!(
            full_spec_error(&fixture),
            SpecError::DuplicatePermissionMode { .. }
        ));
    }

    #[test]
    fn a_permission_mode_takes_no_placeholder() {
        let fixture = full_spec_text().replace(
            "args = [\"-s\", \"workspace-write\"]",
            "args = [\"--model\", \"{model}\"]",
        );
        assert!(matches!(
            full_spec_error(&fixture),
            SpecError::InvalidTemplate {
                field: "permissions.modes.args",
                ..
            }
        ));
    }

    #[test]
    fn a_cross_program_permission_key_is_program_colon_mode() {
        let fixture = full_spec_text().replace("\"claude:plan\"", "\"claudeplan\"");
        assert!(matches!(
            full_spec_error(&fixture),
            SpecError::InvalidAcrossKey { .. }
        ));
    }

    #[test]
    fn a_first_message_brief_may_not_name_a_flag() {
        let fixture = full_spec_text().replace(
            "delivery = \"system-prompt-file\"",
            "delivery = \"first-message\"",
        );
        assert!(matches!(
            full_spec_error(&fixture),
            SpecError::BriefFlagWithoutFile { .. }
        ));
    }

    #[test]
    fn a_system_prompt_file_brief_needs_a_flag() {
        let fixture = full_spec_text()
            .replace("args = [\"--sys\", \"{prompt_file}\"]\n", "")
            .replace("file = \"path\"", "file = \"contents\"");
        assert!(matches!(
            full_spec_error(&fixture),
            SpecError::BriefFileWithoutFlag
        ));
    }

    #[test]
    fn a_path_brief_cannot_pass_the_words_themselves() {
        let fixture = full_spec_text().replace("\"{prompt_file}\"", "\"{prompt}\"");
        assert!(matches!(
            full_spec_error(&fixture),
            SpecError::InvalidTemplate {
                field: "brief.args",
                ..
            }
        ));
    }

    #[test]
    fn a_contents_brief_cannot_pass_a_path() {
        let fixture = full_spec_text().replace("file = \"path\"", "file = \"contents\"");
        assert!(matches!(
            full_spec_error(&fixture),
            SpecError::InvalidTemplate {
                field: "brief.args",
                ..
            }
        ));
    }

    #[test]
    fn a_brief_flag_must_carry_the_prompt_file() {
        let fixture =
            full_spec_text().replace("\"--sys\", \"{prompt_file}\"", "\"--sys\", \"brief.md\"");
        assert!(matches!(
            full_spec_error(&fixture),
            SpecError::InvalidTemplate {
                field: "brief.args",
                ..
            }
        ));
    }

    #[test]
    fn a_models_section_has_one_catalog_source() {
        let fixture = full_spec_text().replace(
            "[models]",
            "[models]\nlist_command = [\"codex\", \"debug\", \"models\"]\nstatic_list = \"claude.toml\"",
        );
        assert!(matches!(
            full_spec_error(&fixture),
            SpecError::TwoCatalogSources
        ));
    }

    #[test]
    fn a_static_list_is_a_bare_file_name() {
        for name in ["data/models/claude.toml", "~/claude.toml", "claude.json"] {
            let fixture = full_spec_text()
                .replace("[models]", &format!("[models]\nstatic_list = \"{name}\"\n"));
            assert!(
                matches!(
                    full_spec_error(&fixture),
                    SpecError::InvalidStaticList { .. }
                ),
                "{name} should be refused"
            );
        }
    }

    #[test]
    fn a_catalog_parser_needs_a_list_command() {
        let fixture = full_spec_text().replace("[models]", "[models]\nparser = \"codex_catalog\"");
        assert!(matches!(
            full_spec_error(&fixture),
            SpecError::ParserWithoutListCommand { .. }
        ));
    }

    #[test]
    fn effort_levels_need_a_source() {
        let fixture = full_spec_text().replace(
            "[models.efforts]\nsource = \"static\"",
            "[models.efforts]\nsource = \"none\"",
        );
        assert!(matches!(
            full_spec_error(&fixture),
            SpecError::EffortsWithoutSource
        ));
    }

    #[test]
    fn a_status_source_list_is_not_empty_and_holds_no_source_twice() {
        let fixture = full_spec_text().replace(
            "sources = [\"hook\", \"osc7501\", \"screen\"]",
            "sources = []",
        );
        assert!(matches!(
            full_spec_error(&fixture),
            SpecError::EmptyStatusSources
        ));

        let fixture = full_spec_text().replace(
            "sources = [\"hook\", \"osc7501\", \"screen\"]",
            "sources = [\"hook\", \"hook\"]",
        );
        let error = full_spec_error(&fixture);
        assert!(matches!(error, SpecError::DuplicateStatusSource { .. }));
        assert!(error.to_string().contains("hook"), "{error}");
    }

    #[test]
    fn a_supported_ask_hook_names_its_installer_facts() {
        let fixture = full_spec_text().replace("config = \".fake/hooks.json\"\n", "");
        assert!(matches!(
            full_spec_error(&fixture),
            SpecError::IncompleteAskHook { missing: "config" }
        ));

        let fixture = full_spec_text().replace(
            "config = \".fake/hooks.json\"",
            "config = \"/etc/hooks.json\"",
        );
        assert!(matches!(
            full_spec_error(&fixture),
            SpecError::InvalidAskConfig { .. }
        ));

        let fixture = full_spec_text().replace(
            "config = \".fake/hooks.json\"",
            "config = \"~/.claude/settings.json\"",
        );
        assert!(matches!(
            full_spec_error(&fixture),
            SpecError::InvalidAskConfig { .. }
        ));

        let fixture =
            full_spec_text().replace("command = \"sh '{hook}'\"", "command = \"sh hook.sh\"");
        assert!(matches!(
            full_spec_error(&fixture),
            SpecError::InvalidAskCommand { .. }
        ));
    }

    #[test]
    fn an_unsupported_ask_hook_carries_no_installer_facts() {
        let fixture = full_spec_text().replace("supported = true", "supported = false");
        assert!(matches!(
            full_spec_error(&fixture),
            SpecError::AskHookWithoutSupport { field: "config" }
        ));
    }

    #[test]
    fn an_ask_kind_is_declared_once() {
        let fixture = full_spec_text().replace("kind = \"plan\"", "kind = \"permission\"");
        assert!(matches!(
            full_spec_error(&fixture),
            SpecError::DuplicateAskKind {
                kind: AskKind::Permission
            }
        ));
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

    fn write_spec(dir: &Path, name: &str, contents: &str) {
        if let Err(error) = std::fs::write(dir.join(name), contents) {
            panic!("should write {name}: {error}");
        }
    }

    fn temp_harness_dir() -> tempfile::TempDir {
        match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(error) => panic!("should create a temp dir: {error}"),
        }
    }

    #[test]
    fn load_dir_rejects_invalid_spec_naming_the_file() {
        let dir = temp_harness_dir();
        let broken = MINIMAL_SPEC.replace(
            "location = \"~/.claude/projects/{cwd_slug}/{session}.jsonl\"",
            "location = \"~/.claude//{session}.jsonl\"",
        );
        write_spec(dir.path(), "broken.toml", &broken);
        let error = match load_dir(dir.path()) {
            Ok(specs) => panic!("invalid spec should be rejected, got {} specs", specs.len()),
            Err(error) => error,
        };
        assert!(matches!(error, SpecError::Invalid { .. }));
        assert!(error.to_string().contains("broken.toml"), "{error}");
    }

    #[test]
    fn load_dir_rejects_duplicate_ids() {
        let dir = temp_harness_dir();
        write_spec(dir.path(), "a.toml", MINIMAL_SPEC);
        write_spec(dir.path(), "b.toml", MINIMAL_SPEC);
        let error = match load_dir(dir.path()) {
            Ok(specs) => panic!(
                "duplicate ids should be rejected, got {} specs",
                specs.len()
            ),
            Err(error) => error,
        };
        assert!(matches!(error, SpecError::DuplicateId { .. }));
    }

    fn template_error(fixture: &str) -> SpecError {
        let spec = parse_fixture(fixture);
        match spec.validate() {
            Ok(()) => panic!("launch template should be rejected"),
            Err(error) => error,
        }
    }

    #[test]
    fn rejects_model_args_without_model_placeholder() {
        let fixture = MINIMAL_SPEC.replacen(
            r#"model_args = ["--model", "{model}"]"#,
            r#"model_args = ["--model", "sonnet"]"#,
            1,
        );
        assert!(matches!(
            template_error(&fixture),
            SpecError::InvalidTemplate { .. }
        ));
    }

    #[test]
    fn rejects_effort_args_using_model_placeholder() {
        let fixture = MINIMAL_SPEC.replacen(
            r#"effort_args = ["--effort", "{effort}"]"#,
            r#"effort_args = ["--effort", "{model}"]"#,
            1,
        );
        assert!(matches!(
            template_error(&fixture),
            SpecError::InvalidTemplate { .. }
        ));
    }

    #[test]
    fn an_effort_flag_must_carry_the_level() {
        let fixture = MINIMAL_SPEC.replacen(
            r#"effort_args = ["--effort", "{effort}"]"#,
            r#"effort_args = ["--effort", "high"]"#,
            1,
        );
        assert!(matches!(
            template_error(&fixture),
            SpecError::InvalidTemplate {
                field: "launch.effort_args",
                ..
            }
        ));
    }

    #[test]
    fn levels_without_a_flag_are_refused() {
        let fixture = full_spec_text().replace("effort_args = [\"--effort\", \"{effort}\"]\n", "");
        assert!(matches!(
            full_spec_error(&fixture),
            SpecError::EffortsWithoutFlag
        ));
    }

    #[test]
    fn accepts_signin_args_using_model() {
        let fixture = MINIMAL_SPEC.replacen(
            r#"signin_hint = "claude auth login""#,
            "signin_hint = \"claude auth login\"\nsignin_args = [\"auth\", \"check\", \"--model\", \"{model}\"]",
            1,
        );
        let spec = parse_fixture(&fixture);
        assert!(
            spec.validate().is_ok(),
            "optionally using {{model}} is allowed"
        );
    }

    #[test]
    fn rejects_signin_args_using_effort_placeholder() {
        let fixture = MINIMAL_SPEC.replacen(
            r#"signin_hint = "claude auth login""#,
            "signin_hint = \"claude auth login\"\nsignin_args = [\"--effort\", \"{effort}\"]",
            1,
        );
        assert!(matches!(
            template_error(&fixture),
            SpecError::InvalidTemplate { .. }
        ));
    }

    #[test]
    fn rejects_two_placeholders_in_one_token() {
        let fixture = MINIMAL_SPEC.replacen(
            r#"model_args = ["--model", "{model}"]"#,
            r#"model_args = ["--model={model}{effort}"]"#,
            1,
        );
        assert!(matches!(
            template_error(&fixture),
            SpecError::InvalidTemplate { .. }
        ));
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
