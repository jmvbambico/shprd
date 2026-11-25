//! Which paths a branch changed, how much, and whether they stayed inside a
//! scope.

use std::path::Path;

use shprd_core::scope;

use crate::git::{Git, GitError};

/// One file a branch changed, as a diff stat counts it.
///
/// `binary` is a file git would not count lines of: it reports `-` instead of a
/// number, and a card that showed `+0 −0` for it would be claiming the file is
/// empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStat {
    /// The path, repo-relative, as git printed it.
    pub path: String,
    /// Lines added.
    pub added: u64,
    /// Lines deleted.
    pub deleted: u64,
    /// Whether git called the file binary.
    pub binary: bool,
}

/// The paths that differ between `base` and `head`.
///
/// Renames are not detected, so a rename shows up as a deletion plus an
/// addition: both the old and the new path have to be inside a task's scope,
/// which is what the scope check below relies on.
pub fn changed_paths(repo: &Path, base: &str, head: &str) -> Result<Vec<String>, GitError> {
    let range = format!("{base}..{head}");
    let raw = Git::new(repo).run(&["diff", "--name-only", "-z", "--no-renames", &range])?;
    Ok(split_nul(&raw))
}

/// What `branch` changed on its own, measured against the current `base`.
///
/// The one baseline for the hand-in check, the real-diff check before review
/// and the landing check. The range starts at the merge base of `base` and
/// `branch` ([`crate::sync::attempt_base`]), not at the commit the task was cut
/// from, so a task that merged the latest base is not blamed for what the base
/// gained. A path whose content at `branch` is the same as at `base` (or at
/// `origin/<base>`) came from the base and is dropped too: that covers a merge
/// base that lags behind, such as a squash-merged sibling.
///
/// # Errors
///
/// Fails when git cannot find a merge base or list the diff.
pub fn task_changes(repo: &Path, base: &str, branch: &str) -> Result<Vec<String>, GitError> {
    let git = Git::new(repo);
    let merge_base = crate::sync::attempt_base(&git, base, branch, false)?;
    let paths = changed_paths(repo, &merge_base, branch)?;
    let tracking = format!("refs/remotes/{}", crate::sync::remote_ref(base));
    let has_tracking = git
        .run(&[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{tracking}^{{commit}}"),
        ])
        .is_ok();
    Ok(paths
        .into_iter()
        .filter(|path| {
            let mine = blob(&git, branch, path);
            let same = |rev: &str| blob(&git, rev, path) == mine;
            !(same(base) || (has_tracking && same(&tracking)))
        })
        .collect())
}

/// The blob id of `path` at `rev`, [`None`] when it is not there.
fn blob(git: &Git, rev: &str, path: &str) -> Option<String> {
    git.run(&["rev-parse", "--verify", "--quiet", &format!("{rev}:{path}")])
        .ok()
        .map(|out| out.trim().to_string())
}

/// The files that differ between `base` and `head`, with their line counts.
///
/// It is what the accept card's Changes section reads: the same range
/// [`changed_paths`] walks, asked for counts instead of names. `--no-renames`
/// for the same reason: a rename is a deletion and an addition, which is what
/// both the count and the scope check are about.
pub fn file_stats(repo: &Path, base: &str, head: &str) -> Result<Vec<FileStat>, GitError> {
    let range = format!("{base}..{head}");
    let raw = Git::new(repo).run(&["diff", "--numstat", "-z", "--no-renames", &range])?;
    Ok(parse_numstat(&raw))
}

/// Parses `git diff --numstat -z` output.
///
/// Each record is `added<TAB>deleted<TAB>path` ended by a NUL; a binary file
/// has `-` where the numbers go. A record that is not three fields is dropped
/// rather than guessed at: the count is a display detail, and a half-read one
/// would be a wrong number in front of a person.
fn parse_numstat(text: &str) -> Vec<FileStat> {
    text.split('\0')
        .filter(|record| !record.is_empty())
        .filter_map(|record| {
            let mut fields = record.splitn(3, '\t');
            let added = fields.next()?;
            let deleted = fields.next()?;
            let path = fields.next()?;
            let binary = added == "-" || deleted == "-";
            Some(FileStat {
                path: path.to_string(),
                added: added.parse().unwrap_or(0),
                deleted: deleted.parse().unwrap_or(0),
                binary,
            })
        })
        .collect()
}

/// The paths in `paths` that fall outside `scope`.
///
/// A path that cannot be normalized cannot be shown to be inside the scope,
/// so it counts as outside. Paths are returned as they were given.
pub fn outside_scope(paths: &[String], scope: &[String]) -> Vec<String> {
    paths
        .iter()
        .filter(|path| match scope::normalize(path) {
            Ok(normalized) => !scope::within(&normalized, scope),
            Err(_) => true,
        })
        .cloned()
        .collect()
}

/// Splits NUL-separated git output into fields, dropping the empty tail.
fn split_nul(text: &str) -> Vec<String> {
    text.split('\0')
        .filter(|field| !field.is_empty())
        .map(|field| field.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_string()).collect()
    }

    #[test]
    fn numstat_records_become_file_stats() {
        let raw = "12\t3\tcrates/shprd-git/src/git.rs\0-\t-\tassets/logo.png\0";
        assert_eq!(
            parse_numstat(raw),
            vec![
                FileStat {
                    path: "crates/shprd-git/src/git.rs".to_string(),
                    added: 12,
                    deleted: 3,
                    binary: false,
                },
                FileStat {
                    path: "assets/logo.png".to_string(),
                    added: 0,
                    deleted: 0,
                    binary: true,
                },
            ]
        );
    }

    #[test]
    fn a_record_that_is_not_three_fields_is_dropped() {
        assert!(parse_numstat("").is_empty());
        assert!(parse_numstat("12\t3\0").is_empty());
        // A path with a tab in it keeps the tab: only the first two are split off.
        let odd = parse_numstat("1\t2\ta\tb\0");
        assert_eq!(odd.len(), 1);
        assert_eq!(odd[0].path, "a\tb");
    }

    #[test]
    fn nul_separated_output_splits_into_paths() {
        assert_eq!(split_nul("a\0b/c\0"), strings(&["a", "b/c"]));
        assert!(split_nul("").is_empty());
    }

    #[test]
    fn a_path_outside_the_scope_is_flagged() {
        let scope = strings(&["crates/shprd-git"]);
        let paths = strings(&[
            "crates/shprd-git/src/git.rs",
            "crates/shprd-core/src/lib.rs",
        ]);
        assert_eq!(
            outside_scope(&paths, &scope),
            strings(&["crates/shprd-core/src/lib.rs"])
        );
    }

    #[test]
    fn an_unusable_path_counts_as_outside() {
        let scope = strings(&["crates/shprd-git"]);
        let paths = strings(&["/etc/passwd", "../escape", "crates/shprd-git/src/lib.rs"]);
        assert_eq!(
            outside_scope(&paths, &scope),
            strings(&["/etc/passwd", "../escape"])
        );
    }

    #[test]
    fn a_path_that_needs_normalizing_is_accepted_inside() {
        let scope = strings(&["crates/shprd-git"]);
        assert!(outside_scope(&strings(&["./crates/shprd-git/"]), &scope).is_empty());
    }
}
