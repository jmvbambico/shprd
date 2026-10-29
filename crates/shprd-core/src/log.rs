//! The append-only JSONL event log.
//!
//! One file per run (`<repo>/.shprd/events.jsonl`), one [`Event`] per line,
//! `\n`-terminated. Appends take an exclusive lock on the file so two writers
//! cannot interleave a line or reuse a `seq`. Reads are strict: a complete
//! line that does not parse, or a sequence that skips, is an error. Only a
//! torn final line (a crash mid-write) is tolerated: a final line with no
//! trailing newline is never an event, even when it parses. An append drops
//! such a tail before writing, so the next event still starts a clean line.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde_json::Error as JsonError;

use crate::event::{Actor, Event, EventKind};

/// Wall-clock source, injectable so tests are deterministic.
pub trait Clock {
    /// Milliseconds since the Unix epoch.
    fn now_ms(&self) -> u64;
}

/// The real clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            Ok(elapsed) => u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
            // Clock is set before the epoch: report the epoch rather than fail.
            Err(_) => 0,
        }
    }
}

/// What [`read_all`](EventLog::read_all) found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadOutcome {
    /// Every event that parsed, in log order.
    pub events: Vec<Event>,
    /// True when the file ended mid-line: the final line had no trailing
    /// newline, so it was treated as a torn write from a crash and skipped,
    /// whether or not it parsed.
    pub truncated_tail: bool,
}

/// Anything that can go wrong reading or appending to the log.
#[derive(Debug, thiserror::Error)]
pub enum LogError {
    /// The log file could not be created, read or written.
    #[error("i/o error on {path}: {source}")]
    Io {
        /// The path involved.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// An event could not be turned into JSON.
    #[error("could not serialize event: {0}")]
    Serialize(#[from] JsonError),
    /// A complete line is not a valid event.
    #[error("corrupt event at line {line}: {source}")]
    Corrupt {
        /// 1-based line number.
        line: usize,
        /// Why it did not parse.
        source: JsonError,
    },
    /// `seq` did not continue the log.
    #[error("bad sequence at line {line}: expected {expected}, found {found}")]
    BadSequence {
        /// 1-based line number.
        line: usize,
        /// The sequence number that line should have carried.
        expected: u64,
        /// The sequence number it actually carried.
        found: u64,
    },
}

/// An open handle for appending to a run's event log.
#[derive(Debug)]
pub struct EventLog {
    path: PathBuf,
    next_seq: u64,
}

impl EventLog {
    /// Open (creating if needed) the log at `path`, with parent directories.
    ///
    /// Existing events are read to work out the next `seq`: the last one plus
    /// one, or 1 for an empty log.
    pub fn open(path: &Path) -> Result<Self, LogError> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|source| LogError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        ensure_file(path)?;
        let next_seq = match Self::read_all(path)?.events.last() {
            Some(last) => last.seq.saturating_add(1),
            None => 1,
        };
        Ok(Self {
            path: path.to_path_buf(),
            next_seq,
        })
    }

    /// The log file's path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The `seq` the next append will carry.
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// Append one event and return it.
    ///
    /// The file is locked exclusively for the whole operation; the last `seq`
    /// is re-read under the lock because another process may have appended
    /// since this handle was opened. A torn final line left by a crash is
    /// truncated away first, so the new event never shares a line with it.
    /// Exactly one `\n`-terminated JSON line is written and flushed to disk
    /// before returning.
    pub fn append(
        &mut self,
        actor: Actor,
        kind: EventKind,
        clock: &dyn Clock,
    ) -> Result<Event, LogError> {
        let file = OpenOptions::new()
            .read(true)
            .append(true)
            .open(&self.path)
            .map_err(|source| LogError::Io {
                path: self.path.clone(),
                source,
            })?;
        file.lock().map_err(|source| LogError::Io {
            path: self.path.clone(),
            source,
        })?;

        let outcome = read_locked(&file, &self.path)?;
        // A crash can leave a partial line behind. Reads skip it, but an append
        // would glue the new event onto it, so cut it off before writing.
        repair_torn_tail(&file, &self.path)?;
        let seq = match outcome.events.last() {
            Some(last) => last.seq.saturating_add(1),
            None => 1,
        };
        let event = Event {
            seq,
            at_ms: clock.now_ms(),
            actor,
            kind,
        };

        let result = write_line(&file, &event, &self.path).and_then(|()| {
            file.sync_data().map_err(|source| LogError::Io {
                path: self.path.clone(),
                source,
            })
        });
        let unlocked = file.unlock().map_err(|source| LogError::Io {
            path: self.path.clone(),
            source,
        });
        result?;
        unlocked?;

        self.next_seq = seq.saturating_add(1);
        Ok(event)
    }

    /// Read and validate every event in the log at `path`.
    ///
    /// Every complete line must parse and carry the next `seq`; a final line
    /// with no trailing newline is treated as a torn write and skipped, with
    /// [`ReadOutcome::truncated_tail`] set, whether or not it parses.
    pub fn read_all(path: &Path) -> Result<ReadOutcome, LogError> {
        let file = File::open(path).map_err(|source| LogError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        read_locked(&file, path)
    }
}

/// Create the log file if it does not exist yet, without truncating it.
fn ensure_file(path: &Path) -> Result<(), LogError> {
    OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .open(path)
        .map(|_| ())
        .map_err(|source| LogError::Io {
            path: path.to_path_buf(),
            source,
        })
}

/// Write one event as a single JSON line and flush it to the OS.
fn write_line(mut file: &File, event: &Event, path: &Path) -> Result<(), LogError> {
    let mut line = serde_json::to_string(event)?;
    line.push('\n');
    let io_err = |source: std::io::Error| LogError::Io {
        path: path.to_path_buf(),
        source,
    };
    file.write_all(line.as_bytes()).map_err(io_err)?;
    file.flush().map_err(io_err)
}

/// Drop any bytes after the final `\n` so the next append starts a clean line.
///
/// Only bytes after the last newline are ever dropped, so no complete line is
/// lost, and the cut is synced before the caller writes.
fn repair_torn_tail(file: &File, path: &Path) -> Result<(), LogError> {
    let io_err = |source: std::io::Error| LogError::Io {
        path: path.to_path_buf(),
        source,
    };
    let len = file.metadata().map_err(io_err)?.len();
    if len == 0 {
        return Ok(());
    }
    let mut reader = file;
    let mut keep = 0u64;
    let mut chunk = [0u8; 4096];
    let mut end = len;
    while end > 0 {
        let take = usize::try_from(end.min(4096)).unwrap_or(4096);
        let start = end.saturating_sub(take as u64);
        reader.seek(SeekFrom::Start(start)).map_err(io_err)?;
        reader.read_exact(&mut chunk[..take]).map_err(io_err)?;
        if let Some(index) = chunk[..take].iter().rposition(|byte| *byte == b'\n') {
            keep = start.saturating_add(index as u64).saturating_add(1);
            break;
        }
        end = start;
    }
    if keep == len {
        return Ok(());
    }
    file.set_len(keep).map_err(io_err)?;
    file.sync_all().map_err(io_err)
}

/// Parse and validate the whole stream behind `file`.
fn read_locked(file: &File, path: &Path) -> Result<ReadOutcome, LogError> {
    let mut reader = BufReader::new(file);
    let mut events: Vec<Event> = Vec::new();
    let mut truncated_tail = false;
    let mut line_no = 0usize;
    let mut buf: Vec<u8> = Vec::new();

    loop {
        buf.clear();
        let read = reader
            .read_until(b'\n', &mut buf)
            .map_err(|source| LogError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        if read == 0 {
            break;
        }
        line_no = line_no.saturating_add(1);
        if buf.last() != Some(&b'\n') {
            // Only a `\n`-terminated line is an event, so whatever trails the
            // last newline is a torn write from a crash, whether or not it
            // happens to parse. It was not appended, so it is skipped.
            truncated_tail = true;
            break;
        }
        let text: &[u8] = buf.get(..buf.len().saturating_sub(1)).unwrap_or(&[]);
        let event = serde_json::from_slice::<Event>(text).map_err(|source| LogError::Corrupt {
            line: line_no,
            source,
        })?;
        let expected = u64::try_from(events.len())
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        if event.seq != expected {
            return Err(LogError::BadSequence {
                line: line_no,
                expected,
                found: event.seq,
            });
        }
        events.push(event);
    }

    Ok(ReadOutcome {
        events,
        truncated_tail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::ReportStatus;

    /// A clock that always answers the same time.
    struct FixedClock(u64);

    impl Clock for FixedClock {
        fn now_ms(&self) -> u64 {
            self.0
        }
    }

    fn started() -> EventKind {
        EventKind::RunStarted {
            run_id: "run-1".to_string(),
            repo: "/tmp/repo".to_string(),
            base_branch: "main".to_string(),
            integration_branch: "integration/m1".to_string(),
        }
    }

    fn reported() -> EventKind {
        EventKind::Reported {
            task: "M1.1".to_string(),
            attempt: 1,
            status: ReportStatus::Completed,
            summary: "done".to_string(),
            tests: vec!["cargo test".to_string()],
            changed_paths: vec![],
            commit: None,
            skills_used: vec![],
        }
    }

    fn finished() -> EventKind {
        EventKind::RunFinished {
            abandoned: Vec::new(),
        }
    }

    fn open_log(dir: &Path) -> EventLog {
        match EventLog::open(&dir.join(".shprd/events.jsonl")) {
            Ok(log) => log,
            Err(err) => panic!("open failed: {err}"),
        }
    }

    fn append(log: &mut EventLog, kind: EventKind) -> Event {
        match log.append(Actor::Engine, kind, &FixedClock(1_700_000_000_000)) {
            Ok(event) => event,
            Err(err) => panic!("append failed: {err}"),
        }
    }

    fn read_all(log: &EventLog) -> ReadOutcome {
        match EventLog::read_all(log.path()) {
            Ok(outcome) => outcome,
            Err(err) => panic!("read_all failed: {err}"),
        }
    }

    fn write_raw(path: &Path, text: &str) {
        if let Err(err) = std::fs::write(path, text) {
            panic!("write failed: {err}");
        }
    }

    fn line_of(event: &Event) -> String {
        match serde_json::to_string(event) {
            Ok(text) => text,
            Err(err) => panic!("serialize failed: {err}"),
        }
    }

    #[test]
    fn append_three_reopen_and_continue() {
        let dir = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(err) => panic!("tempdir: {err}"),
        };
        let path = dir.path().join(".shprd/events.jsonl");

        let mut log = open_log(dir.path());
        assert_eq!(log.next_seq(), 1, "a fresh log starts at seq 1");
        assert!(path.exists(), "open creates the file and its parents");

        let first = append(&mut log, started());
        let second = append(&mut log, reported());
        let third = append(&mut log, finished());
        assert_eq!(
            (first.seq, second.seq, third.seq),
            (1, 2, 3),
            "seq starts at 1 and increases by one"
        );
        assert_eq!(first.at_ms, 1_700_000_000_000);
        assert_eq!(first.actor, Actor::Engine);
        assert_eq!(log.next_seq(), 4);
        drop(log);

        let outcome = match EventLog::read_all(&path) {
            Ok(outcome) => outcome,
            Err(err) => panic!("read_all failed: {err}"),
        };
        assert!(!outcome.truncated_tail);
        assert_eq!(outcome.events, vec![first.clone(), second, third]);
        assert_eq!(
            outcome.events.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );

        let mut reopened = match EventLog::open(&path) {
            Ok(log) => log,
            Err(err) => panic!("reopen failed: {err}"),
        };
        assert_eq!(reopened.next_seq(), 4, "reopen resumes after the last seq");
        let fourth = append(&mut reopened, finished());
        assert_eq!(fourth.seq, 4, "the next append continues the log");
    }

    #[test]
    fn one_json_line_per_event() {
        let dir = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(err) => panic!("tempdir: {err}"),
        };
        let mut log = open_log(dir.path());
        append(&mut log, started());
        append(&mut log, reported());

        let text = match std::fs::read_to_string(log.path()) {
            Ok(text) => text,
            Err(err) => panic!("read failed: {err}"),
        };
        assert!(text.ends_with('\n'), "every line is newline-terminated");
        assert_eq!(text.lines().count(), 2, "{text}");
        for line in text.lines() {
            assert!(line.contains("\"kind\":\""), "kind present in {line}");
            if let Err(err) = serde_json::from_str::<Event>(line) {
                panic!("line did not parse: {err}\n{line}");
            }
        }
    }

    #[test]
    fn torn_tail_is_skipped() {
        let dir = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(err) => panic!("tempdir: {err}"),
        };
        let mut log = open_log(dir.path());
        let first = append(&mut log, started());
        let second = append(&mut log, finished());

        let mut text = match std::fs::read_to_string(log.path()) {
            Ok(text) => text,
            Err(err) => panic!("read failed: {err}"),
        };
        text.push_str("{\"seq\":3,\"at_ms\":17,\"actor\":{\"type\":\"eng");
        write_raw(log.path(), &text);

        let outcome = read_all(&log);
        assert!(outcome.truncated_tail, "torn tail must be flagged");
        assert_eq!(outcome.events, vec![first, second]);

        let mut reopened = match EventLog::open(log.path()) {
            Ok(log) => log,
            Err(err) => panic!("reopen failed: {err}"),
        };
        let third = append(&mut reopened, finished());
        assert_eq!(third.seq, 3, "the torn tail did not consume a seq");
    }

    #[test]
    fn append_repairs_a_torn_tail_first() {
        let dir = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(err) => panic!("tempdir: {err}"),
        };
        let mut log = open_log(dir.path());
        let first = append(&mut log, started());
        let second = append(&mut log, finished());

        let mut text = match std::fs::read_to_string(log.path()) {
            Ok(text) => text,
            Err(err) => panic!("read failed: {err}"),
        };
        text.push_str("{\"seq\":3,\"at_ms\":17,\"actor\":{\"type\":\"eng");
        write_raw(log.path(), &text);

        let third = append(&mut log, finished());
        assert_eq!(third.seq, 3, "the torn tail did not consume a seq");
        let after = match std::fs::read_to_string(log.path()) {
            Ok(text) => text,
            Err(err) => panic!("read failed: {err}"),
        };
        assert!(after.ends_with('\n'), "the log ends on a line boundary");
        for line in after.lines() {
            if let Err(err) = serde_json::from_str::<Event>(line) {
                panic!("corrupt line: {err}\n{line}");
            }
        }

        let outcome = read_all(&log);
        assert!(!outcome.truncated_tail, "the torn tail was repaired");
        assert_eq!(
            outcome.events.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(outcome.events, vec![first, second, third]);
    }

    #[test]
    fn complete_json_without_a_newline_is_still_torn() {
        let dir = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(err) => panic!("tempdir: {err}"),
        };
        let mut log = open_log(dir.path());
        let first = append(&mut log, started());
        let second = append(&mut log, finished());

        let pending = Event {
            seq: 3,
            at_ms: 17,
            actor: Actor::Human,
            kind: finished(),
        };
        let mut text = match std::fs::read_to_string(log.path()) {
            Ok(text) => text,
            Err(err) => panic!("read failed: {err}"),
        };
        text.push_str(&line_of(&pending));
        write_raw(log.path(), &text);

        let outcome = read_all(&log);
        assert!(
            outcome.truncated_tail,
            "an unterminated line is torn even when it parses"
        );
        assert_eq!(outcome.events, vec![first, second]);

        let next = append(&mut log, finished());
        assert_eq!(next.seq, 3, "the torn tail did not consume a seq");
        assert_eq!(read_all(&log).events.len(), 3, "the tail was dropped");
    }

    #[test]
    fn corrupt_middle_line_is_an_error() {
        let dir = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(err) => panic!("tempdir: {err}"),
        };
        let mut log = open_log(dir.path());
        let first = append(&mut log, started());
        append(&mut log, finished());

        let text = format!("{}\nnot json\n{}\n", line_of(&first), line_of(&first));
        write_raw(log.path(), &text);

        match EventLog::read_all(log.path()) {
            Err(LogError::Corrupt { line, .. }) => assert_eq!(line, 2),
            other => panic!("expected Corrupt at line 2, got {other:?}"),
        }
    }

    #[test]
    fn sequence_gap_is_an_error() {
        let dir = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(err) => panic!("tempdir: {err}"),
        };
        let path = dir.path().join(".shprd/events.jsonl");
        let mut log = open_log(dir.path());
        let first = append(&mut log, started());
        assert_eq!(first.seq, 1);

        let gap = line_of(&Event {
            seq: 3,
            at_ms: 1,
            actor: Actor::Human,
            kind: finished(),
        });
        write_raw(&path, &format!("{}\n{}\n", line_of(&first), gap));

        match EventLog::read_all(&path) {
            Err(LogError::BadSequence {
                line,
                expected,
                found,
            }) => {
                assert_eq!((line, expected, found), (2, 2, 3));
            }
            other => panic!("expected BadSequence, got {other:?}"),
        }
    }

    #[test]
    fn system_clock_is_monotonic_enough() {
        let clock = SystemClock;
        let first = clock.now_ms();
        assert!(
            first > 1_600_000_000_000,
            "clock looks like unix ms: {first}"
        );
        assert!(clock.now_ms() >= first);
    }
}
