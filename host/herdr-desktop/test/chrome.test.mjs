// Unit tests for the footer's service row: how it starts `shprd serve`, and why
// it must be `hc run --bg` rather than the app's own `exec`.
//
//   node --import ./test/boot.mjs --test test/
//   bun test --preload ./test/boot-bun.mjs test/
//
// The claim under test is the one that matters: the start must not be a bounded
// `machine.exec`, because that is documented "everything it starts dies with it"
// (M7ef §2.3). `hc run --bg` starts a herdr-core terminal and answers at once,
// so the server outlives the call. `test/stub-herdr.js` records every `hc` call;
// nothing here needs a Herdr window, a core or a service.

import test from "node:test";
import assert from "node:assert/strict";

import * as herdr from "herdr";
import Chrome from "../mods/shprd-chrome/main.js";
import { terminalName } from "../mods/shprd-chrome/lib/client.js";

/** A context that answers `cx.sleep`, `cx.notify` and `cx.timer`. */
function context() {
  const after = [];
  const every = [];
  return {
    cx: {
      sleep: async () => {},
      notify: () => {},
      timer: {
        every: (ms, fn) => {
          every.push({ ms, fn });
          return { cancel: () => {} };
        },
        after: (ms, fn) => {
          after.push({ ms, fn });
          return { cancel: () => {} };
        },
      },
      spawn: () => ({ cancel: () => {} }),
    },
    after,
    every,
  };
}

/** A footer whose first look found nothing, so the start row is live. */
async function footer() {
  herdr.reset();
  herdr.setSpaces([{ id: "s1", active: true, folder: "/repo/demo" }]);
  const run = context();
  const view = new Chrome();
  view.cx = run.cx;
  view.service = null;
  view.folder = "/repo/demo";
  return { view, ...run };
}

test("the start is hc run --bg, never the app's bounded exec", async () => {
  const { view } = await footer();
  await view.start("/repo/demo");

  const run = herdr.HC_CALLS.find((args) => args[0] === "run");
  assert.ok(run, "a terminal was started");
  // `--bg` is the whole point: `hc run` without it waits on the program, and the
  // app's `exec` would kill it when the call ends.
  assert.ok(run.includes("--bg"), `the start asks the core to come back: ${run}`);
  assert.ok(run.includes("--keep"), "a lost program leaves a shell, not a restart");
  assert.ok(run.includes("--no-parent"), "the service is nobody's child");
  const name = run[run.indexOf("--name") + 1];
  assert.equal(name, terminalName("/repo/demo"), "a terminal per repository");
  assert.match(name, /^[a-z0-9_-]{1,32}$/, "a name hc accepts");
  assert.equal(run[run.indexOf("--cwd") + 1], "/repo/demo");
  assert.deepEqual(run.slice(run.indexOf("--") + 1), ["shprd", "serve"]);
  // A kept terminal that outlived its service holds the name: it is ended first.
  assert.deepEqual(herdr.HC_CALLS[0], ["kill", name]);
});

test("two repositories get two terminals, and two folders of one name stay apart", () => {
  assert.notEqual(terminalName("/work/shop"), terminalName("/work/scratchpad"));
  assert.notEqual(terminalName("/work/shop"), terminalName("/other/shop"));
  assert.equal(terminalName("/work/shop/"), terminalName("/work/shop"));
});

test("a start that fails says why, in the row, and does not retry every look", async () => {
  const { view } = await footer();
  herdr.setHc(async (args) =>
    args[0] === "run" ? { code: 1, stdout: "", stderr: "no such folder: /repo/demo\nmore text" } : { code: 0, stdout: "", stderr: "" },
  );
  await view.start("/repo/demo");
  assert.equal(view.failure, "no such folder: /repo/demo", "the first line, not the stack");
  assert.equal(view.service, null);
  const word = view.actions(herdr.theme()).find((action) => action.id === "service").word;
  assert.equal(word, "demo: no such folder: /repo/demo");
  assert.equal(herdr.shared("service.status").phase, "failed");

  // The next look inside the retry window starts nothing.
  const before = herdr.HC_CALLS.length;
  await view.check(view.cx);
  assert.equal(herdr.HC_CALLS.length, before, "a failed repository waits before it is tried again");
});

test("a service that dies at once is explained by what it said", async () => {
  const { view } = await footer();
  herdr.setHc(async (args) =>
    args[0] === "read"
      ? { code: 0, stdout: "starting\nError: a shprd service is already running for /repo/demo\n", stderr: "" }
      : { code: 0, stdout: "", stderr: "" },
  );
  await view.start("/repo/demo");
  assert.equal(view.failure, "a shprd service is already running for /repo/demo");
});

test("a folder that is not a git repository is not started", async () => {
  const { view } = await footer();
  herdr.setExec(async () => ({ code: 128, stdout: "", stderr: "fatal: not a git repository" }));
  await view.start("/repo/demo");
  assert.equal(herdr.HC_CALLS.length, 0, "nothing was started");
  assert.equal(view.failure, "demo is not a git repository");
});

test("a space with no service starts one by itself; an up service is left alone", async () => {
  const { view } = await footer();
  await view.check(view.cx);
  assert.ok(herdr.HC_CALLS.some((args) => args[0] === "run"), "clicking a repo's space starts its service");

  const second = await footer();
  second.view.service = null;
  second.view.starting = true;
  const calls = herdr.HC_CALLS.length;
  await second.view.check(second.view.cx);
  assert.equal(herdr.HC_CALLS.length, calls, "no second start while one is starting");
});

test("a start with no space to point at says so and starts nothing", async () => {
  const { view } = await footer();
  herdr.setSpaces([]);
  await view.check(view.cx);
  assert.equal(herdr.HC_CALLS.length, 0, "nothing was started");
  const word = view.actions(herdr.theme()).find((action) => action.id === "service").word;
  assert.equal(word, "Open a repo's space");
});

test("a start that the core is already serving is not run twice", async () => {
  const { view } = await footer();
  view.starting = true;
  await view.start("/repo/demo");
  assert.equal(herdr.HC_CALLS.length, 0, "a second click while starting does nothing");
});

// -- the footer's rows -----------------------------------------------------

test("the footer's rows are the app's settings, shprd's own, and the service", async () => {
  const { view } = await footer();
  const actions = view.actions(herdr.theme());
  assert.deepEqual(
    actions.map((action) => action.id),
    ["settings", "shprd", "service"],
    "Shepherdr's three rows, with shprd's service in place of Add machine",
  );
  // The service row is a click only while there is nothing to look at.
  view.folder = "/repo/demo";
  assert.equal(typeof view.actions(herdr.theme())[2].onClick, "function");
  herdr.setSpaces([{ id: "s1", active: true, folder: "/repo/demo" }]);
  view.folder = "/repo/demo";
  view.service = { record: { port: 7311 } };
  assert.equal(view.actions(herdr.theme())[2].onClick, null);
  assert.equal(view.actions(herdr.theme())[2].word, "demo: service up");
});

test("the shprd row opens the settings window over the layout, not a panel tab", async () => {
  const { view } = await footer();
  const row = view.actions(herdr.theme()).find((action) => action.id === "shprd");
  row.onClick();

  assert.equal(herdr.OVERLAYS.length, 1, "one overlay");
  const overlay = herdr.OVERLAYS[0];
  // Shepherdr's own `openSettings()`: a dialog of the settings mod, most of the
  // window's height, with the frame left to the mod.
  assert.equal(overlay.mod, "shprd-settings");
  assert.equal(overlay.kind, "dialog");
  assert.equal(overlay.frame, "none");
  assert.equal(overlay.width, 860);
  assert.equal(overlay.height, 0.92);
  assert.equal(overlay.props.page, undefined, "no page asked for: the window opens on Agents");
  // Nothing was asked of the side panel: the settings are a window now.
  assert.deepEqual(
    herdr.RUNS.filter((call) => call.name.startsWith("layout.")),
    [],
  );
});

test("the app's own settings row is still the app's", async () => {
  const { view } = await footer();
  const row = view.actions(herdr.theme()).find((action) => action.id === "settings");
  row.onClick();
  assert.deepEqual(herdr.RUNS, [{ name: "app.settings", args: undefined }]);
  assert.deepEqual(herdr.OVERLAYS, [], "and opens no overlay of ours");
});

test("a record whose process is alive is never killed or restarted", async () => {
  const { view } = await footer();
  herdr.setHome("/home/ana");
  herdr.putFile("/home/ana/.local/state/shprd/repos/demo-1/service.json", JSON.stringify({ port: 7001, id: "x", pid: 4242, root: "/repo/demo" }));
  herdr.setExec(async (command, args) => (command === "kill" ? { code: 0, stdout: "", stderr: "" } : { code: 0, stdout: "", stderr: "" }));
  await view.start("/repo/demo");
  assert.equal(herdr.HC_CALLS.length, 0, "a busy service is left alone");
  assert.equal(view.failure, null);
});
