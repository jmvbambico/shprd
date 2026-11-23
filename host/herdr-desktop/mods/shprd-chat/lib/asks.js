// The needs-you cards' reading of the service: one card per open ask and per
// escalation, each with the answer it takes.
//
// Shepherdr draws question, permission, plan and secret cards inline in the
// conversation; shprd keeps the same cards in two places, because the run records
// two different things:
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/chat/main.js` (the question, permission, plan and secret cards
//   in the timeline: the question, the numbered options, the free-text answer),
//   `core/asks.js` (the queue and its open/answered split)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// An **ask** is a file a program's hook raised (`GET /api/asks`): it has choices,
// it is answered exactly once through `POST /api/asks/answer`, and the hook that
// wrote it is waiting for that answer.
//
// An **escalation** is a log line (`GET /api/state`'s `needs`): the engine stopped
// and a person has to decide. Some are plain decisions; the ones a spent review
// leaves after its answer name the verb a person still has to run (M10.0,
// `shprd_views::cards`), and this module reads that verb out of the words so a
// card can show it on its own line.
//
// A card a person answers that leaves them a verb is therefore drawn once, from
// its ask, and the escalation behind it is dropped by [`cards`]: two cards for one
// decision would offer an answer the service has already consumed.
//
// Shared: the source is `mods/lib/asks.js`; `bin/sync-shared` copies it into
// every mod folder as `lib/asks.js`. Edit the source, never a copy.

/** The words a card's heading carries: a card the person decides, or one that only tells. */
export const HEADING = {
  decision: "Needs a decision",
  notice: "Notice",
};

/** The words a card's heading carried before every card had a kind (kept for readers of old cards). */
export const KIND = {
  question: HEADING.decision,
  permission: HEADING.decision,
  plan: HEADING.decision,
  escalation: HEADING.decision,
};

/** The two sections a panel draws, in order: what can be answered, then what a person must do. */
export const SECTION = {
  ask: "Waiting for your answer",
  escalation: "Needs a decision",
};

/** Every backticked word or command in a piece of text, in order. */
export function quoted(text) {
  return [...String(text ?? "").matchAll(/`([^`]+)`/g)].map((match) => match[1].trim());
}

/**
 * The commands a card's body names, in order.
 *
 * Cards write a verb as a backticked `shprd …` command ("you answered `drop it`,
 * so run `shprd drop T1 --reason …`"), so a backticked segment that starts with
 * the binary's name is a command and everything else — a choice, an ask id — is
 * not.
 */
export function commands(text) {
  return quoted(text).filter((part) => part.split(/\s+/)[0] === "shprd");
}

/** The first command a card's body names, or null. */
export function verbOf(text) {
  return commands(text)[0] ?? null;
}

/**
 * The service action one command is, when the service has one.
 *
 * `POST /api/action` can record a task and a progress line; everything else a
 * card names — dropping a task, overriding a gate, spawning an attempt — is a
 * person's own verb on the command line, so the card shows it as words and
 * offers no button. A button that did nothing would be worse than the words.
 */
export function actionFor(command) {
  const parts = String(command ?? "").trim().split(/\s+/);
  if (parts[0] !== "shprd" || parts[1] !== "task" || parts[2] !== "status") return null;
  const task = parts[3];
  if (!task || task.startsWith("--")) return null;
  const at = parts.indexOf("--msg");
  if (at === -1 || at + 1 >= parts.length) return null;
  const msg = parts.slice(at + 1).join(" ").replace(/^["']|["']$/g, "");
  if (msg.trim() === "") return null;
  return { kind: "task.status", task, msg };
}

/**
 * Whether a card's answer may be typed as well as clicked.
 *
 * A question always takes words — that is what a question is for — and a
 * permission or a plan takes them only when it offers no choice, because a yes or
 * a no with a free-text box beside it invites an answer the hook cannot read as
 * either.
 */
export function freeText(kind, options) {
  if (kind === "question") return true;
  return (options ?? []).length === 0;
}

/** How long an ask has left, in words, or "" when it does not expire. */
export function expiry(expiresAt, now = Date.now()) {
  if (!Number.isFinite(expiresAt)) return "";
  const ms = expiresAt - now;
  if (ms <= 0) return "expired";
  if (ms < 60_000) return "under a minute left";
  const minutes = Math.floor(ms / 60_000);
  if (minutes < 60) return `${minutes}m left`;
  return `${Math.floor(minutes / 60)}h left`;
}

/** The label a button of each service action id wears (the service's own table). */
export const ACTION_LABEL = {
  review: "Review",
  retry: "Send back",
  retry_review: "Retry review",
  resend_brief: "Resend brief",
  drop: "Drop task\u2026",
  ask_assistant: "Ask the assistant",
  reply: "Reply",
  dismiss: "Dismiss",
};

/** What a button says while its press is in flight, by action id. */
export const ACTION_BUSY = {
  review: "Opening\u2026",
  retry: "Sending back\u2026",
  retry_review: "Retrying review\u2026",
  resend_brief: "Resending\u2026",
  drop: "Dropping\u2026",
  ask_assistant: "Asking\u2026",
  reply: "Replying\u2026",
  dismiss: "Dismissing\u2026",
};

/** The busy word of a button that answers an ask. */
export const ANSWER_BUSY = "Sending\u2026";

/** The words a choice of a scope card begins with (`shprd_core::gate::WIDEN_CHOICE`). */
const WIDEN_CHOICE = "widen the scope to ";
/** The choice that stops the attempt and starts another (`SEND_BACK_CHOICE`). */
const SEND_BACK_CHOICE = "send it back to a new attempt";
/** The choice that leaves a scope as it is (`KEEP_SCOPE_CHOICE`). */
const KEEP_SCOPE_CHOICE = "keep the scope";
/** The ids of the two asks that are one scope question. */
const SCOPE_ASK_PREFIXES = ["scope-widen-", "hand-in-refused-"];

/** The button word of a choice an ask offers: the scope choices read as short verbs, the rest as written. */
export function choiceLabel(option) {
  const text = String(option ?? "").trim();
  if (text.startsWith(WIDEN_CHOICE)) return "Allow these files";
  if (text === SEND_BACK_CHOICE) return "Send back";
  if (text === KEEP_SCOPE_CHOICE) return "Keep the scope";
  return text;
}

/**
 * The key of an ask: cards with one key are one question.
 *
 * A service that sends one is believed; the two scope asks of a task (a refused
 * hand-in's and a widening verb's) share `scope:<task>`, the same key
 * `shprd_core::ask::scope_key` gives them.
 */
export function askKey(ask) {
  if (typeof ask?.key === "string" && ask.key !== "") return ask.key;
  const id = String(ask?.id ?? "");
  if (ask?.task && SCOPE_ASK_PREFIXES.some((prefix) => id.startsWith(prefix))) return `scope:${ask.task}`;
  return `ask:${ask?.task ?? ""}:${id}`;
}

/** One button that answers one ask with one of its choices. */
function answerButton(ask, option) {
  const label = choiceLabel(option);
  return {
    id: `answer:${ask.id}:${option}`,
    label,
    busy: ANSWER_BUSY,
    chose: label,
    kind: "answer",
    task: ask.task ?? null,
    ask: ask.id ?? null,
    answer: option,
  };
}

/** The service action a button of this id sends for `need`, or null when it is a thing the window does. */
function payloadOf(id, need) {
  switch (id) {
    case "retry":
      return need.task ? { kind: "task.retry", task: need.task } : null;
    case "retry_review":
      return need.task ? { kind: "task.review_again", task: need.task } : null;
    case "resend_brief":
      return need.task ? { kind: "task.resend_brief", task: need.task } : null;
    case "drop":
      return need.task ? { kind: "task.drop", task: need.task, reason: "dropped from its card" } : null;
    case "dismiss":
      return { kind: "need.dismiss", seq: need.seq };
    default:
      return null;
  }
}

/** One button for one action id of an escalation. */
function actionButton(id, need) {
  const label = ACTION_LABEL[id] ?? String(id);
  return {
    id: `action:${id}:${need.seq}`,
    label,
    busy: ACTION_BUSY[id] ?? "Working\u2026",
    chose: label.replace(/\u2026$/, ""),
    kind: "action",
    action: id,
    task: need.task ?? null,
    seq: need.seq,
    payload: payloadOf(id, need),
    confirm: id === "drop",
  };
}

/** One card from one open ask. */
function fromAsk(ask) {
  const options = (ask.options ?? []).map((option) => ({ label: choiceLabel(option), answer: option }));
  return {
    id: `ask:${ask.task}:${ask.id}`,
    key: askKey(ask),
    from: "ask",
    task: ask.task ?? null,
    ask: ask.id ?? null,
    asks: [ask.id].filter(Boolean),
    kind: ask.kind ?? "question",
    notice: false,
    title: HEADING.decision,
    text: ask.text ?? "",
    options,
    buttons: (ask.options ?? []).map((option) => answerButton(ask, option)),
    freeText: freeText(ask.kind, ask.options),
    verb: null,
    action: null,
    actionLabel: "Run",
    default_action: null,
    ifNothing: "",
    at: Number.isFinite(ask.created_at_ms) ? ask.created_at_ms : null,
    expires: Number.isFinite(ask.expires_at_ms) ? ask.expires_at_ms : null,
  };
}

/** The body a spawn records when the brief never reached the agent. */
export const BRIEF_LOST = "brief not delivered";

/**
 * What a card says it will do if nobody answers, or "" when it should say nothing.
 *
 * A default that names a command (a backtick in it) is a thing the person has to do,
 * and the card has a button for that; only a plain default is worth a line.
 */
export function ifNothing(defaultAction) {
  const text = String(defaultAction ?? "").trim();
  if (text === "" || text.includes("`")) return "";
  return `If you do nothing: ${text}`;
}

/** One card from one escalation in the snapshot's `needs`. */
function fromNeed(need) {
  const command = verbOf(need.body);
  // A service that sends buttons is believed; one that does not (an old service) gets
  // the single button the card has always had.
  const classified = Array.isArray(need.actions) && need.actions.length > 0;
  const notice = need.kind === "notice";
  // A lost brief has a button: the service types it into the agent again, and the
  // card goes once that worked.
  const resend = need.body === BRIEF_LOST && need.task;
  const action = resend ? { kind: "task.resend_brief", task: need.task } : actionFor(command);
  const actionLabel = resend ? "Resend" : "Run";
  const buttons = classified
    ? need.actions.map((id) => actionButton(id, need))
    : action
      ? [{ id: `action:run:${need.seq}`, label: actionLabel, busy: "Running\u2026", chose: actionLabel, kind: "action", action: "run", task: need.task ?? null, seq: need.seq, payload: action, confirm: false }]
      : [];
  return {
    id: `need:${need.seq}`,
    key: typeof need.key === "string" && need.key !== "" ? need.key : `need:${need.seq}`,
    seq: need.seq,
    from: "escalation",
    task: need.task ?? null,
    ask: null,
    asks: [],
    kind: "escalation",
    notice,
    title: notice ? HEADING.notice : HEADING.decision,
    text: need.body ?? "",
    options: [],
    buttons,
    freeText: false,
    // A card with buttons of its own does not also print the command it would have named.
    verb: classified ? null : command,
    action,
    actionLabel,
    default_action: need.default_action ?? null,
    ifNothing: notice ? "" : ifNothing(need.default_action),
    at: null,
    expires: null,
  };
}

/**
 * Whether an escalation names an ask that is still open.
 *
 * A spent review's card repeats the ask id it offers ("answer the card `…`"), so
 * a card whose body names one of the open asks is the same decision as that ask —
 * and the ask is the half that can still be answered.
 */
export function namesOpenAsk(body, ids) {
  return quoted(body).some((part) => ids.has(part));
}

/** One card out of two that share a key: their buttons are the union, their words each once. */
function merge(first, next) {
  const buttons = [...first.buttons];
  for (const button of next.buttons) {
    if (!buttons.some((have) => have.label === button.label)) buttons.push(button);
  }
  const text = first.text.includes(next.text) || next.text === "" ? first.text : `${first.text}\n\n${next.text}`;
  const lead = first.from === "ask" ? first : next.from === "ask" ? next : first;
  return {
    ...first,
    from: lead.from,
    ask: first.ask ?? next.ask,
    asks: [...first.asks, ...next.asks],
    options: [...first.options, ...next.options.filter((option) => !first.options.some((have) => have.label === option.label))],
    buttons,
    text,
    notice: first.notice && next.notice,
    title: first.notice && next.notice ? HEADING.notice : HEADING.decision,
    freeText: first.freeText || next.freeText,
    ifNothing: first.ifNothing || next.ifNothing,
    default_action: first.default_action ?? next.default_action,
    verb: first.verb ?? next.verb,
    at: first.at ?? next.at,
    expires: first.expires ?? next.expires,
  };
}

/**
 * Every card the panel draws, in the order it draws them: the asks that can be
 * answered now, oldest first, then the escalations, oldest first. Cards that share
 * a key are one card (the first one's place), with the union of their buttons.
 *
 * `asks` is `GET /api/asks`, `state` is `GET /api/state`; either may be null while
 * the service has not answered yet, which is a panel with nothing to show rather
 * than a failure.
 */
export function cards(state, asks) {
  const open = (asks?.asks ?? []).map(fromAsk);
  const ids = new Set(open.map((card) => card.ask));
  const needs = (state?.needs ?? [])
    .filter((need) => !namesOpenAsk(need.body, ids))
    .map(fromNeed);
  const out = [];
  for (const card of [...open, ...needs]) {
    const at = out.findIndex((have) => have.key === card.key);
    if (at === -1) out.push(card);
    else out[at] = merge(out[at], card);
  }
  return out;
}

/** The cards of one section, in the order [`cards`] put them. */
export function section(cards, from) {
  return (cards ?? []).filter((card) => card.from === from);
}

/** How many cards of each kind there are, for a heading or a badge. */
export function tally(cards) {
  const counted = { ask: 0, escalation: 0, total: 0 };
  for (const card of cards ?? []) {
    counted[card.from] = (counted[card.from] ?? 0) + 1;
    counted.total += 1;
  }
  return counted;
}

/**
 * The body `POST /api/asks/answer` takes for one card and one answer, or null
 * when the card cannot be answered (an escalation) or the answer is blank.
 */
export function answerBody(card, answer, ask = null) {
  const id = ask ?? card?.ask;
  if (!id || !card?.task) return null;
  const text = String(answer ?? "").trim();
  if (text === "") return null;
  return { task: card.task, ask: id, answer: text };
}

/**
 * What a card says under its words: "If you do nothing: …" for a decision whose
 * default is a plain sentence, and nothing otherwise (no grey default, no countdown).
 */
export function footer(card) {
  return card?.ifNothing ?? "";
}

/**
 * The cards that belong in one chat's timeline (Shepherdr draws a task's cards in
 * that task's thread): a task's own, or — for the coordinator's chat — the run's,
 * the ones with no task. The Asks panel is gone, so every card has to land in
 * exactly one of these.
 */
export function forSpace(list, space) {
  const wanted = space === "coordinator" ? null : space;
  return (list ?? []).filter((card) => (card.task ?? null) === wanted);
}
