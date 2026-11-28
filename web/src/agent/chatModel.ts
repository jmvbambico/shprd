// Reading a chat: the pure half of the chat screen.
//
// The service reads the agent's own record and answers a page and a cursor
// (`GET /api/chat`), so the page never parses a transcript: it folds the pages
// together and draws them. These are the same rules, in the same words, as the
// Herdr window's chat (`host/herdr-desktop/mods/lib/chat.js`, ported here; the
// original of those is Shepherdr's `mods/chat/main.js` and `core/chat-view.js`,
// MIT, commit a3f327a, see THIRD_PARTY_NOTICES.md).

import type { Card } from "../tasks/asks";
import type { ChatDto, ChatMessageDto, ChatSendDto } from "../gen";

/** How many messages a screen keeps. */
export const KEEP = 200;

/** The space shown when none was picked: the coordinator's. */
export const COORDINATOR = "coordinator";

/** Shepherdr's fixed words for the timeline. */
export const WORDS = {
  top: "Start of conversation",
  showMore: "Show more",
  showLess: "Show less",
  emptyTitle: "No messages yet",
  emptyTask: "The agent's conversation shows here as it writes it.",
  emptyCoordinator: "The coordinator plans the work and keeps you posted.",
} as const;

/** A reply folds when it would run past this many lines, to the first [`FOLD`]. */
export const FOLD = 20;
/** See [`FOLD`]. */
export const FOLD_TALL = 28;
/** About how many characters fit on a line, for the line estimate. */
export const LINE_CHARS = 64;

/** What the screen holds of one space's conversation. */
export interface Chat {
  space: string;
  program: string | null;
  session: string | null;
  messages: ChatMessageDto[];
  cursor: string;
  more: boolean;
  /** Where to ask for earlier history, from the first (newest) read. */
  older: string | null;
  note: string | null;
}

/** An empty chat, before the first page arrives. */
export function empty(space: string = COORDINATOR): Chat {
  return { space, program: null, session: null, messages: [], cursor: "", more: false, older: null, note: null };
}

/**
 * Folds one answer into the chat already held.
 *
 * A page is what came after the cursor, so it is appended; a new session (the
 * agent restarted) starts the list over rather than gluing two conversations
 * together; the list is trimmed from the front to [`KEEP`].
 */
export function fold(chat: Chat | null, answer: ChatDto): Chat {
  const held = chat ?? empty(answer.space);
  const restarted = held.session !== null && answer.session !== null && held.session !== answer.session;
  const messages = restarted ? [] : held.messages.slice();
  for (const message of answer.messages) {
    messages.push(normalize(message));
  }
  return {
    space: answer.space,
    program: answer.program ?? held.program,
    session: answer.session ?? held.session,
    messages: messages.length > KEEP ? messages.slice(messages.length - KEEP) : messages,
    cursor: answer.cursor,
    more: answer.more,
    older: held.session === null || restarted || held.cursor === "" ? answer.older : held.older,
    note: answer.note,
  };
}

/** One message with the fields a row draws always present. */
export function normalize(message: ChatMessageDto): ChatMessageDto {
  return {
    role: message.role === "assistant" || message.role === "news" ? message.role : "user",
    text: String(message.text ?? ""),
    tools: Array.isArray(message.tools)
      ? message.tools.map((tool) => ({ name: String(tool.name ?? "tool"), summary: String(tool.summary ?? "") }))
      : [],
    at_ms: Number.isFinite(message.at_ms) ? message.at_ms : null,
  } as ChatMessageDto;
}

/** Whether a message came from the person, which decides its side. */
export function mine(message: ChatMessageDto): boolean {
  return message.role === "user";
}

/** One tool call as a row states it: `Bash · shprd tick --once`. */
export function toolLine(tool: { name: string; summary: string }): string {
  const summary = tool.summary.trim();
  return summary === "" ? tool.name : `${tool.name} · ${summary}`;
}

/** The time a message's row shows, or `""` when the program recorded none. */
export function when(atMs: number | null | undefined): string {
  if (atMs === null || atMs === undefined || !Number.isFinite(atMs) || atMs <= 0) {
    return "";
  }
  const at = new Date(atMs);
  const hours = String(at.getHours()).padStart(2, "0");
  const minutes = String(at.getMinutes()).padStart(2, "0");
  return `${hours}:${minutes}`;
}

/** The words an empty chat shows, by the space it is for. */
export function emptyWords(space: string): string {
  return space === COORDINATOR ? WORDS.emptyCoordinator : WORDS.emptyTask;
}

/** The body `POST /api/chat` takes, or `null` when nothing worth sending was typed. */
export function outgoing(space: string, text: string): ChatSendDto | null {
  const words = text.trim();
  return words === "" ? null : { space: space || COORDINATOR, text: words };
}

/** A stable key for a message, for its fold state and its row. */
export function messageKey(message: ChatMessageDto): string {
  return `m:${message.at_ms ?? "-"}:${message.role}:${message.text.slice(0, 24)}`;
}

/** About how many lines `text` takes: each line wraps at `width`. */
export function lineCount(text: string, width = LINE_CHARS): number {
  return text
    .split("\n")
    .reduce((sum, line) => sum + Math.max(1, Math.ceil(line.length / Math.max(8, width))), 0);
}

/** What a bubble draws: the whole reply, or its first lines and a "Show more". */
export function folding(text: string, open = false, width = LINE_CHARS): { text: string; foldable: boolean; folded: boolean } {
  const total = lineCount(text, width);
  if (open || total <= FOLD_TALL) {
    return { text, foldable: total > FOLD_TALL, folded: false };
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
export function timeline(chat: Chat | null, cards: Card[] = []): Row[] {
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
