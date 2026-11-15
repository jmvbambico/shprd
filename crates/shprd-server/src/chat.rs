//! The chat: one space's agent conversation, and a turn typed back into it.
//!
//! A space is the coordinator's or a task's ([`Space`]). Reading one is the
//! service's own work: it finds the agent's program from the ledger (a task's
//! latest attempt names it) or from the settings (the coordinator role does),
//! asks that program's harness where the session lives
//! ([`shprd_agents::transcript::Transcript::locate`]) and answers what the
//! record gained after the client's cursor. Nothing is written: a transcript is
//! read-only evidence, and the agent's own file is the only copy of it.
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
use shprd_agents::transcript::Transcript;
use shprd_core::settings::Loaded;
use shprd_core::state::{RunState, Task};

use crate::api::AppState;
use crate::dto::{ChatDto, ChatMessageDto, ChatSendDto, ChatSendReply, ChatToolDto};
use crate::error::ApiReject;
use crate::ledger;

/// The word a client uses for the coordinator's space.
pub const COORDINATOR: &str = "coordinator";

/// The role whose program is the coordinator.
const COORDINATOR_ROLE: &str = "orchestrator";

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
}

/// The writer a service holds, with a `Debug` that never prints a host.
///
/// A host has no printable state and a service is logged with its state in tow,
/// so the wrapper says only whether one is wired.
#[derive(Clone, Default)]
pub struct ChatHost(Option<std::sync::Arc<dyn ChatWriter>>);

impl ChatHost {
    /// A host with no writer: a send is refused rather than pretended.
    pub fn new() -> Self {
        Self::default()
    }

    /// The host that types chat lines through `writer`.
    pub fn with(writer: std::sync::Arc<dyn ChatWriter>) -> Self {
        Self(Some(writer))
    }

    /// The writer, when one is wired.
    pub fn get(&self) -> Option<&std::sync::Arc<dyn ChatWriter>> {
        self.0.as_ref()
    }
}

impl std::fmt::Debug for ChatHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatHost")
            .field("wired", &self.0.is_some())
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
}

/// `GET /api/chat`: the messages a space's agent record gained since `since`.
pub(crate) fn page(state: &AppState, space: &Space, since: &str) -> Result<ChatDto, ApiReject> {
    let place = place_of(state, space)?;
    let mut answer = ChatDto {
        space: space.name().to_owned(),
        program: place.program.clone(),
        session: None,
        messages: Vec::new(),
        cursor: since.to_owned(),
        more: false,
        note: place.note.clone(),
    };
    let Some(program) = place.program.as_deref() else {
        return Ok(answer);
    };
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
    let home = state.home.as_deref().ok_or_else(|| {
        ApiReject::internal(
            "this service has no home directory, so it cannot find a transcript: \
             set SHPRD_HOME or HOME",
        )
    })?;
    let Some(transcript) = Transcript::locate(&spec.transcript, home, &place.cwd) else {
        answer.note = Some(match spec.transcript.format {
            // An OpenCode store is a folder of sessions; a missing one is a
            // program that has not run here yet.
            Some(TranscriptFormat::OpencodeStorage) => {
                format!("{program} has not run in {} yet", place.cwd.display())
            }
            _ => format!(
                "{program} has not written a session for {} yet",
                place.cwd.display()
            ),
        });
        return Ok(answer);
    };
    let page = transcript
        .read(since)
        .map_err(|err| ApiReject::internal(err.to_string()))?;
    answer.session = Some(transcript.session().to_owned());
    answer.cursor = page.cursor;
    answer.more = page.more;
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

/// `POST /api/chat`: type one turn into a space's agent, as the person.
pub(crate) fn send(state: &AppState, body: &[u8]) -> Result<ChatSendReply, ApiReject> {
    let request: ChatSendDto = serde_json::from_slice(body).map_err(|err| {
        ApiReject::bad_request(format!(
            "the body is not a chat message: {err}; send {{\"space\": \"coordinator\", \"text\": \"…\"}}"
        ))
    })?;
    let text = request.text.trim();
    if text.is_empty() {
        return Err(ApiReject::bad_request(
            "a chat message needs some words: `text` was empty, so nothing was typed",
        ));
    }
    let space = Space::parse(Some(&request.space));
    let writer = state.chat.get().ok_or_else(|| {
        ApiReject::refused(
            "this service cannot type into a terminal: restart `shprd serve` from a build \
             with the host wired. Nothing was sent",
        )
    })?;
    // Resolving the space first means a task that does not exist is refused
    // before anything is typed anywhere.
    place_of(state, &space)?;
    writer
        .send(space.target(), text)
        .map_err(|reason| ApiReject::refused(format!("{reason}. Nothing was sent")))?;
    Ok(ChatSendReply {
        space: space.name().to_owned(),
        sent: true,
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
    match resolved.role(COORDINATOR_ROLE) {
        Some(role) => match role.agent.map(|agent| agent.value) {
            Some(program) if !program.trim().is_empty() => place.program = Some(program),
            _ => {
                place.note = Some(format!(
                    "no coordinator program is set: pick one in the settings (roles.{COORDINATOR_ROLE}.agent)"
                ))
            }
        },
        None => {
            place.note = Some(format!(
                "no {COORDINATOR_ROLE} role is configured: pick one in the settings \
                 (roles.{COORDINATOR_ROLE}.agent)"
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
        });
    };
    Ok(Place {
        program: Some(attempt.agent.clone()),
        cwd: PathBuf::from(&attempt.worktree),
        note: None,
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
