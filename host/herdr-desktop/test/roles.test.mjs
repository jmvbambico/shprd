// A role's name on screen: the person's own word for it, else the built-in one.
//
//   node --import ./test/boot.mjs --test test/roles.test.mjs

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import { ROLE_DEFAULTS, roleRename, roleTitle } from "../mods/lib/tokens.js";
import { agentRows, taskLine } from "../mods/lib/tree.js";
import { coordinatorRow } from "../mods/lib/spaces.js";
import { jumpItems } from "../mods/lib/jump.js";
import { newsItems } from "../mods/lib/toasts.js";
import { header } from "../mods/lib/chat.js";
import { TITLE_MAX, cleanTitle, needsCoordinatorRestart, roles } from "../mods/lib/settings.js";

const NOW = 1_800_000_000_000;
const state = JSON.parse(readFileSync(new URL("./fixtures/proto.json", import.meta.url), "utf8"));
for (const agent of state.live_agents) agent.since_ms = NOW - agent.since_s * 1000;
const named = { ...state, role_titles: { coordinator: "Orchestrator", worker: "Dev", reviewer: "QA" } };

test("a role is called by its name, else its built-in word, else its key", () => {
  assert.equal(roleTitle(named, "coordinator"), "Orchestrator");
  assert.equal(roleTitle(named, "worker"), "Dev");
  assert.equal(roleTitle(named, "reviewer"), "QA");
  assert.equal(roleTitle({}, "coordinator"), ROLE_DEFAULTS.coordinator);
  assert.equal(roleTitle(null, "worker"), "Worker");
  assert.equal(roleTitle({ role_titles: { worker: "   " } }, "worker"), "Worker", "a blank name falls back");
  assert.equal(roleTitle({}, "docs"), "docs");
  assert.equal(roleRename(named, "worker"), "Dev");
  assert.equal(roleRename(state, "worker"), null);
  assert.equal(roleRename({ role_titles: { worker: "Worker" } }, "worker"), null);
});

test("the Agents pane and the tree name each agent's role as the person did", () => {
  const rows = agentRows(named, { now: NOW });
  assert.ok(rows.some((row) => row.role === "Dev"), "a worker is a Dev");
  assert.ok(rows.some((row) => row.role === "Orchestrator"), "the coordinator is the Orchestrator");
  assert.ok(!rows.some((row) => row.role === "Worker" || row.role === "Assistant"));
  const plain = agentRows(state, { now: NOW });
  assert.ok(plain.some((row) => row.role === "Worker"));
});

test("a task's line says the reviewer's own name only when it has one", () => {
  const reviewing = {
    ...named,
    tasks: [{ id: "T1", title: "x", derived: { status: "working" }, attempts: [] }],
    live_agents: [{ id: "a", task: "T1", role: "reviewer", program: "codex", state: "working", since_ms: NOW - 60_000 }],
  };
  assert.match(taskLine(reviewing, reviewing.tasks[0], NOW), /codex · QA reviewing/);
  const plain = { ...reviewing, role_titles: {} };
  assert.match(taskLine(plain, plain.tasks[0], NOW), /codex · reviewing/);
});

test("the coordinator's row and Jump carry its name", () => {
  assert.equal(coordinatorRow(named).title, "Orchestrator");
  assert.equal(coordinatorRow(state).title, "Assistant");
  const items = jumpItems(named, [{ id: "sp", name: "shprd · shprd", folder: "/x/shprd", active: true, tabs: [] }], "orch");
  assert.ok(items.some((item) => /^Orchestrator/.test(item.sub ?? "")), JSON.stringify(items));
});

test("the chat header falls back to the coordinator's name", () => {
  assert.equal(header(null, "Orchestrator"), "Orchestrator");
  assert.equal(header({ program: "claude", session: "abc" }, "Orchestrator"), "claude · abc");
});

test("a failed review says the reviewer's own name", () => {
  const change = [{ seq: 9, kind: "gate_failed", task: "T1" }];
  const base = { tasks: [{ id: "T1", title: "x", derived: { status: "working" }, attempts: [] }] };
  const plain = newsItems(change, base);
  const qa = newsItems(change, { ...base, role_titles: { reviewer: "QA" } });
  assert.match(plain[0].title, /Review failed/);
  assert.match(qa[0].title, /QA failed it/);
});

test("a typed name is trimmed and cut like the service does", () => {
  assert.equal(cleanTitle("  Dev   team "), "Dev team");
  assert.equal(cleanTitle("   "), "");
  assert.equal(cleanTitle("x".repeat(80)).length, TITLE_MAX);
});

test("the settings page lists each role's name, with the built-in word as its fallback", () => {
  const document = {
    values: [
      { key: "roles.order", value: ["coordinator", "worker"], source: "built_in" },
      { key: "roles.coordinator.title", value: "Orchestrator", source: "you" },
      { key: "roles.worker.title", value: "Worker", source: "built_in" },
    ],
  };
  const list = roles(document, { programs: [] });
  const [coordinator, worker] = list;
  assert.equal(coordinator.title, "Orchestrator");
  assert.deepEqual([coordinator.name.key, coordinator.name.value, coordinator.name.fallback], ["roles.coordinator.title", "Orchestrator", "Assistant"]);
  assert.equal(worker.title, "Worker(s)", "the built-in word is headed as a list");
});

test("only the coordinator's own starting keys ask for a restart", () => {
  for (const key of ["agent", "model", "effort", "fast", "perm"]) assert.ok(needsCoordinatorRestart(`roles.coordinator.${key}`));
  assert.ok(!needsCoordinatorRestart("roles.worker.agent"));
  assert.ok(!needsCoordinatorRestart("roles.coordinator.title"));
});
