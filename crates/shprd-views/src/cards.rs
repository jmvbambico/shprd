//! The needs-you card an escalation draws, and whether it still holds.
//!
//! An escalation is a log line: it records what needs deciding at the moment it
//! was raised. What a board draws is that line read against everything the log
//! has recorded since, so a card a person already answered is drawn as the
//! answer left it, and a card about work that has since landed or been dropped
//! is not drawn at all.
//!
//! It is one projector for two readers — `/api/state`'s `needs` list
//! ([`crate::status`]'s caller in the service) and the derived row's reason
//! ([`crate::derived`]) — so the board's card and `shprd status` cannot disagree
//! about whether a person is needed.

use shprd_core::gate;
use shprd_core::state::{Escalation, RunState, TaskStatus};

/// Whether a card asks the person to choose, or only tells them what shprd did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CardKind {
    /// shprd stopped and the person chooses: it needs them.
    #[default]
    Decision,
    /// shprd tells the person something; nothing is left to choose, so it never
    /// makes a task read Needs you.
    Notice,
}

/// The ids a card's actions are drawn from, in the order a client offers them.
pub mod action {
    /// Open the accept dialog for the task.
    pub const REVIEW: &str = "review";
    /// Send the task back to a new attempt (`task.retry`); the task must be failed.
    pub const RETRY: &str = "retry";
    /// Start the task's review again (`task.review_again`); the task must be reported.
    pub const RETRY_REVIEW: &str = "retry_review";
    /// Deliver the task's brief again (`task.resend_brief`).
    pub const RESEND_BRIEF: &str = "resend_brief";
    /// Open the drop dialog for the task.
    pub const DROP: &str = "drop";
    /// Pass the card's text to the coordinator to sort out.
    pub const ASK_ASSISTANT: &str = "ask_assistant";
    /// Answer an agent's escalation in the chat the card is drawn in.
    pub const REPLY: &str = "reply";
    /// Remove the card (`need.dismiss`).
    pub const DISMISS: &str = "dismiss";
}

/// One button a card offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardAction {
    /// The id, one of [`action`]'s.
    pub id: String,
    /// The words on the button.
    pub label: String,
}

impl CardAction {
    /// The action `id` with its button words.
    fn of(id: &str) -> Self {
        let label = match id {
            action::REVIEW => "Review",
            action::RETRY => "Send back",
            action::RETRY_REVIEW => "Retry review",
            action::RESEND_BRIEF => "Resend brief",
            action::DROP => "Drop task\u{2026}",
            action::ASK_ASSISTANT => "Ask the assistant",
            action::REPLY => "Reply",
            action::DISMISS => "Dismiss",
            other => {
                debug_assert!(false, "unknown card action id {other}");
                other
            }
        };
        Self {
            id: id.to_string(),
            label: label.to_string(),
        }
    }
}

/// How long a clash notice waits for the redo to start before it asks the person.
pub const CLASH_NOTICE_MS: u64 = 10 * 60 * 1000;

/// The key every self-update card shares: only the newest is drawn.
pub const SELF_UPDATE_KEY: &str = "self-update";

/// One card as a board draws it: what it says, what the person can do about it,
/// and what happens if nobody answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Card {
    /// What needs deciding, as the reader should see it now.
    pub body: String,
    /// The action taken if nobody answers.
    pub default_action: String,
    /// Whether the person chooses, or is only told.
    pub kind: CardKind,
    /// The buttons, first is the primary; never empty.
    pub actions: Vec<CardAction>,
    /// Cards with one key are one question: the newest of a run-wide family, or
    /// the card's own sequence.
    pub key: String,
}

/// What a card says: a land step that keeps failing is already one plain
/// sentence ("Couldn't merge T1: …"), so the engine's marker in front of it
/// (`T1: its `land` step has failed the same way 3 passes in a row (attempt 1): `)
/// is dropped; every other body is read as written.
fn plain_body(body: &str) -> String {
    // A log written before SHPR-83 holds "T1 is ready to land: run `shprd approve T1`": the
    // person in the desktop never types a command, so it reads as the new words do.
    if let Some((head, rest)) = body.split_once(" is ready to land: run `shprd approve ")
        && !head.contains(' ')
        && rest.ends_with('`')
    {
        return format!("{head} is ready to land: Review and Accept it in the Tasks pane");
    }
    // The forge form of the same notice: "... asks: approve pull request #3 on the forge, or run `shprd approve T1`".
    if let Some((head, rest)) = body.split_once(" and its merge policy asks: approve ")
        && let Some((which, tail)) = rest.split_once(" on the forge, or run `shprd approve ")
        && tail.ends_with('`')
    {
        return format!(
            "{head} and its merge policy asks: Review and Accept it in the Tasks pane, or approve {which} on the forge"
        );
    }
    let plain = body
        .split_once(" step has failed the same way ")
        .filter(|(head, _)| head.ends_with("its `land`"))
        .and_then(|(_, rest)| rest.split_once("): "))
        .map(|(_, tail)| tail)
        .filter(|tail| tail.starts_with("Couldn't "));
    plain.unwrap_or(body).to_string()
}

/// The body of the card a spawn records when the brief never reached the agent.
pub const BRIEF_LOST: &str = "brief not delivered";

/// The card `escalation` draws against `state` now, or `None` when it is history.
///
/// It reads the wall clock for the one rule that waits (a clash notice that ages
/// into a decision); [`card_at`] takes the time and is the pure form.
pub fn card(state: &RunState, escalation: &Escalation) -> Option<Card> {
    card_at(state, escalation, wall_ms())
}

/// The wall clock, in ms since the epoch.
pub(crate) fn wall_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| u64::try_from(since.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// The card `escalation` draws against `state` at `now_ms`, or `None` when it is
/// history.
///
/// A card is history when nothing is left for the person to decide:
///
/// - its task landed or was dropped, so the work it held back is decided;
/// - a newer attempt answered it, which is how a "retry" a person gave — or a
///   spawn someone ran by hand — retires a card about a failed review, and how
///   an engine card that names no attempt ends once an attempt starts after it;
/// - it offered a spent review's choices and those were answered with a verb
///   the person then ran (see [`answered`]);
/// - it is a self-update card and a newer one of the family was raised.
///
/// A card about no single task (a run's own) is kept unless it is a self-update
/// card that a success or a newer failure settled.
pub fn card_at(state: &RunState, escalation: &Escalation, now_ms: u64) -> Option<Card> {
    let Some(id) = escalation.task.as_deref() else {
        // A successful update settles the failed updates before it; its record is no card.
        if gate::is_update_settled(escalation) || gate::update_failure_settled(state, escalation) {
            return None;
        }
        // Only the newest failed update is drawn: every self-update card shares one key.
        if is_update_failure(escalation)
            && state
                .escalations
                .iter()
                .any(|later| later.seq > escalation.seq && is_update_failure(later))
        {
            return None;
        }
        return Some(run_wide_card(escalation));
    };
    let Some(task) = state.tasks.get(id) else {
        // A card about a task the log no longer holds is the log's to keep: it
        // was raised about something, and dropping it would lose the record.
        return Some(build(
            escalation,
            plain_body(&escalation.body),
            CardKind::Decision,
            &[action::ASK_ASSISTANT, action::DISMISS],
        ));
    };
    if matches!(task.status, TaskStatus::Integrated | TaskStatus::Dropped) {
        return None;
    }
    // A card about an attempt names it, and a newer attempt is the answer to the
    // failure it named.
    let named_attempt = gate::card_attempt(id, &escalation.body);
    if let Some(attempt) = named_attempt
        && task
            .latest_attempt()
            .is_some_and(|latest| latest.n > attempt)
    {
        return None;
    }
    // An engine card that names no attempt is about the attempt that was latest when it was
    // raised: an attempt spawned after it (a redo, a send-back) is what it was waiting for.
    // "Ready to land" is about the task's work, not an attempt, and is never retired so.
    if !escalation.by_agent
        && named_attempt.is_none()
        && !is_ready_to_land(&escalation.body)
        && task
            .latest_attempt()
            .and_then(|latest| latest.spawn_seq)
            .is_some_and(|spawned| spawned > escalation.seq)
    {
        return None;
    }
    // An agent's escalation is its opinion when it wrote it. A review that passed after it
    // outranks it: the person is then asked to Accept, not to settle a claim the log has
    // since answered (SHPR-83: a coordinator's false "the worker never asked you" kept a
    // reviewed task at Needs you). The engine's own cards are facts and are never retired so.
    if escalation.by_agent
        && named_attempt.is_none()
        && task.latest_attempt().is_some_and(|latest| {
            latest
                .gates
                .iter()
                .any(|g| g.gate == gate::REVIEW_GATE && g.passed && g.seq > escalation.seq)
        })
    {
        return None;
    }
    // A lost brief is retired by any progress line recorded after it: the agent
    // reported (so it had the brief) or a person's resend recorded one.
    if escalation.body == BRIEF_LOST
        && task
            .last_status
            .as_ref()
            .is_some_and(|progress| progress.seq > escalation.seq)
    {
        return None;
    }
    // A review whose first turn never arrived is retired by the review being started again:
    // the engine records a new request, which the Retry review button asks for.
    // The body may end with what the CLI was waiting on ("...: claude is asking
    // to trust the folder"), so the turn's own words are matched as a prefix.
    if is_review_not_delivered(&escalation.body)
        && task.latest_attempt().is_some_and(|latest| {
            latest
                .reviews
                .iter()
                .any(|request| request.seq > escalation.seq)
        })
    {
        return None;
    }
    if let Some(attempt) = spent_review_attempt(id, &escalation.body)
        && let Some(answer) = gate::rounds_spent_answer(state, task, attempt)
    {
        return answered(task, attempt, escalation, &answer);
    }
    Some(task_card(task, escalation, now_ms))
}

/// Whether `escalation` is one of the three a failed self-update raises.
fn is_update_failure(escalation: &Escalation) -> bool {
    escalation.task.is_none()
        && gate::UPDATE_FAILED_ACTIONS.contains(&escalation.default_action.as_str())
}

/// Whether `body` is the engine's "ready to land" notice.
fn is_ready_to_land(body: &str) -> bool {
    body.contains(" is ready to land")
}

/// Whether `body` says a review's first turn never arrived.
fn is_review_not_delivered(body: &str) -> bool {
    body.starts_with("review kickoff not delivered")
        || body.starts_with("review brief not delivered")
}

/// The id a card with none of its own is keyed by: its sequence.
fn own_key(escalation: &Escalation) -> String {
    format!("need:{}", escalation.seq)
}

/// A card for `escalation` with `body`, `kind` and the `ids` of its buttons.
///
/// Two ids in a row the same (a gated button that fell back to what is already
/// there) are one button.
fn build(escalation: &Escalation, body: String, kind: CardKind, ids: &[&str]) -> Card {
    let mut actions: Vec<CardAction> = Vec::new();
    for id in ids {
        if !actions.iter().any(|have| have.id == *id) {
            actions.push(CardAction::of(id));
        }
    }
    Card {
        body,
        default_action: escalation.default_action.clone(),
        kind,
        actions,
        key: own_key(escalation),
    }
}

/// The card of an escalation with no task: a self-update's outcome, or whatever
/// else a run says.
fn run_wide_card(escalation: &Escalation) -> Card {
    if !is_update_failure(escalation) {
        let (kind, ids): (_, &[&str]) = if escalation.by_agent {
            (CardKind::Decision, &[action::REPLY, action::DISMISS])
        } else {
            (
                CardKind::Decision,
                &[action::ASK_ASSISTANT, action::DISMISS],
            )
        };
        return build(escalation, plain_body(&escalation.body), kind, ids);
    }
    // The first two outcomes leave shprd as it was: it tells, and nothing is left to choose.
    // The third leaves it possibly not running, which is the person's to put right.
    let (kind, ids): (_, &[&str]) = if escalation.default_action == gate::UPDATE_FAILED_ACTIONS[2] {
        (
            CardKind::Decision,
            &[action::ASK_ASSISTANT, action::DISMISS],
        )
    } else {
        (CardKind::Notice, &[action::DISMISS])
    };
    let mut card = build(escalation, no_backticks(&escalation.body), kind, ids);
    card.key = SELF_UPDATE_KEY.to_string();
    card
}

/// `text` with no backtick in it: a card is plain words, never a command.
fn no_backticks(text: &str) -> String {
    text.replace('`', "'")
}

/// The card of an engine or agent escalation about `task`, read against `now_ms`.
fn task_card(task: &shprd_core::state::Task, escalation: &Escalation, now_ms: u64) -> Card {
    let body = &escalation.body;
    let id = task.id.as_str();
    // `Send back` is refused for a task that has not failed, `Retry review` for one that has
    // not reported: the card then offers the assistant in its place.
    let send_back = if task.status == TaskStatus::Failed {
        action::RETRY
    } else {
        action::ASK_ASSISTANT
    };
    let retry_review = if task.status == TaskStatus::Reported {
        action::RETRY_REVIEW
    } else {
        action::ASK_ASSISTANT
    };
    if escalation.by_agent {
        return build(
            escalation,
            plain_body(body),
            CardKind::Decision,
            &[action::REPLY, action::DISMISS],
        );
    }
    let decision = |text: String, ids: &[&str]| build(escalation, text, CardKind::Decision, ids);
    if is_ready_to_land(body) {
        return decision(plain_body(body), &[action::REVIEW]);
    }
    if body == BRIEF_LOST {
        return decision(plain_body(body), &[action::RESEND_BRIEF, action::DROP]);
    }
    if is_review_not_delivered(body)
        || body.starts_with(&format!("{id}: the engine's review did not start: "))
        || body.starts_with(&format!(
            "{id}: the reviewer's findings could not be read: "
        ))
    {
        return decision(plain_body(body), &[retry_review, action::DROP]);
    }
    if spent_review_attempt(id, body).is_some() {
        return decision(plain_body(body), &[action::REVIEW, action::DROP]);
    }
    if body.starts_with(&format!("{id}: sending attempt ")) {
        return decision(plain_body(body), &[send_back, action::DROP]);
    }
    if body.contains(" step has failed the same way ") {
        return decision(plain_body(body), &[action::ASK_ASSISTANT, action::DROP]);
    }
    // A backup that did not start and a provider that gave up share this default.
    if escalation.default_action == "pick the task up by hand" {
        return decision(plain_body(body), &[send_back, action::DROP]);
    }
    if let Some(files) = clash_files(id, body) {
        let line = if files.is_empty() {
            String::new()
        } else {
            format!("\nFiles: {}", no_backticks(&files))
        };
        // The redo is shprd's own: a notice, until it has had time to start and has not.
        if now_ms.saturating_sub(task.touched_at_ms) < CLASH_NOTICE_MS {
            return build(
                escalation,
                format!("{}{line}", clash_notice_words(id)),
                CardKind::Notice,
                &[action::DISMISS],
            );
        }
        return decision(
            format!("{}{line}", clash_stalled_words(id)),
            &[send_back, action::DROP],
        );
    }
    decision(plain_body(body), &[action::ASK_ASSISTANT, action::DISMISS])
}

/// What a clash notice says: the cause, and that shprd is already putting it right.
pub fn clash_notice_words(task: &str) -> String {
    format!(
        "{task} clashed with work that landed after it. shprd is redoing it on top of the latest. Nothing for you to do."
    )
}

/// What a clash says once the redo has not started in time.
pub fn clash_stalled_words(task: &str) -> String {
    format!("{task} clashed with work that landed after it, and the redo has not started.")
}

/// The files `body` says clashed, when it is the card of a landing that clashed.
///
/// An empty list is a clash that named none. The body is written by the landing
/// ("T clashed with work that landed after it; it is being redone …
/// (files in conflict: a, b)") and by older logs in the same words.
fn clash_files(task: &str, body: &str) -> Option<String> {
    let rest = body.strip_prefix(task)?;
    rest.strip_prefix(" clashed with work that landed after it")?;
    let files = body
        .rsplit_once(" (files in conflict: ")
        .and_then(|(_, tail)| tail.strip_suffix(')'))
        .unwrap_or_default();
    Some(files.to_string())
}

/// The attempt a spent review's card names, when `body` is one of those.
///
/// A send-back that did not start is a card too, but it offers no choices and
/// has no ask behind it, so the answer rule never applies to it.
fn spent_review_attempt(task: &str, body: &str) -> Option<u32> {
    let attempt = gate::card_attempt(task, body)?;
    let marker = format!("{task}: its review of attempt {attempt} failed ");
    body.starts_with(&marker).then_some(attempt)
}

/// What an answered spent-review card draws now, or `None` once its verb ran.
///
/// The ask that offered the choices has been consumed by the answer, so a card
/// that kept pointing at it would draw buttons every click of which is refused.
/// Two answers leave the engine's own work:
///
/// - "retry with another round" is a round the engine spends by itself, so the
///   card stands down at once and the new attempt takes over;
/// - any other answer is a decision the run records and the person carries out
///   with a verb of their own, so the card keeps needing them — with the verb
///   named — until that verb's record is in the log.
fn answered(
    task: &shprd_core::state::Task,
    attempt: u32,
    escalation: &Escalation,
    answer: &str,
) -> Option<Card> {
    if !gate::answer_leaves_a_verb(answer) {
        return None;
    }
    // The verb the card names is already in the log, so there is nothing left
    // for the person to do: dropping it is the task's `Dropped` status (read
    // above) and taking it as it is is the override.
    if gate::rounds_spent_verb_ran(task, answer) {
        return None;
    }
    let send_back = if task.status == TaskStatus::Failed {
        action::RETRY
    } else {
        action::ASK_ASSISTANT
    };
    Some(build(
        escalation,
        gate::answered_rounds_body(task, attempt, gate::failed_review_rounds(task), answer),
        CardKind::Decision,
        &[send_back, action::REVIEW, action::DROP],
    ))
}

#[cfg(test)]
mod tests {

    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use shprd_core::ask::AskKind;
    use shprd_core::event::{Finding, Severity};
    use shprd_core::gate;
    use shprd_core::state::{
        AskAnswerRecord, AskRecord, Attempt, Escalation, GateResult, RunState, Task, TaskStatus,
    };

    #[test]
    fn a_repeated_land_failure_card_reads_as_the_plain_sentence() {
        let marked = "T1: its `land` step has failed the same way 3 passes in a row (attempt 1): Couldn't merge T1: this repository has no GitHub remote";
        assert_eq!(
            super::plain_body(marked),
            "Couldn't merge T1: this repository has no GitHub remote"
        );
        let other = "T1: its `start` step has failed the same way 3 passes in a row (attempt 1): no workspace";
        assert_eq!(super::plain_body(other), other);
    }
    use super::card;

    /// One attempt that recorded a failed review gate.
    fn attempt(n: u32) -> Attempt {
        Attempt {
            n,
            agent: String::from("w1"),
            model: None,
            worktree: String::from("/wt/T1"),
            branch: String::from("shprd/r1/T1"),
            spawn_seq: Some(1),
            stopped: false,
            report: None,
            gates: vec![GateResult {
                gate: String::from(gate::REVIEW_GATE),
                passed: false,
                findings: vec![Finding {
                    severity: Severity::Major,
                    title: String::from("No evidence that test_calc.py still passes"),
                    location: None,
                    detail: String::new(),
                }],
                source: None,
                seq: 4,
            }],
            reviews: Vec::new(),
        }
    }

    /// A task holding `attempts`, in the given status.
    fn task(status: TaskStatus, attempts: Vec<Attempt>) -> Task {
        Task {
            id: String::from("T1"),
            title: String::from("a task"),
            scope: Vec::new(),
            acceptance: Vec::new(),
            tags: Vec::new(),
            skills: Vec::new(),
            deps: Vec::new(),
            status,
            attempts,
            overrides: Vec::new(),
            dropped_reason: None,
            pr: None,
            approved_head: None,
            last_status: None,
            nothing_to_merge: None,
            fallbacks: Vec::new(),
            added_at_ms: 0,
            touched_at_ms: 0,
        }
    }

    /// A run holding `T1` and its spent-review card, as the failure leaves it.
    fn spent_run(answer: Option<&str>) -> (RunState, Escalation) {
        let body = gate::rounds_spent_body(&task(TaskStatus::Failed, vec![attempt(1)]), 1, 2);
        let mut state = RunState::default();
        state.tasks.insert(
            String::from("T1"),
            task(TaskStatus::Failed, vec![attempt(1)]),
        );
        state.asks.insert(
            (String::from("T1"), gate::rounds_spent_ask(1)),
            AskRecord {
                id: gate::rounds_spent_ask(1),
                task: String::from("T1"),
                kind: AskKind::Question,
                text: body.clone(),
                options: gate::ROUNDS_SPENT_OPTIONS
                    .iter()
                    .map(|choice| (*choice).to_string())
                    .collect(),
                created_at_ms: 1,
                expires_at_ms: None,
                raised_seq: 3,
                answer: answer.map(|text| AskAnswerRecord {
                    text: text.to_string(),
                    seq: 5,
                }),
            },
        );
        let escalation = Escalation {
            task: Some(String::from("T1")),
            body,
            default_action: String::from(gate::ESCALATION_ACTION),
            seq: 4,
            by_agent: false,
        };
        state.escalations.push(escalation.clone());
        (state, escalation)
    }

    #[test]
    fn an_agents_claim_is_retired_by_a_review_that_passed_after_it_but_the_engines_card_is_not() {
        // SHPR-83: a coordinator wrote "T1's worker never asked you" before the review passed.
        let mut at = attempt(1);
        at.gates.push(shprd_core::state::GateResult {
            gate: gate::REVIEW_GATE.to_string(),
            passed: true,
            findings: Vec::new(),
            source: None,
            seq: 9,
        });
        let mut state = RunState::default();
        state
            .tasks
            .insert(String::from("T1"), task(TaskStatus::Passed, vec![at]));
        let claim = |by_agent, seq| Escalation {
            task: Some(String::from("T1")),
            body: String::from("T1's worker never asked you about divide-by-zero"),
            default_action: String::from("wait"),
            seq,
            by_agent,
        };
        assert!(
            card(&state, &claim(true, 4)).is_none(),
            "the review outranks the opinion"
        );
        assert!(
            card(&state, &claim(true, 12)).is_some(),
            "a claim after the review is still the person's"
        );
        assert!(
            card(&state, &claim(false, 4)).is_some(),
            "an engine card is a fact"
        );
    }

    #[test]
    fn a_review_kickoff_card_is_retired_by_the_review_starting_again() {
        // SHPR-83: "review kickoff not delivered" kept T1 at Needs you even after the review
        // was started again and passed, so its Accept never showed.
        let mut at = attempt(1);
        at.reviews.push(shprd_core::state::ReviewRequest {
            reviewer: "claude".into(),
            degraded: false,
            note: None,
            seq: 3,
        });
        let mut state = RunState::default();
        state
            .tasks
            .insert(String::from("T1"), task(TaskStatus::Reported, vec![at]));
        let card_at = |seq| Escalation {
            task: Some(String::from("T1")),
            body: String::from("review kickoff not delivered"),
            default_action: String::from("press Retry review"),
            seq,
            by_agent: false,
        };
        // SHPR-86: the body may say what the CLI was waiting on; it retires the same way.
        let named_at = |seq| Escalation {
            body: String::from(
                "review kickoff not delivered: claude is asking to trust the folder",
            ),
            ..card_at(seq)
        };
        assert!(card(&state, &named_at(4)).is_some());
        assert!(card(&state, &named_at(2)).is_none());
        assert!(
            card(&state, &card_at(4)).is_some(),
            "no newer review request: it stands"
        );
        assert!(
            card(&state, &card_at(2)).is_none(),
            "a review started after it retires it"
        );
    }

    #[test]
    fn old_logs_with_the_command_wording_read_as_the_desktop_words() {
        assert_eq!(
            super::plain_body("T1 is ready to land: run `shprd approve T1`"),
            "T1 is ready to land: Review and Accept it in the Tasks pane"
        );
        assert_eq!(
            super::plain_body(
                "T1 is ready to land and its merge policy asks: approve pull request #3 on the forge, or run `shprd approve T1`"
            ),
            "T1 is ready to land and its merge policy asks: Review and Accept it in the Tasks pane, or approve pull request #3 on the forge"
        );
    }

    #[test]
    fn an_unanswered_card_is_drawn_as_raised() {
        let (state, escalation) = spent_run(None);
        let drawn = card(&state, &escalation).expect("the card still needs a person");
        assert_eq!(drawn.body, escalation.body);
        assert!(
            drawn.body.contains("answer the card `review-out-1`"),
            "{}",
            drawn.body
        );
    }

    #[test]
    fn a_card_answered_with_drop_it_names_the_drop_verb_and_not_the_ask() {
        // Bug 1 of gate run #4: the ask is consumed by the answer, so a card
        // that kept pointing at it drew buttons every click of which is refused.
        let (state, escalation) = spent_run(Some("drop it"));
        let drawn = card(&state, &escalation).expect("the person still has to drop it");
        assert!(!drawn.body.contains("answer the card"), "{}", drawn.body);
        assert!(
            drawn.body.contains("you answered `drop it`"),
            "{}",
            drawn.body
        );
        assert!(!drawn.body.contains("shprd "), "{}", drawn.body);
    }

    #[test]
    fn a_card_answered_with_drop_it_goes_once_the_drop_is_recorded() {
        let (mut state, escalation) = spent_run(Some("drop it"));
        // The `shprd drop` verb records a `TaskDropped`, which folds into this.
        state.tasks.get_mut("T1").expect("the task").status = TaskStatus::Dropped;
        assert!(
            card(&state, &escalation).is_none(),
            "the verb the card named has run"
        );
    }

    #[test]
    fn a_card_answered_with_take_it_as_it_is_goes_once_the_override_is_recorded() {
        let (mut state, escalation) = spent_run(Some("take it as it is"));
        let waiting = card(&state, &escalation).expect("the person still has to accept it");
        assert!(!waiting.body.contains("shprd "), "{}", waiting.body);
        state.tasks.get_mut("T1").expect("the task").overrides.push(
            shprd_core::state::OverrideRecord {
                rule: String::from(shprd_core::rules::INTEGRATE_AFTER_FAILED_GATE),
                reason: String::from("the failure is cosmetic"),
                seq: 6,
            },
        );
        assert!(card(&state, &escalation).is_none());
    }

    #[test]
    fn a_card_answered_with_retry_stands_down_at_once() {
        // A retry is the engine's own work: the round it buys starts another
        // attempt, which is what retires the card.
        let (state, escalation) = spent_run(Some("retry with another round"));
        assert!(card(&state, &escalation).is_none());
    }

    #[test]
    fn a_card_a_newer_attempt_answered_is_history() {
        let (mut state, escalation) = spent_run(None);
        state
            .tasks
            .get_mut("T1")
            .expect("the task")
            .attempts
            .push(Attempt { n: 2, ..attempt(2) });
        assert!(card(&state, &escalation).is_none());
    }

    #[test]
    fn a_card_about_work_already_in_is_history() {
        for status in [TaskStatus::Integrated, TaskStatus::Dropped] {
            let (mut state, escalation) = spent_run(None);
            state.tasks.get_mut("T1").expect("the task").status = status;
            assert!(card(&state, &escalation).is_none(), "{status:?}");
        }
    }

    #[test]
    fn a_run_wide_card_is_always_kept() {
        let mut state = RunState::default();
        let escalation = Escalation {
            task: None,
            body: String::from("the orchestrator is waiting on its own prompt"),
            default_action: String::from("answer it in its tab"),
            seq: 1,
            by_agent: false,
        };
        let drawn = card(&state, &escalation).expect("a run's own card is never resolved");
        assert_eq!(drawn.body, escalation.body);
        // It stays even once the run is finished: nothing else could clear it.
        state.finished = true;
        assert!(card(&state, &escalation).is_some());
    }

    #[test]
    fn a_card_about_a_step_that_keeps_failing_leaves_once_the_task_moves_on() {
        // A repeated failure names the attempt it happened in, so the card is
        // about that attempt and not about the task for ever: a new attempt
        // after the person fixed the cause retires it.
        let mut state = RunState::default();
        state.tasks.insert(
            String::from("C2"),
            task(TaskStatus::Queued, vec![attempt(1)]),
        );
        let escalation = Escalation {
            task: Some(String::from("C2")),
            body: String::from(
                "C2: its `start` step has failed the same way 3 passes in a row (attempt 1): \
                 task \"C2\" path \"calc.py\" overlaps task \"C1\"",
            ),
            default_action: String::from("narrow C2's scope"),
            seq: 4,
            by_agent: false,
        };
        assert!(card(&state, &escalation).is_some(), "the card still holds");

        state
            .tasks
            .get_mut("C2")
            .expect("the task")
            .attempts
            .push(Attempt { n: 2, ..attempt(2) });
        assert!(
            card(&state, &escalation).is_none(),
            "the attempt the card was about is behind the task"
        );
    }

    #[test]
    fn a_send_back_card_is_not_treated_as_a_spent_review() {
        // A send-back that did not start offers no choices and has no ask, so
        // the answer rule never applies to it: it leaves once a newer attempt
        // exists, and not before.
        let mut state = RunState::default();
        state.tasks.insert(
            String::from("T1"),
            task(TaskStatus::Failed, vec![attempt(1)]),
        );
        let escalation = Escalation {
            task: Some(String::from("T1")),
            body: format!(
                "{}no workspace \"shprd · repo\" yet",
                gate::send_back_failure_prefix("T1", 1)
            ),
            default_action: String::from("spawn it by hand"),
            seq: 9,
            by_agent: false,
        };
        let drawn = card(&state, &escalation).expect("the card still holds");
        assert_eq!(drawn.body, escalation.body);
    }

    #[test]
    fn a_failed_update_card_clears_after_a_later_success_and_returns_after_a_new_failure() {
        use shprd_core::gate::{UPDATE_FAILED_ACTIONS, update_settled_body};
        let mk = |seq: u64, body: String, action: &str| Escalation {
            task: None,
            body,
            default_action: action.to_string(),
            seq,
            by_agent: false,
        };
        let failed = |seq| {
            mk(
                seq,
                "shprd could not build".into(),
                UPDATE_FAILED_ACTIONS[0],
            )
        };
        let ok = |seq| mk(seq, update_settled_body("9.9"), "nothing needs doing");
        let cards = |all: Vec<Escalation>| -> usize {
            let state = RunState {
                escalations: all.clone(),
                ..Default::default()
            };
            all.iter().filter_map(|e| card(&state, e)).count()
        };
        assert_eq!(cards(vec![failed(1)]), 1);
        assert_eq!(cards(vec![failed(1), ok(2)]), 0);
        assert_eq!(cards(vec![failed(1), ok(2), failed(3)]), 1);
    }

    // ---- kind, buttons, key (UXC-1) ----

    use super::{BRIEF_LOST, CLASH_NOTICE_MS, CardKind, SELF_UPDATE_KEY, card_at};

    /// An engine escalation about `T1` with `body` and `default_action`.
    fn raised(seq: u64, body: &str, default_action: &str) -> Escalation {
        Escalation {
            task: Some(String::from("T1")),
            body: body.to_string(),
            default_action: default_action.to_string(),
            seq,
            by_agent: false,
        }
    }

    /// A run holding `T1` in `status` with one attempt spawned at seq 1.
    fn run_with(status: TaskStatus) -> RunState {
        let mut state = RunState::default();
        state
            .tasks
            .insert(String::from("T1"), task(status, vec![attempt(1)]));
        state
    }

    fn ids(card: &super::Card) -> Vec<&str> {
        card.actions
            .iter()
            .map(|action| action.id.as_str())
            .collect()
    }

    #[test]
    fn every_body_of_the_table_has_a_kind_and_buttons_and_an_unknown_one_can_be_asked_about() {
        use gate::{rounds_spent_prefix, send_back_failure_prefix};
        let bodies: Vec<(Escalation, TaskStatus)> = vec![
            (
                raised(5, "T1 is ready to land: Review and Accept it", "wait"),
                TaskStatus::Reported,
            ),
            (
                raised(5, &rounds_spent_prefix("T1", 1, 2), gate::ESCALATION_ACTION),
                TaskStatus::Failed,
            ),
            (
                raised(
                    5,
                    "T1: its `land` step has failed the same way 3 passes in a row (attempt 1): Couldn't merge T1: no remote",
                    "the merge is tried again",
                ),
                TaskStatus::Passed,
            ),
            (
                raised(
                    5,
                    "T1: its `start` step has failed the same way 3 passes in a row (attempt 1): x",
                    "fix it",
                ),
                TaskStatus::Queued,
            ),
            (
                raised(
                    5,
                    "T1: claude didn't start: no tab",
                    "pick the task up by hand",
                ),
                TaskStatus::Failed,
            ),
            (
                raised(
                    5,
                    "T1: the provider is out of credit",
                    "pick the task up by hand",
                ),
                TaskStatus::Working,
            ),
            (
                raised(
                    5,
                    &format!("{}no workspace", send_back_failure_prefix("T1", 1)),
                    "spawn it",
                ),
                TaskStatus::Failed,
            ),
            (
                raised(5, "review kickoff not delivered", "press Retry review"),
                TaskStatus::Reported,
            ),
            (
                raised(
                    5,
                    "T1: the reviewer's findings could not be read: bad json",
                    "decide by hand",
                ),
                TaskStatus::Reported,
            ),
            (raised(5, BRIEF_LOST, "resend"), TaskStatus::Working),
            (
                Escalation {
                    by_agent: true,
                    ..raised(5, "I am stuck on the schema", "wait")
                },
                TaskStatus::Working,
            ),
            (
                raised(5, "something nobody wrote a rule for", "wait"),
                TaskStatus::Working,
            ),
        ];
        for (escalation, status) in bodies {
            let state = run_with(status);
            let card = card_at(&state, &escalation, 0)
                .unwrap_or_else(|| panic!("no card for {:?}", escalation.body));
            assert!(
                !card.actions.is_empty(),
                "a card is never buttonless: {:?}",
                escalation.body
            );
            assert!(!card.key.is_empty());
            // No card offers what the rules refuse in the state it is drawn against.
            if status != TaskStatus::Failed {
                assert!(
                    !ids(&card).contains(&"retry"),
                    "{:?} {status:?}",
                    ids(&card)
                );
            }
            if status != TaskStatus::Reported {
                assert!(!ids(&card).contains(&"retry_review"), "{:?}", ids(&card));
            }
        }
        let state = run_with(TaskStatus::Working);
        let unknown = card_at(&state, &raised(5, "never seen before", "x"), 0).unwrap();
        assert_eq!(unknown.kind, CardKind::Decision);
        assert_eq!(ids(&unknown), ["ask_assistant", "dismiss"]);
        // A run-wide unknown body too.
        let run_wide = Escalation {
            task: None,
            ..raised(5, "never seen before", "x")
        };
        let card = card_at(&RunState::default(), &run_wide, 0).unwrap();
        assert_eq!(ids(&card), ["ask_assistant", "dismiss"]);
    }

    #[test]
    fn a_give_up_card_offers_send_back_only_on_a_failed_task() {
        let give_up = raised(
            5,
            "T1: the provider is out of credit",
            "pick the task up by hand",
        );
        let failed = card_at(&run_with(TaskStatus::Failed), &give_up, 0).unwrap();
        assert_eq!(ids(&failed), ["retry", "drop"]);
        for status in [
            TaskStatus::Working,
            TaskStatus::Reported,
            TaskStatus::Queued,
        ] {
            let card = card_at(&run_with(status), &give_up, 0).unwrap();
            assert_eq!(ids(&card), ["ask_assistant", "drop"], "{status:?}");
        }
        let review = raised(5, "review kickoff not delivered", "press Retry review");
        let reported = card_at(&run_with(TaskStatus::Reported), &review, 0).unwrap();
        assert_eq!(ids(&reported), ["retry_review", "drop"]);
        let failed = card_at(&run_with(TaskStatus::Failed), &review, 0).unwrap();
        assert_eq!(ids(&failed), ["ask_assistant", "drop"]);
    }

    /// The clash card a landing records, as `record_conflict` writes it.
    fn clash(seq: u64) -> Escalation {
        raised(
            seq,
            "T1 clashed with work that landed after it; it is being redone on top of the latest (files in conflict: a.rs, b.rs)",
            "redo the task on top of the latest",
        )
    }

    #[test]
    fn a_clash_is_a_notice_for_ten_minutes_then_a_decision_and_a_new_attempt_retires_it() {
        let mut state = run_with(TaskStatus::Failed);
        state.tasks.get_mut("T1").unwrap().touched_at_ms = 1_000_000;
        let soon = card_at(&state, &clash(6), 1_000_000 + CLASH_NOTICE_MS - 1).unwrap();
        assert_eq!(soon.kind, CardKind::Notice);
        assert_eq!(ids(&soon), ["dismiss"]);
        assert_eq!(
            soon.body,
            "T1 clashed with work that landed after it. shprd is redoing it on top of the latest. Nothing for you to do.\nFiles: a.rs, b.rs"
        );
        assert!(!soon.body.contains('`'));
        let late = card_at(&state, &clash(6), 1_000_000 + CLASH_NOTICE_MS).unwrap();
        assert_eq!(late.kind, CardKind::Decision);
        assert_eq!(ids(&late), ["retry", "drop"]);
        assert!(
            late.body.starts_with(
                "T1 clashed with work that landed after it, and the redo has not started."
            ),
            "{}",
            late.body
        );
        // The redo starting after the card retires it, at either age.
        state.tasks.get_mut("T1").unwrap().attempts.push(Attempt {
            n: 2,
            spawn_seq: Some(9),
            ..attempt(2)
        });
        assert!(card_at(&state, &clash(6), 1_000_100).is_none());
        assert!(card_at(&state, &clash(6), 9_000_000).is_none());
    }

    #[test]
    fn only_the_newest_self_update_card_is_drawn_and_it_holds_no_backtick() {
        let mk = |seq: u64, body: &str, action: &str| Escalation {
            task: None,
            body: body.to_string(),
            default_action: action.to_string(),
            seq,
            by_agent: false,
        };
        let a = mk(
            1,
            "shprd could not update itself to a1. Why: `cargo` failed.",
            gate::UPDATE_FAILED_ACTIONS[0],
        );
        let b = mk(
            2,
            "shprd could not update itself to b2. Why: nope.",
            gate::UPDATE_FAILED_ACTIONS[0],
        );
        let c = mk(
            3,
            "The new shprd did not start.",
            gate::UPDATE_FAILED_ACTIONS[2],
        );
        let state = RunState {
            escalations: vec![a.clone(), b.clone(), c.clone()],
            ..Default::default()
        };
        assert!(card_at(&state, &a, 0).is_none());
        assert!(card_at(&state, &b, 0).is_none());
        let newest = card_at(&state, &c, 0).unwrap();
        assert_eq!(newest.key, SELF_UPDATE_KEY);
        assert_eq!(newest.kind, CardKind::Decision);
        assert_eq!(ids(&newest), ["ask_assistant", "dismiss"]);
        // A failure that leaves shprd as it was is a notice with only Dismiss.
        let state = RunState {
            escalations: vec![a.clone()],
            ..Default::default()
        };
        let only = card_at(&state, &a, 0).unwrap();
        assert_eq!(only.kind, CardKind::Notice);
        assert_eq!(ids(&only), ["dismiss"]);
        assert!(!only.body.contains('`'), "{}", only.body);
    }

    #[test]
    fn an_engine_card_with_no_attempt_named_retires_when_an_attempt_starts_after_it() {
        let escalation = raised(
            5,
            "T1: claude didn't start: no tab",
            "pick the task up by hand",
        );
        let mut state = run_with(TaskStatus::Failed);
        assert!(card_at(&state, &escalation, 0).is_some());
        state.tasks.get_mut("T1").unwrap().attempts.push(Attempt {
            n: 2,
            spawn_seq: Some(8),
            ..attempt(2)
        });
        assert!(card_at(&state, &escalation, 0).is_none());
        // Ready to land is about the task's work, not an attempt, so it stays.
        let ready = raised(5, "T1 is ready to land: Review and Accept it", "wait");
        assert!(card_at(&state, &ready, 0).is_some());
    }
}
