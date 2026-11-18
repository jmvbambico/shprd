// The sidebar's reading of the board: the coordinator's space, then one row per
// task space, each with the word shprd's rules give it.
//
// The rows are a pure function of two things a mod already has: `GET /api/state`
// (through `lib/client.js`) and Herdr's live space list (`spaces()`). Nothing
// here reads a file, calls a service or knows what time it is, so the whole
// sidebar's shape is testable with a captured snapshot and a list of spaces.
//
// What shprd's own naming decides, and this module reads instead of guessing:
// `shprd serve` makes the repository's space `shprd · <repo>` and gives every
// task `shprd · <repo> · <run> · <task>`
// (`crates/shprd-mux/src/lib.rs`, `repo_space_name` and `task_space_name`). A
// space named another way is Herdr's or the person's, so it is not a row here:
// shprd never renames someone else's space, and a sidebar that listed one would
// be claiming it.
//
// Ported from Shepherdr's spaces list (MIT):
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/spaces-chat/main.js` (a row per space: a mark, the name, and the
//   red/green counts at the end; `rowHeight`, `compact()`'s rail and `narrow()`),
//   `mods/spaces-chat/popups.js` (the row's menu: `heightOf`, `openMenu`,
//   `closeSpace`), `core/menus.js` (`spaceMenu`'s items and order), `core/tasks.js`
//   (`place()`'s grouping)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// Shared: the source is `mods/lib/spaces.js`; `bin/sync-shared` copies it into
// every mod folder that imports it. Edit the source, never a copy.

import { NEED, STATUS_ORDER, status as statusOf } from "./tokens.js";
import { counts, isLive, progressOf, secondLine } from "./board.js";

/** The id a coordinator row carries, so a click and a test name the same thing. */
export const COORDINATOR_ID = "coordinator";

/** The separator shprd's space names are built from (`task_space_name`). */
export const SEP = " · ";

/** The prefix every space shprd owns begins with. */
export const PREFIX = "shprd";

/**
 * The width below which the sidebar draws its rail, in px.
 *
 * It is Shepherdr's own threshold, and it is why the rail is read from the layout
 * rather than kept as a flag: dragging the edge narrows it too
 * (`mods/spaces-chat/main.js:790`, `narrow()`).
 */
export const NARROW_PX = 160;

/** A menu popup's measurements, as Shepherdr's `popups.js` `heightOf` has them. */
export const MENU = { width: 210, row: 32, pad: 5, sep: 9 };

/** A row's kind: the coordinator's space, a task's space, or a task with none. */
export const KIND = { coordinator: "coordinator", task: "task", waiting: "waiting" };

/**
 * What a space name says, from shprd's own naming rule.
 *
 * `{kind: "coordinator", repo}` for `shprd · shop`; `{kind: "task", repo, run,
 * task}` for `shprd · shop · m10a · T1`; `{kind: "other"}` for a name shprd did
 * not give a space — a person's own space, which the sidebar leaves alone.
 */
export function splitSpaceName(name) {
  const text = String(name ?? "");
  const parts = text.split(SEP);
  if (parts.length < 2 || parts[0] !== PREFIX || parts[1] === "") {
    return { kind: "other", name: text };
  }
  if (parts.length === 2) return { kind: KIND.coordinator, repo: parts[1], name: text };
  if (parts.length === 4 && parts[2] !== "" && parts[3] !== "") {
    return { kind: KIND.task, repo: parts[1], run: parts[2], task: parts[3], name: text };
  }
  return { kind: "other", name: text };
}

/**
 * The spaces shprd owns, sorted into the coordinator's and each task's.
 *
 * The first space with a name wins, as Herdr's list does: two copies of one
 * label would otherwise make a row ambiguous, and shprd's own `ensure_space`
 * only ever reuses the one it found.
 */
export function spaceIndex(list = []) {
  let coordinator = null;
  const tasks = new Map();
  for (const space of list ?? []) {
    const named = splitSpaceName(space?.name);
    if (named.kind === KIND.coordinator) {
      coordinator = coordinator ?? { ...space, repo: named.repo };
    } else if (named.kind === KIND.task && !tasks.has(named.task)) {
      tasks.set(named.task, { ...space, run: named.run, task: named.task, repo: named.repo });
    }
  }
  return { coordinator, tasks };
}

/**
 * The tasks the sidebar groups, in the order a panel draws them: what needs the
 * person first, then what moves, then what waits, then what is over.
 *
 * `dropped` tasks are history, exactly as the board reads them.
 */
export function orderedTasks(state) {
  const rank = (task) => {
    const index = STATUS_ORDER.indexOf(task?.derived?.status);
    return index === -1 ? STATUS_ORDER.length : index;
  };
  return (state?.tasks ?? [])
    .filter(isLive)
    .slice()
    .sort((left, right) => rank(left) - rank(right) || String(left.id).localeCompare(String(right.id)));
}

/** One task's status as a sidebar row draws it: its word, tone and glyph. */
export function taskLook(task) {
  const look = statusOf(task?.derived?.status);
  return {
    status: task?.derived?.status ?? "to_do",
    word: look.word,
    tone: look.tone,
    glyph: look.glyph,
    pct: progressOf(task),
  };
}

/**
 * The two numbers a row's badge shows: what needs the person, and what is ready.
 *
 * It is Shepherdr's rule for its space rows — one badge, red *Needs you* on the
 * left and green *Ready* on the right, drawn by `countBadge(need, ready)` —
 * filled the way shprd's data allows: the coordinator's row counts the run's
 * tasks, a task's row is one task and so counts at most one of each. A row with
 * nothing to say gets `{need: 0, ready: 0}`, and the badge is not drawn.
 */
export function attention(row) {
  if (!row) return { need: 0, ready: 0 };
  if (row.kind === KIND.coordinator) {
    return { need: Math.max(0, row.count ?? 0), ready: Math.max(0, row.readyCount ?? 0) };
  }
  return { need: row.needs ? 1 : 0, ready: row.ready ? 1 : 0 };
}

/**
 * Every row the sidebar draws, from the snapshot and Herdr's space list.
 *
 * - `coordinator`: the run's own space, or `null` before a run exists. Its word
 *   is the run's, not a task's: nothing on that row is work a person can do.
 * - `tasks`: one row per task space, in [`orderedTasks`]' order. A task whose
 *   space is not up yet is not here — see `waiting`.
 * - `waiting`: the live tasks with no space of their own. They are the board's
 *   rows with nothing to focus yet, and a sidebar that hid them would show an
 *   empty run the moment a plan was written and before its first agent started.
 *
 * `active` marks the row whose space the window is showing, which is the one a
 * click would leave.
 */
export function sidebarRows(state, list = []) {
  const index = spaceIndex(list);
  const tasks = [];
  const waiting = [];
  for (const task of orderedTasks(state)) {
    const look = taskLook(task);
    const space = index.tasks.get(task.id) ?? null;
    const row = {
      kind: space ? KIND.task : KIND.waiting,
      id: task.id,
      label: task.id,
      title: String(task.title ?? ""),
      sub: (look.status === "needs_you" ? needWords(task, state) : null) ?? secondLine(task),
      status: look.status,
      word: look.word,
      tone: look.tone,
      glyph: look.glyph,
      pct: look.pct,
      needs: look.status === "needs_you",
      ready: look.status === "ready",
      active: Boolean(space?.active),
      space: space ?? null,
      folder: space?.folder ?? "",
    };
    if (space) tasks.push(row);
    else waiting.push(row);
  }
  return { coordinator: coordinatorRow(state, index.coordinator), tasks, waiting };
}

/**
 * The coordinator's row: the run, and whether the window can work in it.
 *
 * `word` is the run's own state rather than a task's eight words, because the
 * coordinator is not a task: it is the space every task's agent was handed from
 * (`repo_space_name`), and a person looking at it wants to know whether the run
 * is moving, not that it is "Working".
 */
export function coordinatorRow(state, space = null) {
  const tally = counts(state);
  const run = state?.run ?? null;
  const entry = {
    kind: KIND.coordinator,
    id: COORDINATOR_ID,
    label: String(run?.repo ?? "").split("/").filter(Boolean).pop() || "This repository",
    title: "Coordinator",
    sub: run ? `run ${run.id} · ${run.integration_branch}` : (state?.hint ?? "No run yet"),
    status: run ? (state?.finished ? "done" : "working") : "to_do",
    word: run ? (state?.finished ? "Finished" : "Running") : "No run",
    tone: run ? (state?.finished ? "green" : "amber") : "dim",
    glyph: run ? (state?.finished ? "tick" : "pie") : "ring",
    needs: tally.needs > 0,
    count: tally.needs,
    ready: false,
    readyCount: tally.ready,
    // The row's own tail, in the slot Shepherdr gives a repo's git state: the run
    // this space belongs to, which is what a person needs to see on it.
    tail: run ? `run ${run.id}` : "",
    active: Boolean(space?.active),
    space,
    folder: space?.folder ?? "",
  };
  return entry;
}

/**
 * Why a row says what it says, when it needs the person: the card's own words
 * when the service has one, else the derived reason, else the token table's.
 *
 * A one-line row does not draw it, but the row keeps it: it is what a toast or a
 * later view says about the same task, and it is derived here once.
 */
export function needWords(task, state) {
  const need = task?.derived?.need;
  if (!need) return null;
  const card = (state?.needs ?? []).find((entry) => entry?.task === task?.id);
  if (card?.body) return card.body;
  if (task?.derived?.reason) return task.derived.reason;
  return NEED[need] ?? null;
}

/**
 * Whether the sidebar is drawn as its rail: one glyph a row, no names.
 *
 * Shepherdr reads the width, not a flag ("the sidebar's rail goes by its width,
 * not a flag: dragging its edge narrows it too" — `mods/spaces-chat/main.js:50`,
 * and `narrow()` at `:795`), so a person who drags the edge gets the rail without
 * asking for it. The tree is Herdr's `layout()`; a tree with no `sidebar` node
 * keeps the wide form.
 */
export function narrowSidebar(tree) {
  const width = findNode(tree, "sidebar")?.size?.px;
  return typeof width === "number" && width < NARROW_PX;
}

/** The first node of a layout tree with `id`, depth first. */
function findNode(node, id) {
  if (node?.id === id) return node;
  for (const child of node?.children ?? []) {
    const found = findNode(child, id);
    if (found) return found;
  }
  return null;
}

/**
 * A space row's right-click menu, in Shepherdr's order and words.
 *
 * Shepherdr's list is Notify, [Start Assistant], [Mark as read], Pin/Unpin,
 * Rename…, Space settings…, [worktree rows], Close… (`core/menus.js:16-30`). shprd
 * keeps the two that mean something for a task's space and drops the rest:
 *
 * - **Open task** is shprd's own: in Shepherdr a click switches to the space and
 *   that is all a space is, while here a space is one task, so the menu can also
 *   put that task in front in the Tasks panel.
 * - Notify, Mark as read and Pin have no shprd counterpart — the service has no
 *   per-space notify, no unread and no pins.
 * - **Rename…** is left out on purpose: Shepherdr renames the *space*, and a
 *   space's name is the contract that ties it to its task here
 *   (`task_space_name`; [`splitSpaceName`] reads it back). A renamed space would
 *   stop being that task's space, and the row would vanish from this list. Renaming
 *   the task instead is service work (no `task.edit` action exists).
 * - Space settings and the worktree rows have nothing behind them (one repo per
 *   service, worktrees made by the engine, artifacts deferred).
 *
 * A task with no space yet gets Open task only: there is nothing to close.
 *
 * `row` is a sidebar row, or the popup's own props: both say which space they are
 * about (`space` an object, or its id).
 */
export function menuItems(row) {
  const space = row?.space?.id ?? row?.space ?? null;
  const items = [{ id: "open", label: "Open task" }];
  if (space) items.push({ sep: true }, { id: "close", label: "Close…" });
  return items;
}

/** A popup's height for its items: rows, separators and padding, as `heightOf`. */
export function menuHeight(items = []) {
  return items.reduce((h, item) => h + (item.sep ? MENU.sep : MENU.row), MENU.pad * 2);
}

/**
 * What closing a space asks, in Shepherdr's words (`mods/spaces-chat/popups.js:31-39`):
 * a question titled with what is closed, a line saying what goes with it, and a
 * button that says the verb — never "Confirm".
 *
 * The title names the task, not the space: shprd's space names are long and
 * derived (`shprd · shop · m11 · T4`), and the person picked the row by its id.
 */
export function closeWords(p) {
  const named = String(p?.task ?? "").trim();
  return {
    title: `Close “${named || "this space"}”?`,
    detail: "Its tabs close and every program in them ends.",
    verb: "Close space",
  };
}
