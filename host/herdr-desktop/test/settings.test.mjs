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
import { readFileSync } from "node:fs";
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
  refused,
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
  assert.deepEqual(roleIds(SETTINGS), ["coordinator", "worker", "reviewer", "backup", "docs"]);
  assert.deepEqual(
    roles(SETTINGS, ROSTER).map((entry) => entry.title),
    ["Assistant", "Worker", "Reviewer", "Backup agent", "docs"],
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
  assert.equal(pick("backup", "model").value, null);
});

test("a program is shown by its name, and written by its id", () => {
  const agent = pick("worker", "agent");
  assert.equal(agent.key, "roles.worker.agent");
  assert.equal(agent.value, "pi");
  assert.equal(agent.display, "Pi");
  assert.equal(agent.note, null);
  const backup = pick("backup", "agent");
  assert.equal(backup.value, "codex");
  assert.equal(backup.display, "Codex");
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
  assert.equal(choices.length, 4);
  assert.deepEqual(choices[0], { value: null, label: "The program's default", hint: null });
  assert.deepEqual(
    choices.slice(1).map((choice) => choice.value),
    ["fable", "opus", "sonnet"],
  );
  assert.equal(choices[2].label, "Opus");

  const model = pick("coordinator", "model");
  assert.equal(model.value, "opus");
  assert.equal(model.display, "opus");
  assert.equal(model.source, "you");
  assert.equal(model.choices.length, 4);
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
    "backup",
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
    "backup",
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
    assert.equal(row.word, "Shipped");
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
  assert.equal(edited[1].word, "Shipped");
  assert.equal(edited[1].about, "a prompt of your own");
  assert.equal(edited[2].word, "Unreadable");
  assert.equal(edited[2].state, null);
  assert.match(edited[2].problem, /cannot read/);
});

test("the editor says what saving and resetting do", () => {
  assert.match(promptLine({ state: "edited" }), /never replaced by an upgrade/);
  assert.match(promptLine({ state: "default" }), /saving your own stores them/);
  // What a save just answered wins over what was read before it.
  assert.match(promptLine({ state: "edited" }, { state: "default" }), /shipped words/);
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
    ["files", "changes", "tasks", "asks"],
    "settings opens over the window, so it is not a stack the panel switches to",
  );
  assert.equal(node("settings"), null);
  // And the stack's own selection still names a child that is there.
  assert.ok(panel.children.some((child) => child.id === panel.active), panel.active);
});

test("the pages are the three shprd has, in the window's order, and Remote is a place held", () => {
  assert.deepEqual(
    PAGES.map((page) => page.title),
    ["Agents", "Tasks", "Prompts", "Remote"],
  );
  assert.equal(TITLE, "Settings");
  // Remote is drawn and inert: it is the page M11.7 gives it, and opening it
  // must never land on a page that has nothing behind it.
  const remote = PAGES.find((page) => page.id === "remote");
  assert.equal(remote.soon, true);
  assert.equal(pageOf("remote").id, "agents");
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
      ["ask", "Ask me"],
      ["checks", "Checks pass"],
      ["auto", "By itself"],
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
  assert.equal(rows.find((row) => row.key === "tasks.start_notes").control, "box");
  assert.equal(rows.find((row) => row.key === "review.cross_vendor").value, true);
  // Every key the fixture answers is on the page, so no workflow setting is
  // reachable only from the command line.
  const shown = new Set(rows.map((row) => row.key));
  for (const row of SETTINGS.values) {
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
      sleep: async () => {},
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
  return {
    calls,
    putSetting: async (key, value, forRepo_) => {
      calls.push({ key, value, forRepo: forRepo_ });
      if (error) throw error;
      return reply ?? { outcome: "stored", settings: SETTINGS };
    },
    prompts: async () => PROMPTS,
    prompt: async () => null,
    resetPrompt: async () => null,
    putPrompt: async () => null,
    roster: async () => ROSTER,
    call: async () => SETTINGS,
  };
}

test("the window opens on the page it was asked for, and keeps to the ones it has", () => {
  assert.equal(window_({ page: "tasks" }).view.page, "tasks");
  assert.equal(window_({ page: "prompts" }).view.page, "prompts");
  assert.equal(window_({ page: "nope" }).view.page, "agents");
  assert.equal(window_({ page: "remote" }).view.page, "agents", "Remote is a place held, not a page");
  assert.equal(window_().view.scope, "you", "the person's own settings come first");
});

test("Esc closes what is open inside the window before it closes the window", () => {
  const { view } = window_();
  view.open = "worker.agent";
  view.escape();
  assert.equal(view.open, null, "the first Esc closes the open picker");
  assert.deepEqual(herdr.CLOSED, [], "and leaves the window up");

  view.escape();
  assert.deepEqual(herdr.CLOSED, ["ov-1"], "the next Esc closes the overlay it was given");

  // A prompt being edited is closed by Esc before the window is.
  const editing = window_({ page: "prompts" }).view;
  editing.prompt = { id: "worker", text: "mine", state: "edited", path: "/p/worker.md" };
  editing.escape();
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
  view.dismiss();
  assert.equal(settingsWindow(), null, "a dismissal is not a window that is still up");
  assert.deepEqual(herdr.CLOSED, ["ov-7"]);
  // And the app's own last word does the same for a copy it takes away itself.
  const second = window_({ overlay: "ov-8" }).view;
  assert.equal(settingsWindow().id, "ov-8");
  await second.close(run.cx);
  assert.equal(settingsWindow(), null);
});
