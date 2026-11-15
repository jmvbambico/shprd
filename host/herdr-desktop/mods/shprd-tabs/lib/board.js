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

/** One task's status, as the tone and glyph a row draws. */
export function look(task) {
  return statusOf(task?.derived?.status);
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
