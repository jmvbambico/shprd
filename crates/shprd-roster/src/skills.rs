//! Skills discovery: the curated compatibility list, the per-CLI visibility
//! rules, and what a CLI actually sees in the environment it is launched with.
//!
//! Users manage their global skills with `npx skills`, which writes
//! `~/.agents/skills` and keeps `~/.agents/.skill-lock.json` (each entry holds
//! the upstream `source` and the `skillFolderHash` of the version on disk).
//! Each CLI also reads its own native skill directories, and some CLIs skip
//! directories depending on the environment variables they start with. The
//! curated verdicts live in `data/skills/compat.toml` (maintainer-only) and
//! the environment rules in `data/skills/visibility.toml`.
//!
//! Everything here is read-only: no skill directory is ever written, and only
//! the YAML frontmatter of a `SKILL.md` is touched.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// An error while loading or checking skills data.
#[derive(Debug, thiserror::Error)]
pub enum SkillsError {
    /// A skills data file could not be read.
    #[error("failed to read skills file {path}: {source}")]
    ReadFile {
        /// The file that failed.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// A skills data file is not valid TOML or does not match the schema.
    #[error("failed to parse skills file {path}: {source}")]
    Parse {
        /// The file that failed.
        path: PathBuf,
        /// The underlying TOML error.
        #[source]
        source: toml::de::Error,
    },
    /// The `npx skills` lock file is not valid JSON for its schema.
    #[error("failed to parse skills lock {path}: {source}")]
    ParseJson {
        /// The file that failed.
        path: PathBuf,
        /// The underlying JSON error.
        #[source]
        source: serde_json::Error,
    },
    /// Two entries of the compatibility list share a name.
    #[error("duplicate skill name {name} in {path}")]
    DuplicateName {
        /// The repeated name.
        name: String,
        /// The file that repeats it.
        path: PathBuf,
    },
    /// A compatibility entry has an empty `reviewed_hash`.
    #[error("skill {name} in {path} has an empty reviewed_hash")]
    EmptyHash {
        /// The entry that failed.
        name: String,
        /// The file that holds it.
        path: PathBuf,
    },
    /// A skills directory could not be read.
    #[error("failed to read skills directory {path}: {source}")]
    ReadDir {
        /// The directory that failed.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },
}

/// The curated skills compatibility list from `data/skills/compat.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompatList {
    /// Schema version of the file.
    pub schema: u32,
    /// One entry per skill offered in setup, in file order.
    #[serde(default, rename = "skill")]
    pub skills: Vec<CompatEntry>,
    /// One entry per CLI-native skill that conflicts with shprd.
    #[serde(default, rename = "native")]
    pub natives: Vec<NativeEntry>,
}

/// One curated verdict for a skill managed by `npx skills`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompatEntry {
    /// Skill directory name, which is also the lock-file key.
    pub name: String,
    /// Upstream repository the skill is installed from, e.g. `obra/superpowers`.
    pub source: String,
    /// What setup does with the skill.
    pub verdict: Verdict,
    /// Roles the skill is offered to.
    #[serde(default)]
    pub roles: Vec<Role>,
    /// Task tags that pull the skill in automatically.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Why the verdict is what it is, shown to the user.
    #[serde(default)]
    pub reason: String,
    /// The `skillFolderHash` the entry was reviewed at.
    pub reviewed_hash: String,
}

/// What setup does with a curated skill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    /// Offered and pre-ticked for the listed roles.
    Allow,
    /// Offered but not pre-ticked.
    OptIn,
    /// Not offered by default.
    Deny,
}

/// A role a curated skill is offered to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// The orchestrator session.
    Orchestrator,
    /// A dispatched worker session.
    Worker,
    /// A cross-vendor reviewer session.
    Reviewer,
}

/// A CLI-native skill that is never hidden but is flagged in setup.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeEntry {
    /// Skill directory name.
    pub name: String,
    /// Why it conflicts with shprd.
    #[serde(default)]
    pub reason: String,
}

impl CompatList {
    /// Load and validate a compatibility list.
    ///
    /// Validation rejects unknown fields, repeated `name`s, empty
    /// `reviewed_hash` values and an unknown `schema` version.
    pub fn load(path: &Path) -> Result<Self, SkillsError> {
        let text = fs::read_to_string(path).map_err(|source| SkillsError::ReadFile {
            path: path.to_path_buf(),
            source,
        })?;
        let list: Self = toml::from_str(&text).map_err(|source| SkillsError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
        list.validate(path)?;
        Ok(list)
    }

    /// Check the invariants a curated list must hold.
    pub fn validate(&self, path: &Path) -> Result<(), SkillsError> {
        let mut seen = BTreeMap::new();
        for entry in &self.skills {
            if seen.insert(entry.name.as_str(), ()).is_some() {
                return Err(SkillsError::DuplicateName {
                    name: entry.name.clone(),
                    path: path.to_path_buf(),
                });
            }
            if entry.reviewed_hash.trim().is_empty() {
                return Err(SkillsError::EmptyHash {
                    name: entry.name.clone(),
                    path: path.to_path_buf(),
                });
            }
        }
        for native in &self.natives {
            if seen.insert(native.name.as_str(), ()).is_some() {
                return Err(SkillsError::DuplicateName {
                    name: native.name.clone(),
                    path: path.to_path_buf(),
                });
            }
        }
        Ok(())
    }

    /// The curated entry for a skill name, if the list has one.
    pub fn entry(&self, name: &str) -> Option<&CompatEntry> {
        self.skills.iter().find(|entry| entry.name == name)
    }

    /// Whether the list marks a skill as CLI-native.
    pub fn is_native(&self, name: &str) -> bool {
        self.natives.iter().any(|native| native.name == name)
    }
}

/// The environment-dependent visibility rules from `data/skills/visibility.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisibilityRules {
    /// Schema version of the file.
    pub schema: u32,
    /// One entry per environment variable that hides skill directories.
    #[serde(default, rename = "rule")]
    pub rules: Vec<EnvRule>,
    /// Folder names inside a skills directory that are junk, not skills.
    #[serde(default, rename = "ignore")]
    pub ignores: Vec<IgnorePattern>,
}

/// One environment variable that hides skill directories for one CLI.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvRule {
    /// Harness id from `data/harness/<id>.toml`, e.g. `opencode`.
    pub cli: String,
    /// Environment variable that switches the directories off.
    pub env: String,
    /// Directories it hides, in `skills.home_dirs`/`skills.project_dirs` spelling.
    pub hides: Vec<String>,
}

/// A folder name inside a skills directory that is junk rather than a skill.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IgnorePattern {
    /// Directory name to skip, e.g. `.trash`.
    pub pattern: String,
}

impl VisibilityRules {
    /// Load and validate visibility rules.
    pub fn load(path: &Path) -> Result<Self, SkillsError> {
        let text = fs::read_to_string(path).map_err(|source| SkillsError::ReadFile {
            path: path.to_path_buf(),
            source,
        })?;
        toml::from_str(&text).map_err(|source| SkillsError::Parse {
            path: path.to_path_buf(),
            source,
        })
    }

    /// The ignore patterns as plain directory names, in file order.
    pub fn ignore_patterns(&self) -> Vec<String> {
        self.ignores
            .iter()
            .map(|ignore| ignore.pattern.clone())
            .collect()
    }

    /// The rules that name one CLI.
    pub fn rules_for<'a>(&'a self, cli: &'a str) -> impl Iterator<Item = &'a EnvRule> + 'a {
        self.rules.iter().filter(move |rule| rule.cli == cli)
    }

    /// Whether a variable value means "off": unset, empty, or `0`.
    pub fn env_disables(value: Option<&String>) -> bool {
        match value {
            None => false,
            Some(value) => {
                let value = value.trim();
                !value.is_empty() && value != "0"
            }
        }
    }

    /// The directories a CLI's launch environment hides, with the variable
    /// that hides each of them.
    pub fn hidden(&self, cli: &str, env: &BTreeMap<String, String>) -> Vec<(String, String)> {
        let mut hidden: Vec<(String, String)> = Vec::new();
        for rule in self.rules_for(cli) {
            if !Self::env_disables(env.get(&rule.env)) {
                continue;
            }
            for dir in &rule.hides {
                if !hidden.iter().any(|(known, _)| known == dir) {
                    hidden.push((dir.clone(), rule.env.clone()));
                }
            }
        }
        hidden
    }
}

/// A skill found on disk, from one skills directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundSkill {
    /// Directory name, which is the skill's id.
    pub name: String,
    /// `name:` from the `SKILL.md` frontmatter, if present and non-empty.
    pub front_name: Option<String>,
    /// `description:` from the `SKILL.md` frontmatter, if present.
    pub description: Option<String>,
    /// The skill directory, i.e. the entry inside the skills directory.
    pub path: PathBuf,
    /// The skills directory the scan started from.
    pub dir: PathBuf,
    /// Whether the entry itself is a symlink.
    pub symlink: bool,
    /// Where the symlink points, when it is one.
    pub target: Option<PathBuf>,
}

impl FoundSkill {
    /// The name to show: the frontmatter `name:` when it has one, else the
    /// directory name.
    pub fn display_name(&self) -> &str {
        self.front_name.as_deref().unwrap_or(&self.name)
    }
}

/// One entry of `~/.agents/.skill-lock.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockEntry {
    /// Upstream repository the skill was installed from.
    pub source: String,
    /// Hash of the installed skill folder, compared against `reviewed_hash`.
    pub folder_hash: String,
}

/// The fields of a lock-file entry that shprd reads.
#[derive(Debug, Deserialize)]
struct LockEntryFile {
    #[serde(default)]
    source: String,
    #[serde(default, rename = "skillFolderHash")]
    folder_hash: String,
}

/// The part of `~/.agents/.skill-lock.json` that shprd reads.
#[derive(Debug, Deserialize)]
struct LockFile {
    #[serde(default, rename = "skills")]
    skills: BTreeMap<String, LockEntryFile>,
}

/// Read the `npx skills` lock file under `home`.
///
/// A missing file is not an error: it means nothing is installed.
pub fn read_lock(home: &Path) -> Result<BTreeMap<String, LockEntry>, SkillsError> {
    let path = home.join(".agents").join(".skill-lock.json");
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(source) => {
            return Err(SkillsError::ReadFile {
                path: path.clone(),
                source,
            });
        }
    };
    let file: LockFile = serde_json::from_str(&text).map_err(|source| SkillsError::ParseJson {
        path: path.clone(),
        source,
    })?;
    Ok(file
        .skills
        .into_iter()
        .map(|(name, entry)| {
            (
                name,
                LockEntry {
                    source: entry.source,
                    folder_hash: entry.folder_hash,
                },
            )
        })
        .collect())
}
