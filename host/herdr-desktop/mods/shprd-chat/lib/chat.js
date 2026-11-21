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
import { spaceIndex } from "./spaces.js";

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

/**
 * Shepherdr's column width, which now only sizes a message bubble: rows fill the
 * pane, [`GUTTER`] px in from each edge (SHPR-80). The shprd layout's centre pane
 * is wider than Shepherdr's (about 1060 pt against 865 in a 1919 pt window), so a
 * capped, centred column left about 120 px of margin each side where Shepherdr
 * leaves 36; filling the pane gives the same 36 at any width.
 */
export const COLUMN = 760;
/** The room a row keeps at each side of the pane: Shepherdr's measured 36 px from the edge to a message. */
export const GUTTER = 36;
/** The widest a message bubble is at a pane width: 80% of the column or the pane. */
export function bubbleWidth(width) {
  return Math.max(240, Math.floor(Math.min(Number(width) || 0, COLUMN) * 0.8));
}

/**
 * The width a message's bubble is given: its longest line at about 8 px a
 * character plus the bubble's padding, never past [`bubbleWidth`] and never
 * under 120. A bubble sized by its content alone collapsed to one letter per
 * row when its text was shorter than the column (SHPR-79 e2e run: "T1 landed.
 * T2 and T3 can now start" drawn as a strip), because the markdown view
 * measures a shrink-wrapped parent at its narrowest; a definite width cannot.
 */
export function bubbleSpan(text, paneWidth) {
  const longest = String(text ?? "")
    .split("\n")
    .reduce((most, line) => Math.max(most, line.length), 0);
  return Math.min(bubbleWidth(paneWidth), Math.max(120, Math.ceil(longest * 8 + 30)));
}

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
 * together (unless `keep` names why the person restarted it: then the old
 * messages stay, followed by one news line saying `keep`). The list is trimmed from the front to [`KEEP`], because a panel
 * that grew for ever would cost a render its time budget.
 */
export function fold(chat, answer, keep = null) {
  const held = chat ?? empty(answer?.space ?? COORDINATOR);
  const session = answer?.session ?? null;
  const restarted = held.session !== null && session !== null && held.session !== session;
  // A restart the person asked for (the model picker) keeps what was said so far
  // and says why the conversation changed hands, instead of an empty panel.
  const messages = restarted && keep === null ? [] : held.messages.slice();
  if (restarted && keep !== null) {
    messages.push(normalize({ role: "news", text: String(keep), tools: [], at_ms: Date.now() }));
  }
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
    role: message?.role === "assistant" || message?.role === "news" ? message.role : "user",
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

/** What a tool call did, in Shepherdr's words: read, edit, run, search, web, agent, plan or tool. */
export function toolKind(name) {
  const n = String(name ?? "").toLowerCase();
  if (/^(read|view|cat|notebookread)$/.test(n)) return "read";
  if (/^(edit|write|multiedit|notebookedit|apply_patch|patch|str_replace.*)$/.test(n)) return "edit";
  if (/^(bash|shell|exec|exec_command|run|command|bashoutput|local_shell)$/.test(n)) return "run";
  if (/^(grep|glob|ls|find|search|toolsearch)$/.test(n)) return "search";
  if (/^(webfetch|websearch|fetch)$/.test(n)) return "web";
  if (/^(task|agent|subagent)$/.test(n)) return "agent";
  if (/^(todowrite|update_plan|plan|exitplanmode)$/.test(n)) return "plan";
  return "tool";
}

/**
 * A run of tool calls as one folded line (Shepherdr `core/chat-log.js`
 * `summary`): "Ran a command", "Read a file, ran 2 commands".
 */
export function toolsSummary(calls) {
  const n = {};
  for (const call of calls ?? []) {
    const kind = toolKind(call?.name);
    n[kind] = (n[kind] ?? 0) + 1;
  }
  const say = (k, one, many) => (n[k] ? (n[k] === 1 ? one : many.replace("#", n[k])) : null);
  const parts = [
    say("read", "read a file", "read # files"),
    say("edit", "edited a file", "edited # files"),
    say("run", "ran a command", "ran # commands"),
    say("search", "searched", "searched # times"),
    say("web", "looked on the web", "looked on the web # times"),
    say("agent", "asked a helper", "asked # helpers"),
    say("plan", "updated its plan", "updated its plan"),
    say("tool", "used a tool", "used # tools"),
  ].filter(Boolean);
  const line = parts.join(", ");
  return line ? line[0].toUpperCase() + line.slice(1) : "Worked";
}

/**
 * The name a pane's header gives an agent of shprd's own: "shprd · claude",
 * the way Shepherdr labels its Assistant ("Assistant · claude"). The program's
 * own name ("Claude Code") is its first word, lowercased.
 */
export function agentLabel(programName) {
  const word = String(programName ?? "").trim().split(/\s+/)[0].toLowerCase();
  return word === "" ? "shprd" : `shprd · ${word}`;
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
export function outgoing(space, text, files = []) {
  const words = String(text ?? "").trim();
  const attached = (files ?? []).filter((file) => typeof file?.path === "string" && file.path !== "");
  // Files alone are a message: the service names them to the agent.
  if (words === "" && attached.length === 0) return null;
  const body = { space: space || COORDINATOR, text: words };
  if (attached.length > 0) body.attachments = attached.map((file) => ({ path: file.path, name: file.name }));
  return body;
}

/**
 * What a key press in the composer does: `"send"` (Enter, also with Ctrl or Cmd),
 * `"newline"` (Alt+Enter, which the text area would not turn into one itself) or
 * `null` (anything else, Shift+Enter included: the text area makes that a new
 * line). The composer asks this before the text area sees the key, because the
 * text area's own `submit` event is only Ctrl/Cmd+Enter: a plain Enter reached
 * it as a new line (SHPR-84).
 */
export function keyAction(event) {
  const key = String(event?.key ?? "").toLowerCase();
  if (key !== "enter" && key !== "return") return null;
  const m = event?.modifiers ?? {};
  if (m.alt) return "newline";
  if (m.shift) return null;
  return "send";
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
export function header(chat, title = "Coordinator") {
  if (!chat?.program) return title;
  if (!chat?.session) return `${chat.program} · no session yet`;
  return `${chat.program} · ${cut(chat.session, 24)}`;
}

/** The cursor the next request asks from. */
export function nextCursor(chat) {
  return chat?.cursor ?? "";
}

// ---------------------------------------------------------------------------
// The full-size chat (M11 U4): the task thread's Chat view and a pane's Chat
// view. Everything below is pure, so a fixture tests it without a window.

/** Shepherdr's fixed words for the timeline (`core/chat-view.js` `WORDS`). */
export const WORDS = {
  top: "Start of conversation",
  showMore: "Show more",
  showLess: "Show less",
  emptyTitle: "No messages yet",
  emptyTask: "The agent's conversation shows here as it writes it.",
  emptyCoordinator: "The coordinator plans the work and keeps you posted.",
  coordinatorDown:
    "shprd's coordinator isn't running, so a message has nowhere to go. shprd tries to start it again; nothing is sent to any other assistant.",
};

/**
 * What the coordinator's chat says when shprd's own coordinator is not running
 * (`GET /api/state`'s `live_agents` holds none). Only shprd's agents are listed
 * there, so another product's Assistant in the space never counts; the chat
 * never falls through to it. A coordinator whose process is alive reads as up
 * whatever word its state has between two turns (pi reads `done` there): only
 * an ended one (`ended_ms`) is down. `null` when one runs, or when the state does not
 * say (an older service, or nothing read yet).
 */
export function coordinatorDown(space, state) {
  if (space !== COORDINATOR || !Array.isArray(state?.live_agents)) return null;
  const running = state.live_agents.some(
    (agent) => agent.role === "coordinator" && !agent.ended_ms,
  );
  return running ? null : WORDS.coordinatorDown;
}

/**
 * A reply folds when it would run past [`FOLD_TALL`] lines, to its first [`FOLD`]
 * (Shepherdr `mods/chat/main.js:128`, `FOLD = 20, FOLD_TALL = 28`).
 */
export const FOLD = 20;
export const FOLD_TALL = 28;

/** About how many characters fit on a line of a bubble, for the line estimate. */
export const LINE_CHARS = 64;

/** About how many lines `text` takes: each line wraps at [`LINE_CHARS`]. */
export function lineCount(text, width = LINE_CHARS) {
  return String(text ?? "")
    .split("\n")
    .reduce((sum, line) => sum + Math.max(1, Math.ceil(line.length / Math.max(8, width))), 0);
}

/**
 * What a bubble draws: the whole reply, or its first lines and a "Show more".
 *
 * `open` is the person's pick for this message. A reply under [`FOLD_TALL`]
 * lines is never folded, so a short one has no toggle at all.
 */
export function folding(text, open = false, width = LINE_CHARS) {
  const words = String(text ?? "");
  if (open || lineCount(words, width) <= FOLD_TALL) {
    return { text: words, foldable: lineCount(words, width) > FOLD_TALL, folded: false };
  }
  const out = [];
  let room = FOLD;
  for (const line of words.split("\n")) {
    if (room <= 0) break;
    const n = Math.max(1, Math.ceil(line.length / Math.max(8, width)));
    out.push(n > room ? line.slice(0, room * width) : line);
    room -= n;
  }
  return { text: out.join("\n"), foldable: true, folded: true };
}

/**
 * Words as markdown that shows them as written (Shepherdr `core/chat-view.js`
 * `literal`): a `<` or an `&` the author typed is a character, not markup. Code,
 * fenced or inline, is left alone.
 */
export function literal(text) {
  return String(text ?? "")
    .split(/(```[\s\S]*?(?:```|$)|`[^`\n]*`)/)
    .map((part, i) => (i % 2 ? part : part.replace(/</g, "\\<").replace(/&(?=#?\w+;)/g, "\\&")))
    .join("");
}

/**
 * The person's own words: line breaks they typed stay breaks (markdown joins
 * single lines), and markup stays text. Fenced code keeps its own lines.
 */
export function typed(text) {
  return literal(
    String(text ?? "")
      .split(/(```[\s\S]*?(?:```|$))/)
      .map((part, i) => (i % 2 ? part : part.replace(/([^\n])\n(?!\n)/g, "$1  \n")))
      .join(""),
  );
}

/** The markdown a message is drawn from: an agent's is escaped as markup text. */
export function markdownOf(message, text = message?.text) {
  return mine(message) ? typed(text) : literal(text);
}

/** Whether a link a bubble holds may be opened: web pages only. */
export function safeLink(url) {
  return /^https?:\/\/[^\s]+$/i.test(String(url ?? "").trim());
}

/** A stable key for a message, for its fold state and its row. */
export function messageKey(message) {
  return `m:${message?.at_ms ?? "-"}:${message?.role}:${String(message?.text ?? "").slice(0, 24)}`;
}

/**
 * The timeline: the conversation's messages with the cards that are waiting for
 * an answer among them, every row a direct child of the scroll area.
 *
 * A card with a time sits before the first message written after it; a card
 * without one (an escalation) and any card newer than the last message end the
 * timeline, which is where the newest thing is.
 *
 * Rows: `{ kind: "top" | "message" | "tools" | "card", key, ... }`. A `tools` row
 * holds a whole run of tool calls (`calls`), drawn folded as [`toolsSummary`].
 */
export function timeline(chat, cards = []) {
  const rows = [];
  const messages = chat?.messages ?? [];
  const waiting = cards.slice().sort((a, b) => (a.at ?? Infinity) - (b.at ?? Infinity));
  let next = 0;
  if (!chat?.more && messages.length > 0) rows.push({ kind: "top", key: "top" });
  for (const message of messages) {
    while (next < waiting.length && waiting[next].at !== null && message.at_ms !== null && waiting[next].at < message.at_ms) {
      rows.push({ kind: "card", key: waiting[next].id, card: waiting[next] });
      next += 1;
    }
    if (message.text.trim() !== "" || message.tools.length === 0) {
      rows.push({ kind: "message", key: messageKey(message), message: { ...message, tools: [] } });
    }
    if (message.tools.length > 0) {
      // A run of tool calls is one folded row, however many replies it spans: a
      // row of words between two runs ends the first.
      const last = rows[rows.length - 1];
      if (last?.kind === "tools") last.calls.push(...message.tools);
      else rows.push({ kind: "tools", key: `tools:${messageKey(message)}`, calls: message.tools.slice() });
    }
  }
  for (; next < waiting.length; next += 1) {
    rows.push({ kind: "card", key: waiting[next].id, card: waiting[next] });
  }
  return rows;
}

/** The index of the first card row, or -1: what "open at its card" scrolls to. */
export function firstCard(rows) {
  return rows.findIndex((row) => row.kind === "card");
}

/** Which space a chat shows, from where it is mounted. */
export function spaceOf({ props = {}, thread = null } = {}) {
  if (props?.space) return String(props.space);
  if (props?.thread) return thread?.task ? String(thread.task) : null;
  return COORDINATOR;
}

/**
 * The chat space a tab belongs to: a task's id in a task's space, `coordinator`
 * in the repository's own, `null` for any other (a plain shell's space).
 */
export function spaceOfTab(tabId, list = []) {
  const found = (list ?? []).find((space) => (space?.tabs ?? []).some((tab) => tab?.id === tabId));
  if (!found) return null;
  const index = spaceIndex([found]);
  if (index.coordinator) return COORDINATOR;
  const [task] = [...index.tasks.keys()];
  return task ?? null;
}

/** The words an empty chat shows, by the space it is for. */
export function emptyWords(space) {
  return space === COORDINATOR ? WORDS.emptyCoordinator : WORDS.emptyTask;
}

// ---------------------------------------------------------------------------
// The pane's Chat · Terminal switch (Shepherdr `mods/pane-chat/main.js`).

/** The mod a chat pane is. */
export const CHAT_MOD = "shprd-chat";

/**
 * The core label a parked agent carries, naming the chat pane that shows it
 * (Shepherdr's `herdr.chat`; its own word here so the two products never read each
 * other's panes).
 */
export const PARK_LABEL = "shprd.chat";

/** What goes in [`PARK_LABEL`] for a chat pane: its id without a machine. */
export function chatLabel(pane) {
  return pane ? String(pane).split("/").pop() : "";
}

/** The live agent a chat pane shows: the terminal parked behind it. */
export function agentOf(chat, terminals = []) {
  const wanted = chatLabel(chat);
  const found = (terminals ?? []).find(
    (term) => chatLabel(term?.labels?.[PARK_LABEL]) === wanted && wanted !== "" && term?.state === "live",
  );
  return found?.address ?? null;
}

/**
 * What a pane's header offers: whether it is a chat pane, which agent it shows,
 * which space the conversation is, and whether the switch is drawn.
 *
 * An agent's terminal pane switches when the program reports a status (it is an
 * agent) and sits in a space whose chat shprd reads — a task's or the
 * coordinator's. A chat pane switches back while its agent is still running.
 */
export function paneChat({ pane, terminals = [], spaces = [] } = {}) {
  const chatMode = pane?.mod === CHAT_MOD;
  const space = spaceOfTab(pane?.tab, spaces);
  if (chatMode) {
    const agent = agentOf(pane?.pane, terminals);
    return { chatMode, agent, space, canSwitch: agent !== null };
  }
  const agent = pane?.terminal ?? null;
  const reports = pane?.status !== null && pane?.status !== undefined;
  return { chatMode, agent, space, canSwitch: Boolean(agent) && reports && space !== null };
}

/** A chat pane's ✕ asks first, as Shepherdr's does (`core/menus.js` `endAgent`). */
export function endAgent(program) {
  return { ask: `End ${program}?`, cancel: "Cancel", verb: "End agent" };
}
