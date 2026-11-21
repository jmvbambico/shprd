//! Moving from one service per repository to the one service (SHPR-77).
//!
//! An earlier version ran a `shprd serve` per repository, each with its own
//! state directory under `<state>/shprd/repos/<name>-<hash>/` holding a
//! `service.json`, a token, a lock, a web password and sessions. The one
//! service keeps only each repository's change file and last pass there, so on
//! its first start it stops those services and removes their records, and moves
//! one web password up to the service's own directory.
//!
//! Only shprd's own are touched: a record's pid is stopped only while it is
//! still a `shprd serve` (a pid the kernel has since given to something else is
//! left alone), and only files this version wrote are removed.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use shprd_server::ServiceRecord;

/// The per-repository files that belonged to a service of its own.
const OWN_FILES: [&str; 5] = [
    "service.json",
    "token",
    "service.lock",
    "web.json",
    "sessions.json",
];

/// How long a stopping service is given before it is reported as still there.
const STOP_WAIT: Duration = Duration::from_secs(6);

/// Stops the per-repository services under `state_dir` and removes their
/// records; answers one plain line per thing done (nothing when there was
/// nothing to do, which is every start after the first).
pub(crate) fn migrate(state_dir: &Path) -> Vec<String> {
    let mut notes = Vec::new();
    let Ok(entries) = fs::read_dir(state_dir.join("repos")) else {
        return notes;
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    let mut passwords: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for dir in &dirs {
        let name = dir
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        let record = fs::read_to_string(dir.join("service.json"))
            .ok()
            .and_then(|text| serde_json::from_str::<ServiceRecord>(&text).ok());
        if let Some(record) = &record
            && record.pid != std::process::id()
            && alive(record.pid)
            && is_shprd_serve(record.pid)
        {
            signal("-TERM", record.pid);
            let deadline = Instant::now() + STOP_WAIT;
            while alive(record.pid) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(100));
            }
            if alive(record.pid) {
                notes.push(format!(
                    "the old service for {name} (pid {}) did not stop; stop it by hand",
                    record.pid
                ));
                continue;
            }
            notes.push(format!("stopped the old service for {name} (pid {})", record.pid));
        }
        let mut removed = false;
        for file in OWN_FILES {
            removed |= fs::remove_file(dir.join(file)).is_ok();
        }
        if removed && record.is_none() {
            notes.push(format!("removed the old service records of {name}"));
        }
        if let Ok(meta) = fs::metadata(dir.join("web-auth.json"))
            && let Ok(modified) = meta.modified()
        {
            passwords.push((modified, dir.join("web-auth.json")));
        }
    }
    notes.extend(carry_password(state_dir, passwords));
    notes
}

/// Moves the newest per-repository web password up to the service's own
/// directory, unless the service already has one; the others are removed, and
/// the line says which repository's was kept and that the rest are re-asked.
fn carry_password(
    state_dir: &Path,
    mut found: Vec<(std::time::SystemTime, PathBuf)>,
) -> Vec<String> {
    let mut notes = Vec::new();
    found.sort();
    let target = state_dir.join("web-auth.json");
    let mut newest = found.pop();
    if !target.exists()
        && let Some((_, from)) = newest.take()
    {
        let which = from
            .parent()
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        if fs::rename(&from, &target).is_ok() {
            notes.push(format!(
                "the web password of {which} is now the service's one password"
            ));
        }
    }
    let dropped = found.len() + usize::from(newest.is_some());
    for (_, path) in found.into_iter().chain(newest) {
        let _ = fs::remove_file(path);
    }
    if dropped > 0 {
        notes.push(String::from(
            "the other repositories' web passwords were dropped: set one with `shprd web password`",
        ));
    }
    notes
}

/// Whether `pid` is a live process.
fn alive(pid: u32) -> bool {
    std::process::Command::new("/bin/kill")
        .args(["-0", "--", &pid.to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Whether `pid`'s command line is a `shprd serve`.
fn is_shprd_serve(pid: u32) -> bool {
    let Ok(output) = std::process::Command::new("/bin/ps")
        .args(["-o", "command=", "-p", &pid.to_string()])
        .stdin(std::process::Stdio::null())
        .output()
    else {
        return false;
    };
    let line = String::from_utf8_lossy(&output.stdout);
    let words: Vec<&str> = line.split_whitespace().collect();
    words
        .first()
        .is_some_and(|program| program.rsplit('/').next() == Some("shprd"))
        && words.contains(&"serve")
}

/// Sends `signal` to `pid`.
fn signal(signal: &str, pid: u32) {
    let _ = std::process::Command::new("/bin/kill")
        .args([signal, "--", &pid.to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn old_dir(state: &Path, name: &str) -> PathBuf {
        let dir = state.join("repos").join(name);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn old_records_are_removed_and_one_password_moves_up() {
        let state = tempfile::tempdir().unwrap();
        let a = old_dir(state.path(), "shop-1");
        let b = old_dir(state.path(), "blog-2");
        for dir in [&a, &b] {
            // A record naming no live process: nothing to stop.
            fs::write(
                dir.join("service.json"),
                r#"{"port":1,"id":"x","pid":4294967,"version":"0"}"#,
            )
            .unwrap();
            fs::write(dir.join("token"), "t").unwrap();
            fs::write(dir.join("service.lock"), "").unwrap();
            fs::write(dir.join("changes"), "{}").unwrap();
        }
        fs::write(a.join("web-auth.json"), "a").unwrap();
        std::thread::sleep(Duration::from_millis(20));
        fs::write(b.join("web-auth.json"), "b").unwrap();

        let notes = migrate(state.path());
        for dir in [&a, &b] {
            assert!(!dir.join("service.json").exists());
            assert!(!dir.join("token").exists());
            assert!(!dir.join("service.lock").exists());
            assert!(dir.join("changes").exists(), "the change file stays");
        }
        assert_eq!(
            fs::read_to_string(state.path().join("web-auth.json")).unwrap(),
            "b",
            "the newest password is the service's"
        );
        assert!(!a.join("web-auth.json").exists());
        assert!(notes.iter().any(|note| note.contains("web password")), "{notes:?}");
        assert!(
            migrate(state.path()).is_empty(),
            "a second start has nothing to migrate"
        );
    }

    #[test]
    fn a_pid_that_is_not_a_shprd_serve_is_left_alone() {
        // This test process is alive and is not `shprd serve`: it must survive.
        assert!(alive(std::process::id()));
        assert!(!is_shprd_serve(std::process::id()));
    }
}
