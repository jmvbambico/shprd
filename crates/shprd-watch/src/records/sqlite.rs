//! Readers for the opencode and kilo conversation records.
//!
//! Both CLIs keep a SQLite database (`~/.local/share/opencode/opencode.db`
//! and `~/.local/share/kilo/kilo.db`). Messages live in `message(id,
//! session_id, time_created, time_updated, data)` with the payload in the
//! `data` column as JSON, and sessions in `session(id, directory, ...)`.
//!
//! The database is opened read-only through a `mode=ro` URI: a watchdog must
//! never be the reason a live CLI's store is locked or migrated.

use std::path::Path;

use rusqlite::{Connection, OpenFlags};

use super::{ReadError, RecordError, Scan, classify, truncate};

/// The error name opencode uses for a user-cancelled message; not a provider
/// fault, so it is never reported.
const ABORTED: &str = "MessageAbortedError";

/// Scan one opencode or kilo database for the errors of `session_dir`.
///
/// Only messages of sessions whose `directory` is `session_dir`, recorded at
/// or after `since_ms`, are considered; the newest error wins.
pub fn scan_sqlite(db: &Path, session_dir: &Path, since_ms: u64) -> Result<Scan, ReadError> {
    let connection = open_read_only(db)?;
    let directory = session_dir.to_string_lossy().to_string();
    let since = i64::try_from(since_ms).unwrap_or(i64::MAX);
    // One query over message and session, ordered by time: the last error seen
    // is then the newest, whatever order the sessions happen to sit in.
    let mut messages = connection
        .prepare(
            "SELECT m.data, m.time_created FROM message m \
             JOIN session s ON m.session_id = s.id \
             WHERE s.directory = ?1 AND m.time_created >= ?2 \
             ORDER BY m.time_created, m.id",
        )
        .map_err(|e| ReadError::storage(db, e))?;
    let rows = messages
        .query_map((&directory, since), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(|e| ReadError::storage(db, e))?;
    let mut scan = Scan::default();
    for row in rows {
        let (data, created) = row.map_err(|e| ReadError::storage(db, e))?;
        apply_message(&mut scan, &data, created);
    }
    Ok(scan)
}

/// Open `db` read-only, refusing to create or migrate anything.
fn open_read_only(db: &Path) -> Result<Connection, ReadError> {
    let uri = format!("file:{}?mode=ro&immutable=0", db.display());
    Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|source| ReadError::Io {
        path: db.to_path_buf(),
        source: std::io::Error::other(source),
    })
}

/// Fold one message row into `scan`: its activity time and any error it holds.
fn apply_message(scan: &mut Scan, data: &str, created: i64) {
    let Ok(column) = u64::try_from(created) else {
        return;
    };
    scan.last_activity_ms = Some(scan.last_activity_ms.unwrap_or(column).max(column));
    let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
        return;
    };
    let at = value
        .get("time")
        .and_then(|t| t.get("created"))
        .and_then(|v| v.as_i64())
        .and_then(|ms| u64::try_from(ms).ok())
        .unwrap_or(column);
    let Some(error) = value.get("error").filter(|e| !e.is_null()) else {
        return;
    };
    let name = error
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("error");
    if name == ABORTED {
        return;
    }
    let detail = error
        .get("data")
        .and_then(|d| d.get("message"))
        .and_then(|v| v.as_str())
        .unwrap_or(name);
    scan.last_error = Some(RecordError {
        kind: classify(&format!("{name} {detail}")),
        message: truncate(detail),
        at_ms: Some(at),
        resets_at_ms: None,
    });
}

/// Scan the opencode database for the errors of `session_dir`.
pub fn scan_opencode(db: &Path, session_dir: &Path, since_ms: u64) -> Result<Scan, ReadError> {
    scan_sqlite(db, session_dir, since_ms)
}

/// Scan the kilo database for the errors of `session_dir`.
pub fn scan_kilo(db: &Path, session_dir: &Path, since_ms: u64) -> Result<Scan, ReadError> {
    scan_sqlite(db, session_dir, since_ms)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::path::{Path, PathBuf};

    use super::*;
    use crate::records::ErrorKind;

    /// One message row: session id, session directory, created ms, data JSON.
    type Row<'a> = (&'a str, &'a str, i64, &'a str);

    /// Build a database with the opencode/kilo shape and `rows` in it.
    fn fixture(name: &str, rows: &[Row<'_>]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT, time_updated INTEGER);
                 CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER,
                    time_updated INTEGER, data TEXT);",
            )
            .unwrap();
        for (index, (session, directory, created, data)) in rows.iter().enumerate() {
            connection
                .execute(
                    "INSERT OR IGNORE INTO session (id, directory, time_updated) VALUES (?1, ?2, ?3)",
                    (session, directory, created),
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO message (id, session_id, time_created, time_updated, data) \
                     VALUES (?1, ?2, ?3, ?3, ?4)",
                    (format!("m{index}"), session, created, data),
                )
                .unwrap();
        }
        (dir, path)
    }

    const OLD: &str = r#"{"time":{"created":1788220800000},"error":{"name":"APIError","data":{"message":"rate limit"}}}"#;
    const AUTH: &str = r#"{"time":{"created":1793073600000},"error":{"name":"APIError","data":{"message":"401 Unauthorized: invalid api key"}}}"#;
    const QUOTA: &str = r#"{"time":{"created":1793077200000},"error":{"name":"UnknownError","data":{"message":"insufficient credit"}}}"#;
    const ABORTED_ROW: &str = r#"{"time":{"created":1793080800000},"error":{"name":"MessageAbortedError","data":{"message":"aborted by user"}}}"#;

    #[test]
    fn keeps_the_newest_error_of_the_matching_directory() {
        let worktree = Path::new("/tmp/worktree");
        let (_dir, path) = fixture(
            "opencode.db",
            &[
                ("s0", "/tmp/worktree", 1_788_220_800_000, OLD),
                ("s1", "/tmp/worktree", 1_793_073_600_000, AUTH),
                ("s1", "/tmp/worktree", 1_793_077_200_000, QUOTA),
                ("s1", "/tmp/worktree", 1_793_080_800_000, ABORTED_ROW),
                ("s2", "/tmp/elsewhere", 1_799_000_000_000, QUOTA),
                ("s1", "/tmp/worktree", 1_793_084_400_000, "not json"),
            ],
        );
        let scan = scan_sqlite(&path, worktree, 1_792_972_800_000).unwrap();
        let error = scan.last_error.unwrap();
        assert_eq!(error.kind, ErrorKind::Quota);
        assert_eq!(error.message, "insufficient credit");
        assert_eq!(error.at_ms, Some(1_793_077_200_000));
        assert_eq!(scan.last_activity_ms, Some(1_793_084_400_000));
    }

    #[test]
    fn ignores_everything_before_the_cutoff() {
        let worktree = Path::new("/tmp/worktree");
        let (_dir, path) = fixture(
            "kilo.db",
            &[("s1", "/tmp/worktree", 1_793_073_600_000, AUTH)],
        );
        let scan = scan_kilo(&path, worktree, 1_800_000_000_000).unwrap();
        assert!(scan.last_error.is_none());
        assert!(scan.last_activity_ms.is_none());

        let scan = scan_opencode(&path, worktree, 0).unwrap();
        assert_eq!(scan.last_error.unwrap().kind, ErrorKind::Auth);
    }

    #[test]
    fn the_newest_error_wins_across_sessions() {
        let worktree = Path::new("/tmp/worktree");
        // The session inserted first holds the newer error, so scanning
        // sessions one by one would let the later, older error win.
        let (_dir, path) = fixture(
            "opencode.db",
            &[
                ("newer", "/tmp/worktree", 1_793_077_200_000, QUOTA),
                ("older", "/tmp/worktree", 1_793_073_600_000, AUTH),
            ],
        );
        let scan = scan_sqlite(&path, worktree, 0).unwrap();
        let error = scan.last_error.unwrap();
        assert_eq!(error.kind, ErrorKind::Quota);
        assert_eq!(error.message, "insufficient credit");
        assert_eq!(error.at_ms, Some(1_793_077_200_000));
    }

    #[test]
    fn reports_a_missing_database_as_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.db");
        assert!(matches!(
            scan_sqlite(&missing, dir.path(), 0),
            Err(ReadError::Io { .. })
        ));
    }
}
