// Chat panes are pure chat; the coordinator's tab is its role's (T32).
//
//   node --import ./test/boot.mjs --test test/

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import * as herdr from "herdr";
import { record, walk } from "gpui-kit";
import Pane from "../mods/shprd-pane/main.js";
import { needTask } from "../mods/lib/asks.js";
import { openNeeds } from "../mods/lib/panel.js";
import { coordinatorProgram, tabRows } from "../mods/lib/tabstrip.js";

/** The text of a node's own string children. */
const words = (node) => node.kids.filter((kid) => typeof kid === "string");

test("no chat pane draws a needs-you strip", () => {
  for (const file of ["../mods/shprd-chat/main.js", "../mods/lib/asks.js"]) {
    const source = readFileSync(new URL(file, import.meta.url), "utf8");
    assert.ok(!/chat-strip|stripItems|STRIP_OPEN/.test(source), `${file} has no strip`);
  }
});

test("the pane header keeps its title and folder and draws no icon", () => {
  herdr.reset();
  herdr.setProps({ pane: "chat1" });
  herdr.setSpaces([{ id: "sp", name: "shprd · shop", tabs: [{ id: "tab1" }] }]);
  herdr.setPanes([{ pane: "chat1", mod: "shprd-chat", tab: "tab1" }]);
  herdr.setTerminals([
    {
      address: "t1",
      state: "live",
      labels: { "shprd.chat": "chat1" },
      cwd: "/work/shop",
      program: { agent: true, name: "Claude", icon: "claude.png" },
    },
  ]);
  record(true);
  let tree;
  try {
    tree = new Pane().render({ notify() {}, spawn() {} });
  } finally {
    record(false);
  }
  const [first, second] = tree.__node.kids.map((kid) => kid.__node);
  assert.ok(words(first).length === 1, "the first thing in the header is the title, not an icon");
  assert.match(words(first)[0], /\S/);
  assert.match(words(second).join(""), /shop/, "the folder follows the title");
});

test("the Needs you badge and pill open the Tasks pane entry", async () => {
  herdr.reset();
  openNeeds("T2");
  assert.equal(herdr.SHARED.get("tasks.focus").task, "T2");
  const selects = herdr.RUNS.filter((r) => r.name === "layout.select").map((r) => r.args.child);
  assert.deepEqual(selects, ["tasks", "tasks-list"]);
  assert.ok(herdr.RUNS.some((r) => r.name === "layout.show"));

  const state = { tasks: [{ id: "T7", title: "x", derived: { status: "needs_you" } }], asks: [] };
  assert.equal(needTask(state, { asks: [] }), "T7", "a needs-you task with no card yet");
  assert.equal(needTask({ tasks: [], asks: [] }, { asks: [] }), null);
});

const SPACE = { id: "sp", tabs: [{ id: "shprd-coordinator-sp", name: "shprd", active: true }] };
const ICONS = { claude: { icon: "claude.svg", tint: false }, codex: { icon: "codex.svg", tint: true } };
const settings = (agent) => ({ values: [{ key: "roles.coordinator.agent", value: agent }] });

test("the coordinator's tab is named by its role and wears its harness's icon", () => {
  const state = { role_titles: {} };
  let [row] = tabRows(SPACE, [], state, coordinatorProgram(settings("claude"), ICONS));
  assert.equal(row.label, "Assistant", "the default role name, not the repository");
  assert.equal(row.icon, "claude.svg");
  assert.equal(row.tint, false);

  // Renamed and switched in Settings: the next read follows both.
  [row] = tabRows(SPACE, [], { role_titles: { coordinator: "Orchestrator" } }, coordinatorProgram(settings("codex"), ICONS));
  assert.equal(row.label, "Orchestrator");
  assert.equal(row.icon, "codex.svg");
  assert.equal(row.tint, true);

  [row] = tabRows(SPACE, [], state, coordinatorProgram(settings("nope"), ICONS));
  assert.equal(row.icon, null, "a harness with no icon draws none");
});
