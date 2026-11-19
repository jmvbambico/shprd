// Unit tests for the settings window's view model: the pages it lists, the level
// its writes go to, the roles table, the workflow keys of the Tasks page, the
// layers every value came from, the choices the pickers offer, a refusal's words
// and the template list.
//
//   node --import ./test/boot.mjs --test test/          (node 22+)
//   bun test --preload ./test/boot-bun.mjs test/        (bun)
//
// The fixtures are a real `shprd serve`'s answers, captured from a throwaway run
// (`test/fixtures/settings.json`, `roster.json`, `prompts.json`): a person's own
// file with picks for every role and one custom role, the shipped harness table
// with its catalogs, and the four templates this build ships. Nothing here
// invents a shape the service does not send; a program's catalog is cut to its
// first few models, because one of them lists hundreds and this file is read by
// tests, not by a picker.

import test from "node:test";
import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import * as herdr from "herdr";

import { ServiceError, reason } from "../mods/lib/client.js";
import {
  BUILTIN,
  CHANGED_HERE,
  LAYER,
  LEVEL,
  PAGES,
  PICKS,
  RANGE,
  SCOPES,
  TITLE,
  UNSET_LAYER,
  WORKFLOW,
  HIDDEN,
  ARCHIVE_PRESETS,
  CHAIN_NOTE,
  hasLines,
  SELECT_HARNESS,
  promptName,
  boxProblem,
  branchProblem,
  maskLink,
  ago,
  sessionRows,
  tunnelView,
  effortChoices,
  effortLevels,
  forRepo,
  health,
  layerOf,
  levelNote,
  modelChoices,
  outcomeOf,
  pageOf,
  programChoices,
  programOf,
  promptLine,
  promptRows,
  nextLineProgram,
  plainRefusal,
  refused,
  reviewBlocks,
  reviewModelBlocks,
  roleIds,
  roleOf,
  roles,
  scopeOf,
  stepped,
  withRole,
  withoutRole,
  workflowRows,
} from "../mods/lib/settings.js";
import {
  openSettings,
  settingsWindow,
  settingsWindowClosed,
} from "../mods/lib/panel.js";
import { tone } from "../mods/lib/tokens.js";
import Settings from "../mods/shprd-settings/main.js";

/** One fixture, parsed. */
function fixture(name) {
  return JSON.parse(readFileSync(fileURLToPath(new URL(`./fixtures/${name}`, import.meta.url)), "utf8"));
}

/** The settings document a real `GET /api/settings` answered. */
const SETTINGS = fixture("settings.json");
/** The programs a real `GET /api/roster` answered. */
const ROSTER = fixture("roster.json");
/** The templates a real `GET /api/prompts` answered. */
const PROMPTS = fixture("prompts.json");

/** One role from the page's rows, by id. */
function role(id) {
  return roles(SETTINGS, ROSTER).find((entry) => entry.id === id);
}

/** One pick of one role. */
function pick(id, field) {
  return role(id).picks.find((entry) => entry.field === field);
}

// -- the roles table -------------------------------------------------------

test("the roles are the ones the settings resolve, in their order", () => {
  assert.deepEqual(roleIds(SETTINGS), ["coordinator", "worker", "reviewer", "docs"]);
  assert.deepEqual(
    roles(SETTINGS, ROSTER).map((entry) => entry.title),
    ["Assistant", "Worker (s)", "Reviewer (s)", "docs"],
  );
  assert.deepEqual(role("worker").picks.map((entry) => entry.field), PICKS);
});

test("a role this build does not ship reads as the person's own", () => {
  assert.equal(role("docs").custom, true);
  assert.equal(role("docs").kind, "helper");
  for (const id of BUILTIN) {
    assert.equal(role(id).custom, false, id);
  }
  assert.equal(role("reviewer").readonly, true, "the reviewer only reads");
});

test("one settings file with no order still shows the roles this build ships", () => {
  const shipped = SETTINGS.values.filter(
    (row) => row.key !== "roles.order" && !row.key.startsWith("roles.docs."),
  );
  assert.deepEqual(roleIds({ values: shipped }), BUILTIN);
  // A role whose keys are in the file but not in the order is kept, not lost.
  const stray = {
    values: [...shipped, { key: "roles.handyman.agent", value: "pi", source: "you" }],
  };
  assert.deepEqual(roleIds(stray), [...BUILTIN, "handyman"]);
});

test("every value says the layer that answered it, in the service's words", () => {
  assert.equal(layerOf("you"), "you");
  assert.equal(layerOf("built_in"), "built in");
  assert.equal(layerOf("repo_entry"), "this repo");
  assert.equal(layerOf(null), UNSET_LAYER);
  assert.deepEqual(Object.keys(LAYER), [
    "task",
    "repo_entry",
    "you",
    "legacy",
    "repo_file",
    "built_in",
  ]);
  assert.equal(pick("worker", "agent").layer, "you");
  assert.equal(pick("worker", "effort").layer, UNSET_LAYER, "nothing set the effort");
});

test("a program is shown by its name, and written by its id", () => {
  const agent = pick("worker", "agent");
  assert.equal(agent.key, "roles.worker.agent");
  assert.equal(agent.value, "pi");
  assert.equal(agent.display, "Pi");
  assert.equal(agent.note, null);
  // An unset role reads as one that has not chosen yet, not as a broken one.
  const empty = roles({ values: SETTINGS.values.filter((row) => row.key !== "roles.worker.agent") }, ROSTER);
  const worker = empty.find((entry) => entry.id === "worker");
  const unset = worker.picks.find((entry) => entry.field === "agent");
  assert.equal(unset.value, null);
  assert.equal(unset.display, "Not chosen yet");
  assert.match(unset.note, /Pick the program/);
});

test("the program picker offers every program the harness table defines", () => {
  assert.deepEqual(
    programChoices(ROSTER).map((choice) => choice.value),
    ["claude", "codex", "kilo", "opencode", "pi"],
  );
  assert.deepEqual(programChoices(ROSTER)[0], {
    value: "claude",
    label: "Claude Code",
    hint: "anthropic",
  });
  assert.equal(programOf(ROSTER, "pi").name, "Pi");
  assert.equal(programOf(ROSTER, "nope"), null);
});

test("a model picker offers the catalog's models after the program's default", () => {
  const claude = programOf(ROSTER, "claude");
  const choices = modelChoices(claude);
  assert.equal(choices.length, 6);
  assert.deepEqual(choices[0], { value: null, label: "The program's default", hint: null });
  assert.deepEqual(
    choices.slice(1).map((choice) => choice.value),
    ["fable", "opus", "opus[1m]", "sonnet", "haiku"],
  );
  assert.equal(choices[2].label, "Opus 5.5");

  const model = pick("coordinator", "model");
  assert.equal(model.value, "opus");
  assert.equal(model.display, "opus");
  assert.equal(model.source, "you");
  assert.equal(model.choices.length, 6);
});

test("a program whose catalog named no model still offers the default, and says why", () => {
  // pi lists its models itself; this service was started without its provider
  // configured, so the catalog came back with nothing.
  const pi = programOf(ROSTER, "pi");
  assert.deepEqual(pi.models, []);
  assert.match(pi.models_note, /type a model id/);
  const model = pick("worker", "model");
  assert.equal(model.value, "commandcode/z-ai/glm-5.3-flash");
  assert.equal(model.choices.length, 1, "the default is still a choice");
  assert.match(model.note, /type a model id/);
  assert.deepEqual(modelChoices(null), [], "no program, no choices");
});

test("an effort picker offers the model's own levels, and nothing where it takes none", () => {
  const claude = programOf(ROSTER, "claude");
  assert.deepEqual(effortChoices(claude, "opus").map((choice) => choice.value), [
    null,
    "low",
    "medium",
    "high",
    "xhigh",
    "max",
  ]);
  assert.deepEqual(effortLevels(claude, "opus"), ["low", "medium", "high", "xhigh", "max"]);
  // A model the catalog does not name falls back to the program's own levels.
  assert.deepEqual(effortLevels(claude, "typed-by-hand"), ["low", "medium", "high", "xhigh", "max"]);
  // kilo takes no effort at all, so there is nothing to pick, and the row says so.
  assert.deepEqual(effortChoices(programOf(ROSTER, "kilo"), null), []);
  const kilo = roles(
    {
      values: SETTINGS.values.map((row) =>
        row.key === "roles.worker.agent" ? { ...row, value: "kilo" } : row,
      ),
    },
    ROSTER,
  ).find((entry) => entry.id === "worker");
  const effort = kilo.picks.find((entry) => entry.field === "effort");
  assert.deepEqual(effort.choices, []);
  assert.match(effort.note, /takes no effort level/);
  // A role with no program picked gets the picker's own hint.
  const noProgram = roles(
    { values: SETTINGS.values.filter((row) => row.key !== "roles.worker.agent") },
    ROSTER,
  ).find((entry) => entry.id === "worker");
  assert.match(
    noProgram.picks.find((entry) => entry.field === "effort").note,
    /Pick a program first/,
  );
});

test("a model that wants its provider says so", () => {
  const empty = roles({ values: SETTINGS.values.filter((row) => row.key !== "roles.worker.model") }, ROSTER);
  const worker = empty.find((entry) => entry.id === "worker");
  const model = worker.picks.find((entry) => entry.field === "model");
  assert.match(model.note, /provider\/model/);
});

// -- writing ---------------------------------------------------------------

test("a role is added and removed through the order alone", () => {
  assert.deepEqual(withRole(SETTINGS, "tester"), [
    "coordinator",
    "worker",
    "reviewer",
    "docs",
    "tester",
  ]);
  assert.equal(withRole(SETTINGS, "worker"), null, "an id already there is not added twice");
  assert.equal(withRole(SETTINGS, "order"), null, "`order` is the list, not a role");
  assert.equal(withRole(SETTINGS, "a.b"), null);
  assert.equal(withRole(SETTINGS, "  "), null);
  assert.deepEqual(withoutRole(SETTINGS, "docs"), [
    "coordinator",
    "worker",
    "reviewer",
  ]);
});

test("a refusal is the service's sentence, kept word for word", () => {
  // The exact text the service refuses a same-vendor review with.
  const service =
    "`roles.reviewer.agent` would be refused: worker role is anthropic (pi/claude-opus-5-5), " +
    "reviewer role is anthropic (claude/opus): they are both anthropic, so the review would not " +
    "be independent";
  const error = new ServiceError(service, { status: 409, code: "refused" });
  const shown = refused("roles.reviewer.agent", error);
  assert.equal(shown.key, "roles.reviewer.agent");
  assert.equal(shown.text, service, "the words are passed through, not summarised");
  assert.equal(shown.text, reason(error));
  // A transport failure says what it can, and never nothing.
  assert.match(refused("roles.worker.agent", new Error("the service did not answer")).text, /did not answer/);
});

test("a reviewer's list rules out the worker's vendor, so the refusal cannot be clicked into", () => {
  const document = fixture("settings.json");
  const row = document.values.find((entry) => entry.key === "roles.worker.agent");
  row.value = "claude";
  const first = reviewBlocks(document, ROSTER, "reviewer", 0);
  assert.deepEqual(Object.keys(first), ["claude"]);
  assert.match(first.claude, /same vendor as a Worker/);
  assert.deepEqual(Object.keys(reviewBlocks(document, ROSTER, "reviewer", 1)), ["claude"]);
  // Pi runs several vendors' models, so it is never ruled out; other roles are not held to the rule.
  assert.equal(reviewBlocks(document, ROSTER, "reviewer", 0).pi, undefined);
  assert.deepEqual(reviewBlocks(document, ROSTER, "worker", 0), {});
  // Add New never picks a program the rule rules out: the reviewer's second line is not claude.
  assert.equal(nextLineProgram(document, ROSTER, "reviewer"), "codex");
  // No worker program, no rule.
  row.value = null;
  assert.deepEqual(reviewBlocks(document, ROSTER, "reviewer", 0), {});
});

test("a reviewer cannot pick a Pi model of the worker's vendor", () => {
  const document = fixture("settings.json");
  document.values.find((entry) => entry.key === "roles.worker.agent").value = "claude";
  const roster = structuredClone(ROSTER);
  const pi = roster.programs.find((program) => program.id === "pi");
  pi.models = [
    { id: "a", name: "A", vendor: "anthropic" },
    { id: "o", name: "O", vendor: "openai" },
  ];
  assert.deepEqual(Object.keys(reviewModelBlocks(document, roster, "reviewer", 0)), ["pi/a"]);
  assert.deepEqual(reviewModelBlocks(document, roster, "worker", 0), {});
});

test("a refusal is drawn in plain words, with no key path or backticks", () => {
  const later =
    "`roles.reviewer.agents` would be refused: the reviewer `reviewer`'s later line `claude` and the worker `worker` are both anthropic, so the review is not cross-vendor: pick a program from another family";
  assert.equal(
    plainRefusal(later, ROSTER),
    "Claude Code is the same vendor as a Worker (s) line; pick another harness for review.",
  );
  const first =
    "`roles.reviewer.agent` would be refused: the reviewer `reviewer` and the worker `worker` are both anthropic, so the review is not cross-vendor: set roles.reviewer.agent to a program from another family";
  assert.match(plainRefusal(first, ROSTER), /^The first Reviewer \(s\) line is the same vendor/);
  assert.equal(plainRefusal("`roles.x` would be refused: no `thing` here"), "no thing here");
  assert.doesNotMatch(plainRefusal(later), /`|roles\./);
});

test("a write says what it did", () => {
  assert.equal(outcomeOf({ outcome: "stored" }).text, "Saved");
  assert.equal(
    outcomeOf({ outcome: "equal_to_inherited" }).text,
    "That is what it would inherit, so nothing was stored",
  );
  assert.equal(outcomeOf({ outcome: "removed" }).text, "Removed: it inherits again");
  assert.equal(outcomeOf(null).text, "Saved");
});

test("the settings' own problems and skipped checks are shown, not hidden", () => {
  assert.deepEqual(health(SETTINGS), { problems: [], notChecked: null });
  const broken = {
    ...SETTINGS,
    problems: ["worker role is anthropic, reviewer role is anthropic: they are both anthropic"],
    not_checked: "no harness data directory: set SHPRD_DATA_DIR",
  };
  assert.deepEqual(health(broken).problems, broken.problems);
  assert.match(health(broken).notChecked, /SHPRD_DATA_DIR/);
});

// -- templates -------------------------------------------------------------

test("the template list says which prompts are the person's own", () => {
  const rows = promptRows(PROMPTS);
  assert.deepEqual(
    rows.map((row) => row.id),
    ["assistant", "worker", "reviewer", "backup"],
  );
  for (const row of rows) {
    assert.equal(row.word, "Default");
    assert.equal(row.edited, false);
    assert.equal(row.shipped, true);
    assert.equal(row.problem, null);
  }
  assert.equal(rows[1].about, "Does the work of one task on its own branch.");
  assert.match(rows[1].path, /prompts\/worker\.md$/);
});

test("an edited or unreadable template is marked as such", () => {
  const edited = promptRows({
    prompts: [
      { id: "worker", about: "the worker", path: "/p/worker.md", state: "edited", shipped: true },
      { id: "mine", path: "/p/mine.md", shipped: false },
      { id: "bad", path: "/p/bad.md", shipped: false, problem: "cannot read /p/bad.md" },
    ],
  });
  assert.equal(edited[0].word, "Edited");
  assert.equal(edited[0].edited, true);
  assert.equal(edited[1].word, "Default");
  assert.equal(edited[1].about, "a prompt of your own");
  assert.equal(edited[2].word, "Unreadable");
  assert.equal(edited[2].state, null);
  assert.match(edited[2].problem, /cannot read/);
});

test("the editor says what saving and resetting do", () => {
  assert.match(promptLine({ state: "edited" }), /never replaced by an upgrade/);
  assert.match(promptLine({ state: "default" }), /saving your own stores them/);
  // What a save just answered wins over what was read before it.
  assert.match(promptLine({ state: "edited" }, { state: "default" }), /default words/);
  assert.equal(promptLine(null), "");
});

test("a role key is read back as its role and field", () => {
  assert.deepEqual(roleOf("roles.worker.agent"), { id: "worker", field: "agent" });
  assert.equal(roleOf("roles.order"), null);
  assert.equal(roleOf("tasks.merge"), null);
  assert.equal(roleOf(undefined), null);
});

// -- the window's frame ----------------------------------------------------
//
// Settings is a dialog over the window, not a tab of the side panel (Shepherdr's
// own `openSettings()`): the page list, the level switch, and one row a setting
// on the right. What a page draws is a pure function of the service's answers, so
// these are the words and the choices a person reads.

/** The layout file the window draws from, parsed. */
const LAYOUT = JSON.parse(
  readFileSync(new URL("../layouts/shprd.json", import.meta.url), "utf8"),
);

/** The layout node called `id`, or null. */
function node(id, node_ = LAYOUT) {
  if (node_?.id === id) return node_;
  for (const child of node_?.children ?? []) {
    const found = node(id, child);
    if (found) return found;
  }
  return null;
}

test("the layout no longer carries a settings block for the panel to show", () => {
  const panel = node("panel-body");
  assert.ok(panel, "the side panel is still a stack");
  assert.deepEqual(
    panel.children.map((child) => child.id),
    ["files", "changes", "tasks"],
    "settings opens over the window, so it is not a stack the panel switches to",
  );
  assert.equal(node("settings"), null);
  // And the stack's own selection still names a child that is there.
  assert.ok(panel.children.some((child) => child.id === panel.active), panel.active);
});

test("the pages are the four shprd has, in the window's order, each with an icon", () => {
  assert.deepEqual(
    PAGES.map((page) => page.title),
    ["Agents", "Tasks", "Prompts", "Remote"],
  );
  assert.equal(TITLE, "Settings");
  assert.deepEqual(
    PAGES.map((page) => page.icon),
    ["bot", "list-checks", "file-text", "globe"],
  );
  for (const page of PAGES) {
    assert.ok(!page.soon, `${page.id} is a page, not a place held`);
    assert.ok(existsSync(fileURLToPath(new URL(`../mods/shprd-tabs/icons/${page.icon}.svg`, import.meta.url))), page.icon);
  }
  assert.equal(pageOf("remote").id, "remote");
  assert.equal(pageOf("nope").id, "agents");
  assert.equal(pageOf("tasks").id, "tasks");
});

test("a page a repository can own takes the level switch; the others say they cannot", () => {
  assert.equal(PAGES.find((page) => page.id === "agents").repo, true);
  assert.equal(PAGES.find((page) => page.id === "tasks").repo, true);
  assert.equal(Boolean(PAGES.find((page) => page.id === "prompts").repo), false);
  assert.equal(LEVEL.label, "Applies to");
  assert.match(LEVEL.one, /every repository/);
});

test("the level switch says what a write to each layer does", () => {
  assert.deepEqual(
    SCOPES.map((scope) => [scope.id, scope.word]),
    [
      ["you", "You"],
      ["repo", "This repo"],
    ],
  );
  assert.equal(forRepo("you"), false);
  assert.equal(forRepo("repo"), true);
  assert.equal(forRepo("nope"), false, "an unknown level writes the person's own file");
  assert.match(levelNote("you"), /your own settings/i);
  assert.match(levelNote("repo", "shop"), /only this repository \(shop\)/i);
  assert.match(levelNote("repo", null), /inherited from you/);
  assert.equal(scopeOf("repo").word, "This repo");
  assert.equal(CHANGED_HERE, "Set for this repository");
});

test("the Tasks page shows the workflow keys the settings API exposes", () => {
  const rows = workflowRows(SETTINGS);
  assert.deepEqual(
    rows.map((row) => row.key),
    WORKFLOW.map((row) => row.key),
  );
  // The three the task names, and the layers they came from, in the service's
  // own words.
  const merge = rows.find((row) => row.key === "tasks.merge");
  assert.deepEqual(
    { value: merge.value, layer: merge.layer, control: merge.control },
    { value: "ask", layer: "built in", control: "segmented" },
  );
  assert.deepEqual(
    merge.choices.map(([value, word]) => [value, word]),
    [
      ["ask", "On Accept"],
      ["checks", "After checks pass"],
      ["auto", "Right away"],
    ],
  );
  const running = rows.find((row) => row.key === "tasks.max_running");
  assert.equal(running.value, 1);
  assert.equal(running.layer, "you");
  assert.equal(running.control, "stepper");
  const tick = rows.find((row) => row.key === "tasks.tick_secs");
  assert.equal(tick.value, 15);
  assert.equal(tick.display, "15 seconds");
  assert.equal(rows.find((row) => row.key === "tasks.proof").control, "switch");
  assert.equal(rows.find((row) => row.key === "tasks.proof").display, "On");
  assert.equal(rows.find((row) => row.key === "tasks.archive_namespace").control, "box");
  assert.equal(rows.find((row) => row.key === "tasks.archive_after").control, "segmented");
  assert.equal(rows.find((row) => row.key === "review.cross_vendor").value, true);
  // Every key the fixture answers is on the page, so no workflow setting is
  // reachable only from the command line.
  const shown = new Set(rows.map((row) => row.key));
  for (const row of SETTINGS.values) {
    if (HIDDEN.includes(row.key)) continue;
    if (row.key.startsWith("tasks.") || row.key === "review.cross_vendor") {
      assert.ok(shown.has(row.key), `${row.key} is on the page`);
    }
  }
});

test("a workflow key the service did not answer is a row, not a hole", () => {
  const rows = workflowRows({ values: [] });
  assert.equal(rows.length, WORKFLOW.length);
  for (const row of rows) {
    assert.equal(row.value, null, row.key);
    assert.equal(row.layer, UNSET_LAYER, row.key);
  }
  assert.equal(rows.find((row) => row.key === "tasks.merge").display, "not set");
});

test("a stepper never proposes a value the service would refuse", () => {
  for (const [key, [min, max]] of Object.entries(RANGE)) {
    assert.equal(stepped(min, -1, key), min, `${key} stays at its floor`);
    assert.equal(stepped(max, 1, key), max, `${key} stays under its ceiling`);
    assert.equal(stepped(min + 1, -1, key), min);
  }
  assert.deepEqual(RANGE["tasks.max_running"], [1, 10]);
  assert.deepEqual(RANGE["tasks.tick_secs"], [1, 3600]);
  // A row's own bounds are the ones its stepper uses.
  const row = workflowRows(SETTINGS).find((entry) => entry.key === "tasks.tick_secs");
  assert.equal(stepped(row.value, 1, row.key), 16);
});

// -- the window itself -----------------------------------------------------

/**
 * A context the window's `init` can be given: no timers of its own, and a focus
 * handle that is only asked to take the keyboard.
 */
function context() {
  const after = [];
  return {
    cx: {
      // A clock that never runs out: `flush()`'s bound must not cut a test's
      // writes short, and nothing here needs a read to time out.
      sleep: () => new Promise(() => {}),
      notify: () => {},
      focus_handle: () => ({ focus: () => {} }),
      spawn: () => ({ cancel: () => {} }),
      timer: {
        after: (ms, fn) => {
          after.push({ ms, fn });
          return { cancel: () => {} };
        },
        every: () => ({ cancel: () => {} }),
      },
    },
    after,
  };
}

/** A window opened the way the footer opens it: `props().overlay` names it. */
function window_(props_ = {}) {
  herdr.reset();
  herdr.setProps({ overlay: "ov-1", ...props_ });
  const run = context();
  const view = new Settings();
  view.init(props_, run.cx);
  view.cx = run.cx;
  return { view, ...run };
}

/**
 * A window whose first look at the service is over, as it is in a test with no
 * service running: `init` starts a read, and the read's own failure must not
 * land in the middle of what a test is doing.
 */
async function settled(props_ = {}) {
  const opened = window_(props_);
  await new Promise((resolve) => setTimeout(resolve, 0));
  return opened;
}

/** A client that answers writes with what the service would, and records them. */
function client(reply = null, error = null) {
  const calls = [];
  const promptWrites = [];
  /** The words the service answers a prompt with, by id. */
  const texts = new Map([["worker", "shipped words"]]);
  return {
    calls,
    promptWrites,
    texts,
    putSetting: async (key, value, forRepo_) => {
      calls.push({ key, value, forRepo: forRepo_ });
      if (error) throw error;
      return reply ?? { outcome: "stored", settings: SETTINGS };
    },
    prompts: async () => PROMPTS,
    prompt: async (id) => ({
      id,
      text: texts.get(id) ?? "shipped words",
      state: "default",
      path: `/cfg/prompts/${id}.md`,
    }),
    resetPrompt: async (id) => ({ id, text: "shipped words", state: "default" }),
    putPrompt: async (id, text) => {
      promptWrites.push({ id, text });
      if (error) throw error;
      return { id, text, state: "edited", path: `/cfg/prompts/${id}.md` };
    },
    roster: async () => ROSTER,
    call: async () => SETTINGS,
  };
}

test("the window opens on the page it was asked for, and keeps to the ones it has", () => {
  assert.equal(window_({ page: "tasks" }).view.page, "tasks");
  assert.equal(window_({ page: "prompts" }).view.page, "prompts");
  assert.equal(window_({ page: "nope" }).view.page, "agents");
  assert.equal(window_({ page: "remote" }).view.page, "remote", "Remote is a page now");
  assert.equal(window_().view.scope, "you", "the person's own settings come first");
});

test("Esc closes what is open inside the window before it closes the window", async () => {
  const { view } = window_();
  view.renaming = "worker";
  await view.escape();
  assert.equal(view.renaming, null, "the first Esc leaves the name field");
  assert.deepEqual(herdr.CLOSED, [], "and leaves the window up");

  await view.escape();
  assert.deepEqual(herdr.CLOSED, ["ov-1"], "the next Esc closes the overlay it was given");

  // A prompt being edited is closed by Esc before the window is.
  const editing = window_({ page: "prompts" }).view;
  editing.prompt = { id: "worker", text: "mine", state: "edited", path: "/p/worker.md" };
  await editing.escape();
  assert.equal(editing.prompt, null);
  assert.deepEqual(herdr.CLOSED, []);
});

test("a write goes to the layer the level switch names", async () => {
  const { view } = await settled();
  view.settings = SETTINGS;
  view.client = client();
  await view.apply("tasks.max_running", 2);
  view.scope = "repo";
  await view.apply("tasks.max_running", 3);
  assert.deepEqual(view.client.calls, [
    { key: "tasks.max_running", value: 2, forRepo: false },
    { key: "tasks.max_running", value: 3, forRepo: true },
  ]);
  // Reset is the same write with nothing: the repository's value goes, and what
  // it inherits applies again.
  await view.apply("tasks.max_running", null);
  assert.deepEqual(view.client.calls[2], { key: "tasks.max_running", value: null, forRepo: true });
  assert.equal(view.saved.text, "Saved");
});

test("a refused write keeps the service's sentence, and writes nothing twice", async () => {
  const service = new ServiceError(
    "`roles.reviewer.agent` would be refused: worker role is anthropic, reviewer role is anthropic",
    { status: 409, code: "refused" },
  );
  const { view } = await settled();
  view.settings = SETTINGS;
  view.client = client(null, service);
  await view.apply("roles.reviewer.agent", "claude");
  assert.equal(view.refusal.key, "roles.reviewer.agent");
  assert.match(view.refusal.text, /both anthropic|worker role is anthropic/);
  assert.equal(view.refusal.text, reason(service), "the words are passed through, not summarised");
  assert.equal(view.writing, null, "the window is free to try another write");
});

test("opening the window again keeps the one copy that is up", async () => {
  herdr.reset();
  const first = openSettings({ page: "tasks" });
  assert.equal(herdr.OVERLAYS.length, 1, "one window");
  assert.equal(herdr.OVERLAYS[0].mod, "shprd-settings");
  assert.equal(settingsWindow().id, first);

  const again = openSettings();
  assert.equal(herdr.OVERLAYS.length, 1, "a second ask opens no second window");
  assert.equal(again, first);

  // A window that is gone — closed by the app, not by our own button — is
  // forgotten, and the next ask opens it again.
  settingsWindowClosed(first);
  assert.equal(settingsWindow(), null);
  openSettings();
  assert.equal(herdr.OVERLAYS.length, 2);
});

test("a window that stopped saying it is open is believed for twenty seconds only", () => {
  herdr.reset();
  const id = openSettings();
  assert.equal(settingsWindow().id, id);
  // The record the dialog renews on every read is stamped: one older than half a
  // minute is a window that went away without saying so, and it does not shut the
  // settings out of the rest of the app's run.
  herdr.setShared("shprd.settings.window", { id: "overlay-9", at: Date.now() - 21_000 });
  assert.equal(settingsWindow(), null);
  openSettings();
  assert.equal(herdr.OVERLAYS.length, 2);
});

test("closing the window gives its place back at once", async () => {
  herdr.reset();
  herdr.setProps({ overlay: "ov-7" });
  const run = context();
  const view = new Settings();
  view.init({}, run.cx);
  view.cx = run.cx;
  assert.equal(settingsWindow().id, "ov-7");
  await view.dismiss();
  assert.equal(settingsWindow(), null, "a dismissal is not a window that is still up");
  assert.deepEqual(herdr.CLOSED, ["ov-7"]);
  // And the app's own last word does the same for a copy it takes away itself.
  const second = window_({ overlay: "ov-8" }).view;
  assert.equal(settingsWindow().id, "ov-8");
  await second.close(run.cx);
  assert.equal(settingsWindow(), null);
});

// -- leaving the prompt editor ---------------------------------------------

/** A window on the Prompts page with the worker prompt open in its editor. */
async function editing() {
  const { view } = await settled({ page: "prompts" });
  view.settings = SETTINGS;
  view.client = client();
  await view.openPrompt({ id: "worker" });
  return view;
}

test("Esc on the Prompts page saves the words typed since the last pause", async () => {
  // The defect the review found: Esc closed the editor and the pending autosave
  // was cancelled by the close, so the last keystrokes vanished.
  const view = await editing();
  view.editor.set_value("the person's own words");
  // What a keystroke does: the change handler, and the debounce armed behind it.
  view.editor.handlers.get("change")?.({}, view.cx);

  await view.escape();

  assert.deepEqual(
    view.client.promptWrites,
    [{ id: "worker", text: "the person's own words" }],
    "the words reached the service before the editor went",
  );
  assert.equal(view.prompt, null, "and the editor is closed afterwards");
  // The debounce that was armed must not store them a second time.
  assert.equal(view.client.promptWrites.length, 1);
});

test("Back saves the words and keeps the editor closed as the write lands", async () => {
  // The other defect: a save that answered after the list was asked for put the
  // editor back, so a first Back press looked like it had done nothing.
  const view = await editing();
  view.editor.set_value("mine, and worth keeping");
  await view.leavePrompt();
  assert.equal(view.prompt, null, "one press leaves the list showing");
  assert.deepEqual(view.client.promptWrites, [{ id: "worker", text: "mine, and worth keeping" }]);
});

test("a prompt the service refused keeps the editor, and the next Esc closes the window", async () => {
  // The words are still only in the box, so closing would lose them: the editor
  // stays with the service's reason. The second Esc closes, so a broken service
  // cannot trap a person in the editor.
  const service = new ServiceError("`prompts.worker` could not be stored: the disk is full", {
    status: 500,
    code: "io",
  });
  const view = await editing();
  view.client = client(null, service);
  view.editor.set_value("words that will not store");
  await view.escape();
  assert.equal(view.prompt?.id, "worker", "the editor stays, with the words in it");
  assert.match(view.refusal.text, /the disk is full/);
  assert.deepEqual(herdr.CLOSED, [], "and the window is still up");

  await view.escape();
  assert.deepEqual(herdr.CLOSED, ["ov-1"], "the second Esc closes the window");
});

test("closing the window stores the last words before it goes", async () => {
  const view = await editing();
  view.editor.set_value("the last thing typed");
  await view.dismiss();
  assert.deepEqual(view.client.promptWrites, [{ id: "worker", text: "the last thing typed" }]);
  assert.deepEqual(herdr.CLOSED, ["ov-1"]);
});

// -- the record a window keeps of itself -----------------------------------

test("a window whose service is down still renews its place", async () => {
  // The other defect the review found: the record was renewed only after a read
  // answered, so with `shprd serve` down it went stale and the footer's next click
  // stacked a second window.
  const { view } = await settled();
  assert.equal(settingsWindow().id, "ov-1");
  herdr.setShared("shprd.settings.window", { id: "ov-1", at: Date.now() - 9_000 });
  await view.refresh();
  assert.ok(Date.now() - settingsWindow().at < 1_000, "the failed read renewed it anyway");

  openSettings();
  assert.equal(herdr.OVERLAYS.length, 0, "so a click keeps the one copy instead of stacking one");
});

// -- words typed while the window cannot write -----------------------------
//
// The save paths used to give up when the window had no client (a transient read
// timeout drops it while the stale page stays drawn) or when another write was in
// flight, and the next re-read then replaced the box's text with the service's
// value: the person's words were gone, with no refusal and nothing to retry. Every
// change is now queued and written when the service answers, and a re-read leaves a
// changed box alone.

/** A window on the Tasks page with the notes boxes drawn (as rendering makes them). */
async function tasksWindow() {
  const { view } = await settled({ page: "tasks" });
  view.settings = SETTINGS;
  view.client = client();
  view.resetBoxes();
  return view;
}

test("a note typed while the window has no client is written once one answers", async () => {
  const view = await tasksWindow();
  const box = view.boxes.get("tasks.archive_namespace");
  assert.ok(box, "the notes box exists");
  box.state.set_value("read the README first");

  // The service went away under the page: `refresh()` drops the client on a
  // timeout and leaves the page as it was.
  view.client = null;
  await view.writeBox("tasks.archive_namespace");
  assert.deepEqual(view.client?.calls ?? [], [], "nothing could be written");
  assert.deepEqual(view.queue.map((write) => write.key), ["tasks.archive_namespace"], "the words wait their turn");

  // A re-read that finds the service again writes them, instead of replacing them
  // with the value the service still holds.
  const live = client();
  view.client = live;
  view.resetBoxes();
  assert.equal(view.boxes.get("tasks.archive_namespace").state.value(), "read the README first");
  await view.pump();
  assert.deepEqual(live.calls, [
    { key: "tasks.archive_namespace", value: "read the README first", forRepo: false },
  ]);
  assert.equal(view.boxes.get("tasks.archive_namespace").saved, "read the README first");
});

test("a note typed while another write is in flight is queued, not dropped", async () => {
  const view = await tasksWindow();
  let release = null;
  const held = new Promise((resolve) => {
    release = resolve;
  });
  const live = client();
  const putSetting = live.putSetting;
  let started = null;
  const began = new Promise((resolve) => {
    started = resolve;
  });
  live.putSetting = async (key, value, forRepo_) => {
    if (key !== "tasks.max_running") return putSetting(key, value, forRepo_);
    // A write that takes its time: the first one has begun but not answered.
    started();
    await held;
    return putSetting(key, value, forRepo_);
  };
  view.client = live;

  const first = view.apply("tasks.max_running", 2);
  await began;
  view.boxes.get("tasks.archive_namespace").state.set_value("second, and not lost");
  // Not awaited: a box writes through the queue, and the queue is busy until the
  // write in front of it lands.
  const second = view.writeBox("tasks.archive_namespace");
  assert.equal(view.writing, "tasks.max_running", "the first write is the one in flight");
  assert.equal(live.calls.length, 0, "and it has not answered yet");
  assert.deepEqual(
    view.queue.map((write) => write.key),
    ["tasks.archive_namespace"],
    "the note is queued, not dropped",
  );

  release();
  await first;
  await second;
  assert.deepEqual(
    live.calls.map((call) => `${call.key}=${call.value}`),
    ["tasks.max_running=2", "tasks.archive_namespace=second, and not lost"],
    "and goes out as soon as the first one lands",
  );
});

test("a re-read never replaces words the service does not have yet", async () => {
  const view = await tasksWindow();
  const box = view.boxes.get("tasks.archive_namespace");
  box.state.set_value("typed, not stored");
  // The service's answer has moved on for other keys, but this box's words are
  // still only in the box: the re-read must leave them, and queue them.
  view.client = null;
  view.resetBoxes();
  assert.equal(box.state.value(), "typed, not stored", "the words are still there");
  assert.deepEqual(view.queue.map((write) => write.key), ["tasks.archive_namespace"]);
  // A box the service owns and the person has not touched does take its value.
  const archive = view.boxes.get("tasks.base");
  assert.equal(archive.state.value(), "");
});

test("Esc on the Prompts page says so when the words could not be stored", async () => {
  const view = await editing();
  view.editor.set_value("words typed while the service was away");
  // The client is dropped, as one timed-out read does.
  view.client = null;
  await view.escape();
  assert.ok(view.prompt, "the editor stays with the words in it");
  assert.match(view.refusal.text, /did not answer/);
  assert.equal(view.editor.value(), "words typed while the service was away");
  // And the next Esc closes the window rather than trapping the person.
  await view.escape();
  assert.deepEqual(herdr.CLOSED, ["ov-1"]);
});

test("a write whose service is back writes the queue before the page is redrawn", async () => {
  const view = await tasksWindow();
  view.boxes.get("tasks.archive_namespace").state.set_value("say what you tested");
  view.client = null;
  await view.writeBox("tasks.archive_namespace");
  const live = client();
  view.client = live;
  // What a successful read does before it resets the boxes.
  if (view.queue.length > 0) view.pump();
  await view.pump();
  assert.deepEqual(live.calls, [
    { key: "tasks.archive_namespace", value: "say what you tested", forRepo: false },
  ]);
});

test("a second change is queued behind the first instead of being dropped", async () => {
  const { view } = await settled();
  view.settings = SETTINGS;
  const live = client();
  view.client = live;
  await Promise.all([view.apply("tasks.max_running", 2), view.apply("tasks.max_running", 3)]);
  assert.deepEqual(live.calls, [
    { key: "tasks.max_running", value: 2, forRepo: false },
    { key: "tasks.max_running", value: 3, forRepo: false },
  ]);
});

test("a restarted service does not throw the editor's words away", async () => {
  // `forget()` runs when `attach` sees a new id in service.json. A prompt is a file
  // in the config directory, so the words typed over it are not that run's to drop:
  // the editor stays, and the next read — from the service that is back — stores
  // them.
  const view = await editing();
  view.editor.set_value("words from the run that went away");
  view.forget();
  assert.equal(view.prompt?.id, "worker", "the editor stays with its words");
  // What a read that answers does with them (a unit test has no service for
  // `refresh()` to find, so it drives the same branch the read runs).
  const live = client();
  view.client = live;
  view.forget();
  await view.retryWrites();
  assert.deepEqual(live.promptWrites, [
    { id: "worker", text: "words from the run that went away" },
  ]);
});

test("a restarted service closes an editor with nothing typed in it", async () => {
  const view = await editing();
  view.forget();
  assert.equal(view.prompt, null, "nothing typed, nothing kept");
});

test("a refusal is the person's to read, not retried behind their back", async () => {
  const service = new ServiceError("`prompts.worker` could not be stored: read-only file", {
    status: 500,
    code: "io",
  });
  const view = await editing();
  view.client = client(null, service);
  view.editor.set_value("words the service refused");
  await view.escape();
  assert.ok(view.prompt, "the editor keeps them");
  assert.ok(view.client.promptWrites.length > 0, "leaving tried to store them");
  // The next read that answers must leave a refused prompt alone: only a transport
  // failure is the window's to retry behind the person.
  const live = client();
  view.client = live;
  await view.retryWrites();
  assert.equal(live.promptWrites.length, 0, "a refusal is not re-sent by a read");
});

// -- SHPR-22: the settings dialog completed -----------------------------------

test("review rounds may be Off, and Off hides what a review must fix", () => {
  assert.deepEqual(RANGE["tasks.review_rounds"], [0, 10]);
  assert.equal(stepped(0, -1, "tasks.review_rounds"), 0);
  const off = { values: SETTINGS.values.map((row) => (row.key === "tasks.review_rounds" ? { ...row, value: 0 } : row)) };
  const rows = workflowRows(off);
  assert.equal(rows.find((row) => row.key === "tasks.review_rounds").display, "Off");
  assert.equal(rows.some((row) => row.key === "tasks.must_fix"), false, "nothing is reviewed, so nothing must be fixed");
  const on = workflowRows(SETTINGS);
  assert.ok(on.some((row) => row.key === "tasks.must_fix"));
  assert.equal(on.find((row) => row.key === "tasks.review_rounds").display, "2");
});

test("archive is a row of presets, and a value set by hand is still a segment", () => {
  assert.deepEqual(ARCHIVE_PRESETS.map(([value]) => value), [0, 1, 3, 7, 30]);
  const row = (value) =>
    workflowRows({ values: [{ key: "tasks.archive_after", value, source: "you" }] }).find((entry) => entry.key === "tasks.archive_after");
  assert.equal(row(7).display, "1 week");
  assert.equal(row(0).display, "Off");
  assert.deepEqual(row(7).choices.length, 5);
  const odd = row(12);
  assert.equal(odd.choices.at(-1)[0], 12);
  assert.equal(odd.display, "12 days");
});

test("the two notes are rows now that a brief carries them", () => {
  assert.deepEqual(HIDDEN, []);
  const rows = workflowRows(SETTINGS);
  for (const key of ["tasks.start_notes", "tasks.finish_notes"]) {
    const row = rows.find((entry) => entry.key === key);
    assert.ok(row, `${key} is drawn`);
    assert.equal(row.control, "box");
    assert.equal(row.kind, "notes");
  }
});

test("a branch pattern git would refuse is refused with a sentence", () => {
  assert.equal(branchProblem("task/{n}-{slug}"), null);
  assert.equal(branchProblem("feature/{slug}"), null);
  assert.equal(branchProblem(""), null, "empty means the default");
  assert.equal(branchProblem("{run}/{task}"), null);
  assert.match(branchProblem("task/{n}"), /{task} or {slug}/);
  assert.match(branchProblem("task/fixed"), /{task} or {slug}/);
  assert.match(branchProblem("{run}/x"), /{task} or {slug}/);
  assert.match(branchProblem("task/{id}"), /only {run}, {task}, {n} and {slug}/);
  assert.match(branchProblem("task/{slug} x"), /spaces/);
  assert.match(branchProblem("task//{slug}"), /\//);
  assert.match(branchProblem("task/{slug}.lock"), /\.lock/);
  assert.match(branchProblem("task/..{slug}"), /\.\./);
  assert.equal(boxProblem("tasks.base", "integration/m11"), null);
  assert.match(boxProblem("tasks.base", "bad name"), /spaces/);
  assert.equal(boxProblem("tasks.archive_namespace", "anything at all"), null);
});

test("a branch box never writes a name git would refuse", async () => {
  const view = await tasksWindow();
  const box = view.boxes.get("tasks.branch");
  box.state.set_value("task/fixed");
  await view.writeBox("tasks.branch");
  assert.deepEqual(view.client.calls, [], "nothing was sent");
  assert.equal(view.refusal.key, "tasks.branch");
  assert.match(view.refusal.text, /{task} or {slug}/);
  box.state.set_value("work/{n}-{slug}");
  await view.writeBox("tasks.branch");
  assert.deepEqual(view.client.calls, [{ key: "tasks.branch", value: "work/{n}-{slug}", forRepo: false }]);
});

test("Fast and Permissions are offered where the program has them", () => {
  const list = roles(SETTINGS, ROSTER);
  const worker = list.find((entry) => entry.id === "worker");
  const reviewer = list.find((entry) => entry.id === "reviewer");
  assert.equal(worker.fast, null, "pi has no fast mode, so no dead switch");
  assert.equal(reviewer.fast.key, "roles.reviewer.fast", "claude has one");
  assert.equal(reviewer.fast.value, false);
  assert.deepEqual(
    reviewer.perm.choices.map((choice) => choice.value),
    [null, "manual", "auto", "acceptEdits", "plan", "bypassPermissions"],
  );
  assert.match(reviewer.perm.display, /Ask/);
  assert.equal(worker.fallback, undefined, "no separate Fallback control: workers and reviewers hold lines");
  assert.equal(reviewer.fallback, undefined);
});

test("no Backup agent: its role is gone, and workers and reviewers hold a chain of lines", () => {
  assert.equal(roles(SETTINGS, ROSTER).find((entry) => entry.id === "backup"), undefined);
  assert.ok(!BUILTIN.includes("backup"));
  assert.ok(hasLines("worker") && hasLines("reviewer") && !hasLines("coordinator"));
  assert.match(CHAIN_NOTE, /first line is the default/);
  assert.equal(SELECT_HARNESS, "Select Harness");
  assert.equal(promptName("backup"), "Takeover");
});

test("a role's fast, permission and lines writes go to its own keys", async () => {
  const { view } = await settled();
  view.settings = SETTINGS;
  view.roster = ROSTER;
  view.client = client();
  await view.apply("roles.reviewer.fast", true);
  await view.apply("roles.reviewer.perm", "plan");
  await view.apply("roles.reviewer.fallback", ["codex"]);
  assert.deepEqual(view.client.calls, [
    { key: "roles.reviewer.fast", value: true, forRepo: false },
    { key: "roles.reviewer.perm", value: "plan", forRepo: false },
    { key: "roles.reviewer.fallback", value: ["codex"], forRepo: false },
  ]);
});

test("the remote page's words: a masked link, ages, devices and the tunnel", () => {
  assert.equal(maskLink("https://abc123.trycloudflare.com/x?k=secret"), "https://••••••.trycloudflare.com/••••");
  assert.ok(!maskLink("https://abc123.trycloudflare.com/x?k=secret").includes("secret"));
  assert.equal(maskLink("nonsense"), "••••••••");
  assert.equal(ago(100, 130), "just now");
  assert.equal(ago(100, 100 + 5 * 60), "5 min ago");
  assert.equal(ago(100, 100 + 3 * 3600), "3 h ago");
  assert.equal(ago(100, 100 + 2 * 86_400), "2 d ago");
  const rows = sessionRows(
    { sessions: [{ id: "a1", label: "Phone", last_seen: 90, remember: true }, { id: "b2", label: "Laptop", last_seen: 0, remember: false }], current: "b2" },
    100,
  );
  assert.deepEqual(rows.map((row) => [row.id, row.here, row.remembered]), [["a1", false, true], ["b2", true, false]]);
  const status = {
    on: false, starting: false, why: "cloudflared exited", password_set: false, default_provider: "tailscale",
    providers: [{ id: "tailscale", public_internet: false }, { id: "ngrok", public_internet: true }],
  };
  assert.equal(tunnelView(status).provider, "tailscale");
  assert.equal(tunnelView(status).needsPassword, false);
  const pub = tunnelView(status, "ngrok");
  assert.equal(pub.needsPassword, true);
  assert.match(pub.note, /shprd web password set/);
  assert.equal(pub.why, "cloudflared exited");
  assert.equal(pub.word, "Off");
  const up = tunnelView({ ...status, on: true, link: "https://x.example/y", provider: "ngrok" });
  assert.equal(up.word, "On");
  assert.equal(up.link, "https://x.example/y");
  assert.equal(up.why, null);
});

/** A client for the Remote page: what each route answers, and every call made. */
function remoteClient(answers) {
  const calls = [];
  return {
    calls,
    putSetting: async () => ({ outcome: "stored", settings: SETTINGS }),
    call: async (method, path, options = {}) => {
      calls.push({ method, path, body: options.body ?? null });
      const answer = answers[`${method} ${path}`] ?? answers[path];
      if (answer instanceof Error) throw answer;
      return answer ?? null;
    },
  };
}

test("the Remote page reads the tunnel and the devices, and says when a route is missing", async () => {
  const { view } = await settled({ page: "remote" });
  view.client = remoteClient({
    "/api/tunnels": { on: false, starting: false, at: 1, password_set: true, default_provider: "tailscale", providers: [] },
    "/api/sessions": new ServiceError("not found", { status: 404 }),
  });
  await view.readRemote();
  assert.equal(view.tunnelRead.data.default_provider, "tailscale");
  assert.deepEqual(view.sessionRead, { unavailable: true });
});

test("Start and Stop go through the service, and its refusal is shown word for word", async () => {
  const { view } = await settled({ page: "remote" });
  const up = { on: true, starting: false, at: 2, password_set: true, default_provider: "tailscale", providers: [], link: "https://x.example", provider: "tailscale" };
  view.client = remoteClient({ "POST /api/tunnels/start": up, "POST /api/tunnels/stop": { ...up, on: false, link: undefined } });
  view.tunnelRead = { data: { ...up, on: false, providers: [{ id: "tailscale", public_internet: false }] } };
  view.provider = "tailscale";
  await view.tunnelCall("start");
  assert.deepEqual(view.client.calls[0], { method: "POST", path: "/api/tunnels/start", body: { provider: "tailscale" } });
  assert.equal(view.tunnelRead.data.on, true);
  assert.equal(view.remoteBusy, null);
  await view.tunnelCall("stop");
  assert.equal(view.client.calls[1].path, "/api/tunnels/stop");
  assert.equal(view.tunnelRead.data.on, false);
  const refusal = new ServiceError("a public tunnel needs a password", { status: 409, code: "refused" });
  view.client = remoteClient({ "POST /api/tunnels/start": refusal });
  await view.tunnelCall("start");
  assert.equal(view.refusal.text, reason(refusal));
});

test("Revoke ends one device and Log out all ends every one", async () => {
  const { view } = await settled({ page: "remote" });
  const list = { sessions: [{ id: "a1", label: "Phone" }, { id: "b2", label: "Laptop" }], current: "b2" };
  view.client = remoteClient({ "/api/sessions": list, "/api/tunnels": { on: false, providers: [] } });
  view.sessionRead = { data: list };
  await view.revokeSession("a1");
  assert.deepEqual(view.client.calls[0], { method: "DELETE", path: "/api/sessions/a1", body: null });
  view.client.calls.length = 0;
  await view.logoutAll();
  assert.deepEqual(
    view.client.calls.filter((call) => call.method === "DELETE").map((call) => call.path),
    ["/api/sessions/a1", "/api/sessions/b2"],
  );
  assert.equal(view.remoteBusy, null);
});

test("every page, Remote included, draws without throwing", async () => {
  for (const page of ["agents", "tasks", "prompts", "remote"]) {
    const { view } = await settled({ page });
    view.settings = SETTINGS;
    view.roster = ROSTER;
    view.prompts = PROMPTS;
    view.tunnelRead = { data: { on: true, starting: false, at: 1, password_set: true, default_provider: "tailscale", link: "https://x.example/y", provider: "tailscale", providers: [{ id: "tailscale", public_internet: false }] } };
    view.sessionRead = { data: { sessions: [{ id: "a1", label: "Phone", last_seen: 1, remember: true }], current: "a1" } };
    view.page = page;
    const t = herdr.theme();
    const colors = tone(t);
    assert.ok(view.nav(t, colors), `${page}: the page list`);
    assert.ok(view.pageBody(t, colors).length > 0, page);
    view.revealed = true;
    assert.ok(view.pageBody(t, colors).length > 0, `${page} with the link showing`);
  }
});
