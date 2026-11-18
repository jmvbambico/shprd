// The needs-you cards' reading of the service: one card per open ask and per
// escalation, drawn in the task's chat timeline.
//
// The web's copy of the Herdr mods' `lib/asks.js`. An **ask** is a file a
// program's hook raised (`GET /api/asks`): it has choices and is answered once
// through `POST /api/asks/answer`. An **escalation** is a log line (the board's
// `needs`): the engine stopped and a person has to decide; it names the verb to
// run and offers no button.
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/chat/main.js` (the question, permission and plan cards in the
//   timeline), `core/asks.js` (the queue)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik

import type { AnswerAsk, AskDto, AskKindDto, AskListDto, SnapshotDto } from "../gen";

/** The words a card's heading carries for the kind it is. */
export const KIND: Record<AskKindDto | "escalation", string> = {
  question: "Question",
  permission: "Permission",
  plan: "Plan",
  escalation: "Needs a decision",
};

/** One card. */
export interface Card {
  id: string;
  from: "ask" | "escalation";
  task: string | null;
  ask: string | null;
  kind: AskKindDto | "escalation";
  title: string;
  text: string;
  options: { label: string; answer: string }[];
  freeText: boolean;
  verb: string | null;
  default_action: string | null;
  at: number | null;
  expires: number | null;
}

/** Every backticked word or command in a piece of text, in order. */
export function quoted(text: unknown): string[] {
  return [...String(text ?? "").matchAll(/`([^`]+)`/g)].map((m) => (m[1] ?? "").trim());
}

/** The first `shprd …` command a card's body names, or null. */
export function verbOf(text: unknown): string | null {
  return quoted(text).find((part) => part.split(/\s+/)[0] === "shprd") ?? null;
}

/** Whether a card's answer may be typed: a question always, others only when they offer no choice. */
export function freeText(kind: AskKindDto, options: string[] | undefined): boolean {
  return kind === "question" || (options ?? []).length === 0;
}

/** How long an ask has left, in words, or "" when it does not expire. */
export function expiry(expiresAt: number | null | undefined, now = Date.now()): string {
  if (typeof expiresAt !== "number" || !Number.isFinite(expiresAt)) {
    return "";
  }
  const ms = expiresAt - now;
  if (ms <= 0) {
    return "expired";
  }
  if (ms < 60_000) {
    return "under a minute left";
  }
  const minutes = Math.floor(ms / 60_000);
  return minutes < 60 ? `${minutes}m left` : `${Math.floor(minutes / 60)}h left`;
}

function fromAsk(ask: AskDto): Card {
  return {
    id: `ask:${ask.task}:${ask.id}`,
    from: "ask",
    task: ask.task,
    ask: ask.id,
    kind: ask.kind,
    title: KIND[ask.kind] ?? KIND.question,
    text: ask.text,
    options: ask.options.map((option) => ({ label: option, answer: option })),
    freeText: freeText(ask.kind, ask.options),
    verb: null,
    default_action: null,
    at: Number.isFinite(ask.created_at_ms) ? ask.created_at_ms : null,
    expires: ask.expires_at_ms ?? null,
  };
}

function fromNeed(need: SnapshotDto["needs"][number]): Card {
  return {
    id: `need:${need.seq}`,
    from: "escalation",
    task: need.task ?? null,
    ask: null,
    kind: "escalation",
    title: KIND.escalation,
    text: need.body,
    options: [],
    freeText: false,
    verb: verbOf(need.body),
    default_action: need.default_action ?? null,
    at: null,
    expires: null,
  };
}

/**
 * Every card, asks that can be answered now first (oldest first), then the
 * escalations. An escalation that names an open ask is the same decision as
 * that ask, and the ask is the half that can still be answered.
 */
export function cards(state: SnapshotDto | null, asks: AskListDto | null): Card[] {
  const open = (asks?.asks ?? []).map(fromAsk);
  const ids = new Set(open.map((card) => card.ask));
  const needs = (state?.needs ?? [])
    .filter((need) => !quoted(need.body).some((part) => ids.has(part)))
    .map(fromNeed);
  return [...open, ...needs];
}

/** The body `POST /api/asks/answer` takes, or null when the card cannot be answered or the answer is blank. */
export function answerBody(card: Card, answer: string): AnswerAsk | null {
  if (!card.ask || !card.task) {
    return null;
  }
  const text = answer.trim();
  return text === "" ? null : { task: card.task, ask: card.ask, answer: text };
}

/** What a card says under its words: an escalation's default action, or an ask's expiry. */
export function footer(card: Card, now = Date.now()): string {
  return card.from === "escalation" ? (card.default_action ?? "") : expiry(card.expires, now);
}

/** The cards that belong in one task's timeline. */
export function forTask(list: Card[], task: string): Card[] {
  return list.filter((card) => card.task === task);
}
