// The load check fails a mod that cannot load, and fails on the cases below; `bin/check` runs it on the real mods.

import test from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const script = fileURLToPath(new URL("./load-mods.mjs", import.meta.url));

function load(dir) {
  return spawnSync("node", [script, dir], { encoding: "utf8" });
}

test("a mod with a duplicate import fails the check", () => {
  const dir = mkdtempSync(join(tmpdir(), "shprd-load-mods-"));
  try {
    mkdirSync(join(dir, "shprd-bad"));
    writeFileSync(
      join(dir, "shprd-bad", "main.js"),
      'import { layout, knownAgents, knownAgents } from "herdr";\nexport default {};\n',
    );
    const r = load(dir);
    assert.notEqual(r.status, 0);
    assert.match(r.stderr, /shprd-bad did not load/);
    assert.match(r.stderr, /already been declared/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("a mod importing a file that is not there fails the check", () => {
  const dir = mkdtempSync(join(tmpdir(), "shprd-load-mods-"));
  try {
    mkdirSync(join(dir, "shprd-bad"));
    writeFileSync(join(dir, "shprd-bad", "main.js"), 'import "./lib/missing.js";\n');
    const r = load(dir);
    assert.notEqual(r.status, 0);
    assert.match(r.stderr, /shprd-bad did not load/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
