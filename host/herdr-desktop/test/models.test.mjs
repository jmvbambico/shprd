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
  selectionLabel,
  selectionWrites,
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
  view.rec("model-worker").at = { x: 600, y: 200, width: 120, height: 28 };
  const { stopped } = click(root, "model-worker");
  assert.ok(stopped);
  assert.equal(herdr.OVERLAYS.length, 1);
  const popup = herdr.OVERLAYS[0];
  assert.equal(popup.mod, "shprd-models");
  assert.equal(popup.kind, "popup");
  assert.equal(popup.props.mode, "models");
  assert.equal(popup.props.key, "role:worker");
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
