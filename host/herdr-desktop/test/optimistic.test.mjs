// The optimistic layer (SHPR-81): a pressed action shows in every pane at once,
// holds against a refresh that still shows the old state, steps aside for the
// outcome, and rolls back with one plain line.
//
//   node --import ./test/boot.mjs --test test/

import test, { beforeEach } from "node:test";
import assert from "node:assert/strict";

import * as herdr from "herdr";
import { RepoCache, press } from "../mods/lib/client.js";
import * as opt from "../mods/lib/optimistic.js";
import { counts, rowActions, secondLine, runShort } from "../mods/lib/board.js";
import { graphRows } from "../mods/lib/tree.js";

const FOLDER = "/work/shop";
beforeEach(() => herdr.SHARED.clear());

const task = (id, status, word, extra = {}) => ({ id, title: `Task ${id}`, status, derived: { status: word }, deps: [], ...extra });
const board = () => ({
  run: { id: "r1", integration_branch: "integration/m1" },
  tasks: [task("A", "passed", "ready"), task("B", "queued", "queued", { deps: ["A"] }), task("C", "working", "working")],
  needs: [{ seq: 1, task: "A", body: "A is ready to land" }],
});

function cacheWith(raw) {
  const cache = new RepoCache();
  cache.set(FOLDER, raw);
  return cache;
}

test("Accept moves the row to Merging at once and starts the task that waited on it", () => {
  const raw = board();
  const cache = cacheWith(raw);
  const op = opt.begin({ kind: "task.accept", tasks: ["A"], board: raw, folder: FOLDER });
  assert.ok(op);
  const shown = cache.get(FOLDER);
  const by = (id) => shown.tasks.find((t) => t.id === id);
  assert.equal(by("A").derived.status, "merging");
  assert.equal(secondLine(by("A")), "Merging…");
  assert.equal(by("B").derived.status, "working", "the dependent shows as starting");
  assert.equal(secondLine(by("B")), "Starting…");
  assert.deepEqual(rowActions(by("A")), [], "no pill left on a merging row");
  assert.equal(shown.needs.length, 0, "its escalation card is gone from the chat");
  // The status bar counts what the panes now show.
  assert.ok(counts(shown).working > counts(raw).working, "the bar counts the dependent as working");
  assert.match(runShort(shown), /working/);
  // The service's own board is untouched.
  assert.equal(cache.raw(FOLDER).tasks[0].derived.status, "ready");
  assert.equal(raw.tasks[0].derived.status, "ready");
});

test("the Graph shows the pending word, and every pane that reads the shared list sees the same", () => {
  const raw = board();
  opt.begin({ kind: "task.accept", tasks: ["A"], board: raw, folder: FOLDER });
  const other = cacheWith(raw); // another pane's cache of the same board
  const rows = graphRows(other.get(FOLDER));
  assert.equal(rows.find((r) => r.id === "A").word, "Merging…");
  assert.equal(rows.find((r) => r.id === "B").word, "Starting…");
});

test("Review says Reviewing until the card closes", () => {
  const raw = board();
  const cache = cacheWith(raw);
  opt.startReview(raw.tasks[0], FOLDER);
  assert.equal(secondLine(cache.get(FOLDER).tasks[0]), "Reviewing…");
  assert.equal(cache.get(FOLDER).tasks[0].derived.status, "ready", "still Ready: Review decides nothing");
  opt.finishReview("A", FOLDER);
  assert.equal(cache.get(FOLDER).tasks[0].pending, undefined);
});

test("Stop shows Stopping…; Drop removes the row; an answered question's card goes", () => {
  const raw = board();
  const cache = cacheWith(raw);
  opt.begin({ kind: "task.stop", tasks: ["C"], board: raw, folder: FOLDER });
  const c = cache.get(FOLDER).tasks.find((t) => t.id === "C");
  assert.equal(secondLine(c), "Stopping…");
  assert.equal(c.derived.status, "needs_you");
  opt.begin({ kind: "task.drop", tasks: ["B"], board: raw, folder: FOLDER });
  assert.equal(cache.get(FOLDER).tasks.find((t) => t.id === "B").status, "dropped");
  opt.begin({ kind: "ask.answer", tasks: ["A"], board: raw, folder: FOLDER, extra: { ask: "q1", remaining: 1 } });
  assert.ok(opt.answeredAsks(FOLDER).has("A/q1"));
});

test("a refresh that still shows the old state does not undo it; one that shows the outcome wins", () => {
  const raw = board();
  const cache = cacheWith(raw);
  const op = opt.begin({ kind: "task.accept", tasks: ["A"], board: raw, folder: FOLDER });
  opt.confirm(op.id);
  // A refresh arrives with the same old state: still merging on screen.
  cache.set(FOLDER, board());
  assert.equal(cache.get(FOLDER).tasks[0].derived.status, "merging");
  // The next refresh shows the real outcome: Done, and nothing of the overlay is left on it.
  const landed = board();
  landed.tasks[0] = task("A", "integrated", "done");
  cache.set(FOLDER, landed);
  const a = cache.get(FOLDER).tasks[0];
  assert.equal(a.derived.status, "done");
  assert.equal(a.pending, undefined);
});

test("double presses are ignored, while a different verb on a confirmed task is a new decision", () => {
  const raw = board();
  const op = opt.begin({ kind: "task.accept", tasks: ["A"], board: raw, folder: FOLDER });
  assert.equal(opt.begin({ kind: "task.accept", tasks: ["A"], board: raw, folder: FOLDER }), null);
  assert.equal(opt.begin({ kind: "task.drop", tasks: ["A"], board: raw, folder: FOLDER }), null, "pending: no second verb");
  opt.confirm(op.id);
  assert.equal(opt.begin({ kind: "task.accept", tasks: ["A"], board: raw, folder: FOLDER }), null, "confirmed: same verb still a double press");
  assert.ok(opt.begin({ kind: "task.drop", tasks: ["A"], board: raw, folder: FOLDER }));
});

test("press: the effect shows before the service answers, a refusal rolls back with one plain line", async () => {
  const cache = cacheWith(board());
  const owner = { client: { folder: FOLDER }, cache, state: cache.get(FOLDER), cx: { notify() {} } };
  let seenBefore = null;
  const refused = await press(owner, {
    kind: "task.accept",
    tasks: ["A"],
    send: async () => {
      seenBefore = cache.get(FOLDER).tasks[0].derived.status;
      throw new Error("merge conflict in src/lib.rs");
    },
  });
  assert.equal(seenBefore, "merging", "the pane already showed Merging when the request went out");
  assert.equal(refused.ok, false);
  assert.equal(refused.error, "Accept A: merge conflict in src/lib.rs");
  assert.equal(owner.state.tasks[0].derived.status, "ready", "back to the true state");
  assert.equal(opt.errorLine(owner.state), "Accept A: merge conflict in src/lib.rs");
  const ok = await press(owner, { kind: "task.start", tasks: ["C"], send: async () => ({ ok: true }) });
  assert.equal(ok.ok, true);
});

test("a merge the service cannot do (a failed land after the confirm) rolls an Accept back with the reason", () => {
  const raw = board();
  const op = opt.begin({ kind: "task.accept", tasks: ["A"], board: raw, folder: FOLDER, now: 1000 });
  opt.confirm(op.id, 1000);
  const after = { ...board(), last_tick: { at_ms: 2000, failed: [{ step: "land", task: "A", error: "Couldn't merge A: conflict in src/lib.rs" }] } };
  const shown = opt.overlay(after, FOLDER, opt.read().ops, 2500);
  assert.equal(shown.tasks[0].derived.status, "ready");
  assert.equal(opt.errorLine(shown), "Couldn't merge A: conflict in src/lib.rs");
  // An old failure (before the press) is not this press's.
  const old = { ...board(), last_tick: { at_ms: 500, failed: [{ step: "land", task: "A", error: "x" }] } };
  assert.equal(opt.overlay(old, FOLDER, opt.read().ops, 2500).tasks[0].derived.status, "merging");
});

test("ops expire by their timestamps, and another repository's ops are not applied", () => {
  const raw = board();
  const op = opt.begin({ kind: "task.accept", tasks: ["A"], board: raw, folder: FOLDER, now: 0 });
  assert.equal(opt.overlay(raw, FOLDER, opt.read().ops, opt.PENDING_MS + 1).tasks[0].derived.status, "ready");
  assert.equal(opt.overlay(raw, "/work/blog", opt.read().ops, 1).tasks[0].derived.status, "ready");
  opt.confirm(op.id, 0);
  assert.equal(opt.overlay(raw, FOLDER, opt.read().ops, opt.ACCEPT_MS + 1).tasks[0].derived.status, "ready");
  assert.equal(counts(raw).total, 3);
});

test("an Accept whose merge the engine could not do shows the task as it is, with the engine's words", () => {
  const raw = board();
  const op = opt.begin({ kind: "task.accept", tasks: ["A"], board: raw, folder: FOLDER, now: 1000 });
  opt.confirm(op.id, 1000);
  // 20 s later the engine tried: the task is failed and says why.
  const after = board();
  after.tasks[0] = { ...task("A", "failed", "needs_you"), last_status: { message: "Waiting: Couldn't merge A: its work conflicts with the integration branch in 1 file" } };
  const shown = opt.overlay(after, FOLDER, opt.read().ops, 21_000);
  assert.equal(shown.tasks[0].derived.status, "needs_you", "the true state, not Merging");
  assert.equal(opt.errorLine(shown), "Couldn't merge A: its work conflicts with the integration branch in 1 file");
  // Still holding Merging at 20 s when the board has shown nothing yet.
  assert.equal(opt.overlay(board(), FOLDER, opt.read().ops, 21_000).tasks[0].derived.status, "merging");
});

test("a newer action on a confirmed task wins over the older one", () => {
  const raw = { tasks: [task("S", "queued", "to_do")] };
  const start = opt.begin({ kind: "task.start", tasks: ["S"], board: raw, folder: FOLDER });
  opt.confirm(start.id);
  const stop = opt.begin({ kind: "task.stop", tasks: ["S"], board: raw, folder: FOLDER });
  assert.ok(stop, "a different verb on a confirmed task is a new decision");
  const shown = opt.overlay(raw, FOLDER, opt.read().ops);
  assert.equal(shown.tasks[0].pending.label, "Stopping…");
});
