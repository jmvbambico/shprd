//! Where a review's packet lives, and the one file read back out of it.
//!
//! A reviewer runs outside the repository: it reads a packet folder and writes
//! `findings.json`, and nothing else. The engine is what turns that file into a
//! gate result, and it may be running as `shprd watch`, as `shprd serve` or as a
//! person's `shprd agent review-verdict`. They all have to look for the file in
//! the same place and trust it at the same age, so the path and the settle
//! window live here rather than in whichever caller was written first.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use thiserror::Error;

use crate::event::Finding;

/// How long a findings file must sit untouched before a reader trusts it.
///
/// A reviewer writes its file in one go, but a reader can fall between the open
/// and the last byte: reading a file younger than this would decide a review
/// from half of it.
pub const FINDINGS_SETTLE: Duration = Duration::from_secs(2);

/// Where a review packet lives: `<reviews>/<run>/<task>-a<attempt>/`.
pub fn packet_dir(reviews: &Path, run: &str, task: &str, attempt: u32) -> PathBuf {
    reviews.join(run).join(format!("{task}-a{attempt}"))
}

/// The file a reviewer writes its findings to, inside its packet folder.
///
/// It is the one file read back out of a packet: the engine watches it, so a
/// finished review is picked up without the reviewer running any command.
pub fn findings_path(reviews: &Path, run: &str, task: &str, attempt: u32) -> PathBuf {
    packet_dir(reviews, run, task, attempt).join("findings.json")
}

/// What a human is asked to do about a reviewer's unreadable findings file.
pub const INVALID_FINDINGS_ACTION: &str = "read the packet and decide the review by hand";

/// What every escalation about an unreadable findings file begins with.
///
/// It is the marker a later pass compares, so one bad file is one escalation
/// rather than one per pass.
pub fn invalid_findings_prefix(task: &str) -> String {
    format!("{task}: the reviewer's findings could not be read: ")
}

/// The mtime of `path`, when the file is old enough to be read.
///
/// A file that is missing, unreadable or still being written yields `None`: all
/// three mean "nothing to decide yet", and none of them is a failed review. A
/// restart asks the same question, which is why a caller that must not delete
/// findings mid-write (`review --again`) reads the answer from here too.
pub fn settled_mtime(path: &Path) -> Option<SystemTime> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let age = SystemTime::now().duration_since(modified).ok()?;
    (age >= FINDINGS_SETTLE).then_some(modified)
}

/// Reads the JSON array of findings a reviewer wrote.
///
/// Unreadable or invalid findings are the reviewer's mistake, never the engine's:
/// the error names the file so the caller can escalate it and a person can look
/// at the packet.
pub fn read_findings(path: &Path) -> Result<Vec<Finding>, FindingsError> {
    let bytes = std::fs::read(path).map_err(|source| FindingsError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    parse_findings(&bytes, path)
}

/// Parses the bytes of a reviewer's findings file.
///
/// It is separate from [`read_findings`] because a caller that also records the
/// file's hash needs the exact bytes it parsed, and reading the file twice could
/// read two different files.
pub fn parse_findings(bytes: &[u8], path: &Path) -> Result<Vec<Finding>, FindingsError> {
    serde_json::from_slice(bytes).map_err(|source| FindingsError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

/// Everything that can go wrong reading a reviewer's findings.
#[derive(Debug, Error)]
pub enum FindingsError {
    /// The file could not be read.
    #[error("reading findings from {path}: {source}")]
    Read {
        /// The file that failed.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// The file is not the JSON array of findings the packet asks for.
    #[error("parsing findings in {path}: {source}")]
    Parse {
        /// The file that failed.
        path: PathBuf,
        /// The underlying JSON error.
        source: serde_json::Error,
    },
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::io::Write;
    use std::time::Duration;

    use super::{FINDINGS_SETTLE, findings_path, packet_dir, read_findings, settled_mtime};

    /// A fresh file whose mtime is `age` before now.
    fn file_at(dir: &std::path::Path, name: &str, body: &str, age: Duration) -> std::path::PathBuf {
        let path = dir.join(name);
        let mut file = std::fs::File::create(&path).expect("the file is created");
        file.write_all(body.as_bytes())
            .expect("the file is written");
        file.set_modified(std::time::SystemTime::now() - age)
            .expect("the mtime is set");
        path
    }

    #[test]
    fn the_packet_path_names_the_run_the_task_and_the_attempt() {
        let reviews = std::path::Path::new("/home/.shprd/reviews");
        assert_eq!(
            packet_dir(reviews, "r1", "T1", 2),
            std::path::Path::new("/home/.shprd/reviews/r1/T1-a2")
        );
        assert_eq!(
            findings_path(reviews, "r1", "T1", 2),
            std::path::Path::new("/home/.shprd/reviews/r1/T1-a2/findings.json")
        );
    }

    #[test]
    fn a_findings_file_is_read_only_once_it_has_settled() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let fresh = file_at(dir.path(), "fresh.json", "[]", Duration::from_millis(0));
        assert!(
            settled_mtime(&fresh).is_none(),
            "a file just written may still be being written"
        );
        let settled = file_at(
            dir.path(),
            "settled.json",
            "[]",
            FINDINGS_SETTLE + Duration::from_millis(10),
        );
        assert!(settled_mtime(&settled).is_some());
        assert!(
            settled_mtime(&dir.path().join("missing.json")).is_none(),
            "a missing file says nothing about the review"
        );
    }

    #[test]
    fn findings_parse_into_the_list_the_rules_read() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = file_at(
            dir.path(),
            "findings.json",
            r#"[{"severity":"major","title":"a finding","detail":"fix it"}]"#,
            FINDINGS_SETTLE,
        );
        let findings = read_findings(&path).expect("the file is valid");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].title, "a finding");

        let broken = file_at(dir.path(), "broken.json", "not json", FINDINGS_SETTLE);
        assert!(
            read_findings(&broken).is_err(),
            "invalid findings are an error"
        );
    }
}
