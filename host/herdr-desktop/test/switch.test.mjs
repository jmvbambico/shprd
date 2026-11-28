// The Chat · Terminal switch in its two places (SHPR-64): an agent's pane header
// (`shprd-pane`) and the task thread's head (`shprd-accept`).
//
//   node --import ./test/boot.mjs --test test/
//
// A window `cx` is valid only for the host call that produced it, so the pane's
// switch must use the cx its *click* was given, never the one `render` got: the
// render cx used from a click throws a stale-context error, and the click did
// nothing (SHPR-64).

import test from "node:test";
import assert from "node:assert/strict";

import * as herdr from "herdr";
import { click, record, walk } from "gpui-kit";
import Pane from "../mods/shprd-pane/main.js";
import Accept from "../mods/shprd-accept/main.js";

/** The cx a `render` is handed: dead as soon as render returns, as the window's is. */
function renderCx() {
  const dead = { live: true };
  const guard = (name) => () => {
    if (!dead.live) throw new Error(`stale context: cx.${name} after render`);
    return { cancel() {} };
  };
  return { dead, cx: { notify: guard("notify"), spawn: guard("spawn"), stop_propagation: guard("stop") } };
}

/** The cx a click is handed: live for the handler; its `spawn` runs the body at once. */
function clickCx(bodies) {
  return {
    notify() {},
    stop_propagation() {},
    spawn(body) {
      bodies.push(body({ notify() {} }));
      return { cancel() {} };
    },
  };
}

/** Records every `herdr-app do <command>` and `hc` call, answering `{pane}` for a split and a keep. */
function listen() {
  const calls = [];
  herdr.setExec(async (command, args) => {
    if (command === "herdr-app" && args[0] === "do") {
      calls.push(args[1]);
      const out = args[1] === "pane.split" ? { pane: "chat1" } : args[1] === "terminal.keep" ? { pane: "term1" } : {};
      return { code: 0, stdout: JSON.stringify(out), stderr: "" };
    }
    return { code: 0, stdout: "{}", stderr: "" };
  });
  return calls;
}

const SPACES = [{ id: "sp", name: "shprd · shop", tabs: [{ id: "tab1" }] }];

/** Draws one pane header and presses `id` as the window would; answers what ran. */
async function press(id) {
  record(true);
  try {
    const view = new Pane();
    const { dead, cx } = renderCx();
    const tree = view.render(cx);
    dead.live = false;
    const bodies = [];
    const node = walk(tree).find((entry) => entry.idv === id);
    assert.ok(node, `the header draws ${id}`);
    node.click({}, clickCx(bodies));
    await Promise.all(bodies);
  } finally {
    record(false);
  }
}

test("an agent pane's Chat segment swaps its terminal for the chat", async () => {
  herdr.reset();
  const calls = listen();
  herdr.setProps({ pane: "p1", terminal: "t1" });
  herdr.setSpaces(SPACES);
  herdr.setPanes([{ pane: "p1", terminal: "t1", tab: "tab1", status: "idle", program: { agent: true, name: "Claude" } }]);
  herdr.setTerminals([{ address: "t1", state: "live", labels: {} }]);
  await press("pane-mode-p1-chat");
  assert.deepEqual(calls, ["pane.split", "terminal.park"], "chat opened beside it, then the terminal parked");
  assert.deepEqual(herdr.HC_CALLS[0], ["label", "t1", "shprd.chat=chat1"]);
});

test("a chat pane's Terminal segment brings the terminal back, even right after the swap", async () => {
  herdr.reset();
  const calls = listen();
  herdr.setProps({ pane: "chat1" });
  herdr.setSpaces(SPACES);
  herdr.setPanes([{ pane: "chat1", mod: "shprd-chat", tab: "tab1" }]);
  herdr.setTerminals([{ address: "t1", state: "live", labels: { "shprd.chat": "chat1" } }]);
  await press("pane-mode-chat1-terminal");
  assert.deepEqual(calls, ["pane.focus", "terminal.keep", "pane.close"]);
  // A second swap straight away (Chat, then Terminal within 3 s) is not dropped.
  herdr.setProps({ pane: "p1", terminal: "t1" });
  herdr.setPanes([{ pane: "p1", terminal: "t1", tab: "tab1", status: "idle", program: { agent: true, name: "Claude" } }]);
  calls.length = 0;
  await press("pane-mode-p1-chat");
  assert.deepEqual(calls, ["pane.split", "terminal.park"]);
});

/** A head with one task whose agent holds terminal `t5`, as the thread reads it. */
function head() {
  const view = new Accept();
  view.headCopy = true;
  view.cx = { notify() {} };
  view.state = {
    tasks: [{ id: "T1", title: "One", derived: { status: "working" }, attempts: [] }],
    live_agents: [{ id: "a1", task: "T1", role: "worker", state: "working", since_ms: Date.now(), terminal: "t5" }],
  };
  herdr.setShared("tasks.thread", { task: "T1", title: "One" });
  return view;
}

test("the thread's Terminal view re-focuses the task's agent when another agent was focused since", () => {
  herdr.reset();
  herdr.setTerminals([{ address: "t5", state: "live" }]);
  herdr.setLayout({ id: "tasks-thread-body", type: "stack", active: "tasks-thread-term", children: [] });
  const view = head();
  herdr.setFocused({ agent: "t5" });
  view.settle();
  assert.deepEqual(herdr.RUNS.filter((r) => r.name === "agent.focus"), [], "already showing it: nothing to do");
  view.settle();
  // The person focused another agent's pane; the block now shows that one.
  herdr.setFocused({ agent: "t9" });
  view.settle();
  assert.deepEqual(
    herdr.RUNS.filter((r) => r.name === "agent.focus").map((r) => r.args),
    [{ terminal: "t5" }],
    "the head asks Herdr which agent is shown, not its own memory of the last ask",
  );
});

test("the thread's Chat and Terminal segments select their stack children, both ways, repeatedly", () => {
  herdr.reset();
  herdr.setTerminals([{ address: "t5", state: "live" }]);
  herdr.setLayout({ id: "tasks-thread-body", type: "stack", active: "tasks-thread-chat", children: [] });
  record(true);
  try {
    const view = head();
    const press = (id) => {
      const node = walk(view.render({})).find((entry) => entry.idv === id);
      assert.ok(node, `the head draws ${id}`);
      node.click({}, { notify() {}, stop_propagation() {} });
    };
    for (let round = 0; round < 3; round += 1) {
      press("accept-view-term");
      herdr.setLayout({ id: "tasks-thread-body", type: "stack", active: "tasks-thread-term", children: [] });
      press("accept-view-chat");
      herdr.setLayout({ id: "tasks-thread-body", type: "stack", active: "tasks-thread-chat", children: [] });
    }
    const picks = herdr.RUNS.filter((r) => r.name === "layout.select").map((r) => r.args.child);
    assert.deepEqual(picks, Array(3).fill(["tasks-thread-term", "tasks-thread-chat"]).flat());
  } finally {
    record(false);
  }
});

test("the coordinator's terminal request is ignored by a task's chat-only pane, taken by its own", async () => {
  herdr.reset();
  const calls = listen();
  herdr.setSpaces(SPACES);
  herdr.setShared("shprd.pane.terminal", { space: "coordinator", at: Date.now() + 1 });
  // A task chat in the coordinator's tab: props name T1, nothing parked behind it.
  herdr.setProps({ pane: "chat9" });
  herdr.setPanes([{ pane: "chat9", mod: "shprd-chat", tab: "tab1", props: { space: "T1" } }]);
  herdr.setTerminals([]);
  const view = new Pane();
  view.terminalSeen = null;
  view.takeTerminalRequest({ spawn() {}, notify() {} });
  assert.deepEqual(calls, [], "a task's pane does not act on the coordinator's request");
});

test("closing a chat-only pane closes the pane and never kills an agent", async () => {
  herdr.reset();
  const calls = listen();
  herdr.setProps({ pane: "chat9" });
  herdr.setSpaces(SPACES);
  herdr.setPanes([{ pane: "chat9", mod: "shprd-chat", tab: "tab1", props: { space: "T1" } }]);
  herdr.setTerminals([{ address: "t1", state: "live", labels: {} }]);
  const view = new Pane();
  const { cx } = renderCx();
  view.hovered = true;
  record(true);
  try {
    const tree = view.render(cx);
    const node = walk(tree).find((entry) => entry.idv === "pane-close-chat9");
    assert.ok(node, "the header draws a close");
    node.click({}, clickCx([]));
  } finally {
    record(false);
  }
  assert.equal(view.ending, undefined, "no 'End agent?' question is asked");
  assert.deepEqual(herdr.RUNS.map((r) => r.name), ["pane.close"]);
  assert.deepEqual(calls, []);
  // closeAgent reached some other way still only closes the pane
  herdr.RUNS.length = 0;
  view.closeAgent("chat9", { chatMode: true, chatOnly: true, agent: null }, null);
  assert.deepEqual(herdr.RUNS.map((r) => r.name), ["pane.close"]);
});
