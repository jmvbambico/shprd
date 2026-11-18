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
  MENU,
  NARROW_PX,
  attention,
  closeWords,
  coordinatorRow,
  menuHeight,
  menuItems,
  narrowSidebar,
  needWords,
  orderedTasks,
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
  rememberPanel,
  shownPanel,
  tabLabel,
  tabProgram,
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
  DONE_SHOWN,
  counts,
  etaMinutes,
  filterChips,
  found,
  lastFailure,
  listView,
  newTask,
  rowActions,
  runLabel,
  runShort,
  runState,
  since,
  statusCounts,
  taskById,
} from "../mods/lib/board.js";
import {
  FILES_SHOWN,
  SUMMARY_MAX,
  WORDS,
  card as acceptCard,
  changeRows,
  changeCounts,
  changesLine,
  commitRows,
  canReview,
  mustLeavePr,
  reportLine,
  threadViews,
  checks as handInChecks,
  findings as reviewFindings,
  head as acceptHead,
  reviewLine,
  verbs,
  whereNow,
  words as summaryWords,
} from "../mods/lib/accept.js";
import { PR_WORDS, branchLine, openUrl, prSize, taskPr } from "../mods/lib/taskpr.js";
import { openAccept } from "../mods/lib/panel.js";
import * as herdr from "herdr";

/** One fixture, parsed. */
function fixture(name) {
  return JSON.parse(readFileSync(fileURLToPath(new URL(`./fixtures/${name}`, import.meta.url)), "utf8"));
}

/** The snapshot a real `GET /api/state` answered. */
const STATE = fixture("state.json");
/** The other real snapshot: the M10.5 board, with a hand-in, a review and PRs. */
const BOARD = fixture("board.json");
/** What `GET /api/tasks/T2/changes` answered for that board's T2. */
const CHANGES = fixture("task-changes.json");
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

test("a row's badge counts what needs the person and what is ready", () => {
  const rows = sidebarRows(STATE, SPACES);
  assert.deepEqual(attention(rows.coordinator), { need: 1, ready: 1 }, "the run's own counts");
  assert.deepEqual(attention(rows.tasks.find((row) => row.id === "T3")), { need: 1, ready: 0 });
  assert.deepEqual(attention(rows.tasks.find((row) => row.id === "T2")), { need: 0, ready: 1 });
  assert.deepEqual(attention(rows.tasks.find((row) => row.id === "T1")), { need: 0, ready: 0 });
  assert.deepEqual(attention(null), { need: 0, ready: 0 });
});

test("the rail is read from the sidebar's width, as Shepherdr reads it", () => {
  const tree = (px) => ({ id: "root", children: [{ id: "sidebar", size: { px }, children: [] }] });
  assert.equal(narrowSidebar(tree(NARROW_PX)), false, "160 is still wide");
  assert.equal(narrowSidebar(tree(NARROW_PX - 1)), true);
  assert.equal(narrowSidebar(tree(391)), false);
  assert.equal(narrowSidebar({ id: "root", children: [] }), false, "no sidebar node: the wide form");
  assert.equal(narrowSidebar(null), false);
});

test("a space's row menu is Open task then Close…, and only what a space can do", () => {
  const rows = sidebarRows(STATE, SPACES);
  const withSpace = menuItems(rows.tasks.find((row) => row.id === "T2"));
  assert.deepEqual(withSpace, [{ id: "open", label: "Open task" }, { sep: true }, { id: "close", label: "Close…" }]);
  // The popup has only the props: the space is its id there, and the words are the same.
  assert.deepEqual(menuItems({ task: "T2", space: "s4" }), withSpace);
  // A task with no space has nothing to close, so it has no Close… row.
  assert.deepEqual(menuItems(rows.waiting.find((row) => row.id === "T6")), [{ id: "open", label: "Open task" }]);
  // Shepherdr's `heightOf`: 5 px of padding, 32 px a row, 9 px a separator.
  assert.equal(menuHeight(withSpace), MENU.pad * 2 + MENU.row + MENU.sep + MENU.row);
  assert.equal(menuHeight(menuItems({ task: "T6" })), MENU.pad * 2 + MENU.row);
});

test("closing a space asks in Shepherdr's words", () => {
  assert.deepEqual(closeWords({ task: "T2" }), {
    title: "Close “T2”?",
    detail: "Its tabs close and every program in them ends.",
    verb: "Close space",
  });
  assert.equal(closeWords({}).title, "Close “this space”?");
});

test("task looks are the tokens' words", () => {
  assert.deepEqual(taskLook(task("T1")), {
    status: "working",
    word: "Working",
    tone: "amber",
    glyph: "pie",
    pct: 62,
  });
  assert.deepEqual(taskLook({}), { status: "to_do", word: "To do", tone: "dim", glyph: "ring", pct: null });
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
  // The two titles shprd writes are the only roles there are: a third segment it
  // does not write is read as the worker's, not invented as a role of its own.
  assert.equal(parseTabName("pi·T3·backup").role, "worker");
  assert.equal(parseTabName("claude·T3·extra").role, "worker");
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

test("a pill is called by its name, or by what runs in it when it has none", () => {
  // shprd's own title is the pill's name.
  assert.equal(tabLabel({ id: "a", name: "pi·T1" }, parseTabName("pi·T1"), { name: "pi" }), "pi·T1");
  // A name a person gave it is kept, even though shprd did not give it.
  assert.equal(tabLabel({ id: "a", name: "my shell" }, parseTabName("my shell"), { name: "zsh" }), "my shell");
  // Herdr's own names — a position, or "tb3" on older builds — are not names.
  assert.equal(tabLabel({ id: "a", name: "1" }, parseTabName("1"), { name: "zsh" }), "zsh");
  assert.equal(
    tabLabel({ id: "a", name: "tb3" }, parseTabName("tb3"), { name: "Claude Code" }),
    "Claude Code",
  );
  // Nothing to call it by is still a word, never an empty pill.
  assert.equal(tabLabel({ id: "a", name: "1" }, parseTabName("1"), null), "Terminal");
});

test("a pill wears the program in its tab and counts what it needs", () => {
  const panes = [
    { tab: "a", program: { name: "zsh", icon: "icons/zsh.svg", tint: true } },
    {
      tab: "a",
      program: { name: "Claude Code", icon: "icons/none.svg", tint: false, agent: true },
    },
    { tab: "b", program: { name: "zsh", icon: "icons/zsh.svg", tint: true } },
  ];
  // An agent's own glyph wins over the shell a tab's first pane runs.
  assert.equal(tabProgram(panes, "a").name, "Claude Code");
  assert.equal(tabProgram(panes, "b").name, "zsh");
  assert.equal(tabProgram(panes, "c"), null, "a tab with no pane claims no program");

  const space = {
    id: "s",
    name: "shprd · shop",
    tabs: [
      { id: "a", name: "claude·T2", active: true, panes: 1 },
      { id: "b", name: "zsh", panes: 1 },
    ],
  };
  const rows = tabRows(space, panes, STATE);
  assert.deepEqual(
    rows.map((row) => [row.icon, row.tint, row.program, row.named]),
    [
      ["icons/none.svg", false, "Claude Code", false],
      ["icons/zsh.svg", true, "zsh", false],
    ],
  );
  // The badge's two numbers: what needs the person, then what is ready.
  assert.deepEqual(
    rows.map((row) => [row.need, row.ready]),
    [
      [0, 1],
      [0, 0],
    ],
    "T2 is ready, and a shell says nothing about a task",
  );
  const blocked = tabRows(space, [{ tab: "a", status: "blocked" }], STATE);
  assert.deepEqual([blocked[0].need, blocked[0].ready], [1, 0]);
});

test("the side panel's buttons carry the counts of the stacks they open", () => {
  const buttons = panelButtons(STATE, "files");
  assert.deepEqual(
    buttons.map((button) => button.id),
    ["files", "changes", "tasks"],
    "the panel's stacks only: settings is a dialog, not a tab of the panel",
  );
  const tasks = buttons.find((button) => button.id === "tasks");
  assert.deepEqual({ needs: tasks.needs, ready: tasks.ready, active: tasks.active }, {
    needs: 1,
    ready: 1,
    active: false,
  });
  assert.equal(buttons.find((button) => button.id === "files").active, true);
  assert.equal(buttons.some((button) => button.id === "asks"), false, "the cards live in the task thread");
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

// -- the tasks panel's reading of a row's act ------------------------------

/** One task of the M10.5 board, by id. */
function boardTask(id) {
  return BOARD.tasks.find((entry) => entry.id === id);
}

test("a row offers the one verb the engine has for its state", () => {
  // T2 is passed, so its act opens the Accept card rather than posting a verb.
  assert.deepEqual(rowActions(boardTask("T2")), [{ id: "review", label: "Review", action: null }]);
  // T3 failed its review: the send-back is a spawn, which is what a retry is.
  assert.deepEqual(rowActions(boardTask("T3")), [{ id: "retry", label: "Retry", action: "task.retry" }]);
  // T5 is working, so the one act is a stop.
  assert.deepEqual(rowActions(boardTask("T5")), [{ id: "stop", label: "Stop", action: "task.stop" }]);
  // A done task is history, and a queued one waits for its deps: neither has an
  // act, so a row never offers a press that would be refused.
  assert.deepEqual(rowActions(boardTask("T1")), []);
  assert.deepEqual(rowActions(boardTask("T4")), []);
  assert.deepEqual(rowActions(boardTask("T6")), []);
  assert.deepEqual(rowActions(null), []);
});

test("a task that may start is the one the rules would accept a start for", () => {
  const startable = { ...boardTask("T6"), status: "queued", derived: { status: "to_do" } };
  assert.deepEqual(rowActions(startable), [{ id: "start", label: "Start", action: "task.start" }]);
  // Scheduled starts by itself at a time; starting it now is the same verb.
  const scheduled = { ...startable, derived: { status: "scheduled" } };
  assert.equal(rowActions(scheduled)[0].action, "task.start");
  // A dropped task is not history to act on.
  assert.deepEqual(rowActions({ ...startable, status: "dropped" }), []);
});

test("the add-task form says what is missing before the press", () => {
  assert.deepEqual(newTask({ title: " Tabs ", scope: "a, b", acceptance: "it works; it stays" }), {
    title: "Tabs",
    scope: ["a", "b"],
    acceptance: ["it works", "it stays"],
    why: "",
    ready: true,
  });
  assert.equal(newTask({}).ready, false);
  assert.match(newTask({}).why, /title/);
  assert.match(newTask({ title: "x" }).why, /folder or path/);
  assert.match(newTask({ title: "x", scope: "a" }).why, /must be true/);
});

test("a panel finds the task its shared selection names, or nothing", () => {
  assert.equal(taskById(BOARD, "T2").title, "Tasks panel");
  assert.equal(taskById(BOARD, "nope"), null);
  assert.equal(taskById(BOARD, null), null);
  assert.equal(taskById(null, "T2"), null);
});

// -- the accept card's reading of a hand-in --------------------------------

test("the card shows the task the panel picked", () => {
  assert.equal(acceptCard(BOARD, "T3").title, "Accept card");
  assert.equal(acceptCard(BOARD, "T9"), null);
  const head = acceptHead(boardTask("T2"));
  assert.equal(head.id, "T2");
  assert.equal(head.word, "needs you");
  assert.equal(head.tone, "red", "the engine wants the person, so the header says so");
  assert.match(head.question, /^Accept T2\?$/);
});

test("a long summary keeps its first sentence and folds the rest away", () => {
  const short = summaryWords(boardTask("T1"));
  assert.equal(short.details, "", "a summary that fits is not folded");
  assert.match(short.summary, /^the sidebar lists every task/);
  // T2's summary fits as well, so nothing is folded.
  assert.equal(summaryWords(boardTask("T2")).details, "");
  // Over the limit, the top of the card is the first sentence and the whole text
  // is under Details (Shepherdr's rule, `core/accept.js`'s `words()`).
  const paragraph = `${boardTask("T2").attempts[0].report.summary}. It also pages the list and keeps the row's act at the end, then draws the counts over it and folds the rest away.`;
  assert.ok(paragraph.length > SUMMARY_MAX);
  const long = summaryWords({ attempts: [{ report: { summary: `  ${paragraph}\n` } }] });
  assert.ok(long.summary.length <= SUMMARY_MAX + 1, long.summary);
  assert.ok(long.summary.endsWith("…") || long.summary.endsWith("."), long.summary);
  assert.ok(long.details.length > long.summary.length);
  // A summary with no sentence end is cut at a word, not mid-word.
  const cutAtWord = summaryWords({ attempts: [{ report: { summary: "word ".repeat(60) } }] });
  assert.ok(cutAtWord.summary.endsWith("…"), cutAtWord.summary);
  assert.ok(!cutAtWord.summary.includes("  "), cutAtWord.summary);
  // Nothing said at all is said quietly.
  assert.deepEqual(summaryWords(boardTask("T6")), { summary: "", details: "" });
  assert.equal(summaryWords(null).summary, "");
});

test("the changes are counted once, from the branch git read", () => {
  // The service names the commits too: "N commits · N files changed +a −r".
  assert.equal(changesLine(CHANGES), "7 commits · 3 files changed +6 −0");
  assert.deepEqual(changeCounts(CHANGES), { commits: "7 commits", files: "3 files changed", added: "+6", removed: "−0" });
  // An older service has no commit count, and the line is the files alone.
  assert.equal(changesLine({ files: [{ path: "a", added: 1, deleted: 2 }], added: 1, deleted: 2 }), "1 file changed +1 −2");
  assert.equal(changesLine({ commit_count: 1, files: [{ path: "a", added: 1, deleted: 2 }], added: 1, deleted: 2 }), "1 commit · 1 file changed +1 −2");
  assert.equal(changesLine({ files: [] }), WORDS.noChanges);
  assert.equal(changesLine(null), WORDS.noChanges);
});

test("the newest commits are listed and the rest are counted", () => {
  const { rows, rest } = commitRows(CHANGES);
  assert.equal(rows.length, 5);
  assert.deepEqual(rows[0], { sha: "a1b2c3d", subject: "feat(tasks): page the list" });
  assert.equal(rest, 2, "seven commits, five listed: And 2 more");
  assert.equal(WORDS.moreLine(rest), "And 2 more");
  assert.deepEqual(commitRows(null), { rows: [], rest: 0 });
});

test("the thread's switch is Chat, Terminal and, once it has one, Pull request", () => {
  const T1 = boardTask("T1");
  const without = { ...boardTask("T2"), pr: undefined };
  const words_ = (task, opts) => threadViews(task, opts).map((seg) => seg.word);
  assert.deepEqual(words_(without, { live: true }), ["Chat", "Terminal"], "no Pull request before it has one");
  assert.deepEqual(words_(T1, { live: true }), ["Chat", "Terminal", "Pull request"]);
  // Chat is always on and is what a thread opens on; Terminal is dim when no agent is up.
  const open = threadViews(T1, { live: false });
  assert.equal(open[0].on, true, "a thread opens on its chat");
  const segs = threadViews(T1, { live: false, shown: "tasks-thread-term" });
  assert.equal(segs[0].dead, false);
  assert.equal(segs[0].child, "tasks-thread-chat");
  assert.equal(segs[0].on, false);
  assert.equal(segs[1].dead, true, "Not running");
  assert.equal(segs[1].on, true);
  assert.equal(threadViews(T1, { live: true, shown: "tasks-thread-pr" }).find((seg) => seg.on).key, "pr");
  // The pull request of a task that lost it falls back to the terminal.
  assert.equal(mustLeavePr(without, "tasks-thread-pr"), true);
  assert.equal(mustLeavePr(T1, "tasks-thread-pr"), false);
  assert.equal(mustLeavePr(without, "tasks-thread-term"), false);
  assert.equal(mustLeavePr(without, "tasks-thread-chat"), false);
});

test("the head's Review is the Ready row's Review, and its status line is the agent's", () => {
  for (const id of ["T1", "T2", "T3", "T4", "T5", "T6"]) {
    const task = boardTask(id);
    assert.equal(canReview(task), rowActions(task).some((act) => act.id === "review"), id);
  }
  assert.equal(reportLine({ last_status: { message: " Fixing the tests ", progress: 40 } }), "Fixing the tests · 40%");
  assert.equal(reportLine({ last_status: { message: "Done" } }), "Done");
  assert.equal(reportLine({}), null);
});

test("the changed files are the biggest first, and a binary file says so", () => {
  const { rows, rest } = changeRows(CHANGES);
  assert.deepEqual(
    rows.map((row) => [row.path, row.stat]),
    [
      ["host/herdr-desktop/rows.js", "+4 −0"],
      ["host/herdr-desktop/board.js", "+1 −0"],
      ["src/tasks.rs", "+1 −0"],
    ],
  );
  assert.equal(rest, 0);
  const many = {
    files: Array.from({ length: FILES_SHOWN + 3 }, (_, n) => ({ path: `f${n}.rs`, added: n, deleted: 0 })),
  };
  const folded = changeRows(many);
  assert.equal(folded.rows.length, FILES_SHOWN);
  assert.equal(folded.rest, 3, "the card says how many it folded, once");
  const binary = changeRows({ files: [{ path: "logo.png", added: 0, deleted: 0, binary: true }] });
  assert.equal(binary.rows[0].stat, "binary", "a binary file is not +0 −0");
});

test("the checks are the ones the hand-in recorded, with their exit codes", () => {
  const { rows, alsoRan } = handInChecks(boardTask("T2"));
  assert.deepEqual(
    rows.map((row) => [row.name, row.command, row.state]),
    [
      ["fmt", "cargo fmt --all --check", "passed"],
      ["lint", "cargo clippy --workspace --all-targets -- -D warnings", "passed"],
      ["tests", "cargo test --workspace", "passed"],
    ],
  );
  // The detail is the last line it printed, cut: a whole tail would push the
  // review off the card.
  assert.equal(rows[2].detail, "test result: ok. 1652 passed; 0 failed");
  assert.deepEqual(alsoRan, ["cargo test --workspace: 1652 passed"]);
  // An attempt with no report at all says nothing rather than guessing.
  assert.deepEqual(handInChecks(boardTask("T6")), { rows: [], alsoRan: [] });
  // A failing exit code is a failed check, and a timeout is neither.
  const failed = {
    attempts: [{ report: { checks: [{ gate: "lint", command: "cargo clippy", exit_code: 101, output_tail: "error: boom" }] } }],
  };
  assert.equal(handInChecks(failed).rows[0].state, "failed");
  const timedOut = {
    attempts: [{ report: { checks: [{ gate: "tests", command: "cargo test", output_tail: "timed out" }] } }],
  };
  assert.equal(handInChecks(timedOut).rows[0].state, "unknown");
});

test("the reviewer's findings are worst first, each with its wave", () => {
  const found = reviewFindings(boardTask("T3"));
  assert.equal(found.length, 2);
  assert.deepEqual(
    found.map((finding) => [finding.severity, finding.word, finding.tone]),
    [
      ["major", "Major", "red"],
      ["minor", "Minor", "amber"],
    ],
  );
  assert.equal(found[0].reviewer, "codex", "a review's finding carries the reviewer");
  assert.match(found[0].location, /^host\/herdr-desktop\/mods\/shprd-accept\/main\.js:/);
  assert.match(found[0].detail, /tasks\.merge/);
  // A gate of the engine's own is a finding too, but it has no reviewer.
  const own = {
    attempts: [{ reviews: [], gates: [{ gate: "integrate", passed: false, findings: [{ severity: "trivial", title: "t", detail: "" }] }] }],
  };
  assert.equal(reviewFindings(own)[0].reviewer, null);
  // A task with no finding is an empty list, not a made-up one.
  assert.deepEqual(reviewFindings(boardTask("T2")), []);
});

test("the review line names the reviewer and whether it was independent", () => {
  assert.equal(reviewLine(boardTask("T2")).line, "codex reviewed it: passed · another vendor");
  assert.equal(reviewLine(boardTask("T3")).line, "codex reviewed it: failed · another vendor");
  const degraded = {
    attempts: [{ reviews: [{ reviewer: "claude", degraded: true }], gates: [{ gate: "review", passed: true, findings: [], proven: true }] }],
  };
  const said = reviewLine(degraded);
  assert.match(said.line, /same vendor, not independent/);
  assert.equal(said.degraded, true);
  const none = { attempts: [{ reviews: [], gates: [] }] };
  assert.deepEqual(reviewLine(none), { line: WORDS.noReview, degraded: false });
});

test("the card offers Accept only where a merge policy asks", () => {
  const ready = boardTask("T2");
  assert.equal(verbs(ready, "ask").accept, true);
  for (const policy of ["checks", "auto"]) {
    const offered = verbs(ready, policy);
    assert.equal(offered.accept, false, policy);
    assert.match(offered.note, new RegExp(`tasks\.merge is ${policy}`));
  }
  // A service that could not say leaves the decision to the engine.
  assert.equal(verbs(ready, null).accept, true);
  // A hand-in nobody reviewed yet is not a decision for the person: the card
  // offers no Accept until a review has passed it.
  const reported = { id: "T9", status: "reported", derived: { status: "working" }, attempts: [] };
  assert.equal(verbs(reported, "ask").accept, false);
  assert.equal(verbs(boardTask("T2"), "ask").accept, true);
  // A failed task is sent back, never accepted.
  const failed = verbs(boardTask("T3"), "ask");
  assert.deepEqual({ accept: failed.accept, sendBack: failed.sendBack, drop: failed.drop }, {
    accept: false,
    sendBack: true,
    drop: true,
  });
  // Work in progress can be dropped, and nothing else: a send-back is refused
  // while an attempt is unfinished, and an accept is not a decision yet.
  const working = verbs(boardTask("T5"), "ask");
  assert.deepEqual({ accept: working.accept, sendBack: working.sendBack, drop: working.drop }, {
    accept: false,
    sendBack: false,
    drop: true,
  });
  // A landed or dropped task is not a decision any more.
  for (const id of ["T1", "T4"]) {
    assert.deepEqual(verbs(boardTask(id), "ask"), { accept: false, sendBack: false, drop: false, note: null });
  }
  assert.equal(whereNow(boardTask("T1")), "Landed: its work is in the base branch.");
  assert.equal(whereNow(boardTask("T4")), "Landed: its work is in the base branch.");
  assert.equal(whereNow(null), "This task is no longer on the board.");
});

test("Review opens the Accept card as a 640 by 640 modal dialog for that task", () => {
  const before = herdr.OVERLAYS.length;
  const id = openAccept({ task: "T2", title: "Paging" });
  assert.equal(herdr.OVERLAYS.length, before + 1);
  const spec = herdr.OVERLAYS[herdr.OVERLAYS.length - 1];
  assert.equal(spec.id, id);
  assert.equal(spec.mod, "shprd-accept");
  assert.equal(spec.kind, "dialog");
  assert.equal(spec.modal, true, "it blocks the window under it, as Shepherdr's does");
  assert.equal(spec.width, 640);
  assert.equal(spec.height, 640);
  assert.equal(spec.props.task, "T2", "the dialog is about the task it was opened for");
  assert.equal(spec.props.overlay, id, "and knows its own id, to close itself on Esc");
});

// -- the task PR's reading of a landing ------------------------------------

test("a task that landed through a forge shows its number, state and link", () => {
  const merged = taskPr(boardTask("T1"));
  assert.equal(merged.kind, "pr");
  assert.equal(merged.number, "#58");
  assert.equal(merged.state, PR_WORDS.merged);
  assert.equal(merged.tone, "green");
  assert.equal(openUrl(merged), "https://github.com/jmvbambico/shprd/pull/58");
  const open = taskPr(boardTask("T2"));
  assert.equal(open.state, PR_WORDS.open);
  assert.equal(open.tone, "amber");
  assert.equal(open.approved, false);
  assert.equal(open.line, PR_WORDS.notApproved);
});

test("a local landing and a task that never landed say so, and open nothing", () => {
  const local = taskPr(boardTask("T4"));
  assert.equal(local.kind, "local", "it merged into its base, so there is no pull request");
  assert.equal(local.number, null);
  assert.equal(openUrl(local), null);
  assert.match(local.line, /^Landed locally/);
  const none = taskPr(boardTask("T5"));
  assert.equal(none.kind, "none");
  assert.equal(none.line, PR_WORDS.none);
  assert.equal(openUrl(none), null);
  // A URL that is not one is not opened: an empty call is a window asked for
  // nothing.
  assert.equal(openUrl({ kind: "pr", url: "" }), null);
  assert.equal(openUrl({ kind: "pr", url: "/pull/1" }), null);
});

test("the pull request bar says how big the change is and which branches it joins", () => {
  assert.deepEqual(prSize(CHANGES), { files: "3 files", added: "+6", removed: "−0" });
  assert.deepEqual(prSize({ files: [{ path: "a" }], added: 2, deleted: 1 }), { files: "1 file", added: "+2", removed: "−1" });
  assert.equal(prSize(null), null, "before the changes were read there is no size to draw");
  assert.equal(branchLine({ head: "shprd/m10b/T2" }, CHANGES), "shprd/m10b/T2 → main");
  assert.equal(branchLine({ head: "shprd/m10b/T2" }, null), "shprd/m10b/T2", "no base known, only the head");
  assert.equal(PR_WORDS.openOnGitHub, "Open on GitHub");
  assert.equal(PR_WORDS.copyLink, "Copy link");
  assert.equal(PR_WORDS.copied, "Copied");
});

// -- the tasks panel's search, filter row and groups (M11 U2) ---------------

/** One task of the service fixture, by id. */
function stateTask(id) {
  return STATE.tasks.find((entry) => entry.id === id);
}

/** A board of `n` done tasks beside the fixture's other words. */
function withDone(n) {
  const extra = Array.from({ length: n }, (_, i) => ({
    id: `D${i}`,
    title: `finished ${i}`,
    status: "done",
    derived: { status: "done" },
  }));
  return { ...STATE, tasks: [...STATE.tasks.filter((t) => t.derived.status !== "done"), ...extra] };
}

test("the search finds a task by id, title, status word or PR number, and by nothing else", () => {
  const t2 = stateTask("T2");
  assert.equal(found(t2, "t2"), true);
  assert.equal(found(t2, t2.title.split(" ")[0]), true);
  assert.equal(found(t2, "ready"), true);
  assert.equal(found({ ...t2, pr: { number: 42 } }, "#42"), true);
  assert.equal(found(t2, "zzz-not-there"), false);
  assert.equal(found(t2, ""), true);
});

test("typing an id leaves only that task's row", () => {
  const view = listView(STATE, { query: "T3" });
  const ids = view.groups.flatMap((g) => g.tasks.map((t) => t.id));
  assert.deepEqual(ids, ["T3"]);
});

test("For you comes first: needs you, then Ready; Done is a line until opened", () => {
  const view = listView(STATE, {});
  assert.equal(view.groups[0].label, "For you");
  assert.deepEqual(
    view.groups[0].tasks.map((t) => t.derived.status),
    ["needs_you", "ready"],
  );
  assert.equal(view.groups.some((g) => g.key === "done"), false);
  assert.match(view.doneLine.label, /^\d+ done$/);
  const opened = listView(STATE, { doneOpen: true });
  assert.equal(opened.doneLine, null);
  assert.equal(opened.groups.at(-1).key, "done");
});

test("an opened Done shows five rows and 'Show N more', whole once asked", () => {
  const board = withDone(8);
  const cut = listView(board, { doneOpen: true }).groups.find((g) => g.key === "done");
  assert.equal(cut.tasks.length, DONE_SHOWN);
  assert.equal(cut.cut, 3);
  assert.equal(cut.count, 8);
  const all = listView(board, { doneOpen: true, allDone: true }).groups.find((g) => g.key === "done");
  assert.equal(all.tasks.length, 8);
  assert.equal(all.more, true);
});

test("a folded heading is reported shut, except while searching", () => {
  const shut = new Set(["working"]);
  assert.equal(listView(STATE, { shut }).groups.find((g) => g.key === "working").shut, true);
  assert.equal(listView(STATE, { shut, query: "T1" }).groups.find((g) => g.key === "working").shut, false);
});

test("a search finds Done tasks too, all shown", () => {
  const view = listView(withDone(8), { query: "finished" });
  assert.equal(view.doneLine, null);
  assert.equal(view.groups.find((g) => g.key === "done").tasks.length, 8);
});

test("a status filter is that word's tasks, flat; the chips count what is there", () => {
  const view = listView(STATE, { filter: "ready" });
  assert.equal(view.groups.length, 1);
  assert.equal(view.groups[0].flat, true);
  assert.deepEqual(view.groups[0].tasks.map((t) => t.id), ["T2"]);
  const chips = filterChips(STATE);
  assert.deepEqual(chips.primary.map((c) => c.key), ["needs_you", "working", "ready"]);
  assert.equal(chips.more.some((c) => c.key === "done"), true);
  assert.equal(chips.primary.find((c) => c.key === "ready").count, 1);
});

test("minutes left show only when the service reports them", () => {
  assert.equal(etaMinutes(stateTask("T5")), null);
  assert.equal(etaMinutes({ eta_minutes: 12.2 }), 13);
  assert.equal(etaMinutes({ derived: { eta_minutes: 4 } }), 4);
  assert.equal(etaMinutes({ eta_minutes: 0 }), null);
});

/** A layout as `layout()` answers it, the panel and its stack as given. */
const layoutWith = (hidden, active) => ({
  id: "root",
  children: [
    { id: "panel", hidden, children: [{ id: "panel-body", type: "stack", active }] },
  ],
});

test("the panel button that is lit is the one the layout shows, not one the bar remembers", () => {
  assert.equal(shownPanel(layoutWith(false, "changes")), "changes");
  assert.equal(shownPanel(layoutWith(false, "tasks")), "tasks");
  // A hidden panel lights nothing, and so does a stack that has no button.
  assert.equal(shownPanel(layoutWith(true, "tasks")), null);
  assert.equal(shownPanel(layoutWith(false, "settings")), null);
  assert.equal(shownPanel(null), null);
  const lit = panelButtons(STATE, shownPanel(layoutWith(false, "changes")))
    .filter((button) => button.active)
    .map((button) => button.id);
  assert.deepEqual(lit, ["changes"]);
  assert.deepEqual(panelButtons(STATE, null).filter((button) => button.active), []);
});

test("each space gets its own panel back when it is shown again", () => {
  const memory = new Map();
  let last = null;
  const look = (space, current) => {
    const seen = rememberPanel(memory, last, space, current);
    last = seen.last;
    return seen.restore;
  };
  // Space a: Changes open. Recorded, nothing to restore.
  assert.equal(look("a", "changes"), undefined);
  assert.equal(look("a", "changes"), undefined);
  // Switch to space b, not seen before: the panel is left as it is.
  assert.equal(look("b", "changes"), undefined);
  // In b the person hides the panel.
  assert.equal(look("b", null), undefined);
  // Back to a: its Changes panel returns, even though the layout shows b's hidden one.
  assert.equal(look("a", null), "changes");
  // Then b again: hidden is what it had.
  assert.equal(look("b", "changes"), null);
  // No space at all changes nothing.
  assert.equal(rememberPanel(memory, "b", null, "tasks").restore, undefined);
});
