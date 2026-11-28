// The menus of the tab bar, as rows of data: the "+" menu, a tab's menu, the
// app's menu and a pane's menu.
//
// They are data, not drawing, for the reason Shepherdr's `core/menus.js` gives:
// the thing that opens a menu and the thing that draws it must offer the same
// rows in the same words and the same order, so the list is a pure function of
// what is there (the roster, the tab, the pane) and a test can read it.
//
// Ported from Shepherdr (MIT):
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `core/menus.js` (`tabMenu`, `appMenu`, `NEW_MENU`, `agentChoices`,
//   `agentHint`), `mods/menu-tasks/main.js` (the pane menu's items and the tab
//   menu's height), `mods/new-menu-tasks/main.js` (the + menu's rows, the 1–9
//   keys), `lib/ui.js` (`menuRow`, the 32 px row)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// What is shprd's own here, and why:
//
// - *Edit instructions…* and *Switch agent ›* are the coordinator tab's rows, as
//   in Shepherdr's Assistant tab menu. Edit instructions opens the Settings
//   window on Prompts; Switch agent lists the roster's installed programs and
//   writes the coordinator role's program (`roles.coordinator.agent`), which the
//   coordinator takes when it next starts.
// - A tab shprd named (`<agent>·<task>`, `coordinator`) has no *Rename…*: its
//   title is how shprd finds it again, so only a person's own tabs rename.
// - *Restart* restarts the task's agent through the service
//   (`POST /api/action {"action":"task.retry"}`), never by relaunching a program
//   in a tab: the attempt, its worktree and its brief are the engine's. A tab
//   that names no task therefore has no Restart.
// - The app menu's four *Theme: …* rows are dropped on purpose: shprd follows
//   Herdr's theme (the app's own setting) rather than offering its own list, so
//   there is one place a person changes it.
// - *New task…* takes *New space*'s place: shprd's spaces are made by
//   `shprd serve` (one repository, one space a task), so the thing a person adds
//   from here is a task. It opens the Tasks panel's own "+" form.
//
// Shared: the source is `mods/lib/menu.js`; `bin/sync-shared` copies it into every
// mod folder that imports it. Edit the source, never a copy.

import { parseTabName } from "./tabstrip.js";

/** The width each menu opens at, and the sizes its rows take, in px. */
export const MENU = {
  /** A tab's menu (Shepherdr opens it 236 wide, `mods/tabs-tasks/main.js:166`). */
  tab: 236,
  /** The app's menu, opened from the bar's empty part. */
  app: 236,
  /** A pane's menu: the app's pointer popup, at the tab menu's width. */
  pane: 236,
  /** The "+" menu (Shepherdr opens it 250 wide, `mods/tabs-tasks/main.js:208`). */
  new: 250,
  /** The Tasks panel's Group by and More popups. */
  tasks: 200,
  /** One row: `H.lg`, `lib/ui.js`'s menu row. */
  row: 32,
  /** A heading over a group of rows: `lib/ui.js`'s 28. */
  heading: 28,
  /** A separator: 1 px with 4 above and below. */
  sep: 9,
  /** The padding around a menu's rows, each side. */
  pad: 5,
};

/** The "+" menu's fixed words (Shepherdr's `NEW_MENU`, with shprd's New task…). */
export const NEW_WORDS = {
  terminal: "Terminal",
  newSpace: "New space…",
  newTask: "New task…",
  agents: "Start an agent",
  notInstalled: "Not installed",
  loading: "Checking installed agents…",
  failed: "Couldn't load agents",
};

/**
 * The most not-installed programs a menu offers, after the installed ones.
 *
 * Shepherdr's `agentChoices` keeps at most two (`core/menus.js:102`): a menu is a
 * place to start something, not a list of everything a machine lacks.
 */
export const NOT_INSTALLED_SHOWN = 2;

/** A separator row. */
const sep = () => ({ sep: true });

/** The title shprd gives the coordinator's tab (`COORDINATOR_TITLE` in `spawn.rs`). */
export const COORDINATOR_TAB = "coordinator";

/** Whether a tab is the coordinator's, by the title shprd opened it with. */
export function isCoordinator(tab) {
  return String(tab?.name ?? "").trim().toLowerCase() === COORDINATOR_TAB;
}

/**
 * Whether a person may rename a tab: not one shprd titled (`<agent>·<task>`,
 * `coordinator`), because that title is how shprd finds the tab again.
 */
export function renamable(tab) {
  return !parseTabName(tab?.name).titled && !isCoordinator(tab);
}

/**
 * Whether a row can be picked: a heading, a note, a separator and a disabled row
 * cannot, so ↑ and ↓ step over them.
 */
export function pickable(row) {
  return Boolean(row) && !row.sep && !row.heading && !row.note && !row.disabled;
}

/** The index of the first row that can be picked, or 0 when none can. */
export function firstPickable(rows) {
  const at = (rows ?? []).findIndex(pickable);
  return at === -1 ? 0 : at;
}

/**
 * The row ↑ (`delta` -1) or ↓ (+1) lands on from `at`: the next one that can be
 * picked, wrapping at the ends; `at` itself when none can.
 */
export function stepPick(rows, at, delta) {
  const list = rows ?? [];
  const n = list.length;
  for (let i = 1; i <= n; i += 1) {
    const next = (((at + delta * i) % n) + n) % n;
    if (pickable(list[next])) return next;
  }
  return at;
}

/**
 * The installed programs first, then at most two that are not (Shepherdr's order).
 *
 * `programs` is `GET /api/roster`'s `programs` array. It is read, never invented:
 * a program the roster does not name is not a row, so the menu can only offer
 * what the service can start.
 */
export function agentChoices(programs) {
  const list = (programs ?? []).filter((program) => program && program.id);
  const installed = list.filter((program) => program.installed);
  const missing = list.filter((program) => !program.installed);
  return [...installed, ...missing.slice(0, NOT_INSTALLED_SHOWN)];
}

/**
 * The hint at a program row's right: its key for the first nine installed ones,
 * else *Not installed* (Shepherdr's `agentHint`).
 *
 * `index` counts every offered row; the number is what a press starts, so only
 * the installed ones are numbered, and only the first nine — a tenth has no key
 * and says nothing rather than naming one that does nothing.
 */
export function agentHint(program, index) {
  if (!program?.installed) return NEW_WORDS.notInstalled;
  return index < 9 ? String(index + 1) : "";
}

/**
 * The "+" menu's rows: Terminal, New space…, New task…, then *Start an agent* and the roster.
 *
 * `roster` is the service's answer (`{programs}`), or null while it is being read
 * or when it failed — then one dim line says which, in the group's own place, so
 * the menu never looks like an empty one. Rows that start something carry what to
 * run them: a `command` for the app's own verbs and an `action` for what only this
 * window (or the service) can do.
 */
export function newMenu({ roster = null, loading = false, failure = null } = {}) {
  const rows = [
    {
      id: "terminal",
      label: NEW_WORDS.terminal,
      icon: "terminal",
      command: { name: "tab.new", args: {} },
    },
    { id: "new-space", label: NEW_WORDS.newSpace, icon: "plus" },
    { id: "new-task", label: NEW_WORDS.newTask, icon: "plus" },
    { heading: NEW_WORDS.agents },
  ];
  if (failure) return [...rows, { note: failure }];
  if (!roster) return [...rows, { note: loading ? NEW_WORDS.loading : "" }];
  const choices = agentChoices(roster.programs);
  if (choices.length === 0) return [...rows, { note: NEW_WORDS.failed }];
  return [
    ...rows,
    ...choices.map((program, index) => ({
      id: `agent:${program.id}`,
      label: program.name || program.id,
      hint: agentHint(program, index),
      disabled: !program.installed,
      action: "agent",
      program: program.id,
    })),
  ];
}

/**
 * A tab's menu: Rename…, Restart (when the tab can), then Close tab.
 *
 * `restartable` is what `restartable()` answered for the tab, so a terminal or a
 * person's own tab offers no Restart rather than a row that would do nothing.
 */
export function tabMenu({
  id = null,
  restartable = false,
  coordinator = false,
  renamable = true,
} = {}) {
  return [
    ...(renamable ? [{ id: "rename", label: "Rename…", icon: "square-pen" }] : []),
    ...(coordinator
      ? [
          { id: "edit-instructions", label: "Edit instructions…", icon: "square-pen" },
          { id: "switch-agent", label: "Switch agent", icon: "rotate-ccw", hint: "›" },
        ]
      : []),
    ...(restartable
      ? [{ id: "restart", label: "Restart", icon: "rotate-ccw", action: "restart" }]
      : []),
    sep(),
    {
      id: "close",
      label: "Close tab",
      icon: "x",
      command: { name: "tab.close", args: id ? { id } : {} },
    },
  ];
}

/**
 * The *Switch agent* list: the installed programs, the coordinator's own marked.
 *
 * It is the second level of the tab menu, drawn in the same popup with a *Back*
 * row first (Shepherdr's submenu is a side popup, which a popup that is its own
 * window cannot open). Only installed programs are offered: a switch to one the
 * machine lacks could not start.
 */
export function switchAgentMenu({ roster = null, current = null, loading = false } = {}) {
  const rows = [{ id: "back", label: "Back", icon: "back" }, { heading: "Switch agent" }];
  if (!roster) return [...rows, { note: loading ? NEW_WORDS.loading : NEW_WORDS.failed }];
  const installed = (roster.programs ?? []).filter((program) => program?.id && program.installed);
  if (installed.length === 0) return [...rows, { note: NEW_WORDS.failed }];
  return [
    ...rows,
    ...installed.map((program) => ({
      id: `switch:${program.id}`,
      label: program.name || program.id,
      hint: program.id === current ? "Current" : "",
      action: "switch",
      program: program.id,
    })),
  ];
}

/**
 * The app's menu, opened from the bar's empty part: New space…, New task…, then Keybinds.
 *
 * Shepherdr's has *New space*, *Keybinds* and four *Theme: …* rows; New task… and
 * Keybinds are its counterparts here and the themes are left to Herdr's own
 * setting (see this file's header).
 */
export function appMenu() {
  return [
    { id: "new-space", label: NEW_WORDS.newSpace, icon: "plus" },
    { id: "new-task", label: NEW_WORDS.newTask, icon: "plus" },
    sep(),
    { id: "keybinds", label: "Keybinds", icon: "keyboard" },
  ];
}

/**
 * A pane's menu: Split right, Split down, Zoom, Send back, Close pane.
 *
 * *Send back* is Shepherdr's row for a kept sub-agent (`mods/menu-tasks/main.js:50`)
 * and appears only when the terminal has a parent: sending a top-level pane
 * "back" is a row that cannot run. The *Send right-clicks to pane* toggle is
 * Herdr's own and is left to Herdr (this file's header explains why the menus
 * carry no toggle of it).
 *
 * `pane` is the pane frame's `props().pane`; a pane shown outside a tab (a layout
 * terminal block) has none, and then only the rows that need no pane are left —
 * none of these, so the caller opens no menu at all in that case.
 */
export function paneMenu({ pane = null, sendBack = null } = {}) {
  if (!pane) return [];
  return [
    {
      id: "split-right",
      label: "Split right",
      icon: "split-right",
      command: { name: "pane.split", args: { direction: "right", pane } },
    },
    {
      id: "split-down",
      label: "Split down",
      icon: "split-down",
      command: { name: "pane.split", args: { direction: "down", pane } },
    },
    {
      id: "zoom",
      label: "Zoom",
      icon: "zoom",
      command: { name: "pane.zoom", args: { pane } },
    },
    ...(sendBack?.terminal
      ? [
          {
            id: "send-back",
            label: `Send back to ${sendBack.name || sendBack.terminal}`,
            icon: "terminal",
            command: { name: "terminal.park", args: { terminal: sendBack.terminal } },
          },
        ]
      : []),
    {
      id: "close-pane",
      label: "Close pane",
      icon: "x",
      command: { name: "pane.close", args: { pane } },
    },
  ];
}

/**
 * Whether a tab's agent can be restarted: shprd restarts a task's attempt.
 *
 * A restart is `task.retry`, which needs a task and an agent on it. A tab whose
 * title is not `<agent>·<task>` (a person's own, a plain terminal, an agent
 * someone started from the "+" menu) has no attempt to retry, and a tab shprd
 * titled but in which nothing reports as an agent has nothing to restart.
 */
export function restartable(tab, panes) {
  const named = parseTabName(tab?.name);
  if (!named.titled || !named.task) return false;
  return (panes ?? []).some(
    (pane) => pane?.tab === tab?.id && Boolean(pane?.program?.agent),
  );
}

/**
 * The Tasks panel's two popups: *Group by* (For you, Space, Status) and *More*
 * (the status chips that do not fit in the filter row).
 *
 * A row's `pick` is what the menu mod hands back to the panel, through the
 * shared `tasks.choice`, because the popup is its own window and cannot call the
 * panel. `current` marks the row in force with a tick.
 */
export function tasksMenu({ menu = "group", items = [], current = null } = {}) {
  const row = (key, label, count) => ({
    id: `pick-${key}`,
    label,
    hint: current === key ? "✓" : count > 0 ? String(count) : null,
    pick: { menu, key },
  });
  if (menu === "more") return items.map((item) => row(item.key, item.word, item.count));
  return [{ heading: "Group by" }, ...items.map((item) => row(item.key, item.label, 0))];
}

/**
 * A menu's height, from its rows: what `openOverlay` is asked for.
 *
 * Shepherdr computes it the same way at each open site (a tab's
 * `19 + 32 * rows`), because the rows change with what the menu offers. One
 * function serves every menu here, so a height can never disagree with the rows
 * it is about to draw.
 */
export function menuHeight(rows) {
  const inner = (rows ?? []).reduce((sum, row) => {
    if (row?.sep) return sum + MENU.sep;
    if (row?.heading) return sum + MENU.heading;
    return sum + MENU.row;
  }, 0);
  return MENU.pad * 2 + inner + 1;
}
