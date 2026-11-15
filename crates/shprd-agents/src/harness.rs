//! One program's harness, loaded and checked, as adapters work with it.
//!
//! [`crate::spec`] is the TOML schema; this module is the handle M9.2's
//! adapters, M9.3's roles and M9.5's ask cards hold. It adds no behaviour of its
//! own: every accessor reads the spec, and the few that choose between fields
//! say which one wins.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::asks::{AskHookInstaller, HarnessAskHook};
use crate::spec::{
    AskHookSpec, BriefSpec, CatalogSource, EffortSpec, FastSpec, HarnessSpec, ModelsSpec,
    PermissionMode, PermissionsSpec, SpecError, StatusSource, Vendor,
};

/// One program's harness: its spec, checked, and the file it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Harness {
    spec: HarnessSpec,
    path: PathBuf,
}

impl Harness {
    /// Load one `data/harness/*.toml` file and check it.
    ///
    /// The same rules as [`crate::spec::load_dir`]: unknown keys are refused by
    /// the TOML schema, and [`HarnessSpec::validate`] checks what the type
    /// cannot express. A spec that fails either is an error naming the file.
    pub fn load(path: &Path) -> Result<Harness, SpecError> {
        let text = std::fs::read_to_string(path).map_err(|source| SpecError::ReadFile {
            path: path.to_owned(),
            source,
        })?;
        Harness::parse(path, &text)
    }

    /// Check a harness file already read in, naming `path` in any error.
    ///
    /// This is [`Harness::load`] for a file that is not on this machine as a
    /// file: the copy embedded in the binary, which [`crate::data::DataDir`]
    /// reads into text.
    pub fn parse(path: &Path, text: &str) -> Result<Harness, SpecError> {
        let spec = crate::spec::parse(path, text)?;
        spec.validate().map_err(|source| SpecError::Invalid {
            path: path.to_owned(),
            source: Box::new(source),
        })?;
        Ok(Harness {
            spec,
            path: path.to_owned(),
        })
    }

    /// Everything the file says, for a caller that needs a field no accessor
    /// here names yet.
    pub fn spec(&self) -> &HarnessSpec {
        &self.spec
    }

    /// The file this harness was loaded from.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The program's short id, e.g. `claude`.
    pub fn id(&self) -> &str {
        &self.spec.id
    }

    /// The program's human-readable name, e.g. `Claude Code`.
    pub fn name(&self) -> &str {
        &self.spec.name
    }

    /// The vendor that ships the program.
    pub fn vendor(&self) -> Vendor {
        self.spec.vendor
    }

    /// Whether the program runs other vendors' models too.
    pub fn multi_model(&self) -> bool {
        self.spec.multi_model
    }

    /// The access modes the program offers, in the order a settings UI shows
    /// them.
    pub fn permissions(&self) -> &PermissionsSpec {
        &self.spec.permissions
    }

    /// The access mode called `id`, if this program has one.
    pub fn permission(&self, id: &str) -> Option<&PermissionMode> {
        self.spec.permissions.mode(id)
    }

    /// The access mode a role gets when it names none.
    pub fn default_permission(&self) -> Option<&PermissionMode> {
        self.spec.permissions.default_mode()
    }

    /// The access mode that counts as full access, for the cross-program carry.
    pub fn full_permission(&self) -> Option<&PermissionMode> {
        self.spec.permissions.full_mode()
    }

    /// The access mode a reviewer runs in.
    pub fn readonly_permission(&self) -> Option<&PermissionMode> {
        self.spec.permissions.readonly_mode()
    }

    /// This program's nearest mode to another program's `<program>:<mode>`.
    pub fn permission_across(&self, from: &str) -> Option<&PermissionMode> {
        self.spec.permissions.across_mode(from)
    }

    /// How this program's models are listed.
    pub fn models(&self) -> &ModelsSpec {
        &self.spec.models
    }

    /// What the model catalog is read from, if anything.
    pub fn catalog_source(&self) -> CatalogSource {
        self.spec.models.catalog_source()
    }

    /// Where this program's effort levels come from.
    pub fn efforts(&self) -> &EffortSpec {
        &self.spec.models.efforts
    }

    /// The arguments that switch this program's fast mode, if it has one.
    pub fn fast(&self) -> &FastSpec {
        &self.spec.fast
    }

    /// How a brief reaches this program.
    pub fn brief(&self) -> &BriefSpec {
        &self.spec.brief
    }

    /// The flag that carries this program's brief, when it takes one as a
    /// system-prompt file.
    ///
    /// [`BriefSpec::args`] wins when the file sets it; otherwise the flag is
    /// `launch.system_prompt_args`, where it has always lived.
    pub fn brief_args(&self) -> Option<&[String]> {
        self.spec.brief.args(&self.spec.launch)
    }

    /// Where this program's status can be read, most trusted source first.
    pub fn status_sources(&self) -> &[StatusSource] {
        &self.spec.status.sources
    }

    /// Whether and how this program accepts a hook for its asks.
    pub fn asks(&self) -> &AskHookSpec {
        &self.spec.asks
    }

    /// The ask-hook installer for this program.
    ///
    /// It is chosen by the harness's `[asks] format`: Claude Code gets its
    /// worktree-local project settings, Codex its project hooks, and every
    /// other program a no-op that touches nothing (M9.5).
    pub fn ask_installer(&self) -> impl AskHookInstaller + '_ {
        HarnessAskHook::for_spec(&self.spec.asks, &self.spec.id)
    }
}

/// Environment variable pointing shprd at the data directory, for a checkout, a
/// worker's own table or a test's fixture.
///
/// It wins over everything else, and the directory it names is read as the whole
/// table; with it unset, [`crate::data::DataDir`] finds a person's overrides
/// under the data home and then the copy embedded in the binary.
pub const DATA_ENV: &str = "SHPRD_DATA_DIR";

/// The data home for `home`: `xdg_data_home` (the `$XDG_DATA_HOME` the caller
/// read) when it names a directory, else `<home>/.local/share`.
///
/// An empty value is unset, which is what the XDG specification says. The value
/// is an argument rather than an environment lookup so the one place that reads
/// `$XDG_DATA_HOME` — and `$HOME` behind it — is the binary's wiring.
pub fn data_home(home: &Path, xdg_data_home: Option<&OsStr>) -> PathBuf {
    match xdg_data_home.filter(|value| !value.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => home.join(".local").join("share"),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};

    use super::{Harness, data_home};
    use crate::asks::AskHookInstaller;
    use crate::spec::{
        AskHookConfig, BriefDelivery, BriefFileMode, CatalogParser, CatalogSource, StatusSource,
    };

    /// The production rule with the environment's value handed in: a set
    /// `$XDG_DATA_HOME` wins, and an unset or empty one falls back to the
    /// person's `~/.local/share`.
    #[test]
    fn the_data_home_honours_the_xdg_setting_it_is_given() {
        let home = Path::new("/home/me");
        assert_eq!(
            data_home(home, Some(OsStr::new("/xdg/data"))),
            Path::new("/xdg/data")
        );
        assert_eq!(data_home(home, None), home.join(".local").join("share"));
        assert_eq!(
            data_home(home, Some(OsStr::new(""))),
            home.join(".local").join("share"),
            "an empty setting is unset"
        );
    }

    /// A directory named outright is the answer, whether or not it exists and
    /// whatever the checkout and the data home hold: this is what a worker's
    /// `SHPRD_DATA_DIR` means.
    #[test]
    fn a_named_data_directory_wins_as_it_is() {
        let home = tempfile::tempdir().expect("a temp home");
        let data_home = data_home(home.path(), Some(OsStr::new("/xdg/data")));
        let named = home.path().join("named");
        let data = crate::data::DataDir::resolve(Some(&named), Some(&data_home));
        assert_eq!(
            data.dir(),
            Some(named.as_path()),
            "a named directory is read as it is, whether or not it exists"
        );
    }

    fn harness_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/harness")
    }

    fn load(id: &str) -> Harness {
        let path = harness_dir().join(format!("{id}.toml"));
        match Harness::load(&path) {
            Ok(harness) => harness,
            Err(error) => panic!("{id} should load: {error}"),
        }
    }

    #[test]
    fn loads_every_shipped_harness_by_id() {
        for id in ["claude", "codex", "kilo", "opencode", "pi"] {
            let harness = load(id);
            assert_eq!(harness.id(), id);
            assert_eq!(harness.path(), harness_dir().join(format!("{id}.toml")));
            assert!(!harness.name().is_empty());
        }
    }

    #[test]
    fn claude_code_reads_its_models_from_a_static_list() {
        let claude = load("claude");
        assert_eq!(claude.catalog_source(), CatalogSource::Static);
        assert_eq!(
            claude.models().static_list.as_deref(),
            Some("claude.toml"),
            "the static catalog should be a bare file name in data/models/"
        );
    }

    #[test]
    fn codex_asks_its_own_catalog() {
        let codex = load("codex");
        assert_eq!(codex.catalog_source(), CatalogSource::Command);
        assert_eq!(codex.models().parser, CatalogParser::CodexCatalog);
        assert!(codex.efforts().levels.len() > 1);
    }

    #[test]
    fn claude_code_takes_its_brief_as_a_system_prompt_file() {
        let claude = load("claude");
        assert_eq!(claude.brief().delivery, BriefDelivery::SystemPromptFile);
        assert_eq!(claude.brief().file, BriefFileMode::Path);
        assert_eq!(
            claude.brief_args(),
            Some(
                ["--append-system-prompt-file", "{prompt_file}"]
                    .map(str::to_owned)
                    .as_slice()
            )
        );
    }

    #[test]
    fn a_program_without_a_brief_flag_takes_it_as_its_first_message() {
        for id in ["kilo", "opencode"] {
            let harness = load(id);
            assert_eq!(harness.brief().delivery, BriefDelivery::FirstMessage);
            assert_eq!(harness.brief_args(), None);
        }
    }

    #[test]
    fn every_harness_names_its_status_sources_in_order() {
        for id in ["claude", "codex", "kilo", "opencode", "pi"] {
            let harness = load(id);
            assert_eq!(
                harness.status_sources().first(),
                Some(&StatusSource::Osc7501),
                "{id} should trust the agent's own report first"
            );
            assert!(harness.status_sources().len() >= 2, "{id}");
        }
    }

    #[test]
    fn claude_code_installs_its_ask_hook_into_its_own_settings() {
        let claude = load("claude");
        let asks = claude.asks();
        assert!(asks.supported);
        assert_eq!(asks.format, AskHookConfig::ClaudeSettings);
        assert_eq!(asks.config.as_deref(), Some(".claude/settings.local.json"));
        let installer = claude.ask_installer();
        assert!(installer.available());
    }

    #[test]
    fn a_program_with_no_ask_hook_says_so() {
        for id in ["kilo", "opencode", "pi"] {
            let harness = load(id);
            assert!(!harness.asks().supported, "{id}");
            assert_eq!(harness.asks().format, AskHookConfig::Unsupported, "{id}");
            assert!(!harness.ask_installer().available(), "{id}");
        }
    }

    #[test]
    fn a_program_with_modes_says_which_one_is_the_default() {
        for id in ["claude", "codex"] {
            let harness = load(id);
            let permissions = harness.permissions();
            assert!(!permissions.modes.is_empty(), "{id}");
            let default = harness
                .default_permission()
                .unwrap_or_else(|| panic!("{id} should default to one of its modes"));
            assert!(permissions.modes.contains(default), "{id}");
            assert!(
                harness.readonly_permission().is_some(),
                "{id} should name the mode a reviewer runs in"
            );
        }
    }

    #[test]
    fn only_a_program_with_a_fast_switch_takes_one() {
        let claude = load("claude");
        assert!(claude.fast().available());
        assert_eq!(
            claude.fast().args(true),
            ["--settings", "{\"fastMode\":true}"].map(str::to_owned)
        );
        assert_eq!(
            claude.fast().args(false),
            ["--settings", "{\"fastMode\":false}"].map(str::to_owned)
        );
        for id in ["codex", "kilo", "opencode", "pi"] {
            assert!(!load(id).fast().available(), "{id} takes no fast switch");
        }
    }

    #[test]
    fn a_missing_file_is_a_read_error() {
        let error = match Harness::load(&harness_dir().join("nope.toml")) {
            Ok(_) => panic!("a missing file should be refused"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("nope.toml"), "{error}");
    }
}
