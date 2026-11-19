// The sidebar tree, the Agents pane's rows and the Graph's rows, over the
// prototype's fixture (`fixtures/proto.json`).

import test from "node:test";
import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";

import { age, agentKept, agentLine, agentRows, agentsOfTask, graphRows, plainSpaces, scopeToSpace, spaceTree, spacesOf } from "../mods/lib/tree.js";

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

test("the tree has a space per repo and only its live tasks, needs you first", () => {
  const tree = spaceTree(state, [], NOW);
  assert.deepEqual(tree.map((space) => space.name), ["shprd", "shop", "relay"]);
  assert.equal(tree[0].branch, "integration/m11");
  // To do, queued and done tasks are the Tasks panel's, not the tree's.
  assert.deepEqual(tree[0].tasks.map((task) => task.id), ["SHPR-23", "SHPR-22", "SHPR-26", "SHPR-28"]);
  assert.equal(tree[0].tasks.filter((task) => task.last).length, 1);
  assert.deepEqual([tree[0].needs, tree[0].ready], [1, 1]);
  assert.deepEqual(tree[1].tasks.map((task) => task.id), ["SHOP-2"]);
});

test("a queued task with an agent on it is in the tree; one without is not", () => {
  const queued = { ...state, tasks: state.tasks.map((task) => (task.id === "SHPR-24" ? { ...task } : task)) };
  assert.ok(!spaceTree(queued, [], NOW)[0].tasks.some((task) => task.id === "SHPR-24"));
  queued.live_agents = [...state.live_agents, { id: "a9", space: "sp-shprd", task: "SHPR-24", role: "worker", program: "pi", state: "working", since_ms: NOW - 5000 }];
  assert.ok(spaceTree(queued, [], NOW)[0].tasks.some((task) => task.id === "SHPR-24"));
});

test("a task row's second line says who is on it and where it stands", () => {
  const tasks = spaceTree(state, [], NOW)[0].tasks;
  const line = (id) => tasks.find((task) => task.id === id).sub;
  assert.equal(line("SHPR-22"), "claude · working · 4m · 62%");
  assert.equal(line("SHPR-26"), "pi · working · 9m · 35%");
  assert.equal(line("SHPR-23"), "needs you");
  assert.equal(line("SHPR-28"), "ready to approve");
  assert.equal(spaceTree(state, [], NOW)[2].tasks[0].sub, "asks you · 3m ago");
});

test("a queued task whose agent already works reads working, not its raw status", () => {
  const queued = { ...state, live_agents: [...state.live_agents, { id: "a9", space: "sp-shprd", task: "SHPR-24", role: "worker", program: "pi", state: "working", since_ms: NOW - 5000 }] };
  assert.equal(spaceTree(queued, [], NOW)[0].tasks.find((task) => task.id === "SHPR-24").sub, "pi · working · now");
});

test("a running review shows as the reviewer's line", () => {
  const live = { ...state, live_agents: [...state.live_agents, { id: "r1", space: "sp-shop", task: "SHOP-2", role: "reviewer", program: "codex", state: "working", since_ms: NOW - 60_000 }] };
  assert.equal(spaceTree(live, [], NOW)[1].tasks[0].sub.split(" · ").slice(0, 2).join(" · "), "codex · reviewing");
});

test("spaces Herdr made show as plain rows; the served repo's own space does not repeat", () => {
  const herdr = [
    { id: "s1", name: "shprd · shprd", folder: "/x/shprd" },
    { id: "s2", name: "notes", folder: "/home/me/notes", active: true },
  ];
  const tree = spaceTree(state, herdr, NOW);
  assert.deepEqual(tree.map((space) => space.name), ["shprd", "shop", "relay", "notes"]);
  const plain = tree.at(-1);
  assert.deepEqual([plain.plain, plain.herdr, plain.tasks.length], [true, "s2", 0]);
});

// SHPR-32: the person's real Herdr, with `shprd serve` for ~/projects/jmanager.
const JMANAGER = "/Users/cryogenix/projects/jmanager";
const REAL = [
  { id: "w1", name: "jmanager", folder: JMANAGER, active: true },
  { id: "w2", name: "shprd", folder: "/Users/cryogenix/projects/shprd" },
  { id: "w3", name: "shprd · jmanager", folder: JMANAGER },
];
const served = { run: null, tasks: [], spaces: [{ id: "sp-jmanager", name: "jmanager", folder: JMANAGER, branch: "main", herdr_id: "w3", herdr_name: "shprd · jmanager" }] };

test("the served space is Herdr's real one, and only it is left out of the plain rows", () => {
  const tree = spaceTree(served, REAL, NOW);
  assert.deepEqual(tree.map((space) => space.name), ["shprd · jmanager", "jmanager", "shprd"]);
  const [own, mine, other] = tree;
  assert.deepEqual([own.plain, own.herdr, own.branch, own.id], [false, "w3", "main", "sp-jmanager"]);
  assert.deepEqual([mine.plain, mine.herdr, other.plain, other.herdr], [true, "w1", true, "w2"], "the person's own space in the same folder still shows");
});

test("plainSpaces hides exactly the served space's id", () => {
  assert.deepEqual(plainSpaces(served.spaces, REAL).map((space) => space.herdr), ["w1", "w2"]);
  const noId = [{ id: "sp-jmanager", name: "jmanager", folder: JMANAGER, branch: "" }];
  assert.deepEqual(plainSpaces(noId, REAL).map((space) => space.herdr), ["w1", "w2"], "without an id the `shprd · repo` name stands in");
});

test("the Tasks list's scope keeps one space's tasks, and all when none is picked", () => {
  assert.equal(scopeToSpace(state, null), state);
  assert.ok(scopeToSpace(state, "sp-shop").tasks.every((task) => task.space === "sp-shop"));
  assert.equal(scopeToSpace(state, "gone"), state);
  assert.deepEqual(scopeToSpace(state, "herdr:s2").tasks, [], "a space Herdr made has no tasks");
});

test("a finished agent stays in the Agents pane 30 minutes; a coordinator stays", () => {
  const ended = (ms) => ({ id: "x", space: "sp-shprd", task: "SHPR-28", role: "reviewer", program: "codex", state: "done", ended_ms: NOW - ms });
  assert.equal(agentKept(ended(29 * 60_000), NOW), true);
  assert.equal(agentKept(ended(30 * 60_000), NOW), false);
  assert.equal(agentKept({ ...ended(5 * 3600_000), role: "coordinator", task: null }, NOW), true);
  assert.equal(agentKept({ ...ended(0), ended_ms: undefined, since_ms: NOW - 31 * 60_000 }, NOW), false);
  const aged = { ...state, live_agents: [ended(31 * 60_000), ...state.live_agents.filter((agent) => agent.state !== "done")] };
  assert.equal(agentRows(aged, { now: NOW }).filter((row) => row.state === "done").length, 0);
  // The fixture's own done reviewer ended 12 minutes ago: still shown.
  assert.equal(agentRows(state, { now: NOW }).filter((row) => row.state === "done").length, 1);
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
  const coordinator = rows.find((row) => row.role === "Assistant" && row.space === "sp-shprd");
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

test("the Graph is a view of the Tasks panel: the layout holds no graph block, terminal block or graph mod", () => {
  const layout = readFileSync(new URL("../layouts/shprd.json", import.meta.url), "utf8");
  assert.ok(!/graph/i.test(layout), "no graph stack or block in the layout");
  assert.ok(!layout.includes('"terminal"'), "no terminal block for Herdr to prune");
  assert.ok(!existsSync(new URL("../mods/shprd-graph", import.meta.url)), "the graph mod is gone");
  const tasks = readFileSync(new URL("../mods/shprd-tasks/main.js", import.meta.url), "utf8");
  assert.ok(tasks.includes('this.show("graph")'), "G switches the Tasks panel to its Graph");
});
