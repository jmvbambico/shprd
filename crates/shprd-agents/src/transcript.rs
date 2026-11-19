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
//! two additions: for a chat, the session is often one the service did not start
//! and has no id for, so the placeholders and `*` are matched as wildcards when
//! looking; and `{data_home}` names the XDG data home a CLI writes under, which
//! is `<home>/.local/share` only while `$XDG_DATA_HOME` is unset (OpenCode and
//! kilo both resolve it). `{cwd_slug}` is the program's own name for a working
//! directory, because Claude Code and pi fold a path into a folder name
//! differently ([`cwd_slug`]).
//!
//! # Whose home
//!
//! A record is the CLI's own file, so it is looked for under the roots *that
//! process* had: the real `HOME` it inherits (shprd never rewrites it — `HOME`
//! is a protected variable, [`crate::env`]) and its XDG data home. That is
//! [`Roots`], and it is deliberately not the engine's home: a service whose
//! `SHPRD_HOME` is somewhere else still reads the transcripts the agents wrote,
//! because no CLI has ever written one under `SHPRD_HOME`.
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
//! starts over rather than seeking past its end. OpenCode keeps a session as a
//! row in a database (or, before 1.18, one file a message and a folder of parts
//! for each message), so its cursor is the index of the next message to deliver
//! and only the new parts are read.
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

use std::collections::BTreeMap;
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
    /// What shprd's service told the agent: a task's news, typed into the
    /// conversation as `<shprd-news>` lines and shown as plain news lines.
    News,
}

impl Role {
    /// The word a client shows, which is also its JSON spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::News => "news",
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
    /// OpenCode's database, where a session's messages and their parts are rows.
    Database { path: PathBuf, session: String },
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
            Source::Store { .. } | Source::Database { .. } => None,
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
        Self::locate_in(spec, &Roots::at(home), cwd)
    }

    /// [`Transcript::locate`], with the home *and* the data home the CLI ran
    /// with handed in.
    ///
    /// A transcript is the CLI's own file, so it is looked for under the roots
    /// that CLI had — the real `HOME` it inherits and its XDG data home — never
    /// under the engine's home, which no CLI writes a session into.
    pub fn locate_in(spec: &TranscriptSpec, roots: &Roots, cwd: &Path) -> Option<Transcript> {
        Self::locate_since(spec, roots, cwd, None)
    }

    /// [`Transcript::locate_in`] for an agent that started at `since_ms` (epoch
    /// milliseconds): only a session that began then or later is its own.
    ///
    /// A directory holds every conversation anyone had there, a person's own
    /// and an earlier agent's included, so "the newest in this directory" can be
    /// somebody else's. An agent knows when it started, and its session is the
    /// first one begun after that. `None` is the old rule: the newest session.
    pub fn locate_since(
        spec: &TranscriptSpec,
        roots: &Roots,
        cwd: &Path,
        since_ms: Option<u64>,
    ) -> Option<Transcript> {
        let format = spec.format?;
        let location = spec.location.as_deref()?;
        match format {
            TranscriptFormat::OpencodeStorage => {
                locate_store(&expand_path(location, roots)?, cwd, since_ms)
            }
            TranscriptFormat::OpencodeSqlite => {
                locate_database(&expand_path(location, roots)?, cwd, since_ms)
            }
            _ => locate_file(format, location, roots, cwd, since_ms),
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
            Source::Database { path, session } => self.read_database(path, session, from),
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

    /// OpenCode's database: the session's messages after `from`.
    ///
    /// The cursor is the same message index the store format uses (one
    /// OpenCode's two shapes hold the same rows), so a session that moved from a
    /// folder to the database keeps paging where it was.
    fn read_database(
        &self,
        path: &Path,
        session: &str,
        from: usize,
    ) -> Result<Page, TranscriptError> {
        let rows = database_messages(path, session)?;
        let total = rows.len();
        let mut page = Vec::new();
        let mut position = from.min(total);
        while position < total && page.len() < MAX_MESSAGES {
            if let Some(message) = database_message(&rows[position]) {
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
    let message = match format {
        TranscriptFormat::ClaudeJsonl => claude_message(&value),
        TranscriptFormat::CodexJsonl => codex_message(&value),
        TranscriptFormat::PiJsonl => pi_message(&value),
        // OpenCode's two shapes are read from files of their own, not lines.
        TranscriptFormat::OpencodeStorage | TranscriptFormat::OpencodeSqlite => None,
    }?;
    Some(news_of(message))
}

/// A user turn that is shprd's news, as news lines.
///
/// The service types `<shprd-news task="T7" event="needs-you">…</shprd-news>`
/// into the coordinator, and the program records it as something the person
/// said. It is not: the words inside the tags are shown, as a news message, and
/// the tags, and the wrapper a paste adds around them, are not.
fn news_of(message: Message) -> Message {
    if message.role != Role::User || !message.text.contains("<shprd-news") {
        return message;
    }
    let mut lines: Vec<String> = Vec::new();
    let mut rest = String::new();
    let mut remaining = message.text.as_str();
    while let Some(at) = remaining.find("<shprd-news") {
        rest.push_str(&remaining[..at]);
        let after = &remaining[at..];
        let Some(open_end) = after.find('>') else {
            rest.push_str(after);
            remaining = "";
            break;
        };
        let body = &after[open_end + 1..];
        let (inside, tail) = match body.find("</shprd-news>") {
            Some(end) => (&body[..end], &body[end + "</shprd-news>".len()..]),
            None => (body, ""),
        };
        for line in inside.lines() {
            let line = line.trim();
            if !line.is_empty() {
                lines.push(line.to_owned());
            }
        }
        remaining = tail;
    }
    rest.push_str(remaining);
    // What is left is a paste's wrapper tags and whitespace, or words the
    // person typed beside the news.
    let rest = unwrap_tag(&rest, "pasted_content");
    let rest = rest.trim();
    if !rest.is_empty() {
        lines.push(rest.to_owned());
    }
    if lines.is_empty() {
        return message;
    }
    Message {
        role: Role::News,
        text: lines.join("\n"),
        ..message
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
        // A user line is not always the person's: Claude Code writes a slash
        // command, a system reminder and a local command's output there too, so
        // the line has to be cleaned before it is shown as something said.
        "user" => (
            Role::User,
            claude_person(&texts(content, &["text"]))?,
            Vec::new(),
        ),
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
            // Codex records its own brief (the AGENTS.md text, the environment)
            // in user-role blocks; a person's turn is the one that is neither.
            let text = match role {
                Role::User => {
                    texts_where(content, &["input_text", "output_text", "text"], |text| {
                        !codex_injected(text)
                    })
                }
                Role::Assistant | Role::News => {
                    texts(content, &["input_text", "output_text", "text"])
                }
            };
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

// ---------------------------------------------------------------------------
// What the CLI wrote, and what the person did
// ---------------------------------------------------------------------------

/// Prefixes of the lines Claude Code writes into a user turn that a person
/// never typed.
///
/// Ported from Shepherdr's `INJECTED` (`core/chat-log.js:167`, MIT, `a3f327a`):
/// a slash command's wrapper, a system reminder, a background task's notice, a
/// local command's output and its caveat, and the "[Request interrupted" line.
/// Every real Claude session carries these, so a chat that showed them would
/// put the CLI's own bookkeeping in the person's mouth from the first open.
const CLAUDE_INJECTED: [&str; 9] = [
    "<local-command-",
    "<system-reminder",
    "<bash-",
    "<task-notification",
    "<user-memory-input",
    "<user-prompt-submit-hook",
    "<command-message",
    "Caveat: The messages below",
    "[Request interrupted",
];

/// Prefixes of the blocks Codex writes into a user turn for the program itself.
///
/// Ported from Shepherdr's `CODEX_INJECTED` (`core/chat-log.js:168`): the
/// repository's instructions, the environment, a shell command it ran, and the
/// markers of a turn or an agent it started.
const CODEX_INJECTED: [&str; 8] = [
    "# AGENTS.md instructions",
    "<environment_context",
    "<user_instructions",
    "<user_shell_command",
    "<turn_aborted",
    "<subagent_notification",
    "<skill",
    "<INSTRUCTIONS",
];

/// Whether a Claude Code user line is the CLI's own text rather than the
/// person's.
fn claude_injected(text: &str) -> bool {
    let text = text.trim_start();
    CLAUDE_INJECTED
        .iter()
        .any(|prefix| text.starts_with(prefix))
}

/// Whether one of Codex's user-role blocks is the program's own text.
fn codex_injected(text: &str) -> bool {
    let text = text.trim_start();
    CODEX_INJECTED.iter().any(|prefix| text.starts_with(prefix))
}

/// The person's words out of a Claude Code user line, or `None` when the line
/// holds none.
///
/// A slash command is the one wrapped line that *is* the person's: the CLI
/// records `/login` as `<command-name>`/`<command-args>`, so it comes back as
/// the command they typed. Everything else is filtered, and the wrappers Claude
/// puts around a person's own words — a system reminder in the middle, a
/// `<pasted_content>` around a paste — are removed rather than shown.
fn claude_person(text: &str) -> Option<String> {
    if let Some(name) = tagged(text, "command-name") {
        let args = tagged(text, "command-args").unwrap_or_default();
        let line = format!("{} {}", name.trim(), args.trim()).trim().to_owned();
        return (!line.is_empty()).then_some(line);
    }
    if claude_injected(text) {
        return None;
    }
    // A system reminder's *body* is the CLI's too, so it goes with its tags; a
    // paste's tags are the CLI's wrapper around the person's own words, so only
    // the tags go.
    let cleaned = unwrap_tag(&strip_tag(text, "system-reminder"), "pasted_content");
    let cleaned = cleaned.trim();
    (!cleaned.is_empty()).then(|| cleaned.to_owned())
}

/// The text between `<name>` and `</name>`, when both are on the line.
fn tagged(text: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close)? + start;
    Some(text[start..end].to_owned())
}

/// `text` with every `<name …>…</name>` block taken out.
///
/// The opening tag may carry attributes (`<pasted_content id="6fd4">`), which is
/// how the CLI wraps what a person pasted.
fn strip_tag(text: &str, name: &str) -> String {
    let open = format!("<{name}");
    let close = format!("</{name}");
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(&open) {
        out.push_str(&rest[..at]);
        let after = &rest[at..];
        let Some(gt) = after.find('>') else {
            out.push_str(after);
            return out;
        };
        let body = &after[gt + 1..];
        let Some(end) = body.find(&close) else {
            out.push_str(after);
            return out;
        };
        let tail = &body[end..];
        let Some(tail_gt) = tail.find('>') else {
            out.push_str(after);
            return out;
        };
        rest = &tail[tail_gt + 1..];
    }
    out.push_str(rest);
    out
}

/// `text` with the `<name …>` and `</name>` tags taken off and what they
/// wrapped kept, which is how the CLI's wrapper around a paste is removed
/// without removing the paste.
fn unwrap_tag(text: &str, name: &str) -> String {
    let open = format!("<{name}");
    let close = format!("</{name}>");
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix(close.as_str()) {
            rest = after;
            continue;
        }
        // A closing tag that carries attributes (`</pasted_content id="56ab">`)
        // or has its slash escaped (`<\/pasted_content …>`) is the same wrapper
        // as the pasted block shows it.
        let closing = format!("</{name}");
        let escaped = format!("<\\/{name}");
        if let Some(after) = rest
            .strip_prefix(closing.as_str())
            .or_else(|| rest.strip_prefix(escaped.as_str()))
        {
            match after.find('>') {
                Some(gt) => {
                    rest = &after[gt + 1..];
                    continue;
                }
                None => break,
            }
        }
        if let Some(after) = rest.strip_prefix(open.as_str()) {
            match after.find('>') {
                Some(gt) => {
                    rest = &after[gt + 1..];
                    continue;
                }
                None => break,
            }
        }
        // Copy up to the next '<', which is where a tag could start.
        let next = rest
            .char_indices()
            .skip(1)
            .find(|(_, ch)| *ch == '<')
            .map_or(rest.len(), |(at, _)| at);
        out.push_str(&rest[..next]);
        rest = &rest[next..];
    }
    out
}

/// The text of a message's content, which a program writes as a string or as a
/// list of typed blocks.
///
/// `kinds` names the block types this program uses for a person's or an agent's
/// words; every other block (thinking, a tool's result, an image) is not text a
/// chat row shows.
fn texts(content: &Value, kinds: &[&str]) -> String {
    texts_where(content, kinds, |_| true)
}

/// [`texts`], keeping only the blocks `keep` accepts.
fn texts_where(content: &Value, kinds: &[&str], keep: impl Fn(&str) -> bool) -> String {
    match content {
        Value::String(text) => {
            let text = text.trim();
            if keep(text) {
                text.to_owned()
            } else {
                String::new()
            }
        }
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
                .filter(|text| !text.is_empty() && keep(text))
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
    roots: &Roots,
    cwd: &Path,
    since_ms: Option<u64>,
) -> Option<Transcript> {
    let (root, components) = pattern_parts(location, roots, cwd, format)?;
    let mut found: Option<(SystemTime, PathBuf)> = None;
    // A second of slack: a clock the host and the file system read apart.
    let since = since_ms.map(|ms| {
        std::time::UNIX_EPOCH + std::time::Duration::from_millis(ms.saturating_sub(1_000))
    });
    walk(&root, &components, 0, &mut |path| {
        if format == TranscriptFormat::CodexJsonl && !codex_belongs(path, cwd) {
            return;
        }
        let Ok(meta) = std::fs::metadata(path) else {
            return;
        };
        let Ok(modified) = meta.modified() else {
            return;
        };
        match since {
            // The first session begun since the agent started is its own.
            Some(since) => {
                let began = meta.created().unwrap_or(modified);
                if began < since {
                    return;
                }
                if found.as_ref().is_none_or(|(kept, _)| began < *kept) {
                    found = Some((began, path.to_path_buf()));
                }
            }
            None => {
                if found.as_ref().is_none_or(|(kept, _)| modified >= *kept) {
                    found = Some((modified, path.to_path_buf()));
                }
            }
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
fn locate_store(root: &Path, cwd: &Path, since_ms: Option<u64>) -> Option<Transcript> {
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
            // older OpenCode writes the folder only into the message parts, and
            // an empty `directory` records nothing either.
            let directory = value.get("directory").and_then(Value::as_str);
            if directory.is_some_and(|directory| {
                !directory.is_empty() && !same_dir(Path::new(directory), cwd)
            }) {
                continue;
            }
            let updated = value
                .get("time")
                .and_then(|time| time.get("updated"))
                .and_then(number_ms)
                .unwrap_or(0);
            // An agent's own session began after it started: it is judged by
            // when it was created, never by when it was last touched.
            let created = value
                .get("time")
                .and_then(|time| time.get("created"))
                .and_then(number_ms)
                .unwrap_or(updated);
            if since_ms.is_some_and(|since| created < i64::try_from(since).unwrap_or(i64::MAX)) {
                continue;
            }
            candidates.push((
                if since_ms.is_some() { created } else { updated },
                id.to_owned(),
                path,
            ));
        }
    }
    candidates.sort_by_key(|candidate| candidate.0);
    // The newest session, or (for an agent that knows when it started) the
    // first one begun since.
    let (_, session, _) = if since_ms.is_some() {
        candidates.into_iter().next()?
    } else {
        candidates.pop()?
    };
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

/// One OpenCode part, folded into the message it belongs to.
///
/// The two shapes of OpenCode's store hold the same part objects — the folder
/// has one JSON file a part, the database a `data` column a part — so both read
/// them through this one function, and a program that moved its store cannot
/// start showing different words.
fn apply_part(value: &Value, texts: &mut Vec<String>, tools: &mut Vec<ToolCall>) {
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

/// One OpenCode message header and its parts, folded into a message.
///
/// The header is the object the folder store keeps in `message/<id>.json` and
/// the database keeps in `message.data`; the parts are the objects the folder
/// store keeps in `part/<id>/` and the database keeps in `part.data`.
fn fold_opencode(header: &Value, parts: &[Value]) -> Option<Message> {
    let role = match header.get("role").and_then(Value::as_str)? {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        _ => return None,
    };
    let at_ms = header
        .get("time")
        .and_then(|time| time.get("created"))
        .and_then(number_ms);
    let mut texts = Vec::new();
    let mut tools = Vec::new();
    for part in parts {
        apply_part(part, &mut texts, &mut tools);
    }
    let message = Message {
        role,
        text: texts.join("\n\n"),
        tools,
        at_ms,
    };
    (!message.is_empty()).then_some(message)
}

/// One OpenCode message, joined with the parts of which it is made.
fn store_message(root: &Path, id: &str, path: &Path) -> Option<Message> {
    let header = std::fs::read_to_string(path).ok()?;
    let header: Value = serde_json::from_str(&header).ok()?;
    let parts = root.join("part").join(id);
    let mut values = Vec::new();
    for part in std::fs::read_dir(&parts).ok()?.flatten() {
        let path = part.path();
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            values.push(value);
        }
    }
    fold_opencode(&header, &values)
}

/// The newest OpenCode session in `db` whose own `directory` is `cwd`.
///
/// OpenCode 1.18.35 writes the same `session`/`message`/`part` tables into a
/// SQLite file rather than a folder of JSON files, and its `session` row names
/// the directory the session belongs to. The database is opened read-only
/// through a `mode=ro` URI, exactly as the watchdog reads it: a chat must never
/// be the reason a live CLI's store is locked or migrated.
fn locate_database(db: &Path, cwd: &Path, since_ms: Option<u64>) -> Option<Transcript> {
    let connection = open_ro(db)?;
    let mut rows = session_rows(&connection, true).or_else(|| session_rows(&connection, false))?;
    let floor = since_ms.and_then(|since| i64::try_from(since).ok());
    // An agent's own session is the first one *created* since it started, not
    // whichever was touched last; with no start to go by it is the newest.
    if floor.is_some() {
        rows.sort_by_key(|(id, _, _, created)| (*created, id.clone()));
    }
    // The first row is the newest session that belongs here: the query is
    // newest-first, so the search stops at its answer.
    let session = rows
        .into_iter()
        .filter(|(_, _, _, created)| {
            floor.is_none_or(|floor| created.is_some_and(|created| created >= floor))
        })
        .find(|(_, directory, _, _)| {
            // A session that records no directory at all is not refused: an older
            // OpenCode wrote the folder only into the message parts, and a row
            // with an empty `directory` records nothing either.
            directory
                .as_deref()
                .is_none_or(|directory| directory.is_empty() || same_dir(Path::new(directory), cwd))
        })
        .map(|(id, _, _, _)| id)?;
    Some(Transcript {
        format: TranscriptFormat::OpencodeSqlite,
        source: Source::Database {
            path: db.to_path_buf(),
            session: session.clone(),
        },
        session,
    })
}

/// One `session` row: its id, directory, last update and (where the schema has
/// it) creation time.
type SessionRow = (String, Option<String>, Option<i64>, Option<i64>);

/// The `session` rows, newest-updated first. A schema with no `time_created`
/// column (an older OpenCode) is read with the creation time unknown.
fn session_rows(connection: &rusqlite::Connection, with_created: bool) -> Option<Vec<SessionRow>> {
    let sql = if with_created {
        "SELECT id, directory, time_updated, time_created FROM session \
         ORDER BY time_updated DESC, id DESC"
    } else {
        "SELECT id, directory, time_updated, NULL FROM session \
         ORDER BY time_updated DESC, id DESC"
    };
    let mut statement = connection.prepare(sql).ok()?;
    let rows = statement
        .query_map((), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, Option<i64>>(3)?,
            ))
        })
        .ok()?;
    Some(rows.flatten().collect())
}

/// One message row of OpenCode's database, with its parts, oldest first.
struct DatabaseMessage {
    /// The message's own id, which its parts name.
    id: String,
    /// When the message was created, in ms: the order the program wrote them.
    created: i64,
    /// The message header, as the folder store keeps it in a file.
    header: Value,
    /// The parts of which the message is made.
    parts: Vec<Value>,
}

/// Every message of `session`, oldest first, with its parts.
fn database_messages(db: &Path, session: &str) -> Result<Vec<DatabaseMessage>, TranscriptError> {
    let list = |path: &Path, source: rusqlite::Error| TranscriptError::List {
        path: path.to_path_buf(),
        source: std::io::Error::other(source),
    };
    let connection = open_ro(db).ok_or_else(|| TranscriptError::List {
        path: db.to_path_buf(),
        source: std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "the database could not be opened for reading",
        ),
    })?;
    // A database that predates the `part` table has no parts, not an error: its
    // messages still show, with the words the row itself carries.
    let mut parts: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    if let Ok(mut statement) = connection.prepare(
        "SELECT message_id, data FROM part WHERE session_id = ?1 \
         ORDER BY time_created, id",
    ) {
        let rows = statement
            .query_map((session,), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|error| list(db, error))?;
        for row in rows {
            let (message_id, data) = row.map_err(|error| list(db, error))?;
            if let Ok(value) = serde_json::from_str::<Value>(&data) {
                parts.entry(message_id).or_default().push(value);
            }
        }
    }
    let mut statement = connection
        .prepare(
            "SELECT id, time_created, data FROM message WHERE session_id = ?1 \
             ORDER BY time_created, id",
        )
        .map_err(|error| list(db, error))?;
    let rows = statement
        .query_map((session,), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|error| list(db, error))?;
    let mut messages = Vec::new();
    for row in rows {
        let (id, created, data) = row.map_err(|error| list(db, error))?;
        let Ok(header) = serde_json::from_str::<Value>(&data) else {
            continue;
        };
        let mine = parts.remove(&id).unwrap_or_default();
        messages.push(DatabaseMessage {
            id,
            created,
            header,
            parts: mine,
        });
    }
    // The id breaks a tie, exactly as the folder store's own ordering does, so
    // two messages created in the same millisecond keep the order the program
    // wrote them in.
    messages.sort_by(|left, right| (left.created, &left.id).cmp(&(right.created, &right.id)));
    Ok(messages)
}

/// One database message as the chat shows it.
fn database_message(row: &DatabaseMessage) -> Option<Message> {
    fold_opencode(&row.header, &row.parts)
}

/// Open a SQLite database for reading only.
///
/// The `mode=ro` URI is the whole point: a chat is a reader of a file another
/// process owns, and opening it any other way could lock a live CLI's store or
/// create one that was never there.
fn open_ro(path: &Path) -> Option<rusqlite::Connection> {
    use rusqlite::OpenFlags;
    let uri = format!("file:{}?mode=ro", path.display());
    rusqlite::Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .ok()
}

// ---------------------------------------------------------------------------
// The location pattern
// ---------------------------------------------------------------------------

/// The directories a `[transcript]` location may name, resolved by the caller.
///
/// A transcript is written by the agent CLI itself, so the roots are the ones
/// *that* process had: the home it inherits (which shprd never rewrites —
/// `HOME` is a protected variable, [`crate::env`]) and the XDG data home it
/// resolves (`$XDG_DATA_HOME`, else `<home>/.local/share`). Neither is the
/// engine's own home (`SHPRD_HOME`), which a CLI never writes a session under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Roots {
    /// The home directory the CLI ran with, which `~` names.
    pub home: PathBuf,
    /// The data home it wrote under, which `{data_home}` names.
    pub data_home: PathBuf,
}

impl Roots {
    /// The roots for `home`, with the XDG data rule applied: `data_home` wins
    /// when it is set and not empty, else `<home>/.local/share`.
    pub fn new(home: &Path, data_home: Option<&Path>) -> Roots {
        Roots {
            home: home.to_path_buf(),
            data_home: match data_home.filter(|dir| !dir.as_os_str().is_empty()) {
                Some(dir) => dir.to_path_buf(),
                None => home.join(".local").join("share"),
            },
        }
    }

    /// The roots for a machine with no `$XDG_DATA_HOME` of its own.
    pub fn at(home: &Path) -> Roots {
        Roots::new(home, None)
    }

    /// The roots of the environment an agent CLI is launched with.
    ///
    /// It is exactly what the CLI itself will resolve: its `HOME`
    /// ([`crate::env::LaunchEnv`] never rewrites it, so it is the inherited one)
    /// and its `$XDG_DATA_HOME` when the base environment carries one. A launch
    /// that rewrote `HOME` in a later milestone would then move the reader with
    /// it, which is what keeps this a fact about the CLI rather than about
    /// shprd: a client that has no such variable answers `None`, and the caller
    /// passes the home it resolved instead.
    pub fn for_launch(base: &std::collections::BTreeMap<String, String>) -> Roots {
        let home = base
            .get("HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        let data_home = base
            .get("XDG_DATA_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        match home {
            Some(home) => Roots::new(&home, data_home.as_deref()),
            // No `HOME` in the base environment at all is a launch nothing can
            // read a record from; `""` makes every lookup miss rather than
            // reaching some other directory.
            None => Roots {
                home: PathBuf::new(),
                data_home: data_home.unwrap_or_default(),
            },
        }
    }
}

/// A location pattern as a root and the components under it.
///
/// `~` is the home directory, `{data_home}` the data home and a leading `/` the
/// filesystem's root; a pattern with none of them names nothing this reader can
/// look under, which is a table mistake rather than a missing session.
/// `{cwd_slug}` becomes this format's name for `cwd`, and every other
/// placeholder stays `*`: a chat looks for a session it was never told the id
/// of.
fn pattern_parts(
    location: &str,
    roots: &Roots,
    cwd: &Path,
    format: TranscriptFormat,
) -> Option<(PathBuf, Vec<String>)> {
    let (root, body) = if let Some(rest) = location
        .strip_prefix("{data_home}/")
        .or_else(|| (location == "{data_home}").then_some(""))
    {
        (roots.data_home.clone(), rest)
    } else if let Some(rest) = location.strip_prefix("~/") {
        (roots.home.clone(), rest)
    } else {
        (PathBuf::from("/"), location.strip_prefix('/')?)
    };
    let slug = cwd_slug(format, cwd);
    let components = body
        .split('/')
        .filter(|raw| !raw.is_empty())
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

/// A location with no wildcards as a path, for a store or a database whose file
/// is named outright.
fn expand_path(location: &str, roots: &Roots) -> Option<PathBuf> {
    if let Some(rest) = location.strip_prefix("{data_home}/") {
        return Some(roots.data_home.join(rest));
    }
    if let Some(rest) = location.strip_prefix("~/") {
        return Some(roots.home.join(rest));
    }
    location.starts_with('/').then(|| PathBuf::from(location))
}

/// The name `format`'s program gives `cwd` as one folder.
///
/// Each program has its own folding rule, and a chat is only found when the
/// rule is the program's rather than an approximation of it:
///
/// * Claude Code folds *every* character that is not an ASCII letter or digit
///   into `-`, keeping the leading separator (`/work/my_repo.v2` becomes
///   `-work-my-repo-v2`). A folded name longer than 200 characters is cut
///   there and suffixed with `-<base36 hash of the whole path>`, so two deep
///   paths that share a prefix do not collide.
/// * pi drops the leading separator, folds `/`, `\` and `:` into `-` and wraps
///   the result in `--…--` (`/work/my_repo.v2` becomes
///   `--work-my_repo.v2--`: an underscore or a dot stays as it is).
///
/// Both rules are the programs' own: Claude Code's is `bD`/`k`/`Pe` in
/// `/opt/homebrew/Caskroom/claude-code/2.1.311/claude` and pi's is
/// `getDefaultSessionDirPath` in
/// `@earendil-works/pi-coding-agent/dist/core/session-manager.js` (1.4.0), and
/// the shapes they produce were checked against the folders both wrote on this
/// machine.
pub fn cwd_slug(format: TranscriptFormat, cwd: &Path) -> String {
    // A record names the directory the program saw, which on macOS is the
    // resolved `/private/var/…` form, while the service may hold the symlinked
    // one (`/tmp/…`). The slug is built from the resolved path, so both spell
    // the same folder.
    let resolved = cwd.canonicalize();
    let cwd: &Path = resolved.as_deref().unwrap_or(cwd);
    let text = cwd.to_string_lossy();
    match format {
        TranscriptFormat::ClaudeJsonl => claude_slug(&text),
        TranscriptFormat::PiJsonl => format!(
            "--{}--",
            text.trim_start_matches(['/', '\\'])
                .replace(['/', '\\', ':'], "-")
        ),
        // Codex's date tree, OpenCode's store and its database name no
        // directory; this is only ever fed to `{cwd_slug}` in a pattern, which
        // none of them use.
        _ => text.replace(['/', '\\'], "-"),
    }
}

/// The length Claude Code folds a project name to before it hashes the path.
///
/// A shorter name is the fold alone; a longer one is the first this many
/// characters of the fold, a `-`, and six base-36 digits of a hash of the whole
/// path (see [`cwd_slug`]).
const CLAUDE_SLUG_MAX: usize = 200;

/// Claude Code's project folder name for a directory.
///
/// The fold is `replace(/[^a-zA-Z0-9]/g, "-")` over the path, which is why an
/// underscore and a dot both become `-` (the gate's real session lived in
/// `…-xm09k80000gn-T-hp-…`, not `…-xm09k80000gn_T_hp-…`).
fn claude_slug(path: &str) -> String {
    let folded: String = path
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect();
    // JavaScript counts UTF-16 code units, not `char`s: the cut can differ for a
    // path holding an astral character. A path that long is not a path a person
    // has, and the hash keeps the name unique either way.
    if folded.chars().count() <= CLAUDE_SLUG_MAX {
        return folded;
    }
    let head: String = folded.chars().take(CLAUDE_SLUG_MAX).collect();
    format!("{head}-{}", base36_hash(path))
}

/// The hash Claude Code puts on a folded name that was too long: its `Ome`
/// function (`e = (e << 5) - e + c | 0`, i.e. a Java-style string hash) over the
/// whole path, as the absolute value in base 36.
fn base36_hash(path: &str) -> String {
    let mut hash: i32 = 0;
    for unit in path.encode_utf16() {
        hash = hash
            .wrapping_shl(5)
            .wrapping_sub(hash)
            .wrapping_add(i32::from(unit));
    }
    // The program takes the absolute value of the signed 32-bit result, which is
    // why `i32::MIN` is widened before it is negated.
    let magnitude = i64::from(hash).unsigned_abs();
    let mut digits = String::new();
    let mut rest = magnitude;
    loop {
        let digit = (rest % 36) as u32;
        digits.push(std::char::from_digit(digit, 36).unwrap_or('0'));
        rest /= 36;
        if rest == 0 {
            break;
        }
    }
    digits.chars().rev().collect()
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
        CLAUDE_SLUG_MAX, Role, TranscriptFormat, claude_person, clip, cwd_slug, matches_name,
        parse_line, summary_of, whole_lines,
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

    /// The gate bug this fix is about: Claude Code folds `_` and every other
    /// non-alphanumeric character too, so a transcript under
    /// `…/hp-501/210041/SHP-60/repo2` is in `…-hp-501-…`, whose parent has an
    /// underscore in it (`xm09k80000gn_T_hp`). These are real folder names from
    /// this machine's `~/.claude/projects`, paired with the `cwd` their own
    /// session records.
    #[test]
    fn claude_folds_everything_that_is_not_alphanumeric() {
        for (cwd, folder) in [
            (
                "/private/var/folders/kl/8t4rp31969n3ts1h9_xm09k80000gn/T/hp-501/210041/SHP-60/repo2",
                "-private-var-folders-kl-8t4rp31969n3ts1h9-xm09k80000gn-T-hp-501-210041-SHP-60-repo2",
            ),
            (
                "/private/var/folders/kl/8t4rp31969n3ts1h9_xm09k80000gn/T/hp-501/210041/SHP-60/repo",
                "-private-var-folders-kl-8t4rp31969n3ts1h9-xm09k80000gn-T-hp-501-210041-SHP-60-repo",
            ),
            (
                "/private/var/folders/kl/8t4rp31969n3ts1h9_xm09k80000gn/T/hp-501/210041/SHP-26/claudemode.SqCv",
                "-private-var-folders-kl-8t4rp31969n3ts1h9-xm09k80000gn-T-hp-501-210041-SHP-26-claudemode-SqCv",
            ),
            (
                "/private/var/folders/kl/8t4rp31969n3ts1h9_xm09k80000gn/T/hp-501/210041/SHP-60/home/.shprd/reviews/m10h/zeta-a1",
                "-private-var-folders-kl-8t4rp31969n3ts1h9-xm09k80000gn-T-hp-501-210041-SHP-60-home--shprd-reviews-m10h-zeta-a1",
            ),
        ] {
            assert_eq!(
                cwd_slug(TranscriptFormat::ClaudeJsonl, Path::new(cwd)),
                folder,
                "{cwd}"
            );
        }
    }

    /// The space a path has in it is folded like any other character, and a
    /// path with no such characters folds to itself.
    #[test]
    fn claude_folds_spaces_and_keeps_alphanumerics() {
        assert_eq!(
            cwd_slug(TranscriptFormat::ClaudeJsonl, Path::new("/work/my repo")),
            "-work-my-repo"
        );
        assert_eq!(
            cwd_slug(TranscriptFormat::ClaudeJsonl, Path::new("/work/v2.1_repo")),
            "-work-v2-1-repo"
        );
    }

    /// A folded name Claude Code would cut gets the cut and the hash, exactly as
    /// the CLI's own `bD` builds it (checked against the CLI's functions with
    /// the same two paths).
    #[test]
    fn a_very_long_claude_path_is_cut_and_hashed() {
        let long = format!("/Users/me/projects/{}", "a".repeat(200));
        let slug = cwd_slug(TranscriptFormat::ClaudeJsonl, Path::new(&long));
        assert_eq!(slug.len(), CLAUDE_SLUG_MAX + 1 + 6, "{slug}");
        assert!(slug.starts_with("-Users-me-projects-aaa"), "{slug}");
        assert!(slug.ends_with("-6ihb7w"), "{slug}");
        // Two paths that agree over the cut but differ after it are two folders,
        // which is the whole reason for the hash.
        let other = format!("{long}b");
        assert_ne!(
            cwd_slug(TranscriptFormat::ClaudeJsonl, Path::new(&other)),
            slug
        );
        // A name exactly at the limit is not cut at all.
        let at_limit = format!("/{}", "b".repeat(CLAUDE_SLUG_MAX - 1));
        assert_eq!(at_limit.chars().count(), CLAUDE_SLUG_MAX);
        let slug = cwd_slug(TranscriptFormat::ClaudeJsonl, Path::new(&at_limit));
        assert_eq!(slug, at_limit.replace('/', "-"));
    }

    /// pi leaves an underscore and a dot alone: its rule is separators only.
    #[test]
    fn pi_keeps_underscores_and_dots_and_folds_only_separators() {
        assert_eq!(
            cwd_slug(TranscriptFormat::PiJsonl, Path::new("/work/my_repo.v2/sub")),
            "--work-my_repo.v2-sub--"
        );
        // A Windows-shaped path, which pi folds the same way.
        assert_eq!(
            cwd_slug(TranscriptFormat::PiJsonl, Path::new("C:\\work\\repo")),
            "--C--work-repo--"
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
    fn shprd_news_is_a_news_message_not_raw_tags() {
        // SHPR-65 bug 5: the service types its news into the coordinator, and a
        // paste's wrapper tags (one with an attribute, one with its slash
        // escaped) came out around it as text.
        let pasted = "<pasted_content id=\"56ab\">\n\
            <shprd-news task=\"T7\" event=\"needs-you\">T7 needs you: Should pow handle negative exponents? (Add pow)</shprd-news>\n\
            <shprd-news task=\"T10\" event=\"landed\">T10 is done (Create docs/b.md)</shprd-news>\n\
            <\\/pasted_content id=\"56ab\">";
        let value = serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": pasted}
        });
        let message = parse_line(TranscriptFormat::ClaudeJsonl, &line(&value)).unwrap();
        assert_eq!(message.role, Role::News);
        assert_eq!(
            message.text,
            "T7 needs you: Should pow handle negative exponents? (Add pow)\nT10 is done (Create docs/b.md)"
        );
        assert!(!message.text.contains('<'), "{}", message.text);
        // A person's own words are still theirs.
        let said = serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": "start T3 please"}
        });
        let message = parse_line(TranscriptFormat::ClaudeJsonl, &line(&said)).unwrap();
        assert_eq!(message.role, Role::User);
    }

    #[test]
    fn a_pastes_closing_tag_with_an_attribute_or_an_escaped_slash_is_removed() {
        for close in ["</pasted_content id=\"9\">", "<\\/pasted_content id=\"9\">"] {
            let text = format!("<pasted_content id=\"9\">hello{close}");
            let value =
                serde_json::json!({"type": "user", "message": {"role": "user", "content": text}});
            let message = parse_line(TranscriptFormat::ClaudeJsonl, &line(&value)).unwrap();
            assert_eq!(message.text, "hello", "{close}");
        }
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
    fn the_clis_own_lines_are_not_the_persons_words() {
        // A local command's caveat, its slash command and its output: real
        // lines from a session on this machine.
        let caveat = "<local-command-caveat>Caveat: The messages below were generated by the user while running local commands.</local-command-caveat>";
        assert!(claude_person(caveat).is_none());
        let output = "<local-command-stdout>Login successful</local-command-stdout>";
        assert!(claude_person(output).is_none());
        assert!(claude_person("[Request interrupted by user]").is_none());
        // A slash command is the person's: the CLI's three tags become the
        // command they typed.
        let command = "<command-name>/login</command-name>\n            <command-message>login</command-message>\n            <command-args></command-args>";
        assert_eq!(claude_person(command).as_deref(), Some("/login"));
        let with_args = "<command-name>/model</command-name><command-args>opus</command-args>";
        assert_eq!(claude_person(with_args).as_deref(), Some("/model opus"));
    }

    #[test]
    fn a_wrapper_around_the_persons_words_is_removed_and_their_words_kept() {
        // A paste the CLI wrapped: the tags are the CLI's, the text is theirs.
        let pasted = "<pasted_content id=\"6fd4\">\nRead the run log.\n</pasted_content>";
        assert_eq!(claude_person(pasted).as_deref(), Some("Read the run log."));
        // A system reminder inside a real message: the reminder goes, the
        // question stays.
        let reminded = "What changed?\n<system-reminder>You are low on context.</system-reminder>";
        assert_eq!(claude_person(reminded).as_deref(), Some("What changed?"));
        // Nothing but a reminder is no message at all.
        assert!(claude_person("<system-reminder>Context left: 1%</system-reminder>").is_none());
    }

    #[test]
    fn codexs_own_user_blocks_are_skipped_but_a_mixed_turn_keeps_the_words() {
        let brief =
            "# AGENTS.md instructions for /work/repo\n\n<INSTRUCTIONS>Read it.</INSTRUCTIONS>";
        assert!(super::codex_injected(brief));
        assert!(super::codex_injected(
            "<environment_context>\n  <cwd>/work/repo</cwd>\n</environment_context>"
        ));
        assert!(!super::codex_injected("Review the diff on branch work/12."));
        let mixed = serde_json::json!([
            {"type": "input_text", "text": "<environment_context><cwd>/work/repo</cwd></environment_context>"},
            {"type": "input_text", "text": "Review the diff."}
        ]);
        assert_eq!(
            super::texts_where(&mixed, &["input_text"], |text| !super::codex_injected(text)),
            "Review the diff."
        );
    }

    #[test]
    fn a_codex_turn_of_only_injected_blocks_is_no_message() {
        let value = serde_json::json!({
            "type": "response_item",
            "timestamp": "2026-11-03T00:16:03.000Z",
            "payload": {"type": "message", "role": "user", "content": [
                {"type": "input_text", "text": "<environment_context><cwd>/work/repo</cwd></environment_context>"},
                {"type": "input_text", "text": "# AGENTS.md instructions for /work/repo"}
            ]}
        });
        assert!(parse_line(TranscriptFormat::CodexJsonl, &line(&value)).is_none());
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
