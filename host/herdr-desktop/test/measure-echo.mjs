// Times what the person sees after pressing send in the coordinator chat.
//
//   node --import ./test/boot.mjs test/measure-echo.mjs
//
// A stand-in service gives each hop a fixed, realistic cost (the delivery into the
// terminal 150 ms, a chat page read 60 ms, the board and asks 40 ms each, and the
// agent's transcript holding the turn 400 ms after it was delivered), so the number
// printed is the mod's own: how long after the key press the turn shows, the
// composer empties, and the chat has settled on the transcript's single copy.

import * as herdr from "herdr";
import Chat from "../mods/shprd-chat/main.js";
import { serviceFile } from "../mods/lib/client.js";
import * as lib from "../mods/lib/chat.js";
import { inputState } from "./stub-gpui.js";

const DELIVER = 150;
const PAGE = 60;
const BOARD = 40;
const TRANSCRIPT_AFTER = 400;
const wait = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const timers = [];
const cx = {
  sleep: wait,
  notify: () => {},
  timer: { every: () => ({ cancel() {} }), after: (ms, fn) => { timers.push(setTimeout(fn, ms)); return { cancel() {} }; } },
  spawn: () => ({ cancel() {} }),
};
herdr.putFile(serviceFile("/state"), '{"port": 7788, "id": "abc"}');
let delivered = null;
let held = [];
const text = "what landed?";
const client = {
  dir: "/state", serviceDir: "/state", record: { port: 7788, id: "abc" }, id: () => "abc",
  state: async () => { await wait(BOARD); return { tasks: [], needs: [] }; },
  asks: async () => { await wait(BOARD); return { asks: [] }; },
  call: async (method, path, options = {}) => {
    if (method === "POST") {
      await wait(DELIVER);
      delivered = Date.now();
      return { sent: true };
    }
    await wait(PAGE);
    const there = delivered !== null && Date.now() - delivered >= TRANSCRIPT_AFTER;
    const messages = there && held.length === 0 ? [{ role: "user", text, at_ms: Date.now() }] : [];
    if (there) held = messages.length ? messages : held;
    return { space: "coordinator", program: "claude", session: "s1", messages, cursor: there ? "s1@2" : "s1@1", more: false };
  },
};
const view = new Chat();
Object.assign(view, { cx, space: "coordinator", client, chat: lib.empty(), failure: null, refusal: null, sending: false, watching: true, opened: new Set(), answering: null, answerRefusal: null, typing: null, draft: null });
view.input = inputState({ placeholder: "Message the coordinator" });

const drawn = () => {
  const chat = lib.withEchoes ? lib.withEchoes(view.chat, view.myEchoes()) : view.chat;
  return chat.messages.filter((message) => message.text === text).length;
};
view.input.set_value(text);
const t0 = Date.now();
const sending = view.send();
const sample = { cleared: null, shown: null, settled: null };
const done = (async () => {
  while (Date.now() - t0 < 4000) {
    if (sample.cleared === null && view.input.value() === "") sample.cleared = Date.now() - t0;
    if (sample.shown === null && drawn() >= 1) sample.shown = Date.now() - t0;
    if (sample.shown !== null && view.chat.messages.some((m) => m.text === text) && (view.myEchoes?.() ?? []).length === 0 && sample.settled === null) sample.settled = Date.now() - t0;
    if (drawn() > 1) { sample.duplicate = true; }
    await wait(5);
  }
})();
await sending;
sample.postReturned = Date.now() - t0;
await done;
timers.forEach(clearTimeout);
console.log(JSON.stringify({ ...sample, copiesAtEnd: drawn() }));
process.exit(0);
