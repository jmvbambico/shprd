// Every harness in every slot: each roster program, set as the first line and as a
// second line of Worker (s) and of Reviewer (s), from a role that already holds
// another program's model, level and access mode, through the writes the dialog makes
// (`selectionWrites`, `lineWrites`, `newLine`). Each write goes to a small stand-in
// for the service's checks (the shipped `roles.rs` rules: a model must have the shape
// its program takes, a level and an access mode must be ones the program offers, a
// reviewer never runs in full access, the reviewer's vendor differs from the worker's),
// and the test fails on any refusal that is not the cross-vendor message.
//
// The service-side half is `crates/shprd-server/tests/settings.rs`
// (`every_harness_is_accepted_in_every_slot`), which sends the same final settings to
// the real service with the real harness table.

import { describe, expect, it } from "vitest";

import type { ProgramDto, RosterDto, SettingsDto } from "../gen";
import {
  chooseModel,
  defaultModel,
  encodeLine,
  lineVendor,
  lineWrites,
  menusOf,
  newLine,
  parseLine,
  selectionWrites,
  withLine,
  workerLines,
  type Selection,
  type Write,
} from "./menu";

function program(id: string, extra: Partial<ProgramDto>): ProgramDto {
  return {
    id,
    name: id,
    installed: true,
    vendor: id,
    multi_model: false,
    needs_provider: false,
    models: [],
    models_source: "static",
    efforts: [],
    fast: false,
    permissions: [],
    ...extra,
  };
}

const ROSTER: RosterDto = {
  programs: [
    program("claude", {
      vendor: "anthropic",
      models: [
        { id: "opus", name: "Opus", efforts: ["low", "high"], fast: true, vendor: "anthropic" },
        { id: "haiku", name: "Haiku", efforts: [], fast: false, vendor: "anthropic" },
      ],
      efforts: ["low", "medium", "high", "xhigh", "max"],
      permissions: [
        { id: "manual", label: "Manual" },
        { id: "plan", label: "Plan" },
        { id: "bypass", label: "Bypass" },
      ],
      default_permission: "manual",
    }),
    program("codex", {
      vendor: "openai",
      models: [{ id: "gpt-6-luna", name: "GPT", efforts: ["low", "ultra"], fast: false, vendor: "openai" }],
      efforts: ["low", "medium", "high", "xhigh", "max", "ultra"],
      permissions: [
        { id: "read-only", label: "Read only" },
        { id: "full", label: "Full" },
      ],
      default_permission: "read-only",
    }),
    program("kilo", {
      vendor: "kilo",
      multi_model: false,
      models: [{ id: "kilo/auto", name: "Auto", efforts: [], fast: false, vendor: "kilo" }],
      permissions: [{ id: "ask", label: "Ask" }],
      default_permission: "ask",
    }),
    program("opencode", {
      vendor: "opencode",
      models: [{ id: "opencode/big-pickle", name: "Pickle", efforts: [], fast: false, vendor: "opencode" }],
      permissions: [{ id: "ask", label: "Ask" }],
      default_permission: "ask",
    }),
    program("pi", {
      vendor: "pi",
      multi_model: true,
      needs_provider: true,
      models: [
        { id: "Command Code/deepseek/deepseek-v4.1-flash", name: "DeepSeek", efforts: ["off", "low"], fast: false, vendor: "deepseek" },
        { id: "Command Code/z-ai/glm-5.3-flash", name: "GLM", efforts: [], fast: false, vendor: "zhipu" },
      ],
      efforts: ["off", "minimal", "low", "medium", "high", "xhigh", "max"],
      permissions: [
        { id: "default", label: "As you set it up" },
        { id: "read-only", label: "Read only" },
      ],
      default_permission: "default",
    }),
  ],
};

const FULL: Record<string, string> = { claude: "bypass", codex: "full", pi: "default" };

/** The service's checks on one line, or the sentence it refuses with. */
function refusalOf(role: string, line: Selection): string | null {
  const spec = ROSTER.programs.find((entry) => entry.id === line.agent);
  if (!spec) return `no harness ${line.agent}`;
  if (line.model && spec.needs_provider) {
    const [provider, ...rest] = line.model.split("/");
    if (!provider || rest.join("/") === "") return `\`${spec.id}\` needs a provider: ${line.model}`;
  }
  if (line.effort && spec.efforts.length > 0 && !spec.efforts.includes(line.effort)) {
    return `${spec.id} does not list the effort ${line.effort}`;
  }
  if (line.perm) {
    if (!spec.permissions.some((mode) => mode.id === line.perm)) return `${spec.id} does not offer ${line.perm}`;
    if (role === "reviewer" && FULL[spec.id] === line.perm) return `a reviewer may not run in ${line.perm}`;
  }
  return null;
}

const CROSS_VENDOR = "not cross-vendor";

/** A tiny service: keys in, a `SettingsDto` out, and the checks above on every write. */
class Fake {
  values = new Map<string, string | boolean | string[] | null>();
  seen: string[] = [];
  crossVendor = 0;

  constructor(initial: Record<string, string | boolean | string[] | null>) {
    for (const [key, value] of Object.entries(initial)) this.values.set(key, value);
  }

  document(): SettingsDto {
    return {
      repo: "r",
      problems: [],
      values: [...this.values].map(([key, value]) => ({ key, value, source: "you" as const })),
    } as SettingsDto;
  }

  private selection(role: string): Selection {
    const get = (field: string) => this.values.get(`roles.${role}.${field}`);
    const text = (field: string) => (typeof get(field) === "string" ? (get(field) as string) : null);
    return { agent: text("agent"), model: text("model"), effort: text("effort"), fast: get("fast") === true, perm: text("perm") };
  }

  /** Applies the writes in order; a refusal that is not cross-vendor is recorded. */
  apply(writes: Write[]) {
    for (const write of writes) {
      const next = new Map(this.values);
      next.set(write.key, write.value);
      const [, role, field] = write.key.split(".");
      const probe = new Fake({});
      probe.values = next;
      const lines = [probe.selection(role!)];
      const more = next.get(`roles.${role}.agents`);
      for (const raw of Array.isArray(more) ? more : []) {
        const line = parseLine(raw);
        if (line) lines.push(line);
      }
      const bad = lines.map((line) => (line.agent ? refusalOf(role!, line) : null)).find((reason) => reason !== null);
      if (bad && field !== undefined) {
        this.seen.push(`${write.key}: ${bad}`);
        return;
      }
      // The cross-vendor rule: the reviewer's first line against the worker's first line.
      const worker = probe.selection("worker");
      const reviewer = probe.selection("reviewer");
      if (worker.agent && reviewer.agent && lineVendor(ROSTER, worker) !== null && lineVendor(ROSTER, worker) === lineVendor(ROSTER, reviewer)) {
        this.crossVendor += 1;
        this.seen.push(`${write.key}: ${CROSS_VENDOR}`);
        return;
      }
      this.values = next;
    }
  }
}

function state(): Fake {
  // Both roles start on other programs with a model, level and access mode set.
  return new Fake({
    "roles.worker.agent": "claude",
    "roles.worker.model": "opus",
    "roles.worker.effort": "high",
    "roles.worker.perm": "plan",
    "roles.worker.agents": [encodeLine({ agent: "codex", model: "gpt-6-luna", effort: "ultra", fast: false, perm: "full" })],
    "roles.reviewer.agent": "codex",
    "roles.reviewer.model": "gpt-6-luna",
    "roles.reviewer.effort": "ultra",
    "roles.reviewer.perm": "read-only",
    "roles.reviewer.agents": [encodeLine({ agent: "claude", model: "haiku", effort: null, fast: false, perm: "plan" })],
  });
}

/** What the model menu does when a program's model is picked on a line. */
function pick(fake: Fake, role: string, at: number, programId: string): Selection[] {
  const menus = menusOf(ROSTER);
  const lines = workerLines(fake.document(), role);
  const model = menus.programs[programId]!.models[0]!;
  const next = chooseModel(menus, lines[at]!, programId, model.id);
  return withLine(lines, at, next);
}

describe("every harness in every slot", () => {
  const roles = ["worker", "reviewer"] as const;
  for (const role of roles) {
    for (const position of [0, 1] as const) {
      for (const target of ROSTER.programs) {
        it(`${target.id} as ${role} line ${position + 1}, picked from a model of another program`, () => {
          const fake = state();
          // a program another line already runs is not pickable (the menu shows it as taken)
          const taken = workerLines(fake.document(), role).some((line, i) => i !== position && line.agent === target.id);
          if (taken) return;
          const lines = pick(fake, role, position, target.id);
          fake.apply(lineWrites(fake.document(), role, lines));
          const refused = fake.seen.filter((reason) => !reason.endsWith(CROSS_VENDOR));
          expect(refused, refused.join("; ")).toEqual([]);
          if (fake.crossVendor === 0) {
            expect(workerLines(fake.document(), role)[position]?.agent).toBe(target.id);
          }
        });

        it(`${target.id} as ${role} line ${position + 1}, added with "Add New +" and given its default model`, () => {
          const fake = state();
          const open = new Fake({ ...Object.fromEntries(fake.values), [`roles.${role}.agents`]: [] });
          // The chain holds every program but the target; "Add New +" adds the target.
          const others = ROSTER.programs.filter((entry) => entry.id !== target.id).map((entry) => entry.id);
          open.values.set(`roles.${role}.agent`, others[0]!);
          open.values.set(`roles.${role}.model`, null);
          open.values.set(`roles.${role}.effort`, null);
          open.values.set(`roles.${role}.perm`, null);
          open.values.set(`roles.${role}.agents`, others.slice(1).map((agent) => encodeLine({ agent, model: null, effort: null, fast: false, perm: null })));
          const added = newLine(open.document(), ROSTER, role);
          expect(added?.agent).toBe(target.id);
          expect(added?.model === null, "only a program that needs a provider gets a default model").toBe(!target.needs_provider);
          open.apply(lineWrites(open.document(), role, [...workerLines(open.document(), role), added!]));
          const refused = open.seen.filter((reason) => !reason.endsWith(CROSS_VENDOR));
          expect(refused, refused.join("; ")).toEqual([]);
        });
      }
    }
  }

  it("changing a program writes the old program's model, level and mode out before the new program in", () => {
    const writes = selectionWrites(
      "worker",
      { agent: "claude", model: "opus", effort: "high", fast: false, perm: "plan" },
      { agent: "pi", model: "Command Code/z-ai/glm-5.3-flash", effort: null, fast: false, perm: null },
    );
    expect(writes.map((write) => `${write.key.split(".")[2]}=${String(write.value)}`)).toEqual([
      "model=null",
      "effort=null",
      "perm=null",
      "agent=pi",
      "model=Command Code/z-ai/glm-5.3-flash",
    ]);
  });

  it("a program that needs a provider gets a model of another vendor than the one to avoid", () => {
    expect(defaultModel(ROSTER, "pi", [])).toBe("Command Code/deepseek/deepseek-v4.1-flash");
    expect(defaultModel(ROSTER, "pi", ["deepseek"])).toBe("Command Code/z-ai/glm-5.3-flash");
    expect(defaultModel(ROSTER, "claude", [])).toBeNull();
  });
});
