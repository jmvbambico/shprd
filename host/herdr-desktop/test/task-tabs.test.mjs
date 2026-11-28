// A task's chat as a tab in the main pane (T29): a click opens one tab per task,
// a repeat click reuses it, closing leaves the worker, an ended task's tab closes,
// the other entry points open no tab, a waiting ask focuses its card, and the tab
// wears the icon of the harness on the task.
//
//   node --import ./test/boot.mjs --test test/

import test, { beforeEach } from "node:test";
import assert from "node:assert/strict";

import * as herdr from "herdr";
import { activeTaskId, openTaskChat, openNeeds, openThread, sweepTaskTabs, taskTabOf } from "../mods/lib/panel.js";
import { herdrSpaceOf } from "../mods/lib/tree.js";
import { tabRows } from "../mods/lib/tabstrip.js";

const SPACES = [
  { id: "sp1", folder: "/work/shop", active: true, tabs: [{ id: "tab-main", name: "Assistant", active: true }] },
  { id: "sp2", folder: "/work/api", active: false, tabs: [{ id: "tab-api", name: "Assistant", active: false }] },
];
const STATE = {
  spaces: [
    { id: "shop", name: "shop", folder: "/work/shop" },
    { id: "api", name: "api", folder: "/work/api" },
  ],
  tasks: [{ id: "T1", space: "api", status: "running", derived: { status: "working" }, attempts: [{ n: 1, agent: "claude", reviews: [] }] }],
};
const tabsOf = () => herdr.RUNS.filter((r) => r.name === "tab.new");

beforeEach(() => {
  herdr.reset();
  herdr.setPanes([]);
  herdr.setSpaces(structuredClone(SPACES));
});

/** The window after `tab.new`: a chat pane for the task in a tab of its space, shown or not. */
function showTab(task, tab = "tab-t", { active = false } = {}) {
  herdr.setPanes([{ pane: `p-${tab}`, mod: "shprd-chat", tab, props: { space: task } }]);
  const spaces = structuredClone(SPACES);
  spaces[1].tabs.push({ id: tab, name: task, active });
  if (active) {
    spaces[0].active = false;
    spaces[1].active = true;
    spaces[1].tabs[0].active = false;
  }
  herdr.setSpaces(spaces);
}

test("a click opens the task's chat as one tab in its repo space, titled with the id, composer focused", () => {
  openTaskChat({ task: "T1", herdr: herdrSpaceOf(STATE, "api", herdr.spaces()) });
  assert.deepEqual(herdr.RUNS.map((r) => r.name), ["space.select", "tab.new"], "the repo's space is shown first");
  assert.deepEqual(herdr.RUNS[0].args, { id: "sp2" });
  const made = tabsOf();
  assert.equal(made.length, 1);
  assert.equal(made[0].args.name, "T1", "its title is the task id");
  assert.equal(made[0].args.mod, "shprd-chat");
  assert.deepEqual(JSON.parse(made[0].args.props), { space: "T1" }, "fixed to the task, not the following thread");
  assert.equal(made[0].args.focus, true);
  assert.equal(herdr.shared("shprd.chat.composer").space, "T1", "the composer takes the keyboard");
  assert.equal(herdr.shared("shprd.chat.focus"), null, "no card unless an ask waits");
});

test("a repeat click selects the existing tab and never makes a second", () => {
  openTaskChat({ task: "T1", herdr: "sp2" });
  showTab("T1");
  herdr.RUNS.length = 0;
  assert.equal(taskTabOf("T1"), "tab-t", "keyed on the chat pane's props");
  openTaskChat({ task: "T1", herdr: "sp2" });
  openTaskChat({ task: "T1", herdr: "sp2" });
  assert.equal(tabsOf().length, 0);
  assert.deepEqual(herdr.RUNS.map((r) => r.name), ["tab.select", "tab.select"]);
  assert.deepEqual(herdr.RUNS[0].args, { id: "tab-t" });
});

test("closing the tab leaves the worker alone, and a new click reopens it", () => {
  showTab("T1");
  herdr.setPanes([]);
  herdr.setSpaces(structuredClone(SPACES));
  herdr.RUNS.length = 0;
  openTaskChat({ task: "T1", herdr: "sp2" });
  assert.equal(tabsOf().length, 1, "the live task opens again");
  const ending = herdr.RUNS.filter((r) => /agent|terminal|kill|stop|drop/.test(r.name));
  assert.deepEqual(ending, [], "nothing touches the worker");
  assert.deepEqual(herdr.HC_CALLS, []);
});

test("an ended task's tab closes when it is not shown, and once the person leaves it when it is", () => {
  showTab("T1", "tab-t", { active: false });
  const landed = { tasks: [{ id: "T1", status: "merged", derived: { status: "done" } }] };
  assert.deepEqual(sweepTaskTabs(landed), ["T1"]);
  assert.deepEqual(herdr.RUNS, [{ name: "tab.close", args: { id: "tab-t" } }]);

  herdr.RUNS.length = 0;
  showTab("T1", "tab-t", { active: true });
  assert.equal(activeTaskId(), "T1");
  assert.deepEqual(sweepTaskTabs(landed), [], "the shown tab stays");
  showTab("T1", "tab-t", { active: false });
  assert.deepEqual(sweepTaskTabs(landed), ["T1"], "closed once left");

  herdr.RUNS.length = 0;
  assert.deepEqual(sweepTaskTabs({ tasks: [{ id: "T1", status: "dropped", derived: { status: "done" } }] }), ["T1"], "dropped");
  herdr.RUNS.length = 0;
  herdr.setPanes([]);
  openTaskChat({ task: "T1", herdr: "sp2" });
  herdr.RUNS.length = 0;
  showTab("T1");
  assert.deepEqual(sweepTaskTabs({ tasks: [] }), ["T1"], "cleared (gone from the board)");
  herdr.RUNS.length = 0;
  assert.deepEqual(sweepTaskTabs(STATE), [], "a live task's tab stays");
  assert.deepEqual(sweepTaskTabs({}), [], "an unread board closes nothing");
});

test("the other entry points never open a tab", () => {
  openThread({ task: "T1", title: "" }, "chat");
  openNeeds("T1");
  openNeeds(null);
  assert.deepEqual(tabsOf(), []);
  assert.ok(!herdr.RUNS.some((r) => r.name === "tab.select"));
});

test("a worker's ask: its entry opens the tab at the card, or focuses the open tab there", () => {
  // Tab not open.
  openTaskChat({ task: "T1", herdr: "sp2" }, { card: true });
  assert.equal(tabsOf().length, 1);
  assert.equal(herdr.shared("shprd.chat.focus").space, "T1");
  // Tab already open.
  showTab("T1");
  herdr.RUNS.length = 0;
  const before = herdr.shared("shprd.chat.focus").at;
  openTaskChat({ task: "T1", herdr: "sp2" }, { card: true });
  assert.deepEqual(herdr.RUNS.map((r) => r.name), ["tab.select"], "focused, not doubled");
  assert.equal(herdr.shared("shprd.chat.focus").space, "T1");
  assert.ok(herdr.shared("shprd.chat.focus").at >= before, "a fresh request for the card");
});

test("the tab wears the icon of the harness on the task: the worker's while working, the reviewer's in review", () => {
  showTab("T1");
  const icons = { claude: { icon: "claude.svg", tint: false }, codex: { icon: "codex.svg", tint: true } };
  const iconOf = (state) => tabRows(herdr.spaces()[1], herdr.panes(), state, null, icons).find((row) => row.id === "tab-t");

  const working = structuredClone(STATE);
  assert.equal(iconOf(working).icon, "claude.svg");
  assert.equal(iconOf(working).label, "T1");

  const review = structuredClone(STATE);
  review.tasks[0].attempts[0].reviews = [{ reviewer: "codex" }];
  review.tasks[0].derived.status = "working";
  assert.equal(iconOf(review).icon, "codex.svg", "in review: the reviewer's");
  assert.equal(iconOf(review).tint, true);

  review.tasks[0].attempts[0].gates = [{ gate: "review" }];
  assert.equal(iconOf(review).icon, "claude.svg", "back with the worker once the review is done");

  const live = structuredClone(STATE);
  live.live_agents = [{ id: "a", task: "T1", role: "reviewer", program: "codex", state: "working" }];
  assert.equal(iconOf(live).icon, "codex.svg", "a live reviewer agent is the same answer");
});

// SHP-7: a restart opens no tab by itself, a stale request is dropped, a landed task's tab closes at startup, a task tab selects on click.
import { clearStaleRequests, staleRequests } from "../mods/lib/panel.js";
import { pillRelease } from "../mods/lib/tabstrip.js";

test("a restart with a stale focus key opens no tab and clears the key", () => {
  herdr.setShared("shprd.chat.focus", { space: "T34", at: Date.now() - 86_400_000 });
  herdr.setShared("shprd.chat.composer", { space: "T34", at: Date.now() - 86_400_000 });
  assert.deepEqual(clearStaleRequests(STATE).sort(), ["shprd.chat.composer", "shprd.chat.focus"]);
  assert.equal(herdr.shared("shprd.chat.focus"), null);
  assert.equal(herdr.shared("shprd.chat.composer"), null);
  assert.deepEqual(tabsOf(), [], "no tab opened");
  assert.ok(!herdr.RUNS.some((r) => r.name === "tab.new" || r.name === "tab.select"));
});

test("a request naming a landed, dropped or unknown task is ignored and cleared, a live fresh one stays", () => {
  const now = 1_000_000;
  const tasks = [
    { id: "T1", status: "running", derived: { status: "working" } },
    { id: "T2", status: "merged", derived: { status: "done" } },
    { id: "T3", status: "dropped" },
  ];
  const at = now - 1000;
  const entries = { a: { space: "T1", at }, b: { space: "T2", at }, c: { space: "T3", at }, d: { space: "T9", at }, e: null, f: { space: "coordinator", at } };
  assert.deepEqual(staleRequests(tasks, entries, now), ["b", "c", "d"]);
});

test("a landed task's restored tab is closed at startup, even when it is the shown one", () => {
  const spaces = structuredClone(SPACES);
  spaces[0].tabs.push({ id: "tab-t34", name: "T34", active: true });
  spaces[0].tabs[0].active = false;
  herdr.setSpaces(spaces);
  herdr.setPanes([{ pane: "p", mod: "shprd-chat", tab: "tab-t34", props: { space: "T34" } }]);
  const landed = { tasks: [{ id: "T34", status: "merged", derived: { status: "done" } }] };
  assert.deepEqual(sweepTaskTabs(landed), [], "a later sweep keeps the shown tab");
  assert.deepEqual(sweepTaskTabs(landed, { startup: true }), ["T34"]);
  assert.deepEqual(herdr.RUNS, [{ name: "tab.select", args: { id: "tab-main" } }, { name: "tab.close", args: { id: "tab-t34" } }]);
});

test("a task tab selects on click, even when tabs shifted between press and release", () => {
  const rows = [{ id: "coord", index: 0 }, { id: "tab-t34", index: 1 }];
  assert.deepEqual(pillRelease(rows[1], rows[1], rows), { select: "tab-t34" });
  assert.deepEqual(pillRelease(null, rows[1], rows), { select: "tab-t34" }, "no remembered press");
  // The tab pressed closed meanwhile (an ended tab swept): a click, not a move.
  assert.deepEqual(pillRelease({ id: "gone", index: 1 }, rows[1], rows), { select: "tab-t34" });
  assert.deepEqual(pillRelease(rows[0], rows[1], rows), { move: "coord", index: 2 }, "a real drag still moves");
});
