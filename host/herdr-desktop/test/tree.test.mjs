// The sidebar tree, the Agents pane's rows and the Graph's rows, over the
// prototype's fixture (`fixtures/proto.json`).

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import { age, agentLine, agentRows, agentsOfTask, graphRows, spaceTree, spacesOf } from "../mods/lib/tree.js";

const NOW = 1_800_000_000_000;
const state = JSON.parse(readFileSync(new URL("./fixtures/proto.json", import.meta.url), "utf8"));
for (const agent of state.live_agents) agent.since_ms = NOW - agent.since_s * 1000;

test("age is the shortest honest words", () => {
  assert.equal(age(NOW - 10_000, NOW), "now");
  assert.equal(age(NOW - 240_000, NOW), "4m");
  assert.equal(age(NOW - 3 * 3600_000, NOW), "3h");
  assert.equal(age(NOW - 72 * 3600_000, NOW), "3d");
});

test("an agent's line says its state and how long, and 'ago' once it is done", () => {
  assert.equal(agentLine({ state: "working", since_ms: NOW - 240_000 }, NOW), "working · 4m");
  assert.equal(agentLine({ state: "needs_you", since_ms: NOW - 60_000 }, NOW), "needs you · 1m");
  assert.equal(agentLine({ state: "done", since_ms: NOW - 720_000 }, NOW), "done · 12m ago");
});

test("the tree has a space per repo, its branch, and its tasks needs-you first", () => {
  const tree = spaceTree(state);
  assert.deepEqual(tree.map((space) => space.name), ["shprd", "shop", "relay"]);
  assert.equal(tree[0].branch, "integration/m11");
  assert.equal(tree[0].tasks[0].id, "SHPR-23", "needs you leads");
  assert.equal(tree[0].tasks.at(-1).status, "done");
  assert.equal(tree[0].tasks.filter((task) => task.last).length, 1);
  assert.deepEqual([tree[0].needs, tree[0].ready], [1, 1]);
});

test("a working task's second line is its worker and progress; others have none", () => {
  const tasks = spaceTree(state)[0].tasks;
  assert.equal(tasks.find((task) => task.id === "SHPR-22").sub, "claude · 62%");
  assert.equal(tasks.find((task) => task.id === "SHPR-26").sub, "pi · 35%");
  assert.equal(tasks.find((task) => task.id === "SHPR-24").sub, null);
});

test("a state with no spaces still draws one, from its run", () => {
  const bare = { run: { repo: "/x/shop", integration_branch: "integration/m1" }, tasks: [{ id: "T1", title: "a", derived: { status: "working" } }] };
  assert.equal(spacesOf(bare)[0].name, "shop");
  assert.equal(spaceTree(bare)[0].tasks.length, 1);
});

test("agents: needs you first, with role, program and state", () => {
  const rows = agentRows(state, { now: NOW });
  assert.equal(rows.length, 8);
  assert.equal(rows[0].state, "needs_you");
  const worker = rows.find((row) => row.task === "SHPR-22");
  assert.deepEqual([worker.role, worker.program, worker.line], ["Worker", "claude", "working · 4m"]);
  const coordinator = rows.find((row) => row.role === "Coordinator" && row.space === "sp-shprd");
  assert.equal(coordinator.name, "shprd");
  assert.equal(agentsOfTask(state, "SHPR-23")[0].program, "codex");
});

test("grouped agents put a heading per space before its rows", () => {
  const rows = agentRows(state, { grouped: true, now: NOW });
  assert.deepEqual(rows.filter((row) => row.kind === "head").map((row) => [row.name, row.count]), [["shprd", 5], ["shop", 2], ["relay", 1]]);
  assert.equal(rows[0].kind, "head");
  assert.equal(rows.length, 8 + 3);
});

test("the graph groups by phase, hangs a task under its last dependency and says what it waits for", () => {
  const rows = graphRows(state, "sp-shprd");
  assert.deepEqual(rows.filter((row) => row.kind === "phase").map((row) => row.name), ["SIDEBAR", "SERVER"]);
  const word = (id) => rows.find((row) => row.id === id).word;
  assert.equal(word("SHPR-21"), "done");
  assert.equal(word("SHPR-22"), "working");
  assert.equal(word("SHPR-23"), "needs you");
  assert.equal(word("SHPR-24"), "waits SHPR-23");
  assert.equal(word("SHPR-27"), "waits SHPR-26 SHPR-22");
  const at = (id) => rows.findIndex((row) => row.id === id);
  assert.ok(at("SHPR-24") > at("SHPR-23"), "a dependent follows what it waits for");
  assert.match(rows[at("SHPR-24")].guide, /└─/);
});
