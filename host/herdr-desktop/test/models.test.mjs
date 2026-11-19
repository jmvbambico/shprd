// The Settings dropdowns, driven the way the window drives them: a click goes to
// the element pressed and then up through every ancestor until one stops it.
//
//   node --import ./test/boot.mjs --test test/
//
// SHPR-43: every picker button in the dialog used to leave its click to bubble to
// the panel's own handler, which put the list away in the same click, so no
// dropdown could be seen to open. The generic test here clicks every clickable
// element of every page and asks that each one takes its click; the rest drive the
// popup mod, the inline rename and the restart notice.

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import * as herdr from "herdr";
import { record, walk, click } from "gpui-kit";

import Settings from "../mods/shprd-settings/main.js";
import ModelMenu from "../mods/shprd-models/main.js";
import {
  PICK_KEY,
  chooseModel,
  coordinatorRunning,
  detectedPrograms,
  levelsFor,
  menusOf,
  modelRows,
  restartNotice,
  fallbackLabel,
  fallbackWrite,
  lineLabel,
  lineWrites,
  menuHeight,
  nextLineProgram,
  parseLine,
  encodeLine,
  selectionLabel,
  selectionWrites,
  withLine,
  workerLines,
} from "../mods/lib/settings.js";

const fixture = (name) =>
  JSON.parse(readFileSync(fileURLToPath(new URL(`./fixtures/${name}`, import.meta.url)), "utf8"));
const SETTINGS = fixture("settings.json");
const ROSTER = fixture("roster.json");
const PROMPTS = fixture("prompts.json");

function context() {
  const after = [];
  return {
    cx: {
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

/** A window with the service's answers already in it, drawn into a recorded tree. */
async function drawn(page = "agents", extra = {}) {
  herdr.reset();
  herdr.setProps({ overlay: "ov-1" });
  record(true);
  const run = context();
  const view = new Settings();
  view.init({ page }, run.cx);
  view.cx = run.cx;
  await new Promise((resolve) => setTimeout(resolve, 0));
  view.failure = null;
  view.settings = SETTINGS;
  view.roster = ROSTER;
  view.prompts = PROMPTS;
  view.loading = true; // no read of a service that is not there
  view.loadedAt = Date.now();
  view.page = page;
  const calls = [];
  view.client = {
    calls,
    putSetting: async (key, value, forRepo) => {
      calls.push({ key, value, forRepo });
      return { outcome: "stored", settings: SETTINGS };
    },
    restartCoordinator: async () => {
      calls.push({ restart: true });
    },
    prompt: async (id) => ({ id, text: "words", state: "default", path: `/p/${id}.md` }),
    prompts: async () => PROMPTS,
    call: async () => ({}),
  };
  Object.assign(view, extra);
  return { view, calls, ...run, tree: () => view.render() };
}

test.afterEach(() => record(false));

test("every clickable control of every page takes its click (the cause of the dead dropdowns)", async () => {
  for (const page of ["agents", "tasks", "prompts", "remote"]) {
    const { tree } = await drawn(page);
    const root = tree();
    const ids = walk(root).filter((node) => node.click && node.idv).map((node) => node.idv);
    assert.ok(ids.length > 0, `${page} has clickable controls`);
    for (const id of ids) {
      // the page list, the panel itself and the backdrop are not controls
      if (id === "shprd-settings" || id === "shprd-settings-root") continue;
      const { stopped } = click(root, id);
      assert.ok(stopped, `${page}: ${id} must stop its click, or the panel's own handler runs with it`);
    }
  }
});

test("a click on a role's model button opens the menu popup, and nothing else closes it", async () => {
  const { view, tree } = await drawn("agents");
  const root = tree();
  // the button remembers where it was pressed, as the window's mouse down tells it
  view.rec("model-coordinator").at = { x: 600, y: 200, width: 120, height: 28 };
  const { stopped } = click(root, "model-coordinator");
  assert.ok(stopped);
  assert.equal(herdr.OVERLAYS.length, 1);
  const popup = herdr.OVERLAYS[0];
  assert.equal(popup.mod, "shprd-models");
  assert.equal(popup.kind, "popup");
  assert.equal(popup.props.mode, "models");
  assert.equal(popup.props.key, "role:coordinator");
  assert.deepEqual(herdr.CLOSED, [], "the click that opened it did not close it");
});

test("the model menu's tabs are the installed programs only", () => {
  const roster = structuredClone(ROSTER);
  const hidden = roster.programs.find((program) => program.id === "pi");
  hidden.installed = false;
  const menus = menusOf(roster);
  assert.ok(!menus.order.includes("pi"), "an undetected program has no tab");
  assert.deepEqual(menus.order, detectedPrograms(roster).map((program) => program.id));
  assert.ok(menus.order.includes("claude"));
});

test("a pick writes program, model and level in order, and none of what did not change", () => {
  const menus = menusOf(ROSTER);
  const before = { agent: "claude", model: "opus", effort: "high", fast: false, perm: null };
  const after = chooseModel(menus, before, "claude", "sonnet");
  assert.deepEqual(selectionWrites("worker", before, after).map((w) => w.key), ["roles.worker.model"]);
  assert.equal(selectionLabel(menus, { ...before, effort: "high" }, ROSTER).includes("High"), true);
  assert.deepEqual(modelRows(menus, "codex", "gpt-6-luna"), [
    { agent: "codex", id: "gpt-6-luna", name: "Use gpt-6-luna", typed: true },
  ]);
  assert.ok(levelsFor(menus, before).length > 0);
});

test("the model menu answers each change through the shared key", async () => {
  herdr.reset();
  record(true);
  const menus = menusOf(ROSTER);
  herdr.setProps({
    mode: "models",
    key: "role:worker",
    overlay: "ov-9",
    current: { agent: "claude", model: "opus", effort: null, fast: false, perm: null },
    menus,
    icons: {},
    width: 460,
    height: 400,
  });
  const run = context();
  const menu = new ModelMenu();
  menu.init({}, run.cx);
  const root = menu.render();
  assert.ok(click(root, "m-claude-sonnet").stopped);
  let reply = herdr.shared(PICK_KEY);
  assert.equal(reply.key, "role:worker");
  assert.equal(reply.field, "selection");
  assert.equal(reply.value.model, "sonnet");
  const level = walk(menu.render()).find((node) => node.idv?.startsWith("level-"));
  assert.ok(level, "Reasoning offers the levels the model takes");
  assert.ok(click(menu.render(), level.idv.replace("level-", "level-")).stopped);
  reply = herdr.shared(PICK_KEY);
  assert.ok(reply.value.effort, "a level was picked");
  assert.deepEqual(herdr.CLOSED, [], "picking keeps the menu up");
});

test("inline rename: click opens the field, Enter saves, Esc cancels, blank is the built-in name", async () => {
  const { view, calls, after } = await drawn("agents");
  const role = (await import("../mods/lib/settings.js")).roles(SETTINGS, ROSTER).find((r) => r.id === "worker");
  view.startRename(role);
  assert.equal(view.renaming, "worker");
  const box = view.boxes.get("roles.worker.title");
  assert.equal(box.state.value(), role.title, "the field opens on the name shown");
  after.length = 0;

  // Esc: the old name is back and nothing is written
  box.state.set_value("Nope");
  view.cancelRename();
  assert.equal(view.renaming, null);
  assert.equal(box.state.value(), role.title);
  await view.writeBox("roles.worker.title");
  assert.equal(calls.length, 0, "Esc wrote nothing");

  // Enter: the trimmed name is written and the field closes
  view.startRename(role);
  box.state.set_value("  Dev  team ");
  box.state.handlers.get("submit")({}, {});
  await view.pump();
  assert.equal(view.renaming, null);
  assert.deepEqual(calls.at(-1), { key: "roles.worker.title", value: "Dev team", forRepo: false });

  // blank: the built-in name comes back (an empty write)
  view.startRename(role);
  box.state.set_value("   ");
  box.state.handlers.get("submit")({}, {});
  await view.pump();
  assert.deepEqual(calls.at(-1), { key: "roles.worker.title", value: "", forRepo: false });
});

test("the restart notice shows only after the coordinator's own choice changed, while one runs", async () => {
  const { view, tree } = await drawn("agents", { coordRunning: true });
  const has = () => walk(tree()).some((node) => node.idv === "restart-coordinator");
  assert.equal(has(), false, "not a permanent row");
  // a worker change does not raise it
  await view.apply("roles.worker.model", "opus");
  assert.equal(has(), false);
  // the coordinator's does
  await view.apply("roles.coordinator.model", "opus");
  assert.equal(has(), true);
  // restarting puts it away, and its answer shows on the coordinator's row alone
  const notices = () => walk(tree()).filter((node) => node.idv === "restart-notice").length;
  assert.equal(notices(), 1);
  const pending = view.restartCoordinator();
  await pending;
  assert.equal(has(), false);
  assert.equal(notices(), 1, "Restarted shows on the coordinator only");

  // no coordinator running: a change never raises it
  const idle = await drawn("agents", { coordRunning: false });
  await idle.view.apply("roles.coordinator.model", "opus");
  assert.equal(walk(idle.tree()).some((node) => node.idv === "restart-coordinator"), false);

  assert.equal(restartNotice({ changed: true, running: true }), true);
  assert.equal(restartNotice({ changed: true, running: false }), false);
  assert.equal(coordinatorRunning({ live_agents: [] }), false);
  assert.equal(coordinatorRunning({ live_agents: [{ role: "coordinator", state: "working" }] }), true);
});

test("a refused later write puts back what the choice already wrote", async () => {
  const { view, calls } = await drawn("agents");
  view.settings = structuredClone(SETTINGS);
  const before = view.settings.values.find((v) => v.key === "roles.worker.agent");
  before.source = "you";
  const held = before.value;
  view.client.putSetting = async (key, value, forRepo) => {
    calls.push({ key, value, forRepo });
    if (key === "roles.worker.model" && value === "bad-id") throw new Error("that model is not one codex has");
    return { outcome: "stored", settings: view.settings };
  };
  await view.applyBatch([
    { key: "roles.worker.agent", value: "codex" },
    { key: "roles.worker.model", value: "bad-id" },
  ]);
  await view.pump();
  assert.deepEqual(
    calls.map((c) => [c.key, c.value]),
    [
      ["roles.worker.agent", "codex"],
      ["roles.worker.model", "bad-id"],
      ["roles.worker.agent", held],
    ],
    "the program is put back after the model was refused",
  );
  assert.match(view.refusal.text, /not one codex has/, "the refusal still shows");
});

test("an undo the service refuses is said, not swallowed", async () => {
  const { view, calls } = await drawn("agents");
  view.settings = structuredClone(SETTINGS);
  view.settings.values.find((v) => v.key === "roles.worker.agent").source = "you";
  view.client.putSetting = async (key, value, forRepo) => {
    calls.push({ key, value, forRepo });
    if (key === "roles.worker.model") throw new Error("that model is not one codex has");
    if (calls.length === 3) throw new Error("the worker and the reviewer share a vendor");
    return { outcome: "stored", settings: view.settings };
  };
  await view.applyBatch([
    { key: "roles.worker.agent", value: "codex" },
    { key: "roles.worker.model", value: "bad-id" },
  ]);
  await view.pump();
  assert.match(view.refusal.text, /not one codex has/);
  assert.match(view.refusal.text, /putting roles\.worker\.agent back was refused too: the worker and the reviewer share a vendor/);
});

test("a typed word that is already a model's name is not offered as a new id", () => {
  const menus = menusOf(ROSTER);
  const first = menus.programs.claude.models[0];
  assert.deepEqual(modelRows(menus, "claude", first.name).filter((r) => r.typed), []);
});

test("a refused write drops the rest of its choice and closes the popup", async () => {
  const { view, calls } = await drawn("agents");
  const popup = herdr.openOverlay({ mod: "shprd-models", kind: "popup" });
  view.popup = popup;
  view.client.putSetting = async (key, value, forRepo) => {
    calls.push({ key, value, forRepo });
    throw new Error("the reviewer and the worker share a vendor");
  };
  await view.applyBatch([
    { key: "roles.reviewer.agent", value: "claude" },
    { key: "roles.reviewer.model", value: "opus" },
  ]);
  assert.equal(calls.length, 1, "the model after a refused program is not sent");
  assert.match(view.refusal.text, /share a vendor/);
  assert.deepEqual(herdr.CLOSED, [popup]);
  assert.equal(view.popup, null);
});

// -- SHPR-44: several worker programs, a reviewer fallback, a program dropdown ----

/** The fixture's settings with these rows set (a row replaces the one of its key). */
function withRows(rows) {
  const document = structuredClone(SETTINGS);
  for (const [key, value, source = "you"] of rows) {
    const row = document.values.find((entry) => entry.key === key);
    if (row) Object.assign(row, { value, source });
    else document.values.push({ key, value, source });
  }
  return document;
}

test("a stored line reads and writes back as the same line", () => {
  const line = { agent: "codex", model: "gpt-6-luna", effort: "high", fast: true, perm: "auto" };
  assert.deepEqual(parseLine(encodeLine(line)), line);
  assert.deepEqual(parseLine("pi/commandcode/z-ai/glm"), { agent: "pi", model: "commandcode/z-ai/glm", effort: null, fast: false, perm: null });
  assert.deepEqual(parseLine("kilo"), { agent: "kilo", model: null, effort: null, fast: false, perm: null });
  assert.equal(parseLine("|model"), null);
  assert.equal(parseLine(""), null);
});

test("the worker's lines are its own pick, then agents; the old fallback reads as lines too", () => {
  assert.deepEqual(workerLines(SETTINGS).map((line) => line.agent), ["pi"]);
  const two = withRows([["roles.worker.agents", ["codex|gpt-6-luna||1|auto", "claude"]]]);
  const lines = workerLines(two);
  assert.deepEqual(lines.map((line) => line.agent), ["pi", "codex", "claude"]);
  assert.equal(lines[1].fast, true);
  assert.equal(lines[1].perm, "auto");
  // A file written before lines existed: the fallback programs follow the first.
  const old = withRows([["roles.worker.fallback", ["codex", "claude"]]]);
  assert.deepEqual(workerLines(old).map((line) => line.agent), ["pi", "codex", "claude"]);
  // A program is listed once.
  const twice = withRows([["roles.worker.agents", ["pi", "codex"]]]);
  assert.deepEqual(workerLines(twice).map((line) => line.agent), ["pi", "codex"]);
});

test("a line label leads with the program, then the model and the level", () => {
  const menus = menusOf(ROSTER);
  assert.equal(lineLabel(menus, { agent: "claude", model: "haiku", effort: "high" }, ROSTER), "Claude Code · Haiku 5.5 · High");
  assert.equal(lineLabel(menus, { agent: "codex", model: null, effort: null }, ROSTER), "Codex");
  assert.equal(lineLabel(menus, { agent: null }, ROSTER), "Choose a program");
  assert.equal(fallbackLabel(menus, null, ROSTER), "None");
  assert.equal(fallbackLabel(menus, { agent: "claude", model: "sonnet" }, ROSTER), "Claude Code · Sonnet 5.5");
});

test("adding a line writes agents with the next free program; removing one writes it back", () => {
  const next = nextLineProgram(SETTINGS, ROSTER);
  assert.equal(next, "claude", "pi has a line, claude is the first program that has none");
  const added = [...workerLines(SETTINGS), { agent: next, model: null, effort: null, fast: false, perm: null }];
  assert.deepEqual(lineWrites(SETTINGS, "worker", added).map((w) => [w.key, w.value]), [
    ["roles.worker.agents", ["claude|||"+"|"]],
  ]);
  const doc = withRows([["roles.worker.agents", ["claude|||", "codex|||"]]]);
  const lines = workerLines(doc);
  const removed = withLine(lines, 1, null);
  assert.deepEqual(lineWrites(doc, "worker", removed).map((w) => [w.key, w.value]), [
    ["roles.worker.agents", ["codex||||"]],
  ]);
  // Removing the first line promotes the next into the role's own fields.
  const promoted = lineWrites(doc, "worker", withLine(lines, 0, null)).map((w) => [w.key, w.value]);
  assert.deepEqual(promoted[0], ["roles.worker.agent", "claude"]);
  assert.deepEqual(promoted.at(-1), ["roles.worker.agents", ["codex||||"]]);
  // The old fallback list is emptied by the first edit, so it cannot come back.
  const old = withRows([["roles.worker.fallback", ["codex"]]]);
  const writes = lineWrites(old, "worker", withLine(workerLines(old), 1, null));
  assert.deepEqual(writes.at(-1), { key: "roles.worker.fallback", value: [], prior: ["codex"] });
  // No program left to add: no "+".
  const all = withRows([["roles.worker.agents", ["claude", "codex", "kilo", "opencode"]]]);
  assert.equal(nextLineProgram(all, ROSTER), null);
});

test("the reviewer's fallback is one program and model; None writes an empty list", () => {
  const writes = fallbackWrite(SETTINGS, "reviewer", { agent: "codex", model: "gpt-6-luna" });
  assert.deepEqual(writes.map((w) => [w.key, w.value]), [["roles.reviewer.fallback", ["codex/gpt-6-luna"]]]);
  const doc = withRows([["roles.reviewer.fallback", ["codex/gpt-6-luna"]]]);
  assert.deepEqual(fallbackWrite(doc, "reviewer", null).map((w) => w.value), [[]]);
  assert.deepEqual(fallbackWrite(doc, "reviewer", { agent: "codex", model: "gpt-6-luna" }), [], "no change, no write");
});

test("the Workers row shows a picker per line, a + while a program is free, and × when there are two", async () => {
  const one = await drawn("agents");
  let ids = walk(one.tree()).map((node) => node.idv).filter(Boolean);
  assert.ok(ids.includes("line-worker-0"));
  assert.ok(ids.includes("add-line-worker"), "the + is there");
  assert.ok(!ids.some((id) => id.startsWith("remove-line-worker")), "the last line has no ×");
  const two = await drawn("agents", { settings: withRows([["roles.worker.agents", ["codex"]]]) });
  ids = walk(two.tree()).map((node) => node.idv).filter(Boolean);
  assert.ok(ids.includes("line-worker-1"));
  assert.ok(ids.includes("remove-line-worker-0") && ids.includes("remove-line-worker-1"));
  assert.ok(!ids.includes("fallback-worker"), "the worker's separate Fallback control is gone");
});

test("the + adds a line, the line's menu opens with the program taken, a pick replaces it, × removes it", async () => {
  const { view, calls, tree } = await drawn("agents");
  assert.ok(click(tree(), "add-line-worker").stopped);
  await view.pump();
  assert.deepEqual(calls.at(-1), { key: "roles.worker.agents", value: ["claude|||"+"|"], forRepo: false });

  view.settings = withRows([["roles.worker.agents", ["claude|||"+"|"]]]);
  view.rec("line-worker-1").at = { x: 600, y: 300, width: 200, height: 28 };
  click(view.render(), "line-worker-1");
  const popup = herdr.OVERLAYS.at(-1);
  assert.equal(popup.props.key, "line:worker:1");
  assert.deepEqual(popup.props.taken, ["pi"], "the program the other line runs is not pickable");
  assert.equal(popup.props.light, false);

  // the menu answers with the new selection; the window writes the list
  view.handleReply({ key: "line:worker:1", field: "selection", value: { agent: "codex", model: "gpt-6-luna", effort: null, fast: false, perm: null }, at: 1 });
  await view.pump();
  assert.deepEqual(calls.at(-1), { key: "roles.worker.agents", value: ["codex|gpt-6-luna|||"], forRepo: false });

  view.settings = withRows([["roles.worker.agents", ["codex|gpt-6-luna|||"]]]);
  click(view.render(), "remove-line-worker-1");
  await view.pump();
  assert.deepEqual(calls.at(-1), { key: "roles.worker.agents", value: [], forRepo: false });
});

test("the reviewer's Fallback is a second picker with None, the same menu as the main pick", async () => {
  const { view, calls, tree } = await drawn("agents");
  const ids = walk(tree()).map((node) => node.idv).filter(Boolean);
  assert.ok(ids.includes("fallback-reviewer"));
  view.rec("fallback-reviewer").at = { x: 500, y: 200, width: 160, height: 28 };
  click(view.render(), "fallback-reviewer");
  const popup = herdr.OVERLAYS.at(-1);
  assert.equal(popup.props.key, "fb:reviewer");
  assert.equal(popup.props.allowNone, true);
  assert.equal(popup.props.light, true);
  view.handleReply({ key: "fb:reviewer", field: "selection", value: { agent: "codex", model: null, effort: null, fast: false, perm: null }, at: 2 });
  await view.pump();
  assert.deepEqual(calls.at(-1), { key: "roles.reviewer.fallback", value: ["codex"], forRepo: false });
  view.settings = withRows([["roles.reviewer.fallback", ["codex"]]]);
  view.handleReply({ key: "fb:reviewer", field: "selection", value: null, at: 3 });
  await view.pump();
  assert.deepEqual(calls.at(-1), { key: "roles.reviewer.fallback", value: [], forRepo: false }, "None is an empty list");
});

test("the program selector is a dropdown listing every detected program, not tabs", () => {
  herdr.reset();
  record(true);
  const roster = structuredClone(ROSTER);
  // a machine with many CLIs: ten more detected programs
  for (let i = 0; i < 10; i += 1) roster.programs.push({ ...roster.programs[0], id: `extra${i}`, name: `Extra ${i}`, models: [] });
  const menus = menusOf(roster);
  herdr.setProps({
    mode: "models",
    key: "line:worker:0",
    overlay: "ov-7",
    current: { agent: "claude", model: "haiku", effort: null, fast: false, perm: null },
    menus,
    icons: {},
    width: 460,
    height: 500,
    taken: ["codex"],
  });
  const menu = new ModelMenu();
  menu.init({}, context().cx);
  const ids = () => walk(menu.render()).map((node) => node.idv).filter(Boolean);
  assert.ok(ids().includes("program-select"));
  assert.ok(!ids().some((id) => id.startsWith("tab-")), "no row of tabs");
  assert.ok(!ids().some((id) => id.startsWith("program-claude")), "closed: the list is not drawn");
  assert.ok(ids().includes("m-claude-haiku"), "Haiku is listed for Claude");
  assert.ok(click(menu.render(), "program-select").stopped);
  const open = ids();
  for (const program of menus.order) assert.ok(open.includes(`program-${program}`), `${program} is in the dropdown`);
  assert.ok(!open.some((id) => id.startsWith("m-")), "the programs take the models' place while open");
  // a program another line runs is not pickable
  click(menu.render(), "program-codex");
  assert.equal(menu.tab, "claude", "a taken program is refused");
  assert.ok(click(menu.render(), "program-kilo").stopped);
  assert.equal(menu.tab, "kilo");
  assert.ok(ids().some((id) => id.startsWith("m-kilo-")), "kilo's models are listed");
  // the popup is as tall as the longest list, a program list included
  assert.ok(menuHeight(menus) >= menuHeight(menusOf(ROSTER)));
});

test("a light menu (the reviewer's fallback) has None and no Reasoning, Fast or Permissions, and closes on a pick", () => {
  herdr.reset();
  record(true);
  herdr.setProps({
    mode: "models",
    key: "fb:reviewer",
    overlay: "ov-8",
    current: { agent: null, model: null, effort: null, fast: false, perm: null },
    menus: menusOf(ROSTER),
    icons: {},
    width: 460,
    height: 400,
    light: true,
    allowNone: true,
  });
  const menu = new ModelMenu();
  menu.init({}, context().cx);
  const ids = walk(menu.render()).map((node) => node.idv).filter(Boolean);
  assert.ok(ids.includes("none"));
  assert.ok(!ids.includes("fast") && !ids.includes("perms") && !ids.some((id) => id.startsWith("level-")));
  assert.ok(menuHeight(menusOf(ROSTER), { light: true, none: true }) < menuHeight(menusOf(ROSTER)));
  click(menu.render(), "m-claude-haiku");
  assert.equal(herdr.shared(PICK_KEY).value.model, "haiku");
  assert.deepEqual(herdr.CLOSED, ["ov-8"], "a pick closes a light menu");
  herdr.reset();
  click(menu.render(), "none");
  assert.equal(herdr.shared(PICK_KEY).value, null);
});
