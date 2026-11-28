// Each task status maps to one shared colour token, the same for the Tasks list,
// the Graph, and the Agents and Spaces panes.

import test from "node:test";
import assert from "node:assert/strict";

import { look } from "../mods/lib/board.js";
import { agentLook } from "../mods/lib/tree.js";
import { statusColor, statusColors, tone } from "../mods/lib/tokens.js";

const task = (status, extra = {}) => ({ derived: { status, ...extra } });

test("each status maps to its token", () => {
  assert.equal(look(task("done")).color, "done");
  assert.equal(look(task("ready")).color, "done");
  assert.equal(look(task("working")).color, "working");
  assert.equal(look(task("needs_you", { need: "stopped" })).color, "failed");
  assert.equal(statusColor({ status: "failed", derived: { status: "needs_you" } }), "failed");
  assert.equal(look(task("needs_you", { need: "asks", for_you: false })).color, "waiting");
  assert.equal(look(task("needs_you", { need: "attention" })).color, "waiting");
  for (const grey of ["queued", "to_do", "scheduled"]) assert.equal(look(task(grey)).color, undefined);
});

test("the agents pane draws with the same tokens", () => {
  assert.equal(agentLook({ state: "done" }).color, "done");
  assert.equal(agentLook({ state: "working" }).color, "working");
  assert.equal(agentLook({ state: "needs_you" }).color, "waiting");
});

test("the tokens resolve in both themes and are in tone()", () => {
  for (const dark of [true, false]) {
    const set = tone({ dark });
    for (const key of Object.keys(statusColors({ dark }))) assert.match(set[key], /^#[0-9a-f]{6}$/);
    assert.equal(set.done, set.green);
    assert.equal(set.failed, set.red);
    assert.equal(set.waiting, set.amber);
  }
});
