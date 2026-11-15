// Unit tests for the four mods' view models: shprd's state and Herdr's window in,
// rows and toasts out.
//
//   node --import ./test/boot.mjs --test test/          (node 22+)
//   bun test --preload ./test/boot-bun.mjs test/        (bun)
//
// The fixtures are a real service's answers, captured from a throwaway run
// (`shprd serve` over a repository with six tasks in six of the eight words, a
// landed task, a failed review and a failed pass) and a real `herdr-app ls` of
// the window that showed it: `test/fixtures/state.json`, `changes.json` and
// `spaces.json`. Nothing here invents a shape the service does not send.

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import {
  COORDINATOR_ID,
  KIND,
  coordinatorRow,
  needWords,
  orderedTasks,
  rowBadge,
  rowCounts,
  sidebarRows,
  spaceIndex,
  splitSpaceName,
  taskLook,
} from "../mods/lib/spaces.js";
import {
  ROLE,
  paneState,
  panelButtons,
  parseTabName,
  pillCounts,
  tabRows,
  tabState,
} from "../mods/lib/tabstrip.js";
import {
  BODY_MAX,
  KEEP,
  KEEP_MS,
  SHOWN,
  TIMEOUT_DEFAULT,
  clip,
  dismiss,
  fromSnapshot,
  levelTone,
  mergeNews,
  newsItems,
  passItem,
  timeoutOf,
  visible,
} from "../mods/lib/toasts.js";
import {
  counts,
  lastFailure,
  runLabel,
  runShort,
  runState,
  since,
  statusCounts,
} from "../mods/lib/board.js";

/** One fixture, parsed. */
function fixture(name) {
  return JSON.parse(readFileSync(fileURLToPath(new URL(`./fixtures/${name}`, import.meta.url)), "utf8"));
}

/** The snapshot a real `GET /api/state` answered. */
const STATE = fixture("state.json");
/** The events a real `GET /api/changes?since=0` answered, as the notice shapes. */
const EVENTS = fixture("changes.json").events;
/** The spaces and tabs a real `herdr-app ls` answered. */
const SPACES = fixture("spaces.json").spaces;
/** The T2 space of that window: a worker and a reviewer's tab. */
const SPACE_T2 = SPACES.find((space) => space.name.endsWith("· T2"));

/** One task of the fixture snapshot, by id. */
function task(id) {
  return STATE.tasks.find((entry) => entry.id === id);
}

// -- the sidebar's reading of a space name ---------------------------------

test("a space name says whether shprd owns it", () => {
  assert.deepEqual(splitSpaceName("shprd · shop"), {
    kind: KIND.coordinator,
    repo: "shop",
    name: "shprd · shop",
  });
  assert.deepEqual(splitSpaceName("shprd · shop · m10a · T1"), {
    kind: KIND.task,
    repo: "shop",
    run: "m10a",
    task: "T1",
    name: "shprd · shop · m10a · T1",
  });
  // A person's own space, and a name that is almost ours: neither is a row.
  assert.equal(splitSpaceName("worktree: fix login").kind, "other");
  assert.equal(splitSpaceName("shprd · shop · extra").kind, "other");
  assert.equal(splitSpaceName("someone · shop").kind, "other");
  assert.equal(splitSpaceName("").kind, "other");
});

test("the spaces shprd owns are sorted into the coordinator's and each task's", () => {
  const index = spaceIndex([
    { id: "s0", name: "my own space" },
    { id: "s1", name: "shprd · shop" },
    { id: "s2", name: "shprd · shop · m10a · T1" },
    { id: "s3", name: "shprd · shop · m10a · T1" },
  ]);
  assert.equal(index.coordinator.id, "s1");
  assert.equal(index.coordinator.repo, "shop");
  assert.equal(index.tasks.get("T1").id, "s2", "the first space with a label wins");
  assert.equal(index.tasks.has("T2"), false);
});

// -- the sidebar's rows ----------------------------------------------------

test("every task gets one row, in the order the board groups them", () => {
  const rows = sidebarRows(STATE, SPACES);
  assert.deepEqual(
    rows.tasks.map((row) => row.id),
    ["T3", "T2", "T1", "T4"],
    "needs you, then ready, then working, then done",
  );
  assert.deepEqual(
    rows.waiting.map((row) => row.id),
    ["T6", "T5"],
    "queued, then to do: no space of their own yet",
  );
  assert.equal(rows.coordinator.kind, KIND.coordinator);
  assert.equal(rows.coordinator.id, COORDINATOR_ID);
  assert.equal(rows.coordinator.label, "shop");
  assert.equal(rows.coordinator.sub, "run m10a · integration/m10");
  assert.equal(rows.coordinator.word, "Running");
  assert.equal(rows.coordinator.tone, "amber");
  assert.equal(rows.coordinator.needs, true, "T3 needs the person");
});

test("a task row carries the word, the colour and the reason the service gave it", () => {
  const rows = sidebarRows(STATE, SPACES);
  const t3 = rows.tasks.find((row) => row.id === "T3");
  assert.equal(t3.word, "Needs you");
  assert.equal(t3.tone, "red");
  assert.equal(t3.glyph, "dot");
  assert.equal(t3.needs, true);
  assert.equal(t3.ready, false);
  assert.match(t3.sub, /the count badge overflows/);
  assert.equal(t3.folder, "/tmp/sh55/shop");

  const t2 = rows.tasks.find((row) => row.id === "T2");
  assert.equal(t2.word, "Ready");
  assert.equal(t2.tone, "green");
  assert.equal(t2.needs, false);
  assert.equal(t2.ready, true);
  assert.equal(t2.active, true, "the window is showing T2's space");
  assert.equal(t2.title, "Tab strip");

  const t4 = rows.tasks.find((row) => row.id === "T4");
  assert.equal(t4.word, "Done");
  assert.equal(t4.active, false);
});

test("a task with no space is a row that cannot be clicked, and says why", () => {
  const rows = sidebarRows(STATE, SPACES);
  for (const row of rows.waiting) {
    assert.equal(row.kind, KIND.waiting);
    assert.equal(row.space, null);
  }
  const t6 = rows.waiting.find((row) => row.id === "T6");
  assert.equal(t6.word, "Queued");
  assert.equal(t6.tone, "dim");
  assert.equal(t6.sub, "after T1");
});

test("the whole board is still read when the window has no spaces yet", () => {
  const rows = sidebarRows(STATE, []);
  assert.equal(rows.tasks.length, 0);
  assert.equal(rows.waiting.length, STATE.tasks.length);
  assert.equal(rows.coordinator.needs, true);
  assert.equal(rows.coordinator.space, null);
});

test("no run is a board with a hint, not an empty sidebar", () => {
  const rows = sidebarRows({ seq: 0, run: null, hint: "no run yet: start one with `shprd run start`", tasks: [] }, []);
  assert.equal(rows.coordinator.word, "No run");
  assert.equal(rows.coordinator.tone, "dim");
  assert.equal(rows.coordinator.label, "This repository");
  assert.equal(rows.coordinator.sub, "no run yet: start one with `shprd run start`");
  assert.deepEqual(rows.tasks, []);
  assert.deepEqual(rows.waiting, []);
});

test("the dropped tasks a run keeps are history, not rows", () => {
  const stale = {
    ...STATE,
    tasks: [{ id: "T9", title: "dropped one", status: "dropped", derived: { status: "done" }, attempts: [] }],
  };
  assert.deepEqual(orderedTasks(stale), []);
  assert.deepEqual(sidebarRows(stale, []).tasks, []);
});

test("a row's badge is a count on the run and a mark on a task", () => {
  const rows = sidebarRows(STATE, SPACES);
  assert.equal(rowBadge(rows.coordinator), "1", "how many tasks need the person");
  assert.equal(rowBadge(rows.tasks.find((row) => row.id === "T3")), "!");
  assert.equal(rowBadge(rows.tasks.find((row) => row.id === "T2")), null);
  assert.equal(rowBadge(null), null);
});

test("row counts and task looks are the tokens' words", () => {
  const tally = rowCounts(sidebarRows(STATE, SPACES));
  assert.deepEqual(tally, { needs: 1, ready: 1, working: 1, total: 6 });
  assert.deepEqual(taskLook(task("T1")), {
    status: "working",
    word: "Working",
    tone: "amber",
    glyph: "pie",
  });
  assert.deepEqual(taskLook({}), { status: "to_do", word: "To do", tone: "dim", glyph: "ring" });
});

test("a needs-you row says the card's words when the service has a card", () => {
  assert.match(needWords(task("T3"), STATE), /the count badge overflows its row at ten/);
  assert.equal(needWords(task("T1"), STATE), null);
  assert.equal(needWords(task("T3"), { tasks: STATE.tasks, needs: [] }), "the count badge overflows its row at ten");
});

test("the coordinator row says the run's state, finished or not", () => {
  const finished = coordinatorRow({ ...STATE, finished: true }, SPACES[0]);
  assert.equal(finished.word, "Finished");
  assert.equal(finished.tone, "green");
  assert.equal(finished.glyph, "tick");
});

// -- the tab bar's reading of a tab ----------------------------------------

test("a tab's title names its agent, its task and its role", () => {
  assert.deepEqual(parseTabName("claude·T1"), {
    titled: true,
    agent: "claude",
    task: "T1",
    role: "worker",
    name: "claude·T1",
  });
  const review = parseTabName("codex·T2·review");
  assert.equal(review.role, "reviewer");
  assert.equal(review.agent, "codex");
  assert.equal(review.task, "T2");
  assert.equal(ROLE[review.role], "Reviewer");
  assert.equal(parseTabName("pi·T3·backup").role, "backup");
  // A title shprd did not give a tab claims no agent and no task.
  const own = parseTabName("Assistant");
  assert.equal(own.titled, false);
  assert.equal(own.agent, null);
  assert.equal(own.task, null);
  assert.equal(parseTabName("").titled, false);
});

test("a pill says the host's state when it has one, else the task's own word", () => {
  const space = SPACE_T2;
  const rows = tabRows(space, [], STATE);
  assert.deepEqual(
    rows.map((row) => row.label),
    ["claude·T2", "codex·T2·review"],
  );
  assert.equal(rows[0].active, false);
  for (const row of rows) {
    assert.equal(row.from, "shprd", "a shell reports no agent status");
    assert.equal(row.task, "T2");
    assert.equal(row.word, "Ready");
    assert.equal(row.tone, "green");
    assert.equal(row.title, "Tab strip");
  }
  assert.equal(rows[1].roleWord, "Reviewer");

  // The host's word wins: a reviewer at work is Working, whatever the task says.
  const working = tabRows(space, [{ tab: rows[1].id, status: "working" }], STATE);
  assert.equal(working[1].from, "host");
  assert.equal(working[1].word, "Working");
  assert.equal(working[1].tone, "amber");
  assert.equal(working[0].word, "Ready", "the worker's own tab is unchanged");

  // A blocked agent needs the person, and the pill says so.
  const blocked = tabRows(space, [{ tab: rows[1].id, status: "blocked" }], STATE);
  assert.equal(blocked[1].word, "Needs you");
  assert.equal(blocked[1].needs, true);
});

test("a tab whose title is not shprd's is still a pill, with no claims", () => {
  const space = { id: "s", name: "shprd · shop", tabs: [{ id: "tb1", name: "zsh", active: true, panes: 1 }] };
  const rows = tabRows(space, [], STATE);
  assert.equal(rows.length, 1);
  assert.equal(rows[0].label, "zsh");
  assert.equal(rows[0].agent, null);
  assert.equal(rows[0].role, null);
  assert.equal(rows[0].word, "Idle");
  assert.equal(rows[0].tone, "dim");
  assert.equal(rows[0].active, true);
});

test("a space with no tabs draws no pills", () => {
  assert.deepEqual(tabRows({ id: "s", name: "shprd · shop" }, [], STATE), []);
  assert.deepEqual(pillCounts([]), { total: 0, working: 0, needs: 0, ready: 0 });
});

test("the most urgent status in a tab is the one its pill shows", () => {
  const panes = [
    { tab: "a", status: "working" },
    { tab: "a", status: "blocked" },
    { tab: "a", status: "idle" },
    { tab: "b", status: "done" },
  ];
  assert.equal(paneState(panes, "a"), "blocked");
  assert.equal(paneState(panes, "b"), "done");
  assert.equal(paneState(panes, "c"), null);
  assert.equal(paneState([], "a"), null);
  assert.equal(tabState({ id: "c", name: "claude·T1" }, panes, STATE).status, "working");
});

test("the side panel's buttons carry the counts of the stacks they open", () => {
  const buttons = panelButtons(STATE, "asks");
  assert.deepEqual(
    buttons.map((button) => button.id),
    ["files", "changes", "tasks", "asks", "settings"],
  );
  const tasks = buttons.find((button) => button.id === "tasks");
  assert.deepEqual({ needs: tasks.needs, ready: tasks.ready, active: tasks.active }, {
    needs: 1,
    ready: 1,
    active: false,
  });
  assert.equal(buttons.find((button) => button.id === "asks").active, true);
  assert.equal(buttons.find((button) => button.id === "files").needs, 0);
});

test("the pills' counts are the bar's summary", () => {
  const rows = tabRows(SPACE_T2, [{ tab: SPACE_T2.tabs[1].id, status: "blocked" }], STATE);
  assert.deepEqual(pillCounts(rows), { total: 2, working: 0, needs: 1, ready: 1 });
});

// -- the toasts' reading of the news ---------------------------------------

test("the news is the events a person acts on, each with the seq that made it", () => {
  const items = newsItems(EVENTS, STATE);
  assert.deepEqual(
    items.map((item) => item.id),
    ["ev:17", "ev:18", "ev:24", "ev:25"],
    "the failed review, a needs-you card, the landing, and the second card",
  );
  const landed = items[2];
  assert.equal(landed.level, "success");
  assert.equal(landed.title, "T4 · Landed");
  assert.equal(landed.body, "Toasts · its work is on integration/m10");

  const review = items[0];
  assert.equal(review.level, "error");
  assert.equal(review.title, "T3 · Review failed");
  assert.equal(review.body, "the count badge overflows its row at ten");

  const needs = items[1];
  assert.equal(needs.level, "warning");
  assert.equal(needs.title, "T3 · Needs you");
  assert.match(needs.body, /the count badge overflows its row at ten/);
  assert.notEqual(items[1].id, items[3].id, "two cards are two toasts");
});

test("a card about a task the board calls finished is not news", () => {
  const on_a_done_task = { seq: 40, kind: "escalated", task: "T4" };
  assert.deepEqual(newsItems([on_a_done_task], STATE), []);
  const on_a_live_task = { seq: 41, kind: "escalated", task: "T1" };
  assert.equal(newsItems([on_a_live_task], STATE).length, 1);
  // A landing is worth telling however the task reads now.
  assert.equal(newsItems([{ seq: 42, kind: "task_landed", task: "T4" }], STATE).length, 1);
});

test("a batch of events nobody acts on says nothing", () => {
  const quiet = EVENTS.filter((event) => ["spawned", "reported", "task_status", "task_added"].includes(event.kind));
  assert.deepEqual(newsItems(quiet, STATE), []);
});

test("a failed pass is a toast, filed under the pass's own time", () => {
  const item = passItem(STATE);
  assert.equal(item.id, `tick:${STATE.last_tick.at_ms}`);
  assert.equal(item.level, "error");
  assert.equal(item.title, "The last pass failed");
  assert.match(item.body, /^land on T2: could not run git/);
  assert.match(item.body, /\(\+1\)/);
  assert.equal(passItem({ ...STATE, last_tick: { at_ms: 1, steps: 1, failed: [] } }), null);
  assert.equal(passItem({ seq: 0, tasks: [] }), null);
});

test("one reading of the service is the news plus the pass", () => {
  const items = fromSnapshot(EVENTS, STATE);
  assert.equal(items.length, 5);
  assert.equal(items[items.length - 1].title, "The last pass failed");
});

test("an event told twice is one toast", () => {
  const at = 1_000;
  const first = mergeNews([], newsItems(EVENTS, STATE), at);
  const again = mergeNews(first, newsItems(EVENTS, STATE), at + 500);
  assert.deepEqual(
    again.map((item) => item.id),
    first.map((item) => item.id),
    "a replayed batch adds nothing",
  );
});

test("the list keeps the newest few, and forgets what is old", () => {
  const at = 1_000_000;
  let list = [];
  for (let n = 0; n < 12; n += 1) {
    list = mergeNews(list, [{ id: `ev:${n}`, level: "info", title: `t${n}`, body: "" }], at + n);
  }
  assert.equal(list.length, KEEP + 1);
  assert.equal(list[list.length - 1].id, "ev:11");
  // Everything older than KEEP_MS is gone on the next merge.
  const later = mergeNews(list, [], at + 11 + KEEP_MS + 1);
  assert.deepEqual(later, []);
});

test("three toasts show at once, and an expired one goes by itself", () => {
  const at = 5_000_000;
  const list = [0, 1, 2, 3, 4].map((n) => ({
    id: `ev:${n}`,
    level: "info",
    title: `t${n}`,
    body: "",
    at: at - (5 - n) * 100,
  }));
  const shown = visible(list, at);
  assert.deepEqual(
    shown.map((item) => item.id),
    ["ev:2", "ev:3", "ev:4"],
    "the newest three, oldest first",
  );
  assert.equal(visible(list, at).length, SHOWN);
  // Everything but the newest has timed out: the list is empty.
  assert.deepEqual(visible(list, at + TIMEOUT_DEFAULT), []);
  // An error stays twice as long as an info toast.
  const error = [{ id: "ev:9", level: "error", title: "x", body: "", at }];
  assert.equal(visible(error, at + 11_000).length, 1);
  assert.equal(visible(error, at + 13_000).length, 0);
});

test("a dismissed toast never comes back", () => {
  const at = 7_000_000;
  const list = [
    { id: "ev:1", level: "info", title: "one", body: "", at },
    { id: "ev:2", level: "info", title: "two", body: "", at },
  ];
  const gone = new Set(["ev:1"]);
  assert.deepEqual(
    visible(list, at, gone).map((item) => item.id),
    ["ev:2"],
  );
  assert.deepEqual(
    dismiss(list, "ev:2").map((item) => item.id),
    ["ev:1"],
  );
  assert.equal(timeoutOf({ level: "alert" }), 20_000);
  assert.equal(timeoutOf({ level: "warning" }), 10_000);
  assert.equal(timeoutOf({ level: "success" }), TIMEOUT_DEFAULT);
  assert.equal(timeoutOf({ level: "info", timeout: 1234 }), 1234);
});

test("a level's colour is the one the tokens give it", () => {
  assert.equal(levelTone("error"), "red");
  assert.equal(levelTone("alert"), "red");
  assert.equal(levelTone("warning"), "amber");
  assert.equal(levelTone("success"), "green");
  assert.equal(levelTone("info"), "accent");
});

test("a body is cut at three lines' worth of characters", () => {
  const long = "x".repeat(BODY_MAX + 40);
  const cut = clip(long);
  assert.equal(cut.length, BODY_MAX);
  assert.ok(cut.endsWith("…"));
  assert.equal(clip("  short  "), "short");
  assert.equal(clip(null), "");
});

// -- the status bar's reading of the board ---------------------------------

test("every word of the board is counted, in the panel's order", () => {
  assert.deepEqual(
    statusCounts(STATE).map((row) => [row.status, row.count]),
    [
      ["needs_you", 1],
      ["ready", 1],
      ["working", 1],
      ["queued", 1],
      ["to_do", 1],
      ["done", 1],
    ],
  );
  assert.equal(statusCounts({ tasks: [] }).length, 0);
  assert.equal(statusCounts(null).length, 0);
  // The four numbers a badge shows still come from `counts`.
  assert.deepEqual(counts(STATE), {
    needs: 1,
    ready: 1,
    working: 1,
    done: 1,
    total: 6,
    open_asks: 0,
  });
});

test("the bar names the run, its failure and the service's absence", () => {
  assert.equal(runState(STATE), "running");
  assert.equal(runState({ ...STATE, finished: true }), "finished");
  assert.equal(runState({ seq: 0, run: null }), "none");
  assert.equal(runLabel(STATE), "run m10a · integration/m10");
  assert.equal(runShort(STATE), "run m10a", "the bar names the run; the sidebar names the branch");
  assert.equal(runLabel({ seq: 0, run: null, hint: "no run yet" }), "no run yet");
  assert.match(lastFailure(STATE), /^land on T2: could not run git/);
  assert.equal(lastFailure({ seq: 1, run: null }), null);
});

test("a time in the bar is words, not a date", () => {
  const now = 1_800_000_000_000;
  assert.equal(since(now - 1_000, now), "just now");
  assert.equal(since(now - 4 * 60_000, now), "4m ago");
  assert.equal(since(now - 3 * 3_600_000, now), "3h ago");
  assert.equal(since(now - 2 * 86_400_000, now), "2d ago");
  assert.equal(since(undefined, now), "just now");
});
