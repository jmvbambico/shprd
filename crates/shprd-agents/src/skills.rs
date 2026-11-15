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

use crate::spec::HarnessSpec;

/// Lines of a `SKILL.md` that may hold the YAML frontmatter.
const FRONTMATTER_LINES: usize = 40;

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
    /// A skills data file declares a `schema` version shprd does not know.
    #[error("skills file {path} has unknown schema {schema}: shprd reads schema 1")]
    UnknownSchema {
        /// The file that declares it.
        path: PathBuf,
        /// The version it declares.
        schema: u32,
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
    /// The only schema version shprd understands.
    pub const SCHEMA: u32 = 1;

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
    ///
    /// A file declaring any schema but [`SCHEMA`](Self::SCHEMA) is refused
    /// rather than half-applied.
    pub fn validate(&self, path: &Path) -> Result<(), SkillsError> {
        if self.schema != Self::SCHEMA {
            return Err(SkillsError::UnknownSchema {
                path: path.to_path_buf(),
                schema: self.schema,
            });
        }
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
    /// The only schema version shprd understands.
    pub const SCHEMA: u32 = 1;

    /// Load and validate visibility rules.
    ///
    /// A file declaring any [`schema`](Self::SCHEMA) but the one shprd reads is
    /// refused rather than half-applied.
    pub fn load(path: &Path) -> Result<Self, SkillsError> {
        let text = fs::read_to_string(path).map_err(|source| SkillsError::ReadFile {
            path: path.to_path_buf(),
            source,
        })?;
        let rules: Self = toml::from_str(&text).map_err(|source| SkillsError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
        if rules.schema != Self::SCHEMA {
            return Err(SkillsError::UnknownSchema {
                path: path.to_path_buf(),
                schema: rules.schema,
            });
        }
        Ok(rules)
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

/// Scan one skills directory for skills.
///
/// Every immediate subdirectory (or symlink to one) that holds a `SKILL.md` is
/// a skill. Only the leading [`FRONTMATTER_LINES`] lines of that file are read,
/// and only its `name:` and `description:` fields: the body is never touched.
/// Entries whose name matches an ignore pattern are reported as junk and never
/// scanned; any `SKILL.md` directory underneath them is reported as junk too.
/// A directory that cannot be read is treated as absent.
pub fn scan_dir(dir: &Path, ignore: &[String]) -> (Vec<FoundSkill>, Vec<PathBuf>) {
    let mut skills = Vec::new();
    let mut junk = Vec::new();
    let read = match fs::read_dir(dir) {
        Ok(read) => read,
        Err(_) => return (skills, junk),
    };
    let mut entries: Vec<_> = read.filter_map(Result::ok).collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if ignore.iter().any(|pattern| pattern == &name) {
            collect_junk(&path, &mut junk);
            junk.push(path);
            continue;
        }
        let is_dir = fs::metadata(&path)
            .map(|meta| meta.is_dir())
            .unwrap_or(false);
        if !is_dir || !path.join("SKILL.md").is_file() {
            continue;
        }
        let symlink = fs::symlink_metadata(&path)
            .map(|meta| meta.file_type().is_symlink())
            .unwrap_or(false);
        let target = if symlink {
            fs::read_link(&path).ok()
        } else {
            None
        };
        let (front_name, description) = read_frontmatter(&path.join("SKILL.md"));
        skills.push(FoundSkill {
            name,
            front_name,
            description,
            path,
            dir: dir.to_path_buf(),
            symlink,
            target,
        });
    }
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    (skills, junk)
}

/// How deep junk collection descends, so no filesystem can cost more.
const JUNK_DEPTH: usize = 8;

/// Push every directory below `dir` that holds a `SKILL.md` onto `junk`.
///
/// A symlink is never followed, because a link loop under a junk folder would
/// otherwise walk forever, and the walk stops after [`JUNK_DEPTH`] levels.
fn collect_junk(dir: &Path, junk: &mut Vec<PathBuf>) {
    collect_junk_at(dir, junk, 0);
}

/// [`collect_junk`] with the depth reached so far.
fn collect_junk_at(dir: &Path, junk: &mut Vec<PathBuf>, depth: usize) {
    if depth >= JUNK_DEPTH {
        return;
    }
    let read = match fs::read_dir(dir) {
        Ok(read) => read,
        Err(_) => return,
    };
    for entry in read.filter_map(Result::ok) {
        let path = entry.path();
        let is_dir = fs::symlink_metadata(&path)
            .map(|meta| meta.is_dir())
            .unwrap_or(false);
        if !is_dir {
            continue;
        }
        if path.join("SKILL.md").is_file() {
            junk.push(path.clone());
        }
        collect_junk_at(&path, junk, depth + 1);
    }
}

/// Read `name:` and `description:` from the YAML frontmatter of a `SKILL.md`.
///
/// Stops after [`FRONTMATTER_LINES`] lines in every case, so a large skill file
/// costs the same as a small one. Unquoted, single-quoted and double-quoted
/// scalar values are accepted; anything else is left as written.
fn read_frontmatter(path: &Path) -> (Option<String>, Option<String>) {
    use std::io::BufRead;

    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return (None, None),
    };
    let mut name = None;
    let mut description = None;
    let mut seen_marker = false;
    for line in io::BufReader::new(file).lines().take(FRONTMATTER_LINES) {
        let line = match line {
            Ok(line) => line,
            Err(_) => break,
        };
        let trimmed = line.trim();
        if trimmed == "---" {
            if seen_marker {
                break;
            }
            seen_marker = true;
            continue;
        }
        if !seen_marker {
            continue;
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        let value = unquote(value.trim());
        match key.trim() {
            "name" if name.is_none() && !value.is_empty() => name = Some(value),
            "description" if description.is_none() && !value.is_empty() => {
                description = Some(value);
            }
            _ => {}
        }
    }
    (name, description)
}

/// Strip one layer of matching single or double quotes from a scalar.
fn unquote(value: &str) -> String {
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
        {
            return inner.to_string();
        }
    }
    value.to_string()
}

/// The skills one CLI sees when launched with a given environment.
#[derive(Debug, Clone, Default)]
pub struct CliSkills {
    /// Harness id the scan was made for.
    pub cli: String,
    /// Every skills directory the CLI reads, in spec order, with whether the
    /// launch environment leaves it visible.
    pub dirs: Vec<(PathBuf, bool)>,
    /// Skills found, one entry per name: the first directory to provide a name
    /// wins.
    pub skills: Vec<FoundSkill>,
    /// Directories hidden by an environment variable, with the variable's name.
    pub hidden_dirs: Vec<(PathBuf, String)>,
    /// Junk folders found inside a visible skills directory.
    pub junk: Vec<PathBuf>,
    /// Names provided by more than one directory, with the later directory.
    pub duplicates: Vec<(String, PathBuf)>,
}

/// Where the curated compatibility list lives inside the repository.
pub fn compat_path(repo: &Path) -> PathBuf {
    repo.join("data").join("skills").join("compat.toml")
}

/// Where the visibility rules live inside the repository.
pub fn visibility_path(repo: &Path) -> PathBuf {
    repo.join("data").join("skills").join("visibility.toml")
}

/// Expand a spec directory against the home directory and the repo root.
fn resolve(raw: &str, home: &Path, repo: Option<&Path>) -> Option<PathBuf> {
    if let Some(rest) = raw.strip_prefix("~/") {
        Some(home.join(rest))
    } else {
        repo.map(|repo| repo.join(raw))
    }
}

/// Find the skills a CLI sees, given the environment it is launched with.
///
/// Directories come from the harness spec: `skills.home_dirs` are expanded
/// under `home`, `skills.project_dirs` are joined to `repo` (skipped when no
/// repo is given). A directory is hidden when an [`EnvRule`] for this CLI names
/// it and its variable is set to a non-empty value other than `0`. A skill
/// offered by two directories counts once.
pub fn visible(
    spec: &HarnessSpec,
    home: &Path,
    repo: Option<&Path>,
    env: &BTreeMap<String, String>,
    rules: &VisibilityRules,
    ignore: &[String],
) -> CliSkills {
    let hidden = rules.hidden(&spec.id, env);
    let mut seen: BTreeMap<String, PathBuf> = BTreeMap::new();
    let mut out = CliSkills {
        cli: spec.id.clone(),
        ..CliSkills::default()
    };
    let raws = spec
        .skills
        .home_dirs
        .iter()
        .chain(spec.skills.project_dirs.iter());
    for raw in raws {
        let Some(path) = resolve(raw, home, repo) else {
            continue;
        };
        if let Some((_, variable)) = hidden.iter().find(|(dir, _)| dir == raw) {
            out.hidden_dirs.push((path.clone(), variable.clone()));
            out.dirs.push((path, false));
            continue;
        }
        out.dirs.push((path.clone(), true));
        let (found, junk) = scan_dir(&path, ignore);
        out.junk.extend(junk);
        for skill in found {
            if let Some(first) = seen.get(&skill.name) {
                out.duplicates.push((skill.name.clone(), first.clone()));
                continue;
            }
            seen.insert(skill.name.clone(), skill.path.clone());
            out.skills.push(skill);
        }
    }
    out
}

/// One row of `shprd doctor`'s skills review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    /// Skill name, i.e. the directory name.
    pub name: String,
    /// The curated verdict, absent when the list does not know the skill.
    pub verdict: Option<Verdict>,
    /// What the review found.
    pub state: ReviewState,
}

/// What a review found for one skill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewState {
    /// Curated and installed at the reviewed hash.
    Reviewed,
    /// Installed, but the folder hash moved past the reviewed one.
    ChangedUpstream {
        /// The hash in the compatibility list.
        reviewed: String,
        /// The hash on disk.
        current: String,
    },
    /// Installed but absent from the compatibility list.
    NotInCompatList,
    /// Curated but not installed.
    MissingLocally,
}

/// Compare the curated list with what `npx skills` installed.
///
/// Curated entries come first in file order, then installed skills the list
/// does not know about, in lock-file order.
pub fn review(compat: &CompatList, lock: &BTreeMap<String, LockEntry>) -> Vec<Review> {
    let mut rows = Vec::with_capacity(compat.skills.len() + lock.len());
    for entry in &compat.skills {
        let state = match lock.get(&entry.name) {
            None => ReviewState::MissingLocally,
            Some(installed) if installed.folder_hash == entry.reviewed_hash => {
                ReviewState::Reviewed
            }
            Some(installed) => ReviewState::ChangedUpstream {
                reviewed: entry.reviewed_hash.clone(),
                current: installed.folder_hash.clone(),
            },
        };
        rows.push(Review {
            name: entry.name.clone(),
            verdict: Some(entry.verdict),
            state,
        });
    }
    for name in lock.keys() {
        if compat.entry(name).is_some() {
            continue;
        }
        rows.push(Review {
            name: name.clone(),
            verdict: None,
            state: ReviewState::NotInCompatList,
        });
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `unwrap`/`expect` are denied workspace-wide, tests included.
    trait Must<T> {
        /// Return the value or panic with `what`.
        fn must(self, what: &str) -> T;
    }

    impl<T, E: std::fmt::Display> Must<T> for Result<T, E> {
        fn must(self, what: &str) -> T {
            match self {
                Ok(value) => value,
                Err(error) => panic!("{what}: {error}"),
            }
        }
    }

    impl<T> Must<T> for Option<T> {
        fn must(self, what: &str) -> T {
            match self {
                Some(value) => value,
                None => panic!("{what}: not found"),
            }
        }
    }

    /// The repository root, where the shipped data files live.
    fn repo() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    /// The shipped OpenCode spec, which reads two shared home directories.
    fn opencode_spec() -> HarnessSpec {
        let specs = crate::spec::load_dir(&repo().join("data/harness")).must("specs load");
        specs
            .into_iter()
            .find(|spec| spec.id == "opencode")
            .must("opencode spec is shipped")
    }

    /// Write a skill directory holding a frontmattered `SKILL.md`.
    fn write_skill_at(path: &Path, name: &str, description: &str) {
        fs::create_dir_all(path).must("skill dir");
        let body = format!("---\nname: {name}\ndescription: {description}\n---\n\n# {name}\n");
        fs::write(path.join("SKILL.md"), body).must("SKILL.md");
    }

    /// Write `<root>/<dir>/<name>/SKILL.md` and return the skill directory.
    fn write_skill(root: &Path, dir: &str, name: &str, description: &str) -> PathBuf {
        let path = root.join(dir).join(name);
        write_skill_at(&path, name, description);
        path
    }

    /// A lock map with one entry.
    fn lock_of(name: &str, hash: &str) -> BTreeMap<String, LockEntry> {
        let mut lock = BTreeMap::new();
        lock.insert(
            name.to_owned(),
            LockEntry {
                source: "obra/superpowers".to_owned(),
                folder_hash: hash.to_owned(),
            },
        );
        lock
    }

    /// A compat list with one curated entry.
    fn compat_of(name: &str, hash: &str) -> CompatList {
        CompatList {
            schema: 1,
            skills: vec![CompatEntry {
                name: name.to_owned(),
                source: "obra/superpowers".to_owned(),
                verdict: Verdict::Allow,
                roles: vec![Role::Worker],
                tags: Vec::new(),
                reason: "fixture".to_owned(),
                reviewed_hash: hash.to_owned(),
            }],
            natives: Vec::new(),
        }
    }

    #[test]
    fn finds_skills_including_symlinked_ones() {
        let tmp = tempfile::tempdir().must("tempdir");
        let home = tmp.path();
        write_skill(
            home,
            ".agents/skills",
            "writing-plans",
            "Turns asks into tasks.",
        );
        let real = write_skill(home, "elsewhere", "brainstorming", "Asks questions.");
        let link = home.join(".agents/skills/brainstorming");
        std::os::unix::fs::symlink(&real, &link).must("symlink");
        let (found, junk) = scan_dir(&home.join(".agents/skills"), &[]);
        assert!(junk.is_empty());
        let names: Vec<&str> = found.iter().map(|skill| skill.name.as_str()).collect();
        assert_eq!(names, ["brainstorming", "writing-plans"]);
        let linked = found
            .iter()
            .find(|skill| skill.name == "brainstorming")
            .must("symlinked skill is found");
        assert!(linked.symlink);
        assert_eq!(linked.target.as_deref(), Some(real.as_path()));
        assert_eq!(linked.display_name(), "brainstorming");
        assert_eq!(linked.description.as_deref(), Some("Asks questions."));
        assert!(!found[1].symlink);
        assert_eq!(found[1].dir, home.join(".agents/skills"));
    }

    #[test]
    fn reports_junk_dirs_and_skips_them() {
        let tmp = tempfile::tempdir().must("tempdir");
        let home = tmp.path();
        let dir = home.join(".agents/skills");
        write_skill(home, ".agents/skills", "kept", "Real skill.");
        write_skill_at(&dir.join(".trash/removed"), "removed", "Deleted skill.");
        write_skill_at(&dir.join("synced/copied"), "copied", "Mirrored skill.");
        let (found, mut junk) = scan_dir(&dir, &[".trash".to_owned(), "synced".to_owned()]);
        let names: Vec<&str> = found.iter().map(|skill| skill.name.as_str()).collect();
        assert_eq!(names, ["kept"]);
        junk.sort();
        assert_eq!(
            junk,
            [
                dir.join(".trash"),
                dir.join(".trash/removed"),
                dir.join("synced"),
                dir.join("synced/copied"),
            ]
        );
    }

    #[test]
    fn never_follows_a_symlink_loop_under_junk() {
        let tmp = tempfile::tempdir().must("tempdir");
        let home = tmp.path();
        let dir = home.join(".agents/skills");
        write_skill(home, ".agents/skills", "kept", "Real skill.");
        write_skill_at(&dir.join(".trash/removed"), "removed", "Deleted skill.");
        std::os::unix::fs::symlink("..", dir.join(".trash/loop")).must("loop symlink");
        let (found, mut junk) = scan_dir(&dir, &[".trash".to_owned()]);
        let names: Vec<&str> = found.iter().map(|skill| skill.name.as_str()).collect();
        assert_eq!(names, ["kept"]);
        junk.sort();
        assert_eq!(junk, [dir.join(".trash"), dir.join(".trash/removed")]);
    }

    #[test]
    fn reads_only_the_frontmatter_window() {
        let tmp = tempfile::tempdir().must("tempdir");
        let skill = tmp.path().join("late");
        fs::create_dir_all(&skill).must("skill dir");
        let mut body = String::from("# late\n");
        for _ in 0..FRONTMATTER_LINES {
            body.push_str("prose line\n");
        }
        body.push_str("name: late\n");
        fs::write(skill.join("SKILL.md"), body).must("SKILL.md");
        let (found, _) = scan_dir(tmp.path(), &[]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].front_name, None);
        assert_eq!(found[0].display_name(), "late");
    }

    #[test]
    fn rejects_an_unknown_visibility_schema() {
        let tmp = tempfile::tempdir().must("tempdir");
        let path = tmp.path().join("visibility.toml");
        fs::write(&path, "schema = 2\n").must("visibility.toml");
        match VisibilityRules::load(&path) {
            Err(SkillsError::UnknownSchema { schema, .. }) => assert_eq!(schema, 2),
            other => panic!("schema 2 must be refused, got {other:?}"),
        }
    }

    #[test]
    fn rejects_an_unknown_compat_schema() {
        let tmp = tempfile::tempdir().must("tempdir");
        let path = tmp.path().join("compat.toml");
        fs::write(&path, "schema = 2\n").must("compat.toml");
        match CompatList::load(&path) {
            Err(SkillsError::UnknownSchema { schema, .. }) => assert_eq!(schema, 2),
            other => panic!("schema 2 must be refused, got {other:?}"),
        }
    }

    /// The shipped visibility rules.
    fn rules() -> VisibilityRules {
        VisibilityRules::load(&visibility_path(&repo())).must("visibility rules load")
    }

    #[test]
    fn env_rules_hide_directories() {
        let tmp = tempfile::tempdir().must("tempdir");
        let home = tmp.path();
        write_skill(home, ".agents/skills", "brainstorming", "Shared skill.");
        write_skill(home, ".claude/skills", "claude-only", "Claude skill.");
        let spec = opencode_spec();
        let rules = rules();
        let mut env = BTreeMap::new();

        let plain = visible(&spec, home, None, &env, &rules, &rules.ignore_patterns());
        let names: Vec<&str> = plain.skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["brainstorming", "claude-only"]);
        assert!(plain.hidden_dirs.is_empty());
        assert!(plain.dirs.iter().all(|(_, shown)| *shown));

        env.insert(
            "OPENCODE_DISABLE_EXTERNAL_SKILLS".to_owned(),
            "0".to_owned(),
        );
        let zero = visible(&spec, home, None, &env, &rules, &rules.ignore_patterns());
        assert_eq!(zero.skills.len(), 2, "`0` does not disable anything");

        env.insert(
            "OPENCODE_DISABLE_EXTERNAL_SKILLS".to_owned(),
            "1".to_owned(),
        );
        let hidden = visible(&spec, home, None, &env, &rules, &rules.ignore_patterns());
        assert!(hidden.skills.is_empty());
        assert_eq!(
            hidden.hidden_dirs,
            [
                (
                    home.join(".agents/skills"),
                    "OPENCODE_DISABLE_EXTERNAL_SKILLS".to_owned()
                ),
                (
                    home.join(".claude/skills"),
                    "OPENCODE_DISABLE_EXTERNAL_SKILLS".to_owned()
                ),
            ]
        );
        let hidden_count = hidden.dirs.iter().filter(|(_, shown)| !*shown).count();
        assert_eq!(hidden_count, 2, "only the two hidden dirs are marked");
    }

    #[test]
    fn scans_project_directories() {
        let tmp = tempfile::tempdir().must("tempdir");
        let home = tmp.path().join("home");
        let repo = tmp.path().join("repo");
        write_skill(&repo, "skills", "repo-helper", "Project skill.");
        let mut spec = opencode_spec();
        spec.skills.home_dirs = Vec::new();
        spec.skills.project_dirs = vec!["skills".to_owned()];
        let rules = rules();
        let env = BTreeMap::new();
        let found = visible(&spec, &home, Some(&repo), &env, &rules, &[]);
        assert_eq!(found.dirs, [(repo.join("skills"), true)]);
        let names: Vec<&str> = found.skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["repo-helper"]);
    }

    #[test]
    fn duplicate_skill_counts_once() {
        let tmp = tempfile::tempdir().must("tempdir");
        let home = tmp.path();
        write_skill(home, ".agents/skills", "brainstorming", "Shared copy.");
        write_skill(home, ".claude/skills", "brainstorming", "Claude copy.");
        let spec = opencode_spec();
        let rules = rules();
        let env = BTreeMap::new();
        let found = visible(&spec, home, None, &env, &rules, &[]);
        assert_eq!(found.skills.len(), 1);
        assert_eq!(found.skills[0].dir, home.join(".agents/skills"));
        assert_eq!(
            found.duplicates,
            [(
                "brainstorming".to_owned(),
                home.join(".agents/skills/brainstorming")
            )]
        );
    }

    #[test]
    fn hash_change_is_changed_upstream() {
        let rows = review(&compat_of("brainstorming", "aaa"), &lock_of("x", "bbb"));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "brainstorming");
        assert_eq!(rows[0].verdict, Some(Verdict::Allow));
        assert_eq!(rows[0].state, ReviewState::MissingLocally);
        assert_eq!(rows[1].verdict, None);
        assert_eq!(rows[1].state, ReviewState::NotInCompatList);

        let rows = review(
            &compat_of("brainstorming", "aaa"),
            &lock_of("brainstorming", "bbb"),
        );
        assert_eq!(
            rows[0].state,
            ReviewState::ChangedUpstream {
                reviewed: "aaa".to_owned(),
                current: "bbb".to_owned(),
            }
        );
        let rows = review(
            &compat_of("brainstorming", "aaa"),
            &lock_of("brainstorming", "aaa"),
        );
        assert_eq!(rows[0].state, ReviewState::Reviewed);
    }

    #[test]
    fn reads_the_skill_lock() {
        let tmp = tempfile::tempdir().must("tempdir");
        let home = tmp.path();
        assert!(read_lock(home).must("missing lock is empty").is_empty());
        fs::create_dir_all(home.join(".agents")).must("agents dir");
        let lock = r#"{"version":1,"skills":{"brainstorming":{"source":"obra/superpowers","skillFolderHash":"abc"}}}"#;
        fs::write(home.join(".agents/.skill-lock.json"), lock).must("lock file");
        let entries = read_lock(home).must("lock loads");
        let entry = entries.get("brainstorming").must("entry is present");
        assert_eq!(entry.source, "obra/superpowers");
        assert_eq!(entry.folder_hash, "abc");
    }

    #[test]
    fn shipped_skills_data_loads() {
        let list = CompatList::load(&compat_path(&repo())).must("compat list loads");
        assert_eq!(list.schema, 1);
        assert!(list.skills.len() > 10);
        assert!(!list.natives.is_empty());
        let entry = list.entry("brainstorming").must("brainstorming is curated");
        assert_eq!(entry.verdict, Verdict::Allow);
        assert_eq!(entry.roles, [Role::Orchestrator]);
        assert!(list.is_native("herdr-projects"));

        let rules = rules();
        assert_eq!(rules.schema, 1);
        assert_eq!(rules.rules_for("opencode").count(), 2);
        let ignores = rules.ignore_patterns();
        assert_eq!(ignores, [".trash", "synced"]);
        let mut env = BTreeMap::new();
        env.insert("OPENCODE_DISABLE_CLAUDE_CODE".to_owned(), "1".to_owned());
        assert_eq!(
            rules.hidden("opencode", &env),
            [(
                "~/.claude/skills".to_owned(),
                "OPENCODE_DISABLE_CLAUDE_CODE".to_owned()
            )]
        );
        assert!(rules.hidden("claude", &env).is_empty());
    }
}
