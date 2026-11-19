// Jump: the one find box over the coordinator, the task spaces and the tasks.
//
// Pure functions of the board (`GET /api/state`) and Herdr's space list, so the
// whole box — what it lists, in what order, what a pick does — is testable with
// a captured snapshot and no window.
//
// Ported from Shepherdr's Jump (MIT):
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Path: `mods/jump/main.js` (`matches`, `items`, `rows`, `go`, the groups, the
//   8-row "Show N more" and the empty words)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// What is shprd's own: Shepherdr's four kinds (space, task, artifact, machine)
// are two here. A task space is reached through its task (a task whose space is
// up selects that space; one with none opens in the Tasks panel), so *Spaces*
// holds the coordinator and the spaces shprd did not make. Artifacts and
// machines come with their own tasks (decisions 2026-11-14).
//
// Shared: the source is `mods/lib/jump.js`; `bin/sync-shared` copies it into
// every mod folder that imports it. Edit the source, never a copy.

import { matches, secondLine } from "./board.js";
import { KIND, coordinatorRow, spaceIndex, splitSpaceName } from "./spaces.js";
import { STATUS, STATUS_ORDER, roleTitle } from "./tokens.js";

/** The popup's measurements, Shepherdr's: 620 × 480 at the top, offset 56. */
export const JUMP = { width: 620, height: 480, offset: 56, row: 48, head: 30, more: 32, shown: 8 };

/** The box's words. */
export const JUMP_WORDS = {
  placeholder: "Search tasks and spaces",
  loading: "Reading the board…",
  none: "Nothing found. Try fewer words.",
  empty: "Nothing here.",
  down: "shprd serve is not running: only this window's spaces are listed.",
};

/** The group headings, in the order they are drawn. */
export const GROUPS = { space: "Spaces", task: "Tasks" };

/** A task's place in the list: its status's, a dropped task last. */
function rank(task) {
  if (task?.status === "dropped") return STATUS_ORDER.length + 1;
  const index = STATUS_ORDER.indexOf(task?.derived?.status);
  return index === -1 ? STATUS_ORDER.length : index;
}

/**
 * Everything the box can go to, filtered by `query` (every word in the item
 * somewhere, any case).
 *
 * A dropped task is history and is found only by typing something, as
 * Shepherdr's archived tasks are.
 */
export function jumpItems(state, list = [], query = "") {
  const q = String(query ?? "").trim();
  const index = spaceIndex(list);
  const out = [];

  const coordinator = index.coordinator ? coordinatorRow(state, index.coordinator) : null;
  if (coordinator) {
    const space = index.coordinator;
    if (matches(q, coordinator.label, roleTitle(state, "coordinator"), "Coordinator", space.name, space.folder, coordinator.sub)) {
      out.push({
        kind: "space",
        key: `s:${space.id}`,
        id: space.id,
        label: coordinator.label,
        sub: `${roleTitle(state, "coordinator")} · ${coordinator.sub}`,
        mark: "#",
        active: Boolean(space.active),
      });
    }
  }
  for (const space of list ?? []) {
    if (splitSpaceName(space?.name).kind !== "other") continue;
    if (!matches(q, space.name, space.folder)) continue;
    out.push({
      kind: "space",
      key: `s:${space.id}`,
      id: space.id,
      label: String(space.name || space.folder || "Space"),
      sub: String(space.folder ?? ""),
      mark: "#",
      active: Boolean(space.active),
    });
  }

  const tasks = (state?.tasks ?? []).slice().sort(
    (a, b) => rank(a) - rank(b) || String(a.id).localeCompare(String(b.id)),
  );
  for (const task of tasks) {
    if (task?.status === "dropped" && q === "") continue;
    const status = task?.derived?.status ?? "to_do";
    const word = (STATUS[status] ?? STATUS.to_do).word;
    const line = secondLine(task);
    const space = index.tasks.get(task.id) ?? null;
    if (!matches(q, task.id, task.title, line, word, space?.name)) continue;
    out.push({
      kind: "task",
      key: `t:${task.id}`,
      id: task.id,
      ref: task.id,
      label: String(task.title ?? ""),
      sub: [word, line && line !== task.title ? line : null].filter(Boolean).join(" · "),
      status,
      space: space?.id ?? null,
      active: Boolean(space?.active),
    });
  }
  return out;
}

/**
 * The list as drawn: each non-empty group's heading, its rows (8 before "Show N
 * more", unless `more` holds the group), and the "Show N more" row.
 */
export function jumpRows(items, more = new Set()) {
  const rows = [];
  for (const kind of ["space", "task"]) {
    const mine = items.filter((item) => item.kind === kind);
    if (mine.length === 0) continue;
    rows.push({ kind: "head", key: `h:${kind}`, label: GROUPS[kind], count: mine.length });
    const shown = more.has(kind) ? mine : mine.slice(0, JUMP.shown);
    rows.push(...shown);
    if (mine.length > shown.length) {
      rows.push({ kind: "more", key: `more:${kind}`, group: kind, count: mine.length - shown.length });
    }
  }
  return rows;
}

/** The height of one drawn row. */
export function rowHeight(row) {
  return row.kind === "head" ? JUMP.head : row.kind === "more" ? JUMP.more : JUMP.row;
}

/**
 * What a pick does, as data the mod runs: `{more}` unfolds a group;
 * `{command, args}` is a Herdr command (a space is shown); `{task, title}` opens
 * the task's thread in the Tasks panel (`openThread`), whether or not it has a
 * space of its own: an agent is reached through its task, not through a tab.
 */
export function jumpTarget(row) {
  if (!row || row.kind === "head") return null;
  if (row.kind === "more") return { more: row.group };
  if (row.kind === "space") return { command: "space.select", args: { id: row.id } };
  return { task: row.id, title: row.label };
}
