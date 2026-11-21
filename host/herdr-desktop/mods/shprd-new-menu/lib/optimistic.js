// The optimistic layer (SHPR-81): what a task action does to the panes the moment
// it is pressed, before the service has answered.
//
// A pressed Accept used to show a busy button and nothing else until the service
// answered; the row, the Graph, the status bar and the chat kept the old word. Now
// one shared layer lives here, not in each pane:
//
// - **A shared list of ops** (`tasks.optimistic`). Every mod is its own runtime, so
//   the list is what the panes share, and `setShared` re-renders every mod at once.
//   An op is `{id, kind, folder, tasks, items, phase, at, ...}`: `items` is the
//   plan, worked out from the board at the press (which task gets which word).
// - **A pure overlay** ([`overlay`]) that the client's [`RepoCache`] applies on
//   `get`. The cached board is never changed, so rolling back is just no longer
//   applying an op.
// - **No ninth status.** An item maps a task to one of the eight words plus a
//   separate display `label` ("Reviewing…", "Stopping…") that rows and the Graph
//   show instead of the word.
// - **A refresh must not undo it.** An item applies only while the board still
//   shows the task as it was at the press (`from`). Once the board shows anything
//   else, that is the outcome (or the truth), and the item steps aside.
// - **Reconcile.** `confirm` keeps the op a few seconds so a pane whose next read
//   is late does not snap back; `fail` drops the effect at once and keeps one plain
//   error line (`board.optimistic.error`); a merge that the service tries and
//   cannot do (`last_tick.failed`, step `land`) ends an Accept the same way.
// - **Double presses are ignored**: `begin` answers null while the task has a
//   live op.
// - **No timers.** Ops expire by their timestamps, checked when drawn.
//
// Shared: the source is `mods/lib/optimistic.js`; `bin/sync-shared` copies it into
// every mod folder as `lib/optimistic.js`. Edit the source, never a copy.

import { exec, setShared, shared } from "herdr";

/** The shared key the list of ops lives under. */
export const KEY = "tasks.optimistic";
/** An op whose answer never came is dropped after this long, ms. */
export const PENDING_MS = 30_000;
/** A confirmed op keeps its effect this long for a pane that has not read yet, ms. */
export const CONFIRMED_MS = 6_000;
/**
 * An Accept's approval is recorded at once but the merge is the engine's next pass
 * (seconds, and longer while it reviews or gates), so a confirmed Accept holds
 * "Merging…" this long unless the board shows what happened, ms.
 */
export const ACCEPT_MS = 60_000;
/** A failed op keeps its error line this long, ms. */
export const FAILED_MS = 8_000;
/** A Review is a local act (it opens the card): its word lasts until the card closes, at most this long, ms. */
export const REVIEW_MS = 60_000;

/** The words a pressed action shows while it waits. */
export const LABELS = {
  "task.accept": "Merging…",
  "task.start": "Starting…",
  "task.retry": "Retrying…",
  "task.review_again": "Retrying review…",
  "task.stop": "Stopping…",
  "task.drop": "Dropping…",
  "task.drop_many": "Dropping…",
  review: "Reviewing…",
  "ask.answer": "Answered…",
};

/** What the chat says under the timeline while an op is pending. */
const NOTICE = {
  "task.accept": "Accepting",
  "task.start": "Starting",
  "task.retry": "Retrying",
  "task.review_again": "Retrying review of",
  "task.stop": "Stopping",
  "task.drop": "Dropping",
  "task.drop_many": "Dropping",
  review: "Reviewing",
  "ask.answer": "Sending your answer to",
};

/** A folder as the layer keys it: no trailing slash, `/private` of `/var`, `/tmp`, `/etc` off. */
export function folderKey(folder) {
  let text = String(folder ?? "").trim();
  while (text.length > 1 && text.endsWith("/")) text = text.slice(0, -1);
  return text.replace(/^\/private(?=\/(var|tmp|etc)(\/|$))/, "");
}

const find = (board, id) => (board?.tasks ?? []).find((task) => task?.id === id) ?? null;
const word = (task) => task?.derived?.status ?? null;
const snap = (task) => ({ status: task?.status ?? null, word: word(task) });

/**
 * What an action does to the board, as items `{id, from, to}`.
 *
 * `from` is the task as the board shows it now; `to` is what the pane should show:
 * `{status?, word?, need?, label}`. An empty list means the board has nothing to
 * say about it (a verb the layer does not know).
 */
export function plan(kind, board, tasks, extra = {}) {
  const items = [];
  const put = (task, to) => items.push({ id: task.id, from: snap(task), to });
  const label = LABELS[kind] ?? "Working…";
  for (const id of tasks) {
    const task = find(board, id);
    if (!task) continue;
    switch (kind) {
      case "task.accept": {
        put(task, { word: "merging", label });
        // A task that was only waiting on this one starts the moment this lands.
        const doneAfter = new Set(
          (board?.tasks ?? []).filter((other) => word(other) === "done" || other.id === id).map((other) => other.id),
        );
        for (const other of board?.tasks ?? []) {
          const waiting = other.status === "queued" && ["to_do", "queued", "scheduled"].includes(word(other));
          if (!waiting || !(other.deps ?? []).includes(id)) continue;
          if ((other.deps ?? []).every((dep) => doneAfter.has(dep))) {
            put(other, { status: "working", word: "working", label: LABELS["task.start"] });
          }
        }
        break;
      }
      case "task.start":
      case "task.retry":
        put(task, { status: "working", word: "working", label });
        break;
      case "task.stop":
        put(task, { word: "needs_you", need: "stopped", label });
        break;
      case "task.drop":
      case "task.drop_many":
        put(task, { status: "dropped", word: word(task), label });
        break;
      case "review":
      case "task.review_again":
        put(task, { label });
        break;
      case "ask.answer":
        // The card goes at once; the task resumes only when that was its last question.
        if ((extra.remaining ?? 1) <= 1 && word(task) === "needs_you" && task.derived?.need === "asks") {
          put(task, { word: "working", need: null, label });
        }
        break;
      default:
        break;
    }
  }
  return items;
}

/** The ops now shared: `{v, ops}`. */
export function read(get = shared) {
  const value = get(KEY);
  return value && Array.isArray(value.ops) ? { v: Number(value.v) || 0, ops: value.ops } : { v: 0, ops: [] };
}

/** How long an op lives in its phase, ms. */
function lifeOf(op) {
  if (op.phase === "failed") return FAILED_MS;
  if (op.phase === "confirmed") return op.kind === "task.accept" ? ACCEPT_MS : CONFIRMED_MS;
  return op.kind === "review" ? REVIEW_MS : PENDING_MS;
}

/** Whether an op still counts at `now`. */
export function alive(op, now = Date.now()) {
  return now - (op.since ?? op.at ?? 0) < lifeOf(op);
}

function write(ops, put = setShared, get = shared) {
  const { v } = read(get);
  put(KEY, { v: v + 1, ops });
}

/**
 * Starts an op for `kind` on `tasks`, in the repository `folder`: the effect shows
 * in every pane at once. Answers the op, or `null` when a task already has a live op
 * (a double press, which is ignored).
 *
 * `board` is the board as the service last said it (the raw one, not an overlaid
 * one), `extra.ask` names the question an answer is for.
 */
export function begin({ kind, tasks, board, folder, extra = {}, now = Date.now(), get = shared, put = setShared }) {
  const key = folderKey(folder);
  const ids = [...new Set(tasks)];
  const { ops } = read(get);
  // A Review only opens the card (no answer to wait for): a verb pressed from that card
  // (Accept, Send back, Drop) ends it instead of being turned away by it. Without this the
  // Accept the person presses in the dialog they just opened is ignored, and nothing says so.
  const supersedes = (op) => kind !== "review" && op.kind === "review" && op.folder === key && op.tasks.some((id) => ids.includes(id));
  const live = ops.filter((op) => alive(op, now) && !supersedes(op));
  const askKey = extra.ask ? `${ids[0]}/${extra.ask}` : null;
  // A task with an action still waiting for its answer takes no second press; once
  // confirmed, only the same verb again is a double press (Stop right after Start is
  // a new decision), and a question already answered is not answered twice while its op lives.
  const busy = live.some(
    (op) =>
      op.folder === key &&
      op.phase !== "failed" &&
      (op.tasks.some((id) => ids.includes(id)) || (askKey !== null && op.ask === askKey)) &&
      (op.phase === "pending" || op.kind === kind),
  );
  if (busy) return null;
  const op = {
    id: `${now.toString(36)}-${Math.random().toString(36).slice(2, 7)}`,
    kind,
    folder: key,
    tasks: ids,
    items: plan(kind, board, ids, extra),
    ask: askKey,
    phase: "pending",
    at: now,
    since: now,
  };
  write([...live, op], put, get);
  return op;
}

function change(id, edit, { get = shared, put = setShared } = {}) {
  const { ops } = read(get);
  if (!ops.some((op) => op.id === id)) return;
  write(
    ops.map((op) => (op.id === id ? edit(op) : op)),
    put,
    get,
  );
}

/** The service accepted it: the effect stays until a board shows the outcome (or [`CONFIRMED_MS`]). */
export function confirm(id, now = Date.now(), io = {}) {
  change(id, (op) => ({ ...op, phase: "confirmed", since: now, confirmedAt: now }), io);
}

/** The service refused it (or never answered): the effect goes at once and `text` says why. */
export function fail(id, text, now = Date.now(), io = {}) {
  change(id, (op) => ({ ...op, phase: "failed", since: now, error: String(text ?? "") }), io);
}

/** Ends an op with no outcome to show (a Review whose card closed). */
export function finish(id, io = {}) {
  const { ops } = read(io.get ?? shared);
  if (!ops.some((op) => op.id === id)) return;
  write(
    ops.filter((op) => op.id !== id),
    io.put ?? setShared,
    io.get ?? shared,
  );
}

/** Ends a task's Review op, whichever it is (the card is closed). */
export function finishReview(task, folder, io = {}) {
  const key = folderKey(folder);
  for (const op of read(io.get ?? shared).ops) {
    if (op.kind === "review" && op.folder === key && op.tasks.includes(task)) finish(op.id, io);
  }
}

/** The questions answered but not yet gone from the service's list, as `task/ask` keys. */
export function answeredAsks(folder, now = Date.now(), get = shared) {
  const key = folderKey(folder);
  return new Set(
    read(get)
      .ops.filter((op) => op.kind === "ask.answer" && op.folder === key && op.phase !== "failed" && alive(op, now))
      .map((op) => op.ask),
  );
}

/** The op that is pending or confirmed for `task` in `folder`, or null. */
export function opFor(task, folder, now = Date.now(), get = shared) {
  const key = folderKey(folder);
  return (
    read(get).ops.find((op) => op.folder === key && op.phase !== "failed" && alive(op, now) && op.tasks.includes(task)) ?? null
  );
}

/**
 * Whether the board has shown what an op was after (or the opposite): `"outcome"`
 * when an item's task is no longer as it was at the press, `"failed"` when a pass
 * since the confirm could not do it, else `"waiting"`.
 */
export function resolved(op, board) {
  if (op.kind === "review") return "waiting";
  // The engine tried to land it and could not (a merge conflict): the task no longer
  // reads Ready but Needs you, with the engine's own words on it.
  if (op.kind === "task.accept" && op.phase === "confirmed") {
    const self = op.items?.[0];
    const raw = find(board, op.tasks[0]);
    const was = self?.from?.word;
    if (raw && was !== "needs_you" && (raw.status === "failed" || word(raw) === "needs_you") && raw.status !== self?.from?.status) {
      return "failed";
    }
  }
  const tried = board?.last_tick;
  if (op.kind === "task.accept" && op.phase === "confirmed" && tried) {
    const bad = (tried.failed ?? []).find((entry) => entry.step === "land" && op.tasks.includes(entry.task));
    if (bad && Number(tried.at_ms) >= (op.confirmedAt ?? Infinity)) return "failed";
  }
  return "waiting";
}

/** The plain line for a failed land, like the status bar's. */
function landError(op, board) {
  const said = String(find(board, op.tasks[0])?.last_status?.message ?? "")
    .replace(/^Waiting:\s*/, "")
    .trim();
  if (said.startsWith("Couldn't ")) return said;
  const bad = (board?.last_tick?.failed ?? []).find((entry) => entry.step === "land" && op.tasks.includes(entry.task));
  const why = String(bad?.error ?? "").trim();
  return why.startsWith("Couldn't ") ? why : `Couldn't merge ${op.tasks[0]}${why ? `: ${why}` : ""}`;
}

/**
 * `board` with the live ops applied, for the repository `folder`: a new board, with
 * `optimistic: {notes, error}` on it. The board it was given is never changed. A board
 * with nothing to apply is returned as it is.
 *
 * `notes` is one `{kind, tasks, text}` per pending op (the chat's line); `error` is
 * `{task, text}` for the last op that was refused, or null.
 */
export function overlay(board, folder, ops, now = Date.now()) {
  if (!board || !Array.isArray(ops) || ops.length === 0) return board;
  const key = folderKey(folder);
  const mine = ops.filter((op) => op.folder === key && alive(op, now));
  if (mine.length === 0) return board;
  const patches = new Map();
  // The tasks whose pending question (an escalation card in the chat) the press answers.
  const settled = new Set();
  const notes = [];
  let error = null;
  for (const op of mine) {
    if (op.phase === "failed") {
      error = { op: op.id, task: op.tasks[0] ?? null, text: op.error ?? "" };
      continue;
    }
    if (resolved(op, board) === "failed") {
      error = { op: op.id, task: op.tasks[0] ?? null, text: landError(op, board) };
      continue;
    }
    notes.push({ op: op.id, kind: op.kind, tasks: op.tasks, text: `${NOTICE[op.kind] ?? "Working on"} ${op.tasks.join(", ")}…` });
    for (const item of op.items ?? []) {
      // A later op on the same task wins: ops are in press order, and each judges the raw board.
      const raw = find(board, item.id);
      // The board shows the task as it was at the press: the effect holds. Anything
      // else is the outcome or the truth, and the board wins.
      if (!raw || raw.status !== item.from.status || word(raw) !== item.from.word) continue;
      const to = item.to;
      const derived = { ...(raw.derived ?? {}) };
      if (to.word) derived.status = to.word;
      if ("need" in to) derived.need = to.need;
      patches.set(item.id, {
        ...raw,
        status: to.status ?? raw.status,
        derived,
        pending: { label: to.label ?? null, kind: op.kind },
      });
      if (["task.accept", "task.drop", "task.drop_many", "task.start", "task.retry"].includes(op.kind)) {
        settled.add(item.id);
      }
    }
  }
  if (patches.size === 0 && notes.length === 0 && error === null) return board;
  return {
    ...board,
    tasks: (board.tasks ?? []).map((task) => patches.get(task.id) ?? task),
    ...(settled.size > 0 && Array.isArray(board.needs)
      ? { needs: board.needs.filter((need) => !settled.has(need?.task)) }
      : {}),
    optimistic: { notes, error },
  };
}

/** The label a task's row shows in place of its word, or null. */
export function pendingLabel(task) {
  return task?.pending?.label ?? null;
}

/** The refused line for a board an op was applied to, or null. */
export function errorLine(board) {
  return board?.optimistic?.error?.text || null;
}

/** The chat's line for the ops still waiting, or null. */
export function noticeLine(board) {
  const notes = board?.optimistic?.notes ?? [];
  return notes.length === 0 ? null : notes[notes.length - 1].text;
}

/**
 * Starts a Review: not a verb (it only opens the Accept card), so the op has no
 * answer to wait for. The row says "Reviewing…" in every pane until the card closes
 * ([`finishReview`]) or [`REVIEW_MS`] passes. `task` is the task as the pane has it.
 */
export function startReview(task, folder, io = {}) {
  return begin({ kind: "review", tasks: [task.id], board: { tasks: [task] }, folder, ...io });
}

/** What a pane has already told the e2e probe about, by pane and op. */
const told = new Set();

/**
 * The live check's probe: when a run sets the shared key `e2e.probe` to a file, each
 * pane appends one line the first time it draws an op's effect (or its refusal), with
 * the time on its own clock, so a run can say how long a press took to reach every
 * pane. Nothing happens when the key is not set, which is always outside a check.
 */
export function probe(label, board) {
  let path = null;
  try {
    path = shared("e2e.probe");
  } catch {
    return;
  }
  if (typeof path !== "string" || path === "" || !board?.optimistic) return;
  const seen = [
    ...board.optimistic.notes.map((note) => ({ op: note.op, what: note.kind })),
    ...(board.optimistic.error ? [{ op: board.optimistic.error.op, what: "error" }] : []),
  ];
  for (const one of seen) {
    const key = `${label}:${one.op}:${one.what}`;
    if (told.has(key)) continue;
    told.add(key);
    const line = JSON.stringify({ pane: label, op: one.op, what: one.what, at: Date.now() });
    exec("sh", ["-c", `printf '%s\\n' '${line}' >> '${path.replace(/'/g, "")}'`]).catch(() => null);
  }
}
