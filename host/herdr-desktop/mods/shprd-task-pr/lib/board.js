// Reading the board: the one place a mod turns `GET /api/state` into the lines a
// panel draws.
//
// The service folds the ledger and derives every task's word itself
// (`crates/shprd-server/src/snapshot.rs`, `crates/shprd-views/src/derived.rs`), so
// a mod never decides what a task is doing: it shows `derived.status`,
// `derived.need` and `derived.reason` as they arrive. That is the rule
// Shepherdr's window follows too (its `core/tasks.js` derives status from live
// terminals, and the window only renders it), and it is what keeps a card, the
// CLI's `shprd status` and this panel from disagreeing.
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `core/tasks.js` (the row's second line and the reference a person
//   reads), `mods/tasks/main.js` (`STATUS`, the grouping order)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// Shared: the source is `mods/lib/board.js`; `bin/sync-shared` copies it into
// every mod folder as `lib/board.js`. Edit the source, never a copy.

import { NEED, STATUS, STATUS_ORDER, status as statusOf } from "./tokens.js";

/** How many characters a row's second line may hold (Shepherdr `LINE_MAX`). */
export const LINE_MAX = 40;

/** Cuts `text` to `max` characters, ending in an ellipsis when it was cut. */
export function cut(text, max = LINE_MAX) {
  const value = String(text ?? "").trim();
  return value.length > max ? `${value.slice(0, max - 1)}…` : value;
}

/**
 * A task's second line: what it says now.
 *
 * The order is Shepherdr's (`mods/tasks/main.js`, the row's `says`): the card's
 * own reason first, then why it needs the person, then the agent's last progress
 * line, then its title. A row that says nothing at all is not drawn, so this
 * always answers something.
 */
export function secondLine(task) {
  const derived = task?.derived ?? {};
  const said = derived.reason ?? (derived.need ? NEED[derived.need] : null);
  if (said) return cut(said);
  if (task?.last_status?.message) return cut(task.last_status.message);
  return cut(task?.title ?? "");
}

/** Whether a task still wants drawing: a dropped task is history, not work. */
export function isLive(task) {
  return task?.status !== "dropped";
}

/** The counts a badge or the status bar shows, over the live tasks. */
export function counts(state) {
  const tally = { needs: 0, ready: 0, working: 0, done: 0, total: 0, open_asks: 0 };
  for (const task of state?.tasks ?? []) {
    if (!isLive(task)) continue;
    tally.total += 1;
    const word = task?.derived?.status;
    if (word === "needs_you") tally.needs += 1;
    else if (word === "ready") tally.ready += 1;
    else if (word === "working" || word === "merging") tally.working += 1;
    else if (word === "done") tally.done += 1;
  }
  tally.open_asks = (state?.asks ?? []).length;
  return tally;
}

/**
 * Every live task, grouped the way the panel draws them: the eight words in
 * [`STATUS_ORDER`], each group only when it has rows.
 *
 * `grouped` is a list of `{status, word, tasks}`; an empty list is a board with
 * nothing on it, which is a run nobody started rather than a failure.
 */
export function grouped(state) {
  const byWord = new Map(STATUS_ORDER.map((word) => [word, []]));
  for (const task of state?.tasks ?? []) {
    if (!isLive(task)) continue;
    const word = task?.derived?.status;
    if (byWord.has(word)) byWord.get(word).push(task);
  }
  const groups = [];
  for (const word of STATUS_ORDER) {
    const tasks = byWord.get(word) ?? [];
    if (tasks.length === 0) continue;
    groups.push({ status: word, word: STATUS[word].word, tasks });
  }
  return groups;
}

/**
 * The task a panel's shared selection names, from the board it is reading.
 *
 * The board is the only place a task is looked up: a panel shares *which* task it
 * is showing (`shared("tasks.thread")`, Shepherdr's own contract) and never a copy
 * of the task, so two views of one drill-in cannot drift apart. A task that is no
 * longer on the board answers null — it landed or was dropped while a view was
 * open, and that is a calm answer, not a missing one.
 */
export function taskById(state, id) {
  if (!id) return null;
  return (state?.tasks ?? []).find((task) => task.id === id) ?? null;
}

/** One task's status, as the tone and glyph a row draws. */
export function look(task) {
  return statusOf(task?.derived?.status);
}

/**
 * The live tasks per word, in the order a panel groups them: what needs the
 * person first, then what moves, then what waits, then what is over.
 *
 * `counts` answers the four numbers a badge shows; this answers all eight, for a
 * status bar that says how the whole board stands. A word nobody's task is in
 * is left out, so a bar never draws a row of zeroes.
 */
export function statusCounts(state) {
  const byWord = new Map(STATUS_ORDER.map((word) => [word, 0]));
  for (const task of state?.tasks ?? []) {
    if (!isLive(task)) continue;
    const word = task?.derived?.status;
    if (byWord.has(word)) byWord.set(word, byWord.get(word) + 1);
  }
  const rows = [];
  for (const word of STATUS_ORDER) {
    const count = byWord.get(word) ?? 0;
    if (count === 0) continue;
    rows.push({ status: word, word: STATUS[word].word, tone: STATUS[word].tone, count });
  }
  return rows;
}

/**
 * The run's own state as one word: what the status bar says at the left.
 *
 * A service that answered but has no run is not the same as a service that is
 * not there — the bar draws `down` for the second and this for the first.
 */
export function runState(state) {
  if (!state?.run) return "none";
  return state.finished ? "finished" : "running";
}

/** How long ago `at` was, in words a bar can show (`just now`, `4m`, `2h`). */
export function since(at, now = Date.now()) {
  const ms = Number.isFinite(at) ? now - at : 0;
  if (ms < 45_000) return "just now";
  const minutes = Math.floor(ms / 60_000);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  return `${Math.floor(hours / 24)}d ago`;
}

/**
 * The run as the status bar names it: the run's id, else why there is none.
 *
 * `GET /api/state` answers `run: null` with a `hint` before `shprd run start`,
 * and a bar that said "no run" without saying how to start one would be a dead
 * end (the M9 gate's bug 6 was exactly that kind of silence).
 */
export function runLabel(state) {
  const run = state?.run;
  if (!run) return state?.hint ?? "No run yet";
  return `run ${run.id} · ${run.integration_branch}`;
}

/**
 * The run as a *status bar* names it: its id alone, because the branch it lands
 * on is on the sidebar's coordinator row already and the bar has one line for
 * everything.
 *
 * A bar with no run says why, exactly as [`runLabel`] does: a bar that said
 * "no run" without naming the verb would be a dead end.
 */
export function runShort(state) {
  const run = state?.run;
  if (!run) return state?.hint ?? "No run yet";
  return `run ${run.id}`;
}

/** The newest attempt of a task, when it has one. */
export function attempt(task) {
  const attempts = task?.attempts ?? [];
  return attempts.length === 0 ? null : attempts[attempts.length - 1];
}

/** A task's latest review, when one was dispatched. */
export function review(task) {
  const latest = attempt(task);
  const reviews = latest?.reviews ?? [];
  return reviews.length === 0 ? null : reviews[reviews.length - 1];
}

/**
 * The last pass's first failure, one line, or null when the last pass was clean.
 *
 * The service keeps it (`last_tick`) so a step that failed is visible on the
 * board as well as in the service's own output; a panel that hid it would leave
 * a run that looks idle when it is stuck.
 */
export function lastFailure(state) {
  const failed = state?.last_tick?.failed ?? [];
  if (failed.length === 0) return null;
  const first = failed[0];
  const where = first.task ? ` on ${first.task}` : "";
  const others = failed.length - 1;
  return `${first.step}${where}: ${cut(first.error, 80)}${others > 0 ? ` (+${others})` : ""}`;
}

/**
 * The one act a row offers, as `{id, label, action}` — or nothing.
 *
 * The verbs are the engine's, and a row offers one only where that verb would
 * be accepted: a start needs a task the rules call queued, a retry needs one
 * whose attempt failed (a send-back is a spawn, and a spawn is refused while any
 * attempt is unfinished), and a stop needs an agent that is supposed to be
 * running. A ready row's act is not an action at all: it opens the Accept card,
 * so `action` is null and the row knows to drill in.
 *
 * Nothing is offered for a task that is merging, done or dropped: those are the
 * engine's or history's, and a button that always refuses is worse than none.
 */
export function rowActions(task) {
  const word = task?.derived?.status;
  const status = task?.status;
  if (!isLive(task)) return [];
  if (word === "ready" || status === "passed" || status === "reported") {
    return [{ id: "review", label: "Review", action: null }];
  }
  if (status === "queued" && (word === "to_do" || word === "scheduled")) {
    return [{ id: "start", label: "Start", action: "task.start" }];
  }
  if (status === "failed") {
    return [{ id: "retry", label: "Retry", action: "task.retry" }];
  }
  if (status === "working" && word === "working") {
    return [{ id: "stop", label: "Stop", action: "task.stop" }];
  }
  return [];
}

/**
 * The tasks a panel can offer to start, and the ones it must leave alone.
 *
 * It is the add-task form's reading of the board: what the service would accept
 * as a scope (`a path, or a folder`) is not decided here — the rules are — but a
 * title and one acceptance line are what the form asks for, and a form that sent
 * an empty one would be refused for it. `id` also says why a form cannot be
 * submitted yet, in words, or is empty when it can.
 */
export function newTask(body) {
  const title = String(body?.title ?? "").trim();
  const scope = String(body?.scope ?? "")
    .split(",")
    .map((part) => part.trim())
    .filter((part) => part !== "");
  const acceptance = String(body?.acceptance ?? "")
    .split(";")
    .map((part) => part.trim())
    .filter((part) => part !== "");
  const why = title === ""
    ? "A task needs a title"
    : scope.length === 0
      ? "Name the folder or path it may change"
      : acceptance.length === 0
        ? "Say what must be true when it is done"
        : "";
  return { title, scope, acceptance, why, ready: why === "" };
}
