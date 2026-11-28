// What a waiting task is labelled by: one helper for the Graph, the Tasks list and the toasts.
import test from "node:test";
import assert from "node:assert/strict";
import { graphRows } from "../mods/lib/tree.js";
import { taskLook } from "../mods/lib/spaces.js";
import { newsItems } from "../mods/lib/toasts.js";
import { waitLabel } from "../mods/lib/tokens.js";

const task = (id, derived) => ({ id, title: id, status: "reported", derived: { status: "needs_you", ...derived } });
const STATE = {
  run: { id: "r1" },
  tasks: [
    task("W1", { need: "attention", for_you: true }),
    task("W2", { need: "attention", for_you: false }),
    task("W3", { need: "asks", for_you: false }),
  ],
};
const word = (id) => graphRows(STATE).find((row) => row.id === id)?.word;

test("a task only the person can settle reads 'needs you'", () => {
  assert.equal(waitLabel(STATE.tasks[0]), "needs you");
  assert.equal(word("W1"), "needs you");
  assert.equal(taskLook(STATE.tasks[0]).word, "Needs you");
});

test("a coordinator's card with choices reads 'needs decision', in the Graph and the Tasks list", () => {
  assert.equal(waitLabel(STATE.tasks[1]), "needs decision");
  assert.equal(word("W2"), "needs decision");
  assert.equal(taskLook(STATE.tasks[1]).word, "needs decision");
});

test("a worker's question reads 'needs answer', and a toast says only what the helper says", () => {
  assert.equal(waitLabel(STATE.tasks[2]), "needs answer");
  assert.equal(word("W3"), "needs answer");
  assert.equal(taskLook(STATE.tasks[2]).word, "needs answer");
  const items = newsItems([{ seq: 1, kind: "needs_you", task: "W1" }], STATE);
  for (const item of items) assert.match(item.title, /Needs you$/);
});
