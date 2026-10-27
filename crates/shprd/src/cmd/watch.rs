//! The `watch` verb: poll every working agent and apply the dry/hand-off state
//! machine to what its pane and its transcript say.

use std::path::{Path, PathBuf};

use shprd_watch::records::RecordError;
use shprd_watch::records::claude::scan_claude;
use shprd_watch::records::pi::scan_pi;
use shprd_watch::records::sqlite::{scan_kilo, scan_opencode};

use crate::cmd::spawn::Program;

/// The newest `*.jsonl` file in `dir`, if the directory holds one.
///
/// A directory that cannot be read yields `None`: a missing record says
/// nothing about the agent's health.
fn newest_jsonl(dir: &Path) -> Option<PathBuf> {
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()? {
        let path = entry.ok()?.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
        let later = match &newest {
            Some((seen, _)) => modified >= *seen,
            None => true,
        };
        if later {
            newest = Some((modified, path));
        }
    }
    newest.map(|(_, path)| path)
}

/// The newest record `claude` wrote for `worktree`.
///
/// Its project directory is the worktree path with every `/` and `.` turned
/// into `-`, so `/.worktrees` becomes `--worktrees`.
fn claude_record(home: &Path, worktree: &Path) -> Option<PathBuf> {
    let slug = worktree.to_string_lossy().replace(['/', '.'], "-");
    newest_jsonl(&home.join(".claude").join("projects").join(slug))
}

/// The newest record `pi` wrote for `worktree`.
///
/// Its session directory is the worktree path without the leading `/`, with
/// `/` turned into `-`, wrapped in `--`.
fn pi_record(home: &Path, worktree: &Path) -> Option<PathBuf> {
    let slug = worktree
        .to_string_lossy()
        .trim_start_matches('/')
        .replace('/', "-");
    let dir = home.join(".pi").join("agent").join("sessions");
    newest_jsonl(&dir.join(format!("--{slug}--")))
}

/// The newest provider error `program` recorded for `worktree` at or after
/// `since_ms`.
///
/// A record that cannot be read is not an error here: a missing or
/// half-written transcript is not evidence that the agent is dry, and codex
/// keeps no record we can classify, so both answer `None`.
fn record_error(
    program: &Program,
    home: &Path,
    worktree: &Path,
    since_ms: u64,
) -> Option<RecordError> {
    let db = |name: &str| home.join(".local").join("share").join(name);
    let scan = match program.spec.id.as_str() {
        "claude" => scan_claude(&claude_record(home, worktree)?, since_ms),
        "pi" => scan_pi(&pi_record(home, worktree)?, since_ms),
        "opencode" => scan_opencode(&db("opencode").join("opencode.db"), worktree, since_ms),
        "kilo" => scan_kilo(&db("kilo").join("kilo.db"), worktree, since_ms),
        _ => return None,
    };
    scan.ok()?.last_error
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use tempfile::TempDir;

    use super::{claude_record, pi_record};

    /// A worktree path with both a dot and several separators in it.
    const WORKTREE: &str = "/repos/.worktrees/shprd/m5-watch";

    /// An empty home directory.
    fn home() -> TempDir {
        TempDir::new().unwrap_or_else(|err| panic!("{err}"))
    }

    /// Writes a one-line record `name` under `dir`, returning its path.
    fn touch(dir: &Path, name: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap_or_else(|err| panic!("{err}"));
        let path = dir.join(name);
        std::fs::write(&path, "{}\n").unwrap_or_else(|err| panic!("{err}"));
        path
    }

    #[test]
    fn claude_record_slugifies_every_separator_and_dot() {
        let home = home();
        let dir = home
            .path()
            .join(".claude/projects/-repos--worktrees-shprd-m5-watch");
        let path = touch(&dir, "session.jsonl");
        assert_eq!(claude_record(home.path(), Path::new(WORKTREE)), Some(path));
    }

    #[test]
    fn pi_record_slugs_without_the_leading_slash() {
        let home = home();
        let dir = home
            .path()
            .join(".pi/agent/sessions/--repos-.worktrees-shprd-m5-watch--");
        let path = touch(&dir, "session.jsonl");
        assert_eq!(pi_record(home.path(), Path::new(WORKTREE)), Some(path));
    }

    #[test]
    fn both_builders_pick_the_newest_record() {
        let home = home();
        let claude_dir = home
            .path()
            .join(".claude/projects/-repos--worktrees-shprd-m5-watch");
        let pi_dir = home
            .path()
            .join(".pi/agent/sessions/--repos-.worktrees-shprd-m5-watch--");
        touch(&claude_dir, "old.jsonl");
        touch(&pi_dir, "old.jsonl");
        std::thread::sleep(Duration::from_millis(20));
        let claude_new = touch(&claude_dir, "new.jsonl");
        let pi_new = touch(&pi_dir, "new.jsonl");

        assert_eq!(
            claude_record(home.path(), Path::new(WORKTREE)),
            Some(claude_new)
        );
        assert_eq!(pi_record(home.path(), Path::new(WORKTREE)), Some(pi_new));
    }

    #[test]
    fn a_missing_record_directory_answers_none() {
        let home = home();
        assert_eq!(claude_record(home.path(), Path::new(WORKTREE)), None);
        assert_eq!(pi_record(home.path(), Path::new(WORKTREE)), None);
    }
}
