//! Finding the record one agent wrote in one working directory.
//!
//! Each CLI names its record differently, and only the program's own id says
//! which rule applies. A record that cannot be found is `None`: a missing
//! transcript says nothing about whether a turn was cut off, so the caller
//! leaves the agent alone rather than guessing.
//!
//! The codex sessions live under a date tree, so its record is found by
//! reading each rollout's own `session_meta` line and keeping the newest one
//! whose `cwd` is the directory asked about. Nothing here trusts a file name to
//! name a directory.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::{Cut, ReadError, claude::cut_claude, codex::cut_codex, pi::cut_pi, sqlite::cut_sqlite};

/// The newest record `program` wrote for `cwd`, or `None` when there is none.
pub fn newest_record(program: &str, home: &Path, cwd: &Path) -> Option<PathBuf> {
    match program {
        "claude" => {
            let slug = cwd.to_string_lossy().replace(['/', '.'], "-");
            newest_with_extension(&home.join(".claude").join("projects").join(slug), "jsonl")
        }
        "pi" => {
            let slug = cwd
                .to_string_lossy()
                .trim_start_matches('/')
                .replace('/', "-");
            newest_with_extension(
                &home
                    .join(".pi")
                    .join("agent")
                    .join("sessions")
                    .join(format!("--{slug}--")),
                "jsonl",
            )
        }
        "codex" => {
            let root = home.join(".codex").join("sessions");
            let mut found: Option<(SystemTime, PathBuf)> = None;
            each_file(&root, &mut |path| {
                if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl")
                    || !path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.starts_with("rollout-"))
                {
                    return;
                }
                if !codex_cwd(path).is_some_and(|recorded| recorded == cwd) {
                    return;
                }
                let Ok(modified) = std::fs::metadata(path).and_then(|meta| meta.modified()) else {
                    return;
                };
                if found.as_ref().is_none_or(|(kept, _)| modified >= *kept) {
                    found = Some((modified, path.to_path_buf()));
                }
            });
            found.map(|(_, path)| path)
        }
        "opencode" => {
            let db = home
                .join(".local")
                .join("share")
                .join("opencode")
                .join("opencode.db");
            db.is_file().then_some(db)
        }
        "kilo" => {
            let db = home
                .join(".local")
                .join("share")
                .join("kilo")
                .join("kilo.db");
            db.is_file().then_some(db)
        }
        _ => None,
    }
}

/// The provider cut `program`'s newest record for `cwd` ends on, if any.
///
/// A record that cannot be read is not a cut: the caller must not act on an
/// unreadable transcript, just as it does not act on a missing one.
pub fn cut(program: &str, home: &Path, cwd: &Path) -> Option<Cut> {
    let record = newest_record(program, home, cwd)?;
    let found: Result<Option<Cut>, ReadError> = match program {
        "claude" => cut_claude(&record),
        "codex" => cut_codex(&record),
        "pi" => cut_pi(&record),
        "opencode" | "kilo" => cut_sqlite(&record, cwd),
        _ => return None,
    };
    found.ok().flatten()
}

/// The newest file in `dir` whose extension is `extension`.
fn newest_with_extension(dir: &Path, extension: &str) -> Option<PathBuf> {
    let mut newest: Option<(SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()? {
        let path = entry.ok()?.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some(extension) {
            continue;
        }
        let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
        if newest.as_ref().is_none_or(|(kept, _)| modified >= *kept) {
            newest = Some((modified, path));
        }
    }
    newest.map(|(_, path)| path)
}

/// Run `f` over every file under `dir`, depth first.
fn each_file(dir: &Path, f: &mut impl FnMut(&Path)) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            each_file(&path, f);
        } else {
            f(&path);
        }
    }
}

/// The working directory a codex rollout's first `session_meta` line names.
fn codex_cwd(path: &Path) -> Option<PathBuf> {
    let file = std::fs::File::open(path).ok()?;
    let mut line = String::new();
    BufReader::new(file).read_line(&mut line).ok()?;
    let value: serde_json::Value = serde_json::from_str(&line).ok()?;
    let cwd = value.get("payload")?.get("cwd")?.as_str()?;
    Some(PathBuf::from(cwd))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::fs;

    use super::*;

    /// A home with one claude record for `cwd`, holding one rate-limit cut.
    #[test]
    fn finds_and_reads_a_claude_record() {
        let home = tempfile::tempdir().unwrap();
        let cwd = Path::new("/tmp/worktree");
        let slug = cwd.to_string_lossy().replace(['/', '.'], "-");
        let dir = home.path().join(".claude").join("projects").join(slug);
        fs::create_dir_all(&dir).unwrap();
        let line = r#"{"timestamp":"2026-10-27T06:30:00Z","type":"assistant","isApiErrorMessage":true,"error":"rate_limit","message":{"role":"assistant","content":[{"type":"text","text":"Rate limit reached"}]}}"#;
        fs::write(dir.join("session.jsonl"), line).unwrap();
        let cut = cut("claude", home.path(), cwd).expect("a cut");
        assert_eq!(cut.kind, super::super::ErrorKind::RateLimit);
        assert!(newest_record("elsewhere", home.path(), cwd).is_none());
    }

    /// Codex rollouts for other directories are not this agent's record.
    #[test]
    fn a_codex_record_is_matched_by_its_own_cwd() {
        let home = tempfile::tempdir().unwrap();
        let cwd = Path::new("/tmp/worktree");
        let other = Path::new("/tmp/other");
        let dir = home
            .path()
            .join(".codex")
            .join("sessions")
            .join("2026")
            .join("10")
            .join("27");
        fs::create_dir_all(&dir).unwrap();
        let rollout = |recorded: &Path, text: &str| {
            format!(
                "{}\n{}\n",
                serde_json::json!({"type": "session_meta", "payload": {"cwd": recorded}}),
                serde_json::json!({"timestamp": "2026-10-27T06:30:00Z", "type": "event_msg", "payload": {"type": "task_complete", "turn_id": "t1", "error": {"message": text}}})
            )
        };
        fs::write(
            dir.join("rollout-other.jsonl"),
            rollout(other, "usage limit"),
        )
        .unwrap();
        let wanted = dir.join("rollout-wanted.jsonl");
        fs::write(&wanted, rollout(cwd, "rate limit reached")).unwrap();
        let cut = cut("codex", home.path(), cwd).expect("a cut");
        assert_eq!(cut.kind, super::super::ErrorKind::RateLimit);
        assert_eq!(newest_record("codex", home.path(), cwd), Some(wanted));
    }
}
