//! The data shprd ships: the harness table, the model catalogs, the role
//! templates and the curated skills tables, and where they are read from.
//!
//! One [`DataDir`] answers for all four kinds, so a verb asks for its data
//! rather than for a directory. The resolution is decided once, at the binary's
//! edge ([`crate::harness::DATA_ENV`], then the data home, then the copy in the
//! binary):
//!
//! 1. `$SHPRD_DATA_DIR`, when a person names one: that directory is the whole
//!    table, read exactly as it stands. This is what a worker is told by its
//!    engine, and what a test points at its own fixture.
//! 2. `<data home>/shprd/`, when it holds a `harness/` directory: a person's own
//!    overrides, layered over the embedded copy file by file, so overriding one
//!    harness changes that program and nothing else.
//! 3. The copy embedded in the binary at build time (see `build.rs`), which is
//!    what makes an installed `shprd` work with no data directory at all.
//!
//! Nothing here reads an environment variable: the edge resolves the directory
//! once and passes it in, so a test that names its own home never finds the
//! developer's install.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::adapter;
use crate::families::{Families, FamilyError};
use crate::harness::Harness;
use crate::models::Catalog;
use crate::skills::{CompatList, SkillsError, VisibilityRules};
use crate::spec::{self, HarnessSpec, SpecError};

include!(concat!(env!("OUT_DIR"), "/shipped_data.rs"));

/// One kind of data shprd ships, as `data/`'s subdirectories name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    /// The per-CLI harness table: `data/harness/*.toml`.
    Harness,
    /// The model catalogs and the family table: `data/models/`.
    Models,
    /// The role templates: `data/templates/`.
    Templates,
    /// The curated skills tables: `data/skills/`.
    Skills,
}

impl Kind {
    /// Every kind, in the order a page lists them.
    pub const ALL: [Kind; 4] = [Kind::Harness, Kind::Models, Kind::Templates, Kind::Skills];

    /// The directory under `data/` this kind lives in.
    pub fn dir(self) -> &'static str {
        match self {
            Kind::Harness => "harness",
            Kind::Models => "models",
            Kind::Templates => "templates",
            Kind::Skills => "skills",
        }
    }

    /// The kind named `name`, when `name` is one of [`Kind::dir`]'s answers.
    pub fn parse(name: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|kind| kind.dir() == name)
    }
}

/// Where a [`DataDir`]'s files come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// `$SHPRD_DATA_DIR`: a directory named outright and read as it is.
    Named(PathBuf),
    /// A person's overrides under the data home, `<data home>/shprd/`, layered
    /// over the embedded copy file by file.
    Overrides(PathBuf),
    /// The copy embedded in the binary at build time.
    Embedded,
}

impl Origin {
    /// The origin as a page names it, without the directory.
    pub fn label(&self) -> &'static str {
        match self {
            Origin::Named(_) => crate::harness::DATA_ENV,
            Origin::Overrides(_) => "overrides",
            Origin::Embedded => "embedded",
        }
    }
}

/// One kind of data as this machine stands: where its files come from, how many
/// there are, and how many of them a person overrode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Files {
    /// The origin, as [`Origin::label`] names it.
    pub origin: &'static str,
    /// The directory this kind is read from, when it is read from one.
    pub dir: Option<PathBuf>,
    /// How many files the kind holds here.
    pub total: usize,
    /// How many of them come from the override directory rather than from the
    /// embedded copy.
    pub overridden: usize,
}

impl Files {
    /// How many files come from the embedded copy.
    pub fn embedded(&self) -> usize {
        self.total.saturating_sub(self.overridden)
    }

    /// The line a page prints for this kind: its size and where it came from.
    pub fn describe(&self, kind: Kind) -> String {
        let name = kind.dir();
        let place = match &self.dir {
            Some(dir) => dir.display().to_string(),
            None => "the binary".to_owned(),
        };
        match (self.origin, self.overridden) {
            // An override directory that holds nothing of this kind leaves the
            // whole kind embedded, and the line says so.
            ("embedded", _) | ("overrides", 0) => {
                format!("{name}: {} file(s), embedded", self.total)
            }
            (_, 0) => format!(
                "{name}: {} file(s) in {place} ({})",
                self.total, self.origin
            ),
            (_, overridden) => format!(
                "{name}: {} file(s): {overridden} overridden in {place}, {} embedded",
                self.total,
                self.embedded()
            ),
        }
    }
}

/// The data shprd ships, and the directory — if any — laid over it.
///
/// Built once by the binary's wiring ([`DataDir::resolve`]) and passed down, so
/// no verb and no library reads a directory variable of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataDir {
    origin: Origin,
}

impl DataDir {
    /// Resolves the data: `explicit` (the `$SHPRD_DATA_DIR` the caller read)
    /// when it is set, else `data_home`'s `shprd/` when that holds a `harness/`,
    /// else the copy embedded in the binary.
    ///
    /// `data_home` is `$XDG_DATA_HOME`, else `<home>/.local/share`; [`None`]
    /// means the caller could not resolve a home at all, and then only the
    /// embedded copy can answer.
    pub fn resolve(explicit: Option<&Path>, data_home: Option<&Path>) -> DataDir {
        let origin = match explicit {
            Some(dir) => Origin::Named(dir.to_path_buf()),
            None => match data_home.map(overrides_dir) {
                Some(dir) if dir.join(Kind::Harness.dir()).is_dir() => Origin::Overrides(dir),
                _ => Origin::Embedded,
            },
        };
        DataDir { origin }
    }

    /// Where this data comes from.
    pub fn origin(&self) -> &Origin {
        &self.origin
    }

    /// The origin as a page names it, with the directory when it has one.
    pub fn label(&self) -> String {
        match &self.origin {
            Origin::Named(dir) | Origin::Overrides(dir) => {
                format!("{} ({})", self.origin.label(), dir.display())
            }
            Origin::Embedded => "embedded in the binary".to_owned(),
        }
    }

    /// The directory this data is read from, when it is read from one.
    ///
    /// An override directory holds only the files a person changed; the rest of
    /// the data still comes from the embedded copy, so this is not the whole
    /// table and is never handed on as if it were.
    pub fn dir(&self) -> Option<&Path> {
        match &self.origin {
            Origin::Named(dir) | Origin::Overrides(dir) => Some(dir),
            Origin::Embedded => None,
        }
    }

    /// The value to hand a child process as `$SHPRD_DATA_DIR`, when there is
    /// one to hand on.
    ///
    /// Only a named directory is handed on. A child told to read an override
    /// directory alone would lose every file the binary still embeds; a child
    /// with no variable resolves the overrides itself, from the same home and
    /// data home this process read, and then finds the embedded copy under them.
    pub fn env_value(&self) -> Option<&Path> {
        match &self.origin {
            Origin::Named(dir) => Some(dir),
            Origin::Overrides(_) | Origin::Embedded => None,
        }
    }

    /// Whether `rel` (as `harness/pi.toml`) is part of this data.
    pub fn contains(&self, rel: &str) -> bool {
        match &self.origin {
            Origin::Named(dir) => dir.join(rel).is_file(),
            Origin::Overrides(dir) => dir.join(rel).is_file() || embedded(rel).is_some(),
            Origin::Embedded => embedded(rel).is_some(),
        }
    }

    /// One file's text, by its name under `data/` (`harness/pi.toml`).
    ///
    /// An override directory's copy wins over the embedded one of the same
    /// name. [`io::ErrorKind::NotFound`] means no copy exists here.
    pub fn read(&self, rel: &str) -> io::Result<String> {
        match &self.origin {
            Origin::Named(dir) => std::fs::read_to_string(dir.join(rel)),
            Origin::Overrides(dir) => match std::fs::read_to_string(dir.join(rel)) {
                Ok(text) => Ok(text),
                Err(error) if error.kind() == io::ErrorKind::NotFound => embedded_text(rel),
                Err(error) => Err(error),
            },
            Origin::Embedded => embedded_text(rel),
        }
    }

    /// The path an error message should name for `rel`: the file that would be
    /// read, or a stand-in for the embedded copy.
    pub fn path(&self, rel: &str) -> PathBuf {
        match &self.origin {
            Origin::Named(dir) => dir.join(rel),
            Origin::Overrides(dir) => {
                let file = dir.join(rel);
                if file.is_file() {
                    file
                } else {
                    embedded_path(rel)
                }
            }
            Origin::Embedded => embedded_path(rel),
        }
    }

    /// Every file of `kind`, by its name under `data/`, sorted, with an override
    /// file replacing the embedded one of the same name.
    pub fn files(&self, kind: Kind) -> Vec<String> {
        match &self.origin {
            Origin::Named(dir) => under(kind, &walk(&dir.join(kind.dir()))),
            Origin::Overrides(dir) => {
                let mut names = under(kind, &embedded_names(kind));
                for name in walk(&dir.join(kind.dir())) {
                    let rel = format!("{}/{}", kind.dir(), name);
                    if !names.contains(&rel) {
                        names.push(rel);
                    }
                }
                names.sort();
                names
            }
            Origin::Embedded => under(kind, &embedded_names(kind)),
        }
    }

    /// One kind's files and where they come from, for a page to print.
    pub fn summary(&self, kind: Kind) -> Files {
        let shipped = embedded_names(kind);
        let (total, overridden) = match &self.origin {
            Origin::Named(dir) => (walk(&dir.join(kind.dir())).len(), 0),
            Origin::Embedded => (shipped.len(), 0),
            Origin::Overrides(dir) => {
                let changed = walk(&dir.join(kind.dir()));
                let replaced = changed.iter().filter(|name| shipped.contains(name)).count();
                (changed.len() + shipped.len() - replaced, changed.len())
            }
        };
        Files {
            origin: self.origin.label(),
            dir: self.dir().map(|dir| dir.join(kind.dir())),
            total,
            overridden,
        }
    }

    /// The harness table: every `harness/*.toml`, checked.
    ///
    /// The same rules as [`spec::load_dir`]: unknown keys are refused by the
    /// TOML schema, [`HarnessSpec::validate`] checks what the type cannot
    /// express, and two files naming one id are refused.
    pub fn harness_specs(&self) -> Result<Vec<HarnessSpec>, SpecError> {
        let mut entries = Vec::new();
        let prefix = format!("{}/", Kind::Harness.dir());
        for rel in self.files(Kind::Harness) {
            let Some(name) = rel.strip_prefix(prefix.as_str()) else {
                continue;
            };
            if name.contains('/') || !name.ends_with(".toml") {
                continue;
            }
            let path = self.path(&rel);
            let text = self.read(&rel).map_err(|source| SpecError::ReadFile {
                path: path.clone(),
                source,
            })?;
            entries.push((path, text));
        }
        spec::load_entries(entries)
    }

    /// One program's harness, by id, checked and naming the file it came from.
    pub fn harness(&self, id: &str) -> Result<Harness, SpecError> {
        let rel = format!("{}/{id}.toml", Kind::Harness.dir());
        let path = self.path(&rel);
        let text = self.read(&rel).map_err(|source| SpecError::ReadFile {
            path: path.clone(),
            source,
        })?;
        Harness::parse(&path, &text)
    }

    /// The model family table, `models/families.toml`.
    pub fn families(&self) -> Result<Families, FamilyError> {
        let rel = "models/families.toml";
        let path = self.path(rel);
        let text = self.read(rel).map_err(|source| FamilyError::ReadFile {
            path: path.clone(),
            source,
        })?;
        Families::parse(&path, &text)
    }

    /// The curated skills compatibility list, `skills/compat.toml`.
    pub fn compat(&self) -> Result<CompatList, SkillsError> {
        let rel = "skills/compat.toml";
        let path = self.path(rel);
        let text = self.read(rel).map_err(|source| SkillsError::ReadFile {
            path: path.clone(),
            source,
        })?;
        CompatList::parse(&path, &text)
    }

    /// The curated skills visibility rules, `skills/visibility.toml`.
    pub fn visibility(&self) -> Result<VisibilityRules, SkillsError> {
        let rel = "skills/visibility.toml";
        let path = self.path(rel);
        let text = self.read(rel).map_err(|source| SkillsError::ReadFile {
            path: path.clone(),
            source,
        })?;
        VisibilityRules::parse(&path, &text)
    }

    /// A program's model catalog for a check that may not fail.
    ///
    /// The static list is read from this data; a program that lists its models
    /// itself is asked, with `timeout`. [`None`] when the program has no catalog
    /// or the call failed, because everything this feeds is a hint.
    pub fn catalog(
        &self,
        spec: &HarnessSpec,
        path_env: &str,
        timeout: Duration,
    ) -> Option<Catalog> {
        adapter::catalog(spec, self, path_env, timeout)
    }
}

/// The directory a person's overrides live in: `<data home>/shprd/`.
fn overrides_dir(data_home: &Path) -> PathBuf {
    data_home.join("shprd")
}

/// The embedded copy of `rel`, when the binary holds one.
fn embedded(rel: &str) -> Option<&'static str> {
    EMBEDDED
        .iter()
        .find(|(name, _)| *name == rel)
        .map(|(_, text)| *text)
}

/// The embedded copy of `rel`, or the error that says there is none.
fn embedded_text(rel: &str) -> io::Result<String> {
    match embedded(rel) {
        Some(text) => Ok(text.to_owned()),
        None => Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{rel} is not in the data embedded in the binary"),
        )),
    }
}

/// The stand-in path an error message names for an embedded file.
fn embedded_path(rel: &str) -> PathBuf {
    Path::new("<embedded>").join(rel)
}

/// The embedded copy's file names under `kind`, relative to that directory.
fn embedded_names(kind: Kind) -> Vec<String> {
    let prefix = format!("{}/", kind.dir());
    EMBEDDED
        .iter()
        .filter_map(|(name, _)| name.strip_prefix(prefix.as_str()))
        .map(str::to_owned)
        .collect()
}

/// Prefix names from a walk of `kind`'s directory with the kind's own name.
fn under(kind: Kind, names: &[String]) -> Vec<String> {
    let mut names: Vec<String> = names
        .iter()
        .map(|name| format!("{}/{}", kind.dir(), name))
        .collect();
    names.sort();
    names
}

/// Every file under `dir`, by its name relative to `dir`, sorted.
fn walk(dir: &Path) -> Vec<String> {
    let mut names = Vec::new();
    collect(dir, "", &mut names);
    names.sort();
    names
}

/// The recursive half of [`walk`].
fn collect(dir: &Path, prefix: &str, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        let relative = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        if path.is_dir() {
            collect(&path, &relative, out);
        } else {
            out.push(relative);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::path::{Path, PathBuf};

    use tempfile::TempDir;

    use super::{DataDir, Kind, Origin};

    /// A data home no person has ever written to.
    fn empty_data_home() -> TempDir {
        TempDir::new().expect("a temp data home")
    }

    /// A data home holding the person's own override of `pi.toml`.
    fn data_home_with_pi_override() -> (TempDir, String) {
        let home = TempDir::new().expect("a temp data home");
        let harness = home.path().join("shprd").join("harness");
        std::fs::create_dir_all(&harness).expect("mkdir harness");
        let mut text = embedded_pi();
        text = text.replace("name = \"Pi\"", "name = \"Pi (mine)\"");
        std::fs::write(harness.join("pi.toml"), &text).expect("write the override");
        (home, text)
    }

    /// The shipped pi harness, as the binary holds it.
    fn embedded_pi() -> String {
        DataDir::resolve(None, None)
            .read("harness/pi.toml")
            .expect("the embedded pi harness")
    }

    /// How many harness files the binary ships.
    fn embedded_names_count() -> usize {
        DataDir::resolve(None, None).files(Kind::Harness).len()
    }

    /// An embedded-only data directory — no environment, no overrides — still
    /// has pi's harness, its families and its skills tables.
    #[test]
    fn the_embedded_copy_answers_with_no_data_directory_at_all() {
        let home = empty_data_home();
        let data = DataDir::resolve(None, Some(home.path()));
        assert_eq!(data.origin(), &Origin::Embedded);
        assert_eq!(data.dir(), None);
        assert_eq!(data.env_value(), None);

        let pi = data.harness("pi").expect("pi's harness is embedded");
        assert_eq!(pi.id(), "pi");
        assert_eq!(pi.path(), Path::new("<embedded>").join("harness/pi.toml"));

        let ids: Vec<String> = data
            .harness_specs()
            .expect("the embedded table loads")
            .into_iter()
            .map(|spec| spec.id)
            .collect();
        assert!(ids.contains(&"pi".to_owned()), "{ids:?}");
        assert!(!data.families().expect("families load").families.is_empty());
        assert!(!data.compat().expect("compat loads").skills.is_empty());
        data.visibility().expect("visibility rules load");

        let files = data.files(Kind::Harness);
        assert!(files.contains(&"harness/pi.toml".to_owned()), "{files:?}");
        assert!(
            files.iter().all(|name| !name.ends_with(".md")),
            "only the harness table's files: {files:?}"
        );
        assert_eq!(data.summary(Kind::Harness).total, files.len());
    }

    /// A person's override directory is layered over the embedded copy: the
    /// file they changed wins, and every other file still comes from the binary.
    #[test]
    fn an_override_file_wins_over_the_embedded_copy_of_the_same_name() {
        let (home, text) = data_home_with_pi_override();
        let data = DataDir::resolve(None, Some(home.path()));
        assert_eq!(data.origin(), &Origin::Overrides(home.path().join("shprd")));

        let pi = data.harness("pi").expect("the override loads");
        assert_eq!(pi.name(), "Pi (mine)");
        assert_eq!(
            pi.path(),
            home.path().join("shprd").join("harness").join("pi.toml")
        );
        assert_eq!(data.read("harness/pi.toml").expect("the override"), text);

        // The programs the person did not touch still come from the binary.
        let codex = data.harness("codex").expect("codex is still embedded");
        assert_eq!(codex.id(), "codex");
        assert_eq!(
            codex.path(),
            Path::new("<embedded>").join("harness/codex.toml")
        );

        let summary = data.summary(Kind::Harness);
        assert!(summary.overridden > 0, "{summary:?}");
        assert_eq!(summary.total, data.files(Kind::Harness).len());
        assert_eq!(
            summary.total - summary.overridden,
            embedded_names_count() - 1,
            "every harness but pi's still comes from the binary"
        );
        assert!(
            summary.describe(Kind::Harness).contains("overridden"),
            "{}",
            summary.describe(Kind::Harness)
        );
    }

    /// A directory named outright is the whole table, read as it stands: it
    /// wins over the data home's overrides and over the embedded copy.
    #[test]
    fn a_named_directory_wins_over_the_overrides_and_the_embedded_copy() {
        let (home, _) = data_home_with_pi_override();
        let named = TempDir::new().expect("a temp data directory");
        let harness = named.path().join("harness");
        std::fs::create_dir_all(&harness).expect("mkdir harness");
        let mut text = embedded_pi();
        text = text.replace("name = \"Pi\"", "name = \"Pi (named)\"");
        std::fs::write(harness.join("pi.toml"), &text).expect("write the harness");

        // The named directory wins whatever the data home holds.
        let data = DataDir::resolve(Some(named.path()), Some(home.path()));
        assert_eq!(data.origin(), &Origin::Named(named.path().to_path_buf()));
        assert_eq!(data.env_value(), Some(named.path()));
        assert_eq!(data.harness("pi").expect("pi loads").name(), "Pi (named)");

        // It is the whole table: a program the directory does not hold is not
        // silently borrowed from the embedded copy.
        assert!(!data.contains("harness/codex.toml"));
        assert!(data.harness("codex").is_err());
        assert_eq!(data.files(Kind::Harness), ["harness/pi.toml".to_owned()]);
    }

    /// A data home with no `shprd/harness/` is not an override directory: a
    /// person who never wrote one gets the embedded copy, not an empty table.
    #[test]
    fn a_data_home_without_a_harness_directory_is_not_an_override() {
        let home = empty_data_home();
        std::fs::create_dir_all(home.path().join("shprd").join("models")).expect("mkdir models");
        let data = DataDir::resolve(None, Some(home.path()));
        assert_eq!(data.origin(), &Origin::Embedded);
    }

    /// A missing file reads as not found rather than as a broken table, so a
    /// caller can say what it looked for.
    #[test]
    fn a_file_the_data_does_not_hold_reads_as_not_found() {
        let home = empty_data_home();
        let data = DataDir::resolve(None, Some(home.path()));
        let error = data.read("harness/nope.toml").expect_err("no such harness");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        assert!(!data.contains("harness/nope.toml"));
    }

    /// The kinds are the four directories under `data/`, and a page can name
    /// each of them.
    #[test]
    fn every_kind_names_its_directory() {
        for kind in Kind::ALL {
            assert_eq!(Kind::parse(kind.dir()), Some(kind));
        }
        assert_eq!(Kind::parse("nope"), None);
        let dirs: Vec<PathBuf> = Kind::ALL
            .iter()
            .map(|kind| Path::new(kind.dir()).to_path_buf())
            .collect();
        assert_eq!(
            dirs,
            ["harness", "models", "templates", "skills"].map(PathBuf::from)
        );
    }
}
