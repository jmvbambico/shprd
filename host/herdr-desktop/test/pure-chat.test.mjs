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

test("a kilo agent gets the Kilo icon, and Herdr's own icon still wins", async () => {
  const { harnessIcons } = await import("../mods/lib/panel.js");
  const known = [{ key: "claude", icon: "claude.svg", tint: false }, { key: "codex", icon: "codex.svg", tint: true }];
  const icons = harnessIcons(known);
  assert.equal(icons.kilo.icon, "mods/shprd-tabs/icons/kilo.svg");
  assert.equal(icons.kilo.tint, true);
  assert.equal(icons.claude.icon, "claude.svg");
  assert.ok(readFileSync(new URL("../" + icons.kilo.icon, import.meta.url), "utf8").includes("<svg"), "the file exists");
  // A kilo agent's tab, and the settings' program tabs, read the same map.
  const [row] = tabRows(SPACE, [], { role_titles: {} }, coordinatorProgram(settings("kilo"), icons));
  assert.equal(row.icon, "mods/shprd-tabs/icons/kilo.svg");
  assert.equal(harnessIcons([{ key: "kilo", icon: "theirs.svg" }]).kilo.icon, "theirs.svg");
  assert.equal(harnessIcons(null).kilo.icon, "mods/shprd-tabs/icons/kilo.svg", "Herdr failing leaves kilo's");
});

test("a kilo agent in the Agents pane is drawn with the Kilo icon", async () => {
  const { default: Agents } = await import("../mods/shprd-agents/main.js");
  const { harnessIcons } = await import("../mods/lib/panel.js");
  const { agentRows } = await import("../mods/lib/tree.js");
  const NOW = 1_000_000;
  const state = {
    spaces: [{ id: "sp", name: "shprd" }],
    tasks: [{ id: "T1", title: "x", space: "sp", derived: { status: "working" } }],
    live_agents: [
      { id: "a1", space: "sp", task: "T1", role: "worker", program: "kilo", state: "working", since_ms: NOW - 5000 },
      { id: "a2", space: "sp", task: "T1", role: "reviewer", program: "claude", state: "working", since_ms: NOW - 5000 },
    ],
  };
  // Herdr knows claude only; kilo's icon is shprd's own.
  const icons = harnessIcons([{ key: "claude", icon: "claude.svg", tint: false }]);
  const rows = agentRows(state, { icons, now: NOW });
  const kilo = rows.find((r) => r.program === "kilo");
  assert.equal(kilo.icon, "mods/shprd-tabs/icons/kilo.svg");
  assert.equal(rows.find((r) => r.program === "claude").icon, "claude.svg");

  // Render the pane's real row and find the icon element it draws.
  const pane = Object.create(Agents.prototype);
  pane.picked = null;
  record(true);
  let tree;
  try {
    tree = pane.row(herdr.theme(), { red: "#f00" }, kilo);
  } finally {
    record(false);
  }
  const drawn = walk(tree).map((n) => n.src).filter(Boolean);
  assert.ok(drawn.includes("mods/shprd-tabs/icons/kilo.svg"), `the row draws the Kilo icon (drew ${drawn})`);
});
