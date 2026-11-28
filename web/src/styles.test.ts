// The stylesheet is one hand-merged file: a lost brace nests every rule after it
// under the wrong selector (it once swallowed the whole tasks and chat CSS).

import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

// Read from disk: vitest answers an empty string for a `.css?raw` import, which
// would make every check below pass on nothing.
const raw = readFileSync(new URL("./styles.css", import.meta.url), "utf8");

const css = raw.replace(/\/\*[\s\S]*?\*\//g, "");

describe("styles.css", () => {
  it("is not empty", () => {
    expect(css.length).toBeGreaterThan(1000);
  });

  it("has balanced braces, and every top-level rule starts at depth zero", () => {
    let depth = 0;
    const nested: string[] = [];
    for (const line of css.split("\n")) {
      if (depth !== 0 && /^[.#:\w[*]/.test(line)) {
        nested.push(line);
      }
      depth += (line.match(/\{/g) ?? []).length - (line.match(/\}/g) ?? []).length;
    }
    expect(nested).toEqual([]);
    expect(depth).toBe(0);
  });

  it("keeps the accept card inside a phone's width", () => {
    // An auto grid column grows to the widest nowrap line and pushes the card's
    // buttons off screen; the column must be allowed to shrink.
    const backdrop = /\.accept-backdrop\s*\{[^}]*\}/.exec(css)?.[0] ?? "";
    expect(backdrop).toContain("grid-template-columns: minmax(0, 1fr)");
    const card = /\.accept\s*\{[^}]*\}/.exec(css)?.[0] ?? "";
    expect(card).toContain("min-width: 0");
    expect(card).toContain("max-width: 100%");
    const command = /\.accept-path,\s*\.accept-check-command\s*\{[^}]*\}/.exec(css)?.[0] ?? "";
    expect(command).toContain("overflow-wrap: anywhere");
    expect(command).not.toContain("nowrap");
  });
});
