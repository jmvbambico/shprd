//! Asks: the questions, permissions and plans an agent waits on.
//!
//! An ask is one file in a per-task queue under the run's state directory
//! (`<repo>/.shprd/asks/<task>/<id>.json`), next to the event log. A raised ask
//! is written atomically; its answer is a second file written exactly once, so
//! two cards answering the same ask cannot both win. The event log records the
//! same facts ([`crate::event::EventKind::AskRaised`] and
//! [`crate::event::EventKind::AskAnswered`]), and the derived status reads the
//! fold, so a card and the board never disagree.
//!
//! The flow follows Shepherdr's ask hook (MIT, `service/asks.ts` and
//! `mods/chat/ask-hook.sh`, tag `v3.1.34`, commit `a3f327a`): the hook writes
//! the request and waits for an answer, and an unanswered request is never
//! decided by the hook itself. This file is shprd's own implementation; the
//! credit is the design, not copied code.
//!
//! Copyright (c) 2026 Elias Stravik — design reference, MIT.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::event::TaskId;

/// Identifies one ask inside a task.
pub type AskId = String;

/// What an agent is waiting for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AskKind {
    /// The agent asks a question and waits for an answer.
    Question,
    /// The agent asks permission to run a tool.
    Permission,
    /// The agent offers a plan and waits for approval.
    Plan,
}

impl AskKind {
    /// The word a board shows for this kind.
    pub fn word(self) -> &'static str {
        match self {
            Self::Question => "question",
            Self::Permission => "permission",
            Self::Plan => "plan",
        }
    }
}

/// One request an agent waits on.
///
/// The answer is not part of the file that is raised: [`AskQueue::answer`]
/// writes it in a separate file, and [`AskQueue::read`] joins the two. That is
/// what makes an ask answerable once, atomically, even when a card and a
/// terminal race for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ask {
    /// Its id, unique inside the task.
    pub id: AskId,
    /// The task it belongs to.
    pub task: TaskId,
    /// Question, permission or plan.
    pub kind: AskKind,
    /// What the person is asked, in the agent's or shprd's words.
    pub text: String,
    /// The choices a card offers, when the ask has any.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    /// When it was raised, ms since the Unix epoch.
    pub created_at_ms: u64,
    /// When it stops being open, ms since the Unix epoch, when it does.
    ///
    /// A hook that times out leaves the ask behind; after this instant the
    /// board stops showing it, because no answer can reach the waiting hook
    /// any more.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<u64>,
    /// The answer, once one was given: filled in when the two files are read
    /// together, never stored in the raised file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<AskAnswer>,
}

impl Ask {
    /// A raised ask with no answer and no expiry.
    pub fn new(
        id: impl Into<AskId>,
        task: impl Into<TaskId>,
        kind: AskKind,
        text: impl Into<String>,
        created_at_ms: u64,
    ) -> Self {
        Self {
            id: id.into(),
            task: task.into(),
            kind,
            text: text.into(),
            options: Vec::new(),
            created_at_ms,
            expires_at_ms: None,
            answer: None,
        }
    }

    /// The same ask, with the choices a card offers.
    pub fn with_options(mut self, options: Vec<String>) -> Self {
        self.options = options;
        self
    }

    /// The same ask, open until `expires_at_ms`.
    pub fn expires_at(mut self, expires_at_ms: u64) -> Self {
        self.expires_at_ms = Some(expires_at_ms);
        self
    }

    /// Whether the ask still waits for an answer at `now_ms`.
    pub fn is_open_at(&self, now_ms: u64) -> bool {
        self.answer.is_none() && self.expires_at_ms.is_none_or(|until| until > now_ms)
    }
}

/// The answer to one ask.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AskAnswer {
    /// What the person answered: `allow`/`deny`, `approve`/`reject`, or their
    /// own words for a question.
    pub text: String,
    /// When it was answered, ms since the Unix epoch.
    pub at_ms: u64,
}

/// A failure reading, raising or answering an ask.
#[derive(Debug, thiserror::Error)]
pub enum AskError {
    /// An ask's directory or file could not be read or written.
    #[error("ask file {path} failed: {source}")]
    Io {
        /// The file that failed.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// A file in the queue is not the JSON it should be.
    #[error("ask file {path} is not valid JSON: {source}")]
    Parse {
        /// The file that could not be read.
        path: PathBuf,
        /// The parse error.
        #[source]
        source: serde_json::Error,
    },
    /// An ask with that id is already open for the task.
    #[error("ask {ask:?} of task {task:?} is already open")]
    AlreadyRaised {
        /// The task.
        task: TaskId,
        /// The ask.
        ask: AskId,
    },
    /// No ask by that id exists for the task.
    #[error("task {task:?} has no ask {ask:?}")]
    UnknownAsk {
        /// The task.
        task: TaskId,
        /// The ask.
        ask: AskId,
    },
    /// The ask already has an answer; a second one is refused.
    #[error("ask {ask:?} of task {task:?} was already answered")]
    AlreadyAnswered {
        /// The task.
        task: TaskId,
        /// The ask.
        ask: AskId,
    },
    /// The ask timed out before it was answered.
    #[error("ask {ask:?} of task {task:?} expired at {expires_at_ms}")]
    Expired {
        /// The task.
        task: TaskId,
        /// The ask.
        ask: AskId,
        /// When it stopped being answerable.
        expires_at_ms: u64,
    },
    /// A task or ask id may not be used as a file name.
    #[error("{field} {value:?} is not usable as an ask file name")]
    InvalidName {
        /// Which name was bad.
        field: &'static str,
        /// The value, as given.
        value: String,
    },
}

/// The file queue of asks for one run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskQueue {
    root: PathBuf,
}

impl AskQueue {
    /// The queue under a queue root.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The queue beside a repository's event log: `<repo>/.shprd/asks`.
    pub fn for_repo(repo: &Path) -> Self {
        Self::new(repo.join(".shprd").join("asks"))
    }

    /// The directory every ask lives under.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where one ask's raised file lives.
    pub fn ask_path(&self, task: &str, ask: &str) -> Result<PathBuf, AskError> {
        let task = check_name("task", task)?;
        let ask = check_name("ask", ask)?;
        Ok(self.root.join(task).join(format!("{ask}.json")))
    }

    /// Where one ask's answer file lives.
    pub fn answer_path(&self, task: &str, ask: &str) -> Result<PathBuf, AskError> {
        let task = check_name("task", task)?;
        let ask = check_name("ask", ask)?;
        Ok(self.root.join(task).join(format!("{ask}.answer.json")))
    }

    /// Raises `ask`, writing it atomically and refusing a duplicate id.
    pub fn raise(&self, ask: &Ask) -> Result<(), AskError> {
        let path = self.ask_path(&ask.task, &ask.id)?;
        let mut on_disk = ask.clone();
        on_disk.answer = None;
        let bytes = serde_json::to_vec_pretty(&on_disk).map_err(|source| AskError::Parse {
            path: path.clone(),
            source,
        })?;
        // Create-new, so two hooks raising the same id cannot both win: the
        // loser is told the ask is already raised instead of its write
        // replacing the winner's file (which a plain rename would do).
        if !write_new(&path, &bytes)? {
            return Err(AskError::AlreadyRaised {
                task: ask.task.clone(),
                ask: ask.id.clone(),
            });
        }
        Ok(())
    }

    /// Answers one ask, exactly once.
    ///
    /// The answer is written into place with an atomic create-new link, so a
    /// second answer — from another card, another request, or a retry — is
    /// refused rather than overwriting the first.
    pub fn answer(&self, task: &str, ask: &str, text: &str, now_ms: u64) -> Result<Ask, AskError> {
        let mut raised = self.read(task, ask)?;
        if let Some(until) = raised.expires_at_ms
            && until <= now_ms
        {
            return Err(AskError::Expired {
                task: task.to_string(),
                ask: ask.to_string(),
                expires_at_ms: until,
            });
        }
        let path = self.answer_path(task, ask)?;
        let answer = AskAnswer {
            text: text.to_string(),
            at_ms: now_ms,
        };
        let bytes = serde_json::to_vec_pretty(&answer).map_err(|source| AskError::Parse {
            path: path.clone(),
            source,
        })?;
        if !write_new(&path, &bytes)? {
            return Err(AskError::AlreadyAnswered {
                task: task.to_string(),
                ask: ask.to_string(),
            });
        }
        raised.answer = Some(answer);
        Ok(raised)
    }

    /// Reads one ask and joins its answer, if it has one.
    pub fn read(&self, task: &str, ask: &str) -> Result<Ask, AskError> {
        let path = self.ask_path(task, ask)?;
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                return Err(AskError::UnknownAsk {
                    task: task.to_string(),
                    ask: ask.to_string(),
                });
            }
            Err(source) => {
                return Err(AskError::Io {
                    path: path.clone(),
                    source,
                });
            }
        };
        let mut raised: Ask = serde_json::from_str(&text).map_err(|source| AskError::Parse {
            path: path.clone(),
            source,
        })?;
        raised.answer = self.answer_of(task, ask)?;
        Ok(raised)
    }

    /// Reads one ask's answer, or `None` while it has none.
    pub fn answer_of(&self, task: &str, ask: &str) -> Result<Option<AskAnswer>, AskError> {
        let path = self.answer_path(task, ask)?;
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(AskError::Io {
                    path: path.clone(),
                    source,
                });
            }
        };
        let answer = serde_json::from_str(&text).map_err(|source| AskError::Parse {
            path: path.clone(),
            source,
        })?;
        Ok(Some(answer))
    }

    /// Every ask still open at `now_ms`, oldest first.
    pub fn open(&self, now_ms: u64) -> Result<Vec<Ask>, AskError> {
        let mut open = Vec::new();
        let tasks = match fs::read_dir(&self.root) {
            Ok(tasks) => tasks,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(open),
            Err(source) => {
                return Err(AskError::Io {
                    path: self.root.clone(),
                    source,
                });
            }
        };
        for entry in tasks {
            let entry = entry.map_err(|source| AskError::Io {
                path: self.root.clone(),
                source,
            })?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(task) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            for file in fs::read_dir(&path).map_err(|source| AskError::Io {
                path: path.clone(),
                source,
            })? {
                let file = file.map_err(|source| AskError::Io {
                    path: path.clone(),
                    source,
                })?;
                let name = file.file_name();
                let Some(name) = name.to_str() else { continue };
                // Answers are `<id>.answer.json`; only the raised files are
                // asks. A `.tmp` from a write in flight is not one either.
                let Some(id) = name.strip_suffix(".json") else {
                    continue;
                };
                if id.ends_with(".answer") || name.starts_with('.') {
                    continue;
                }
                let ask = self.read(task, id)?;
                if ask.is_open_at(now_ms) {
                    open.push(ask);
                }
            }
        }
        open.sort_by(|left, right| {
            left.created_at_ms
                .cmp(&right.created_at_ms)
                .then_with(|| left.task.cmp(&right.task))
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(open)
    }

    /// Every ask still open for one task at `now_ms`, oldest first.
    pub fn open_for(&self, task: &str, now_ms: u64) -> Result<Vec<Ask>, AskError> {
        Ok(self
            .open(now_ms)?
            .into_iter()
            .filter(|ask| ask.task == task)
            .collect())
    }

    /// Removes one ask's raised and answer files, if they exist.
    ///
    /// The ledger is the durable record; this only tidies the queue after an
    /// ask is settled or a caller decides to abandon it.
    pub fn remove(&self, task: &str, ask: &str) -> Result<(), AskError> {
        for path in [self.ask_path(task, ask)?, self.answer_path(task, ask)?] {
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(source) => return Err(AskError::Io { path, source }),
            }
        }
        Ok(())
    }
}

/// Refuses a task or ask id that could escape its directory.
fn check_name<'a>(field: &'static str, value: &'a str) -> Result<&'a str, AskError> {
    let bad = value.is_empty()
        || value == "."
        || value == ".."
        || value.starts_with('.')
        || value.contains('/')
        || value.contains('\\');
    if bad {
        return Err(AskError::InvalidName {
            field,
            value: value.to_string(),
        });
    }
    Ok(value)
}

/// Writes `bytes` to `path` only if it does not exist, atomically.
///
/// The temporary file is complete and fsynced before a hard link makes it
/// visible under the final name. A link fails when the name is taken, which is
/// exactly the "raised once, answered once" rule: no reader ever sees a
/// half-written file, and a second writer is told the name is already taken.
/// Returns `Ok(false)` when the name is taken, so each caller can name the
/// refusal it means (an ask already raised, or already answered).
fn write_new(path: &Path, bytes: &[u8]) -> Result<bool, AskError> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir).map_err(|source| AskError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    let temporary = temporary_path(path);
    let wrote = (|| -> io::Result<()> {
        let mut file = fs::File::create(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::hard_link(&temporary, path)
    })();
    let _ = fs::remove_file(&temporary);
    match wrote {
        Ok(()) => Ok(true),
        Err(source) if source.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(source) => Err(AskError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// A unique sibling path for a write in flight.
fn temporary_path(path: &Path) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("ask");
    path.with_file_name(format!(".{name}.{}.{n}.tmp", std::process::id()))
}

/// A count of asks by kind, for a board's badges.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AskCounts {
    /// Questions waiting.
    pub questions: usize,
    /// Permissions waiting.
    pub permissions: usize,
    /// Plans waiting.
    pub plans: usize,
}

impl AskCounts {
    /// Counts `asks` by kind.
    pub fn of(asks: &[Ask]) -> Self {
        let mut counts = Self::default();
        for ask in asks {
            match ask.kind {
                AskKind::Question => counts.questions += 1,
                AskKind::Permission => counts.permissions += 1,
                AskKind::Plan => counts.plans += 1,
            }
        }
        counts
    }

    /// The total number of asks.
    pub fn total(&self) -> usize {
        self.questions + self.permissions + self.plans
    }
}

/// Groups asks by task, oldest first inside each task.
pub fn by_task(asks: &[Ask]) -> BTreeMap<TaskId, Vec<Ask>> {
    let mut grouped: BTreeMap<TaskId, Vec<Ask>> = BTreeMap::new();
    for ask in asks {
        grouped
            .entry(ask.task.clone())
            .or_default()
            .push(ask.clone());
    }
    grouped
}

/// What every ask id raised by `shprd agent task widen` begins with.
pub const SCOPE_WIDEN_ASK_PREFIX: &str = "scope-widen-";

/// The choice that leaves a task's scope as it is.
pub const KEEP_SCOPE_CHOICE: &str = "keep the scope";

/// The id of the `n`th widening card raised for a task.
pub fn scope_widen_ask(n: usize) -> String {
    format!("{SCOPE_WIDEN_ASK_PREFIX}{n}")
}

/// The choices a widening card offers: widen to `paths`, or keep the scope.
pub fn scope_widen_options(paths: &[String]) -> Vec<String> {
    vec![
        format!("{}{}", crate::gate::WIDEN_CHOICE, paths.join(", ")),
        KEEP_SCOPE_CHOICE.to_string(),
    ]
}

/// What a widening card says: the task, the paths and the reason given.
pub fn scope_widen_body(task: &str, paths: &[String], reason: &str) -> String {
    format!(
        "{task}: widen its scope to {}? {}",
        paths.join(", "),
        reason.trim()
    )
}

/// The paths `answer` widens a scope to, when `ask` is a card that offers a
/// widening (a refused hand-in's or a widening verb's) and the answer takes it.
///
/// `keep the scope`, any other answer and any other ask give `None`, so nothing
/// is written for them.
pub fn widening_of(ask: &str, answer: &str) -> Option<Vec<String>> {
    if !ask.starts_with(SCOPE_WIDEN_ASK_PREFIX)
        && !ask.starts_with(crate::gate::HAND_IN_REFUSED_ASK_PREFIX)
    {
        return None;
    }
    let paths: Vec<String> = answer
        .trim()
        .strip_prefix(crate::gate::WIDEN_CHOICE)?
        .split(',')
        .map(|path| path.trim().to_string())
        .filter(|path| !path.is_empty())
        .collect();
    (!paths.is_empty()).then_some(paths)
}

/// The key both scope cards of a task share: a refused hand-in's and a widening
/// verb's. One key is one question, so a client draws one card for it.
pub fn scope_key(task: &str) -> String {
    format!("scope:{task}")
}

/// The key of the scope question `ask` raises for `task`, when it is one.
pub fn scope_key_of(task: &str, ask: &str) -> Option<String> {
    (ask.starts_with(SCOPE_WIDEN_ASK_PREFIX)
        || ask.starts_with(crate::gate::HAND_IN_REFUSED_ASK_PREFIX))
    .then(|| scope_key(task))
}

/// The paths a scope card offers to widen to: those of every `widen` choice,
/// normalised the way a scope is, without repeats.
pub fn offered_paths(options: &[String]) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    for path in options
        .iter()
        .filter_map(|option| widening_of(SCOPE_WIDEN_ASK_PREFIX, option))
        .flatten()
    {
        let path = crate::scope::normalize(&path).unwrap_or(path);
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    paths
}

/// Whether a scope card offering `options` is moot because every path it offers
/// is already in `scope`. A card that offers no paths is never moot.
pub fn offers_only_scoped_paths(options: &[String], scope: &[String]) -> bool {
    let paths = offered_paths(options);
    !paths.is_empty() && paths.iter().all(|path| crate::scope::within(path, scope))
}

/// Whether the open ask `ask` of `task` is a scope card made moot by `scope`:
/// every client lists asks through this, so they agree on which cards to draw.
pub fn is_moot_scope_card(task: &str, ask: &str, options: &[String], scope: &[String]) -> bool {
    scope_key_of(task, ask).is_some() && offers_only_scoped_paths(options, scope)
}

/// What a worker is told when its scope was widened.
pub fn widened_note(paths: &[String]) -> String {
    format!(
        "Your scope was widened by the person: you may now also change {}. Carry on, and hand in \
         again if you were blocked by the scope.",
        paths.join(", ")
    )
}

/// One message waiting to be typed into a task's live worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerNote {
    /// The task whose worker is told.
    pub task: TaskId,
    /// Where the note's file lives.
    pub path: PathBuf,
    /// What is typed.
    pub text: String,
}

/// The messages queued for tasks' live workers: `<repo>/.shprd/worker-notes`.
///
/// A service writes a note when a decision changes what a running worker may
/// do; the next step that can reach a terminal types it and marks it `.sent`,
/// or `.skipped` when no worker was live (the next attempt's brief carries what
/// the note says). A note is never typed twice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerNotes {
    root: PathBuf,
}

impl WorkerNotes {
    /// The queue beside a repository's event log.
    pub fn for_repo(repo: &Path) -> Self {
        Self {
            root: repo.join(".shprd").join("worker-notes"),
        }
    }

    /// Queues `text` for `task`'s worker.
    pub fn queue(&self, task: &str, text: &str) -> Result<PathBuf, AskError> {
        let task_name = check_name("task", task)?;
        let dir = self.root.join(task_name);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_millis());
        loop {
            let n = NEXT_NOTE.fetch_add(1, Ordering::Relaxed);
            let path = dir.join(format!("{now}-{n}.note"));
            if write_new(&path, text.as_bytes())? {
                return Ok(path);
            }
        }
    }

    /// Every note still waiting, oldest first.
    pub fn pending(&self) -> Vec<WorkerNote> {
        let mut found = Vec::new();
        let Ok(tasks) = fs::read_dir(&self.root) else {
            return found;
        };
        for task in tasks.flatten() {
            let Some(name) = task.file_name().to_str().map(str::to_string) else {
                continue;
            };
            let Ok(files) = fs::read_dir(task.path()) else {
                continue;
            };
            for file in files.flatten() {
                let path = file.path();
                if path.extension().and_then(|ext| ext.to_str()) != Some("note") {
                    continue;
                }
                if let Ok(text) = fs::read_to_string(&path) {
                    found.push(WorkerNote {
                        task: name.clone(),
                        path,
                        text,
                    });
                }
            }
        }
        found.sort_by(|a, b| a.path.cmp(&b.path));
        found
    }

    /// Marks a note handled: `.sent` when it was typed, `.skipped` when it was not.
    pub fn settle(&self, note: &WorkerNote, sent: bool) {
        let _ = fs::rename(
            &note.path,
            note.path
                .with_extension(if sent { "sent" } else { "skipped" }),
        );
    }
}

/// Keeps two notes queued in the same millisecond apart.
static NEXT_NOTE: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::sync::{Arc, Barrier};
    use std::thread;

    use super::{Ask, AskError, AskKind, AskQueue, WorkerNotes, widening_of};

    fn queue() -> (tempfile::TempDir, AskQueue) {
        let dir = tempfile::tempdir().expect("temp dir");
        let queue = AskQueue::for_repo(dir.path());
        (dir, queue)
    }

    fn ask(id: &str) -> Ask {
        Ask::new(id, "T1", AskKind::Permission, "Run: cargo test", 1_000)
    }

    #[test]
    fn a_raised_ask_round_trips_without_a_temporary_file_left() {
        let (dir, queue) = queue();
        queue.raise(&ask("a1")).expect("raise");
        let path = queue.ask_path("T1", "a1").expect("path");
        let text = std::fs::read_to_string(&path).expect("read");
        let parsed: Ask = serde_json::from_str(&text).expect("parse");
        assert_eq!(parsed.text, "Run: cargo test");
        assert!(parsed.answer.is_none());
        let leftovers: Vec<String> = std::fs::read_dir(path.parent().expect("dir"))
            .expect("list")
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
            .filter(|name| name.contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        assert!(dir.path().join(".shprd/asks/T1/a1.json").is_file());
    }

    #[test]
    fn raising_the_same_id_twice_is_refused() {
        let (_dir, queue) = queue();
        queue.raise(&ask("a1")).expect("raise");
        match queue.raise(&ask("a1")) {
            Err(AskError::AlreadyRaised { .. }) => {}
            other => panic!("expected AlreadyRaised, got {other:?}"),
        }
    }

    #[test]
    fn a_second_raise_never_overwrites_the_first() {
        let (_dir, queue) = queue();
        queue
            .raise(&Ask::new("a1", "T1", AskKind::Permission, "first", 1_000))
            .expect("raise");
        // A plain temp + rename would replace the file; the create-new write
        // must leave the first raise's words in place.
        assert!(matches!(
            queue.raise(&Ask::new("a1", "T1", AskKind::Permission, "second", 1_001)),
            Err(AskError::AlreadyRaised { .. })
        ));
        let text =
            std::fs::read_to_string(queue.ask_path("T1", "a1").expect("path")).expect("read");
        assert!(
            text.contains("first"),
            "the loser overwrote the winner: {text}"
        );
        assert!(!text.contains("second"), "{text}");
    }

    #[test]
    fn an_ask_is_answered_only_once_even_from_two_threads() {
        let (_dir, queue) = queue();
        queue.raise(&ask("a1")).expect("raise");
        let queue = Arc::new(queue);
        let barrier = Arc::new(Barrier::new(2));
        let mut handles = Vec::new();
        for text in ["allow", "deny"] {
            let queue = Arc::clone(&queue);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                queue.answer("T1", "a1", text, 2_000)
            }));
        }
        let results: Vec<Result<Ask, AskError>> = handles
            .into_iter()
            .map(|handle| handle.join().expect("thread"))
            .collect();
        let wins = results.iter().filter(|result| result.is_ok()).count();
        let refused = results
            .iter()
            .filter(|result| matches!(result, Err(AskError::AlreadyAnswered { .. })))
            .count();
        assert_eq!(wins, 1, "{results:?}");
        assert_eq!(refused, 1, "{results:?}");
        let read = queue.read("T1", "a1").expect("read");
        assert_eq!(read.answer.expect("answered").at_ms, 2_000);
    }

    #[test]
    fn an_unknown_ask_cannot_be_answered() {
        let (_dir, queue) = queue();
        match queue.answer("T1", "missing", "allow", 2_000) {
            Err(AskError::UnknownAsk { .. }) => {}
            other => panic!("expected UnknownAsk, got {other:?}"),
        }
    }

    #[test]
    fn an_expired_ask_is_neither_open_nor_answerable() {
        let (_dir, queue) = queue();
        queue.raise(&ask("a1").expires_at(1_500)).expect("raise");
        assert!(queue.open(1_000).expect("open").len() == 1);
        assert!(queue.open(1_500).expect("open").is_empty());
        match queue.answer("T1", "a1", "allow", 1_500) {
            Err(AskError::Expired { .. }) => {}
            other => panic!("expected Expired, got {other:?}"),
        }
    }

    #[test]
    fn an_answered_ask_leaves_the_open_list() {
        let (_dir, queue) = queue();
        queue.raise(&ask("a1")).expect("raise");
        queue.raise(&ask("a2")).expect("raise");
        queue.answer("T1", "a1", "allow", 2_000).expect("answer");
        let open = queue.open(2_000).expect("open");
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, "a2");
    }

    #[test]
    fn a_task_id_cannot_escape_its_directory() {
        let (_dir, queue) = queue();
        let bad = Ask::new("a1", "../outside", AskKind::Question, "q", 1_000);
        match queue.raise(&bad) {
            Err(AskError::InvalidName { field, .. }) => assert_eq!(field, "task"),
            other => panic!("expected InvalidName, got {other:?}"),
        }
    }

    #[test]
    fn only_a_widen_answer_on_a_widening_card_widens() {
        assert_eq!(
            widening_of("scope-widen-1", "widen the scope to a/b.rs, c.rs"),
            Some(vec!["a/b.rs".to_string(), "c.rs".to_string()])
        );
        assert_eq!(
            widening_of("hand-in-refused-1-1", "widen the scope to x"),
            Some(vec!["x".to_string()])
        );
        assert_eq!(widening_of("scope-widen-1", "keep the scope"), None);
        assert_eq!(widening_of("ask-1", "widen the scope to x"), None);
    }

    #[test]
    fn a_queued_note_is_listed_once_and_settled_once() {
        let dir = tempfile::tempdir().expect("temp dir");
        let notes = WorkerNotes::for_repo(dir.path());
        notes.queue("T1", "hello").expect("queue");
        let pending = notes.pending();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].text, "hello");
        notes.settle(&pending[0], true);
        assert!(notes.pending().is_empty());
    }
}

#[cfg(test)]
mod scope_key_tests {
    use super::*;

    #[test]
    fn both_scope_asks_share_one_key_and_moot_cards_are_found() {
        assert_eq!(
            scope_key_of("T", "scope-widen-1").as_deref(),
            Some("scope:T")
        );
        assert_eq!(
            scope_key_of("T", "hand-in-refused-1-1").as_deref(),
            Some("scope:T")
        );
        assert_eq!(scope_key_of("T", "ask-1"), None);
        let options = scope_widen_options(&["a/b.rs".to_string()]);
        assert!(offers_only_scoped_paths(&options, &["a".to_string()]));
        assert!(!offers_only_scoped_paths(&options, &["c".to_string()]));
        assert!(!offers_only_scoped_paths(
            &["other".to_string()],
            &["a".to_string()]
        ));
        let scope = ["a".to_string()];
        assert!(is_moot_scope_card("T", "scope-widen-1", &options, &scope));
        assert!(!is_moot_scope_card("T", "ask-1", &options, &scope));
    }

    #[test]
    fn every_widen_choice_counts_and_paths_are_normalised() {
        let options = vec![
            format!("{}./a/b.rs/", crate::gate::WIDEN_CHOICE),
            format!("{}c/d.rs, a/b.rs", crate::gate::WIDEN_CHOICE),
        ];
        assert_eq!(offered_paths(&options), vec!["a/b.rs", "c/d.rs"]);
        // One path out of scope keeps the card alive.
        assert!(!offers_only_scoped_paths(&options, &["a".to_string()]));
        assert!(offers_only_scoped_paths(
            &options,
            &["a".to_string(), "c".to_string()]
        ));
    }
}
