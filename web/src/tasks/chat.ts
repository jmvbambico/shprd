// Reading a task's chat: the web's copy of the Herdr mods' `lib/chat.js`.
//
// The service reads the agent's own record and hands back a page and a cursor,
// so the web never parses a transcript: it folds the pages together and draws
// them, with the cards that wait for an answer among them.
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/chat/main.js` (the bubbles, the "show more" folding, the
//   timeline), `core/chat-view.js` (`WORDS`, `folding`)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik

import type { ChatDto, ChatMessageDto, ChatToolDto } from "../gen";
import type { Card } from "./asks";

/** How many messages the view keeps. */
export const KEEP = 200;

/** The fixed words of the timeline. */
export const WORDS = {
  top: "Start of conversation",
  showMore: "Show more",
  showLess: "Show less",
  emptyTitle: "No messages yet",
  emptyTask: "The agent's conversation shows here as it writes it.",
} as const;

/** A reply folds when it would run past this many lines, to its first [`FOLD`]. */
export const FOLD = 20;
/** See [`FOLD`]. */
export const FOLD_TALL = 28;
/** About how many characters fit on a line of a bubble. */
export const LINE_CHARS = 64;

/** An empty chat before the first page. */
export function empty(space: string): ChatDto {
  return { space, program: null, session: null, messages: [], cursor: "", more: false, note: null };
}

/** One message with every field present. */
export function normalize(message: Partial<ChatMessageDto> | null | undefined): ChatMessageDto {
  return {
    role: message?.role === "assistant" ? "assistant" : "user",
    text: String(message?.text ?? ""),
    tools: Array.isArray(message?.tools)
      ? message.tools.map((tool: Partial<ChatToolDto>) => ({
          name: String(tool?.name ?? "tool"),
          summary: String(tool?.summary ?? ""),
        }))
      : [],
    at_ms: typeof message?.at_ms === "number" && Number.isFinite(message.at_ms) ? message.at_ms : null,
  };
}

/**
 * Folds one answer into the chat already held. A page is what came after the
 * cursor, so it is appended; a new session starts over.
 */
export function fold(chat: ChatDto | null, answer: ChatDto): ChatDto {
  const held = chat ?? empty(answer.space);
  const session = answer.session ?? null;
  const restarted = held.session !== null && session !== null && held.session !== session;
  const messages = restarted ? [] : held.messages.slice();
  for (const message of answer.messages ?? []) {
    messages.push(normalize(message));
  }
  return {
    space: answer.space ?? held.space,
    program: answer.program ?? held.program,
    session: session ?? held.session,
    messages: messages.length > KEEP ? messages.slice(messages.length - KEEP) : messages,
    cursor: answer.cursor ?? held.cursor,
    more: Boolean(answer.more),
    note: answer.note ?? null,
  };
}

/** Whether a message came from the person. */
export function mine(message: ChatMessageDto): boolean {
  return message.role === "user";
}

/** One tool call as a row states it: `Bash · shprd tick --once`. */
export function toolLine(tool: ChatToolDto): string {
  const summary = tool.summary.trim();
  return summary === "" ? tool.name : `${tool.name} · ${summary}`;
}

/** The time a message shows, or "" when none was recorded. */
export function when(atMs: number | null): string {
  if (atMs === null || !Number.isFinite(atMs) || atMs <= 0) {
    return "";
  }
  const at = new Date(atMs);
  return `${String(at.getHours()).padStart(2, "0")}:${String(at.getMinutes()).padStart(2, "0")}`;
}

/** What an empty chat says: the service's note wins. */
export function status(chat: ChatDto | null): string | null {
  if ((chat?.messages ?? []).length > 0) {
    return null;
  }
  return chat?.note || WORDS.emptyTask;
}

/** The body `POST /api/chat` takes, or null when nothing was typed. */
export function outgoing(space: string, text: string): { space: string; text: string } | null {
  const words = text.trim();
  return words === "" ? null : { space, text: words };
}

/** About how many lines `text` takes. */
export function lineCount(text: string, width = LINE_CHARS): number {
  return text
    .split("\n")
    .reduce((sum, line) => sum + Math.max(1, Math.ceil(line.length / Math.max(8, width))), 0);
}

/** What a bubble draws: the whole reply, or its first lines and a "Show more". */
export function folding(
  text: string,
  open = false,
  width = LINE_CHARS,
): { text: string; foldable: boolean; folded: boolean } {
  const lines = lineCount(text, width);
  if (open || lines <= FOLD_TALL) {
    return { text, foldable: lines > FOLD_TALL, folded: false };
  }
  const out: string[] = [];
  let room = FOLD;
  for (const line of text.split("\n")) {
    if (room <= 0) {
      break;
    }
    const n = Math.max(1, Math.ceil(line.length / Math.max(8, width)));
    out.push(n > room ? line.slice(0, room * width) : line);
    room -= n;
  }
  return { text: out.join("\n"), foldable: true, folded: true };
}

/** A stable key for a message. */
export function messageKey(message: ChatMessageDto): string {
  return `m:${message.at_ms ?? "-"}:${message.role}:${message.text.slice(0, 24)}`;
}

/** One row of the timeline. */
export type Row =
  | { kind: "top"; key: string }
  | { kind: "message"; key: string; message: ChatMessageDto }
  | { kind: "card"; key: string; card: Card };

/**
 * The timeline: the messages with the cards that wait for an answer among them.
 * A card with a time sits before the first message written after it; the rest end
 * the timeline, where the newest thing is.
 */
export function timeline(chat: ChatDto | null, cards: Card[] = []): Row[] {
  const rows: Row[] = [];
  const messages = chat?.messages ?? [];
  const waiting = cards.slice().sort((a, b) => (a.at ?? Infinity) - (b.at ?? Infinity));
  let next = 0;
  if (!chat?.more && messages.length > 0) {
    rows.push({ kind: "top", key: "top" });
  }
  for (const message of messages) {
    for (;;) {
      const card = waiting[next];
      if (card === undefined || card.at === null || message.at_ms === null || card.at >= message.at_ms) {
        break;
      }
      rows.push({ kind: "card", key: card.id, card });
      next += 1;
    }
    rows.push({ kind: "message", key: messageKey(message), message });
  }
  for (; next < waiting.length; next += 1) {
    const card = waiting[next];
    if (card !== undefined) {
      rows.push({ kind: "card", key: card.id, card });
    }
  }
  return rows;
}
