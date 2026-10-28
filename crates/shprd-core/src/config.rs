//! Layered configuration.
//!
//! shprd reads a global file (`~/.config/shprd/config.toml`, written by the
//! setup wizard) and an optional per-repo override (`<repo>/.shprd/config.toml`).
//! The repo file overrides the global file *field by field*: a scalar or list
//! present in the repo layer replaces the global value, and maps merge by key.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

use crate::event::Severity;

/// How worker workspaces are provisioned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorkspaceMode {
    /// One workspace per repository.
    PerRepo,
    /// One workspace per task.
    PerTask,
    /// Work in the directory shprd was started from.
    Current,
}

/// The terminal multiplexer shprd drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mux {
    /// The herdr multiplexer.
    Herdr,
    /// The tuios multiplexer.
    Tuios,
}

/// When `shprd land --merge` may merge the run's pull request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MergePolicy {
    /// Merge only after a human approved the pull request.
    #[default]
    Ask,
    /// Merge without approval, unless the base branch is protected.
    Allow,
}

/// How a run's pull request is landed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Land {
    /// Whether a merge needs a human's approval.
    pub merge: MergePolicy,
    /// Branches `allow` never merges into without approval.
    pub protected: Vec<String>,
}

impl Default for Land {
    fn default() -> Self {
        Self {
            merge: MergePolicy::Ask,
            protected: vec!["main".to_string(), "master".to_string()],
        }
    }
}

/// Which agent fills each role, and at what effort.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Roster {
    /// Agent string for the orchestrator, e.g. `claude/claude-opus-5-5`.
    pub orchestrator: Option<String>,
    /// Agent string for the reviewer. Must differ from the orchestrator.
    pub reviewer: Option<String>,
    /// Agent strings for the workers, in dispatch order.
    pub workers: Vec<String>,
    /// Effort level per agent string, e.g. `claude/claude-opus-5-5` -> `high`.
    pub effort: BTreeMap<String, String>,
    /// Whether the orchestrator launches with its CLI's auto-approve flags.
    ///
    /// Off by default: the orchestrator runs unattended only when a human asks
    /// for it, and then the pull request is the gate that replaces prompts.
    pub orchestrator_auto_approve: bool,
}

/// Which skills each role gets.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Skills {
    /// Skills given to the orchestrator.
    pub orchestrator: Vec<String>,
    /// Skills given to workers.
    pub workers: Vec<String>,
    /// Skills given to the reviewer.
    pub reviewer: Vec<String>,
    /// Extra skills per tag, e.g. `ui` -> `["frontend-design"]`.
    pub tags: BTreeMap<String, Vec<String>>,
}

/// Workspace provisioning settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Workspace {
    /// How workspaces are provisioned.
    pub mode: WorkspaceMode,
    /// Fraction of the columns the lead pane may take.
    pub lead_ratio: f32,
}

/// Janitor settings for pruning finished work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Janitor {
    /// Archive work older than this many days.
    pub archive_days: u32,
    /// Namespace prefix for archived resources; must end with `/`.
    pub namespace: String,
}

/// The shell commands that must pass before code lands.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Gates {
    /// Gate commands a worker runs before reporting done.
    pub worker: Vec<String>,
    /// Gate commands run once, on the integration branch.
    pub integration: Vec<String>,
}

/// How a review's findings decide pass or fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Review {
    /// The least severe finding that still blocks integration.
    pub must_fix: Severity,
    /// Failed review rounds allowed before the review escalates to a human.
    pub rounds: u32,
}

/// The resolved configuration: defaults with every layer applied.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// Agent roles.
    pub roster: Roster,
    /// Skills per role and per tag.
    pub skills: Skills,
    /// Workspace provisioning.
    pub workspace: Workspace,
    /// Janitor pruning.
    pub janitor: Janitor,
    /// The multiplexer to drive, if one was chosen.
    pub mux: Option<Mux>,
    /// Gate commands.
    pub gates: Gates,
    /// Review policy: what blocks, and how many rounds are allowed.
    pub review: Review,
    /// Land policy: whether a merge needs a human's approval.
    pub land: Land,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            roster: Roster::default(),
            skills: Skills::default(),
            workspace: Workspace {
                mode: WorkspaceMode::PerRepo,
                lead_ratio: 0.68,
            },
            janitor: Janitor {
                archive_days: 14,
                namespace: String::from("shprd/"),
            },
            mux: None,
            gates: Gates::default(),
            review: Review {
                must_fix: Severity::Major,
                rounds: 2,
            },
            land: Land::default(),
        }
    }
}

impl Config {
    /// Overlay one file layer on top of this configuration.
    ///
    /// A scalar or list set in the layer replaces the current value; maps merge
    /// by key with the layer winning.
    fn apply(&mut self, layer: &ConfigLayer) {
        if let Some(roster) = &layer.roster {
            if let Some(orchestrator) = &roster.orchestrator {
                self.roster.orchestrator = Some(orchestrator.clone());
            }
            if let Some(reviewer) = &roster.reviewer {
                self.roster.reviewer = Some(reviewer.clone());
            }
            if let Some(workers) = &roster.workers {
                self.roster.workers = workers.clone();
            }
            if let Some(effort) = &roster.effort {
                self.roster
                    .effort
                    .extend(effort.iter().map(|(k, v)| (k.clone(), v.clone())));
            }
            if let Some(auto_approve) = roster.orchestrator_auto_approve {
                self.roster.orchestrator_auto_approve = auto_approve;
            }
        }
        if let Some(skills) = &layer.skills {
            if let Some(orchestrator) = &skills.orchestrator {
                self.skills.orchestrator = orchestrator.clone();
            }
            if let Some(workers) = &skills.workers {
                self.skills.workers = workers.clone();
            }
            if let Some(reviewer) = &skills.reviewer {
                self.skills.reviewer = reviewer.clone();
            }
            if let Some(tags) = &skills.tags {
                self.skills
                    .tags
                    .extend(tags.iter().map(|(k, v)| (k.clone(), v.clone())));
            }
        }
        if let Some(workspace) = &layer.workspace {
            if let Some(mode) = workspace.mode {
                self.workspace.mode = mode;
            }
            if let Some(lead_ratio) = workspace.lead_ratio {
                self.workspace.lead_ratio = lead_ratio;
            }
        }
        if let Some(janitor) = &layer.janitor {
            if let Some(archive_days) = janitor.archive_days {
                self.janitor.archive_days = archive_days;
            }
            if let Some(namespace) = &janitor.namespace {
                self.janitor.namespace = namespace.clone();
            }
        }
        if let Some(mux) = layer.mux {
            self.mux = Some(mux);
        }
        if let Some(gates) = &layer.gates {
            if let Some(worker) = &gates.worker {
                self.gates.worker = worker.clone();
            }
            if let Some(integration) = &gates.integration {
                self.gates.integration = integration.clone();
            }
        }
        if let Some(review) = &layer.review {
            if let Some(must_fix) = review.must_fix {
                self.review.must_fix = must_fix;
            }
            if let Some(rounds) = review.rounds {
                self.review.rounds = rounds;
            }
        }
        if let Some(land) = &layer.land {
            if let Some(merge) = land.merge {
                self.land.merge = merge;
            }
            if let Some(protected) = &land.protected {
                self.land.protected = protected.clone();
            }
        }
    }
}

/// One parsed config file. Every field is optional: absent means "inherit".
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigLayer {
    /// The `[roster]` section.
    pub roster: Option<RosterLayer>,
    /// The `[skills]` section.
    pub skills: Option<SkillsLayer>,
    /// The `[workspace]` section.
    pub workspace: Option<WorkspaceLayer>,
    /// The `[janitor]` section.
    pub janitor: Option<JanitorLayer>,
    /// The top-level `mux` key.
    pub mux: Option<Mux>,
    /// The `[gates]` section.
    pub gates: Option<GatesLayer>,
    /// The `[review]` section.
    pub review: Option<ReviewLayer>,
    /// The `[land]` section.
    pub land: Option<LandLayer>,
}

/// The `[roster]` section of a config file.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RosterLayer {
    /// Agent string for the orchestrator.
    pub orchestrator: Option<String>,
    /// Agent string for the reviewer.
    pub reviewer: Option<String>,
    /// Agent strings for the workers.
    pub workers: Option<Vec<String>>,
    /// Effort level per agent string, from `[roster.effort]`.
    pub effort: Option<BTreeMap<String, String>>,
    /// Whether the orchestrator launches with auto-approve, from
    /// `[roster] orchestrator_auto_approve = true`.
    pub orchestrator_auto_approve: Option<bool>,
}

/// The `[skills]` section of a config file.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillsLayer {
    /// Skills given to the orchestrator.
    pub orchestrator: Option<Vec<String>>,
    /// Skills given to workers.
    pub workers: Option<Vec<String>>,
    /// Skills given to the reviewer.
    pub reviewer: Option<Vec<String>>,
    /// Extra skills per tag, from `[skills.tags]`.
    pub tags: Option<BTreeMap<String, Vec<String>>>,
}

/// The `[workspace]` section of a config file.
#[derive(Debug, Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceLayer {
    /// How workspaces are provisioned.
    pub mode: Option<WorkspaceMode>,
    /// Fraction of the columns the lead pane may take.
    pub lead_ratio: Option<f32>,
}

/// The `[janitor]` section of a config file.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JanitorLayer {
    /// Archive work older than this many days.
    pub archive_days: Option<u32>,
    /// Namespace prefix for archived resources.
    pub namespace: Option<String>,
}

/// The `[gates]` section of a config file.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatesLayer {
    /// Gate commands a worker runs before reporting done.
    pub worker: Option<Vec<String>>,
    /// Gate commands run once, on the integration branch.
    pub integration: Option<Vec<String>>,
}

/// The `[review]` section of a config file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewLayer {
    /// The least severe finding that still blocks integration.
    pub must_fix: Option<Severity>,
    /// Failed review rounds allowed before the review escalates.
    pub rounds: Option<u32>,
}

/// The `[land]` section of a config file.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LandLayer {
    /// Whether a merge needs a human's approval.
    pub merge: Option<MergePolicy>,
    /// Branches `allow` never merges into without approval.
    pub protected: Option<Vec<String>>,
}

/// Everything that can go wrong loading configuration.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// A config file exists but could not be read.
    #[error("cannot read config file {path}: {source}")]
    Io {
        /// The file that failed.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// A config file exists but is not valid TOML for the schema.
    #[error("cannot parse config file {path}: {source}")]
    Parse {
        /// The file that failed.
        path: PathBuf,
        /// The underlying TOML error.
        source: toml::de::Error,
    },
    /// The merged configuration is not usable.
    #[error("invalid config: {field} {reason}")]
    Invalid {
        /// The dotted field name, e.g. `workspace.lead_ratio`.
        field: &'static str,
        /// Why the value was rejected.
        reason: String,
    },
}

/// Merge file layers, in order, over the built-in defaults.
///
/// Later layers win: scalars and lists replace, maps merge by key.
pub fn from_layers(layers: &[ConfigLayer]) -> Config {
    let mut config = Config::default();
    for layer in layers {
        config.apply(layer);
    }
    config
}

/// Load the global file then the repo file and return the resolved config.
///
/// A path that is `None` or does not exist is skipped. A file that exists but
/// cannot be read or parsed is an error. The merged config is validated.
pub fn load(global: Option<&Path>, repo: Option<&Path>) -> Result<Config, ConfigError> {
    let mut layers = Vec::new();
    for path in [global, repo].into_iter().flatten() {
        if let Some(layer) = read_layer(path)? {
            layers.push(layer);
        }
    }
    let config = from_layers(&layers);
    config.validate()?;
    Ok(config)
}

/// Read and parse one layer, returning `None` when the file is absent.
fn read_layer(path: &Path) -> Result<Option<ConfigLayer>, ConfigError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(ConfigError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let layer = toml::from_str(&text).map_err(|source| ConfigError::Parse {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(Some(layer))
}

impl Config {
    /// Check the invariants a loaded config must satisfy.
    ///
    /// `workspace.lead_ratio` must lie strictly between 0.1 and 0.9,
    /// `janitor.namespace` must be non-empty and end with `/`, and a reviewer
    /// must not be the same agent as the orchestrator.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let lead_ratio = self.workspace.lead_ratio;
        if !(lead_ratio > 0.1 && lead_ratio < 0.9) {
            return Err(ConfigError::Invalid {
                field: "workspace.lead_ratio",
                reason: format!("must be between 0.1 and 0.9 exclusive, got {lead_ratio}"),
            });
        }
        let namespace = &self.janitor.namespace;
        if namespace.is_empty() {
            return Err(ConfigError::Invalid {
                field: "janitor.namespace",
                reason: String::from("must not be empty"),
            });
        }
        if !namespace.ends_with('/') {
            return Err(ConfigError::Invalid {
                field: "janitor.namespace",
                reason: format!("must end with '/', got {namespace:?}"),
            });
        }
        if self.review.rounds < 1 {
            return Err(ConfigError::Invalid {
                field: "review.rounds",
                reason: format!("must be at least 1, got {}", self.review.rounds),
            });
        }
        if let Some(reviewer) = &self.roster.reviewer
            && Some(reviewer) == self.roster.orchestrator.as_ref()
        {
            return Err(ConfigError::Invalid {
                field: "roster.reviewer",
                reason: format!("must differ from the orchestrator, both are {reviewer}"),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Config, ConfigError, ConfigLayer, Janitor, Land, MergePolicy, Mux, Review, Roster, Skills,
        Workspace, WorkspaceMode, from_layers, load,
    };
    use crate::event::Severity;

    fn layer(text: &str) -> ConfigLayer {
        match toml::from_str(text) {
            Ok(layer) => layer,
            Err(error) => panic!("test layer did not parse: {error}"),
        }
    }

    const GLOBAL: &str = r#"
        mux = "herdr"

        [roster]
        orchestrator = "claude/claude-opus-5-5"
        workers = ["claude/claude-opus-5-5", "opencode/deepseek-v4.1-flash-free"]

        [roster.effort]
        "claude/claude-opus-5-5" = "high"

        [skills]
        orchestrator = ["brainstorming"]

        [skills.tags]
        ui = ["design-taste-frontend"]
        bug = ["systematic-debugging"]

        [workspace]
        mode = "per-repo"
        lead_ratio = 0.6

        [janitor]
        archive_days = 30
        namespace = "old/"

        [gates]
        worker = ["cargo test -p shprd-core"]

        [review]
        must_fix = "minor"
        rounds = 3
    "#;

    #[test]
    fn no_files_yields_defaults() {
        match load(None, None) {
            Ok(config) => assert_eq!(config, Config::default()),
            Err(error) => panic!("load of no files failed: {error}"),
        }
    }

    #[test]
    fn defaults_are_the_documented_ones() {
        let config = Config::default();
        assert_eq!(config.roster, Roster::default());
        assert_eq!(config.skills, Skills::default());
        assert_eq!(config.workspace.mode, WorkspaceMode::PerRepo);
        assert!((config.workspace.lead_ratio - 0.68).abs() < f32::EPSILON);
        assert_eq!(
            config.janitor,
            Janitor {
                archive_days: 14,
                namespace: String::from("shprd/"),
            }
        );
        assert_eq!(config.mux, None);
        assert_eq!(config.gates, super::Gates::default());
        assert_eq!(
            config.review,
            Review {
                must_fix: Severity::Major,
                rounds: 2,
            }
        );
        assert_eq!(config.land, Land::default());
        assert!(config.validate().is_ok());
    }

    #[test]
    fn land_defaults_to_asking_and_protects_main_and_master() {
        let config = Config::default();
        assert_eq!(config.land.merge, MergePolicy::Ask);
        assert_eq!(
            config.land.protected,
            vec!["main".to_string(), "master".to_string()]
        );
    }

    #[test]
    fn land_section_is_merged_field_by_field() {
        let global = layer(
            r#"
            [land]
            merge = "allow"
            "#,
        );
        let config = from_layers(std::slice::from_ref(&global));
        assert_eq!(config.land.merge, MergePolicy::Allow);
        assert_eq!(
            config.land.protected,
            vec!["main".to_string(), "master".to_string()],
            "the defaults survive a section that only sets the policy"
        );
        let repo = layer(
            r#"
            [land]
            protected = ["trunk"]
            "#,
        );
        let config = from_layers(&[global, repo]);
        assert_eq!(config.land.merge, MergePolicy::Allow);
        assert_eq!(config.land.protected, vec!["trunk".to_string()]);
    }

    #[test]
    fn an_unknown_merge_policy_is_rejected() {
        let parsed: Result<ConfigLayer, _> = toml::from_str(
            r#"
            [land]
            merge = "always"
            "#,
        );
        assert!(parsed.is_err(), "only ask and allow are policies");
    }

    #[test]
    fn repo_overrides_one_scalar_and_keeps_the_rest() {
        let global = layer(GLOBAL);
        let repo = layer(
            r"
            [workspace]
            lead_ratio = 0.5
            ",
        );
        let config = from_layers(&[global, repo]);
        assert!((config.workspace.lead_ratio - 0.5).abs() < f32::EPSILON);
        assert_eq!(config.workspace.mode, WorkspaceMode::PerRepo);
        assert_eq!(
            config.roster.orchestrator.as_deref(),
            Some("claude/claude-opus-5-5")
        );
        assert_eq!(config.janitor.archive_days, 30);
        assert_eq!(config.janitor.namespace, "old/");
        assert_eq!(config.mux, Some(Mux::Herdr));
        assert_eq!(config.skills.orchestrator, vec!["brainstorming"]);
        assert_eq!(config.gates.worker, vec!["cargo test -p shprd-core"]);
    }

    #[test]
    fn review_section_is_merged_field_by_field() {
        let global = layer(GLOBAL);
        assert_eq!(
            from_layers(std::slice::from_ref(&global)).review,
            Review {
                must_fix: Severity::Minor,
                rounds: 3,
            }
        );
        let repo = layer(
            r#"
            [review]
            rounds = 5
            "#,
        );
        let config = from_layers(&[global, repo]);
        assert_eq!(config.review.must_fix, Severity::Minor);
        assert_eq!(config.review.rounds, 5);
    }

    #[test]
    fn zero_review_rounds_is_invalid() {
        let mut config = Config::default();
        config.review.rounds = 0;
        match config.validate() {
            Err(ConfigError::Invalid { field, reason }) => {
                assert_eq!(field, "review.rounds");
                assert!(reason.contains('0'), "reason names the value: {reason}");
            }
            other => panic!("expected a review.rounds refusal, got {other:?}"),
        }
    }

    #[test]
    fn later_list_replaces_the_earlier_list() {
        let global = layer(GLOBAL);
        let repo = layer(
            r#"
            [roster]
            workers = ["codex/gpt-6"]
            "#,
        );
        let config = from_layers(&[global, repo]);
        assert_eq!(config.roster.workers, vec!["codex/gpt-6"]);
        // The global file's unchanged fields survive.
        assert_eq!(
            config.roster.orchestrator.as_deref(),
            Some("claude/claude-opus-5-5")
        );
    }

    #[test]
    fn the_orchestrator_runs_without_prompts_only_when_asked() {
        assert!(!Config::default().roster.orchestrator_auto_approve);
        let on = layer(
            r#"
            [roster]
            orchestrator_auto_approve = true
            "#,
        );
        let config = from_layers(&[on]);
        assert!(config.roster.orchestrator_auto_approve);
    }

    #[test]
    fn a_later_layer_can_turn_auto_approve_off_again() {
        let on = layer(
            r#"
            [roster]
            orchestrator_auto_approve = true
            "#,
        );
        let off = layer(
            r#"
            [roster]
            orchestrator_auto_approve = false
            "#,
        );
        assert!(!from_layers(&[on, off]).roster.orchestrator_auto_approve);
    }

    #[test]
    fn maps_merge_by_key_with_later_layer_winning() {
        let global = layer(GLOBAL);
        let repo = layer(
            r#"
            [skills.tags]
            ui = ["make-interfaces-feel-better"]

            [roster.effort]
            "opencode/deepseek-v4.1-flash-free" = "low"
            "#,
        );
        let config = from_layers(&[global, repo]);
        assert_eq!(
            config.skills.tags.get("ui"),
            Some(&vec![String::from("make-interfaces-feel-better")])
        );
        assert_eq!(
            config.skills.tags.get("bug"),
            Some(&vec![String::from("systematic-debugging")])
        );
        assert_eq!(
            config.roster.effort.get("claude/claude-opus-5-5"),
            Some(&String::from("high"))
        );
        assert_eq!(
            config
                .roster
                .effort
                .get("opencode/deepseek-v4.1-flash-free"),
            Some(&String::from("low"))
        );
    }

    #[test]
    fn unknown_field_is_a_parse_error_naming_the_field() {
        let dir = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(error) => panic!("cannot create temp dir: {error}"),
        };
        let path = dir.path().join("config.toml");
        let text = "[roster]\norchestratr = \"claude/claude-opus-5-5\"\n";
        if let Err(error) = std::fs::write(&path, text) {
            panic!("cannot write test config: {error}");
        }
        match load(Some(&path), None) {
            Err(ConfigError::Parse {
                path: reported,
                source,
            }) => {
                assert_eq!(reported, path);
                let message = source.to_string();
                assert!(
                    message.contains("orchestratr"),
                    "parse error should name the unknown field, got: {message}"
                );
            }
            Err(other) => panic!("expected a parse error, got: {other}"),
            Ok(config) => panic!("expected a parse error, got config: {config:?}"),
        }
    }

    #[test]
    fn validate_rejects_lead_ratio_out_of_range() {
        let mut config = Config::default();
        config.workspace.lead_ratio = 0.95;
        match config.validate() {
            Err(ConfigError::Invalid { field, .. }) => assert_eq!(field, "workspace.lead_ratio"),
            Err(other) => panic!("expected an invalid error, got: {other}"),
            Ok(()) => panic!("lead_ratio 0.95 should be rejected"),
        }
    }

    #[test]
    fn validate_rejects_namespace_without_trailing_slash() {
        let mut config = Config::default();
        config.janitor.namespace = String::from("shprd");
        match config.validate() {
            Err(ConfigError::Invalid { field, .. }) => assert_eq!(field, "janitor.namespace"),
            Err(other) => panic!("expected an invalid error, got: {other}"),
            Ok(()) => panic!("a namespace without '/' should be rejected"),
        }
    }

    #[test]
    fn validate_rejects_reviewer_equal_to_orchestrator() {
        let mut config = Config::default();
        config.roster.orchestrator = Some(String::from("claude/claude-opus-5-5"));
        config.roster.reviewer = Some(String::from("claude/claude-opus-5-5"));
        match config.validate() {
            Err(ConfigError::Invalid { field, .. }) => assert_eq!(field, "roster.reviewer"),
            Err(other) => panic!("expected an invalid error, got: {other}"),
            Ok(()) => panic!("a reviewer equal to the orchestrator should be rejected"),
        }
    }

    #[test]
    fn modes_and_muxes_round_trip_through_kebab_and_lowercase() {
        let config = from_layers(&[
            layer("[workspace]\nmode = \"per-task\"\n"),
            layer("mux = \"tuios\"\n[workspace]\nmode = \"current\"\n"),
        ]);
        assert_eq!(config.workspace.mode, WorkspaceMode::Current);
        assert_eq!(config.mux, Some(Mux::Tuios));
        assert_eq!(
            from_layers(&[layer("[workspace]\nmode = \"per-task\"\n")])
                .workspace
                .mode,
            WorkspaceMode::PerTask
        );
    }

    #[test]
    fn loads_real_global_and_repo_files_end_to_end() {
        let dir = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(error) => panic!("cannot create temp dir: {error}"),
        };
        let global = dir.path().join("global.toml");
        let repo = dir.path().join("repo.toml");
        if let Err(error) = std::fs::write(&global, GLOBAL) {
            panic!("cannot write global config: {error}");
        }
        if let Err(error) = std::fs::write(
            &repo,
            r#"
            [roster]
            orchestrator = "codex/gpt-6"
            reviewer = "claude/claude-opus-5-5"

            [workspace]
            lead_ratio = 0.7

            [gates]
            integration = ["cargo test --workspace -- --test-threads=1"]
            "#,
        ) {
            panic!("cannot write repo config: {error}");
        }
        let missing = dir.path().join("absent.toml");
        let config = match load(Some(&global), Some(&repo)) {
            Ok(config) => config,
            Err(error) => panic!("load failed: {error}"),
        };
        assert_eq!(config.roster.orchestrator.as_deref(), Some("codex/gpt-6"));
        assert_eq!(
            config.roster.reviewer.as_deref(),
            Some("claude/claude-opus-5-5")
        );
        assert!((config.workspace.lead_ratio - 0.7).abs() < f32::EPSILON);
        // The repo layer did not touch gates.worker, so the global list stands.
        assert_eq!(config.gates.worker, vec!["cargo test -p shprd-core"]);
        assert_eq!(
            config.gates.integration,
            vec!["cargo test --workspace -- --test-threads=1"]
        );
        assert_eq!(config.skills.tags.len(), 2);
        // A path that does not exist is skipped, not an error.
        let skipped = match load(Some(&global), Some(&missing)) {
            Ok(config) => config,
            Err(error) => panic!("missing repo file should be skipped: {error}"),
        };
        assert_eq!(
            skipped.roster.orchestrator.as_deref(),
            Some("claude/claude-opus-5-5")
        );
        assert_eq!(
            skipped.workspace,
            Workspace {
                mode: WorkspaceMode::PerRepo,
                lead_ratio: 0.6
            }
        );
    }
}
