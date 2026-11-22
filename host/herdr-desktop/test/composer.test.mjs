// The chat composer (SHPR-84): Enter sends, Shift+Enter is a new line, the send button
// sends, files are attached by path, the model button switches the coordinator, Esc stops
// a working agent, and the mic is drawn only where its recorder is.
//
//   node --import ./test/boot.mjs --test test/
//
// These drive the mod's own handlers through the elements it draws, as the window
// delivers a key or a click (`keydown`/`click` in `stub-gpui.js`); nothing needs a window.

import test from "node:test";
import assert from "node:assert/strict";

import * as herdr from "herdr";
import { click, keydown, record, walk, inputState } from "gpui-kit";
import Chat from "../mods/shprd-chat/main.js";
import * as files from "../mods/shprd-chat/attach.js";
import { Dictation, available } from "../mods/shprd-chat/dictate.js";
import { COORDINATOR, empty, fold, keyAction, outgoing } from "../mods/lib/chat.js";
import { menusOf, pickLabel, PICK_KEY } from "../mods/lib/settings.js";

const ROSTER = {
  programs: [
    {
      id: "claude",
      name: "Claude Code",
      installed: true,
      efforts: ["low", "medium", "high"],
      models: [
        { id: "opus", name: "Opus 5.5", efforts: ["low", "medium", "high"], default_effort: "medium" },
        { id: "sonnet", name: "Sonnet 5.5", efforts: ["low", "medium", "high"], default_effort: "medium" },
      ],
      permissions: [],
    },
  ],
};

/** The settings document `GET /api/settings` answers for a coordinator on `model`. */
function doc(model = "opus", effort = null) {
  const row = (key, value) => ({ key, value, source: "you" });
  return { values: [row("roles.coordinator.agent", "claude"), row("roles.coordinator.model", model), ...(effort ? [row("roles.coordinator.effort", effort)] : [])] };
}

/** A panel that never ran `init`, with a client that records every call. */
function panel({ refuse = false, state = { tasks: [], needs: [], live_agents: [] } } = {}) {
  const calls = [];
  const timers = [];
  const cx = {
    notify() {},
    sleep: async () => {},
    spawn(work) {
      work({ notify() {}, sleep: async () => {} });
      return { cancel() {} };
    },
    timer: { after: (ms, fn) => timers.push({ ms, fn }), every: () => ({ cancel() {} }) },
  };
  const settings = { doc: doc() };
  const client = {
    dir: "/state",
    folder: "/f",
    id: () => "abc",
    state: async () => state,
    asks: async () => ({ asks: [] }),
    settings: async () => settings.doc,
    roster: async () => ROSTER,
    putSetting: async (key, value, repo) => {
      calls.push({ method: "PUT", key, value, repo });
      if (refuse && key.endsWith(".effort")) throw new Error("refused");
      const rows = settings.doc.values.filter((entry) => entry.key !== key);
      if (value !== null) rows.push({ key, value, source: "repo_entry" });
      settings.doc = { values: rows };
      return settings.doc;
    },
    restartCoordinator: async () => {
      calls.push({ method: "RESTART" });
    },
    call: async (method, path, options = {}) => {
      calls.push({ method, path, body: options.body ?? null });
      if (refuse && method === "POST") {
        const error = new Error("the pane is closed");
        error.name = "ServiceError";
        throw error;
      }
      return { space: COORDINATOR, sent: true };
    },
  };
  const view = new Chat();
  Object.assign(view, {
    cx,
    space: COORDINATOR,
    client,
    chat: empty(),
    state,
    failure: null,
    refusal: null,
    sending: false,
    watching: true,
    opened: new Set(),
    input: inputState({ placeholder: "Message the coordinator" }),
    files: [],
    enterAt: 0,
    doc: settings.doc,
    roster: ROSTER,
    menus: menusOf(ROSTER),
    picked: null,
    pickRec: { at: null },
    dict: new Dictation("t"),
  });
  view.refresh = async () => {};
  return { view, calls, timers, settings };
}

/** Draws the composer and answers its elements. */
function draw(view) {
  record(true);
  try {
    const tree = view.composer(herdr.theme(), { red: "#f00", on: "#fff" });
    return { tree, ids: walk(tree).map((node) => node.idv).filter(Boolean) };
  } finally {
    record(false);
  }
}

const sent = (calls) => calls.filter((call) => call.method === "POST" && call.path === "/api/chat");

test("Enter sends, Shift+Enter is left to the text area, Alt+Enter is a new line", () => {
  assert.equal(keyAction({ key: "enter", modifiers: {} }), "send");
  assert.equal(keyAction({ key: "enter", modifiers: { control: true } }), "send");
  assert.equal(keyAction({ key: "enter", modifiers: { platform: true } }), "send");
  assert.equal(keyAction({ key: "enter", modifiers: { shift: true } }), null);
  assert.equal(keyAction({ key: "enter", modifiers: { alt: true } }), "newline");
  assert.equal(keyAction({ key: "a", modifiers: {} }), null);
});

test("a real Enter in the composer sends the words and the text area never sees it", async () => {
  const { view, calls } = panel();
  view.input.set_value("hello there");
  record(true);
  let result;
  try {
    const tree = view.composer(herdr.theme(), { red: "#f00" });
    result = keydown(tree, "chat-words", { key: "enter" });
  } finally {
    record(false);
  }
  assert.equal(result.stopped, true, "Enter is taken before the text area makes it a new line");
  await new Promise((r) => setTimeout(r, 10));
  assert.deepEqual(sent(calls).map((c) => c.body), [{ space: COORDINATOR, text: "hello there" }]);
  assert.equal(view.input.value(), "", "cleared once the message was accepted");
});

test("Shift+Enter is not taken: the text area makes the new line, nothing is sent", async () => {
  const { view, calls } = panel();
  view.input.set_value("one");
  record(true);
  let result;
  try {
    result = keydown(view.composer(herdr.theme(), { red: "#f00" }), "chat-words", { key: "enter", modifiers: { shift: true } });
  } finally {
    record(false);
  }
  assert.equal(result.stopped, false);
  assert.equal(sent(calls).length, 0);
  assert.equal(view.input.value(), "one");
});

test("Alt+Enter puts a line break in the box and sends nothing", () => {
  const { view, calls } = panel();
  view.input.set_value("one");
  record(true);
  try {
    keydown(view.composer(herdr.theme(), { red: "#f00" }), "chat-words", { key: "enter", modifiers: { alt: true } });
  } finally {
    record(false);
  }
  assert.equal(view.input.value(), "one\n");
  assert.equal(sent(calls).length, 0);
});

test("Enter on an empty box sends nothing and makes no new line", async () => {
  const { view, calls } = panel();
  view.input.set_value("   ");
  record(true);
  let result;
  try {
    result = keydown(view.composer(herdr.theme(), { red: "#f00" }), "chat-words", { key: "enter" });
  } finally {
    record(false);
  }
  await new Promise((r) => setTimeout(r, 10));
  assert.equal(sent(calls).length, 0);
  assert.equal(result.stopped, true, "swallowed: no stray line break either");
});

test("the Enter key and the chord's submit within a blink send once", async () => {
  const { view, calls } = panel();
  view.input.set_value("once");
  view.enter();
  view.input.handlers.get("submit")?.({ secondary: true }, {});
  await new Promise((r) => setTimeout(r, 10));
  assert.equal(sent(calls).length, 1);
});

test("the send button sends the current text; empty it sends nothing; a refusal keeps the words", async () => {
  const { view, calls } = panel();
  view.input.set_value("by button");
  record(true);
  try {
    const tree = view.composer(herdr.theme(), { red: "#f00" });
    const r = click(tree, "chat-send");
    assert.equal(r.stopped, true, "the press is the button's own");
  } finally {
    record(false);
  }
  await new Promise((r) => setTimeout(r, 10));
  assert.equal(sent(calls).length, 1);
  assert.equal(sent(calls)[0].body.text, "by button");
  view.input.set_value("");
  record(true);
  try {
    click(view.composer(herdr.theme(), { red: "#f00" }), "chat-send");
  } finally {
    record(false);
  }
  await new Promise((r) => setTimeout(r, 10));
  assert.equal(sent(calls).length, 1, "an empty message does not send");
  const refused = panel({ refuse: true });
  refused.view.input.set_value("keep me");
  await refused.view.send();
  assert.equal(refused.view.input.value(), "keep me");
  assert.equal(refused.view.refusal, "the pane is closed");
});

test("the composer draws the + the model button and the send button inside one box", () => {
  const { view } = panel();
  const { ids } = draw(view);
  for (const id of ["chat-composer", "chat-attach", "chat-model-pick", "chat-send"]) assert.ok(ids.includes(id), `${id} is drawn`);
  assert.equal(ids.includes("chat-mic"), false, "no mic where the recorder is not installed");
  view.micOn = true;
  assert.ok(draw(view).ids.includes("chat-mic"));
  // A task's chat has no model button: the model is the task's.
  view.space = "T1";
  assert.equal(draw(view).ids.includes("chat-model-pick"), false);
});

test("the model button reads like Shepherdr's: Opus 5.5 · Medium", () => {
  const menus = menusOf(ROSTER);
  assert.equal(pickLabel(menus, { agent: "claude", model: "opus", effort: null }), "Opus 5.5 · Medium");
  assert.equal(pickLabel(menus, { agent: "claude", model: "sonnet", effort: "high" }), "Sonnet 5.5 · High");
  assert.equal(panel().view.modelWords(), "Opus 5.5 · Medium");
});

test("choosing a model writes the Settings keys once, restarts the coordinator and keeps the history", async () => {
  const { view, calls, timers } = panel({
    state: { tasks: [], needs: [], live_agents: [{ role: "coordinator", state: "idle", ended_ms: null }] },
  });
  view.chat = { ...empty(), session: "s1", messages: [{ role: "user", text: "earlier", tools: [], at_ms: 1 }] };
  // Two answers of the menu (a model, then a level), then quiet.
  herdr.setShared(PICK_KEY, { key: "chat:coordinator", field: "selection", value: { agent: "claude", model: "sonnet", effort: null, fast: false, perm: null }, at: 1 });
  view.takeReply();
  herdr.setShared(PICK_KEY, { key: "chat:coordinator", field: "selection", value: { agent: "claude", model: "sonnet", effort: "high", fast: false, perm: null }, at: 2 });
  view.takeReply();
  assert.equal(timers.length, 2);
  timers[0].fn();
  await new Promise((r) => setTimeout(r, 5));
  assert.equal(calls.length, 0, "an earlier answer's timer does nothing: the menu was not quiet");
  timers[1].fn();
  await new Promise((r) => setTimeout(r, 20));
  assert.deepEqual(
    calls.map((c) => (c.method === "PUT" ? `PUT ${c.key}=${c.value}` : c.method)),
    ["PUT roles.coordinator.model=sonnet", "PUT roles.coordinator.effort=high", "RESTART"],
  );
  assert.equal(calls[0].repo, true, "for this repository, the one this service serves");
  assert.equal(view.modelWords(), "Sonnet 5.5 · High", "the button follows what is stored, as Settings does");
  // The restart's new session continues the conversation on screen.
  const next = fold(view.chat, { space: COORDINATOR, session: "s2", messages: [{ role: "assistant", text: "hi" }], cursor: "s2@1" }, view.keep.text);
  assert.deepEqual(next.messages.map((m) => m.text), ["earlier", "Switched the coordinator to Sonnet 5.5 · High", "hi"]);
});

test("another window's answer, and a refused write, change nothing for good", async () => {
  const other = panel();
  herdr.setShared(PICK_KEY, { key: "role:coordinator", field: "selection", value: { agent: "claude", model: "sonnet" }, at: 10 });
  other.view.takeReply();
  assert.equal(other.timers.length, 0, "Settings' own answers are not the chat's");
  const { view, calls, timers } = panel({ refuse: true, state: { live_agents: [{ role: "coordinator", state: "idle" }] } });
  herdr.setShared(PICK_KEY, { key: "chat:coordinator", field: "selection", value: { agent: "claude", model: "sonnet", effort: "high", fast: false, perm: null }, at: 11 });
  view.takeReply();
  timers[0].fn();
  await new Promise((r) => setTimeout(r, 20));
  assert.equal(view.refusal, "refused");
  assert.ok(!calls.some((c) => c.method === "RESTART"), "a refused write restarts nothing");
  assert.equal(view.modelWords(), "Opus 5.5 · Medium", "the earlier write was put back");
});

test("attached files go with the message by path, are kept when it is refused, and clear when it is sent", async () => {
  assert.equal(outgoing(COORDINATOR, "", []), null);
  assert.deepEqual(outgoing(COORDINATOR, "", [{ path: "/a/b.png", name: "b.png" }]), {
    space: COORDINATOR,
    text: "",
    attachments: [{ path: "/a/b.png", name: "b.png" }],
  });
  const refused = panel({ refuse: true });
  refused.view.addFiles(files.entries(["/a/notes.txt"]));
  refused.view.input.set_value("what is this?");
  await refused.view.send();
  assert.equal(refused.view.files.length, 1, "a refused send keeps the files");
  const { view, calls } = panel();
  view.addFiles(files.entries(["/a/notes.txt", "/a/pic.png", "/a/notes.txt"]));
  assert.deepEqual(view.files.map((f) => [f.name, f.image]), [["notes.txt", false], ["pic.png", true]], "a path once");
  await view.send();
  assert.deepEqual(sent(calls)[0].body.attachments.map((a) => a.path), ["/a/notes.txt", "/a/pic.png"]);
  assert.equal(view.files.length, 0);
  record(true);
  try {
    view.addFiles(files.entries(["/a/x.txt"]));
    click(view.composer(herdr.theme(), { red: "#f00" }), "chat-file-x-0");
  } finally {
    record(false);
  }
  assert.equal(view.files.length, 0, "the × takes a file off");
});

test("the file picker answers the chosen paths, and none when cancelled", async () => {
  herdr.setExec(async (command) => {
    if (command === "uname") return { code: 0, stdout: "Darwin\n", stderr: "" };
    return { code: 0, stdout: "/Users/me/a b.txt\n/Users/me/c.png\n", stderr: "" };
  });
  assert.deepEqual((await files.pick()).map((f) => f.path), ["/Users/me/a b.txt", "/Users/me/c.png"]);
  herdr.setExec(async (command) => (command === "uname" ? { code: 0, stdout: "Darwin\n", stderr: "" } : { code: 0, stdout: "", stderr: "" }));
  assert.deepEqual(await files.pick(), []);
});

test("Esc stops a working agent while the box is empty; Stop asks the service", async () => {
  const { view, calls } = panel({ state: { live_agents: [{ role: "coordinator", state: "working", ended_ms: null }] } });
  assert.equal(view.working(), true);
  record(true);
  try {
    const tree = view.composer(herdr.theme(), { red: "#f00" });
    keydown(tree, "chat-words", { key: "escape" });
  } finally {
    record(false);
  }
  await new Promise((r) => setTimeout(r, 10));
  assert.deepEqual(calls.filter((c) => c.path === "/api/chat/stop").map((c) => c.body), [{ space: COORDINATOR }]);
  view.input.set_value("typing");
  record(true);
  try {
    keydown(view.composer(herdr.theme(), { red: "#f00" }), "chat-words", { key: "escape" });
  } finally {
    record(false);
  }
  await new Promise((r) => setTimeout(r, 10));
  assert.equal(calls.filter((c) => c.path === "/api/chat/stop").length, 1, "Esc with words in the box is not Stop");
  const idle = panel();
  assert.equal(idle.view.working(), false);
});

test("the mic is offered only where Shepherdr's recorder and bun are", async () => {
  herdr.setExec(async () => ({ code: 0, stdout: "yes\n", stderr: "" }));
  assert.equal(await available(), true);
  herdr.setExec(async () => ({ code: 0, stdout: "", stderr: "" }));
  assert.equal(await available(), false);
});

test("a dictation records, then its words come back to put in the box", async () => {
  const ran = [];
  herdr.putFile(`${herdr.appHome()}/data/shprd-chat-dictation/t-1.wav.txt`, '{"text":"hello world"}');
  herdr.setExec(async (command, args) => {
    ran.push(args.join(" ").slice(0, 60));
    return { code: 0, stdout: "", stderr: "" };
  });
  const dict = new Dictation("t");
  const realNow = Date.now;
  Date.now = () => 1;
  dict.start();
  Date.now = realNow;
  assert.equal(dict.state, "recording");
  assert.equal(await dict.stop(), "hello world");
  assert.equal(dict.state, "idle");
  assert.ok(ran.some((line) => line.includes("record.ts") || line.includes("kill -INT")));
  // A mic that cannot work cancels and says why.
  herdr.putFile(`${herdr.appHome()}/data/shprd-chat-dictation/t-2.level`, "!Microphone access is off.");
  Date.now = () => 2;
  dict.start();
  Date.now = realNow;
  assert.equal(await dict.level(), 0);
  assert.equal(dict.error, "Microphone access is off.");
  assert.equal(dict.state, "idle");
});

test("a restart the person asked for keeps the old messages and says why", () => {
  const held = { ...empty(), session: "s1", messages: [{ role: "user", text: "old", tools: [], at_ms: 1 }] };
  const answer = { space: COORDINATOR, session: "s2", messages: [], cursor: "s2@0" };
  assert.deepEqual(fold(held, answer).messages, [], "a plain new session starts over");
  assert.deepEqual(fold(held, answer, "Switched").messages.map((m) => m.text), ["old", "Switched"]);
});

test("the restart's new chat pane takes the conversation over, once, for the same folder", () => {
  const old = panel({ state: { live_agents: [{ role: "coordinator", state: "idle" }] } });
  old.view.chat = { ...empty(), session: "s1", messages: [{ role: "user", text: "earlier", tools: [], at_ms: 1 }] };
  herdr.setShared("shprd.chat.carry", { folder: "/f", at: Date.now(), text: "Switched the coordinator to Haiku 5.5 · Medium", messages: old.view.chat.messages });
  const next = panel();
  next.view.carry();
  assert.deepEqual(next.view.chat.messages.map((m) => m.text), ["earlier", "Switched the coordinator to Haiku 5.5 · Medium"]);
  assert.equal(herdr.shared("shprd.chat.carry"), null, "taken once");
  const other = panel();
  herdr.setShared("shprd.chat.carry", { folder: "/elsewhere", at: Date.now(), text: "x", messages: [] });
  other.view.carry();
  assert.equal(other.view.chat.messages.length, 0, "another folder's conversation is not this one's");
});
