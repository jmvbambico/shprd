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
// - There is no *Edit instructions…* row (a coordinator's) and no *Switch agent*
//   row (a coordinator's). shprd has no coordinator tab: its coordinator is the
//   sidebar block (M10.8), and the per-task chat that replaces it is U4.
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
export function tabMenu({ id = null, restartable = false } = {}) {
  return [
    { id: "rename", label: "Rename…", icon: "square-pen" },
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
