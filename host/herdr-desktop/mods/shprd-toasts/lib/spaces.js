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
// - Paths: `mods/spaces-chat/main.js` (a row per space: an avatar, the name, and
//   the count that needs the person), `core/tasks.js` (`place()`'s grouping)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// Shared: the source is `mods/lib/spaces.js`; `bin/sync-shared` copies it into
// every mod folder that imports it. Edit the source, never a copy.

import { NEED, STATUS_ORDER, status as statusOf } from "./tokens.js";
import { counts, isLive, secondLine } from "./board.js";

/** The id a coordinator row carries, so a click and a test name the same thing. */
export const COORDINATOR_ID = "coordinator";

/** The separator shprd's space names are built from (`task_space_name`). */
export const SEP = " · ";

/** The prefix every space shprd owns begins with. */
export const PREFIX = "shprd";

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
  };
}

/** How many rows of each kind a row set holds, for the heading's counts. */
export function rowCounts(rows) {
  const tally = { needs: 0, ready: 0, working: 0, total: 0 };
  for (const row of [...(rows?.tasks ?? []), ...(rows?.waiting ?? [])]) {
    tally.total += 1;
    if (row.tone === "red") tally.needs += 1;
    else if (row.tone === "green") tally.ready += 1;
    else if (row.tone === "amber") tally.working += 1;
  }
  return tally;
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
    ready: false,
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
 * It is the row's second line when the task has nothing more recent to say.
 */
export function needWords(task, state) {
  const need = task?.derived?.need;
  if (!need) return null;
  const card = (state?.needs ?? []).find((entry) => entry?.task === task?.id);
  if (card?.body) return card.body;
  if (task?.derived?.reason) return task.derived.reason;
  return NEED[need] ?? null;
}
