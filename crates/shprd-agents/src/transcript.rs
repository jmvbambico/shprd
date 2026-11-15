//! Reading an agent's own record as a conversation.
//!
//! Every supported CLI already keeps a record of what it did: the watchdog
//! reads it for provider errors ([`shprd_watch`]'s `records`), and this module
//! reads the same files for the chat. The two questions are different — an
//! error reader wants the newest failure out of a whole record, a chat wants the
//! person's words, the agent's words and a summary of what it ran, in order —
//! so the shapes here are a transcript's rather than an error reader's.
//!
//! # Where and how, from the table
//!
//! A harness says both facts in its `[transcript]` section
//! ([`crate::spec::TranscriptSpec`]): the [`TranscriptFormat`] says how to read
//! one session, and the location pattern says where that session lives. Nothing
//! here knows a program by name, so a CLI that moves its files is a table change
//! (`data/harness/*.toml`) rather than a release.
//!
//! The location is the same pattern language a harness's `[records]` location
//! uses — `~`, `{cwd_slug}`, `{session}`, `{yyyy}`, `{mm}`, `{dd}` and `*` — with
//! one addition: for a chat, the session is often one the service did not start
//! and has no id for, so the placeholders and `*` are matched as wildcards when
//! looking. `{cwd_slug}` is the program's own name for a working directory,
//! because Claude Code and pi fold a path into a folder name differently
//! ([`cwd_slug`]).
//!
//! # The common shape
//!
//! A [`Message`] is one of three things a person reads: what they said
//! ([`Role::User`]), what the agent said ([`Role::Assistant`]) and, kept with
//! the reply rather than as a row of their own, the [`ToolCall`]s the agent ran.
//! A tool's output is not in the transcript: it belongs to a row that is opened,
//! not to the conversation's shape, and it is often kilobytes a panel never
//! shows.
//!
//! # Reading in pieces
//!
//! [`Transcript::read`] takes a cursor and answers what came after it, so a long
//! session is read once and then followed. A JSONL record's cursor is a byte
//! offset into that file: only what was appended is parsed, a half-written last
//! line is left for the next read, and a file that was replaced or truncated
//! starts over rather than seeking past its end. OpenCode keeps a session as
//! one file a message and a folder of parts for each message, so its cursor is
//! the index of the next message to deliver and only the new parts are read.
//!
//! One read is bounded ([`MAX_BYTES`], [`MAX_MESSAGES`]) so a first look at a
//! long session answers quickly; [`Page::more`] says another call has work to
//! do.
//!
//! The formats are ported from Shepherdr's own chat reader.
//!
//! - Source: <https://github.com/eliasstravik/shepherdr>
//! - Paths: `core/chat-log.js` (`LOG`, the line kinds it keeps and the shapes it
//!   reads them from) and `core/chat-reader.js` (`ChatReader.pull`, the
//!   newest-page-then-what-it-gained shape)
//! - Commit: `a3f327a` (tag `v3.1.34`)
//! - License: MIT — Copyright (c) 2026 Elias Stravik

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::Value;

use crate::spec::{TranscriptFormat, TranscriptSpec};

/// How much of a JSONL record one read parses at most.
///
/// It is a page, not the whole file: the first look at a long session must
/// answer a panel quickly, and `Page::more` says to come back for the rest.
pub const MAX_BYTES: usize = 512 * 1024;

/// How many messages one read answers at most.
pub const MAX_MESSAGES: usize = 200;

/// Who said a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The person.
    User,
    /// The agent.
    Assistant,
}

impl Role {
    /// The word a client shows, which is also its JSON spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }
}

/// One tool the agent ran, summarised to what a chat row shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    /// The program's own name for the tool, e.g. `Bash`.
    pub name: String,
    /// One line of what it ran or read, already cut short.
    pub summary: String,
}

/// One message of a conversation, in the shape every program's record folds to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// Who said it.
    pub role: Role,
    /// The words, with a program's own markup removed. May be empty when the
    /// message is tool calls only.
    pub text: String,
    /// The tools the agent ran as part of this reply, in order.
    pub tools: Vec<ToolCall>,
    /// When the program recorded it, in ms since the epoch, when it did.
    pub at_ms: Option<i64>,
}

impl Message {
    /// Whether there is nothing a person would read.
    fn is_empty(&self) -> bool {
        self.text.trim().is_empty() && self.tools.is_empty()
    }
}

/// What one read answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    /// The messages after the cursor, oldest first.
    pub messages: Vec<Message>,
    /// Where to ask from next.
    pub cursor: String,
    /// Whether the record had more to give when the page filled up.
    pub more: bool,
}

/// One session's record, found and ready to read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transcript {
    format: TranscriptFormat,
    session: String,
    source: Source,
}

/// Where a located session's bytes are.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Source {
    /// One file, read from a byte offset: the JSONL programs.
    File(PathBuf),
    /// OpenCode's storage folder, where a session is a folder of message files
    /// and each message a folder of parts.
    Store { root: PathBuf, session: String },
}

/// A transcript that could not be read.
#[derive(Debug, thiserror::Error)]
pub enum TranscriptError {
    /// The record's file could not be opened or read.
    #[error("failed to read the transcript {path}: {source}")]
    Read {
        /// The file that failed.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// A store's folder could not be listed.
    #[error("failed to list the transcript folder {path}: {source}")]
    List {
        /// The folder that failed.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
}

impl Transcript {
    /// The session's own name: the record's file stem, or OpenCode's session id.
    pub fn session(&self) -> &str {
        &self.session
    }

    /// How the record is read.
    pub fn format(&self) -> TranscriptFormat {
        self.format
    }

    /// The file the session is read from, when it is one file.
    pub fn path(&self) -> Option<&Path> {
        match &self.source {
            Source::File(path) => Some(path),
            Source::Store { .. } => None,
        }
    }

    /// Find the newest session `format`/`location` describes for `cwd`.
    ///
    /// A program whose files for this directory are not there answers `None`:
    /// an agent that has not written a session yet has nothing to show, which is
    /// an ordinary answer rather than a failure. Codex's rollouts live in a date
    /// tree shared by every directory, so each candidate is asked which
    /// directory it belongs to rather than trusted by its name.
    pub fn locate(spec: &TranscriptSpec, home: &Path, cwd: &Path) -> Option<Transcript> {
        let format = spec.format?;
        let location = spec.location.as_deref()?;
        match format {
            TranscriptFormat::OpencodeStorage => locate_store(&expand_path(location, home)?, cwd),
            _ => locate_file(format, location, home, cwd),
        }
    }

    /// Read what the record gained since `since`.
    ///
    /// A cursor from another session — a program that started a new one, a task
    /// that was handed off — reads from the beginning, because the bytes it
    /// named are another session's.
    pub fn read(&self, since: &str) -> Result<Page, TranscriptError> {
        let from = self.offset(since);
        match &self.source {
            Source::File(path) => self.read_file(path, from),
            Source::Store { root, session } => self.read_store(root, session, from),
        }
    }

    /// The position `since` names in this session, or 0 when it names none.
    fn offset(&self, since: &str) -> usize {
        let Some((session, position)) = since.rsplit_once('@') else {
            return 0;
        };
        if session != self.session {
            return 0;
        }
        position.trim().parse::<usize>().unwrap_or(0)
    }

    /// The cursor naming `position` in this session.
    fn cursor(&self, position: usize) -> String {
        format!("{}@{position}", self.session)
    }

    /// One JSONL record: seek to `from`, parse what was appended.
    fn read_file(&self, path: &Path, from: usize) -> Result<Page, TranscriptError> {
        let mut file = std::fs::File::open(path).map_err(|source| TranscriptError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let len = file
            .metadata()
            .map_err(|source| TranscriptError::Read {
                path: path.to_path_buf(),
                source,
            })?
            .len();
        // A file shorter than the cursor was replaced (a new session writing
        // over an old name) or truncated: its offsets mean nothing now.
        let start = if (from as u64) > len { 0 } else { from as u64 };
        file.seek(SeekFrom::Start(start))
            .map_err(|source| TranscriptError::Read {
                path: path.to_path_buf(),
                source,
            })?;
        let mut buffer = vec![0u8; MAX_BYTES];
        let read = file
            .read(&mut buffer)
            .map_err(|source| TranscriptError::Read {
                path: path.to_path_buf(),
                source,
            })?;
        buffer.truncate(read);
        let at_eof = start + read as u64 >= len;
        let (body, _) = whole_lines(&buffer, at_eof);
        let mut messages = Vec::new();
        let mut position = start as usize;
        for line in body.split_inclusive(|byte| *byte == b'\n') {
            // A page that fills up stops at a line boundary, so the next read
            // starts at that line rather than in the middle of it.
            if messages.len() >= MAX_MESSAGES {
                break;
            }
            position += line.len();
            let text = line.strip_suffix(b"\n").unwrap_or(line);
            if let Some(message) = parse_line(self.format, text) {
                messages.push(message);
            }
        }
        let more = position < len as usize;
        Ok(Page {
            messages,
            cursor: self.cursor(position),
            more,
        })
    }

    /// OpenCode's store: the messages after `from`, with their parts.
    fn read_store(&self, root: &Path, session: &str, from: usize) -> Result<Page, TranscriptError> {
        let messages = store_messages(root, session)?;
        let total = messages.len();
        let mut page = Vec::new();
        let mut position = from.min(total);
        while position < total && page.len() < MAX_MESSAGES {
            let (path, id) = &messages[position];
            if let Some(message) = store_message(root, id, path) {
                page.push(message);
            }
            position += 1;
        }
        Ok(Page {
            messages: page,
            cursor: self.cursor(position),
            more: position < total,
        })
    }
}

/// The whole lines of `buffer`, and how many bytes they take.
///
/// A half-written last line is not a line: a program appending its record can
/// be caught mid-write, and parsing half a JSON object would either fail or,
/// worse, parse something the agent never said. The exception is a tail that is
/// already a complete value, which is how a record whose very last line has no
/// trailing newline still shows.
fn whole_lines(buffer: &[u8], at_eof: bool) -> (&[u8], usize) {
    let Some(last) = buffer.iter().rposition(|byte| *byte == b'\n') else {
        if at_eof && serde_json::from_slice::<Value>(buffer).is_ok() {
            return (buffer, buffer.len());
        }
        return (&[], 0);
    };
    let tail = &buffer[last + 1..];
    if !tail.is_empty() && at_eof && serde_json::from_slice::<Value>(tail).is_ok() {
        return (buffer, buffer.len());
    }
    (&buffer[..=last], last + 1)
}

/// One JSONL line as a message, or `None` when the line is bookkeeping.
///
/// Malformed lines are bookkeeping too: a record is read as it is written, and
/// one line that does not parse must not blind the rest of the conversation.
fn parse_line(format: TranscriptFormat, line: &[u8]) -> Option<Message> {
    if line.iter().all(|byte| byte.is_ascii_whitespace()) {
        return None;
    }
    let value: Value = serde_json::from_slice(line).ok()?;
    match format {
        TranscriptFormat::ClaudeJsonl => claude_message(&value),
        TranscriptFormat::CodexJsonl => codex_message(&value),
        TranscriptFormat::PiJsonl => pi_message(&value),
        TranscriptFormat::OpencodeStorage => None,
    }
}

/// Claude Code: `type: user` and `type: assistant` lines of the main chain.
///
/// A subagent's own conversation is a sidechain (`isSidechain: true`) and is
/// its parent's tool call, not a message the person wrote or read.
fn claude_message(value: &Value) -> Option<Message> {
    if value.get("isSidechain") == Some(&Value::Bool(true))
        || value.get("isApiErrorMessage") == Some(&Value::Bool(true))
    {
        return None;
    }
    let kind = value.get("type")?.as_str()?;
    let content = value.get("message")?.get("content")?;
    let at_ms = value
        .get("timestamp")
        .and_then(|at| at.as_str())
        .and_then(iso_ms);
    let (role, text, tools) = match kind {
        "user" => (Role::User, texts(content, &["text"]), Vec::new()),
        "assistant" => {
            let text = texts(content, &["text"]);
            let tools = tool_uses(content, "tool_use", "input");
            (Role::Assistant, text, tools)
        }
        _ => return None,
    };
    let message = Message {
        role,
        text,
        tools,
        at_ms,
    };
    (!message.is_empty()).then_some(message)
}

/// Pi: `type: message` lines, whose own `message.role` says who spoke.
fn pi_message(value: &Value) -> Option<Message> {
    if value.get("type")?.as_str()? != "message" {
        return None;
    }
    let message = value.get("message")?;
    let role = match message.get("role")?.as_str()? {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        // A tool's output is its call's result, not a message of its own.
        _ => return None,
    };
    let content = message.get("content")?;
    let text = texts(content, &["text"]);
    let tools = if role == Role::Assistant {
        tool_uses(content, "toolCall", "arguments")
    } else {
        Vec::new()
    };
    let at_ms = message.get("timestamp").and_then(number_ms).or_else(|| {
        value
            .get("timestamp")
            .and_then(|at| at.as_str())
            .and_then(iso_ms)
    });
    let message = Message {
        role,
        text,
        tools,
        at_ms,
    };
    (!message.is_empty()).then_some(message)
}

/// Codex: `response_item` lines holding a message or a tool call.
///
/// Codex records its own brief as `developer` messages; a person never wrote
/// them and a chat never shows them.
fn codex_message(value: &Value) -> Option<Message> {
    if value.get("type")?.as_str()? != "response_item" {
        return None;
    }
    let payload = value.get("payload")?;
    let at_ms = value
        .get("timestamp")
        .and_then(|at| at.as_str())
        .and_then(iso_ms);
    match payload.get("type")?.as_str()? {
        "message" => {
            let role = match payload.get("role")?.as_str()? {
                "user" => Role::User,
                "assistant" => Role::Assistant,
                _ => return None,
            };
            let content = payload.get("content")?;
            let text = texts(content, &["input_text", "output_text", "text"]);
            let message = Message {
                role,
                text,
                tools: Vec::new(),
                at_ms,
            };
            (!message.is_empty()).then_some(message)
        }
        "function_call" => Some(tool_message(
            payload.get("name")?.as_str()?,
            payload.get("arguments").and_then(json_text).as_deref(),
            at_ms,
        )),
        "custom_tool_call" => Some(tool_message(
            payload.get("name")?.as_str()?,
            payload.get("input").and_then(json_text).as_deref(),
            at_ms,
        )),
        _ => None,
    }
}

/// A tool call on its own, as Codex records one.
fn tool_message(name: &str, arguments: Option<&str>, at_ms: Option<i64>) -> Message {
    let summary = match arguments {
        Some(text) => match serde_json::from_str::<Value>(text) {
            Ok(value) => summary_of(&value),
            Err(_) => clip(text),
        },
        None => String::new(),
    };
    Message {
        role: Role::Assistant,
        text: String::new(),
        tools: vec![ToolCall {
            name: name.to_owned(),
            summary,
        }],
        at_ms,
    }
}

/// The text of a message's content, which a program writes as a string or as a
/// list of typed blocks.
///
/// `kinds` names the block types this program uses for a person's or an agent's
/// words; every other block (thinking, a tool's result, an image) is not text a
/// chat row shows.
fn texts(content: &Value, kinds: &[&str]) -> String {
    match content {
        Value::String(text) => text.trim().to_owned(),
        Value::Array(blocks) => {
            let parts: Vec<&str> = blocks
                .iter()
                .filter(|block| {
                    block
                        .get("type")
                        .and_then(Value::as_str)
                        .is_some_and(|kind| kinds.contains(&kind))
                })
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .collect();
            parts.join("\n\n")
        }
        _ => String::new(),
    }
}

/// The tool calls of one reply, from the blocks of `kind`.
fn tool_uses(content: &Value, kind: &str, arguments: &str) -> Vec<ToolCall> {
    let Value::Array(blocks) = content else {
        return Vec::new();
    };
    blocks
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some(kind))
        .map(|block| ToolCall {
            name: block
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("tool")
                .to_owned(),
            summary: block.get(arguments).map(summary_of).unwrap_or_default(),
        })
        .collect()
}

/// One line about a tool call: the field a person would want first, else the
/// call's own words cut short.
fn summary_of(value: &Value) -> String {
    const FIELDS: [&str; 10] = [
        "command",
        "file_path",
        "path",
        "pattern",
        "description",
        "query",
        "url",
        "prompt",
        "input",
        "text",
    ];
    if let Value::Object(fields) = value {
        for field in FIELDS {
            if let Some(found) = fields.get(field).and_then(Value::as_str)
                && !found.trim().is_empty()
            {
                return clip(found);
            }
        }
    }
    match value {
        Value::String(text) => clip(text),
        other => clip(&other.to_string()),
    }
}

/// `text`'s first line, cut to a length a row can show.
fn clip(text: &str) -> String {
    const LIMIT: usize = 120;
    let line = text.lines().next().unwrap_or("").trim();
    if line.chars().count() <= LIMIT {
        return line.to_owned();
    }
    let kept: String = line.chars().take(LIMIT).collect();
    format!("{kept}…")
}

/// A JSON value as text, for a field that a program sometimes writes as a
/// string and sometimes as an object.
fn json_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Null => None,
        other => Some(other.to_string()),
    }
}

/// A millisecond timestamp, whichever of the two shapes a program writes.
fn number_ms(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_f64().map(|ms| ms as i64))
        .or_else(|| value.as_str().and_then(iso_ms))
}

/// An RFC 3339 timestamp as ms since the epoch.
fn iso_ms(text: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|at| at.timestamp_millis())
}

// ---------------------------------------------------------------------------
// Finding the session
// ---------------------------------------------------------------------------

/// The newest file a pattern names, for the JSONL programs.
fn locate_file(
    format: TranscriptFormat,
    location: &str,
    home: &Path,
    cwd: &Path,
) -> Option<Transcript> {
    let (root, components) = pattern_parts(location, home, cwd, format)?;
    let mut found: Option<(SystemTime, PathBuf)> = None;
    walk(&root, &components, 0, &mut |path| {
        if format == TranscriptFormat::CodexJsonl && !codex_belongs(path, cwd) {
            return;
        }
        let Ok(modified) = std::fs::metadata(path).and_then(|meta| meta.modified()) else {
            return;
        };
        if found.as_ref().is_none_or(|(kept, _)| modified >= *kept) {
            found = Some((modified, path.to_path_buf()));
        }
    });
    let (_, path) = found?;
    let session = path.file_stem()?.to_str()?.to_owned();
    Some(Transcript {
        format,
        session,
        source: Source::File(path),
    })
}

/// The newest OpenCode session whose own `directory` is `cwd`.
///
/// OpenCode stores one folder a project and one file a session, so the session
/// that belongs to a directory is the one that says so, not the newest one on
/// the machine. The comparison follows symlinks: a session recorded from the
/// resolved path belongs to the directory a person names by its symlinked one.
fn locate_store(root: &Path, cwd: &Path) -> Option<Transcript> {
    let sessions = root.join("session");
    let mut candidates: Vec<(i64, String, PathBuf)> = Vec::new();
    for project in std::fs::read_dir(&sessions).ok()?.flatten() {
        let Ok(files) = std::fs::read_dir(project.path()) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            let Some(id) = value.get("id").and_then(Value::as_str) else {
                continue;
            };
            // A session that records no directory at all is not refused: an
            // older OpenCode writes the folder only into the message parts.
            let directory = value.get("directory").and_then(Value::as_str);
            if directory.is_some_and(|directory| !same_dir(Path::new(directory), cwd)) {
                continue;
            }
            let updated = value
                .get("time")
                .and_then(|time| time.get("updated"))
                .and_then(number_ms)
                .unwrap_or(0);
            candidates.push((updated, id.to_owned(), path));
        }
    }
    candidates.sort_by_key(|candidate| candidate.0);
    let (_, session, _) = candidates.pop()?;
    Some(Transcript {
        format: TranscriptFormat::OpencodeStorage,
        source: Source::Store {
            root: root.to_path_buf(),
            session: session.clone(),
        },
        session,
    })
}

/// Whether a codex rollout's first line names `cwd`.
fn codex_belongs(path: &Path, cwd: &Path) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut line = String::new();
    if std::io::BufRead::read_line(&mut std::io::BufReader::new(file), &mut line).is_err() {
        return false;
    }
    let Ok(value) = serde_json::from_str::<Value>(&line) else {
        return false;
    };
    value
        .get("payload")
        .and_then(|payload| payload.get("cwd"))
        .and_then(Value::as_str)
        .is_some_and(|recorded| same_dir(Path::new(recorded), cwd))
}

/// Whether two paths name the same directory.
///
/// A record names the directory its program saw, which on macOS is the resolved
/// form (`/private/var/…`) while a person's command line may hold the symlinked
/// one (`/tmp/…`), so equality of the text is not enough: both are resolved when
/// they are on disk and compared again.
fn same_dir(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

/// OpenCode's message files for one session, oldest first, as `(path, id)`.
fn store_messages(root: &Path, session: &str) -> Result<Vec<(PathBuf, String)>, TranscriptError> {
    let dir = root.join("message").join(session);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        // A session with no messages yet is not a failure: it is a session
        // that has just started.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(TranscriptError::List { path: dir, source }),
    };
    let mut found: Vec<(i64, String, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let Some(id) = value.get("id").and_then(Value::as_str) else {
            continue;
        };
        let created = value
            .get("time")
            .and_then(|time| time.get("created"))
            .and_then(number_ms)
            .unwrap_or(0);
        found.push((created, id.to_owned(), path));
    }
    // The id breaks a tie, so two messages created in the same millisecond keep
    // the order the program wrote them in.
    found.sort_by(|left, right| (left.0, &left.1).cmp(&(right.0, &right.1)));
    Ok(found.into_iter().map(|(_, id, path)| (path, id)).collect())
}

/// One OpenCode message, joined with the parts of which it is made.
fn store_message(root: &Path, id: &str, path: &Path) -> Option<Message> {
    let header = std::fs::read_to_string(path).ok()?;
    let header: Value = serde_json::from_str(&header).ok()?;
    let role = match header.get("role").and_then(Value::as_str)? {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        _ => return None,
    };
    let at_ms = header
        .get("time")
        .and_then(|time| time.get("created"))
        .and_then(number_ms);
    let parts = root.join("part").join(id);
    let mut texts = Vec::new();
    let mut tools = Vec::new();
    for part in std::fs::read_dir(&parts).ok()?.flatten() {
        let path = part.path();
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        match value.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = value.get("text").and_then(Value::as_str)
                    && !text.trim().is_empty()
                {
                    texts.push(text.trim().to_owned());
                }
            }
            Some("tool") => {
                let name = value
                    .get("tool")
                    .and_then(Value::as_str)
                    .unwrap_or("tool")
                    .to_owned();
                let summary = value
                    .get("state")
                    .and_then(|state| state.get("input"))
                    .map(summary_of)
                    .unwrap_or_default();
                tools.push(ToolCall { name, summary });
            }
            // Reasoning, step markers, patches and snapshots are bookkeeping.
            _ => {}
        }
    }
    let message = Message {
        role,
        text: texts.join("\n\n"),
        tools,
        at_ms,
    };
    (!message.is_empty()).then_some(message)
}

// ---------------------------------------------------------------------------
// The location pattern
// ---------------------------------------------------------------------------

/// A location pattern as a root and the components under it.
///
/// `~` is the home directory and a leading `/` the filesystem's root; a pattern
/// with neither names nothing this reader can look under, which is a table
/// mistake rather than a missing session. `{cwd_slug}` becomes this format's
/// name for `cwd`, and every other placeholder stays `*`: a chat looks for a
/// session it was never told the id of.
fn pattern_parts(
    location: &str,
    home: &Path,
    cwd: &Path,
    format: TranscriptFormat,
) -> Option<(PathBuf, Vec<String>)> {
    let (root, body) = match (location.strip_prefix("~/"), location.strip_prefix('/')) {
        (Some(rest), _) => (home.to_path_buf(), rest),
        (None, Some(rest)) => (PathBuf::from("/"), rest),
        (None, None) => return None,
    };
    let slug = cwd_slug(format, cwd);
    let components = body
        .split('/')
        .map(|raw| {
            let mut text = raw.replace("{cwd_slug}", &slug);
            text = text.replace("{session}", "*");
            for placeholder in ["{yyyy}", "{mm}", "{dd}"] {
                text = text.replace(placeholder, "*");
            }
            text
        })
        .collect();
    Some((root, components))
}

/// A location with no wildcards as a path, for a store whose folder is named
/// outright.
fn expand_path(location: &str, home: &Path) -> Option<PathBuf> {
    if let Some(rest) = location.strip_prefix("~/") {
        return Some(home.join(rest));
    }
    location.starts_with('/').then(|| PathBuf::from(location))
}

/// The name `format`'s program gives `cwd` as one folder.
///
/// The two JSONL programs fold a path differently: Claude Code keeps the
/// leading separator and folds `/` and `.` into `-`, while pi drops the leading
/// separator, folds `/`, `\` and `:` into `-` and wraps the result in `--…--`.
pub fn cwd_slug(format: TranscriptFormat, cwd: &Path) -> String {
    let text = cwd.to_string_lossy();
    match format {
        TranscriptFormat::ClaudeJsonl => text.replace(['/', '.'], "-"),
        TranscriptFormat::PiJsonl => format!(
            "--{}--",
            text.trim_start_matches(['/', '\\'])
                .replace(['/', '\\', ':'], "-")
        ),
        // Codex's date tree and OpenCode's store name no directory.
        _ => text.replace(['/', '\\'], "-"),
    }
}

/// Run `visit` over every file `components` names under `root`.
///
/// A component holding `*` matches each entry of the directory above it, which
/// is how a session with no id is found; anything else is one path step, and a
/// step that is not there simply ends that branch of the search.
fn walk(root: &Path, components: &[String], at: usize, visit: &mut impl FnMut(&Path)) {
    let Some(head) = components.get(at) else {
        return;
    };
    let last = at + 1 == components.len();
    let wildcard = head.contains('*');
    if !wildcard {
        let path = root.join(head);
        if last {
            if path.is_file() {
                visit(&path);
            }
        } else {
            walk(&path, components, at + 1, visit);
        }
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !matches_name(head, &name) {
            continue;
        }
        if last {
            let path = entry.path();
            if path.is_file() {
                visit(&path);
            }
        } else {
            walk(&entry.path(), components, at + 1, visit);
        }
    }
}

/// Whether `name` matches a component pattern, where `*` is any run of
/// characters (including none) inside the one component.
pub fn matches_name(pattern: &str, name: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == name;
    }
    let mut rest = name;
    for (index, part) in parts.iter().enumerate() {
        if index == 0 {
            let Some(after) = rest.strip_prefix(part) else {
                return false;
            };
            rest = after;
            continue;
        }
        if index == parts.len() - 1 {
            return rest.ends_with(part);
        }
        match rest.find(part) {
            Some(found) => rest = &rest[found + part.len()..],
            None => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::path::Path;

    use super::{
        Role, TranscriptFormat, clip, cwd_slug, matches_name, parse_line, summary_of, whole_lines,
    };

    fn line(value: &serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(value).unwrap()
    }

    #[test]
    fn a_star_matches_inside_one_component() {
        assert!(matches_name("*.jsonl", "session.jsonl"));
        assert!(matches_name("rollout-*.jsonl", "rollout-2026-11-03.jsonl"));
        assert!(matches_name("--x--", "--x--"));
        assert!(!matches_name("*.jsonl", "session.json"));
        assert!(!matches_name("rollout-*.jsonl", "session.jsonl"));
    }

    #[test]
    fn each_program_folds_a_path_its_own_way() {
        let cwd = Path::new("/Users/me/.herdr/worktrees/repo");
        assert_eq!(
            cwd_slug(TranscriptFormat::ClaudeJsonl, cwd),
            "-Users-me--herdr-worktrees-repo"
        );
        assert_eq!(
            cwd_slug(TranscriptFormat::PiJsonl, cwd),
            "--Users-me-.herdr-worktrees-repo--"
        );
    }

    #[test]
    fn a_claude_user_message_is_the_persons_words() {
        let value = serde_json::json!({
            "type": "user",
            "isSidechain": false,
            "timestamp": "2026-11-04T23:49:25.622Z",
            "message": {"role": "user", "content": "  hello there  "}
        });
        let message = parse_line(TranscriptFormat::ClaudeJsonl, &line(&value)).unwrap();
        assert_eq!(message.role, Role::User);
        assert_eq!(message.text, "hello there");
        assert!(message.tools.is_empty());
        assert_eq!(message.at_ms, Some(1_793_836_165_622));
    }

    #[test]
    fn a_claude_subagent_line_is_not_a_message() {
        let value = serde_json::json!({
            "type": "user",
            "isSidechain": true,
            "message": {"role": "user", "content": "inside a subagent"}
        });
        assert!(parse_line(TranscriptFormat::ClaudeJsonl, &line(&value)).is_none());
    }

    #[test]
    fn a_claude_tool_result_is_not_a_message_of_its_own() {
        let value = serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": "output"}
            ]}
        });
        assert!(parse_line(TranscriptFormat::ClaudeJsonl, &line(&value)).is_none());
    }

    #[test]
    fn a_claude_reply_keeps_its_tool_calls_summarised() {
        let value = serde_json::json!({
            "type": "assistant",
            "message": {"role": "assistant", "content": [
                {"type": "thinking", "thinking": "hmm"},
                {"type": "text", "text": "Let me look."},
                {"type": "tool_use", "name": "Bash", "input": {"command": "ls\nsecond"}}
            ]}
        });
        let message = parse_line(TranscriptFormat::ClaudeJsonl, &line(&value)).unwrap();
        assert_eq!(message.text, "Let me look.");
        assert_eq!(message.tools.len(), 1);
        assert_eq!(message.tools[0].name, "Bash");
        assert_eq!(message.tools[0].summary, "ls");
    }

    #[test]
    fn a_pi_reply_carries_its_calls_and_its_result_is_skipped() {
        let assistant = serde_json::json!({
            "type": "message",
            "message": {
                "role": "assistant",
                "timestamp": 1794616781220i64,
                "content": [
                    {"type": "thinking", "thinking": "first"},
                    {"type": "text", "text": "On it."},
                    {"type": "toolCall", "name": "read", "arguments": {"path": "src/lib.rs"}}
                ]
            }
        });
        let message = parse_line(TranscriptFormat::PiJsonl, &line(&assistant)).unwrap();
        assert_eq!(message.text, "On it.");
        assert_eq!(message.tools[0].summary, "src/lib.rs");
        assert_eq!(message.at_ms, Some(1_794_616_781_220));
        let result = serde_json::json!({
            "type": "message",
            "message": {"role": "toolResult", "content": [{"type": "text", "text": "ok"}]}
        });
        assert!(parse_line(TranscriptFormat::PiJsonl, &line(&result)).is_none());
    }

    #[test]
    fn a_codex_developer_message_is_not_shown_and_a_call_is() {
        let developer = serde_json::json!({
            "type": "response_item",
            "payload": {"type": "message", "role": "developer", "content": [
                {"type": "input_text", "text": "You are Codex."}
            ]}
        });
        assert!(parse_line(TranscriptFormat::CodexJsonl, &line(&developer)).is_none());
        let call = serde_json::json!({
            "type": "response_item",
            "timestamp": "2026-11-03T00:16:05.000Z",
            "payload": {"type": "custom_tool_call", "name": "exec", "input": "const a = 1;\nmore"}
        });
        let message = parse_line(TranscriptFormat::CodexJsonl, &line(&call)).unwrap();
        assert!(message.text.is_empty());
        assert_eq!(message.tools[0].name, "exec");
        assert_eq!(message.tools[0].summary, "const a = 1;");
    }

    #[test]
    fn a_half_written_last_line_is_left_for_the_next_read() {
        let complete = br#"{"type":"user"}"#;
        let mut buffer = complete.to_vec();
        buffer.push(b'\n');
        buffer.extend_from_slice(br#"{"type":"assis"#);
        let (body, consumed) = whole_lines(&buffer, false);
        assert_eq!(body, &buffer[..complete.len() + 1]);
        assert_eq!(consumed, complete.len() + 1);
    }

    #[test]
    fn a_complete_last_line_with_no_newline_is_read() {
        let buffer = br#"{"type":"user"}"#;
        let (body, consumed) = whole_lines(buffer, true);
        assert_eq!(body, buffer.as_slice());
        assert_eq!(consumed, buffer.len());
        let (body, consumed) = whole_lines(buffer, false);
        assert!(body.is_empty());
        assert_eq!(consumed, 0);
    }

    #[test]
    fn a_long_summary_is_cut_at_a_row_length() {
        let value = serde_json::json!({"command": "x".repeat(400)});
        let summary = summary_of(&value);
        assert_eq!(summary.chars().count(), 121);
        assert!(summary.ends_with('…'));
        assert_eq!(clip("one\ntwo"), "one");
    }
}
