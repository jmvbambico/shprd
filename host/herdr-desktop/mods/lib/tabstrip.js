// The tab bar's reading of a space: one pill per tab, with the agent that runs
// in it and the state it is in, plus the side panel's buttons.
//
// Two facts meet here, and the module only joins them:
//
// - shprd's own tab titles: `<agent>·<task>` for the task's worker
//   (`crates/shprd/src/cmd/spawn.rs`, `worker_title`) and
//   `<agent>·<task>·review` for its reviewer (`crates/shprd/src/cmd/review.rs`,
//   `reviewer_title`). Those are the only two titles shprd writes, and there are
//   only two roles to read from them: a backup that takes a task over does it in
//   the worker's own tab, through the same `open_agent` path with the same
//   `<agent>·<task>` title (`handoff.rs`), so its tab reads as the worker's with
//   the backup agent's name on it — which is what is running there. A tab whose
//   title is neither shape is a person's own tab: it still gets a pill, with no
//   agent and no task claimed.
// - Herdr's live view of a tab: the most urgent agent status in it
//   (`working | blocked | done | idle`), and its panes. That is the truth about
//   the terminal; shprd's snapshot is the truth about the task, and it is what a
//   pill says when the host has no status to report (a shell, a program that
//   reports nothing).
//
// Pure: a snapshot, a `SpaceInfo` and `PaneInfo[]` in; rows out. The mod draws
// them, and the unit test drives them with fixtures.
//
// Ported from Shepherdr's tab bar (MIT):
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/tabs-tasks/main.js` (the pill row and the panel's buttons),
//   `core/tasks.js` (`STATUS`, the words a tab's dot stands for)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// Shared: the source is `mods/lib/tabstrip.js`; `bin/sync-shared` copies it into
// every mod folder that imports it. Edit the source, never a copy.

import { roleTitle, status as statusOf } from "./tokens.js";

/** The separator shprd's tab titles are built from (`worker_title`). */
export const SEP = "·";

/** The words a role is called by. */
export const ROLE = {
  worker: "Worker",
  reviewer: "Reviewer",
};

/**
 * The role a title's last segment names.
 *
 * `review` is the one suffix shprd writes (`reviewer_title`, `·review`); the
 * worker's title has no suffix at all (`worker_title`). A segment this build
 * does not know is read as the worker's, because that is the title a worker
 * tab — and a backup that took one over — really carries, and inventing a
 * third role for a word nobody writes would be a claim the run does not back.
 */
export const SUFFIX = { review: "reviewer", worker: "worker" };

/** Herdr's agent states, as a pill names and colours them. */
export const LIVE = {
  working: { word: "Working", tone: "amber", glyph: "pie" },
  blocked: { word: "Needs you", tone: "red", glyph: "dot" },
  done: { word: "Finished", tone: "green", glyph: "tick" },
  idle: { word: "Idle", tone: "dim", glyph: "ring" },
};

/**
 * What a tab's title says: the agent, the task and the role.
 *
 * `titled: false` means the title is not one shprd gives a tab, so the pill
 * shows the title alone. A reviewer's title ends in `review` (`reviewer_title`);
 * anything else — the worker's own `<agent>·<task>`, and a backup's, which is
 * the same title with another agent in it — is the worker's.
 */
export function parseTabName(name) {
  const text = String(name ?? "").trim();
  const parts = text.split(SEP);
  if (parts.length < 2 || parts[0] === "" || parts[1] === "") {
    return { titled: false, agent: null, task: null, role: null, name: text };
  }
  const suffix = parts.length > 2 ? parts[2].toLowerCase() : "worker";
  const role = SUFFIX[suffix] ?? "worker";
  return {
    titled: true,
    agent: parts[0],
    task: parts[1],
    role,
    name: text,
  };
}

/**
 * The program a tab shows, from the app's own table for its panes.
 *
 * Shepherdr's rule (`mods/tabs-tasks/main.js:155`): the first pane in the tab that
 * reports as an agent, else the tab's first pane at all. That is what gives a pill
 * its icon: an agent's own glyph when one runs there, else whatever the shell is.
 * A tab with no pane reports nothing, and the pill draws no icon rather than
 * guessing at one.
 */
export function tabProgram(panes, tabId) {
  const mine = (panes ?? []).filter((pane) => pane?.tab === tabId);
  const agent = mine.find((pane) => pane?.program?.agent);
  return (agent ?? mine[0])?.program ?? null;
}

/** The most urgent agent status among a tab's panes, or null when none reports. */
export function paneState(panes, tabId) {
  const mine = (panes ?? []).filter((pane) => pane?.tab === tabId);
  for (const word of ["blocked", "done", "working", "idle"]) {
    if (mine.some((pane) => pane.status === word)) return word;
  }
  return null;
}

/** One task of the snapshot, by id, or null. */
export function taskOf(state, id) {
  if (!id) return null;
  return (state?.tasks ?? []).find((task) => task?.id === id) ?? null;
}

/**
 * A pill's state: the host's word when it has one, else the task's own.
 *
 * A host that reports nothing is not silence about the task — shprd's rules
 * still know where the task stands — so the fallback is the snapshot's derived
 * word, and a tab shprd did not title reads `Idle` rather than inventing one.
 */
export function tabState(tab, panes, state) {
  const live = paneState(panes, tab?.id);
  if (live) return { status: live, ...LIVE[live], from: "host" };
  const task = taskOf(state, parseTabName(tab?.name).task);
  if (!task) return { status: "idle", ...LIVE.idle, from: "none" };
  const look = statusOf(task.derived?.status);
  return {
    status: task.derived?.status ?? "to_do",
    word: look.word,
    tone: look.tone,
    glyph: look.glyph,
    from: "shprd",
  };
}

/**
 * What a pill is called: the title shprd gave the tab, else the name someone
 * gave it, else what runs in it (Shepherdr's `tabName`, `core/menus.js:106`).
 *
 * Herdr names a tab nobody named by its position ("1"), which is not a name a
 * person reads on a pill — the program's own name is ("zsh", "Claude Code"), and
 * "Terminal" when even that is unknown.
 */
export function tabLabel(tab, named, program) {
  if (named?.titled) return named.name;
  const text = String(tab?.name ?? named?.name ?? "").trim();
  // Herdr's own names for a tab nobody named are its position ("1") and, on
  // older builds, "tb3"; neither is a name a pill should wear.
  const automatic = text === "" || /^(tb)?\d+$/.test(text);
  if (!automatic) return text;
  return program?.name || "Terminal";
}

/**
 * Every pill of `space`, in the host's order.
 *
 * A pill carries what the mod draws and what a test asserts: the tab's id, the
 * agent and role its title names, the task's id and its derived word, and
 * whether it is the shown tab. `active` is the host's own answer, so a click
 * that missed never moves the highlight here.
 */
export function tabRows(space, panes, state) {
  const tabs = space?.tabs ?? [];
  return tabs.flatMap((tab, index) => {
    // Another product's Assistant tab (Shepherdr's chat pane) is not shprd's to
    // show: its message would reach that product's agent, not the coordinator.
    if (foreignTab(tab, panes)) return [];
    const named = parseTabName(tab?.name);
    const shape = tabState(tab, panes, state);
    const task = taskOf(state, named.task);
    const program = tabProgram(panes, tab?.id);
    return [{
      id: tab.id,
      index,
      name: named.name,
      label: tabLabel(tab, named, program),
      // Whether a person named it (`tab.rename`, `tab.new {name}`): the rename
      // prompt's field holds the name only when there is one.
      named: Boolean(tab.named),
      agent: named.agent,
      role: named.role,
      roleWord: named.role ? roleTitle(state, named.role) : null,
      task: named.task,
      title: String(task?.title ?? ""),
      active: Boolean(tab.active),
      status: shape.status,
      word: shape.word,
      tone: shape.tone,
      glyph: shape.glyph,
      from: shape.from,
      needs: shape.tone === "red",
      // The badge's two numbers: one task is one tab, so a pill's count is at most
      // one of each — what needs the person (red) and what is ready (green). They
      // are counts, not flags, because the badge is the one that takes two.
      need: shape.tone === "red" ? 1 : 0,
      ready: shape.tone === "green" ? 1 : 0,
      // The pill's icon: the program's own, from the app's table (`name` for the
      // tab's own title when nobody named it).
      icon: program?.icon ?? null,
      tint: Boolean(program?.tint),
      program: program?.name ?? null,
      zoomed: Boolean(tab.zoomed),
      panes: Number.isInteger(tab.panes) ? tab.panes : 0,
    }];
  });
}

/** The mod Shepherdr's Assistant chat pane is; shprd's own is `shprd-chat`. */
export const FOREIGN_CHAT_MOD = "chat";

/** Whether a tab is another product's Assistant (a pane of Shepherdr's chat mod). */
export function foreignTab(tab, panes) {
  return (panes ?? []).some((pane) => pane?.tab === tab?.id && pane?.mod === FOREIGN_CHAT_MOD);
}

/** The pills' counts, for a heading or a test. */
export function pillCounts(rows) {
  const tally = { total: 0, working: 0, needs: 0, ready: 0 };
  for (const row of rows ?? []) {
    tally.total += 1;
    if (row.tone === "red") tally.needs += 1;
    else if (row.tone === "amber") tally.working += 1;
    else if (row.tone === "green") tally.ready += 1;
  }
  return tally;
}

/** The side panel's stacks, in the order the bar draws their buttons. */
export const PANELS = [
  { id: "files", word: "Files" },
  { id: "changes", word: "Changes" },
  { id: "tasks", word: "Tasks" },
];

/**
 * The side panel's buttons, with the counts each one's stack would show.
 *
 * The Tasks button carries shprd's two counts the way Shepherdr's did — what
 * needs the person, then what is ready for them. There is no Asks button: the
 * cards a program raises are in its task's chat thread, and the Tasks button's red
 * count is how a person knows one is waiting. Settings has no button of its own: it is a dialog over the window
 * (`openSettings()` in `lib/panel.js`), opened from the sidebar's footer, exactly
 * as Shepherdr's is, so nothing here names it.
 */
export function panelButtons(state, shown = "tasks") {
  const tally = { needs: 0, ready: 0 };
  for (const task of state?.tasks ?? []) {
    if (task?.status === "dropped") continue;
    if (task?.derived?.status === "needs_you") tally.needs += 1;
    else if (task?.derived?.status === "ready") tally.ready += 1;
  }
  return PANELS.map((panel) => ({
    ...panel,
    active: panel.id === shown,
    needs: panel.id === "tasks" ? tally.needs : 0,
    ready: panel.id === "tasks" ? tally.ready : 0,
  }));
}

/** The layout node called `id` under `node`, or null. */
function findNode(node, id) {
  if (node?.id === id) return node;
  for (const child of node?.children ?? []) {
    const found = findNode(child, id);
    if (found) return found;
  }
  return null;
}

/**
 * The side panel's stack that is showing, read from the layout itself, or null
 * when the panel is hidden.
 *
 * The layout is the truth: a row elsewhere (a sidebar task, Jump, a key) moves
 * the panel without telling the bar, so a flag the bar kept would drift.
 */
export function shownPanel(root) {
  const panel = findNode(root, "panel");
  if (!panel || panel.hidden) return null;
  const body = findNode(root, "panel-body");
  const active = body?.active ?? null;
  return PANELS.some((entry) => entry.id === active) ? active : null;
}

/**
 * Per-space panel memory: what the panel showed in each space, put back when the
 * space is shown again.
 *
 * `memory` is a Map of space id to a stack id (or null for hidden), `last` the
 * space seen on the previous look and `current` what the layout shows now.
 * While the space stays the same the look is recorded; when it changes, nothing
 * is recorded (the layout still shows the old space's panel) and `restore` says
 * what to put back: `undefined` for a space not seen before (leave the panel
 * alone), else a stack id or null.
 */
export function rememberPanel(memory, last, spaceId, current) {
  if (!spaceId) return { last, restore: undefined };
  if (spaceId === last) {
    memory.set(spaceId, current);
    return { last: spaceId, restore: undefined };
  }
  return { last: spaceId, restore: memory.has(spaceId) ? memory.get(spaceId) : undefined };
}
