// Unit tests for Jump's list and the Keybinds words: what the box finds, in what
// order, what a pick does, and the keys as people read them. The data is a real
// service's `GET /api/state` and a real window's space list.

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import { JUMP, jumpItems, jumpRows, jumpTarget } from "../mods/lib/jump.js";
import { KEYBINDS, grouped, keyFound, keyText, nameOf } from "../mods/lib/keybinds.js";

const fixture = (name) =>
  JSON.parse(readFileSync(new URL(`./fixtures/${name}`, import.meta.url), "utf8"));
const STATE = fixture("state.json");
const SPACES = fixture("spaces.json").spaces;
const OTHER = { id: "spx", name: "notes", folder: "/tmp/notes", active: false, tabs: [] };

test("with nothing typed the coordinator and the spaces come first, then every task", () => {
  const rows = jumpRows(jumpItems(STATE, SPACES, ""));
  const heads = rows.filter((r) => r.kind === "head").map((r) => r.label);
  assert.deepEqual(heads, ["Spaces", "Tasks"]);
  const spaces = rows.filter((r) => r.kind === "space");
  // Only the coordinator's: a task's space is reached through its task.
  assert.equal(spaces.length, 1);
  assert.match(spaces[0].sub, /^Coordinator/);
  assert.equal(rows.filter((r) => r.kind === "task").length, STATE.tasks.length);
});

test("a space shprd did not make is a space row of its own", () => {
  const items = jumpItems(STATE, [...SPACES, OTHER], "notes");
  assert.deepEqual(items.map((i) => i.kind), ["space"]);
  assert.equal(items[0].id, "spx");
});

test("typing a task id finds it, and Enter opens its thread", () => {
  const items = jumpItems(STATE, SPACES, "t2");
  const task = items.find((i) => i.kind === "task" && i.id === "T2");
  assert.ok(task, "T2 is found");
  // A task's thread, even when it has a space of its own: an agent is reached
  // through its task, not through a tab.
  assert.deepEqual(jumpTarget(task), { task: "T2", title: task.label });
});

test("a task with no space of its own opens its thread in the Tasks panel", () => {
  const items = jumpItems(STATE, [], "T1");
  const task = items.find((i) => i.id === "T1");
  assert.deepEqual(jumpTarget(task), { task: "T1", title: task.label });
});

test("every word typed must be in the item, in any case and order", () => {
  assert.ok(jumpItems(STATE, SPACES, "SIDEBAR spaces").some((i) => i.id === "T1"));
  assert.equal(jumpItems(STATE, SPACES, "zzzz nothing").length, 0);
});

test("a group shows eight rows, then Show N more, which unfolds it", () => {
  const many = Array.from({ length: 11 }, (_, i) => ({
    id: `X${i}`,
    title: `Task ${i}`,
    status: "working",
    derived: { status: "working" },
  }));
  const items = jumpItems({ tasks: many }, [], "");
  const rows = jumpRows(items);
  assert.equal(rows.filter((r) => r.kind === "task").length, JUMP.shown);
  const more = rows.find((r) => r.kind === "more");
  assert.equal(more.count, 3);
  assert.deepEqual(jumpTarget(more), { more: "task" });
  assert.equal(jumpRows(items, new Set(["task"])).filter((r) => r.kind === "task").length, 11);
});

test("a dropped task is found only by typing", () => {
  const state = { tasks: [{ id: "Z1", title: "old", status: "dropped", derived: { status: "to_do" } }] };
  assert.equal(jumpItems(state, [], "").length, 0);
  assert.equal(jumpItems(state, [], "Z1").length, 1);
});

test("a head is never a target", () => {
  assert.equal(jumpTarget({ kind: "head" }), null);
  assert.equal(jumpTarget(null), null);
});

const LIST = [
  { section: "global", keys: "alt-1", action: "tab.select:1" },
  { section: "global", keys: "alt-2", action: "tab.select:2" },
  { section: "global", keys: "alt-up", action: "space.prev" },
  { section: "global", keys: "ctrl-k", action: "mod.open:shprd-jump" },
  { section: "global", keys: "ctrl-shift-b", action: "layout.toggle:sidebar" },
  { section: "layout:shprd", keys: "ctrl-shift-b", action: "layout.toggle:panel" },
  { section: "layout:shprd", keys: "ctrl-shift-n", action: "shprd-tabs.new-task" },
  { section: "layout:other", keys: "ctrl-shift-x", action: "pane.close" },
  { section: "terminal", keys: "alt-left", action: "send:\u001bb" },
  { section: "terminal", keys: "ctrl-1", action: "terminal" },
];

test("every binding in words, grouped, the shown layout's key replacing the global one", () => {
  const groups = grouped(LIST, { layout: "shprd" });
  assert.deepEqual(groups.map((g) => g.group), ["Tabs", "Spaces", "Tasks", "Terminal", "App"]);
  const tabs = groups.find((g) => g.group === "Tabs");
  assert.deepEqual(tabs.rows, [{ name: "Show tab 1–9", keys: ["Alt+1–9"] }]);
  const tasks = groups.find((g) => g.group === "Tasks");
  assert.deepEqual(
    tasks.rows.map((r) => [r.name, r.keys]),
    [["Show or hide the side panel", ["Ctrl+Shift+B"]], ["New task", ["Ctrl+Shift+N"]]],
  );
  const app = groups.find((g) => g.group === "App");
  assert.deepEqual(app.rows.map((r) => r.name), ["Jump to a task, space or the coordinator"]);
  // Another layout's keys and a terminal's passthrough are not listed.
  assert.ok(!JSON.stringify(groups).includes("Close pane"));
  assert.ok(!JSON.stringify(groups).includes("Ctrl+1"));
});

test("the keys read as people read them, on macOS too", () => {
  assert.equal(keyText("ctrl-shift-/"), "Ctrl+Shift+/");
  assert.equal(keyText("alt-escape"), "Alt+Esc");
  assert.equal(keyText("ctrl-alt-left", true), "⌃⌥Left");
  assert.equal(nameOf("pane.go:left"), "Focus the pane left");
  assert.equal(nameOf("mod.open:shprd-keybinds"), "Keybinds");
});

test("typing filters the rows by group, name or keys", () => {
  const row = { name: "Next space", keys: ["Alt+Down"] };
  assert.ok(keyFound("", "Spaces", row));
  assert.ok(keyFound("space alt", "Spaces", row));
  assert.ok(!keyFound("tab", "Spaces", row));
  assert.equal(KEYBINDS.none, "No keys match.");
});
