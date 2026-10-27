//! Which paths a branch changed, and whether they stayed inside a scope.

use std::path::Path;

use shprd_core::scope;

use crate::git::{Git, GitError};

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
