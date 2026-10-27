//! Path scopes: normalized repo-relative paths, and when two sets of them clash.
//!
//! A task declares the paths it may change. Scopes are repo-relative and
//! `/`-separated, so `src` and `src/x.rs` overlap (one contains the other)
//! while `src` and `srcx` do not.

use thiserror::Error;

/// Why a path could not be used as a scope entry.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ScopeError {
    /// The path is empty, or became empty once trimmed.
    #[error("path is empty")]
    Empty,
    /// The path is absolute; scopes are repo-relative.
    #[error("path {path:?} is absolute")]
    Absolute {
        /// The offending path, as given.
        path: String,
    },
    /// The path escapes the repository root.
    #[error("path {path:?} contains a `..` component")]
    ParentEscape {
        /// The offending path, as given.
        path: String,
    },
}

/// Normalizes a repo-relative path for use in a scope.
///
/// Trims surrounding whitespace, strips one leading `./` and any trailing
/// `/`. Rejects empty paths, absolute paths and paths containing a `..`
/// component.
pub fn normalize(path: &str) -> Result<String, ScopeError> {
    let trimmed = path.trim();
    let mut text = trimmed;
    if let Some(rest) = text.strip_prefix("./") {
        text = rest;
    }
    while let Some(rest) = text.strip_suffix('/') {
        text = rest;
    }
    if trimmed.starts_with('/') {
        return Err(ScopeError::Absolute {
            path: trimmed.to_string(),
        });
    }
    if text.split('/').any(|component| component == "..") {
        return Err(ScopeError::ParentEscape {
            path: trimmed.to_string(),
        });
    }
    if text.is_empty() {
        return Err(ScopeError::Empty);
    }
    Ok(text.to_string())
}

/// True when `path` is one of `scope` or sits underneath one of its entries.
///
/// `path` and the entries of `scope` are expected to be normalized.
pub fn within(path: &str, scope: &[String]) -> bool {
    scope
        .iter()
        .any(|entry| path == entry || is_child(path, entry))
}

/// True when any entry of `a` and any entry of `b` are equal or one contains
/// the other.
///
/// `src` overlaps `src/x.rs`, but `src` and `srcx` do not overlap. Entries are
/// expected to be normalized.
pub fn overlaps(a: &[String], b: &[String]) -> bool {
    a.iter().any(|left| {
        b.iter()
            .any(|right| left == right || is_child(left, right) || is_child(right, left))
    })
}

/// True when `path` lies strictly below `parent`.
fn is_child(path: &str, parent: &str) -> bool {
    match path.strip_prefix(parent) {
        Some(rest) => rest.starts_with('/'),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(path: &str) -> String {
        match normalize(path) {
            Ok(text) => text,
            Err(err) => panic!("normalize({path:?}) failed: {err}"),
        }
    }

    fn paths(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_string()).collect()
    }

    #[test]
    fn normalize_strips_noise() {
        assert_eq!(ok("  src/x.rs  "), "src/x.rs");
        assert_eq!(ok("./src/x.rs"), "src/x.rs");
        assert_eq!(ok("src/"), "src");
        assert_eq!(ok("./src/"), "src");
        assert_eq!(ok("src/x.rs"), "src/x.rs");
    }

    #[test]
    fn normalize_rejects_bad_paths() {
        assert_eq!(normalize("   "), Err(ScopeError::Empty));
        assert_eq!(normalize("./"), Err(ScopeError::Empty));
        assert!(matches!(
            normalize("/etc/passwd"),
            Err(ScopeError::Absolute { .. })
        ));
        assert!(matches!(
            normalize("../x"),
            Err(ScopeError::ParentEscape { .. })
        ));
        assert!(matches!(
            normalize("src/../../x"),
            Err(ScopeError::ParentEscape { .. })
        ));
    }

    #[test]
    fn parent_escape_needs_a_real_component() {
        assert_eq!(ok("src/..x/y"), "src/..x/y");
        assert_eq!(ok("src/x.."), "src/x..");
    }

    #[test]
    fn overlap_requires_component_boundaries() {
        assert!(overlaps(&paths(&["src"]), &paths(&["src"])));
        assert!(overlaps(&paths(&["src"]), &paths(&["src/x.rs"])));
        assert!(overlaps(&paths(&["src/x.rs"]), &paths(&["src"])));
        assert!(!overlaps(&paths(&["src"]), &paths(&["srcx"])));
        assert!(!overlaps(&paths(&["src"]), &paths(&["srcx/y.rs"])));
        assert!(!overlaps(&paths(&["src/a"]), &paths(&["src/b"])));
        assert!(!overlaps(&paths(&[]), &paths(&["src"])));
    }

    #[test]
    fn within_is_the_one_sided_half() {
        let scope = paths(&["src", "docs/plan.md"]);
        assert!(within("src", &scope));
        assert!(within("src/x/y.rs", &scope));
        assert!(within("docs/plan.md", &scope));
        assert!(!within("docs/plan.md.bak", &scope));
        assert!(!within("srcx", &scope));
        assert!(!within("src", &[]));
    }
}
