// The model menu's rules: what the Agents page's one "<Model> · <Effort>" button
// opens, and what picking in it writes.
//
// A port of the Herdr desktop's `mods/lib/settings.js` (the model-menu section),
// which is adapted from Shepherdr's model menu, so the window and the web agree.
// The one difference from Shepherdr is the person's own: its program tabs are a
// fixed Claude/Codex pair, and these are the programs the roster says are
// installed on this machine (`ProgramDto.installed`). An undetected program has no
// tab.
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/chat-models/main.js` (the menu), `core/settings.js`
//   (`modelLabel`, `chooseModel`)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik

import type { RosterDto, SettingsDto, SnapshotDto } from "../gen";
import { programOf } from "./model";

function textOfKey(document: SettingsDto | null, key: string): string | null {
  const value = document?.values.find((row) => row.key === key)?.value;
  return typeof value === "string" ? value : null;
}

/** The words for an effort level, as Shepherdr's menu spells them. */
export const LEVEL_WORDS: Record<string, string> = {
  off: "Off",
  none: "None",
  minimal: "Minimal",
  low: "Low",
  medium: "Medium",
  high: "High",
  xhigh: "Extra high",
  max: "Max",
  ultra: "Ultra",
};

/** One effort level in words. */
export function levelWord(level: string): string {
  return LEVEL_WORDS[level] ?? level;
}

/** One model in the menu. */
export interface MenuModel {
  id: string;
  name: string;
  efforts: string[];
  defaultEffort: string | null;
  fast: boolean;
}

/** One program's tab: its models, levels and access modes. */
export interface MenuProgram {
  id: string;
  name: string;
  vendor: string;
  needsProvider: boolean;
  note: string | null;
  efforts: string[];
  fast: boolean;
  perms: Array<{ id: string; label: string }>;
  defaultPerm: string | null;
  models: MenuModel[];
}

/** The menu's data: `order` is the tab order. */
export interface Menus {
  order: string[];
  programs: Record<string, MenuProgram>;
}

/** A role's choice: a missing value is `null`. */
export interface Selection {
  agent: string | null;
  model: string | null;
  effort: string | null;
  fast: boolean;
  perm: string | null;
}

/** The roster's programs that are installed, in its order. */
export function detectedPrograms(roster: RosterDto | null) {
  return (roster?.programs ?? []).filter((program) => program.installed);
}

/** The menu's data from the roster: one tab per detected program, never a fixed pair. */
export function menusOf(roster: RosterDto | null): Menus {
  const menus: Menus = { order: [], programs: {} };
  for (const program of detectedPrograms(roster)) {
    menus.order.push(program.id);
    menus.programs[program.id] = {
      id: program.id,
      name: program.name,
      vendor: program.vendor,
      needsProvider: program.needs_provider,
      note: program.models_note ?? null,
      efforts: program.efforts,
      fast: program.fast,
      perms: program.permissions.map((entry) => ({ id: entry.id, label: entry.label || entry.id })),
      defaultPerm: program.default_permission ?? null,
      models: program.models.map((model) => ({
        id: model.id,
        name: model.name || model.id,
        efforts: model.efforts,
        defaultEffort: model.default_effort ?? null,
        fast: model.fast,
      })),
    };
  }
  return menus;
}

function text(document: SettingsDto | null, key: string): string | null {
  const value = document?.values.find((row) => row.key === key)?.value;
  return typeof value === "string" ? value : null;
}

/** A role's current choice, as the service resolved it. */
export function roleSelection(document: SettingsDto | null, roleId: string): Selection {
  return {
    agent: text(document, `roles.${roleId}.agent`),
    model: text(document, `roles.${roleId}.model`),
    effort: text(document, `roles.${roleId}.effort`),
    fast: document?.values.find((row) => row.key === `roles.${roleId}.fast`)?.value === true,
    perm: text(document, `roles.${roleId}.perm`),
  };
}

/** The model of a selection in a menu, or null (an id the catalog does not list). */
export function modelIn(menus: Menus, sel: Selection): MenuModel | null {
  return menus.programs[sel.agent ?? ""]?.models.find((model) => model.id === sel.model) ?? null;
}

/** The levels the Reasoning row offers: the model's own, else the program's; `[]` hides the row. */
export function levelsFor(menus: Menus, sel: Selection): string[] {
  const program = menus.programs[sel.agent ?? ""];
  if (!program) {
    return [];
  }
  const model = modelIn(menus, sel);
  return model && model.efforts.length > 0 ? model.efforts : program.efforts;
}

/** The level that applies: the chosen one, else the model's default. */
export function levelOf(menus: Menus, sel: Selection): string | null {
  return sel.effort ?? modelIn(menus, sel)?.defaultEffort ?? null;
}

/** Whether the Fast switch is offered. */
export function fastOffered(menus: Menus, sel: Selection): boolean {
  const program = menus.programs[sel.agent ?? ""];
  if (!program) {
    return false;
  }
  const model = modelIn(menus, sel);
  return model ? model.fast : program.fast;
}

/** What the role's button says: "Opus 5.5 · Medium", or "Choose a program" while nothing is chosen. */
export function selectionLabel(menus: Menus, sel: Selection, roster: RosterDto | null = null): string {
  if (!sel.agent) {
    return "Choose a program";
  }
  const program = menus.programs[sel.agent];
  const base = program?.name ?? programOf(roster, sel.agent)?.name ?? sel.agent;
  const model = modelIn(menus, sel);
  const name = sel.model ? (model?.name ?? sel.model) : base;
  const level = program ? levelOf(menus, sel) : sel.effort;
  return level ? `${name} · ${levelWord(level)}` : name;
}

/** One row of the model list. */
export interface ModelRow {
  agent: string;
  id: string;
  name: string;
  typed: boolean;
}

/**
 * The rows of the model list: the tab's models, or — once the person types — every
 * detected program's matching models, then one "Use <id>" row for the tab's program
 * (how a program that lists no models, like `pi`, gets a model at all).
 */
export function modelRows(menus: Menus, tab: string, query: string): ModelRow[] {
  const word = query.trim();
  const lower = word.toLowerCase();
  const rows: ModelRow[] = [];
  for (const agent of lower === "" ? [tab] : menus.order) {
    for (const model of menus.programs[agent]?.models ?? []) {
      if (lower === "" || model.name.toLowerCase().includes(lower) || model.id.toLowerCase().includes(lower)) {
        rows.push({ agent, id: model.id, name: model.name, typed: false });
      }
    }
  }
  // "Use <word>" unless the word is already a listed model's id or name.
  const exact = rows.some((row) => row.id.toLowerCase() === lower || row.name.toLowerCase() === lower);
  if (word !== "" && !exact && menus.programs[tab]) {
    rows.push({ agent: tab, id: word, name: `Use ${word}`, typed: true });
  }
  return rows;
}

/** The selection after a model is picked: effort and speed stay where the model has them. */
export function chooseModel(menus: Menus, sel: Selection, agent: string, id: string, extra?: MenuModel): Selection {
  const next: Selection = { ...sel, agent, model: id };
  const scope: Menus = extra
    ? {
        ...menus,
        programs: {
          ...menus.programs,
          [agent]: { ...menus.programs[agent]!, models: [extra, ...(menus.programs[agent]?.models ?? [])] },
        },
      }
    : menus;
  const levels = levelsFor(scope, next);
  if (next.effort && !levels.includes(next.effort)) {
    next.effort = null;
  }
  if (!fastOffered(scope, next)) {
    next.fast = false;
  }
  if (agent !== sel.agent && !(menus.programs[agent]?.perms ?? []).some((entry) => entry.id === next.perm)) {
    next.perm = null;
  }
  return next;
}

/** One key to write; `null` takes it out of the scope's layer. */
export interface Write {
  key: string;
  value: string | boolean | null;
  /** What the key held before, to put back if a later write of the same choice is refused. */
  prior?: string | boolean | null;
}

/** The writes that take a role from one selection to another: program, model, effort, speed, access. */
export function selectionWrites(roleId: string, before: Selection, after: Selection): Write[] {
  const writes: Write[] = [];
  const put = (field: string, value: string | boolean | null, prior: string | boolean | null) =>
    writes.push({ key: `roles.${roleId}.${field}`, value, prior });
  if (after.agent !== before.agent) put("agent", after.agent ?? null, before.agent ?? null);
  if (after.model !== before.model) put("model", after.model ?? null, before.model ?? null);
  if ((after.effort ?? null) !== (before.effort ?? null)) put("effort", after.effort ?? null, before.effort ?? null);
  if (after.fast !== before.fast) put("fast", after.fast, before.fast);
  if ((after.perm ?? null) !== (before.perm ?? null)) put("perm", after.perm ?? null, before.perm ?? null);
  return writes;
}

/**
 * What putting a key back would write to leave a scope's layer as it was: the value
 * that layer itself held, or `null` (take the key out) when it came from another
 * layer or nothing set it.
 */
export function priorInScope(document: SettingsDto | null, key: string, forRepo: boolean): string | boolean | null {
  const row = document?.values.find((entry) => entry.key === key);
  const value = row?.value;
  const mine = forRepo ? "repo_entry" : "you";
  return row?.source === mine && (typeof value === "string" || typeof value === "boolean") ? value : null;
}

/** The program "Use other CLI" switches the reviewer to, or null (already another vendor, or none). */
export function otherCli(document: SettingsDto | null, roster: RosterDto | null): string | null {
  const worker = text(document, "roles.worker.agent");
  const reviewer = text(document, "roles.reviewer.agent");
  const vendor = programOf(roster, worker)?.vendor ?? null;
  if (vendor === null) {
    return null;
  }
  if (reviewer !== null && programOf(roster, reviewer)?.vendor !== vendor) {
    return null;
  }
  return detectedPrograms(roster).find((program) => program.vendor !== vendor)?.id ?? null;
}

/** Whether a coordinator is running now, from `GET /api/state`'s `live_agents`. */
export function coordinatorRunning(state: Pick<SnapshotDto, "live_agents"> | null): boolean {
  if (!state || !Array.isArray(state.live_agents)) {
    return true;
  }
  return state.live_agents.some(
    (agent) => agent.role === "coordinator" && !agent.ended_ms && agent.state !== "done",
  );
}

/** Whether the inline restart notice shows: the coordinator's choice changed here, and one is running. */
export function restartNotice(args: { changed: boolean; running: boolean }): boolean {
  return args.changed && args.running;
}

/** Whether a change to this key reaches a running coordinator only when it starts again. */
export function needsCoordinatorRestart(key: string): boolean {
  return /^roles\.coordinator\.(agent|model|effort|fast|perm)$/.test(key);
}

/** The keys of a role a repository sets for itself (what Reset puts back). */
export function repoKeysOf(document: SettingsDto | null, roleId: string): string[] {
  return (document?.values ?? [])
    .filter((row) => row.source === "repo_entry" && row.key.startsWith(`roles.${roleId}.`))
    .map((row) => row.key);
}

/**
 * The program a Backup switch turns on with: another program than the worker's,
 * preferably another vendor and one that is installed. Null when there is none.
 */
export function backupChoice(document: SettingsDto | null, roster: RosterDto | null): string | null {
  const worker = textOfKey(document, "roles.worker.agent");
  const vendor = programOf(roster, worker)?.vendor ?? null;
  const others = (roster?.programs ?? []).filter((program) => program.id !== worker);
  const rank = (program: { installed: boolean; vendor: string }) =>
    (program.installed ? 0 : 2) + (program.vendor !== vendor ? 0 : 1);
  return [...others].sort((a, b) => rank(a) - rank(b))[0]?.id ?? null;
}
