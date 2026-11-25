//! Gate baselines: what each worker gate says on the base, before any task.
//!
//! A repository can be red before a run starts (62 eslint errors on `main`).
//! A hand-in is then judged against that baseline: a gate that fails no worse
//! than it did on the base is *pre-existing*, and only a regression refuses.
//! The logic here is pure; the one file it reads and writes is the store.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The words a pre-existing failure's output tail starts with, so the card and
/// the pull request body can say so.
pub const PRE_EXISTING_LABEL: &str = "pre-existing on base";

/// What a failing gate's output boils down to.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fingerprint {
    /// The error count the output states, when it does.
    pub errors: Option<u32>,
    /// The files the output names as failing.
    pub files: Vec<String>,
}

/// One gate command as it ran on the base.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaselineEntry {
    /// The gate command, as configured.
    pub command: String,
    /// True when it passed on the base.
    pub passed: bool,
    /// Its exit code (`None`: a timeout).
    pub exit_code: Option<i32>,
    /// Its failure fingerprint (empty when it passed).
    pub fingerprint: Fingerprint,
    /// The base commit it ran on.
    pub base_sha: String,
}

/// Every baseline the repository has recorded, by command.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Baselines {
    /// One entry per gate command.
    pub entries: Vec<BaselineEntry>,
}

/// Why a baseline could not be stored.
#[derive(Debug, thiserror::Error)]
pub enum BaselineError {
    /// The file could not be written.
    #[error("writing the gate baseline {path}: {source}")]
    Write {
        /// The file.
        path: PathBuf,
        /// The cause.
        source: std::io::Error,
    },
    /// The entries could not be encoded.
    #[error("encoding the gate baseline: {0}")]
    Encode(#[from] serde_json::Error),
}

/// Where the repository at `root` keeps its baselines.
pub fn store_path(root: &Path) -> PathBuf {
    root.join(".shprd").join("gate-baselines.json")
}

impl Baselines {
    /// Reads the store; a missing or unreadable one is empty (no baseline means
    /// no leniency).
    pub fn load(root: &Path) -> Self {
        std::fs::read_to_string(store_path(root))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Writes the store.
    pub fn save(&self, root: &Path) -> Result<(), BaselineError> {
        let path = store_path(root);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|source| BaselineError::Write {
                path: path.clone(),
                source,
            })?;
        }
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, text).map_err(|source| BaselineError::Write { path, source })
    }

    /// The entry for `command`.
    pub fn find(&self, command: &str) -> Option<&BaselineEntry> {
        self.entries
            .iter()
            .find(|entry| entry.command.trim() == command.trim())
    }

    /// Replaces the entry for the same command, or adds it.
    pub fn upsert(&mut self, entry: BaselineEntry) {
        match self
            .entries
            .iter_mut()
            .find(|one| one.command == entry.command)
        {
            Some(slot) => *slot = entry,
            None => self.entries.push(entry),
        }
    }
}

/// Fingerprints a gate's output: the last error count it states, and the files
/// it names at the start of a line (`path/to/file.ext:` or `path/to/file.ext`).
pub fn fingerprint(output: &str) -> Fingerprint {
    let mut errors = None;
    let mut files = BTreeSet::new();
    for line in output.lines() {
        if let Some(count) = error_count(line) {
            errors = Some(count);
        }
        if let Some(file) = file_of(line) {
            files.insert(file);
        }
    }
    Fingerprint {
        errors,
        files: files.into_iter().collect(),
    }
}

/// The number before the word `error`/`errors` in `line`, when there is one.
fn error_count(line: &str) -> Option<u32> {
    let words: Vec<&str> = line.split(|c: char| !c.is_alphanumeric()).collect();
    let mut found = None;
    for pair in words.windows(2) {
        if let (Ok(count), true) = (
            pair[0].parse::<u32>(),
            pair[1].eq_ignore_ascii_case("error") || pair[1].eq_ignore_ascii_case("errors"),
        ) {
            found = Some(count);
        }
    }
    found
}

/// The file a line starts with, when it looks like `dir/file.ext` or
/// `dir/file.ext:line`.
fn file_of(line: &str) -> Option<String> {
    let token = line.trim_start_matches("--> ").split_whitespace().next()?;
    let path = token.split(':').next()?;
    let ext = path.rsplit_once('.')?.1;
    let known = [
        "rs", "ts", "tsx", "js", "jsx", "go", "py", "vue", "svelte", "css",
    ];
    (known.contains(&ext) && !path.starts_with("http")).then(|| path.to_string())
}

/// Whether `now` is no worse than `base`, both failing.
///
/// Needs something to compare: an error count on both sides (not higher now) or
/// a file list on both (now a subset). Both known must agree. A fingerprint
/// that says nothing on either side is a regression: nothing proves it equal.
pub fn no_worse(base: &Fingerprint, now: &Fingerprint) -> bool {
    let mut comparable = false;
    if let (Some(then), Some(here)) = (base.errors, now.errors) {
        if here > then {
            return false;
        }
        comparable = true;
    }
    // Files the base never named cannot excuse files now: a new failing file
    // is a regression even when the error count alone would pass.
    if base.files.is_empty() && !now.files.is_empty() {
        return false;
    }
    if !base.files.is_empty() && !now.files.is_empty() {
        if !now.files.iter().all(|file| base.files.contains(file)) {
            return false;
        }
        comparable = true;
    }
    comparable
}

/// The verdict on one failing check against its baseline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Failed on the base too, and no worse: not the worker's doing.
    PreExisting,
    /// A regression, or no baseline to compare with.
    Regression,
}

/// Judges a failing check (`now` is its output tail's fingerprint).
pub fn judge(baselines: &Baselines, command: &str, output_tail: &str) -> Verdict {
    match baselines.find(command) {
        Some(base) if !base.passed && no_worse(&base.fingerprint, &fingerprint(output_tail)) => {
            Verdict::PreExisting
        }
        _ => Verdict::Regression,
    }
}

/// The output tail a pre-existing failure is recorded with.
pub fn labelled_tail(base: &BaselineEntry, output_tail: &str) -> String {
    let count = base
        .fingerprint
        .errors
        .map(|errors| format!(", {errors} errors on base"))
        .unwrap_or_default();
    format!(
        "{PRE_EXISTING_LABEL} ({}{count}); exit {}\n{output_tail}",
        base.base_sha.chars().take(9).collect::<String>(),
        base.exit_code
            .map_or("none".to_string(), |code| code.to_string())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(errors: u32, files: &[&str]) -> Baselines {
        Baselines {
            entries: vec![BaselineEntry {
                command: "npm run lint".into(),
                passed: false,
                exit_code: Some(1),
                fingerprint: Fingerprint {
                    errors: Some(errors),
                    files: files.iter().map(|f| f.to_string()).collect(),
                },
                base_sha: "abcdef1234567".into(),
            }],
        }
    }

    #[test]
    fn fingerprint_reads_eslint_summary_and_files() {
        let out = "src/a.ts\n  1:1  error  bad\nsrc/b.tsx:3:4 error\n✖ 62 problems (62 errors, 0 warnings)";
        let fp = fingerprint(out);
        assert_eq!(fp.errors, Some(62));
        assert_eq!(fp.files, vec!["src/a.ts", "src/b.tsx"]);
    }

    #[test]
    fn same_failure_is_pre_existing() {
        let base = entry(62, &["src/a.ts"]);
        let tail = "src/a.ts\n✖ 62 problems (62 errors, 0 warnings)";
        assert_eq!(judge(&base, "npm run lint", tail), Verdict::PreExisting);
    }

    #[test]
    fn more_errors_or_new_files_regress() {
        let base = entry(62, &["src/a.ts"]);
        assert_eq!(
            judge(&base, "npm run lint", "src/a.ts\n63 errors"),
            Verdict::Regression
        );
        assert_eq!(
            judge(&base, "npm run lint", "src/new.ts\n10 errors"),
            Verdict::Regression
        );
    }

    #[test]
    fn no_baseline_or_passing_baseline_regresses() {
        assert_eq!(
            judge(&Baselines::default(), "x", "1 errors"),
            Verdict::Regression
        );
        let mut base = entry(0, &[]);
        base.entries[0].passed = true;
        assert_eq!(
            judge(&base, "npm run lint", "1 errors"),
            Verdict::Regression
        );
    }

    #[test]
    fn a_new_failing_file_is_a_regression_when_the_base_named_none() {
        let base = entry(62, &[]);
        assert_eq!(
            judge(&base, "npm run lint", "src/new.ts\n10 errors"),
            Verdict::Regression
        );
    }

    #[test]
    fn an_unreadable_failure_is_a_regression() {
        let base = entry(62, &[]);
        assert_eq!(judge(&base, "npm run lint", "boom"), Verdict::Regression);
    }

    #[test]
    fn the_store_round_trips() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tmp: {e}"));
        let mut stored = Baselines::default();
        stored.upsert(entry(3, &[]).entries[0].clone());
        stored.upsert(entry(4, &[]).entries[0].clone());
        if let Err(e) = stored.save(dir.path()) {
            panic!("save: {e}");
        }
        let back = Baselines::load(dir.path());
        assert_eq!(back.entries.len(), 1);
        assert_eq!(back.entries[0].fingerprint.errors, Some(4));
    }

    #[test]
    fn the_label_names_the_base() {
        let base = entry(62, &[]).entries[0].clone();
        assert!(labelled_tail(&base, "x").starts_with(PRE_EXISTING_LABEL));
    }
}
