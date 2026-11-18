// The stylesheet is one hand-merged file: a lost brace nests every rule after it
// under the wrong selector (it once swallowed the whole tasks and chat CSS).

import { describe, expect, it } from "vitest";

import raw from "./styles.css?raw";

const css = raw.replace(/\/\*[\s\S]*?\*\//g, "");

describe("styles.css", () => {
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
});
