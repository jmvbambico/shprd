//! The chat: one space's agent conversation, and a turn typed back into it.
//!
//! A space is the coordinator's or a task's ([`Space`]). Reading one is the
//! service's own work: it finds the agent's program from the ledger (a task's
//! latest attempt names it) or from the settings (the coordinator role does),
//! asks that program's harness where the session lives
//! ([`shprd_agents::transcript::Transcript::locate_in`]) and answers what the
//! record gained after the client's cursor. Nothing is written: a transcript is
//! read-only evidence, and the agent's own file is the only copy of it.
//!
//! The roots a record is looked for under are the ones the *agent CLI* ran with
//! ([`shprd_agents::transcript::Roots`]: its real `HOME` and its XDG data home),
//! which the binary resolves once and passes in. They are deliberately not the
//! engine's own home: `SHPRD_HOME` is where shprd keeps its settings, briefs and
//! worktrees, and no CLI has ever written a session there.
//!
//! Sending is the one thing the service cannot do alone: typing into a pane is
//! the host's verb, so the binary passes a [`ChatWriter`] here exactly as it
//! passes the spawn and review paths to the tick. The writer types the person's
//! words **as the person** — one turn of input into the pane where that space's
//! agent runs — and never as shprd: nothing is recorded in the ledger as the
//! engine, because a chat line is not an engine decision.
//!
//! The rules for what a program's record holds are the reader's
//! ([`shprd_agents::transcript`]); this module only chooses *which* session and
//! shapes the answer.

use std::path::PathBuf;

use shprd_agents::spec::{HarnessSpec, TranscriptFormat};
use shprd_agents::transcript::{PendingQuestion, Transcript};
use shprd_core::settings::{Loaded, RoleKind};
use shprd_core::state::{RunState, Task};

use crate::api::AppState;
use crate::attachments;
use crate::dto::{
    ChatAnswerDto, ChatAnswerReply, ChatDto, ChatMessageDto, ChatOptionDto, ChatQuestionDto,
    ChatSendDto, ChatSendReply, ChatStopDto, ChatStopReply, ChatToolDto,
};
use crate::error::ApiReject;
use crate::ledger;
use crate::terminal::TermKey;

/// The word a client uses for the coordinator's space.
pub const COORDINATOR: &str = "coordinator";

/// The built-in role a settings file names when nothing else is said: the
/// coordinator's identity, which is a role of kind [`RoleKind::Coordinator`]
/// (a custom role may be one too, and then it is the one the settings order
/// puts first).
const COORDINATOR_ID: &str = "coordinator";

/// One thing pressed or typed to answer a dialog in a terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnswerStep {
    /// One key press.
    Key(TermKey),
    /// Words typed at the cursor, without a newline.
    Text(String),
}

/// What the service needs to type a chat line into a space.
///
/// It is a trait rather than the [`shprd_mux::Host`] itself for the reason the
/// tick's facts are: a test sends a line into a recorder and needs no terminal,
/// while the binary wires the host it already drives. `space` is `None` for the
/// coordinator and a task's id for a task's space.
///
/// [`shprd_mux::Host`]: https://docs.rs/shprd-mux
pub trait ChatWriter: Send + Sync {
    /// Type `text` into `space`'s agent as one turn of the person's input.
    ///
    /// `Err` is a host that could not be written to, or a space with no pane
    /// open; the caller tells the person and nothing is retried, because
    /// retyping a message the agent may have read already is worse than not.
    fn send(&self, space: Option<&str>, text: &str) -> Result<(), String>;

    /// Press Escape in `space`'s agent, which every supported program takes as
    /// "stop what you are doing". A host that cannot press a key says so.
    fn interrupt(&self, _space: Option<&str>) -> Result<(), String> {
        Err(String::from(
            "this service cannot press a key in a terminal",
        ))
    }

    /// Press `steps` in `space`'s agent, one after the other, to answer the
    /// dialog its terminal shows. A host that cannot press keys says so.
    fn answer(&self, _space: Option<&str>, _steps: &[AnswerStep]) -> Result<(), String> {
        Err(String::from(
            "this service cannot press a key in a terminal",
        ))
    }

    /// When the agent `send` types into for `space` started, in epoch
    /// milliseconds, when the host says. The chat shows the conversation that
    /// agent began, not the newest one in its folder (a person's own, or an
    /// earlier coordinator's, can be newer). `None` is not knowing.
    fn started_ms(&self, _space: Option<&str>) -> Option<u64> {
        None
    }

    /// Whether shprd's own coordinator is running now, when the host can say:
    /// `Some(false)` is "none is" (never another product's Assistant in the
    /// same folder), `None` is not knowing. With no coordinator the chat shows
    /// no conversation at all, because the newest one in the folder is not shprd's.
    fn coordinator_running(&self) -> Option<bool> {
        None
    }

    /// What the last `send` did on its own first, as one plain sentence, taken
    /// once: the chat shows it with the answer and it is not said again. `None`
    /// is nothing.
    fn take_notice(&self) -> Option<String> {
        None
    }
}

/// The writer a service holds, with a `Debug` that never prints a host.
///
/// A host has no printable state and a service is logged with its state in tow,
/// so the wrapper says only whether one is wired.
#[derive(Clone, Default)]
pub struct ChatHost {
    writer: Option<std::sync::Arc<dyn ChatWriter>>,
    /// What the background deliveries and transcript watchers share.
    shared: std::sync::Arc<Shared>,
}

/// State the chat keeps between requests: deliveries that failed after the
/// send had returned, and the transcripts a watcher is following.
#[derive(Default)]
struct Shared {
    /// A delivery's failure, by space, until the next page says it once.
    failures: std::sync::Mutex<std::collections::BTreeMap<String, String>>,
    /// The transcript files a watcher thread is following now.
    watched: std::sync::Mutex<std::collections::BTreeSet<PathBuf>>,
    /// Epoch ms of the last page read, so an idle watcher ends.
    last_read_ms: std::sync::atomic::AtomicU64,
}

/// How long a send waits for the host before answering "accepted" (ms): a
/// failure the host reports at once still refuses the send; a slow delivery
/// (the draft hold) goes on in the background.
const ACCEPT_GRACE_MS: u64 = 150;

/// How often a watcher looks at a transcript's size (ms).
const WATCH_EVERY_MS: u64 = 100;

/// How long a watcher outlives the last page read (ms).
const WATCH_IDLE_MS: u64 = 120_000;

impl ChatHost {
    /// A host with no writer: a send is refused rather than pretended.
    pub fn new() -> Self {
        Self::default()
    }

    /// The host that types chat lines through `writer`.
    pub fn with(writer: std::sync::Arc<dyn ChatWriter>) -> Self {
        Self {
            writer: Some(writer),
            shared: std::sync::Arc::default(),
        }
    }

    /// The writer, when one is wired.
    pub fn get(&self) -> Option<&std::sync::Arc<dyn ChatWriter>> {
        self.writer.as_ref()
    }
}

impl std::fmt::Debug for ChatHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatHost")
            .field("wired", &self.writer.is_some())
            .finish()
    }
}

/// Which space a chat request is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Space {
    /// The run's coordinator, in the repository's own space.
    Coordinator,
    /// One task's worker, in the task's space.
    Task(String),
}

impl Space {
    /// The space `word` names: `coordinator` (or nothing) is the coordinator's,
    /// anything else is taken for a task id.
    pub fn parse(word: Option<&str>) -> Space {
        match word.map(str::trim) {
            None | Some("") | Some(COORDINATOR) => Space::Coordinator,
            Some(task) => Space::Task(task.to_owned()),
        }
    }

    /// The space as a client names it.
    pub fn name(&self) -> &str {
        match self {
            Space::Coordinator => COORDINATOR,
            Space::Task(task) => task,
        }
    }

    /// The space as [`ChatWriter::send`] names it.
    fn target(&self) -> Option<&str> {
        match self {
            Space::Coordinator => None,
            Space::Task(task) => Some(task.as_str()),
        }
    }
}

/// Where a space's agent is and which program it runs, once that is known.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Place {
    /// The program's harness id, when it is known.
    program: Option<String>,
    /// The directory the agent ran in, which is what a record's location is
    /// relative to.
    cwd: PathBuf,
    /// What to say when there is nothing to read, if anything is already known.
    note: Option<String>,
    /// When the agent started, when known: only a conversation begun then or
    /// later is its own.
    since_ms: Option<u64>,
}

/// `GET /api/chat`: the messages a space's agent record gained since `since`.
pub(crate) fn page(
    state: &AppState,
    space: &Space,
    since: &str,
    before: &str,
) -> Result<ChatDto, ApiReject> {
    let place = place_of(state, space)?;
    let mut answer = ChatDto {
        space: space.name().to_owned(),
        program: place.program.clone(),
        session: None,
        messages: Vec::new(),
        cursor: since.to_owned(),
        more: false,
        older: None,
        note: place.note.clone(),
        question: None,
    };
    let Some(program) = place.program.as_deref() else {
        return Ok(answer);
    };
    // No shprd coordinator is running: the chat reads nothing from the folder.
    // The newest conversation there can be another product's Assistant, which is
    // never shown here.
    if matches!(space, Space::Coordinator)
        && state
            .chat
            .get()
            .is_some_and(|writer| writer.coordinator_running() == Some(false))
    {
        answer.note = Some(String::from(
            "shprd's coordinator isn't running yet: shprd starts it for this space",
        ));
        return Ok(answer);
    }
    let Some(spec) = harness_spec(state, program)? else {
        answer.note = Some(format!(
            "no harness entry for {program:?}: the chat reads the programs in the data directory"
        ));
        return Ok(answer);
    };
    if !spec.transcript.readable() {
        answer.note = Some(format!("shprd cannot read {program}'s conversation yet"));
        return Ok(answer);
    }
    // A transcript is the agent CLI's own file, so it is looked for under the
    // roots *that* process had: the real `HOME` it inherits (which a spawn never
    // rewrites) and its XDG data home. The engine's own home is a different
    // directory — `SHPRD_HOME` — and no CLI writes a session under it, so using
    // it here is why every chat pane answered "has not written a session" while
    // the session sat in the person's real `~/.claude`.
    let roots = state.transcript_roots.as_ref().ok_or_else(|| {
        ApiReject::internal(
            "this service was not told the home its agents run with, so it cannot find a \
             transcript: start `shprd serve` from a build that passes $HOME in",
        )
    })?;
    let Some(transcript) =
        Transcript::locate_since(&spec.transcript, roots, &place.cwd, place.since_ms)
    else {
        if place.since_ms.is_some() {
            answer.note = Some(format!(
                "the coordinator ({program}) has not said anything yet: its conversation starts with its first turn"
            ));
            return Ok(answer);
        }
        answer.note = Some(match spec.transcript.format {
            // OpenCode's store and database are session stores rather than one
            // file a session; a missing one is a program that has not run here.
            Some(TranscriptFormat::OpencodeStorage | TranscriptFormat::OpencodeSqlite) => {
                format!("{program} has not run in {} yet", place.cwd.display())
            }
            _ => format!(
                "{program} has not written a session for {} yet",
                place.cwd.display()
            ),
        });
        return Ok(answer);
    };
    watch_transcript(state, &transcript);
    // A question the terminal waits on is the card at the bottom of the chat,
    // whatever page of the conversation this is. A record that cannot be read for a
    // moment (an agent restarting) leaves the card out; it does not fail the page.
    answer.question = transcript
        .pending_question()
        .ok()
        .flatten()
        .as_ref()
        .map(question_dto);
    // No cursor of this session (a first read, a panel that reloaded, an agent
    // that started a new session) opens at the newest messages; a cursor of this
    // session follows forward; `before` goes back through history.
    let page = if transcript.names(before) {
        transcript.read_before(before)
    } else if transcript.names(since) {
        transcript.read(since)
    } else {
        transcript.read_tail()
    }
    .map_err(|err| ApiReject::internal(err.to_string()))?;
    answer.session = Some(transcript.session().to_owned());
    answer.cursor = if transcript.names(before) {
        since.to_owned()
    } else {
        page.cursor
    };
    answer.more = page.more;
    answer.older = page.older;
    answer.messages = page
        .messages
        .into_iter()
        .map(|message| ChatMessageDto {
            role: message.role.as_str().to_owned(),
            text: message.text,
            tools: message
                .tools
                .into_iter()
                .map(|tool| ChatToolDto {
                    name: tool.name,
                    summary: tool.summary,
                })
                .collect(),
            at_ms: message.at_ms,
        })
        .collect();
    if answer.messages.is_empty() && answer.note.is_none() {
        answer.note = Some(String::from("nothing new"));
    }
    Ok(answer)
}

/// The card a pending question is, as a client draws it: the dialog's first
/// question with its choices.
fn question_dto(pending: &PendingQuestion) -> ChatQuestionDto {
    let first = &pending.questions[0];
    ChatQuestionDto {
        id: pending.id.clone(),
        header: first.header.clone(),
        question: first.question.clone(),
        options: first
            .options
            .iter()
            .map(|option| ChatOptionDto {
                label: option.label.clone(),
                description: option.description.clone(),
            })
            .collect(),
        multi_select: first.multi_select,
        more: (pending.questions.len() - 1) as u32,
        answerable: pending.questions.len() == 1 && !first.multi_select,
    }
}

/// The keys that answer a one-question, one-choice dialog, the same a person
/// presses: Down to the row, Enter. `choice` is 0-based; the row after the last
/// option is "Type something", an inline text row: once the cursor is on it the
/// answer is typed straight in and Enter sends. A leading Enter there would
/// submit it empty, and claude declines the question (checked against a real
/// claude dialog, claude 2.1.332).
pub fn answer_steps(choice: usize, text: Option<&str>) -> Vec<AnswerStep> {
    let mut steps: Vec<AnswerStep> = (0..choice)
        .map(|_| AnswerStep::Key(TermKey::Down))
        .collect();
    if let Some(text) = text {
        let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
        steps.push(AnswerStep::Text(one_line));
    }
    steps.push(AnswerStep::Key(TermKey::Enter));
    steps
}

/// The question `space`'s terminal waits on, read from its agent's record.
///
/// `None` is nothing waiting, or a record this service cannot read: a send that
/// cannot tell is not stopped by it.
pub(crate) fn pending_question(state: &AppState, space: &Space) -> Option<PendingQuestion> {
    let place = place_of(state, space).ok()?;
    let program = place.program.as_deref()?;
    let spec = harness_spec(state, program).ok()??;
    let roots = state.transcript_roots.as_ref()?;
    let transcript = Transcript::locate_since(&spec.transcript, roots, &place.cwd, place.since_ms)?;
    transcript.pending_question().ok()?
}

/// `GET /api/chat`'s answer, with a delivery failure the person has not heard
/// of yet said once in its note.
pub(crate) fn page_with_failure(
    state: &AppState,
    space: &Space,
    since: &str,
    before: &str,
) -> Result<ChatDto, ApiReject> {
    state
        .chat
        .shared
        .last_read_ms
        .store(now_ms(), std::sync::atomic::Ordering::Relaxed);
    let mut answer = page(state, space, since, before)?;
    let failed = state
        .chat
        .shared
        .failures
        .lock()
        .ok()
        .and_then(|mut held| held.remove(space.name()));
    if let Some(reason) = failed {
        answer.note = Some(format!(
            "Your last message was not delivered: {reason}. Nothing was sent"
        ));
    }
    Ok(answer)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Follows a transcript file's size and publishes a change when it grows, so a
/// client's chat refreshes within ~`WATCH_EVERY_MS` of a reply chunk instead of
/// waiting for its poll. One thread per file; it ends when nobody reads the
/// chat for [`WATCH_IDLE_MS`].
fn watch_transcript(state: &AppState, transcript: &Transcript) {
    let Some(path) = transcript.path().map(std::path::Path::to_path_buf) else {
        return;
    };
    let shared = std::sync::Arc::clone(&state.chat.shared);
    let started = shared
        .watched
        .lock()
        .map(|mut held| held.insert(path.clone()))
        .unwrap_or(false);
    if !started {
        return;
    }
    let hub = state.hub.clone();
    std::thread::spawn(move || {
        let size = |p: &std::path::Path| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        let mut last = size(&path);
        loop {
            std::thread::sleep(std::time::Duration::from_millis(WATCH_EVERY_MS));
            let idle = now_ms().saturating_sub(
                shared
                    .last_read_ms
                    .load(std::sync::atomic::Ordering::Relaxed),
            );
            if idle > WATCH_IDLE_MS {
                break;
            }
            let now = size(&path);
            if now != last {
                last = now;
                hub.poke();
            }
        }
        if let Ok(mut held) = shared.watched.lock() {
            held.remove(&path);
        }
    });
}

/// `POST /api/chat`: type one turn into a space's agent, as the person.
pub(crate) fn send(state: &AppState, body: &[u8]) -> Result<ChatSendReply, ApiReject> {
    let request: ChatSendDto = serde_json::from_slice(body).map_err(|err| {
        ApiReject::bad_request(format!(
            "the body is not a chat message: {err}; send {{\"space\": \"coordinator\", \"text\": \"…\"}}"
        ))
    })?;
    let text = request.text.trim();
    let attachments = request.attachments.unwrap_or_default();
    if text.is_empty() && attachments.is_empty() {
        return Err(ApiReject::bad_request(
            "a chat message needs some words or a file: `text` was empty, so nothing was typed",
        ));
    }
    let space = Space::parse(Some(&request.space));
    let writer = state.chat.get().ok_or_else(|| {
        ApiReject::refused(
            "this service cannot type into a terminal: restart `shprd serve` from a build \
             with the host wired. Nothing was sent",
        )
    })?;
    // A terminal on a question refuses a typed line (or takes it into the
    // dialog): say so, typed, before anything is stored or typed.
    if pending_question(state, &space).is_some() {
        return Err(ApiReject::question_waiting());
    }
    // Resolving the space first means a task that does not exist is refused
    // before anything is typed anywhere.
    let began = std::time::Instant::now();
    let place = place_of(state, &space)?;
    let resolved = began.elapsed();
    // Files go where the agent reads without asking (its own working
    // directory), and the message names the copies.
    let stored = attachments::store(&place.cwd, &attachments)?;
    let message = attachments::compose(text, &stored);
    let stored_at = began.elapsed();
    // Delivery (which may hold a draft for seconds) runs off the request: the
    // person's send returns once the host has had a moment to refuse it.
    let (done_tx, done_rx) = std::sync::mpsc::channel::<Result<(), String>>();
    let background = BackgroundSend {
        writer: std::sync::Arc::clone(writer),
        shared: std::sync::Arc::clone(&state.chat.shared),
        hub: state.hub.clone(),
        space: space.clone(),
        message,
        stored,
        began,
    };
    let delivered_after = stored_at;
    std::thread::spawn(move || background.run(done_tx, resolved, delivered_after));
    if let Ok(Err(reason)) = done_rx.recv_timeout(std::time::Duration::from_millis(ACCEPT_GRACE_MS))
    {
        // The host's own refusal of a line typed at a dialog, when the record
        // could not say so first.
        if reason.contains("blocked (question)") {
            return Err(ApiReject::question_waiting());
        }
        return Err(ApiReject::refused(format!("{reason}. Nothing was sent")));
    }
    tracing::debug!(
        returned_ms = began.elapsed().as_millis() as u64,
        "chat send returned"
    );
    Ok(ChatSendReply {
        space: space.name().to_owned(),
        sent: true,
        notice: writer.take_notice(),
    })
}

/// One accepted message, on its way to the agent's pane.
struct BackgroundSend {
    writer: std::sync::Arc<dyn ChatWriter>,
    shared: std::sync::Arc<Shared>,
    hub: crate::hub::Hub,
    space: Space,
    message: String,
    stored: Vec<PathBuf>,
    began: std::time::Instant,
}

impl BackgroundSend {
    fn run(
        self,
        done: std::sync::mpsc::Sender<Result<(), String>>,
        resolved: std::time::Duration,
        stored_at: std::time::Duration,
    ) {
        let typed = self.writer.send(self.space.target(), &self.message);
        tracing::debug!(
            resolve_ms = resolved.as_millis() as u64,
            attachments_ms = (stored_at - resolved).as_millis() as u64,
            deliver_ms = (self.began.elapsed() - stored_at).as_millis() as u64,
            ok = typed.is_ok(),
            "chat send hops"
        );
        match &typed {
            Ok(()) => {}
            Err(reason) => {
                // Nothing reached the agent, so the copies are not left behind for a retry to repeat.
                attachments::discard(&self.stored);
                if let Ok(mut held) = self.shared.failures.lock() {
                    held.insert(self.space.name().to_owned(), reason.clone());
                }
            }
        }
        // The line is in the pane (or the failure is waiting): tell every
        // watching client now rather than at its next poll.
        self.hub.poke();
        let _ = done.send(typed);
    }
}

/// `POST /api/chat/answer`: answer the question a space's terminal waits on,
/// pressing the keys a person would.
pub(crate) fn answer(state: &AppState, body: &[u8]) -> Result<ChatAnswerReply, ApiReject> {
    let request: ChatAnswerDto = serde_json::from_slice(body).map_err(|err| {
        ApiReject::bad_request(format!(
            "the body is not an answer: {err}; send {{\"space\": \"coordinator\", \"id\": \"…\", \"option\": 1}} or \"text\""
        ))
    })?;
    let space = Space::parse(Some(&request.space));
    let writer = state.chat.get().ok_or_else(|| {
        ApiReject::refused("this service cannot press a key in a terminal. Nothing was answered")
    })?;
    let waiting = pending_question(state, &space).filter(|pending| pending.id == request.id);
    let Some(waiting) = waiting else {
        return Err(ApiReject::refused(
            "that question is not waiting any more. Nothing was answered",
        ));
    };
    let dto = question_dto(&waiting);
    if !dto.answerable {
        return Err(ApiReject::refused(
            "this dialog has several questions or several choices: answer it in the terminal",
        ));
    }
    let steps = match (request.option, request.text.as_deref().map(str::trim)) {
        (Some(option), _) => {
            let at = (option as usize)
                .checked_sub(1)
                .filter(|at| *at < dto.options.len());
            let Some(at) = at else {
                return Err(ApiReject::bad_request(format!(
                    "the question has {} choices: `option` counts from 1",
                    dto.options.len()
                )));
            };
            answer_steps(at, None)
        }
        (None, Some(text)) if !text.is_empty() => answer_steps(dto.options.len(), Some(text)),
        _ => {
            return Err(ApiReject::bad_request(
                "an answer needs a choice (`option`) or some words (`text`)",
            ));
        }
    };
    writer
        .answer(space.target(), &steps)
        .map_err(|reason| ApiReject::refused(format!("{reason}. Nothing was answered")))?;
    state.hub.poke();
    Ok(ChatAnswerReply {
        space: space.name().to_owned(),
        answered: true,
    })
}

/// `POST /api/chat/stop`: press Escape in a space's agent, as the person.
pub(crate) fn stop(state: &AppState, body: &[u8]) -> Result<ChatStopReply, ApiReject> {
    let request: ChatStopDto = serde_json::from_slice(body).map_err(|err| {
        ApiReject::bad_request(format!(
            "the body is not a stop request: {err}; send {{\"space\": \"coordinator\"}}"
        ))
    })?;
    let space = Space::parse(Some(&request.space));
    let writer = state.chat.get().ok_or_else(|| {
        ApiReject::refused("this service cannot press a key in a terminal. Nothing was stopped")
    })?;
    place_of(state, &space)?;
    writer
        .interrupt(space.target())
        .map_err(|reason| ApiReject::refused(format!("{reason}. Nothing was stopped")))?;
    Ok(ChatStopReply {
        space: space.name().to_owned(),
        stopped: true,
    })
}

/// Where `space`'s agent ran, and which program it is.
fn place_of(state: &AppState, space: &Space) -> Result<Place, ApiReject> {
    match space {
        Space::Coordinator => coordinator_place(state),
        Space::Task(task) => task_place(state, task),
    }
}

/// The coordinator's program comes from the settings: the role is the run's own
/// agent, and the repository root is where it runs.
fn coordinator_place(state: &AppState) -> Result<Place, ApiReject> {
    let mut place = Place {
        program: None,
        cwd: state.repo.clone(),
        note: None,
        // The conversation shown is the one of the agent a chat line reaches.
        since_ms: state.chat.get().and_then(|writer| writer.started_ms(None)),
    };
    let Some(home) = state.home.as_deref() else {
        place.note = Some(String::from(
            "this service has no home directory, so the coordinator's program is unknown",
        ));
        return Ok(place);
    };
    let loaded = match Loaded::read(home, Some(&state.repo)) {
        Ok(loaded) => loaded,
        Err(err) => {
            place.note = Some(format!("the settings could not be read: {err}"));
            return Ok(place);
        }
    };
    let resolved = loaded.resolve(None);
    // The coordinator is the first role of kind `coordinator`, read exactly as
    // `shprd doctor` reads it (`kind_role(RoleKind::Coordinator)`): the built-in
    // `coordinator` role is that kind, so `roles.coordinator.agent` is what a
    // person's settings set, and a custom role takes the kind over by being
    // ahead of it in `roles.order`.
    let Some(role) = resolved
        .roles()
        .into_iter()
        .find(|role| role.kind.value == RoleKind::Coordinator)
    else {
        place.note = Some(format!(
            "no coordinator role is configured: pick one in the settings \
             (roles.{COORDINATOR_ID}.agent)"
        ));
        return Ok(place);
    };
    match role.agent.map(|agent| agent.value) {
        Some(program) if !program.trim().is_empty() => place.program = Some(program),
        _ => {
            place.note = Some(format!(
                "no coordinator program is set: pick one in the settings (roles.{}.agent)",
                role.id
            ))
        }
    }
    Ok(place)
}

/// A task's program is the agent its latest attempt ran, and its record lives
/// where that attempt's worktree was.
fn task_place(state: &AppState, task: &str) -> Result<Place, ApiReject> {
    let (run, _) = ledger::load(&state.repo).map_err(|err| match err {
        ledger::ReadError::Log(err) => ApiReject::ledger(&err),
        ledger::ReadError::Fold(err) => {
            ApiReject::internal(format!("the run log does not fold: {err}"))
        }
    })?;
    let found = task_of(&run, task).ok_or_else(|| ApiReject::unknown_task(task))?;
    let Some(attempt) = found.latest_attempt() else {
        return Ok(Place {
            program: None,
            cwd: PathBuf::from(&found.id),
            note: Some(format!(
                "{task} has not started: there is no attempt to read a conversation from"
            )),
            since_ms: None,
        });
    };
    Ok(Place {
        program: Some(attempt.agent.clone()),
        cwd: PathBuf::from(&attempt.worktree),
        note: None,
        since_ms: None,
    })
}

/// One task by id, whether it was ever started or not.
fn task_of<'a>(state: &'a RunState, task: &str) -> Option<&'a Task> {
    state.tasks.get(task)
}

/// The harness entry of `program`, from the data this build ships.
///
/// The table is the [`DataDir`](shprd_agents::data::DataDir) the binary
/// resolved, so a harness read here is the same file a spawn reads, and a chat
/// follows the embedded copy when the machine has no data directory at all.
fn harness_spec(state: &AppState, program: &str) -> Result<Option<HarnessSpec>, ApiReject> {
    let Some(data) = state.data.as_ref() else {
        return Ok(None);
    };
    let specs = data.harness_specs().map_err(|err| {
        ApiReject::internal(format!("the harness table could not be read: {err}"))
    })?;
    Ok(specs.into_iter().find(|spec| spec.id == program))
}
