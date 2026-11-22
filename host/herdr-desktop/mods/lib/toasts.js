// Toasts: what shprd has to tell a person who is looking somewhere else, and how
// many of them a window shows at once.
//
// The numbers are Shepherdr's (`core/toasts.js`): three at a time, 360 px wide, a
// body cut at 140 characters, a dot in the level's colour, and a timeout per
// level — 20 s for an alert, 12 s for an error, 10 s for a warning, 6 s for
// everything else. Ported with the rules, because a person who used Shepherdr
// should not have to relearn when a toast goes.
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `core/toasts.js` (`SHOWN`, `WIDTH`, `BODY_MAX`, `TIMEOUT`, `tone`,
//   `clip`, `withToast`, `unexpired`), `mods/toasts/main.js` (the float)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// What is shprd's own: the news is the *log's*, so every toast carries the seq of
// the event that made it and is deduped by it. A batch replayed after a service
// restart, or a long poll that answers twice with the same event, adds nothing:
// one event, one toast, however many times a client reads it. The snapshot is
// the join for the words — an event notice carries no payload, so the finding a
// review failed on, or the body of a needs-you card, is read from
// `GET /api/state` beside it.
//
// Pure: events and a snapshot in, items out. The mod draws them and waits for
// more; the unit test drives both with fixtures from a real service.
//
// Shared: the source is `mods/lib/toasts.js`; `bin/sync-shared` copies it into
// every mod folder that imports it. Edit the source, never a copy.

import { desktopWords, roleRename } from "./tokens.js";

/** How many toasts are on screen at once (`core/toasts.js:11`). */
export const SHOWN = 3;
/** How wide the float is, px (`core/toasts.js:12`). */
export const WIDTH = 360;
/** How many lines a body may wrap to (`core/toasts.js:13`). */
export const LINES = 3;
/** How many characters a body is cut at: three lines at that width. */
export const BODY_MAX = 140;
/** How long each level stays on screen, ms (`core/toasts.js:17`). */
export const TIMEOUT = { alert: 20_000, error: 12_000, warning: 10_000 };
/** How long a level with no timeout of its own stays, ms. */
export const TIMEOUT_DEFAULT = 6_000;
/** How many are kept besides the newest one (`core/toasts.js:20`). */
export const KEEP = 4;
/** How long one is kept in the list at all, ms (`core/toasts.js:19`). */
export const KEEP_MS = 120_000;

/** How long `item` stays on screen. */
export function timeoutOf(item) {
  return item?.timeout ?? TIMEOUT[item?.level] ?? TIMEOUT_DEFAULT;
}

/**
 * The colour a level is drawn in: red for an alert or an error, amber for a
 * warning, green for something that finished well, the accent otherwise.
 */
export function levelTone(level) {
  if (level === "alert" || level === "error") return "red";
  if (level === "warning") return "amber";
  if (level === "success") return "green";
  return "accent";
}

/** `text` cut to `max` characters, ending in an ellipsis when it was cut. */
export function clip(text, max = BODY_MAX) {
  const value = String(text ?? "").trim();
  return value.length > max ? `${value.slice(0, max - 1).trimEnd()}…` : value;
}

/** The change kinds that are news, and the level each one is told at. */
export const NEWS = {
  task_landed: "success",
  escalated: "warning",
  ask_raised: "warning",
  gate_failed: "error",
};

/**
 * The id a toast carries for one event: its seq, which is what dedupes it.
 *
 * A pass is not an event, so a failed pass is filed under its own timestamp,
 * which is the seq a reader of `last_tick` has for it.
 */
export function eventId(seq) {
  return `ev:${seq}`;
}

/** The id of the "a pass failed" toast. */
export function passId(at) {
  return `tick:${at}`;
}

/**
 * Whether the snapshot says the task is over, so a card about it is history.
 *
 * shprd's status rules read a finished task as Done and say nothing it still
 * needs (`shprd-views/src/derived.rs`), so a needs-you toast for one would be a
 * card the board deliberately does not show: one event, two answers, and the
 * person sent to look at a row that is finished.
 */
function finished(state, taskId) {
  const task = (state?.tasks ?? []).find((entry) => entry?.id === taskId);
  return task?.status === "integrated" || task?.status === "dropped";
}

/** The integration branch a landed task went into, as a person reads it. */
function landedBody(state, taskId) {
  const task = (state?.tasks ?? []).find((entry) => entry?.id === taskId);
  const branch = state?.run?.integration_branch ?? "its integration branch";
  const title = task?.title ? `${task.title} · ` : "";
  return clip(`${title}its work is on ${branch}`);
}

/** The card a task's needs-you toast says, when the service has one. */
function needBody(state, taskId) {
  const card = (state?.needs ?? []).find((entry) => entry?.task === taskId);
  if (card?.body) return clip(desktopWords(card.body, taskId));
  const task = (state?.tasks ?? []).find((entry) => entry?.id === taskId);
  if (task?.derived?.reason) return clip(desktopWords(task.derived.reason, taskId));
  return "It waits for you.";
}

/**
 * The failed review or check a `gate_failed` event named, read from the task in
 * the snapshot: which gate, and the first finding's title.
 *
 * An event notice carries no payload, so this is the join that lets a toast say
 * "the count badge overflows" instead of "a gate failed".
 */
export function failedGate(state, taskId) {
  const task = (state?.tasks ?? []).find((entry) => entry?.id === taskId);
  const attempts = task?.attempts ?? [];
  const attempt = attempts.length === 0 ? null : attempts[attempts.length - 1];
  const gates = (attempt?.gates ?? []).filter((entry) => entry?.passed === false);
  const gate = gates.length === 0 ? null : gates[gates.length - 1];
  if (!gate) return null;
  return {
    gate: String(gate.gate ?? ""),
    words: clip(gate.findings?.[0]?.title ?? `${gate.gate} failed`),
  };
}

/**
 * The toasts a batch of change events asks for, newest last.
 *
 * Only the kinds a person acts on are news: a task landing, something needing
 * them, and a review that failed. Every other event moves the board, which the
 * board shows by itself.
 */
export function newsItems(changes, state) {
  const items = [];
  for (const change of changes ?? []) {
    const level = NEWS[change?.kind];
    if (!level) continue;
    const task = String(change?.task ?? "");
    if (level === "success") {
      items.push({
        id: eventId(change.seq),
        level,
        title: task ? `${task} · Landed` : "Landed",
        body: landedBody(state, task),
        task,
      });
      continue;
    }
    if (level === "warning") {
      if (finished(state, task)) continue;
      items.push({
        id: eventId(change.seq),
        level,
        title: task ? `${task} · Needs you` : "Needs you",
        body: needBody(state, task),
        task,
      });
      continue;
    }
    // A failed gate is a review's verdict in shprd's loop, so the toast says
    // which one and what it found; without the snapshot's join it says the
    // least it truthfully can.
    const failed = failedGate(state, task);
    // The reviewer's own name, when the person gave it one: "QA failed it".
    const qa = roleRename(state, "reviewer");
    const review = qa ? `${qa} failed it` : "Review failed";
    const what = failed?.gate === "review" || !failed ? review : `${failed.gate} failed`;
    items.push({
      id: eventId(change.seq),
      level,
      title: task ? `${task} · ${what}` : what,
      body: failed?.words ?? "Nothing moves without you.",
      task,
    });
  }
  return items;
}

/**
 * One failed step as a person reads it. A land failure is already one plain
 * sentence ("Couldn't merge T1: …"), so it stands alone: no step name in front
 * and no command line behind.
 */
export function failureText(step, where = "") {
  if (step.step === "land" && /^Couldn't /.test(step.error ?? "")) return step.error;
  return `${step.step}${where}: ${step.error}`;
}

/**
 * The toast a failed pass asks for, or null when the last pass was clean.
 *
 * The pass is the engine's own act and not an event, so its failure is only in
 * the snapshot's `last_tick`; the id is that pass's timestamp, which is what
 * makes it one toast however many times a client reads the snapshot.
 */
export function passItem(state) {
  const tick = state?.last_tick;
  const failed = tick?.failed ?? [];
  if (failed.length === 0) return null;
  const first = failed[0];
  const where = first.task ? ` on ${first.task}` : "";
  const others = failed.length - 1;
  // A task that could not start is said in plain words: which task, and that
  // nothing was lost. The engine's own wording (pass, attempt, branch names)
  // stays in its log.
  if ((first.step === "start" || first.step === "send_back") && first.task) {
    return {
      id: passId(tick.at_ms),
      level: "error",
      title: "A task could not start",
      body: clip(
        `${first.task} could not start. Nothing was lost; it is still waiting to be started again.${others > 0 ? ` (+${others} more)` : ""}`,
      ),
      task: first.task,
    };
  }
  return {
    id: passId(tick.at_ms),
    level: "error",
    title: "The last pass failed",
    body: clip(`${failureText(first, where)}${others > 0 ? ` (+${others})` : ""}`),
    task: first.task || "",
  };
}

/** Everything one reading of the service asks for: the news, then the pass. */
export function fromSnapshot(changes, state) {
  const items = newsItems(changes, state);
  const pass = passItem(state);
  if (pass) items.push(pass);
  return items;
}

/**
 * The list with `items` added: newest last, one per id, and nothing stale.
 *
 * An item whose id is already in the list replaces the old one in place, which
 * is what keeps a re-read of the same event from stacking a second toast. The
 * oldest go first once there are more than [`KEEP`] besides the newest.
 */
export function mergeNews(list, items, at = Date.now()) {
  const kept = (list ?? []).filter((item) => at - item.at < KEEP_MS);
  const next = kept.slice();
  for (const item of items ?? []) {
    const index = next.findIndex((entry) => entry.id === item.id);
    const entry = { ...item, at };
    if (index === -1) next.push(entry);
    else next[index] = { ...next[index], ...entry, at: next[index].at };
  }
  return next.slice(-(KEEP + 1));
}

/** The list without `id`. */
export function dismiss(list, id) {
  return (list ?? []).filter((item) => item.id !== id);
}

/**
 * The toasts on screen at `at`, oldest first: neither dismissed nor expired,
 * and never more than [`SHOWN`].
 *
 * An expired toast goes by itself; a dismissed one is a person's answer and is
 * never drawn again, which is why dismissal is the caller's list to keep (the
 * mod holds it for the life of the copy).
 */
export function visible(list, at = Date.now(), dismissed = []) {
  const gone = dismissed instanceof Set ? dismissed : new Set(dismissed ?? []);
  return (list ?? [])
    .filter((item) => !gone.has(item.id))
    .filter((item) => at - item.at < timeoutOf(item))
    .slice(-SHOWN);
}
