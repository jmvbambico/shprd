// Unit tests for the tab bar's menus: the rows the "+" menu, a tab's menu, the
// app's menu and a pane's menu offer, in the order and the words they are drawn.
//
//   node --import ./test/boot.mjs --test test/          (node 22+)
//   bun test --preload ./test/bun-boot.mjs test/        (bun)
//
// The rows are pure data, so nothing here needs Herdr or a service: the roster is
// a real `GET /api/roster` (`test/fixtures/roster.json`) and the tabs are a real
// window's (`test/fixtures/spaces.json`), and the assertions are the words a
// person reads.

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import {
  MENU,
  NEW_WORDS,
  NOT_INSTALLED_SHOWN,
  agentChoices,
  agentHint,
  appMenu,
  menuHeight,
  newMenu,
  paneMenu,
  restartable,
  tabMenu,
} from "../mods/lib/menu.js";

/** The roster a real service answered. */
const ROSTER = JSON.parse(
  readFileSync(new URL("./fixtures/roster.json", import.meta.url), "utf8"),
);

/** One space's tabs, from a real window's list. */
const SPACES = JSON.parse(
  readFileSync(new URL("./fixtures/spaces.json", import.meta.url), "utf8"),
).spaces;

/** The space whose tabs hold a worker and a reviewer. */
const SPACE = SPACES.find((space) => space.tabs.some((tab) => tab.name.endsWith("·review")));

/** The roster with `missing` program names not installed, in its own order. */
function rosterWithout(...missing) {
  return {
    programs: ROSTER.programs.map((program) => ({
      ...program,
      installed: !missing.includes(program.id),
    })),
  };
}

/** The rows' labels, in order, with headings marked. */
const words = (rows) =>
  rows.map((row) => (row.sep ? "—" : row.heading ? `# ${row.heading}` : row.label));

test("the + menu offers Terminal, New task, then the roster", () => {
  const rows = newMenu({ roster: ROSTER });
  assert.deepEqual(words(rows), [
    NEW_WORDS.terminal,
    NEW_WORDS.newTask,
    `# ${NEW_WORDS.agents}`,
    "Claude Code",
    "Codex",
    "Kilo",
    "OpenCode",
    "Pi",
  ]);
  // The installed ones are numbered 1–9, which is the key that starts them.
  assert.deepEqual(
    rows.filter((row) => row.action === "agent").map((row) => row.hint),
    ["1", "2", "3", "4", "5"],
  );
  // The fixed rows carry what they run; the terminal's space is the caller's.
  assert.deepEqual(rows[0].command, { name: "tab.new", args: {} });
  assert.equal(rows[1].id, "new-task");
  assert.equal(rows[1].command, undefined, "New task… is this window's, not a command");
});

test("a program this machine lacks is offered dimmed and says so", () => {
  const rows = newMenu({ roster: rosterWithout("codex") });
  const codex = rows.find((row) => row.id === "agent:codex");
  assert.equal(codex.disabled, true);
  assert.equal(codex.hint, NEW_WORDS.notInstalled);
  // The installed ones keep counting from one, so a key never starts the wrong
  // program: the missing one is not numbered.
  const pi = rows.find((row) => row.id === "agent:pi");
  assert.equal(pi.hint, "4");
  assert.equal(pi.disabled, false);
});

test("at most two programs that are not installed are offered, after the rest", () => {
  const roster = rosterWithout("claude", "codex", "kilo");
  const choices = agentChoices(roster.programs);
  assert.deepEqual(
    choices.map((program) => program.id),
    ["opencode", "pi", "claude", "codex"],
    "the installed ones first, then the first two missing",
  );
  assert.equal(choices.filter((program) => !program.installed).length, NOT_INSTALLED_SHOWN);
  // The installed ones get the keys, whatever their place in the table.
  assert.equal(agentHint(choices[0], 0), "1");
  assert.equal(agentHint(choices[2], 2), NEW_WORDS.notInstalled);
});

test("the roster's absence or failure is a line, not an empty menu", () => {
  const loading = newMenu({ loading: true }).at(-1);
  assert.equal(loading.note, NEW_WORDS.loading);
  // A copy that has not been told either way says nothing rather than a guess.
  assert.equal(newMenu({}).at(-1).note, "");
  const failed = newMenu({ failure: "shprd serve is not running" }).at(-1);
  assert.equal(failed.note, "shprd serve is not running");
  const empty = newMenu({ roster: { programs: [] } }).at(-1);
  assert.equal(empty.note, NEW_WORDS.failed);
});

test("a tab's menu is Rename, Restart when it can, then Close tab", () => {
  const tab = SPACE.tabs.at(-1);
  const plain = tabMenu({ id: tab.id, restartable: false });
  assert.deepEqual(words(plain), ["Rename…", "—", "Close tab"]);
  assert.deepEqual(plain.at(-1).command, { name: "tab.close", args: { id: tab.id } });
  const restarted = tabMenu({ id: tab.id, restartable: true });
  assert.deepEqual(words(restarted), ["Rename…", "Restart", "—", "Close tab"]);
  const restart = restarted.find((row) => row.id === "restart");
  assert.equal(restart.action, "restart", "Restart is the service's retry, not a command");
  assert.equal(restart.command, undefined);
});

test("only a tab shprd titled, with an agent in it, can be restarted", () => {
  const worker = SPACE.tabs[0];
  const panes = [{ tab: worker.id, program: { agent: true } }];
  assert.equal(restartable(worker, panes), true);
  // A person's own tab has no attempt to retry.
  assert.equal(restartable({ id: "tb1", name: "zsh" }, panes), false);
  // A shprd-titled tab with nothing reporting as an agent has nothing to restart.
  assert.equal(
    restartable({ id: "tb1", name: "claude·T1" }, [{ tab: "tb1", program: { agent: false } }]),
    false,
  );
});

test("the app's menu is New task… and Keybinds, with no theme rows", () => {
  assert.deepEqual(words(appMenu()), ["New task…", "—", "Keybinds"]);
  assert.equal(
    appMenu().some((row) => String(row.label).startsWith("Theme")),
    false,
    "the theme is Herdr's own setting, not a row here",
  );
});

test("a pane's menu splits, zooms, closes, and sends back only a kept sub-agent", () => {
  const plain = paneMenu({ pane: "p12" });
  assert.deepEqual(words(plain), ["Split right", "Split down", "Zoom", "Close pane"]);
  assert.deepEqual(plain[0].command, {
    name: "pane.split",
    args: { direction: "right", pane: "p12" },
  });
  assert.deepEqual(plain[1].command, {
    name: "pane.split",
    args: { direction: "down", pane: "p12" },
  });
  assert.deepEqual(plain.at(-1).command, { name: "pane.close", args: { pane: "p12" } });

  const kept = paneMenu({
    pane: "p12",
    sendBack: { name: "assistant", terminal: "t7" },
  });
  assert.deepEqual(words(kept).slice(3, 4), ["Send back to assistant"]);
  assert.deepEqual(kept[3].command, { name: "terminal.park", args: { terminal: "t7" } });

  // A terminal shown outside a tab has no pane: there is no menu to open.
  assert.deepEqual(paneMenu({ pane: null }), []);
});

test("a menu's height is its rows' own, so a popup is never taller than its content", () => {
  // Three rows and a separator: the padding, twice, then the rows, then padding.
  const rows = appMenu();
  assert.equal(
    menuHeight(rows),
    MENU.pad * 2 + MENU.row * 2 + MENU.sep + 1,
  );
  const plus = newMenu({ roster: ROSTER });
  assert.equal(
    menuHeight(plus),
    MENU.pad * 2 + MENU.row * 7 + MENU.heading + 1,
    "two fixed rows, a heading and five programs",
  );
  assert.equal(menuHeight([]), MENU.pad * 2 + 1);
});
