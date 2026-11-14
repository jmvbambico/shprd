//! Readers for the opencode and kilo conversation records.
//!
//! Both CLIs keep a SQLite database (`~/.local/share/opencode/opencode.db`
//! and `~/.local/share/kilo/kilo.db`). Messages live in `message(id,
//! session_id, time_created, time_updated, data)` with the payload in the
//! `data` column as JSON, and sessions in `session(id, directory, ...)`.
//!
//! The database is opened read-only through a `mode=ro` URI: a watchdog must
//! never be the reason a live CLI's store is locked or migrated.
//!
//! The cut reader is ported from Shepherdr's `provider_cut`
//! (`mods/tasks/bin/herdr_task/provider.py`, commit `a3f327a`, MIT); see
//! `THIRD_PARTY_NOTICES.md`.

use std::path::Path;

use rusqlite::{Connection, OpenFlags};

use super::{Cut, NUDGE_MARK, ReadError, RecordError, Scan, classify, truncate};

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

/// The provider cut a session ends on, or `None` when it carried on.
///
/// opencode and kilo keep the error on the message row itself; a later
/// assistant message, or a person's message that is not shprd's carry-on,
/// means the turn is over. A person's words live in the `part` table, which a
/// database that predates it may not have: the words are then simply absent,
/// which starts the tries again, as any other message from a person does.
pub fn cut_sqlite(db: &Path, session_dir: &Path) -> Result<Option<Cut>, ReadError> {
    let connection = open_read_only(db)?;
    let directory = session_dir.to_string_lossy().to_string();
    let mut messages = connection
        .prepare(
            "SELECT m.id, m.data, m.time_created FROM message m \
             JOIN session s ON m.session_id = s.id \
             WHERE s.directory = ?1 ORDER BY m.time_created, m.id",
        )
        .map_err(|e| ReadError::storage(db, e))?;
    let rows = messages
        .query_map((&directory,), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(|e| ReadError::storage(db, e))?;
    let mut cut: Option<Cut> = None;
    let mut tries = 0u32;
    for row in rows {
        let (id, data, created) = row.map_err(|e| ReadError::storage(db, e))?;
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&data) else {
            continue;
        };
        let at = value
            .get("time")
            .and_then(|t| t.get("created"))
            .and_then(|v| v.as_i64())
            .and_then(|ms| u64::try_from(ms).ok())
            .unwrap_or_else(|| u64::try_from(created).unwrap_or_default());
        match value.get("role").and_then(|v| v.as_str()) {
            Some("assistant") => {
                let error = value.get("error").filter(|e| !e.is_null());
                let Some(error) = error else {
                    cut = None;
                    continue;
                };
                let name = error
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("error");
                if name == ABORTED {
                    cut = None;
                    continue;
                }
                tries = tries.saturating_add(1);
                let detail = error
                    .get("data")
                    .and_then(|d| d.get("message"))
                    .and_then(|v| v.as_str())
                    .unwrap_or(name);
                cut = Some(Cut {
                    id: id.clone(),
                    kind: classify(&format!("{name} {detail}")),
                    message: truncate(detail),
                    at_ms: at,
                    resets_at_ms: None,
                    tries,
                });
            }
            Some("user") => {
                let words = part_text(&connection, &id);
                cut = None;
                if !words.contains(NUDGE_MARK) {
                    tries = 0;
                }
            }
            _ => {}
        }
    }
    if let Some(cut) = &mut cut {
        cut.tries = cut.tries.max(1);
    }
    Ok(cut)
}

/// The words of one message's parts, or empty when the table or row is absent.
fn part_text(connection: &Connection, message_id: &str) -> String {
    let Ok(mut parts) =
        connection.prepare("SELECT data FROM part WHERE message_id = ?1 ORDER BY id")
    else {
        return String::new();
    };
    let Ok(rows) = parts.query_map((message_id,), |row| row.get::<_, String>(0)) else {
        return String::new();
    };
    rows.filter_map(Result::ok)
        .filter_map(|data| serde_json::from_str::<serde_json::Value>(&data).ok())
        .filter_map(|value| {
            value
                .get("text")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .collect::<Vec<_>>()
        .join("\n")
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
    use crate::records::{ErrorKind, NUDGE_MARK, Verdict};

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
                    time_updated INTEGER, data TEXT);
                 CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT, data TEXT);",
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

    /// Add the words of user message `m<index>` to a fixture database.
    fn add_words(path: &Path, index: usize, text: &str) {
        let connection = Connection::open(path).unwrap();
        let data = serde_json::json!({"type": "text", "text": text}).to_string();
        connection
            .execute(
                "INSERT INTO part (id, message_id, data) VALUES (?1, ?2, ?3)",
                (format!("p{index}"), format!("m{index}"), data),
            )
            .unwrap();
    }

    /// An assistant message row with an error, or a plain answer when `error` is None.
    fn assistant(created: i64, error: Option<(&str, &str)>) -> String {
        match error {
            Some((name, message)) => serde_json::json!({
                "role": "assistant",
                "time": {"created": created},
                "error": {"name": name, "data": {"message": message}},
            })
            .to_string(),
            None => serde_json::json!({
                "role": "assistant",
                "time": {"created": created},
            })
            .to_string(),
        }
    }

    fn user(created: i64) -> String {
        serde_json::json!({"role": "user", "time": {"created": created}}).to_string()
    }

    #[test]
    fn a_cut_is_classified_per_error_class() {
        let worktree = Path::new("/tmp/worktree");
        let cases = [
            (
                "APIError",
                "rate limit reached",
                ErrorKind::RateLimit,
                Verdict::Passing,
                "Rate limited",
            ),
            (
                "UnknownError",
                "insufficient credit",
                ErrorKind::Quota,
                Verdict::Lasting,
                "Out of allowance",
            ),
            (
                "APIError",
                "401 Unauthorized: invalid api key",
                ErrorKind::Auth,
                Verdict::Lasting,
                "Signed out",
            ),
            (
                "APIError",
                "server overloaded",
                ErrorKind::Overloaded,
                Verdict::Passing,
                "Provider overloaded",
            ),
            (
                "APIError",
                "invalid request",
                ErrorKind::Other,
                Verdict::Lasting,
                "Provider error",
            ),
        ];
        for (name, message, kind, verdict, reason) in cases {
            let data = assistant(1_793_073_600_000, Some((name, message)));
            let (_dir, path) = fixture(
                "opencode.db",
                &[("s1", "/tmp/worktree", 1_793_073_600_000, &data)],
            );
            let cut = cut_sqlite(&path, worktree).unwrap().expect("a cut");
            assert_eq!(cut.kind, kind, "{message}");
            assert_eq!(cut.verdict(), verdict, "{message}");
            assert_eq!(cut.reason(), reason, "{message}");
        }
    }

    #[test]
    fn a_later_answer_ends_the_cut() {
        let worktree = Path::new("/tmp/worktree");
        let error = assistant(1_793_073_600_000, Some(("APIError", "rate limit reached")));
        let answer = assistant(1_793_074_000_000, None);
        let (_dir, path) = fixture(
            "opencode.db",
            &[
                ("s1", "/tmp/worktree", 1_793_073_600_000, &error),
                ("s1", "/tmp/worktree", 1_793_074_000_000, &answer),
            ],
        );
        assert!(cut_sqlite(&path, worktree).unwrap().is_none());
    }

    #[test]
    fn tries_count_cut_turns_since_a_person_wrote() {
        let worktree = Path::new("/tmp/worktree");
        let error = assistant(1_793_073_600_000, Some(("APIError", "server overloaded")));
        let nudge_words = format!("Carry on: {NUDGE_MARK} (provider overloaded)");
        let (_dir, path) = fixture(
            "kilo.db",
            &[
                ("s1", "/tmp/worktree", 1_793_073_600_000, &error),
                (
                    "s1",
                    "/tmp/worktree",
                    1_793_073_600_100,
                    &user(1_793_073_600_100),
                ),
                ("s1", "/tmp/worktree", 1_793_074_000_000, &error),
            ],
        );
        add_words(&path, 1, &nudge_words);
        assert_eq!(cut_sqlite(&path, worktree).unwrap().unwrap().tries, 2);

        let person = r#"{"role":"user","time":{"created":1793074200000}}"#;
        let third = assistant(1_793_075_000_000, Some(("APIError", "server overloaded")));
        let (_dir, path) = fixture(
            "kilo.db",
            &[
                ("s1", "/tmp/worktree", 1_793_073_600_000, &error),
                (
                    "s1",
                    "/tmp/worktree",
                    1_793_073_600_100,
                    &user(1_793_073_600_100),
                ),
                ("s1", "/tmp/worktree", 1_793_074_000_000, &error),
                ("s1", "/tmp/worktree", 1_793_074_100_000, person),
                ("s1", "/tmp/worktree", 1_793_075_000_000, &third),
            ],
        );
        add_words(&path, 1, &nudge_words);
        add_words(&path, 3, "try again");
        assert_eq!(cut_sqlite(&path, worktree).unwrap().unwrap().tries, 1);
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
