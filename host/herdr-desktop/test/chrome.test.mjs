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
  return { view, ...run };
}

test("the start is hc run --bg, never the app's bounded exec", async () => {
  const { view } = await footer();
  await view.start();

  assert.equal(herdr.HC_CALLS.length, 1, "one call");
  const args = herdr.HC_CALLS[0];
  // `--bg` is the whole point: `hc run` without it waits on the program, and the
  // app's `exec` would kill it when the call ends.
  assert.ok(args.includes("--bg"), `the start asks the core to come back: ${args}`);
  assert.ok(args.includes("--keep"), "a lost program leaves a shell, not a restart");
  assert.ok(args.includes("--no-parent"), "the service is nobody's child");
  assert.equal(args[args.indexOf("--name") + 1], "shprd-serve");
  assert.equal(args[args.indexOf("--cwd") + 1], "/repo/demo");
  // The command itself comes after `--`, and is `shprd serve`.
  const command = args.slice(args.indexOf("--") + 1);
  assert.deepEqual(command, ["shprd", "serve"]);
  // And the app's own `exec` was never used for it: the only thing asked of the
  // machine is `hc`, and the only `run` is none at all. (The footer does look
  // for `service.json` afterwards, which is how it reports what the service
  // itself says rather than what this button hoped.)
  assert.equal(herdr.RUNS.length, 0, "no app command was run");
});

test("a start that fails says why, in the row", async () => {
  const { view } = await footer();
  herdr.setHc(async () => ({ code: 1, stdout: "", stderr: "no such folder: /repo/demo\nmore text" }));
  await view.start();
  assert.equal(view.failure, "no such folder: /repo/demo", "the first line, not the stack");
  assert.equal(view.service, null);
});

test("a start with no space to point at says so and starts nothing", async () => {
  const { view } = await footer();
  herdr.setSpaces([]);
  await view.start();
  assert.equal(herdr.HC_CALLS.length, 0, "nothing was started");
  assert.equal(view.failure, "No space names a repository yet");
});

test("a start that the core is already serving is not run twice", async () => {
  const { view } = await footer();
  view.starting = true;
  await view.start();
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
  assert.equal(typeof actions[2].onClick, "function");
  herdr.setSpaces([{ id: "s1", active: true, folder: "/repo/demo" }]);
  view.service = { record: { port: 7311 } };
  assert.equal(view.actions(herdr.theme())[2].onClick, null);
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
  assert.deepEqual(overlay.props, {}, "no page asked for: the window opens on Agents");
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
