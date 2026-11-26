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
    /// The path uses backslashes; scopes are `/`-separated.
    #[error("path {path:?} contains a backslash")]
    Backslash {
        /// The offending path, as given.
        path: String,
    },
    /// The path starts with a drive letter, so it is not repo-relative.
    #[error("path {path:?} starts with a drive letter")]
    Drive {
        /// The offending path, as given.
        path: String,
    },
}

/// Normalizes a repo-relative path for use in a scope.
///
/// Trims surrounding whitespace, strips one leading `./` and any trailing
/// `/`. Rejects empty paths, absolute paths, paths containing a `..`
/// component, paths containing a backslash and paths that start with a drive
/// letter such as `C:`; a scope is always repo-relative and `/`-separated.
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
    if text.contains('\\') {
        return Err(ScopeError::Backslash {
            path: trimmed.to_string(),
        });
    }
    if starts_with_drive(text) {
        return Err(ScopeError::Drive {
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

/// Why `path` may never be committed by any task, or `None` when it may.
///
/// This is the one hard list left from task scopes: shprd's own state
/// (`.shprd/`, except the tracked `.shprd/config.toml`, and `.dagr/`) and real
/// environment files (`.env`, `.env.<name>`, at any depth; `.env.example` and
/// other templates are fine). Everything else a task changes is judged by its
/// review and, for protected paths, by the person at the gate.
///
/// `path` is expected to be normalized.
pub fn forbidden(path: &str) -> Option<&'static str> {
    if path == ".shprd" || (is_child(path, ".shprd") && path != ".shprd/config.toml") {
        return Some("it is shprd's own state");
    }
    if path == ".dagr" || is_child(path, ".dagr") {
        return Some("it is shprd's own state");
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    let env = name == ".env"
        || name.strip_prefix(".env.").is_some_and(|suffix| {
            !matches!(
                suffix,
                "example" | "sample" | "template" | "dist" | "defaults" | "schema"
            )
        });
    if env {
        return Some("it is an environment file, which may hold secrets");
    }
    None
}

/// True when the first component of `text` looks like a Windows drive (`C:`
/// or `c:src`), which no repo-relative scope may.
fn starts_with_drive(text: &str) -> bool {
    let bytes = text.as_bytes();
    match (bytes.first(), bytes.get(1)) {
        (Some(first), Some(b':')) => first.is_ascii_alphabetic(),
        _ => false,
    }
}

/// True when `path` is one of `scope` or sits underneath one of its entries,
/// or is implied by one (see [`implied`]).
///
/// `path` and the entries of `scope` are expected to be normalized.
pub fn within(path: &str, scope: &[String]) -> bool {
    scope
        .iter()
        .any(|entry| path == entry || is_child(path, entry) || implied(path, entry))
}

/// Prefix of the desktop mods' folder.
const MODS: &str = "host/herdr-desktop/mods";

/// True when a scope `entry` implies `path` without declaring it:
///
/// - an entry under `crates/<crate>/` implies that crate's `tests/` folder;
/// - an entry `host/herdr-desktop/mods/lib/<file>` implies each synced copy
///   `host/herdr-desktop/mods/<mod>/lib/<file>`;
/// - an entry under `host/herdr-desktop/mods/` implies `host/herdr-desktop/test/`;
/// - a source file implies its sibling tests in the same folder, in any language
///   (see [`sibling_test`]).
///
/// Only [`within`] uses it: [`overlaps`] compares declared paths, so two tasks
/// whose implied paths meet still run in parallel.
fn implied(path: &str, entry: &str) -> bool {
    if sibling_test(path, entry) {
        return true;
    }
    if let Some(rest) = entry.strip_prefix("crates/")
        && let Some((krate, below)) = rest.split_once('/')
        && !krate.is_empty()
        && !below.is_empty()
    {
        let tests = format!("crates/{krate}/tests");
        if path == tests || is_child(path, &tests) {
            return true;
        }
    }
    if entry == MODS || is_child(entry, MODS) {
        let test = "host/herdr-desktop/test";
        if path == test || is_child(path, test) {
            return true;
        }
    }
    let lib = format!("{MODS}/lib");
    let shared = if entry == lib {
        Some("")
    } else {
        entry
            .strip_prefix(lib.as_str())
            .and_then(|rest| rest.strip_prefix('/'))
    };
    if let Some(shared) = shared
        && let Some(under) = path.strip_prefix(&format!("{MODS}/"))
        && let Some((module, copy)) = under.split_once('/')
        && module != "lib"
        && let Some(tail) = copy.strip_prefix("lib")
    {
        return match tail.strip_prefix('/') {
            None => tail.is_empty() && shared.is_empty(),
            Some(t) => shared.is_empty() || t == shared || is_child(t, shared),
        };
    }
    false
}

/// True when `entry` is a source file and `path` is a test file beside it:
///
/// - Go: `foo.go` implies `foo_test.go` and any `*_test.go` in the same folder;
/// - TypeScript/JavaScript: `x.ts` implies `x.test.ts` and `x.spec.ts` (also
///   for `tsx`, `js`, `jsx`, `mts`, `mjs`);
/// - Python: `x.py` implies `test_x.py` and `x_test.py`.
fn sibling_test(path: &str, entry: &str) -> bool {
    let (edir, efile) = entry.rsplit_once('/').unwrap_or(("", entry));
    let (pdir, pfile) = path.rsplit_once('/').unwrap_or(("", path));
    if edir != pdir {
        return false;
    }
    let Some((stem, ext)) = efile.rsplit_once('.') else {
        return false;
    };
    if stem.is_empty() || efile == pfile {
        return false;
    }
    match ext {
        "go" => !stem.ends_with("_test") && pfile.ends_with("_test.go"),
        "ts" | "tsx" | "js" | "jsx" | "mts" | "mjs" => {
            !stem.ends_with(".test")
                && !stem.ends_with(".spec")
                && [".test", ".spec"]
                    .iter()
                    .any(|mid| pfile == format!("{stem}{mid}.{ext}"))
        }
        "py" => pfile == format!("test_{stem}.py") || pfile == format!("{stem}_test.py"),
        _ => false,
    }
}

/// The nearest real path for a scope `entry` that matches nothing under `root`.
///
/// Returns `None` when the entry exists (as a path, or as a folder or file),
/// otherwise the first sibling of its last component that shares its stem
/// (`client/src/components/icons` -> `client/src/components/icons.tsx`), or
/// else the deepest existing ancestor folder.
pub fn nearest_existing(root: &std::path::Path, entry: &str) -> Option<String> {
    if root.join(entry).exists() {
        return None;
    }
    let (dir, name) = entry.rsplit_once('/').unwrap_or(("", entry));
    if let Ok(read) = std::fs::read_dir(root.join(dir)) {
        let mut hits: Vec<String> = read
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|f| {
                f.strip_prefix(name)
                    .is_some_and(|rest| rest.starts_with('.') || rest.starts_with('/'))
            })
            .collect();
        hits.sort();
        if let Some(hit) = hits.first() {
            return Some(if dir.is_empty() {
                hit.clone()
            } else {
                format!("{dir}/{hit}")
            });
        }
    }
    let mut parent = dir;
    loop {
        if parent.is_empty() {
            return Some(String::new());
        }
        if root.join(parent).is_dir() {
            return Some(parent.to_string());
        }
        parent = parent.rsplit_once('/').map_or("", |(up, _)| up);
    }
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
    fn normalize_rejects_foreign_separators_and_drives() {
        assert!(matches!(normalize("C:/src"), Err(ScopeError::Drive { .. })));
        assert!(matches!(normalize("c:src"), Err(ScopeError::Drive { .. })));
        assert!(matches!(
            normalize("\\src"),
            Err(ScopeError::Backslash { .. })
        ));
        assert!(matches!(
            normalize("src\\x"),
            Err(ScopeError::Backslash { .. })
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

    #[test]
    fn crate_scope_implies_that_crates_tests_only() {
        let scope = paths(&["crates/shprd-core/src/prompts.rs"]);
        assert!(within("crates/shprd-core/tests/x.rs", &scope));
        assert!(!within("crates/shprd/tests/x.rs", &scope));
        assert!(!within("crates/shprd-core/src/other.rs", &scope));
        assert!(!within("crates/shprd-core/testsx/x.rs", &scope));
    }

    #[test]
    fn shared_lib_scope_implies_the_synced_copies() {
        let scope = paths(&["host/herdr-desktop/mods/lib/chat.js"]);
        let copy = "host/herdr-desktop/mods/shprd-chat/lib/chat.js";
        assert!(within(copy, &scope));
        assert!(!within(
            "host/herdr-desktop/mods/shprd-chat/lib/other.js",
            &scope
        ));
        assert!(!within(
            "host/herdr-desktop/mods/shprd-chat/chat.js",
            &scope
        ));
        assert!(within("host/herdr-desktop/test/e2e/x.py", &scope));
    }

    #[test]
    fn mods_scope_implies_the_desktop_tests() {
        let scope = paths(&["host/herdr-desktop/mods/shprd-chat/main.js"]);
        assert!(within("host/herdr-desktop/test/a.py", &scope));
        assert!(!within("host/herdr-desktop/bin/check", &scope));
    }

    #[test]
    fn implied_paths_do_not_make_scopes_overlap() {
        let a = paths(&["crates/shprd-core/src/a.rs"]);
        let b = paths(&["crates/shprd-core/tests/x.rs"]);
        assert!(!overlaps(&a, &b));
    }

    #[test]
    fn sibling_tests_are_implied_in_every_language() {
        let scope = paths(&["pkg/foo.go", "web/x.ts", "tools/x.py"]);
        for yes in [
            "pkg/foo_test.go",
            "pkg/other_test.go",
            "web/x.test.ts",
            "web/x.spec.ts",
            "tools/test_x.py",
        ] {
            assert!(within(yes, &scope), "{yes}");
        }
        for no in [
            "pkg/other.go",
            "pkg/sub/foo_test.go",
            "web/y.test.ts",
            "web/x.test.tsx",
            "tools/test_y.py",
        ] {
            assert!(!within(no, &scope), "{no}");
        }
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn nearest_existing_suggests_the_real_path() {
        let dir = std::env::temp_dir().join(format!("shprd-scope-near-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("client/src/components")).unwrap();
        std::fs::write(dir.join("client/src/components/icons.tsx"), "").unwrap();
        assert_eq!(nearest_existing(&dir, "client/src/components"), None);
        assert_eq!(
            nearest_existing(&dir, "client/src/components/icons").as_deref(),
            Some("client/src/components/icons.tsx")
        );
        assert_eq!(
            nearest_existing(&dir, "client/src/components/zzz/a.rs").as_deref(),
            Some("client/src/components")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_shprd_state_and_real_env_files_are_forbidden() {
        for no in [
            ".shprd/events.jsonl",
            ".shprd",
            ".dagr/run.json",
            ".env",
            ".env.local",
            "web/.env.production",
        ] {
            assert!(forbidden(no).is_some(), "{no}");
        }
        for ok in [
            ".shprd/config.toml",
            ".env.example",
            "web/.env.sample",
            "src/env.rs",
            "Cargo.lock",
            "docs/plan.md",
            ".github/workflows/ci.yml",
            "secrets.md",
        ] {
            assert!(forbidden(ok).is_none(), "{ok}");
        }
    }
}
