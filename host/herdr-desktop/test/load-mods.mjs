// Loads every mod's main.js the way Herdr does, with the app's modules stubbed:
// `node --check` first (a duplicate import is a syntax error), then an import
// of the module, which catches a bad import path or a name the stubs lack.
//
//   node test/load-mods.mjs [mods-dir]
//
// Exits non-zero, naming each mod that cannot load.

import { execFileSync } from "node:child_process";
import { readdirSync, existsSync, readFileSync, statSync } from "node:fs";
import { register } from "node:module";
import { resolve, join } from "node:path";
import { pathToFileURL, fileURLToPath } from "node:url";

const root = resolve(process.argv[2] ?? fileURLToPath(new URL("../mods", import.meta.url)));

/** Every name any file under `dir` imports from the app's modules. */
function importedNames(dir, found = {}) {
  for (const entry of readdirSync(dir)) {
    const path = join(dir, entry);
    if (statSync(path).isDirectory()) importedNames(path, found);
    else if (entry.endsWith(".js")) {
      const text = readFileSync(path, "utf8");
      for (const m of text.matchAll(/import\s*\{([^}]*)\}\s*from\s*["'](herdr|gpui-kit|gpui-base|gpui)["']/g)) {
        const set = (found[m[2]] ??= new Set());
        for (const part of m[1].split(",")) {
          const n = part.trim().split(/\s+as\s+/)[0].trim();
          if (n) set.add(n);
        }
      }
    }
  }
  return found;
}
const found = importedNames(root);
for (const spec of ["herdr", "gpui-kit", "gpui-base", "gpui"]) found[spec] ??= new Set();
process.env.LOAD_MODS_NAMES = JSON.stringify(
  Object.fromEntries(Object.entries(found).map(([k, v]) => [k, [...v]])),
);
register("./load-hooks.mjs", import.meta.url);

let failed = 0;
let count = 0;
for (const name of readdirSync(root).sort()) {
  const main = join(root, name, "main.js");
  if (!existsSync(main)) continue;
  count += 1;
  try {
    execFileSync(process.execPath, ["--check", main], { stdio: "pipe" });
    await import(pathToFileURL(main).href);
  } catch (e) {
    failed += 1;
    const why = String(e.stderr || e.message || e).split("\n").slice(0, 8).join("\n   ");
    console.error(`${name} did not load:\n   ${why}`);
  }
}
if (failed) process.exit(1);
console.log(`ok: ${count} mods load`);
