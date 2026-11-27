// The Tasks pane across a switch of space and a press (SHPR-78): each repository's
// last board is kept and shown at once, "Loading tasks…" only where tasks are
// known to exist, an empty repository says so at once, and a pressed pill is busy
// until the service has answered.
//
//   node --import ./test/boot.mjs --test test/

import test, { afterEach } from "node:test";
import assert from "node:assert/strict";

import * as herdr from "herdr";
import { record, walk } from "gpui-kit";
import { click, inputState } from "./stub-gpui.js";
import Tasks from "../mods/shprd-tasks/main.js";
import { RepoCache } from "../mods/lib/client.js";
import { paneState } from "../mods/lib/board.js";

// The window's space list is shared module state: leave it empty for the next file.
afterEach(() => herdr.setSpaces([]));

/** A pane with no window: its own cache, counts and busy table, as `init` makes them. */
function pane() {
  const view = new Tasks();
  view.cx = { notify() {}, timer: { after() {} } };
  view.cache = new RepoCache();
  view.shownKey = view.cache.key();
  view.state = view.cache.get();
  view.counts = new Map();
  view.busy = new Map();
  view.failure = null;
  view.actionError = null;
  return view;
}

const board = (...ids) => ({ seq: 1, tasks: ids.map((id) => ({ id, status: "queued", derived: { status: "to_do" } })) });

test("the pane's body: cached list, loading only where tasks exist, empty at once, neutral while unknown", () => {
  assert.equal(paneState({ cached: true, count: 0 }), "list", "a cached board is shown whatever the count says");
  assert.equal(paneState({ cached: true, count: null }), "list");
  assert.equal(paneState({ cached: false, count: 3 }), "loading", "tasks exist, board not read yet");
  assert.equal(paneState({ cached: false, count: 0 }), "empty", "known empty: no loading line, the empty state at once");
  assert.equal(paneState({ cached: false, count: null }), "neutral", "count not back yet: neither Loading nor Nothing here");
  assert.equal(paneState({ cached: false }), "neutral");
  assert.equal(paneState(), "neutral");
});

test("a pane switched to another repository shows that repository's last board at once", () => {
  herdr.setSpaces([{ folder: "/work/shop", active: true }]);
  const view = pane();
  view.sync();
  assert.equal(view.state, null, "nothing read for the shop yet");
  assert.equal(view.pane(), "neutral");
  view.counts.set("/work/shop", 2);
  assert.equal(view.pane(), "loading");

  view.cache.set("/work/shop", board("A", "B"));
  view.sync();
  assert.deepEqual(view.state.tasks.map((t) => t.id), ["A", "B"]);
  assert.equal(view.pane(), "list");

  // Switch to the blog: nothing cached, known empty → the empty state, not the shop's list.
  herdr.setSpaces([{ folder: "/work/blog", active: true }]);
  view.counts.set("/work/blog", 0);
  view.sync();
  assert.equal(view.state, null);
  assert.equal(view.pane(), "empty");
  view.cache.set("/work/blog", board("Z"));

  // Back to the shop: its cached board is there with no read at all.
  herdr.setSpaces([{ folder: "/work/shop", active: true }]);
  view.sync();
  assert.deepEqual(view.state.tasks.map((t) => t.id), ["A", "B"]);
  assert.equal(view.pane(), "list");
});

test("a read that finishes after a switch is kept for its own repository, not drawn in the shown one", () => {
  herdr.setSpaces([{ folder: "/work/blog", active: true }]);
  const view = pane();
  view.sync();
  view.cache.set("/work/shop", board("A"));
  view.sync();
  assert.equal(view.state, null, "the shop's late answer does not appear in the blog");
  herdr.setSpaces([{ folder: "/work/shop", active: true }]);
  view.sync();
  assert.equal(view.state.tasks.length, 1);
});

test("a pressed verb shows at once in the pane, a second press is ignored, and it is confirmed after the answer", async () => {
  herdr.SHARED.clear();
  herdr.setSpaces([{ folder: "/work/shop", active: true }]);
  const view = pane();
  const calls = [];
  let answer;
  view.client = {
    folder: "/work/shop",
    action: (body) => {
      calls.push(body);
      return new Promise((resolve) => (answer = resolve));
    },
  };
  view.refresh = async () => {};
  view.cache.set("/work/shop", board("T1"));
  view.sync();
  const task = view.state.tasks[0];
  const first = view.verb(task, "task.start");
  view.sync();
  assert.equal(view.state.tasks[0].derived.status, "working", "the row moved before the service answered");
  assert.equal(view.state.tasks[0].pending.label, "Starting…");
  await view.verb(task, "task.start");
  assert.equal(calls.length, 1, "a double press sends one verb");
  answer({});
  await first;
  assert.equal(view.actionError, null);
});

test("a refused verb rolls the row back and says why in one line", async () => {
  herdr.SHARED.clear();
  herdr.setSpaces([{ folder: "/work/shop", active: true }]);
  const view = pane();
  view.client = { folder: "/work/shop", action: () => Promise.reject(new Error("nope")) };
  view.refresh = async () => {};
  view.cache.set("/work/shop", { seq: 1, tasks: [{ id: "T1", status: "working", derived: { status: "working" } }] });
  view.sync();
  await view.verb(view.state.tasks[0], "task.stop");
  view.sync();
  assert.equal(view.state.tasks[0].derived.status, "working", "back to the true state");
  assert.equal(view.state.tasks[0].pending, undefined);
  assert.match(view.actionError, /Stop T1: nope/);
  herdr.SHARED.clear();
});

test("the Tasks pane draws an old review-out task and its Needs you badge counts the open ask", () => {
  herdr.SHARED.clear();
  herdr.setSpaces([{ folder: "/work/shop", active: true }]);
  const view = pane();
  Object.assign(view, {
    shut: new Set(),
    filter: "all",
    doneOpen: false,
    olderOpen: false,
    allDone: false,
    groupBy: "you",
    chips: { primary: [], more: [] },
    view: "tasks",
    clearing: null,
    adding: false,
    searching: false,
    query: inputState({ placeholder: "Filter tasks" }),
    handle: {},
  });
  const old = Date.now() - 20 * 60 * 60 * 1000;
  const mk = (id, word) => ({
    id,
    title: `task ${id}`,
    status: "queued",
    derived: { status: word },
    added_at_ms: old,
    touched_at_ms: old,
  });
  view.cache.set("/work/shop", {
    seq: 1,
    tasks: [mk("OUT", "needs_you"), mk("OLD", "queued")],
    asks: [{ id: "review-out-2", task: "OUT", kind: "question", text: "OUT: review failed", options: ["retry with another round", "take it as it is", "drop it"] }],
    needs: [{ seq: 1, task: "OUT", body: "OUT: review failed", default_action: "x", kind: "decision", actions: ["review", "drop"] }],
  });
  view.sync();
  record(true);
  let nodes;
  let root;
  try {
    root = view.render();
    nodes = walk(root);
    // The row of a task with an open ask offers Answer (the card's choices) and Drop.
    const opened = [];
    view.open = (task) => opened.push(task.id);
    assert.ok(nodes.some((n) => n.idv === "task-OUT-answer"), "the asking row draws its Answer pill");
    assert.ok(nodes.some((n) => n.idv === "task-OUT-drop"), "the asking row draws its Drop pill");
    click(root, "task-OUT-answer");
    assert.deepEqual(opened, ["OUT"], "Answer opens the task's card, where its three choices are drawn");
  } finally {
    record(false);
  }
  const words = nodes.flatMap((node) => node.kids.filter((kid) => typeof kid === "string"));
  assert.ok(words.includes("Answer"), "the pill is labelled Answer");
  assert.ok(words.includes("1"), "the Needs you badge counts the asking task");
  assert.ok(words.some((w) => w.includes("task OUT")), "the asking task's row is drawn");
  assert.ok(!words.some((w) => w.includes("task OLD")), "the unasked old task stays folded away");
  herdr.SHARED.clear();
});
