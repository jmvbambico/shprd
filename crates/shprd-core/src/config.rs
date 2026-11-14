//! Layered configuration.
//!
//! shprd reads a global file (`~/.config/shprd/config.toml`, written by the
//! setup wizard) and an optional per-repo override (`<repo>/.shprd/config.toml`).
//! The repo file overrides the global file *field by field*: a scalar or list
//! present in the repo layer replaces the global value, and maps merge by key.
//!
//! The `[tick]` section is what the service's advance pass reads:
//!
//! ```toml
//! [tick]
//! max_running = 4      # tasks of one repository running at once (default 4)
//! interval_secs = 15   # seconds between two passes (default 15)
//! ```
//!
//! Both are documented defaults rather than footguns: a repository that wants
//! every task of its plan running says so with `max_running`, and a test that
//! wants to watch two passes back to back sets `interval_secs = 1`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
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

/// When a task's pull request may be merged.
///
/// The policy is one answer for every landing: `ask` waits for the human,
/// `checks` waits for the forge's checks, `auto` merges as soon as the pull
/// request is open. A protected base branch outranks both of the automatic
/// ones (see [`Land::policy_for`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MergePolicy {
    /// Merge only after a human approved the pull request.
    #[default]
    Ask,
    /// Merge once the pull request's checks pass.
    Checks,
    /// Merge as soon as the pull request is open.
    ///
    /// `allow` is the name this policy had before `checks` existed: a config
    /// file that still says `allow` reads as `auto`, because that is what it
    /// asked for.
    #[serde(alias = "allow")]
    Auto,
}

/// How a pull request is landed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Land {
    /// The policy for a base branch that is not protected.
    pub merge: MergePolicy,
    /// Branches no policy merges into without a human's approval.
    ///
    /// The list is a guard against automatic merges into the branch that
    /// ships, so it outranks `checks` and `auto` rather than being checked by
    /// them: landing into one of these always waits for the human.
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

impl Land {
    /// The policy that applies to a pull request into `base`.
    ///
    /// A protected branch always asks. Every other branch uses the configured
    /// policy.
    pub fn policy_for(&self, base: &str) -> MergePolicy {
        if self.protected.iter().any(|branch| branch == base) {
            MergePolicy::Ask
        } else {
            self.merge
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

/// The tick: how often the service advances the world, and how far.
///
/// The values are documented defaults rather than secrets: a repository that
/// wants every task of its plan running at once says so here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tick {
    /// How many tasks of one repository may be running at the same time.
    ///
    /// The tick starts a queued task only while fewer than this many tasks
    /// count as running, so one plan of twenty tasks does not put twenty agents
    /// on one machine. A task that reported and waits on its landing is not
    /// running any more and frees its slot.
    pub max_running: u32,
    /// How long the service waits between passes, in seconds.
    ///
    /// Shepherdr's own tick runs every fifteen seconds, which is short enough
    /// that a human never waits on it and long enough that a pass costs a fold
    /// of the log every time rather than a spin.
    pub interval_secs: u64,
}

impl Default for Tick {
    fn default() -> Self {
        Self {
            max_running: 4,
            interval_secs: 15,
        }
    }
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
    /// How often the service advances the world, and how far.
    pub tick: Tick,
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
            tick: Tick::default(),
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
        if let Some(tick) = &layer.tick {
            if let Some(max_running) = tick.max_running {
                self.tick.max_running = max_running;
            }
            if let Some(interval_secs) = tick.interval_secs {
                self.tick.interval_secs = interval_secs;
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
    /// The `[tick]` section.
    pub tick: Option<TickLayer>,
}

impl ConfigLayer {
    /// Project the keys this layer holds under their pre-M9 names onto the
    /// settings model.
    ///
    /// `[land] merge`, `[tick] max_running`, `[tick] interval_secs`,
    /// `[janitor] archive_days`, `[janitor] namespace`, `[review] must_fix` and
    /// `[review] rounds` predate `settings.toml`. They are folded into the same
    /// `tasks.*` keys
    /// ([`crate::settings::TaskKey`]) so a person who has not migrated resolves
    /// to exactly the values their config file asked for; the settings file
    /// sits above this layer and wins where both set the same key.
    ///
    /// `[roster] reviewer = "<program>/<model>"` is the same for the reviewer
    /// role: it supplies `roles.reviewer.agent` and `roles.reviewer.model`, and
    /// the reviewer's own `[roster.effort]` entry supplies
    /// `roles.reviewer.effort`. A review reads its reviewer through the settings
    /// resolution, so this is the lower fallback layer a person who set the
    /// reviewer the old way keeps.
    ///
    /// What is not projected is what has no settings key: the rest of
    /// `[roster]` (the coordinator, the worker list and the auto-approve
    /// switch), `[skills]`, `[workspace]`, `[gates]`, `mux` and `[land]
    /// protected` are still read by the verbs that need them, straight from
    /// this file.
    pub fn settings_projection(&self) -> crate::settings::Layer {
        use crate::settings::{Key, Layer, RoleField, TaskKey, Value, merge_name, severity_name};

        let mut layer = Layer::new();
        if let Some(roster) = &self.roster {
            if let Some(written) = roster.reviewer.as_deref() {
                let (program, model) = split_agent(written);
                if let Some(program) = program {
                    layer.set(reviewer_key(RoleField::Agent), Value::Text(program));
                }
                if let Some(model) = model {
                    layer.set(reviewer_key(RoleField::Model), Value::Text(model));
                }
                let effort = roster
                    .effort
                    .as_ref()
                    .and_then(|effort| effort.get(written))
                    .filter(|effort| !effort.trim().is_empty());
                if let Some(effort) = effort {
                    layer.set(reviewer_key(RoleField::Effort), Value::Text(effort.clone()));
                }
            }
        }
        if let Some(merge) = self.land.as_ref().and_then(|land| land.merge) {
            layer.set(
                Key::Task(TaskKey::Merge),
                Value::Text(merge_name(merge).to_owned()),
            );
        }
        if let Some(tick) = &self.tick {
            if let Some(max_running) = tick.max_running {
                layer.set(
                    Key::Task(TaskKey::MaxRunning),
                    Value::Number(i64::from(max_running)),
                );
            }
            if let Some(interval_secs) = tick.interval_secs {
                layer.set(
                    Key::Task(TaskKey::TickSecs),
                    Value::Number(i64::try_from(interval_secs).unwrap_or(i64::MAX)),
                );
            }
        }
        if let Some(janitor) = &self.janitor {
            if let Some(archive_days) = janitor.archive_days {
                layer.set(
                    Key::Task(TaskKey::ArchiveAfter),
                    Value::Number(i64::from(archive_days)),
                );
            }
            if let Some(namespace) = &janitor.namespace {
                layer.set(
                    Key::Task(TaskKey::ArchiveNamespace),
                    Value::Text(namespace.clone()),
                );
            }
        }
        if let Some(review) = &self.review {
            if let Some(must_fix) = review.must_fix {
                layer.set(
                    Key::Task(TaskKey::MustFix),
                    Value::Text(severity_name(must_fix).to_owned()),
                );
            }
            if let Some(rounds) = review.rounds {
                layer.set(
                    Key::Task(TaskKey::ReviewRounds),
                    Value::Number(i64::from(rounds)),
                );
            }
        }
        layer
    }
}

/// The settings key for one field of the reviewer role.
fn reviewer_key(field: crate::settings::RoleField) -> crate::settings::Key {
    crate::settings::Key::Role {
        id: "reviewer".to_owned(),
        field,
    }
}

/// Splits an old `program/model` agent string into its two halves.
///
/// The model may itself hold `/` (`pi/deepseek/deepseek-v4.1-flash`), which is
/// why only the first separator splits. An empty program or an empty model is
/// treated as absent, so a malformed line projects nothing rather than an empty
/// setting.
fn split_agent(written: &str) -> (Option<String>, Option<String>) {
    let trimmed = written.trim();
    if trimmed.is_empty() {
        return (None, None);
    }
    match trimmed.split_once('/') {
        Some((program, model)) => (
            Some(program.trim().to_owned()).filter(|program| !program.is_empty()),
            Some(model.trim().to_owned()).filter(|model| !model.is_empty()),
        ),
        None => (Some(trimmed.to_owned()), None),
    }
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
    /// The policy for a base branch that is not protected.
    pub merge: Option<MergePolicy>,
    /// Branches no policy merges into without a human's approval.
    pub protected: Option<Vec<String>>,
}

/// The `[tick]` section of a config file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TickLayer {
    /// How many tasks of one repository may run at once.
    pub max_running: Option<u32>,
    /// Seconds between two passes of the service's tick.
    pub interval_secs: Option<u64>,
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
        if self.tick.max_running < 1 {
            return Err(ConfigError::Invalid {
                field: "tick.max_running",
                reason: format!("must be at least 1, got {}", self.tick.max_running),
            });
        }
        if self.tick.interval_secs < 1 {
            return Err(ConfigError::Invalid {
                field: "tick.interval_secs",
                reason: format!("must be at least 1, got {}", self.tick.interval_secs),
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
    fn the_old_keys_project_onto_the_settings_model() {
        use crate::settings::{Key, TaskKey, Value};

        let global = layer(
            r#"
            [land]
            merge = "checks"

            [tick]
            max_running = 2
            interval_secs = 30

            [janitor]
            archive_days = 30
            namespace = "old/"

            [review]
            must_fix = "minor"
            rounds = 3
            "#,
        );
        let projected = global.settings_projection();
        let cases = [
            (TaskKey::Merge, Value::Text("checks".to_owned())),
            (TaskKey::MaxRunning, Value::Number(2)),
            (TaskKey::TickSecs, Value::Number(30)),
            (TaskKey::ArchiveAfter, Value::Number(30)),
            (TaskKey::ArchiveNamespace, Value::Text("old/".to_owned())),
            (TaskKey::MustFix, Value::Text("minor".to_owned())),
            (TaskKey::ReviewRounds, Value::Number(3)),
        ];
        for (key, expected) in cases {
            assert_eq!(
                projected.get(&Key::Task(key)),
                Some(&expected),
                "{} should project",
                key.name()
            );
        }
        assert_eq!(
            projected.len(),
            7,
            "the projection sets exactly the keys the file names"
        );

        // The old reviewer line and its effort become the reviewer role's pick.
        let reviewer = layer(
            r#"
            [roster]
            reviewer = "pi/deepseek/deepseek-v4.1-flash"

            [roster.effort]
            "pi/deepseek/deepseek-v4.1-flash" = "low"
            "#,
        )
        .settings_projection();
        let role = |field| {
            Key::Role {
                id: "reviewer".to_owned(),
                field,
            }
        };
        assert_eq!(
            reviewer.get(&role(crate::settings::RoleField::Agent)),
            Some(&Value::Text("pi".to_owned()))
        );
        assert_eq!(
            reviewer.get(&role(crate::settings::RoleField::Model)),
            Some(&Value::Text("deepseek/deepseek-v4.1-flash".to_owned())),
            "the model keeps every `/` after the program's"
        );
        assert_eq!(
            reviewer.get(&role(crate::settings::RoleField::Effort)),
            Some(&Value::Text("low".to_owned()))
        );
        // A reviewer named without a model projects the program alone.
        let program_only = layer("[roster]\nreviewer = \"codex\"\n").settings_projection();
        assert_eq!(
            program_only.get(&role(crate::settings::RoleField::Agent)),
            Some(&Value::Text("codex".to_owned()))
        );
        assert!(
            program_only
                .get(&role(crate::settings::RoleField::Model))
                .is_none(),
            "no model was named"
        );

        // A file that names none of them projects to nothing, so the settings
        // model's built-in values stand.
        assert!(ConfigLayer::default().settings_projection().is_empty());
        // `allow` was the old word for `auto` and projects as the same request.
        let allow = layer("[land]\nmerge = \"allow\"\n");
        assert_eq!(
            allow.settings_projection().get(&Key::Task(TaskKey::Merge)),
            Some(&Value::Text("auto".to_owned()))
        );
    }

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
        assert_eq!(config.tick, super::Tick::default());
        assert_eq!(config.tick.max_running, 4);
        assert_eq!(config.tick.interval_secs, 15);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn tick_section_is_merged_field_by_field() {
        let global = layer(
            r#"
            [tick]
            max_running = 2
            "#,
        );
        let config = from_layers(std::slice::from_ref(&global));
        assert_eq!(config.tick.max_running, 2);
        assert_eq!(
            config.tick.interval_secs, 15,
            "the defaults survive a section that only sets the cap"
        );
        let repo = layer(
            r#"
            [tick]
            interval_secs = 30
            "#,
        );
        let config = from_layers(&[global, repo]);
        assert_eq!(config.tick.max_running, 2);
        assert_eq!(config.tick.interval_secs, 30);
    }

    #[test]
    fn a_tick_setting_of_zero_is_invalid() {
        let mut config = Config::default();
        config.tick.max_running = 0;
        match config.validate() {
            Err(ConfigError::Invalid { field, .. }) => assert_eq!(field, "tick.max_running"),
            other => panic!("expected a tick.max_running refusal, got {other:?}"),
        }
        let mut config = Config::default();
        config.tick.interval_secs = 0;
        match config.validate() {
            Err(ConfigError::Invalid { field, .. }) => assert_eq!(field, "tick.interval_secs"),
            other => panic!("expected a tick.interval_secs refusal, got {other:?}"),
        }
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
            merge = "checks"
            "#,
        );
        let config = from_layers(std::slice::from_ref(&global));
        assert_eq!(config.land.merge, MergePolicy::Checks);
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
        assert_eq!(config.land.merge, MergePolicy::Checks);
        assert_eq!(config.land.protected, vec!["trunk".to_string()]);
    }

    #[test]
    fn the_policy_that_was_called_allow_reads_as_auto() {
        // A config file written before `checks` existed asked for an automatic
        // merge; `allow` is that same request under its old name.
        let old = layer(
            r#"
            [land]
            merge = "allow"
            "#,
        );
        assert_eq!(
            from_layers(std::slice::from_ref(&old)).land.merge,
            MergePolicy::Auto
        );
        let auto = layer(
            r#"
            [land]
            merge = "auto"
            "#,
        );
        assert_eq!(
            from_layers(std::slice::from_ref(&auto)).land.merge,
            MergePolicy::Auto
        );
    }

    #[test]
    fn a_protected_branch_always_asks() {
        let land = Land {
            merge: MergePolicy::Auto,
            protected: vec!["main".to_string()],
        };
        assert_eq!(land.policy_for("main"), MergePolicy::Ask);
        assert_eq!(land.policy_for("integration/m8"), MergePolicy::Auto);
        let checks = Land {
            merge: MergePolicy::Checks,
            protected: Vec::new(),
        };
        assert_eq!(checks.policy_for("main"), MergePolicy::Checks);
    }

    #[test]
    fn an_unknown_merge_policy_is_rejected() {
        let parsed: Result<ConfigLayer, _> = toml::from_str(
            r#"
            [land]
            merge = "always"
            "#,
        );
        assert!(parsed.is_err(), "only ask, checks and auto are policies");
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
