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
  foreignTab,
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
  staleIds,
  STALE_MS,
  newTask,
  progressOf,
  rowActions,
  runLabel,
  runShort,
  runState,
  secondLine,
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
  protectedLine,
  commitRows,
  canReview,
  VIEW,
  hasArtifacts,
  mustLeavePr,
  reportLine,
  terminalChild,
  terminalReachable,
  threadAgent,
  threadViews,
  checks as handInChecks,
  proofs as handInProofs,
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
  // The newest decision card speaks for the task (UXC-7).
  assert.match(t3.sub, /sending attempt 1 back/);
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
  // The window never repeats an old service's hint: nobody types a command here.
  assert.equal(rows.coordinator.word, "No run");
  assert.equal(rows.coordinator.tone, "dim");
  assert.equal(rows.coordinator.label, "This repository");
  assert.equal(rows.coordinator.sub, "no tasks yet: say what you want in the chat");
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
  assert.match(needWords(task("T3"), STATE), /sending attempt 1 back/);
  const withNotice = { tasks: STATE.tasks, needs: [...STATE.needs, { task: "T3", body: "FYI only", seq: 99, kind: "notice", actions: ["dismiss"] }] };
  assert.match(needWords(task("T3"), withNotice), /sending attempt 1 back/, "a notice never speaks for the task");
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
  assert.match(needs.body, /sending attempt 1 back/);
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

test("a land failure is one plain sentence in the toast and the status bar", () => {
  const plain = "Couldn't merge T1: this repository has no GitHub remote";
  const state = {
    ...STATE,
    last_tick: { at_ms: 9, steps: 1, failed: [{ step: "land", task: "T1", error: plain }] },
  };
  assert.equal(passItem(state).body, plain);
  assert.equal(lastFailure(state), plain);
  for (const text of [passItem(state).body, lastFailure(state)]) {
    assert.doesNotMatch(text, /`|gh |forge|run /);
  }
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
  assert.equal(runShort(STATE), "run m10a · 1 working · 1 ready", "the bar names the run and where its tasks stand");
  assert.equal(runLabel({ seq: 0, run: null, hint: "no run yet" }), "no tasks yet: say what you want in the chat");
  assert.equal(runShort({ seq: 0, run: null, hint: "no run yet: start one with `shprd run start`" }), "no tasks yet: say what you want in the chat");
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

test("a stuck task can be dropped from its row, and a dropped one is not listed", () => {
  const stopped = { id: "S", status: "working", derived: { status: "needs_you", need: "stopped" } };
  assert.deepEqual(rowActions(stopped), [{ id: "drop", label: "Drop", action: null }]);
  const handedIn = { id: "P", status: "reported", derived: { status: "needs_you", need: "asks" } };
  assert.deepEqual(
    rowActions(handedIn).map((act) => act.id),
    ["review", "drop"],
    "a handed-in task that needs you keeps Review and can be dropped",
  );
  const dropped = { id: "D", status: "dropped", derived: { status: "done", reason: "Dropped · no" } };
  assert.deepEqual(rowActions(dropped), []);
  const view = listView({ tasks: [dropped, { id: "L", status: "integrated", derived: { status: "done" } }] }, {});
  assert.equal(view.doneLine.count, 1, "a dropped task is not counted as done");
});

test("a task in review says so, not its worker's last progress line", () => {
  const reviewing = {
    id: "R",
    title: "t",
    status: "reported",
    derived: { status: "working" },
    last_status: { message: "mul done; test-file scope question", progress: 70 },
    attempts: [{ gates: [], reviews: [{ reviewer: "codex", degraded: false }] }],
  };
  assert.equal(secondLine(reviewing), "In review");
  assert.equal(progressOf(reviewing), null, "a worker's old 70% is not drawn");
  const working = { ...reviewing, status: "working", attempts: [] };
  assert.equal(secondLine(working), "mul done; test-file scope question");
  assert.equal(progressOf(working), 70);
});

test("the add form takes an After list and names an id the board does not hold", () => {
  const ok = newTask({ title: "a", scope: "x", acceptance: "y", deps: "T1, T2" }, ["T1", "T2"]);
  assert.deepEqual(ok.deps, ["T1", "T2"]);
  assert.equal(ok.ready, true);
  const bad = newTask({ title: "a", scope: "x", acceptance: "y", deps: "nope" }, ["T1"]);
  assert.equal(bad.ready, false);
  assert.match(bad.why, /No task named nope/);
});

// -- the tasks panel's reading of a row's act ------------------------------

/** One task of the M10.5 board, by id. */
function boardTask(id) {
  return BOARD.tasks.find((entry) => entry.id === id);
}

test("a row's button follows the word the row shows, for every status and word (SHPR-78)", () => {
  const ids = (task) => rowActions(task).map((act) => act.id);
  const row = (status, word) => ({ id: "X", status, derived: { status: word } });
  // Review only where the row reads Ready, or needs the person about a hand-in.
  assert.deepEqual(ids(row("passed", "ready")), ["review"]);
  assert.deepEqual(ids(row("reported", "ready")), ["review"]);
  assert.deepEqual(ids(row("passed", "needs_you")), ["review", "drop"]);
  assert.deepEqual(ids(row("reported", "needs_you")), ["review", "drop"]);
  // A handed-in task whose agent took the work up again reads Working: no Review.
  for (const status of ["passed", "reported"]) {
    for (const word of ["working", "merging", "done", "queued", "to_do"]) {
      assert.equal(ids(row(status, word)).includes("review"), false, `${status}/${word}`);
      assert.deepEqual(ids(row(status, word)), [], `${status}/${word}`);
    }
  }
  // An older service sends no word: the hand-in still reads as reviewable.
  assert.deepEqual(ids({ id: "X", status: "passed" }), ["review"]);
  // Review never shows on a row that is working, failed, dropped or not yet started.
  for (const [status, word] of [["working", "working"], ["failed", "needs_you"], ["dropped", "done"], ["queued", "to_do"], ["queued", "scheduled"], ["integrated", "done"], ["integrated", "merging"]]) {
    assert.equal(ids(row(status, word)).includes("review"), false, `${status}/${word}`);
  }
  assert.deepEqual(ids(row("working", "working")), ["stop"]);
  assert.deepEqual(ids(row("queued", "to_do")), ["start"]);
  assert.deepEqual(ids(row("failed", "needs_you")), ["retry", "drop"]);
  // The head's Review and the card's Accept agree with the row.
  const again = row("passed", "working");
  assert.equal(canReview(again), false);
  assert.equal(verbs(again, "ask").accept, false, "no Accept on a task that is working again");
});

test("a row offers the one verb the engine has for its state", () => {
  // T2 is passed, so its act opens the Accept card rather than posting a verb.
  // It needs the person too, so it can also be dropped from its row.
  assert.deepEqual(rowActions(boardTask("T2")), [
    { id: "review", label: "Review", action: null },
    { id: "drop", label: "Drop", action: null },
  ]);
  // T3 failed its review: the send-back is a spawn, which is what a retry is.
  // A failed task can also be given up: Drop opens the card on its Drop step.
  assert.deepEqual(rowActions(boardTask("T3")), [
    { id: "retry", label: "Retry", action: "task.retry" },
    { id: "drop", label: "Drop", action: null },
  ]);
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
    deps: [],
    why: "",
    ready: true,
  });
  assert.equal(newTask({}).ready, false);
  assert.match(newTask({}).why, /title/);
  assert.match(newTask({ title: "x" }).why, /must be true/);
  assert.match(newTask({ title: "x", scope: "a" }).why, /must be true/);
  // The paths are an optional hint: a task with none is ready.
  assert.equal(newTask({ title: "x", acceptance: "it works" }).ready, true);
  assert.deepEqual(newTask({ title: "x", acceptance: "it works" }).scope, []);
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
  assert.equal(protectedLine(CHANGES), null);
  assert.equal(protectedLine({ protected: [] }), null);
  assert.equal(
    protectedLine({ protected: ["AGENTS.md", "docs/plan.md"] }),
    "Touches protected paths: AGENTS.md, docs/plan.md",
  );
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

test("the thread's switch is Chat, Terminal, then Pull request and Artifacts once it has them", () => {
  const T1 = boardTask("T1");
  const bare = { ...boardTask("T2"), pr: undefined, attempts: [] };
  const words_ = (task, opts) => threadViews(task, opts).map((seg) => seg.word);
  assert.deepEqual(words_(bare, { live: true }), ["Chat", "Terminal"], "no Pull request or Artifacts before it has one");
  assert.deepEqual(words_({ ...bare, pr: T1.pr }, { live: true }), ["Chat", "Terminal", "Pull request"]);
  assert.deepEqual(words_(T1, { live: true }), ["Chat", "Terminal", "Pull request", "Artifacts"]);
  assert.equal(hasArtifacts(T1), true);
  assert.equal(hasArtifacts(bare), false);
  // Chat is always on and is what a thread opens on; Terminal is dim when no agent is up.
  const open = threadViews(T1, { live: false });
  assert.equal(open[0].on, true, "a thread opens on its chat");
  const segs = threadViews(T1, { live: false, shown: "tasks-thread-term" });
  assert.equal(segs[0].dead, false);
  assert.equal(segs[0].child, "tasks-thread-chat");
  assert.equal(segs[0].on, false);
  assert.equal(segs[1].dead, true, "Not running");
  assert.equal(segs[1].on, true);
  // Terminal is lit for its live terminal and for its read-only stream, and shows the one given.
  assert.equal(threadViews(T1, { live: true, shown: VIEW.stream.id, termChild: VIEW.stream.id })[1].on, true);
  assert.equal(threadViews(T1, { live: true, termChild: VIEW.stream.id })[1].child, VIEW.stream.id);
  assert.equal(threadViews(T1, { live: true, shown: "tasks-thread-pr" }).find((seg) => seg.on).key, "pr");
  assert.equal(threadViews(T1, { live: true, shown: VIEW.artifacts.id }).find((seg) => seg.on).key, "artifacts");
  // The pull request of a task that lost it, or artifacts it has none of, fall back to the chat.
  assert.equal(mustLeavePr(bare, "tasks-thread-pr"), true);
  assert.equal(mustLeavePr(bare, VIEW.artifacts.id), true);
  assert.equal(mustLeavePr(T1, "tasks-thread-pr"), false);
  assert.equal(mustLeavePr(T1, VIEW.artifacts.id), false);
  assert.equal(mustLeavePr(bare, "tasks-thread-term"), false);
  assert.equal(mustLeavePr(bare, "tasks-thread-chat"), false);
});

test("the Terminal view is the agent's own terminal, and a read-only stream only when it cannot be shown", () => {
  const now = Date.now();
  const worker = { id: "t1", terminal: "t1", task: "T1", role: "worker", state: "working", started_ms: now - 60_000 };
  const reviewer = { id: "t2", terminal: "t2", task: "T1", role: "reviewer", state: "working", started_ms: now - 10_000 };
  const ended = { id: "t3", terminal: "t3", task: "T9", role: "worker", state: "done", ended_ms: now - 1000, started_ms: now - 5000 };
  const state = { live_agents: [worker, reviewer, ended, { id: "c", terminal: "c", task: null, role: "coordinator", state: "idle" }] };
  // Whoever is at work now: the newer of a worker and its reviewer; a finished agent when none runs.
  assert.equal(threadAgent(state, "T1", now).id, "t2");
  assert.equal(threadAgent({ live_agents: [worker, { ...reviewer, state: "done" }] }, "T1", now).id, "t1");
  assert.equal(threadAgent(state, "T9", now).id, "t3");
  assert.equal(threadAgent(state, "T7", now), null);
  // An agent without a start time is ordered by when it entered its state.
  const byState = { live_agents: [{ ...worker, started_ms: undefined, since_ms: now - 5 }, { ...reviewer, started_ms: undefined, since_ms: now - 9 }] };
  assert.equal(threadAgent(byState, "T1", now).id, "t1");
  // The window holds a terminal it lists by address; a finished program is not one to type into.
  const held = [{ address: "t2", state: "running" }];
  assert.equal(terminalReachable(reviewer, held), true);
  assert.equal(terminalReachable(worker, held), false, "a terminal the window does not list");
  assert.equal(terminalReachable(ended, [{ address: "t3", state: "running" }]), false);
  assert.equal(terminalReachable(null, held), false);
  assert.equal(terminalChild(reviewer, held), VIEW.term.id);
  assert.equal(terminalChild(worker, held), VIEW.stream.id);
  assert.equal(terminalChild(null, held), VIEW.stream.id);
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

test("the proof the hand-in named is listed by file name and path", () => {
  const named = { attempts: [{ report: { proofs: [".shprd/proof/T5-usage.txt"] } }] };
  assert.deepEqual(handInProofs(named), [{ name: "T5-usage.txt", path: ".shprd/proof/T5-usage.txt" }]);
  // A report that names none, or a service that predates the field, lists none.
  assert.deepEqual(handInProofs({ attempts: [{ report: { checks: [] } }] }), []);
  assert.deepEqual(handInProofs(boardTask("T6")), []);
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

test("a failed task a person accepted offers Accept", () => {
  const failed = { ...boardTask("T2"), status: "failed", derived: { status: "needs_you" } };
  assert.equal(verbs(failed, "ask").accept, false);
  const accepted = { ...failed, derived: { status: "ready" }, accepted_failure: true };
  assert.equal(verbs(accepted, "ask").accept, true);
  assert.equal(verbs(accepted, "checks").accept, false);
});

test("Accept is offered disabled, with the reason, where landing would refuse", () => {
  const why = "T2 was handed in but no review was requested";
  const blocked = { ...boardTask("T2"), accept_blocked: why };
  const offered = verbs(blocked, "ask");
  assert.equal(offered.accept, true, "the button is still on the card");
  assert.equal(offered.acceptBlocked, why);
  assert.match(offered.note, /Can't accept yet: T2 was handed in/);
  assert.equal(verbs(boardTask("T2"), "ask").acceptBlocked, null);
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
  // A task flagged for a protected path waits for the person under any policy.
  for (const policy of ["checks", "auto"]) {
    const flagged = verbs(ready, policy, { protected: ["AGENTS.md"] });
    assert.equal(flagged.accept, true, policy);
    assert.equal(flagged.note, null, policy);
  }
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
    assert.deepEqual(verbs(boardTask(id), "ask"), { accept: false, acceptBlocked: null, sendBack: false, drop: false, note: null });
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

// -- SHPR-27: deps in the form, group by, pins, the done tick ----------------

import { GROUP_MODES, groupMode, splitIds, taskIds } from "../mods/lib/board.js";
import { tasksMenu } from "../mods/lib/menu.js";
import { status as statusWord } from "../mods/lib/tokens.js";

test("the add-task form takes the ids a task waits for, and refuses one nobody has", () => {
  const body = { title: "T2", scope: "a", acceptance: "ok", deps: "T1, T1  M2" };
  assert.deepEqual(newTask(body).deps, ["T1", "M2"]);
  assert.equal(newTask(body).ready, true, "without a board to check against, the service decides");
  assert.equal(newTask({ ...body, deps: "" }).deps.length, 0);
  const checked = newTask(body, ["T1"]);
  assert.equal(checked.ready, false);
  assert.match(checked.why, /No task named M2/);
  assert.equal(newTask(body, ["T1", "M2"]).ready, true);
  assert.deepEqual(splitIds(null), []);
  assert.deepEqual(taskIds(STATE).slice(0, 1), [STATE.tasks[0].id]);
});

test("Group by Status gives every word its own heading, Needs you and Ready included", () => {
  const view = listView(STATE, { groupBy: "status" });
  assert.equal(view.groups.some((g) => g.key === "you"), false);
  assert.equal(view.groups[0].key, "needs_you");
  assert.equal(view.groups.find((g) => g.key === "ready").tasks.length, 1);
  assert.match(view.doneLine.label, /done$/);
});

test("Group by Space gives each space its own heading, tasks in status order", () => {
  const board = {
    tasks: [
      { id: "A1", title: "a", space: "s1", derived: { status: "working" } },
      { id: "A2", title: "b", space: "s1", derived: { status: "needs_you" } },
      { id: "B1", title: "c", space: "s2", derived: { status: "done" } },
      { id: "C1", title: "d", derived: { status: "queued" } },
    ],
  };
  const spaces = [{ id: "s1", name: "shop" }, { id: "s2", name: "docs" }];
  const view = listView(board, { groupBy: "space", spaces });
  assert.deepEqual(view.groups.map((g) => g.label), ["shop"], "Done is folded out of the space groups");
  assert.deepEqual(view.groups[0].tasks.map((t) => t.id), ["A2", "A1", "C1"], "a task with no space is in the first space");
  assert.equal(view.doneLine.label, "1 done");
  const open = listView(board, { groupBy: "space", spaces, doneOpen: true });
  assert.equal(open.groups.at(-1).key, "done", "an opened Done is its own group, which folds back");
  assert.equal(open.doneLine, null);
});

test("There is no pinned group: a row has no star and listView takes no pins (SHPR-78)", () => {
  const view = listView(STATE, { pinned: new Set(["T3", "T1"]) });
  assert.equal(view.groups.some((g) => g.key === "pinned"), false);
  assert.equal(groupMode("nonsense"), "you");
  assert.equal(groupMode("space"), "space");
  assert.deepEqual(GROUP_MODES.map((m) => m.key), ["you", "space", "status"]);
});

test("Done is a grey tick, and only Ready is green", () => {
  assert.deepEqual([statusWord("done").glyph, statusWord("done").tone], ["tick", "dim"]);
  assert.deepEqual([statusWord("ready").glyph, statusWord("ready").tone], ["tick", "green"]);
});

test("the Tasks popups list their rows and mark the one in force", () => {
  const group = tasksMenu({ menu: "group", items: GROUP_MODES, current: "space" });
  assert.equal(group[0].heading, "Group by");
  assert.deepEqual(group.slice(1).map((r) => r.pick), [
    { menu: "group", key: "you" },
    { menu: "group", key: "space" },
    { menu: "group", key: "status" },
  ]);
  assert.equal(group[2].hint, "✓");
  const more = tasksMenu({ menu: "more", items: [{ key: "done", word: "Done", count: 4 }], current: "all" });
  assert.deepEqual(more.map((r) => [r.label, r.hint]), [["Done", "4"]]);
});

test("a Waiting line from the engine keeps its path and task id on the row", () => {
  const line = "Waiting: crates/shprd/tests is in use by install-layout-source";
  assert.equal(secondLine({ title: "t", last_status: { message: line } }), line);
  assert.equal(secondLine({ title: "t", last_status: { message: "x".repeat(60) } }).length, 40);
});

// ---- SHPR-67: old tasks never block a fresh start ----

const NOW = 1_000_000_000_000;
const HOURS = 60 * 60 * 1000;
const aged = (id, word, hours, extra = {}) => ({
  id,
  title: `task ${id}`,
  status: word === "done" ? "integrated" : "queued",
  derived: { status: word },
  added_at_ms: NOW - hours * HOURS,
  touched_at_ms: NOW - hours * HOURS,
  ...extra,
});

test("an open task untouched for hours is old; a fresh one, a working one and a done one are not", () => {
  const board = {
    tasks: [
      aged("OLD1", "to_do", 9),
      aged("OLD2", "queued", 5),
      aged("NEW", "needs_you", 0.1),
      aged("ASKED", "needs_you", 9),
      aged("RUN", "working", 9),
      aged("DONE", "done", 9),
      { ...aged("NOTIME", "to_do", 9), touched_at_ms: undefined },
    ],
  };
  // A needs-you task is never old, however long it has waited (UXC-7).
  assert.deepEqual([...staleIds(board, NOW)].sort(), ["OLD1", "OLD2"]);
  assert.equal(STALE_MS, 3 * HOURS);
});

test("a queued task waiting on an old task is old with it, however new", () => {
  const board = {
    tasks: [
      aged("OLD", "to_do", 9),
      aged("BEHIND", "to_do", 0.1, { deps: ["OLD"] }),
      aged("BEHIND2", "to_do", 0.1, { deps: ["BEHIND"] }),
      aged("FREE", "to_do", 0.1),
    ],
  };
  assert.deepEqual([...staleIds(board, NOW)].sort(), ["BEHIND", "BEHIND2", "OLD"]);
});

test("old tasks fold into one Older group at the bottom; new ones are on top, newest first", () => {
  const board = {
    tasks: [
      aged("OLD1", "to_do", 9),
      aged("OLD2", "to_do", 8),
      aged("ASKED", "needs_you", 9),
      aged("N1", "to_do", 0.5),
      aged("N2", "to_do", 0.1),
      aged("D1", "done", 20),
    ],
  };
  const view = listView(board, { now: NOW });
  assert.deepEqual(view.groups.map((g) => g.key), ["you", "to_do", "older"]);
  assert.deepEqual(view.groups[1].tasks.map((t) => t.id), ["N2", "N1"], "newest first");
  const older = view.groups[2];
  assert.equal(older.shut, true, "folded until opened");
  assert.equal(older.stale, true);
  assert.deepEqual([...older.ids].sort(), ["OLD1", "OLD2"]);
  assert.equal(view.doneLine.label, "1 done");
  assert.equal(listView(board, { now: NOW, olderOpen: true }).groups[2].shut, false);
  // A search still finds an old task in place.
  const found = listView(board, { now: NOW, query: "OLD1" });
  assert.deepEqual(found.groups.flatMap((g) => g.tasks.map((t) => t.id)), ["OLD1"]);
  assert.equal(found.groups[0].key, "to_do");
});

test("an old needs-you task is counted in the badge and never sits under Older", () => {
  const board = { tasks: [aged("OLD", "needs_you", 9), aged("NEW", "needs_you", 0.1)] };
  assert.equal(counts(board, NOW).needs, 2);
  const view = listView(board, { now: NOW });
  assert.deepEqual(view.groups.map((g) => g.key), ["you"]);
  assert.deepEqual(view.groups[0].tasks.map((t) => t.id).sort(), ["NEW", "OLD"]);
});

test("UXC-7: a needs-you row offers its newest decision card's button, and Drop", () => {
  const t = { ...aged("T", "needs_you", 9), status: "failed" };
  const state = (needs, asks = []) => ({ tasks: [t], needs, asks });
  const ids = (s) => rowActions(t, s).map((a) => a.id);
  const card = (seq, actions, kind = "decision") => ({ task: "T", body: "b", seq, kind, actions });
  assert.deepEqual(ids(state([card(1, ["retry"]), card(2, ["resend_brief"])])), ["resend_brief", "drop"]);
  assert.equal(rowActions(t, state([card(2, ["resend_brief"])]))[0].action, "task.resend_brief");
  // A newer notice never speaks for the task.
  assert.deepEqual(ids(state([card(2, ["resend_brief"]), card(5, ["dismiss"], "notice")])), ["resend_brief", "drop"]);
  assert.deepEqual(ids(state([card(2, ["drop"])])), ["drop"]);
  assert.deepEqual(ids(state([card(2, ["retry"])], [{ task: "T", id: 1, options: ["a", "b"] }])), ["answer", "drop"]);
  // No state, or no card: the default acts.
  assert.deepEqual(ids(state([])), ["retry", "drop"]);
});

test("SHPR-69: another product's Assistant tab is not a pill in shprd's tab bar", () => {
  const space = { id: "sp1", tabs: [
    { id: "tb1", name: "Assistant", active: false },
    { id: "tb2", name: "shprd", active: true },
    { id: "tb3", name: "pi·T1" },
  ] };
  const panes = [
    { tab: "tb1", mod: "chat" },
    { tab: "tb2", mod: "shprd-chat" },
    { tab: "tb3", terminal: "t9" },
  ];
  assert.equal(foreignTab(space.tabs[0], panes), true);
  assert.equal(foreignTab(space.tabs[1], panes), false);
  const rows = tabRows(space, panes, STATE);
  assert.deepEqual(rows.map((row) => row.id), ["tb2", "tb3"]);
  // The tabs keep the host's own positions: a drag still names the right slot.
  assert.deepEqual(rows.map((row) => row.index), [1, 2]);
});

// SHPR-79: a shprd space's center shows shprd's coordinator, whatever Shepherdr's tab bar did.
import { seenWith, strayTab, shprdCoordinatorTab } from "../mods/lib/tabstrip.js";
import { COLUMN, GUTTER, bubbleSpan, bubbleWidth } from "../mods/lib/chat.js";

test("strayTab moves a shprd space off Shepherdr's Assistant, and only there (SHPR-79)", () => {
  const panes = [{ tab: "assist", mod: "chat" }, { tab: "shprd-coordinator-sp", mod: "shprd-chat" }];
  const space = (active) => ({
    id: "sp",
    name: "shprd · scratchpad",
    tabs: [
      { id: "assist", name: "Assistant", active: active === "assist" },
      { id: "shprd-coordinator-sp", name: "shprd", active: active === "shprd-coordinator-sp" },
    ],
  });
  assert.equal(shprdCoordinatorTab({ id: "shprd-coordinator-sp" }), true);
  assert.equal(shprdCoordinatorTab({ id: "assist" }), false);
  assert.equal(strayTab(space("assist"), panes), "shprd-coordinator-sp");
  assert.equal(strayTab(space("shprd-coordinator-sp"), panes), null);
  // A space with no shprd coordinator (Shepherdr's own, or `shprd · shprd` where the orchestrator
  // runs) keeps its Assistant: nothing moves.
  const theirs = { id: "x", name: "sweatbrain", tabs: [{ id: "assist", name: "Assistant", active: true }] };
  assert.equal(strayTab(theirs, panes), null);
});

test("seenWith notes shprd's spaces so Shepherdr never adds an Assistant to them (SHPR-79)", () => {
  const spaces = [
    { id: "a", name: "shprd · scratchpad" },
    { id: "b", name: "sweatbrain" },
    { id: "m/c", name: "shprd · remote", machine: "m" },
  ];
  assert.deepEqual(seenWith(["b"], spaces), ["b", "a"]);
  assert.equal(seenWith(["a", "b"], spaces), null);
  assert.equal(seenWith(null, spaces), null, "Shepherdr has not started the list: it notes every space itself");
});

test("the chat's column and bubbles follow Shepherdr's at every pane width (SHPR-79)", () => {
  assert.equal(COLUMN, 760);
  assert.equal(GUTTER, 36);
  assert.equal(bubbleWidth(600), 480);
  assert.equal(bubbleWidth(2000), 608);
  assert.equal(bubbleWidth(100), 240);
});

test("a done task's second line says it is done, never a stale agent line; the bar follows the board (SHPR-79)", () => {
  const stale = { id: "T1", title: "ops", status: "passed", derived: { status: "done", reason: "No longer waiting" }, last_status: { message: "committed, handing in" }, attempts: [] };
  assert.equal(secondLine(stale), "Done");
  const reported = { ...stale, attempts: [{ report: { summary: "Added convert()" } }] };
  assert.equal(secondLine(reported), "Added convert()");
  const t = (id, status) => ({ id, title: id, status: "queued", derived: { status }, attempts: [] });
  const state = (...words) => ({ run: { id: "tcalc1" }, tasks: words.map((w, i) => t(`T${i}`, w)) });
  assert.equal(runShort(state("working", "working", "ready", "to_do")), "run tcalc1 · 2 working · 1 ready");
  assert.equal(runShort(state("done", "done")), "run tcalc1 · all done");
});

test("a bubble gets a definite width from its longest line, within the column (SHPR-79)", () => {
  // The coordinator's bubbles keep their sizing: 8 px a character + 30, never under 120.
  assert.equal(bubbleSpan("ok", 900), 120);
  const line = "T1 landed. T2 and T3 can now start, so I'll run a tick.";
  assert.equal(bubbleSpan(line, 900), line.length * 8 + 30);
  assert.equal(bubbleSpan(line, 900, false), line.length * 8 + 30);
  assert.equal(bubbleSpan("x".repeat(500), 900), bubbleWidth(900));
  assert.equal(bubbleSpan("x".repeat(500), 300), bubbleWidth(300));
  const second = "and a second much longer line of words here";
  assert.equal(bubbleSpan(`short\n${second}`, 900), second.length * 8 + 30);
});

test("the person's bubble hugs its text, up to the same maximum width", () => {
  assert.equal(bubbleSpan("ok", 900, true), 96);
  const short = "shprd restarted and is now running version 0.0.0 (commit 7c2b6b571).";
  assert.equal(bubbleSpan(short, 1060, true), short.length * 7 + 26);
  assert.ok(bubbleSpan(short, 1060, true) < bubbleSpan(short, 1060, false));
  assert.ok(bubbleSpan(short, 1060, true) < bubbleWidth(1060));
  assert.equal(bubbleSpan("x".repeat(500), 900, true), bubbleWidth(900));
});

// UXC-5: the Drop dialog has one Drop task button, in one place at a time.
test("the Drop dialog never draws two Drop task controls, and Enter, Esc and a blank reason behave", async () => {
  const { record, walk, keydown } = await import("gpui-kit");
  const { default: Accept } = await import("../mods/shprd-accept/main.js");
  const labelled = (root, label) => walk(root).filter((n) => n.kids.includes(label));
  const open = (drop) => {
    const view = new Accept();
    view.cx = { notify() {}, timer: { after() {} } };
    view.taskId = "T1";
    view.state = { tasks: [{ id: "T1", title: "One", status: "reported", derived: { status: "needs_you" }, attempts: [] }] };
    view.handle = { focus() {} };
    if (drop) {
      view.openDrop("row");
      // Re-open with a spy: the field has focus at once, and again after layout settles.
      let focused = 0;
      const later = [];
      view.reason.focus = () => {
        focused += 1;
      };
      view.cx.timer.after = (_ms, fn) => later.push(fn);
      view.openDrop("row");
      assert.equal(focused, 1);
      later.forEach((fn) => fn());
      assert.equal(focused, 2);
    }
    const sent = [];
    view.verb = (action, extra) => sent.push([action, extra]);
    return { view, sent, draw: () => view.render({}) };
  };
  record(true);
  try {
    // Opened on the step (a row's Drop): one "Drop task", no "Drop task…".
    const row = open(true);
    assert.equal(row.view.dropFrom, "row");
    assert.equal(labelled(row.draw(), "Drop task").length, 1);
    assert.equal(labelled(row.draw(), "Drop task…").length, 0);
    // Esc closes (never drops) where the dialog opened on the step.
    let closed = 0;
    row.view.close = () => {
      closed += 1;
    };
    keydown(row.draw(), "accept-card", { key: "escape" });
    assert.equal(closed, 1);
    assert.deepEqual(row.sent, []);
    // Enter in the field is the input's submit; a blank reason sends the default.
    row.view.reason.submit();
    assert.deepEqual(row.sent, [["task.drop", { reason: "No reason given" }]]);
    row.view.reason.set_value("  too slow  ");
    row.view.reason.submit();
    assert.deepEqual(row.sent[1], ["task.drop", { reason: "too slow" }]);

    // Opened from the footer: "Drop task…" is the only label until the step opens.
    const foot = open(false);
    assert.equal(labelled(foot.draw(), "Drop task…").length, 1);
    assert.equal(labelled(foot.draw(), "Drop task").length, 0);
    walk(foot.draw()).find((n) => n.idv === "accept-drop").click({}, { notify() {}, stop_propagation() {} });
    assert.equal(foot.view.dropping, true);
    assert.equal(labelled(foot.draw(), "Drop task").length, 1);
    assert.equal(labelled(foot.draw(), "Drop task…").length, 0);
    // Esc is Back here: the footer returns, the typed text stays, nothing is dropped.
    foot.view.reason.set_value("keep me");
    keydown(foot.draw(), "accept-card", { key: "escape" });
    assert.equal(foot.view.dropping, false);
    assert.equal(foot.view.reason.value(), "keep me");
    assert.deepEqual(foot.sent, []);
  } finally {
    record(false);
  }
});
