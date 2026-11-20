// The New space dialog's rules, and the panel helpers that open it and New task.

import test from "node:test";
import assert from "node:assert/strict";

import { NEW_REPO, base, offersRepo, pathOf, spaceName, spaceNewArgs, tilde, assistantTabArgs, madeOf } from "../mods/lib/newspace.js";
import { openAddTask, openNewSpace } from "../mods/lib/panel.js";
import { OVERLAYS, RUNS, SHARED } from "./stub-herdr.js";

test("the field's text names a folder only when it is a path", () => {
  assert.equal(pathOf("~/dev/shop/", "/home/me"), "/home/me/dev/shop");
  assert.equal(pathOf("/srv/x", "/home/me"), "/srv/x");
  assert.equal(pathOf("shop", "/home/me"), null);
  assert.equal(tilde("/home/me/dev", "/home/me"), "~/dev");
});

test("a folder outside git is offered a repo, home never", () => {
  assert.equal(offersRepo("/home/me/new", false, "/home/me"), true);
  assert.equal(offersRepo("/home/me/new", true, "/home/me"), false);
  assert.equal(offersRepo("/home/me/new", undefined, "/home/me"), false);
  assert.equal(offersRepo("/home/me", false, "/home/me"), false);
  assert.deepEqual(NEW_REPO[0].slice(0, 1), ["init"]);
});

test("the space is named for its folder and made with space.new", () => {
  assert.equal(spaceName("/home/me/dev/shop"), "shop");
  assert.equal(base("/x/y/"), "y");
  assert.deepEqual(spaceNewArgs("/a/shop"), ["do", "space.new", "--folder", "/a/shop", "--name", "shop"]);
});

test("New task from anywhere shares a request and selects the list: it never calls the list's own command", () => {
  RUNS.length = 0;
  openAddTask();
  assert.equal(typeof SHARED.get("tasks.add").at, "number");
  assert.ok(RUNS.every((call) => call.name !== "shprd-tasks.add"), "no command to a mod that may not be loaded");
  assert.deepEqual(
    RUNS.map((call) => `${call.name}:${call.args?.child ?? call.args?.id}`),
    ["layout.select:tasks", "layout.select:tasks-list", "layout.show:panel"],
  );
});

test("New space opens its dialog", () => {
  OVERLAYS.length = 0;
  openNewSpace();
  assert.equal(OVERLAYS.at(-1).mod, "shprd-newspace");
  assert.equal(OVERLAYS.at(-1).kind, "dialog");
});

test("the new space opens on the chat view, not a shell (SHPR-75)", () => {
  const args = assistantTabArgs("sp9");
  assert.deepEqual(args.slice(0, 8), ["do", "tab.new", "--space", "sp9", "--name", "Assistant", "--mod", "shprd-chat"]);
  assert.deepEqual(JSON.parse(args[9]), { space: "coordinator" });
  assert.deepEqual(madeOf({ space: "b", tab: "t1", terminal: "t2" }), { id: "b", tab: "t1" }, "only the tab space.new made");
  assert.deepEqual(madeOf({ space: "b" }), { id: "b", tab: "" });
  assert.equal(madeOf({}), null);
  assert.equal(madeOf(null), null);
});
