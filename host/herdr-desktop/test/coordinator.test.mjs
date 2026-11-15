// Unit tests for the coordinator's chat: the view model that folds a page into
// rows, and the panel that sends a turn.
//
//   node --import ./test/boot.mjs --test test/       (node 22+)
//   bun test --preload ./test/boot-bun.mjs test/     (bun)
//
// The pure half is `mods/lib/chat.js`; the panel half drives the mod's own
// `send` and `refresh` with a client that records its calls, so nothing here
// needs a window, a service or a transcript.

import test from "node:test";
import assert from "node:assert/strict";

import Coordinator from "../mods/shprd-coordinator/main.js";
import {
  BUBBLE_MAX,
  COORDINATOR,
  KEEP,
  bubble,
  empty,
  fold,
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
  for (let n = 0; n < KEEP + 20; n += 1) {
    chat = fold(chat, page({ messages: [{ role: "user", text: `m${n}` }] }));
  }
  assert.equal(chat.messages.length, KEEP);
  assert.equal(chat.messages[0].text, "m20");
  assert.equal(chat.messages[KEEP - 1].text, `m${KEEP + 19}`);
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
 * test can prove a page with `more` is followed by another call.
 */
function panel(answers = [page()]) {
  const run = context();
  const calls = [];
  const queue = Array.isArray(answers) ? answers.slice() : [answers];
  const client = {
    dir: "/state",
    call: async (method, path, options = {}) => {
      calls.push({ method, path, body: options.body ?? null });
      if (method === "GET") return queue.length > 1 ? queue.shift() : queue[0];
      return { space: options.body?.space ?? COORDINATOR, sent: true };
    },
  };
  const view = new Coordinator();
  view.cx = run.cx;
  view.space = COORDINATOR;
  view.client = client;
  view.chat = empty();
  view.failure = null;
  view.refusal = null;
  view.sending = false;
  view.watching = true;
  view.input = inputState({ placeholder: "Message the coordinator" });
  return { view, calls, ...run };
}

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
