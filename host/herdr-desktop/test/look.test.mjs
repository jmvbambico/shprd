// Unit tests for the window's look (M11 U8): the icon set is all there, the pie
// steps by five, a toast's expiry is held while the pointer is on it, and a pane
// header writes the home folder as `~`.
//
//   node --import ./test/boot.mjs --test test/

import test from "node:test";
import assert from "node:assert/strict";
import { existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import * as herdr from "herdr";
import { iconFile, pieFile } from "../mods/lib/panel.js";
import { progressOf } from "../mods/lib/board.js";
import Pane from "../mods/shprd-pane/main.js";
import Toasts from "../mods/shprd-toasts/main.js";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");

test("every icon the window draws is shipped", () => {
  const names = [
    "settings", "crook", "plug", "monitor", "branch", "plus", "search", "chevron-down",
    "split-right", "split-down", "zoom", "unzoom", "x", "spinner", "circle", "queued",
    "scheduled", "off", "tick", "merging",
  ];
  for (const name of names) {
    assert.ok(existsSync(join(root, iconFile(name))), `${name}.svg is under mods/shprd-tabs/icons`);
  }
  for (let pct = 5; pct <= 100; pct += 5) {
    assert.ok(existsSync(join(root, pieFile(pct))), `pie/${pct}.svg`);
  }
});

test("the pie fills in steps of five, at least five, half when there is no progress", () => {
  assert.ok(pieFile(62).endsWith("/pie/60.svg"));
  assert.ok(pieFile(63).endsWith("/pie/65.svg"));
  assert.ok(pieFile(0).endsWith("/pie/5.svg"));
  assert.ok(pieFile(250).endsWith("/pie/100.svg"));
  assert.ok(pieFile(null).endsWith("/pie/50.svg"));
  assert.ok(pieFile(undefined).endsWith("/pie/50.svg"));
});

test("a task's progress is the percent its last status reported", () => {
  assert.equal(progressOf({ last_status: { progress: 40 } }), 40);
  assert.equal(progressOf({ last_status: {} }), null);
  assert.equal(progressOf({ last_status: { progress: -1 } }), null);
  assert.equal(progressOf(null), null);
});

test("a toast's time stops while the pointer is on it, and none loses any", () => {
  const view = new Toasts();
  view.items = [{ id: "a", at: 1_000 }];
  view.hovered = null;
  view.frozen = null;
  const real = Date.now;
  try {
    Date.now = () => 5_000;
    view.hover("a", true, null);
    Date.now = () => 9_000;
    assert.equal(view.clock(), 5_000, "the clock stood still at the pointer's arrival");
    view.hover("a", false, null);
    assert.equal(view.items[0].at, 5_000, "the held 4 s are given back");
    assert.equal(view.frozen, null);
  } finally {
    Date.now = real;
  }
});

test("a dismissed toast under the pointer lets time go on", () => {
  const view = new Toasts();
  view.items = [{ id: "a", at: 1_000 }];
  view.dismissed = new Set();
  view.hovered = null;
  view.frozen = null;
  view.hover("a", true, null);
  view.dismiss("a", null);
  assert.equal(view.frozen, null);
  assert.equal(view.hovered, null);
});

test("a pane header writes the home folder as ~", () => {
  herdr.reset();
  herdr.setHome("/Users/someone");
  const pane = new Pane();
  assert.equal(pane.short("/Users/someone"), "~");
  assert.equal(pane.short("/Users/someone/dev/shop"), "~/dev/shop");
  assert.equal(pane.short("/srv/other"), "/srv/other");
  assert.equal(pane.short(null), "");
});
