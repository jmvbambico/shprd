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

/** The words a card's heading carries for the kind it is. */
export const KIND = {
  question: "Question",
  permission: "Permission",
  plan: "Plan",
  escalation: "Needs a decision",
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

/** One card from one open ask. */
function fromAsk(ask) {
  const options = (ask.options ?? []).map((option) => ({ label: option, answer: option }));
  return {
    id: `ask:${ask.task}:${ask.id}`,
    from: "ask",
    task: ask.task ?? null,
    ask: ask.id ?? null,
    kind: ask.kind ?? "question",
    title: KIND[ask.kind] ?? KIND.question,
    text: ask.text ?? "",
    options,
    freeText: freeText(ask.kind, ask.options),
    verb: null,
    action: null,
    default_action: null,
    at: Number.isFinite(ask.created_at_ms) ? ask.created_at_ms : null,
    expires: Number.isFinite(ask.expires_at_ms) ? ask.expires_at_ms : null,
  };
}

/** One card from one escalation in the snapshot's `needs`. */
function fromNeed(need) {
  const command = verbOf(need.body);
  return {
    id: `need:${need.seq}`,
    from: "escalation",
    task: need.task ?? null,
    ask: null,
    kind: "escalation",
    title: KIND.escalation,
    text: need.body ?? "",
    options: [],
    freeText: false,
    verb: command,
    action: actionFor(command),
    default_action: need.default_action ?? null,
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

/**
 * Every card the panel draws, in the order it draws them: the asks that can be
 * answered now, oldest first, then the escalations, oldest first.
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
  return [...open, ...needs];
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
export function answerBody(card, answer) {
  if (!card?.ask || !card?.task) return null;
  const text = String(answer ?? "").trim();
  if (text === "") return null;
  return { task: card.task, ask: card.ask, answer: text };
}

/**
 * The task a card is about, as a row reference, or "" for a run-wide card.
 *
 * A card about no task is the run's own ("no run started"), and a panel that drew
 * a blank reference beside it would look like a missing id.
 */
export function taskRef(card) {
  return card?.task ?? "";
}

/**
 * What a card says under its words: the default action of an escalation, or why
 * it is here.
 *
 * An escalation carries the action taken if nobody answers, which is a refusal's
 * half of the decision; an ask carries its expiry instead, because a hook that
 * times out leaves a card that can no longer be answered.
 */
export function footer(card, now = Date.now()) {
  if (card?.from === "escalation") return card.default_action ?? "";
  return expiry(card?.expires, now);
}
