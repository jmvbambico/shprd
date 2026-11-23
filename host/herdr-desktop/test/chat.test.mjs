// Unit tests for the coordinator's chat: the view model that folds a page into
// rows, and the panel that sends a turn.
//
//   node --import ./test/boot.mjs --test test/       (node 22+)
//   bun test --preload ./test/boot-bun.mjs test/     (bun)
//
// The pure half is `mods/lib/chat.js`; the panel half drives the mod's own
// `send` and `refresh` with a client that records its calls, so nothing here
// needs a window, a service or a transcript.

import { readFileSync } from "node:fs";
import test from "node:test";
import assert from "node:assert/strict";

import * as herdr from "herdr";
import { record, walk } from "gpui-kit";
import Chat from "../mods/shprd-chat/main.js";
import { serviceFile } from "../mods/lib/client.js";
import {
  BUBBLE_MAX,
  CHAT_MOD,
  COORDINATOR,
  FOLD,
  FOLD_TALL,
  PARK_LABEL,
  WORDS,
  agentOf,
  coordinatorDown,
  firstCard,
  folding,
  literal,
  markdownOf,
  messageKey,
  paneChat,
  safeLink,
  spaceOf,
  spaceOfTab,
  timeline,
  toolsSummary,
  agentLabel,
  typed,
  KEEP,
  bubble,
  empty,
  fold,
  foldOlder,
  header,
  latest,
  mine,
  nextCursor,
  outgoing,
  status,
  toolLine,
  when,
} from "../mods/lib/chat.js";
import { inputState } from "./stub-gpui.js";
import { forSpace, cards as cardsOf } from "../mods/lib/asks.js";

/** A context that answers `cx.sleep`, `cx.notify`, `cx.spawn` and `cx.timer`. */
function context() {
  const every = [];
  const spawns = [];
  return {
    cx: {
      sleep: async () => {},
      notify: () => {},
      timer: {
        every: (ms, fn) => {
          every.push({ ms, fn });
          return { cancel: () => {} };
        },
      },
      spawn: (work) => {
        spawns.push(work);
        return { cancel: () => {} };
      },
    },
    every,
    spawns,
  };
}

/** One page as `GET /api/chat` answers it. */
function page(overrides = {}) {
  return {
    space: COORDINATOR,
    program: "pi",
    session: "s1",
    messages: [{ role: "assistant", text: "It landed T1.", tools: [], at_ms: 0 }],
    cursor: "s1@10",
    more: false,
    note: null,
    ...overrides,
  };
}

test("a page is appended to what the panel holds", () => {
  const first = fold(empty(), page());
  assert.equal(first.messages.length, 1);
  assert.equal(first.cursor, "s1@10");
  const second = fold(
    first,
    page({
      messages: [{ role: "user", text: "And T2?", tools: [] }],
      cursor: "s1@20",
    }),
  );
  assert.deepEqual(
    second.messages.map((message) => message.text),
    ["It landed T1.", "And T2?"],
  );
  assert.equal(second.cursor, "s1@20");
  assert.equal(second.program, "pi");
});

test("a new session starts the panel over rather than gluing two conversations", () => {
  const first = fold(empty(), page());
  const second = fold(first, page({ session: "s2", messages: [{ role: "user", text: "fresh" }] }));
  assert.deepEqual(
    second.messages.map((message) => message.text),
    ["fresh"],
  );
  assert.equal(second.session, "s2");
});

test("what the service told the agent stays news, and is not the person's side", () => {
  const chat = fold(empty(), page({ messages: [{ role: "news", text: "T7 needs you: pick one", tools: [] }] }));
  assert.equal(chat.messages[0].role, "news");
  assert.equal(mine(chat.messages[0]), false);
});

test("a message keeps its shape even when the service left a field out", () => {
  const chat = fold(empty(), page({ messages: [{ role: "assistant", text: "hi" }] }));
  assert.deepEqual(chat.messages[0], {
    role: "assistant",
    text: "hi",
    tools: [],
    at_ms: null,
  });
});

test("the panel keeps a bounded tail of a long conversation", () => {
  let chat = empty();
  chat = fold(chat, page({ cursor: "s1@1", older: "s1@0", messages: [{ role: "user", text: "m0" }] }));
  const burst = [];
  for (let n = 1; n < KEEP + 20; n += 1) burst.push({ role: "user", text: `m${n}` });
  chat = fold(chat, page({ cursor: "s1@2", messages: burst }));
  assert.equal(chat.messages.length, KEEP);
  assert.equal(chat.messages[0].text, "m20");
  assert.equal(chat.messages[KEEP - 1].text, `m${KEEP + 19}`);
  // Trimmed: the next read is a fresh first read, and earlier history stays reachable.
  assert.equal(chat.cursor, "");
  assert.equal(chat.older, "s1@0");
  assert.equal(timeline(chat)[0].kind, "older");
  const again = fold(chat, page({ cursor: "s1@9", older: "s1@5", messages: [{ role: "user", text: "fresh" }] }));
  assert.deepEqual(again.messages.map((m) => m.text), ["fresh"], "the first read replaces, never duplicates");
  assert.equal(again.older, "s1@5");
});

test("latest is the tail a short panel draws, oldest first", () => {
  const chat = {
    messages: [1, 2, 3, 4, 5].map((n) => ({ role: "user", text: `m${n}` })),
  };
  assert.deepEqual(
    latest(chat, 2).map((message) => message.text),
    ["m4", "m5"],
  );
  assert.deepEqual(latest(empty(), 3), []);
});

test("a tool call reads as one line, and the person's side is the right one", () => {
  assert.equal(toolLine({ name: "Bash", summary: "shprd tick --once" }), "Bash · shprd tick --once");
  assert.equal(toolLine({ name: "read" }), "read");
  assert.equal(mine({ role: "user" }), true);
  assert.equal(mine({ role: "assistant" }), false);
});

test("a long bubble is folded at a row length", () => {
  assert.equal(bubble("short"), "short");
  const long = bubble("x".repeat(BUBBLE_MAX + 50));
  assert.equal(long.length, BUBBLE_MAX);
  assert.ok(long.endsWith("…"));
  assert.equal(bubble(""), "");
});

test("an empty chat says what it is waiting for, and the service's note wins", () => {
  assert.equal(status(empty()), "Nothing yet.");
  assert.equal(status(fold(empty(), page())), null);
  assert.equal(
    status(fold(empty(), page({ messages: [], note: "pi has not run in /work/repo yet" }))),
    "pi has not run in /work/repo yet",
  );
});

test("only words worth sending are sent, and they name a space", () => {
  assert.deepEqual(outgoing(COORDINATOR, "  hello  "), { space: COORDINATOR, text: "hello" });
  assert.deepEqual(outgoing("T1", "hi"), { space: "T1", text: "hi" });
  assert.equal(outgoing(COORDINATOR, "   "), null);
  assert.equal(outgoing(COORDINATOR, ""), null);
});

test("the header names the program and the session, and the time is a clock", () => {
  assert.equal(header(empty()), "Coordinator");
  assert.equal(header(fold(empty(), page({ session: null }))), "pi · no session yet");
  assert.match(header(fold(empty(), page())), /^pi · s1/);
  assert.equal(when(null), "");
  assert.equal(when(0), "");
  assert.match(when(1_700_000_000_000), /^\d\d:\d\d$/);
});

test("the next request asks from the cursor the last page answered with", () => {
  assert.equal(nextCursor(empty()), "");
  assert.equal(nextCursor(fold(empty(), page())), "s1@10");
});

/**
 * A panel whose client records every call.
 *
 * `answer` is what a GET answers, or a list of answers given out in order, so a
 * test can prove a page with `more` is followed by another call. The client has
 * an `id()` and a `service.json` on disk naming the same one, because every panel
 * now asks `attach` before it reads whether the service it holds is still the one
 * that is running; a client with no record beside it would look like a restart.
 */
function panel(answers = [page()]) {
  const run = context();
  const calls = [];
  const queue = Array.isArray(answers) ? answers.slice() : [answers];
  herdr.putFile(serviceFile("/state"), '{"port": 7788, "id": "abc"}');
  const client = {
    dir: "/state",
    serviceDir: "/state",
    record: { port: 7788, id: "abc" },
    id: () => "abc",
    state: async () => ({ tasks: [], needs: [] }),
    asks: async () => ({ asks: [] }),
    answerAsk: async (task, ask, answer) => {
      calls.push({ method: "ANSWER", path: "/api/asks/answer", body: { task, ask, answer } });
      return { ok: true };
    },
    call: async (method, path, options = {}) => {
      calls.push({ method, path, body: options.body ?? null });
      if (method === "GET") return queue.length > 1 ? queue.shift() : queue[0];
      return { space: options.body?.space ?? COORDINATOR, sent: true };
    },
  };
  const view = new Chat();
  view.cx = run.cx;
  view.space = COORDINATOR;
  view.client = client;
  view.chat = empty();
  view.failure = null;
  view.refusal = null;
  view.sending = false;
  view.watching = true;
  view.opened = new Set();
  view.answering = null;
  view.answerRefusal = null;
  view.typing = null;
  view.draft = null;
  view.input = inputState({ placeholder: "Message the coordinator" });
  return { view, calls, ...run };
}

test("opening a card's answer box focuses it, and a refresh leaves it there", async () => {
  const { view } = panel([page({ messages: [] })]);
  const focused = [];
  view.input.focus = () => focused.push("composer");
  view.draft = inputState({ placeholder: "Type your answer" });
  view.draft.focus = () => focused.push("answer");
  const later = [];
  view.cx.timer.after = (ms, fn) => later.push(fn);
  view.type({ id: "ask-1" });
  assert.deepEqual(focused, ["answer"], "the box takes the keyboard when it opens");
  for (const fn of later) fn();
  assert.deepEqual(focused, ["answer", "answer"], "and again once it is drawn");
  await view.refresh();
  assert.equal(focused.includes("composer"), false, "a refresh never hands focus to the composer");
  // Closing the box before the timer fires does not focus a box that is gone.
  view.type({ id: "ask-1" });
  focused.length = 0;
  for (const fn of later) fn();
  assert.deepEqual(focused, []);
});

test("a submit types the composer's words into the coordinator and clears it", async () => {
  const { view, calls } = panel([page({ messages: [] })]);
  view.input.set_value("  what landed?  ");
  await view.send();
  const posted = calls.find((call) => call.method === "POST");
  assert.deepEqual(posted.body, { space: COORDINATOR, text: "what landed?" });
  assert.equal(view.input.value(), "", "the composer is empty after a send");
  assert.equal(view.failure, null);
});

test("an empty composer sends nothing at all", async () => {
  const { view, calls } = panel([page({ messages: [] })]);
  view.input.set_value("   ");
  await view.send();
  assert.equal(calls.filter((call) => call.method === "POST").length, 0);
});

test("a refused turn keeps the words and says why", async () => {
  const { view } = panel([page({ messages: [] })]);
  view.input.set_value("hello");
  view.client.call = async (method) => {
    if (method === "GET") return page({ messages: [] });
    const error = new Error("no open pane for the coordinator");
    error.name = "ServiceError";
    throw error;
  };
  await view.send();
  assert.equal(view.input.value(), "hello", "a refused line is retyped, not lost");
  assert.equal(view.refusal, "no open pane for the coordinator");
  assert.equal(view.sending, false);
});

test("a page is read from the cursor, and a page with more is followed at once", async () => {
  const first = page({ messages: [{ role: "user", text: "one" }], cursor: "s1@5", more: true });
  const second = page({ messages: [{ role: "user", text: "two" }], cursor: "s1@9", more: false });
  const { view, calls } = panel([first, second]);
  await view.refresh();
  const reads = calls.filter((call) => call.method === "GET");
  assert.equal(reads.length, 2, "the second page is asked for at once");
  assert.equal(reads[0].path, `/api/chat?space=coordinator&since=`);
  assert.equal(reads[1].path, `/api/chat?space=coordinator&since=s1%405`);
  assert.deepEqual(
    view.chat.messages.map((message) => message.text),
    ["one", "two"],
  );
});

test("a service that is not there says so in the panel rather than throwing", async () => {
  const { view } = panel([page({ messages: [] })]);
  view.client.call = async () => {
    const error = new Error("the service did not answer");
    error.name = "ServiceError";
    throw error;
  };
  await view.refresh();
  assert.equal(view.failure, "the service did not answer");
  assert.equal(view.client, null, "a dead client is dropped so the next look rediscovers");
});

// -- the full-size chat (M11 U4) ---------------------------------------------

/** A repository's coordinator space and one task's, as Herdr lists them. */
const SPACES = [
  { id: "sp1", name: "shprd · repo", tabs: [{ id: "tb1" }] },
  { id: "sp2", name: "shprd · repo · run1 · T1", tabs: [{ id: "tb2" }, { id: "tb3" }] },
  { id: "sp3", name: "scratch", tabs: [{ id: "tb4" }] },
];

test("a reply folds past FOLD_TALL lines to its first FOLD, and opens whole", () => {
  const short = "one\ntwo";
  assert.deepEqual(folding(short), { text: short, foldable: false, folded: false });
  const long = Array.from({ length: FOLD_TALL + 10 }, (_, n) => `line ${n}`).join("\n");
  const shown = folding(long);
  assert.equal(shown.folded, true);
  assert.equal(shown.foldable, true);
  assert.equal(shown.text.split("\n").length, FOLD);
  const open = folding(long, true);
  assert.deepEqual({ text: open.text, folded: open.folded, foldable: open.foldable }, { text: long, folded: false, foldable: true });
  assert.equal(WORDS.showMore, "Show more");
  assert.equal(WORDS.showLess, "Show less");
});

test("markup in what an agent wrote is text, in code and out of it", () => {
  assert.equal(literal("use <b>x</b> & &amp; more"), "use \\<b>x\\</b> & \\&amp; more");
  assert.equal(literal("see `<b>` and\n```\n<i>\n```"), "see `<b>` and\n```\n<i>\n```");
  const person = { role: "user" };
  assert.equal(typed("a\nb"), "a  \nb");
  assert.equal(typed("```\na\nb\n```"), "```\na\nb\n```");
  assert.equal(markdownOf(person, "a\nb <c>"), "a  \nb \\<c>");
  assert.equal(markdownOf({ role: "assistant" }, "a\nb"), "a\nb");
});

test("only web links are opened from a bubble", () => {
  assert.equal(safeLink("https://example.com/a?b=1"), true);
  assert.equal(safeLink("http://localhost:3000"), true);
  for (const bad of ["file:///etc/passwd", "javascript:alert(1)", "ssh://host", "mailto:a@b.c", "x", "", null, "https://a b"]) {
    assert.equal(safeLink(bad), false, String(bad));
  }
});

/** A card as `lib/asks.js` makes it. */
function waiting(over = {}) {
  return { id: "ask:T1:a1", from: "ask", task: "T1", ask: "a1", at: null, ...over };
}

test("the timeline puts waiting cards among the messages by time, and the rest last", () => {
  const chat = fold(
    empty("T1"),
    page({
      space: "T1",
      messages: [
        { role: "user", text: "go", at_ms: 100 },
        { role: "assistant", text: "done", at_ms: 300 },
      ],
    }),
  );
  const rows = timeline(chat, [waiting({ id: "late", at: 900 }), waiting({ id: "mid", at: 200 }), waiting({ id: "none" })]);
  assert.deepEqual(
    rows.map((row) => row.kind === "card" ? row.key : row.kind),
    ["top", "message", "mid", "message", "late", "none"],
  );
  assert.equal(firstCard(rows), 2);
  assert.equal(firstCard(timeline(chat, [])), -1);
  // A chat that has older pages to read has no "Start of conversation" yet.
  assert.equal(timeline({ ...chat, more: true, older: "s1@9" }, []).some((row) => row.kind === "top"), false);
});

test("a card belongs to its task's chat, and the run's to the coordinator's", () => {
  const list = [waiting(), waiting({ id: "need:3", from: "escalation", task: null }), waiting({ id: "other", task: "T2" })];
  assert.deepEqual(forSpace(list, "T1").map((card) => card.id), ["ask:T1:a1"]);
  assert.deepEqual(forSpace(list, "coordinator").map((card) => card.id), ["need:3"]);
  assert.deepEqual(forSpace(list, "T9"), []);
});

test("every card the Asks panel drew is in exactly one chat", () => {
  const state = { needs: [{ seq: 3, task: "T1", body: "decide", default_action: "wait" }, { seq: 4, task: null, body: "run level" }] };
  const asks = { asks: [{ task: "T2", id: "q", kind: "question", text: "which?", options: [] }] };
  const all = cardsOf(state, asks);
  const homes = all.map((card) => ["coordinator", "T1", "T2"].filter((space) => forSpace(all, space).includes(card)));
  assert.deepEqual(homes.map((home) => home.length), [1, 1, 1]);
});

test("the space a chat shows is the thread's task, a pane's own, or the coordinator's", () => {
  assert.equal(spaceOf({ props: { thread: true }, thread: { task: "T4" } }), "T4");
  assert.equal(spaceOf({ props: { thread: true }, thread: null }), null);
  assert.equal(spaceOf({ props: { space: "T2" } }), "T2");
  assert.equal(spaceOf({ props: {} }), COORDINATOR);
});

test("a tab's chat space is its task, the coordinator, or none", () => {
  assert.equal(spaceOfTab("tb1", SPACES), COORDINATOR);
  assert.equal(spaceOfTab("tb3", SPACES), "T1");
  assert.equal(spaceOfTab("tb4", SPACES), null);
  assert.equal(spaceOfTab("nope", SPACES), null);
});

test("the switch is drawn on agent panes of a chat space, and on a chat with a live agent", () => {
  const agent = { pane: "p1", terminal: "t1", tab: "tb2", status: "idle" };
  assert.deepEqual(paneChat({ pane: agent, spaces: SPACES }), { chatMode: false, agent: "t1", space: "T1", canSwitch: true });
  const coordinator = { pane: "p2", terminal: "t2", tab: "tb1", status: "working" };
  assert.equal(paneChat({ pane: coordinator, spaces: SPACES }).space, COORDINATOR);
  assert.equal(paneChat({ pane: { ...agent, status: null }, spaces: SPACES }).canSwitch, false, "a shell has no chat");
  assert.equal(paneChat({ pane: { ...agent, tab: "tb4" }, spaces: SPACES }).canSwitch, false, "another space has none");
  const parked = [{ address: "t1", state: "live", labels: { [PARK_LABEL]: "p9" } }];
  const chat = { pane: "p9", terminal: null, mod: CHAT_MOD, tab: "tb2" };
  assert.deepEqual(paneChat({ pane: chat, terminals: parked, spaces: SPACES }), {
    chatMode: true,
    agent: "t1",
    space: "T1",
    canSwitch: true,
  });
  assert.equal(agentOf("p9", [{ ...parked[0], state: "gone" }]), null, "a dead agent has nothing to go back to");
  assert.equal(paneChat({ pane: chat, terminals: [], spaces: SPACES }).canSwitch, false);
});

test("the thread's chat reads the task it is shown and starts over for another", async () => {
  const { view, calls } = panel([page({ space: "T1", messages: [{ role: "assistant", text: "hi" }], cursor: "s1@4" })]);
  herdr.setProps({ thread: true });
  herdr.setShared("tasks.thread", { task: "T1", title: "one" });
  view.thread = true;
  view.fixed = null;
  view.space = "T1";
  await view.refresh();
  assert.equal(calls.find((call) => call.method === "GET").path, "/api/chat?space=T1&since=");
  assert.equal(view.chat.messages.length, 1);
  herdr.setShared("tasks.thread", { task: "T2", title: "two" });
  view.follow_space();
  assert.equal(view.space, "T2");
  assert.equal(view.chat.messages.length, 0, "a different task is a different conversation");
  assert.equal(view.chat.cursor, "");
});

test("a card is answered through the service, once, and a refusal stays on the card", async () => {
  const { view, calls } = panel();
  const card = { id: "ask:T1:a1", task: "T1", ask: "a1" };
  await view.answer(card, "  Yes  ");
  assert.deepEqual(calls.find((call) => call.method === "ANSWER").body, { task: "T1", ask: "a1", answer: "Yes" });
  herdr.SHARED.clear(); // the first answer's op would (rightly) block a second press of the same card
  const { view: refused } = panel();
  refused.client.answerAsk = async () => {
    const error = new Error("the ask was already answered");
    error.name = "ServiceError";
    throw error;
  };
  await refused.answer(card, "Yes");
  assert.deepEqual(refused.answerRefusal, { card: "ask:T1:a1", text: "the ask was already answered" });
  assert.equal(refused.answering, null);
  // Nothing is sent for a blank answer.
  const { view: blank, calls: blankCalls } = panel();
  await blank.answer(card, "   ");
  assert.equal(blankCalls.some((call) => call.method === "ANSWER"), false);
});

test("a message's key is stable, so its fold state survives a page", () => {
  const message = { role: "assistant", text: "hello world", at_ms: 5 };
  assert.equal(messageKey(message), messageKey({ ...message }));
  assert.notEqual(messageKey(message), messageKey({ ...message, role: "user" }));
});

// -- the layout (M11 U4) -------------------------------------------------------

const LAYOUT = JSON.parse(readFileSync(new URL("../layouts/shprd.json", import.meta.url), "utf8"));

/** The layout node called `id`, or null. */
function node(id, at = LAYOUT) {
  if (at?.id === id) return at;
  for (const child of at?.children ?? []) {
    const found = node(id, child);
    if (found) return found;
  }
  return null;
}

test("the thread opens on its chat; the Asks panel and the sidebar's coordinator are gone", () => {
  const body = node("tasks-thread-body");
  assert.equal(body.active, "tasks-thread-chat");
  assert.deepEqual(
    body.children.map((child) => child.id),
    ["tasks-thread-chat", "tasks-thread-term", "tasks-thread-stream", "tasks-thread-pr", "tasks-thread-artifacts"],
  );
  assert.deepEqual(node("tasks-thread-chat").content, { mod: "shprd-chat", props: { thread: true } });
  assert.equal(node("asks"), null, "the cards are in the thread now");
  assert.equal(node("coordinator"), null, "the coordinator's chat is its pane's Chat view");
  const text = JSON.stringify(LAYOUT);
  assert.equal(text.includes("shprd-asks"), false);
  assert.equal(text.includes("shprd-coordinator"), false);
});

test("SHPR-69: the coordinator's chat says plainly when shprd's coordinator is not running", () => {
  const none = { live_agents: [] };
  assert.equal(coordinatorDown(COORDINATOR, none), WORDS.coordinatorDown);
  // Only another product's agent is in the space: shprd's list has no coordinator, so it still says so.
  const worker = { live_agents: [{ role: "worker", state: "working" }] };
  assert.equal(coordinatorDown(COORDINATOR, worker), WORDS.coordinatorDown);
  const ended = { live_agents: [{ role: "coordinator", state: "done", ended_ms: 5 }] };
  assert.equal(coordinatorDown(COORDINATOR, ended), WORDS.coordinatorDown);
  const running = { live_agents: [{ role: "coordinator", state: "idle" }] };
  assert.equal(coordinatorDown(COORDINATOR, running), null);
  // SHPR-70: pi's turn ended and its state reads done, but the process is alive
  // (no ended_ms): between two turns is up, not "not running".
  const between = { live_agents: [{ role: "coordinator", state: "done" }] };
  assert.equal(coordinatorDown(COORDINATOR, between), null);
  // A task's chat, and a state that does not say, are left alone.
  assert.equal(coordinatorDown("T1", none), null);
  assert.equal(coordinatorDown(COORDINATOR, null), null);
});

test("a run of tool calls is one folded row, and a reply's words end the run", () => {
  const bash = (summary) => ({ name: "Bash", summary });
  const chat = fold(
    empty(),
    page({
      messages: [
        { role: "user", text: "go", tools: [], at_ms: 1 },
        { role: "assistant", text: "", tools: [bash("ls"), { name: "Read", summary: "a.py" }], at_ms: 2 },
        { role: "assistant", text: "", tools: [bash("pwd")], at_ms: 3 },
        { role: "assistant", text: "Done.", tools: [bash("git log")], at_ms: 4 },
      ],
    }),
  );
  const rows = timeline(chat, []);
  const kinds = rows.map((row) => row.kind);
  assert.deepEqual(kinds, ["top", "message", "tools", "message", "tools"]);
  assert.equal(rows[2].calls.length, 3);
  assert.equal(toolsSummary(rows[2].calls), "Read a file, ran 2 commands");
  assert.equal(toolsSummary([bash("x")]), "Ran a command");
  assert.deepEqual(rows[3].message.tools, []);
});

test("an shprd agent's header says whose it is", () => {
  assert.equal(agentLabel("Claude Code"), "shprd · claude");
  assert.equal(agentLabel(""), "shprd");
});

test("the person's typed line breaks stay breaks", () => {
  assert.equal(markdownOf({ role: "user" }, "a\n1. b\n2. c"), "a  \n1. b  \n2. c");
});

// SHPR-80: a conversation never read says "Loading…" only briefly, never for ever.
test("an unread conversation says Loading only at first; later it is the empty chat (SHPR-80)", () => {
  const { view } = panel();
  view.loaded = new Set();
  view.slot = "k|coordinator";
  view.failure = null;
  const realNow = Date.now;
  record(true);
  try {
    // The Loading block holds one child (its words); the empty chat a title and a line.
    const kids = (el) => walk(el)[0].kids.length;
    view.slotSince = realNow();
    assert.equal(kids(view.blank({}, null)), 1, "Loading…");
    Date.now = () => realNow() + 10_000;
    assert.equal(kids(view.blank({}, null)), 2, "after a few seconds, never a spinner: the empty chat");
    view.loaded.add(view.slot);
    Date.now = realNow;
    assert.equal(kids(view.blank({}, null)), 2, "a conversation that was read and is empty");
  } finally {
    record(false);
    Date.now = realNow;
  }
});

test("a read given up on changes nothing if it finishes late (SHPR-80)", async () => {
  const { view } = panel([page({ messages: [{ role: "assistant", text: "late" }], cursor: "s1@4" })]);
  let release;
  view.client.state = () => new Promise((resolve) => { release = () => resolve({ tasks: [{ id: "OLD" }], needs: [] }); });
  view.gen = 1;
  const read = view.readOnce(view.cx, 1);
  await new Promise((resolve) => setTimeout(resolve, 20));
  view.gen = 2; // bounded() gave up on it and a newer read owns the state
  const before = view.state;
  release();
  await read;
  assert.equal(view.state, before, "the stale read left the board alone");
  // The conversation is read first, while this read still owned the view, so it is
  // already folded in before the board read is given up on.
  assert.equal(view.chat.messages.length, 1, "the conversation read while it was current stays");
});

// --- optimistic echo (m11-chat-latency) ---

import { echoOf, settleEchoes, withEchoes } from "../mods/lib/chat.js";

test("a send shows the turn and empties the composer before the service answers", async () => {
  const { view } = panel([page({ messages: [] })]);
  let release;
  view.client.call = (method, path) =>
    method === "GET"
      ? Promise.resolve(page({ messages: [] }))
      : new Promise((resolve) => {
          release = () => resolve({ sent: true });
        });
  view.input.set_value("what landed?");
  const pending = view.send();
  assert.equal(view.input.value(), "", "the composer is empty at once");
  const shown = withEchoes(view.chat, view.myEchoes()).messages;
  assert.deepEqual(shown.map((m) => [m.role, m.text]), [["user", "what landed?"]], "the turn shows before the POST returns");
  release();
  await pending;
});

test("the transcript's copy replaces the echo: no duplicate, no flicker", async () => {
  const { view } = panel([page({ messages: [], cursor: "s1@1" })]);
  view.input.set_value("what landed?");
  await view.send();
  const texts = () => withEchoes(view.chat, view.myEchoes()).messages.map((m) => m.text);
  assert.deepEqual(texts(), ["what landed?"], "the echo is the only copy until the transcript holds the turn");
  view.client.call = async () => page({ messages: [{ role: "user", text: "what landed?", at_ms: Date.now() + 5 }], cursor: "s1@2" });
  await view.pull(COORDINATOR, view.slot);
  assert.deepEqual(texts(), ["what landed?"], "then the transcript's alone");
  assert.equal(view.myEchoes().length, 0);
});

test("two identical turns keep two echoes, and one transcript line settles only one", () => {
  const a = echoOf({ text: "ok" }, 1000);
  const b = echoOf({ text: "ok" }, 1001);
  const left = settleEchoes([a, b], [{ role: "user", text: "ok" }], 1002);
  assert.deepEqual(left, [b]);
  assert.deepEqual(settleEchoes([a], [{ role: "assistant", text: "ok" }], 1002), [a], "only the person's lines confirm");
});

test("an echo the transcript never confirms goes after two minutes; an attachment echo names its files", () => {
  const a = echoOf({ text: "hi", attachments: [{ path: "/t/shot.png", name: "shot.png" }] }, 0);
  assert.match(a.shown, /hi\n📎 shot\.png/);
  assert.deepEqual(settleEchoes([a], [], 60_000), [a]);
  assert.deepEqual(settleEchoes([a], [], 130_000), []);
  const filesOnly = echoOf({ text: "", attachments: [{ path: "/t/shot.png", name: "shot.png" }] }, 0);
  assert.deepEqual(settleEchoes([filesOnly], [{ role: "user", text: "Attached file (open it with your file tools):\n/w/shot.png" }], 1), []);
});

test("a refused send takes the echo away and gives the words and files back", async () => {
  const { view } = panel([page({ messages: [] })]);
  view.files = [{ path: "/t/a.png", name: "a.png" }];
  view.input.set_value("hello");
  view.client.call = async (method) => {
    if (method === "GET") return page({ messages: [] });
    throw new Error("no open pane for the coordinator");
  };
  await view.send();
  assert.equal(view.myEchoes().length, 0, "no echo of a turn that was not sent");
  assert.equal(view.input.value(), "hello");
  assert.deepEqual(view.files, [{ path: "/t/a.png", name: "a.png" }]);
  assert.equal(view.refusal, "no open pane for the coordinator");
});

test("a send is posted exactly once, and a second Enter on the emptied composer sends nothing", async () => {
  const { view, calls } = panel([page({ messages: [] })]);
  view.input.set_value("once");
  const first = view.send();
  const second = view.send();
  await Promise.all([first, second]);
  assert.equal(calls.filter((c) => c.method === "POST").length, 1);
});

test("a page read twice from one cursor is folded once", async () => {
  const answer = page({ messages: [{ role: "assistant", text: "hi", at_ms: 1 }], cursor: "s1@3" });
  const { view } = panel([answer]);
  await Promise.all([view.pull(COORDINATOR, view.slot), view.pull(COORDINATOR, view.slot)]);
  assert.equal(view.chat.messages.length, 1);
});

test("a refused send keeps words typed while it was in flight, with the refused words before them", async () => {
  const { view } = panel([page({ messages: [] })]);
  view.input.set_value("first");
  view.client.call = async (method) => {
    if (method === "GET") return page({ messages: [] });
    view.input.set_value("second");
    throw new Error("no open pane");
  };
  await view.send();
  assert.equal(view.input.value(), "first\nsecond");
});

test("a read of one conversation never settles another conversation's echo", async () => {
  const { view } = panel([page({ messages: [{ role: "user", text: "same words", at_ms: Date.now() + 1 }], cursor: "s1@1" })]);
  view.echoes = [{ ...echoOf({ text: "same words" }), slot: "other-slot", sent: true }];
  await view.pull(COORDINATOR, view.slot);
  assert.equal(view.echoes.length, 1);
});

test("an echo is confirmed by the same turn, not by a longer line that starts alike or one from before the send", () => {
  const a = echoOf({ text: "ok" }, 100_000);
  assert.deepEqual(settleEchoes([a], [{ role: "user", text: "ok then, and also more" }], 100_001), [a], "a prefix is another message");
  assert.deepEqual(settleEchoes([a], [{ role: "user", text: "ok", at_ms: 1000 }], 100_001), [a], "an older line is another turn");
  assert.deepEqual(settleEchoes([a], [{ role: "user", text: "ok", at_ms: 100_500 }], 100_501), []);
  const withFile = echoOf({ text: "see", attachments: [{ path: "/t/s.png", name: "s.png" }] }, 100_000);
  assert.deepEqual(settleEchoes([withFile], [{ role: "user", text: "see\n\nAttached file (open it with your file tools):\n/w/s.png" }], 100_001), []);
  assert.deepEqual(settleEchoes([withFile], [{ role: "user", text: "see s.png" }], 100_001), [withFile], "no composed header, no match");
});

test("a refusal shows only on the conversation the send came from", async () => {
  const { view } = panel([page({ messages: [] })]);
  view.input.set_value("hello");
  view.client.call = async (method) => {
    if (method === "GET") return page({ messages: [] });
    view.slot = "another-slot";
    throw new Error("no open pane");
  };
  await view.send();
  assert.equal(view.refusal, null);
});

test("a first read opens at the newest messages and older pages go in front, in order", async () => {
  const tail = page({
    messages: [{ role: "user", text: "m3" }, { role: "assistant", text: "m4" }],
    cursor: "s1@40",
    older: "s1@20",
  });
  const earlier = page({ messages: [{ role: "user", text: "m1" }, { role: "assistant", text: "m2" }], cursor: "s1@40", older: "s1@8" });
  const first = page({ messages: [{ role: "user", text: "m0" }], cursor: "s1@40", older: null });
  const { view, calls } = panel([tail]);
  await view.refresh();
  assert.deepEqual(view.chat.messages.map((m) => m.text), ["m3", "m4"]);
  assert.equal(view.chat.older, "s1@20");
  assert.equal(timeline(view.chat, [])[0].kind, "older");
  // A forward page leaves the older cursor and the order alone.
  view.client.call = async (method, path) => {
    calls.push({ method, path });
    if (path.includes("before=s1%4020")) return earlier;
    if (path.includes("before=s1%408")) return first;
    return page({ messages: [{ role: "assistant", text: "m5" }], cursor: "s1@50" });
  };
  await view.refresh();
  assert.equal(view.chat.older, "s1@20");
  await view.loadOlder();
  assert.deepEqual(view.chat.messages.map((m) => m.text), ["m1", "m2", "m3", "m4", "m5"]);
  assert.equal(view.chat.cursor, "s1@50", "going back never moves the forward cursor");
  await view.loadOlder();
  assert.deepEqual(view.chat.messages.map((m) => m.text), ["m0", "m1", "m2", "m3", "m4", "m5"]);
  assert.equal(view.chat.older, null);
  assert.equal(timeline(view.chat, [])[0].kind, "top");
  // A reload (a new panel) opens at the newest again, not the beginning.
  const again = panel([tail]);
  await again.view.refresh();
  assert.equal(again.calls.find((c) => c.method === "GET").path, "/api/chat?space=coordinator&since=");
  assert.deepEqual(again.view.chat.messages.map((m) => m.text), ["m3", "m4"]);
});

test("loaded history is not trimmed away by new messages", () => {
  let chat = fold(empty(), page({ messages: [{ role: "user", text: "z" }], older: "s1@3" }));
  chat = foldOlder(chat, page({ messages: [{ role: "user", text: "y" }], older: null }));
  for (let n = 0; n < KEEP + 5; n += 1) chat = fold(chat, page({ messages: [{ role: "user", text: `n${n}` }] }));
  assert.equal(chat.messages[0].text, "y");
  assert.equal(foldOlder(chat, page({ session: "other" })), chat, "another session's page is dropped");
});

test("a pressed button shows its busy word at once, then what was chosen; a refusal restores the card", async () => {
  const { view } = panel();
  const [card] = cardsOf(
    { needs: [] },
    { asks: [{ id: "a1", task: "T1", kind: "question", text: "which?", options: ["CLI only", "Both"], created_at_ms: 1 }] },
  );
  let release;
  view.client.answerAsk = () => new Promise((resolve) => { release = () => resolve({ ok: true }); });
  const pending = view.choose(card, card.buttons[0]);
  assert.deepEqual(view.pressed.get(card.id), { label: "CLI only", busy: "Sending…", phase: "busy" });
  release();
  await pending;
  assert.equal(view.pressed.get(card.id).phase, "chose");
  assert.equal(view.pressed.get(card.id).label, "CLI only");

  herdr.SHARED.clear();
  const { view: refused } = panel();
  refused.client.answerAsk = async () => {
    const error = new Error("the ask was already answered");
    error.name = "ServiceError";
    throw error;
  };
  await refused.choose(card, card.buttons[1]);
  assert.equal(refused.pressed.has(card.id), false, "the card is back with its buttons");
  assert.deepEqual(refused.answerRefusal, { card: card.id, text: "the ask was already answered" });
});

test("a notice's Dismiss posts need.dismiss, and a Drop asks twice", async () => {
  const { view } = panel();
  const actions = [];
  view.client.action = async (payload) => { actions.push(payload); return { ok: true }; };
  const [notice, decision] = cardsOf(
    {
      needs: [
        { seq: 5, task: "T1", body: "back in the queue", default_action: "x", kind: "notice", actions: ["dismiss"], key: "need:5" },
        { seq: 6, task: "T2", body: "pick", default_action: "y", kind: "decision", actions: ["drop"], key: "need:6" },
      ],
    },
    { asks: [] },
  );
  await view.choose(notice, notice.buttons[0]);
  assert.deepEqual(actions, [{ kind: "need.dismiss", seq: 5 }]);
  await view.choose(decision, decision.buttons[0]);
  assert.equal(actions.length, 1, "the first press only arms the drop");
  await view.choose(decision, decision.buttons[0]);
  assert.equal(actions[1].kind, "task.drop");
});
