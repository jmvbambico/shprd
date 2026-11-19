// The settings pages' reading of `GET /api/settings`, `GET /api/roster` and
// `GET /api/prompts`: one row per role field, each value with the layer that
// answered it, and the choices the pickers offer.
//
// This is a port of the Herdr desktop's `shprd-settings` mod (`lib/settings.js`),
// which is itself adapted from Shepherdr, so the window and the web say the same
// words about the same value. The web invents no row the window lacks.
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/shepherdr-settings/main.js` (page anatomy, level switch, row
//   words), `mods/shepherdr-settings/core/settings.js` (`SECTIONS`, `LEVEL`,
//   `TITLE`, `roleSel`, `chooseModel`)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// The service resolves the layers; this file never decides what a role is set
// to, and never invents a program, a model or an effort level.

import type {
  ProgramDto,
  PromptListDto,
  RosterDto,
  SettingsDto,
  SettingsOutcomeDto,
  SettingsSourceDto,
  SettingsValueDto,
} from "../gen";
import { roleTitle } from "../roles";

/** One settings value as the service types it. */
export type SettingValue = SettingsValueDto["value"];

/** The layer words the service's `source` names, as `shprd settings show` says them. */
export const LAYER: Record<SettingsSourceDto, string> = {
  task: "this task",
  repo_entry: "this repo",
  you: "you",
  legacy: "your old config file",
  repo_file: "the repo's file",
  built_in: "built in",
};

/** The word a value with no layer gets: nothing sets it. */
export const UNSET_LAYER = "nothing sets it";

/** The role ids this build ships, in their default order. */
export const BUILTIN = ["coordinator", "worker", "reviewer", "backup"];

/** The three fields a role's pick is made of, in the order a page draws them. */
export const PICKS = ["agent", "model", "effort"] as const;

/** One of a role's three picks. */
export type PickField = (typeof PICKS)[number];

/** What one pick is called, the line under it, and the words under an empty one. */
export const PICK: Record<PickField, { label: string; about: string; empty: string }> = {
  agent: {
    label: "Program",
    about: "The program this role runs.",
    empty: "Not chosen yet",
  },
  model: {
    label: "Model",
    about: "Which of the program's models it runs, or its own default.",
    empty: "The program's default",
  },
  effort: {
    label: "Effort",
    about: "How hard the program thinks, where it offers levels.",
    empty: "The program's default",
  },
};

/** The words for what one write did. */
export const OUTCOME: Record<SettingsOutcomeDto, string> = {
  stored: "Saved",
  equal_to_inherited: "That is what it would inherit, so nothing was stored",
  removed: "Removed: it inherits again",
  was_unset: "It was already unset",
};

/** The role id and field a `roles.<id>.<field>` key names, or null for another key. */
export function roleOf(key: string): { id: string; field: string } | null {
  const match = /^roles\.([^.]+)\.([^.]+)$/.exec(key);
  return match && match[1] !== undefined && match[2] !== undefined
    ? { id: match[1], field: match[2] }
    : null;
}

/** The words a source shows beside its value. */
export function layerOf(source: SettingsSourceDto | null | undefined): string {
  return source ? (LAYER[source] ?? UNSET_LAYER) : UNSET_LAYER;
}

/** Every settings row, by key. */
function rowsOf(document: SettingsDto | null): Map<string, SettingsValueDto> {
  const rows = new Map<string, SettingsValueDto>();
  for (const row of document?.values ?? []) {
    rows.set(row.key, row);
  }
  return rows;
}

/** A row's value when it is text. */
function textOf(row: SettingsValueDto | undefined): string | null {
  return typeof row?.value === "string" ? row.value : null;
}

/** A row's value when it is a list of words. */
function listOf(row: SettingsValueDto | undefined): string[] | null {
  return Array.isArray(row?.value)
    ? row.value.filter((item): item is string => typeof item === "string")
    : null;
}

/**
 * The role ids the page shows, in the order the settings resolve them.
 *
 * A role whose keys are in the document but not in the order is appended rather
 * than lost: a half-written order must not hide a role the person made.
 */
export function roleIds(document: SettingsDto | null): string[] {
  const rows = rowsOf(document);
  const order = listOf(rows.get("roles.order")) ?? [...BUILTIN];
  const seen = new Set(order);
  const extra: string[] = [];
  for (const key of rows.keys()) {
    const found = roleOf(key);
    if (found && !seen.has(found.id)) {
      seen.add(found.id);
      extra.push(found.id);
    }
  }
  return [...order, ...extra];
}

/** One program from the roster, by id, or null. */
export function programOf(roster: RosterDto | null, id: string | null): ProgramDto | null {
  if (!id) {
    return null;
  }
  return roster?.programs.find((program) => program.id === id) ?? null;
}

/** One choice a picker offers. `value: null` is "inherit / the program's default". */
export interface Choice {
  /** What a write stores. */
  value: string | null;
  /** The words shown. */
  label: string;
  /** A second, dim word (a vendor, or the id behind a name). */
  hint: string | null;
}

/** The choices a program picker offers: every program, with its vendor. */
export function programChoices(roster: RosterDto | null): Choice[] {
  return (roster?.programs ?? []).map((program) => ({
    value: program.id,
    label: program.name,
    hint: program.vendor,
  }));
}

/**
 * The choices a model picker offers for one program: its own default first, then
 * the models its catalog lists.
 */
export function modelChoices(program: ProgramDto | null): Choice[] {
  if (!program) {
    return [];
  }
  const choices: Choice[] = [{ value: null, label: PICK.model.empty, hint: null }];
  for (const model of program.models) {
    choices.push({
      value: model.id,
      label: model.name === model.id ? model.id : model.name,
      hint: model.name === model.id ? null : model.id,
    });
  }
  return choices;
}

/** The effort levels a model takes, or the program's own when the catalog is silent. */
export function effortLevels(program: ProgramDto | null, model: string | null): string[] {
  if (!program) {
    return [];
  }
  const found = program.models.find((choice) => choice.id === model);
  if (found && found.efforts.length > 0) {
    return found.efforts;
  }
  return program.efforts;
}

/** The choices an effort picker offers for one program and model. */
export function effortChoices(program: ProgramDto | null, model: string | null): Choice[] {
  const levels = effortLevels(program, model);
  if (levels.length === 0) {
    return [];
  }
  return [
    { value: null, label: PICK.effort.empty, hint: null },
    ...levels.map((level) => ({ value: level, label: level, hint: null })),
  ];
}

/** One pick: what it is set to, the layer that set it, and what it may become. */
export interface Pick {
  /** Which of the three. */
  field: PickField;
  /** The settings key, `roles.<id>.<field>`. */
  key: string;
  /** The row's title. */
  label: string;
  /** The line under the title. */
  about: string;
  /** What is stored, or null. */
  value: string | null;
  /** The layer that answered. */
  source: SettingsSourceDto | null;
  /** The layer's words. */
  layer: string;
  /** What the row shows at its right. */
  display: string;
  /** What the picker offers. */
  choices: Choice[];
  /** One line under the picker when there is something to say. */
  note: string | null;
}

/** One role as the Agents page draws it. */
export interface Role {
  /** The role's id. */
  id: string;
  /** What the app calls it: the person's name for it, else the built-in word. */
  title: string;
  /** The name as stored (empty while the role has its built-in one), and the word it falls back to. */
  name: { key: string; value: string; fallback: string; layer: string; source: string | null };
  /** What it does, in one line. */
  about: string;
  /** `worker`, `reviewer`, `helper`… */
  kind: string;
  /** Whether it only reads. */
  readonly: boolean;
  /** Whether the person made it (not one this build ships). */
  custom: boolean;
  /** The template this role starts with (`prompt:<id>`), or null (a path). */
  promptId: string | null;
  /** Program, model, effort. */
  picks: Pick[];
}

function pickOf(
  roleId: string,
  field: PickField,
  rows: Map<string, SettingsValueDto>,
  roster: RosterDto | null,
): Pick {
  const key = `roles.${roleId}.${field}`;
  const row = rows.get(key);
  const agent = textOf(rows.get(`roles.${roleId}.agent`));
  const model = textOf(rows.get(`roles.${roleId}.model`));
  const program = programOf(roster, agent);
  const value = textOf(row);
  const base = {
    field,
    key,
    label: PICK[field].label,
    about: PICK[field].about,
    value,
    source: row?.source ?? null,
    layer: layerOf(row?.source),
  };
  if (field === "agent") {
    return {
      ...base,
      display: program?.name ?? value ?? PICK.agent.empty,
      choices: programChoices(roster),
      note: value === null ? "Pick the program this role runs; nothing is chosen yet." : null,
    };
  }
  if (field === "model") {
    return {
      ...base,
      display: value ?? PICK.model.empty,
      choices: modelChoices(program),
      note: !program
        ? "Pick a program first."
        : program.needs_provider && !value
          ? `${program.name} wants the provider in the id: provider/model`
          : (program.models_note ?? null),
    };
  }
  return {
    ...base,
    display: value ?? PICK.effort.empty,
    choices: effortChoices(program, model),
    note: !program
      ? "Pick a program first."
      : effortLevels(program, model).length === 0
        ? `${program.name} takes no effort level.`
        : null,
  };
}

function promptIdOf(prompt: string | null): string | null {
  return prompt?.startsWith("prompt:") ? prompt.slice("prompt:".length) : null;
}

/** Every role the page draws, in order. */
export function roles(document: SettingsDto | null, roster: RosterDto | null): Role[] {
  const rows = rowsOf(document);
  return roleIds(document).map((id) => {
    const titleRow = rows.get(`roles.${id}.title`);
    const stored = (textOf(titleRow) ?? "").trim();
    return {
    id,
    title: stored || roleTitle(null, id),
    name: {
      key: `roles.${id}.title`,
      value: stored,
      fallback: roleTitle(null, id),
      layer: layerOf(titleRow?.source),
      source: titleRow?.source ?? null,
    },
    about: textOf(rows.get(`roles.${id}.about`)) ?? "",
    kind: textOf(rows.get(`roles.${id}.kind`)) ?? "helper",
    readonly: rows.get(`roles.${id}.readonly`)?.value === true,
    custom: !BUILTIN.includes(id),
    promptId: promptIdOf(textOf(rows.get(`roles.${id}.prompt`))),
    picks: PICKS.map((field) => pickOf(id, field, rows, roster)),
    };
  });
}

/**
 * The order `roles.order` would have after `id` is added, or null when the id
 * cannot be a role or is already there (letters, digits, `-` or `_`; not `order`).
 */
export function withRole(document: SettingsDto | null, id: string): string[] | null {
  const wanted = id.trim();
  if (!/^[A-Za-z0-9_-]+$/.test(wanted) || wanted === "order") {
    return null;
  }
  const order = roleIds(document);
  if (order.includes(wanted)) {
    return null;
  }
  return [...order, wanted];
}

/** The order `roles.order` would have without `id`. */
export function withoutRole(document: SettingsDto | null, id: string): string[] {
  return roleIds(document).filter((role) => role !== id);
}

/** What one accepted write did, in words a page can show. */
export function outcomeOf(outcome: SettingsOutcomeDto | undefined): string {
  return OUTCOME[outcome ?? "stored"] ?? "Saved";
}

/** One row of the Prompts list. */
export interface PromptRow {
  /** The id. */
  id: string;
  /** What it is for. */
  about: string;
  /** The file. */
  path: string;
  /** The word beside the id. */
  word: "Unreadable" | "Edited" | "Shipped";
  /** Why the file cannot be used, when it cannot. */
  problem: string | null;
  /** Whether the words are the person's own. */
  edited: boolean;
}

/** The Prompts page's rows: one per prompt this machine has. */
export function promptRows(list: PromptListDto | null): PromptRow[] {
  return (list?.prompts ?? []).map((prompt) => ({
    id: prompt.id,
    about: prompt.about ?? "a prompt of your own",
    path: prompt.path,
    problem: prompt.problem ?? null,
    word: prompt.problem ? "Unreadable" : prompt.state === "edited" ? "Edited" : "Shipped",
    edited: prompt.state === "edited",
  }));
}

/** What the editor's header says about the file. */
export function promptLine(state: "default" | "edited"): string {
  return state === "edited"
    ? "Your words: an edit is never replaced by an upgrade."
    : "The shipped words: saving your own stores them over this file.";
}

/** What the roles say about the settings as a whole. */
export function health(document: SettingsDto | null): {
  problems: string[];
  notChecked: string | null;
} {
  return { problems: document?.problems ?? [], notChecked: document?.not_checked ?? null };
}

/** The window's title, over the pages. */
export const TITLE = "Settings";

/** One page of the dialog. */
export interface Page {
  /** Its id. */
  id: "agents" | "tasks" | "prompts" | "remote";
  /** Its title. */
  title: string;
  /** Whether a repository can own its settings (puts the level switch over it). */
  repo?: boolean;
}

/** The dialog's pages, in the order its list draws them. */
export const PAGES: Page[] = [
  { id: "agents", title: "Agents", repo: true },
  { id: "tasks", title: "Tasks", repo: true },
  { id: "prompts", title: "Prompts" },
  { id: "remote", title: "Remote" },
];

/** Where a write goes: your own settings, or this repository's entry. */
export const SCOPES = [
  { id: "you", word: "You", for_repo: false },
  { id: "repo", word: "This repo", for_repo: true },
] as const;

/** A scope's id. */
export type ScopeId = (typeof SCOPES)[number]["id"];

/** The level switch's words. */
export const LEVEL = {
  label: "Applies to",
  you: "Your own settings: every repository, unless it sets its own.",
  repo: (name: string | null) =>
    name
      ? `Only this repository (${name}): what it does not set is inherited from you.`
      : "Only this repository: what it does not set is inherited from you.",
  one: "The same for every repository.",
};

/** The words a row shows when this scope set its own value. */
export const CHANGED_HERE = "Set for this repository";
/** The one write that puts a row back to what it inherits. */
export const RESET = "Reset";

/** The words under the level switch for one scope. */
export function levelNote(scope: ScopeId, repository: string | null): string {
  return scope === "repo" ? LEVEL.repo(repository) : LEVEL.you;
}

/** Whether a write at this scope goes to the repository's entry. */
export function forRepo(scope: ScopeId): boolean {
  return scope === "repo";
}

/** One number's bounds, as the service checks them. */
export const RANGE: Record<string, [number, number]> = {
  "tasks.review_rounds": [1, 10],
  "tasks.max_running": [1, 10],
  "tasks.archive_after": [0, 3650],
  "tasks.tick_secs": [1, 3600],
  "tasks.news_settle_secs": [0, 3600],
};

/** What kind of control one workflow row takes. */
export type WorkflowKind = "choice" | "switch" | "number" | "text" | "notes";

/** One Tasks-page row's fixed words. */
export interface WorkflowSpec {
  /** The settings key. */
  key: string;
  /** Title. */
  title: string;
  /** One line about it. */
  about: string;
  /** Control kind. */
  kind: WorkflowKind;
  /** `[value, label]` pairs for a choice. */
  choices?: Array<[string, string]>;
  /** A unit word for a number. */
  unit?: string;
}

/** The Tasks page's rows, in the core's order. */
export const WORKFLOW: WorkflowSpec[] = [
  {
    key: "tasks.merge",
    title: "When a task lands",
    about: "What has to be true before a finished task is merged.",
    kind: "choice",
    choices: [
      ["ask", "Ask me"],
      ["checks", "Checks pass"],
      ["auto", "By itself"],
    ],
  },
  {
    key: "tasks.review_rounds",
    title: "Review rounds",
    about: "How many times a task may go back to its worker before it needs you.",
    kind: "number",
  },
  {
    key: "tasks.must_fix",
    title: "Must-fix findings",
    about: "Findings this severe or worse fail a review.",
    kind: "choice",
    choices: [
      ["critical", "Critical"],
      ["major", "Major"],
      ["minor", "Minor"],
      ["trivial", "Trivial"],
    ],
  },
  {
    key: "tasks.proof",
    title: "Ask for proof",
    about: "Each task hands in one artifact of real proof with its work.",
    kind: "switch",
  },
  {
    key: "tasks.max_running",
    title: "Tasks at once",
    about: "How many tasks this run may work on at the same time.",
    kind: "number",
  },
  {
    key: "tasks.archive_after",
    title: "Archive done tasks",
    about: "Days a finished task stays in your lists; 0 keeps it for ever.",
    kind: "number",
    unit: "days",
  },
  {
    key: "tasks.archive_namespace",
    title: "Archive branches as",
    about: "Where a branch the janitor removes is kept.",
    kind: "text",
  },
  {
    key: "tasks.worktrees",
    title: "A worktree per task",
    about: "Each task works in a worktree of its own, off a branch of its own.",
    kind: "switch",
  },
  {
    key: "tasks.base",
    title: "Base branch",
    about: "The branch a task is cut from, when the repository names none.",
    kind: "text",
  },
  {
    key: "tasks.branch",
    title: "Branch names",
    about: "The pattern a task's branch is named after.",
    kind: "text",
  },
  {
    key: "tasks.start_notes",
    title: "When a task starts",
    about: "Words every task's brief carries before its own work.",
    kind: "notes",
  },
  {
    key: "tasks.finish_notes",
    title: "Before a task reports",
    about: "Words every task's brief carries before it hands in.",
    kind: "notes",
  },
  {
    key: "tasks.tick_secs",
    title: "Time between passes",
    about: "Seconds between two passes of the service's tick.",
    kind: "number",
    unit: "seconds",
  },
  {
    key: "tasks.news_settle_secs",
    title: "Let news settle",
    about: "Seconds a change holds before the coordinator is told about it.",
    kind: "number",
    unit: "seconds",
  },
  {
    key: "review.cross_vendor",
    title: "Cross-vendor review",
    about: "A reviewer never shares the worker's vendor, so the review is independent.",
    kind: "switch",
  },
];

/** One Tasks row as the page draws it. */
export interface WorkflowRow extends WorkflowSpec {
  /** The value the service resolved. */
  value: SettingValue;
  /** How it reads at the right of the line. */
  display: string;
  /** The layer's words. */
  layer: string;
  /** The layer. */
  source: SettingsSourceDto | null;
  /** Lower bound of a number. */
  min: number | null;
  /** Upper bound of a number. */
  max: number | null;
}

function displayOf(spec: WorkflowSpec, value: SettingValue): string {
  if (spec.kind === "switch") {
    return value === true ? "On" : "Off";
  }
  if (spec.kind === "choice") {
    const found = (spec.choices ?? []).find(([choice]) => choice === value);
    return found ? found[1] : String(value ?? "not set");
  }
  if (spec.kind === "number") {
    if (value === null) {
      return "not set";
    }
    return spec.unit ? `${String(value)} ${spec.unit}` : String(value);
  }
  const text = typeof value === "string" ? value : "";
  return text.trim() === "" ? "not set" : text;
}

/** The Tasks page's rows: what each key is set to and the layer that answered. */
export function workflowRows(document: SettingsDto | null): WorkflowRow[] {
  const rows = rowsOf(document);
  return WORKFLOW.map((spec) => {
    const row = rows.get(spec.key);
    const value = row?.value ?? null;
    const bounds = RANGE[spec.key] ?? null;
    return {
      ...spec,
      value,
      display: displayOf(spec, value),
      layer: layerOf(row?.source),
      source: row?.source ?? null,
      min: bounds?.[0] ?? null,
      max: bounds?.[1] ?? null,
    };
  });
}

/** The number a step of a stepper would write, clamped to the row's own bounds. */
export function stepped(value: SettingValue, by: number, key: string): number {
  const [min, max] = RANGE[key] ?? [0, Number.MAX_SAFE_INTEGER];
  const from = Number.isFinite(Number(value)) && value !== null ? Math.trunc(Number(value)) : min;
  return Math.max(min, Math.min(max, from + by));
}
