// The model menu's rules: which programs get a tab, what the button says, what a
// pick writes, and when the coordinator's restart notice shows.

import { describe, expect, it } from "vitest";

import {
  chooseModel,
  coordinatorRunning,
  encodeLine,
  fallbackLabel,
  fallbackWrite,
  lineLabel,
  lineWrites,
  nextLineProgram,
  parseLine,
  withLine,
  workerLines,
  detectedPrograms,
  levelsFor,
  menusOf,
  modelRows,
  otherCli,
  restartNotice,
  roleSelection,
  selectionLabel,
  selectionWrites,
} from "./menu";
import { ROSTER, row, settings } from "./testkit";

const roster = {
  programs: [
    { ...ROSTER.programs[0]!, fast: true, models: [{ id: "opus", name: "Opus 5.5", efforts: ["low", "medium"], default_effort: "medium", fast: true }, { id: "haiku", name: "Haiku 5.5", efforts: [], fast: false }] },
    ROSTER.programs[1]!,
    { ...ROSTER.programs[1]!, id: "pi", name: "Pi", installed: false, vendor: "pi" },
  ],
};

describe("the model menu's tabs", () => {
  it("are the programs the roster says are installed, never a fixed pair", () => {
    expect(detectedPrograms(roster).map((program) => program.id)).toEqual(["claude", "codex"]);
    const menus = menusOf(roster);
    expect(menus.order).toEqual(["claude", "codex"]);
    expect(menus.programs.pi).toBeUndefined();
  });

  it("show a program the machine has that Shepherdr's pair has not", () => {
    const more = { programs: [...roster.programs.slice(0, 2), { ...roster.programs[2]!, installed: true }] };
    expect(menusOf(more).order).toEqual(["claude", "codex", "pi"]);
  });
});

describe("the button and the rows", () => {
  const menus = menusOf(roster);
  it("says the model and its level, or asks for a program", () => {
    expect(selectionLabel(menus, { agent: "claude", model: "opus", effort: null, fast: false, perm: null })).toBe("Opus 5.5 · Medium");
    expect(selectionLabel(menus, { agent: "claude", model: "opus", effort: "low", fast: false, perm: null })).toBe("Opus 5.5 · Low");
    expect(selectionLabel(menus, { agent: null, model: null, effort: null, fast: false, perm: null })).toBe("Choose a program");
  });

  it("lists the tab's models, searches every program, and offers a typed id", () => {
    expect(modelRows(menus, "claude", "").map((r) => r.id)).toEqual(["opus", "haiku"]);
    expect(modelRows(menus, "claude", "hai").map((r) => r.id)).toEqual(["haiku", "hai"]);
    expect(modelRows(menus, "codex", "")).toEqual([]);
    expect(modelRows(menus, "codex", "gpt-6")).toEqual([{ agent: "codex", id: "gpt-6", name: "Use gpt-6", typed: true }]);
  });

  it("offers a typed id only when the word is not already a listed model's id or name", () => {
    expect(modelRows(menus, "claude", "Opus 5.5").map((r) => r.id)).toEqual(["opus"]);
    expect(modelRows(menus, "claude", "opus").map((r) => r.typed)).toEqual([false]);
    expect(modelRows(menus, "claude", "opu").map((r) => r.id)).toEqual(["opus", "opu"]);
  });

  it("offers only the levels the model takes", () => {
    expect(levelsFor(menus, { agent: "claude", model: "opus", effort: null, fast: false, perm: null })).toEqual(["low", "medium"]);
    expect(levelsFor(menus, { agent: "claude", model: "haiku", effort: null, fast: false, perm: null })).toEqual(["low", "high"]);
  });
});

describe("a pick", () => {
  const menus = menusOf(roster);
  it("drops a level or speed the new model does not have, and writes program, model, level in order", () => {
    const before = { agent: "claude", model: "opus", effort: "medium", fast: true, perm: "auto" };
    const after = chooseModel(menus, before, "claude", "haiku");
    expect(after).toMatchObject({ model: "haiku", fast: false, effort: null });
    const moved = chooseModel(menus, before, "codex", "gpt-6");
    expect(moved.perm).toBeNull();
    expect(selectionWrites("worker", before, moved).map((w) => w.key)).toEqual([
      "roles.worker.agent",
      "roles.worker.model",
      "roles.worker.effort",
      "roles.worker.fast",
      "roles.worker.perm",
    ]);
    expect(selectionWrites("worker", before, before)).toEqual([]);
  });

  it("reads a role's selection from the settings", () => {
    const doc = settings([row("roles.worker.fast", true, "you"), row("roles.worker.effort", "low", "you")]);
    expect(roleSelection(doc, "worker")).toEqual({ agent: "claude", model: "sonnet", effort: "low", fast: true, perm: null });
  });

  it("Use other CLI picks an installed program of another vendor, and is gone once the reviewer is one", () => {
    expect(otherCli(settings([row("roles.reviewer.agent", "claude", "you")]), roster)).toBe("codex");
    expect(otherCli(settings(), roster)).toBeNull();
  });
});

describe("the restart notice", () => {
  it("shows only when the coordinator changed here and one is running", () => {
    expect(restartNotice({ changed: false, running: true })).toBe(false);
    expect(restartNotice({ changed: true, running: false })).toBe(false);
    expect(restartNotice({ changed: true, running: true })).toBe(true);
  });

  it("reads a running coordinator from the live agents", () => {
    const agent = { id: "a", space: "s", task: null, program: "claude", state: "working", terminal: "t" };
    expect(coordinatorRunning({ live_agents: [] })).toBe(false);
    expect(coordinatorRunning({ live_agents: [{ ...agent, role: "worker" }] })).toBe(false);
    expect(coordinatorRunning({ live_agents: [{ ...agent, role: "coordinator" }] })).toBe(true);
    expect(coordinatorRunning({ live_agents: [{ ...agent, role: "coordinator", ended_ms: 5 }] })).toBe(false);
  });
});

describe("a worker's program lines", () => {
  const doc = (...rows: Array<ReturnType<typeof row>>) => settings(rows);

  it("a stored line reads and writes back as the same line", () => {
    const line = { agent: "codex", model: "gpt-6-luna", effort: "high", fast: true, perm: "auto" };
    expect(parseLine(encodeLine(line))).toEqual(line);
    expect(parseLine("pi/commandcode/z-ai/glm")).toEqual({ agent: "pi", model: "commandcode/z-ai/glm", effort: null, fast: false, perm: null });
    expect(parseLine("|model")).toBeNull();
  });

  it("are the role's own pick, then agents; the old fallback reads as lines too; a program once", () => {
    expect(workerLines(doc()).map((line) => line.agent)).toEqual(["claude"]);
    const two = doc(row("roles.worker.agents", ["codex|gpt-6-luna||1|auto", "claude"], "you"));
    const lines = workerLines(two);
    expect(lines.map((line) => line.agent)).toEqual(["claude", "codex"]);
    expect(lines[1]).toMatchObject({ model: "gpt-6-luna", fast: true, perm: "auto" });
    const old = doc(row("roles.worker.fallback", ["codex"], "you"));
    expect(workerLines(old).map((line) => line.agent)).toEqual(["claude", "codex"]);
  });

  it("adding writes agents with the next free program, and the old fallback is emptied", () => {
    expect(nextLineProgram(doc(), roster)).toBe("codex");
    const added = [...workerLines(doc()), { agent: "codex", model: null, effort: null, fast: false, perm: null }];
    expect(lineWrites(doc(), "worker", added).map((w) => [w.key, w.value])).toEqual([["roles.worker.agents", ["codex||||"]]]);
    const old = doc(row("roles.worker.fallback", ["codex"], "you"));
    const writes = lineWrites(old, "worker", withLine(workerLines(old), 1, null));
    expect(writes.at(-1)).toEqual({ key: "roles.worker.fallback", value: [], prior: ["codex"] });
    const all = doc(row("roles.worker.agents", ["codex"], "you"));
    expect(nextLineProgram(all, roster)).toBeNull();
  });

  it("removing the first line promotes the next into the role's own fields", () => {
    const two = doc(row("roles.worker.agents", ["codex|gpt-6-luna|||"], "you"));
    const writes = lineWrites(two, "worker", withLine(workerLines(two), 0, null)).map((w) => [w.key, w.value]);
    expect(writes[0]).toEqual(["roles.worker.agent", "codex"]);
    expect(writes.at(-1)).toEqual(["roles.worker.agents", []]);
  });

  it("labels lead with the program; the reviewer's fallback says None when it has none", () => {
    const menus = menusOf(roster);
    expect(lineLabel(menus, { agent: "claude", model: "haiku", effort: "low", fast: false, perm: null }, roster)).toBe("Claude Code · Haiku 5.5 · Low");
    expect(fallbackLabel(menus, null, roster)).toBe("None");
    expect(fallbackWrite(doc(), "reviewer", { agent: "codex", model: "gpt-6-luna", effort: null, fast: false, perm: null }).map((w) => w.value)).toEqual([["codex/gpt-6-luna"]]);
    const set = doc(row("roles.reviewer.fallback", ["codex"], "you"));
    expect(fallbackWrite(set, "reviewer", null).map((w) => w.value)).toEqual([[]]);
    expect(fallbackWrite(set, "reviewer", { agent: "codex", model: null, effort: null, fast: false, perm: null })).toEqual([]);
  });
});
