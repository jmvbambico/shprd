// shprd's UI tokens for the Herdr desktop: one size system, one red/green/amber,
// and the words shprd's own status rules give a task.
//
// Ported from Shepherdr (MIT), whose window uses the same closed system:
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `lib/ui.js` (the size and colour tokens), `lib/dot.js` (the status
//   glyphs) and `mods/tasks/main.js` (`STATUS`'s words)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// The values are Shepherdr's; the status words are shprd's own, because shprd
// derives a task's status from its ledger (`crates/shprd-views/src/derived.rs`)
// and the service hands the eight words to a client as `derived.status`. A mod
// never invents a ninth.
//
// Shared: the source is `mods/lib/tokens.js`; `bin/sync-shared` copies it into
// every mod folder as `lib/tokens.js`, because a Herdr mod can only import files
// inside its own folder. Edit the source, never a copy, then run `sync-shared`.

/** Button heights: 20 only inside a row's second line, 24 in panels, 32 in dialogs. */
export const H = { xs: 20, sm: 24, lg: 32 };

/**
 * The names the shipped roles are called by until the person renames them: the
 * service's own (`builtin_roles` in `shprd-core`), used when a state carries no
 * `role_titles` (an older service, a fixture).
 */
export const ROLE_DEFAULTS = {
  coordinator: "Orchestrator",
  worker: "Worker",
  reviewer: "Reviewer",
};

/**
 * What a role is called on screen: the person's name for it, as `GET /api/state`
 * carries it in `role_titles`, else the built-in title, else the role's key.
 * The key (`worker`) is what settings, labels and routes use and never changes;
 * this is only the word a person reads. An empty name falls back, like the
 * service's own.
 */
export function roleTitle(state, role) {
  const named = state?.role_titles?.[role];
  const text = typeof named === "string" ? named.trim() : "";
  return text || ROLE_DEFAULTS[role] || String(role ?? "");
}

/** The person's own name for a role, or null while it still has its built-in one. */
export function roleRename(state, role) {
  const title = roleTitle(state, role);
  return title === (ROLE_DEFAULTS[role] ?? String(role ?? "")) ? null : title;
}

/** Text sizes: 11 ids and times, 12 second lines and labels, 13 body and titles, 15 page titles. */
export const TEXT = { id: 11, label: 12, body: 13, title: 15 };

/** Corner radii: 6 on controls, 8 on cards and hovers, 10 on the panel and a pane, 12 on bubbles. */
export const RADIUS = { control: 6, card: 8, panel: 10, bubble: 12 };

/**
 * The layout's own sizes, in px at Herdr zoom 1. The layout file repeats them;
 * these are what a mod uses to draw inside them without measuring.
 */
export const SPACE = {
  brand: 46,
  sidebar: 391,
  sidebarRow: 28,
  sidebarFooter: 97,
  tabs: 42,
  status: 28,
  panel: 510,
  panelHeader: 42,
  row: 32,
  rowTwo: 44,
  gap: 6,
  cardGap: 8,
  paneHeader: 30,
};

/** The one strong colour a space's avatar takes, light and dark alike. */
export const MARK = ["#22a99a", "#e3814f", "#5b8def", "#8b7cf6", "#3fae6a", "#d9649a"];

/** Whether the app is in its dark appearance; `settings().dark` is the app's own switch. */
const dark_ = (settings) => settings?.dark !== false;

/** *Needs you*, alerts, errors. */
export const red = (settings) => (dark_(settings) ? "#ff6369" : "#e5484d");
/** *Ready*, finished well. */
export const green = (settings) => (dark_(settings) ? "#3dd68c" : "#30a46c");
/** *Working*, and the time left under a working glyph. */
export const amber = (settings) => (dark_(settings) ? "#f5b544" : "#e2a336");
/**
 * *Working*: dagr's working sky blue. Dagr's source (herdr-dagr, src/style.rs,
 * commit 52991f9) has WORKING = xterm-256 colour 81 = #5fd7ff, used as is on the
 * dark theme; the light theme takes the darker sky #0284c7 so it still reads.
 */
export const working = (settings) => (dark_(settings) ? "#5fd7ff" : "#0284c7");
/**
 * *Failed*: dagr's failed red. Dagr's source (src/style.rs) has FAILED = xterm-256
 * colour 167 = #d75f5f, used as is on dark; light takes the darker #c0392b.
 */
export const failed = (settings) => (dark_(settings) ? "#d75f5f" : "#c0392b");
/**
 * What a task waits for a decision or an answer on: the amber *working* had
 * before it turned blue, which is close to dagr's yellow.
 */
export const waiting = (settings) => amber(settings);
/** Text on a filled status colour (a count badge). */
export const onStatus = (settings) => (dark_(settings) ? "#000000" : "#ffffff");

/**
 * The three status colours plus the text colour for a fill, resolved against the
 * app's appearance. A mod draws with `tone(settings()).red` rather than a hex.
 */
export function tone(settings) {
  return {
    red: red(settings),
    green: green(settings),
    amber: amber(settings),
    ...statusColors(settings),
    on: onStatus(settings),
  };
}

/**
 * The colours a task's state is drawn in, one set for every pane (Tasks, Graph,
 * Agents, Spaces) so they cannot drift: done and landed green, working sky blue,
 * failed red, waiting amber. Queued and to do have none and stay grey.
 */
export function statusColors(settings) {
  return {
    done: green(settings),
    working: working(settings),
    failed: failed(settings),
    waiting: waiting(settings),
  };
}

/** The token a status word is drawn in, or undefined for the greys (queued, to do, scheduled). */
export const STATUS_COLOR = {
  working: "working",
  needs_you: "waiting",
  ready: "done",
  merging: "done",
  done: "done",
};

/**
 * The token a task's glyph or word is drawn in. A needs-you task whose attempt
 * failed (`need` stopped or provider, or a failed task) reads `failed`; any other
 * waiting task reads `waiting`.
 */
export function statusColor(task) {
  const derived = task?.derived ?? {};
  if (task?.status === "failed" || derived.status === "failed") return "failed";
  if (derived.status === "needs_you" && (derived.need === "stopped" || derived.need === "provider")) return "failed";
  return STATUS_COLOR[derived.status];
}

/**
 * The eight words shprd's rules give a task, keyed by the API's
 * `derived.status` (serde `snake_case`), each with the tone it is drawn in and
 * the glyph a row puts beside it.
 *
 * The glyphs are Shepherdr's shapes; `pie` takes the progress a working task
 * reports, and a row draws it as a wedge, so a state change never moves the row.
 */
export const STATUS = {
  to_do: { word: "To do", tone: "dim", glyph: "ring" },
  scheduled: { word: "Scheduled", tone: "dim", glyph: "calendar" },
  queued: { word: "Queued", tone: "dim", glyph: "clock" },
  working: { word: "Working", tone: "amber", glyph: "pie" },
  needs_you: { word: "Needs you", tone: "red", glyph: "dot" },
  ready: { word: "Ready", tone: "green", glyph: "tick" },
  merging: { word: "Merging", tone: "green", glyph: "spin" },
  done: { word: "Done", tone: "dim", glyph: "tick" },
};

/**
 * The same eight, in the order a panel groups them: what needs the person first,
 * then what moves, then what waits, then what is over.
 */
export const STATUS_ORDER = [
  "needs_you",
  "ready",
  "working",
  "merging",
  "queued",
  "scheduled",
  "to_do",
  "done",
];

/** One status, or `to_do` for a word this build does not know. */
export function status(name) {
  const found = STATUS[name] ?? STATUS.to_do;
  return STATUS_COLOR[name] ? { ...found, color: STATUS_COLOR[name] } : found;
}

/**
 * Why a task reads *Needs you*, keyed by the API's `derived.need`
 * (`NeedKindDto`). It is the row's second line when the card has no better one.
 */
export const NEED = {
  attention: "Only you can settle this",
  asks: "Asks you something",
  reply: "Waits for your reply",
  stopped: "Agent stopped",
  provider: "Its provider cut it off",
};

/**
 * What a needs-you task waits for, in the board's lower-case status style. One
 * helper, so the Graph, the Tasks list and the toasts never disagree.
 *
 * "needs you" is only for what For you shows (`for_you` not false: the
 * integration PR, or a task touching an undeclared protected path). Any other
 * waiting task is the coordinator's: a worker's question (`need: asks`) reads
 * "needs answer"; a card with choices (retry, take as is, drop, a proposal)
 * reads "needs decision".
 */
export function waitLabel(task) {
  if (task?.derived?.for_you !== false) return "needs you";
  return task?.derived?.need === "asks" ? "needs answer" : "needs decision";
}

/** The count words a badge shows: *needs you* on the left, *ready* on the right. */
export const COUNT = { needs: "Needs you", ready: "Ready" };

/**
 * A task reference as a person reads it: the id the service hands out, in the
 * app's mono face. Short ids are left alone; a long one is capped so a row keeps
 * its second line.
 */
export function ref(id, max = 14) {
  const text = String(id ?? "");
  return text.length > max ? `${text.slice(0, max - 1)}…` : text;
}

/**
 * What the desktop says for a service's needs-you words. The engine's card for a
 * ready task sends a reader to `shprd approve` or to the forge (words for a
 * terminal); the person here accepts in the Tasks pane and types no command, so
 * those cards read as a pointer to it. Any other card is left as it is.
 */
export function desktopWords(body, taskId = "") {
  const text = String(body ?? "");
  if (!/shprd approve|approve .*(on the forge|pull request)|waiting for your approval/i.test(text)) return text;
  return `${taskId ? `${taskId} is ready. ` : "It is ready. "}Accept it in the Tasks pane.`;
}
