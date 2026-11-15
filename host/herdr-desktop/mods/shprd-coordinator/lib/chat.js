// Reading a chat: the one place a mod turns `GET /api/chat` into the rows a panel
// draws.
//
// The service reads the agent's own record and hands back a page and a cursor
// (`crates/shprd-server/src/chat.rs`, `crates/shprd-agents/src/transcript.rs`),
// so a mod never parses a transcript and never decides what a message is: it
// folds the pages together and draws them. The panel is the coordinator's, and
// M10.5's task thread reads the same pages through the same functions.
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/chat/main.js` (the bubble list, the "show more" folding, the
//   composer and its pending line), `core/chat-reader.js` (`KEEP`, the page of
//   messages a window holds)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// Shared: the source is `mods/lib/chat.js`; `bin/sync-shared` copies it into
// every mod folder as `lib/chat.js`. Edit the source, never a copy.

import { cut } from "./board.js";

/** How many messages a panel keeps, as Shepherdr's reader keeps `KEEP = 2000`
 * records and its chat draws 200-record pages. */
export const KEEP = 200;

/** How many characters one bubble shows before it is folded. */
export const BUBBLE_MAX = 420;

/**
 * How much of a bubble the sidebar's short panel shows.
 *
 * The coordinator's panel is 190 px tall and the side panel's thread is where a
 * long answer is read in full, so a short panel folds a message instead of
 * hiding its end past an edge nobody can scroll to.
 */
export const COMPACT_MAX = 110;

/** The space a panel shows when it was not told: the coordinator's. */
export const COORDINATOR = "coordinator";

/** An empty chat, before the first page arrives. */
export function empty(space = COORDINATOR) {
  return {
    space,
    program: null,
    session: null,
    messages: [],
    cursor: "",
    more: false,
    note: null,
  };
}

/**
 * Fold one answer into the chat the panel already holds.
 *
 * A page is *what came after the cursor*, so it is appended. The one exception
 * is a new session — the agent restarted, or the space was handed to another
 * program — where the panel starts over rather than gluing two conversations
 * together. The list is trimmed from the front to [`KEEP`], because a panel
 * that grew for ever would cost a render its time budget.
 */
export function fold(chat, answer) {
  const held = chat ?? empty(answer?.space ?? COORDINATOR);
  const session = answer?.session ?? null;
  const restarted = held.session !== null && session !== null && held.session !== session;
  const messages = restarted ? [] : held.messages.slice();
  for (const message of answer?.messages ?? []) messages.push(normalize(message));
  const kept = messages.length > KEEP ? messages.slice(messages.length - KEEP) : messages;
  return {
    space: answer?.space ?? held.space,
    program: answer?.program ?? held.program,
    session: session ?? held.session,
    messages: kept,
    cursor: answer?.cursor ?? held.cursor,
    more: Boolean(answer?.more),
    note: answer?.note ?? null,
  };
}

/** One message of an answer, with the fields a row draws always present. */
export function normalize(message) {
  return {
    role: message?.role === "assistant" ? "assistant" : "user",
    text: String(message?.text ?? ""),
    tools: Array.isArray(message?.tools)
      ? message.tools.map((tool) => ({
          name: String(tool?.name ?? "tool"),
          summary: String(tool?.summary ?? ""),
        }))
      : [],
    at_ms: Number.isFinite(message?.at_ms) ? message.at_ms : null,
  };
}

/** The last `count` messages, oldest first: what a short panel can draw. */
export function latest(chat, count) {
  const messages = chat?.messages ?? [];
  return messages.slice(Math.max(0, messages.length - count));
}

/** Whether a message came from the person, which is what decides its side. */
export function mine(message) {
  return message?.role === "user";
}

/** One tool call as a row states it: `Bash · shprd tick --once`. */
export function toolLine(tool) {
  const name = String(tool?.name ?? "tool");
  const summary = String(tool?.summary ?? "").trim();
  return summary === "" ? name : `${name} · ${summary}`;
}

/** How much of a bubble is drawn: the words up to [`BUBBLE_MAX`]. */
export function bubble(text, max = BUBBLE_MAX) {
  const words = String(text ?? "").trim();
  if (words === "") return "";
  if (words.length <= max) return words;
  return `${words.slice(0, max - 1)}…`;
}

/**
 * What a chat with nothing to draw says, in the panel and not in a tooltip.
 *
 * The service's own `note` wins when it sent one ("claude has not written a
 * session for /work/repo yet"), because it knows which program and directory it
 * looked at. A page that came back empty without a note is a conversation with
 * nothing new in it, which is not a failure and should not read as one. A look
 * that finds nothing new while the panel already shows the conversation says
 * nothing at all: a permanent "nothing new" under the messages is noise, not
 * news.
 */
export function status(chat) {
  if ((chat?.messages ?? []).length > 0) return null;
  return chat?.note || "Nothing yet.";
}

/**
 * The body `POST /api/chat` takes, or `null` when the person has typed nothing
 * worth sending.
 *
 * The words are trimmed and never sent empty: an empty turn is a submit of a
 * key that was pressed twice, not a message.
 */
export function outgoing(space, text) {
  const words = String(text ?? "").trim();
  if (words === "") return null;
  return { space: space || COORDINATOR, text: words };
}

/** The time a message's row shows, or `""` when the program recorded none. */
export function when(atMs) {
  if (!Number.isFinite(atMs) || atMs <= 0) return "";
  const at = new Date(atMs);
  const hours = String(at.getHours()).padStart(2, "0");
  const minutes = String(at.getMinutes()).padStart(2, "0");
  return `${hours}:${minutes}`;
}

/** One line for the panel's header: which program and session the page is from. */
export function header(chat) {
  if (!chat?.program) return "Coordinator";
  if (!chat?.session) return `${chat.program} · no session yet`;
  return `${chat.program} · ${cut(chat.session, 24)}`;
}

/** The cursor the next request asks from. */
export function nextCursor(chat) {
  return chat?.cursor ?? "";
}
