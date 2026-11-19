// The model menu's rules: which programs get a tab, what the button says, what a
// pick writes, and when the coordinator's restart notice shows.

import { describe, expect, it } from "vitest";

import {
  chooseModel,
  coordinatorRunning,
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
    expect(after).toMatchObject({ model: "haiku", fast: false, effort: "medium" });
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
