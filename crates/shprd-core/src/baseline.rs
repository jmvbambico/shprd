//! Gate baselines: what each worker gate says on the base, before any task.
//!
//! A repository can be red before a run starts (62 eslint errors on `main`).
//! A hand-in is then judged against that baseline: a gate that fails no worse
//! than it did on the base is *pre-existing*, and only a regression refuses.
//! The logic here is pure: the baselines themselves live in the event log
//! (`GateBaselined`) and fold into the run state.

use std::collections::BTreeSet;

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
    /// True when the base moved (a sync) and no baseline was recorded for the
    /// new base yet: the entries describe an older base, so none may excuse a
    /// failure (fail closed).
    #[serde(default)]
    pub stale: bool,
}

impl Baselines {
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
            .find(|one| one.command.trim() == entry.command.trim())
        {
            Some(slot) => *slot = entry,
            None => self.entries.push(entry),
        }
    }
}

/// Rewrites `root` (the checkout a gate ran in) out of `output`, so the paths a
/// linter prints absolutely read the same in any checkout folder.
pub fn relativize(output: &str, root: &std::path::Path) -> String {
    let prefix = format!("{}/", root.display().to_string().trim_end_matches('/'));
    output.replace(&prefix, "")
}

/// Fingerprints a gate's output: the last error count it states, and the files
/// it names at the start of a line (`path/to/file.ext:` or `path/to/file.ext`).
pub fn fingerprint(output: &str) -> Fingerprint {
    let mut errors = None;
    let mut files = BTreeSet::new();
    for line in output.lines() {
        // The largest count wins: a later "3 errors fixed" must not hide the
        // real summary.
        if let Some(count) = error_count(line) {
            errors = Some(errors.map_or(count, |seen: u32| seen.max(count)));
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
    if baselines.stale {
        return Verdict::Regression;
    }
    match baselines.find(command) {
        Some(base) if !base.passed && no_worse(&base.fingerprint, &fingerprint(output_tail)) => {
            Verdict::PreExisting
        }
        _ => Verdict::Regression,
    }
}

/// The line a failing check carries when the baselines are stale, so the card
/// says why the failure is judged a regression.
pub const STALE_NOTE: &str = "gate baseline is stale: the base moved and its re-baseline did not finish, so this failure counts as a regression";

/// The label a pre-existing failure is recorded with: the base's commit and,
/// when it had one, its error count. The check keeps its true exit code.
pub fn label(base: &BaselineEntry) -> String {
    let count = base
        .fingerprint
        .errors
        .map(|errors| format!(", {errors} errors on base"))
        .unwrap_or_default();
    format!(
        "{PRE_EXISTING_LABEL} ({}{count}; exit {})",
        base.base_sha.chars().take(9).collect::<String>(),
        base.exit_code
            .map_or("none".to_string(), |code| code.to_string())
    )
}

/// The label for a failing check that failed no worse on the base, if it did.
pub fn excuse(baselines: &Baselines, command: &str, output_tail: &str) -> Option<String> {
    match judge(baselines, command, output_tail) {
        Verdict::PreExisting => baselines.find(command).map(label),
        Verdict::Regression => None,
    }
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
            stale: false,
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
    fn a_later_lower_count_does_not_hide_the_real_one() {
        let fp = fingerprint("70 errors\n3 errors fixed");
        assert_eq!(fp.errors, Some(70));
    }

    #[test]
    fn a_stale_baseline_excuses_nothing() {
        let mut base = entry(62, &["src/a.ts"]);
        let tail = "src/a.ts\n62 errors";
        assert_eq!(judge(&base, "npm run lint", tail), Verdict::PreExisting);
        base.stale = true;
        assert_eq!(judge(&base, "npm run lint", tail), Verdict::Regression);
        assert_eq!(excuse(&base, "npm run lint", tail), None);
    }

    #[test]
    fn paths_match_across_checkout_folders() {
        let a = relativize(
            "/tmp/baseline-checkout-1/src/a.ts:3:4 error\n62 errors",
            std::path::Path::new("/tmp/baseline-checkout-1"),
        );
        let b = relativize(
            "/work/tree/src/a.ts:3:4 error\n62 errors",
            std::path::Path::new("/work/tree/"),
        );
        assert_eq!(fingerprint(&a), fingerprint(&b));
        assert_eq!(fingerprint(&a).files, vec!["src/a.ts"]);
    }

    #[test]
    fn the_label_names_the_base() {
        let base = entry(62, &[]).entries[0].clone();
        assert!(label(&base).starts_with(PRE_EXISTING_LABEL));
    }
}
