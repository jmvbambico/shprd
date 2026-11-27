// The settings page's reading of `GET /api/settings` and `GET /api/roster`: one
// row per role field, each value with the layer it came from, and the choices the
// pickers offer.
//
// Shepherdr's window draws its own settings page from one settings object and
// saves on every click (no Save step); this module keeps that rule and changes
// only where the data comes from:
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/shepherdr-settings/main.js` (the page's `row()`/`scopeLine()`
//   anatomy, the role row's model button, the write-with-no-Save-step rule),
//   `core/settings.js` (`roleSel`, `chooseModel`, `modelLabel`)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// shprd's own shape: the service resolves the layers and prints every value with
// the layer that answered it (`GET /api/settings`), and the pickers' choices come
// from the harness table and the model catalogs (`GET /api/roster`). So this
// file never decides what a role is set to — it reads what the service resolved
// — and never invents a program, a model or an effort level.
//
// Shared: the source is `mods/lib/settings.js`; `bin/sync-shared` copies it into
// every mod folder as `lib/settings.js`. Edit the source, never a copy.

import { reason } from "./client.js";
import { ROLE_DEFAULTS } from "./tokens.js";

/**
 * The layer words the service's `source` names, verbatim.
 *
 * They are `shprd_core::settings::Source::label()`'s words, so a page and
 * `shprd settings show` say the same thing about the same value.
 */
export const LAYER = {
  task: "this task",
  repo_entry: "this repo",
  you: "you",
  legacy: "your old config file",
  repo_file: "the repo's file",
  built_in: "built in",
};

/** The word a value with no layer gets: nothing sets it. */
export const UNSET_LAYER = "nothing sets it";

/** The role ids this build ships, in their default order (`BUILTIN_ROLES`). */
export const BUILTIN = ["coordinator", "worker", "reviewer"];

/** What the Agents page heads a built-in role with, until the person names it: a worker and a reviewer are lists. */
export const HEADINGS = { worker: "Worker (s)", reviewer: "Reviewer (s)" };

/** The three fields a role's pick is made of, in the order a page draws them. */
export const PICKS = ["agent", "model", "effort"];

/**
 * What one pick is called, the one line under it, and the words under an empty
 * one: the three parts of Shepherdr's row anatomy (a title, a line about it, and
 * the value at the right).
 */
export const PICK = {
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

/** The words for what one write did (`SettingsOutcomeDto`). */
export const OUTCOME = {
  stored: "Saved",
  equal_to_inherited: "That is what it inherits anyway, so the override here was removed",
  removed: "Removed: it inherits again",
  was_unset: "It was already unset",
};

/** The role id a `roles.<id>.<field>` key names, or null for another key. */
export function roleOf(key) {
  const match = /^roles\.([^.]+)\.([^.]+)$/.exec(String(key ?? ""));
  return match ? { id: match[1], field: match[2] } : null;
}

/** The words a source shows beside its value. */
export function layerOf(source) {
  return LAYER[source] ?? UNSET_LAYER;
}

/** Every settings row, by key, as the page reads it. */
function rowsOf(document) {
  const rows = new Map();
  for (const row of document?.values ?? []) rows.set(row.key, row);
  return rows;
}

/** The value of one row when it is text (a program, a model, a level). */
function textOf(row) {
  return typeof row?.value === "string" ? row.value : null;
}

/** The value of one row when it is a list of words (`roles.order`). */
function listOf(row) {
  return Array.isArray(row?.value) ? row.value.filter((item) => typeof item === "string") : null;
}

/**
 * The role ids the page shows, in the order the settings resolve them.
 *
 * `roles.order` always resolves — the built-in layer sets it — so a page with no
 * settings file at all still shows the four roles this build ships. A role whose
 * keys are in the document but not in the order is appended rather than lost: a
 * half-written order must not hide a role the person made.
 */
export function roleIds(document) {
  const rows = rowsOf(document);
  const order = listOf(rows.get("roles.order")) ?? [...BUILTIN];
  const seen = new Set(order);
  const extra = [];
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
export function programOf(roster, id) {
  if (!id) return null;
  return (roster?.programs ?? []).find((program) => program.id === id) ?? null;
}

/** The choices a program picker offers: every program, with its vendor. */
export function programChoices(roster) {
  return (roster?.programs ?? []).map((program) => ({
    value: program.id,
    label: program.name,
    hint: program.vendor,
  }));
}

/**
 * The choices a model picker offers for one program.
 *
 * The first choice is the program's own default, which is what an unset model
 * means; the rest are the models its catalog lists. A program that lists no
 * model still offers the default, and its `models_note` says why the rest is
 * missing (a page then offers a typed id).
 */
export function modelChoices(program) {
  if (!program) return [];
  const choices = [{ value: null, label: PICK.model.empty, hint: null }];
  for (const model of program.models ?? []) {
    choices.push({
      value: model.id,
      label: model.name === model.id ? model.id : model.name,
      hint: model.name === model.id ? null : model.id,
    });
  }
  return choices;
}

/**
 * The effort levels a model takes, or the program's own levels when the catalog
 * does not say: `[]` when there is nothing to pick, which a page reads as "this
 * program takes no effort level" rather than drawing an empty menu.
 */
export function effortLevels(program, model) {
  if (!program) return [];
  const found = (program.models ?? []).find((choice) => choice.id === model);
  if (found?.efforts?.length) return found.efforts;
  return program.efforts ?? [];
}

/** The choices an effort picker offers for one program and model. */
export function effortChoices(program, model) {
  const levels = effortLevels(program, model);
  if (levels.length === 0) return [];
  return [
    { value: null, label: PICK.effort.empty, hint: null },
    ...levels.map((level) => ({ value: level, label: level, hint: null })),
  ];
}

/**
 * One pick: what it is set to, the layer that set it, and what it may become.
 *
 * `value` is what a write would store (a string), and `display` is what the row
 * shows — the program's name rather than its id, the model's name rather than
 * its id where the catalog names another. `note` is the one line a picker adds
 * under itself when there is something to say: a program that lists no models,
 * a model that needs its provider, no program picked yet.
 */
function pickOf(role, field, rows, roster) {
  const key = `roles.${role.id}.${field}`;
  const row = rows.get(key);
  const agentRow = rows.get(`roles.${role.id}.agent`);
  const agent = textOf(agentRow);
  const model = textOf(rows.get(`roles.${role.id}.model`));
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

/**
 * Every role the page draws, in order: its identity, its picks and whether it is
 * one this build ships.
 *
 * The identity (title, about, kind) is read from the resolved settings, so a
 * person's own title or a custom role's line shows exactly as the service
 * resolved it; nothing here writes those fields, and the page draws them as the
 * words the role has rather than as controls.
 */
export function roles(document, roster) {
  const rows = rowsOf(document);
  return roleIds(document).map((id) => {
    const titleRow = rows.get(`roles.${id}.title`);
    const stored = (textOf(titleRow) ?? "").trim();
    // What the person reads: their own name for the role, else its built-in word,
    // else its id (the service's `display_title`).
    // The service reports the built-in word as a built-in row: that one is headed
    // "Worker (s)"; a name the person set stands as they wrote it.
    const named = stored !== "" && titleRow?.source !== "built_in";
    const title = named ? stored : HEADINGS[id] || ROLE_DEFAULTS[id] || id;
    const name = {
      key: `roles.${id}.title`,
      value: stored,
      fallback: HEADINGS[id] ?? ROLE_DEFAULTS[id] ?? id,
      source: titleRow?.source ?? null,
      layer: layerOf(titleRow?.source),
    };
    const about = textOf(rows.get(`roles.${id}.about`)) ?? "";
    const kind = textOf(rows.get(`roles.${id}.kind`)) ?? "helper";
    const readonly = rows.get(`roles.${id}.readonly`)?.value === true;
    const prompt = textOf(rows.get(`roles.${id}.prompt`)) ?? "";
    // The prompt a role starts with, when it is one of the templates (`prompt:<id>`).
    const promptId = prompt.startsWith("prompt:") ? prompt.slice("prompt:".length) : null;
    const base = { id, title, name, about, kind, readonly, promptId, custom: !BUILTIN.includes(id) };
    return {
      ...base,
      picks: PICKS.map((field) => pickOf(base, field, rows, roster)),
      ...extrasOf(base, rows, roster),
    };
  });
}

/**
 * The rest of a role's fields as controls: Fast and Permissions.
 *
 * Each is offered only where it means something, so none is drawn dead:
 * Fast where the role's program has a fast mode, Permissions where its harness
 * table lists access modes. A worker's and a reviewer's several programs are
 * their lines, `workerLines`.
 */
function extrasOf(role, rows, roster) {
  const agentRow = rows.get(`roles.${role.id}.agent`);
  const agent = textOf(agentRow);
  const program = programOf(roster, agent);
  const out = { fast: null, perm: null };
  const fastRow = rows.get(`roles.${role.id}.fast`);
  if (program?.fast === true) {
    out.fast = {
      key: `roles.${role.id}.fast`,
      label: "Fast mode",
      about: `${program.name}'s higher-priority mode: quicker, at a higher price.`,
      value: fastRow?.value === true,
      display: fastRow?.value === true ? "On" : "Off",
      source: fastRow?.source ?? null,
      layer: layerOf(fastRow?.source),
    };
  }
  const permRow = rows.get(`roles.${role.id}.perm`);
  if (program && (program.permissions ?? []).length > 0) {
    const value = textOf(permRow);
    const fallbackId = program.default_permission ?? null;
    const named = (id) => program.permissions.find((entry) => entry.id === id)?.label ?? id;
    out.perm = {
      key: `roles.${role.id}.perm`,
      label: "Permissions",
      about: "How much the program may do without asking.",
      value,
      display: value ? named(value) : fallbackId ? `${named(fallbackId)} (the program's default)` : PICK.effort.empty,
      source: permRow?.source ?? null,
      layer: layerOf(permRow?.source),
      choices: [
        { value: null, label: fallbackId ? `${named(fallbackId)} (default)` : PICK.effort.empty, hint: null },
        ...program.permissions.map((entry) => ({ value: entry.id, label: entry.label, hint: null })),
      ],
    };
  }
  return out;
}

// -- program lines: the fallback chain of a worker or a reviewer -------------
//
// A worker or a reviewer holds an ordered list of program lines. The first is the role's own
// `agent`/`model`/`effort`/`fast`/`perm`; the rest are `roles.<role>.agents`, one
// `agent|model|effort|fast|perm` string each (the service's `WorkerLine`). A
// settings file written before lines existed has the other programs in
// `roles.<role>.fallback` (the reviewer's old picker too); they read as lines with the program's own defaults.

/** One stored line (`agent|model|effort|fast|perm`, or `program`, or `program/model`) as a selection, or null. */
export function parseLine(text) {
  const raw = String(text ?? "");
  const parts = raw.split("|");
  const head = parts.shift().trim();
  const some = (part) => (part === undefined || part.trim() === "" ? null : part.trim());
  if (parts.length === 0) {
    const at = head.indexOf("/");
    const agent = at < 0 ? head : head.slice(0, at).trim();
    const model = at < 0 ? null : some(head.slice(at + 1));
    return agent === "" ? null : { agent, model, effort: null, fast: false, perm: null };
  }
  if (head === "") return null;
  return {
    agent: head,
    model: some(parts[0]),
    effort: some(parts[1]),
    fast: ["1", "true"].includes((parts[2] ?? "").trim()),
    perm: some(parts[3]),
  };
}

/** The string a line is stored as. */
export function encodeLine(line) {
  return [line.agent, line.model ?? "", line.effort ?? "", line.fast ? "1" : "", line.perm ?? ""].join("|");
}

/**
 * The worker's program lines, first (the default) first: `[{ agent, model, effort,
 * fast, perm }]`. Empty while the worker names no program. A program is listed once.
 */
export function workerLines(document, roleId = "worker") {
  const rows = rowsOf(document);
  const first = roleSelection(document, roleId);
  const lines = first.agent ? [first] : [];
  const stored = listOf(rows.get(`roles.${roleId}.agents`)) ?? [];
  const more = stored.length > 0 ? stored : (listOf(rows.get(`roles.${roleId}.fallback`)) ?? []);
  for (const text of more) {
    const line = parseLine(text);
    if (line && !lines.some((kept) => kept.agent === line.agent)) lines.push(line);
  }
  return lines;
}

/**
 * The writes that make a role's lines `lines`: the first line into the role's own
 * fields, the rest into `agents`. The old `fallback` list is emptied when it held
 * anything, because `agents` replaces it. A write that changes nothing is left out.
 */
export function lineWrites(document, roleId, lines) {
  const before = roleSelection(document, roleId);
  const [first, ...rest] = lines;
  const writes = first ? selectionWrites(roleId, before, first) : [];
  const rows = rowsOf(document);
  const had = listOf(rows.get(`roles.${roleId}.agents`)) ?? [];
  const next = rest.map(encodeLine);
  if (JSON.stringify(had) !== JSON.stringify(next)) {
    writes.push({ key: `roles.${roleId}.agents`, value: next, prior: had.length > 0 ? had : null });
  }
  const old = listOf(rows.get(`roles.${roleId}.fallback`)) ?? [];
  if (old.length > 0) writes.push({ key: `roles.${roleId}.fallback`, value: [], prior: old });
  return writes;
}

/** The program a "+" adds a line for: the first detected program no line has, or null. */
export function nextLineProgram(document, roster, roleId = "worker") {
  const lines = workerLines(document, roleId);
  const used = new Set(lines.map((line) => line.agent));
  // A reviewer's new line is never a program its own rule would refuse.
  const blocked = reviewBlocks(document, roster, roleId, lines.length);
  return detectedPrograms(roster).find((program) => !used.has(program.id) && !(program.id in blocked))?.id ?? null;
}

/**
 * The vendor whose model a line runs: the model's own for a program that runs
 * several vendors' models (`pi`), else the program's. Null when it is not known.
 */
export function lineVendor(roster, line) {
  const program = programOf(roster, line?.agent);
  if (!program) return null;
  if (!program.multi_model) return program.vendor ?? null;
  return program.models.find((model) => model.id === line.model)?.vendor ?? null;
}

/** What a program the reviewer cannot use says on its row. */
export const SAME_VENDOR = "same vendor as a Worker (s) line";

/**
 * The programs a reviewer's line `at` cannot pick, with the reason: every program
 * whose own vendor is the worker's, so the cross-vendor rule can't be hit by a
 * click. The first line is held to the worker's first line; a later line to all
 * of the worker's lines (it reviews when the one before cannot). A program that
 * runs several vendors' models (`pi`) is never blocked: it gets a model of
 * another vendor. Only the `reviewer` role is held to the rule.
 */
export function reviewBlocks(document, roster, roleId, at) {
  if (roleId !== "reviewer") return {};
  const vendors = workerLines(document, "worker")
    .map((line) => lineVendor(roster, line))
    .filter(Boolean);
  if (vendors.length === 0) return {};
  const held = at === 0 ? vendors.slice(0, 1) : vendors;
  const out = {};
  for (const program of detectedPrograms(roster)) {
    if (program.multi_model || !program.vendor) continue;
    if (held.every((vendor) => vendor === program.vendor)) out[program.id] = SAME_VENDOR;
  }
  return out;
}

/**
 * The models of a multi-model program (`pi`) a reviewer's line `at` cannot pick,
 * as `{ "<program>/<model id>": reason }`: the ones of the worker's vendor.
 */
export function reviewModelBlocks(document, roster, roleId, at) {
  if (roleId !== "reviewer") return {};
  const vendors = workerLines(document, "worker")
    .map((line) => lineVendor(roster, line))
    .filter(Boolean);
  if (vendors.length === 0) return {};
  const held = at === 0 ? vendors.slice(0, 1) : vendors;
  const out = {};
  for (const program of detectedPrograms(roster)) {
    if (!program.multi_model) continue;
    for (const model of program.models ?? []) {
      if (model.vendor && held.every((vendor) => vendor === model.vendor)) {
        out[`${program.id}/${model.id}`] = SAME_VENDOR;
      }
    }
  }
  return out;
}

/**
 * A refusal in plain words for the page: no key paths, no backticks. The
 * service's own sentence stays in `refused()`; this is what is drawn.
 */
export function plainRefusal(text, roster = null) {
  const raw = String(text ?? "");
  const later = /later line `([^`]*)` and the worker `[^`]*` are both/.exec(raw);
  if (later) {
    const id = later[1].split("/")[0];
    const name = programOf(roster, id)?.name ?? id.charAt(0).toUpperCase() + id.slice(1);
    return `${name} is the same vendor as a Worker (s) line; pick another harness for review.`;
  }
  if (/the reviewer `[^`]*` and the worker `[^`]*` are both/.test(raw)) {
    return "The first Reviewer (s) line is the same vendor as a Worker (s) line; pick another harness for review.";
  }
  return raw.replace(/^`[^`]*` (would be refused|cannot be stored): /, "").replace(/`/g, "");
}

/**
 * The model a program is given when it is picked without one: none for a program
 * with a default of its own, but a program that needs a provider (`pi`) has none,
 * so it gets the first listed model, preferring one of a vendor not in `avoid` (the
 * other role's vendors) so the cross-vendor rule is not broken by the pick.
 */
export function defaultModel(roster, programId, avoid = []) {
  const program = programOf(roster, programId);
  if (!program || !program.needs_provider || program.models.length === 0) return null;
  const free = program.models.find((model) => !model.vendor || !avoid.includes(model.vendor));
  return (free ?? program.models[0]).id;
}

/** The line a "+" adds to a role: the next free program with a valid default model. */
export function newLine(document, roster, roleId = "worker") {
  const agent = nextLineProgram(document, roster, roleId);
  if (agent === null) return null;
  const other = roleId === "reviewer" ? "worker" : "reviewer";
  const avoid = workerLines(document, other)
    .map((line) => lineVendor(roster, line))
    .filter(Boolean);
  return { agent, model: defaultModel(roster, agent, avoid), effort: null, fast: false, perm: null };
}

/** The lines after line `at` (0-based) is replaced by `line`, or taken out when `line` is null. */
export function withLine(lines, at, line) {
  return line === null ? lines.filter((_, i) => i !== at) : lines.map((kept, i) => (i === at ? line : kept));
}

/**
 * What a worker line's button says: "Claude Code · Opus 5.5 · Medium". The program
 * leads, because a worker's lines are programs; a model the line leaves at the
 * program's default is left out.
 */
export function lineLabel(menus, sel, roster = null) {
  if (!sel?.agent) return SELECT_HARNESS;
  const program = menus?.programs?.[sel.agent];
  const base = program?.name ?? programOf(roster, sel.agent)?.name ?? sel.agent;
  const model = modelIn(menus, sel);
  const level = program ? levelOf(menus, sel) : sel.effort;
  return [base, sel.model ? (model?.name ?? sel.model) : null, level ? levelWord(level) : null]
    .filter(Boolean)
    .join(" · ");
}

/** Whether a role holds an ordered chain of program lines: workers and reviewers do. */
export function hasLines(roleId) {
  return roleId === "worker" || roleId === "reviewer";
}

/** The one note, at the bottom of the Agents page, on how a chain is walked. */
export const CHAIN_NOTE =
  "For workers and reviewers, the first line is the default. If it can't go on, the next one takes over.";

/** What an empty program button says. */
export const SELECT_HARNESS = "Select Harness";

/**
 * The order `roles.order` would have after `id` is added, or null when the id
 * cannot be a role or is already there.
 *
 * Adding a role is one write: the id in the order is what makes the role exist,
 * and the built-in layer it then resolves from is `shprd_core`'s own defaults
 * for a role it does not ship (`helper`, its title the id, its own prompt file).
 * The id rule is the core's (`check_role_id`): letters, digits, `-` or `_`, and
 * not `order`, which is the list itself.
 */
export function withRole(document, id) {
  const wanted = String(id ?? "").trim();
  if (!/^[A-Za-z0-9_-]+$/.test(wanted) || wanted === "order") return null;
  const order = roleIds(document);
  if (order.includes(wanted)) return null;
  return [...order, wanted];
}

/** The order `roles.order` would have without `id`. */
export function withoutRole(document, id) {
  return roleIds(document).filter((role) => role !== id);
}

/**
 * What a refused write says, verbatim.
 *
 * The service refuses a cross-vendor review, an unchecked role, a value a key
 * does not take and a settings file it cannot read, each in its own sentence —
 * and the person needs that sentence, not a summary of it. So the text is passed
 * through exactly, and only the key it was about travels beside it, so the row
 * that was refused can be marked.
 */
export function refused(key, error) {
  return { key, text: reason(error) };
}

/** What one accepted write did, in words a page can show. */
export function outcomeOf(reply) {
  const word = reply?.outcome ?? "stored";
  return { outcome: word, text: OUTCOME[word] ?? "Saved" };
}

/** What the Prompts page calls a prompt: the built-in ones by their words, any other by its id. */
export function promptName(id) {
  return { assistant: "Assistant", worker: "Worker", reviewer: "Reviewer", backup: "Takeover" }[id] ?? id;
}

/** The templates page's rows: one per prompt this machine has. */
export function promptRows(list) {
  return (list?.prompts ?? []).map((prompt) => ({
    id: prompt.id,
    name: promptName(prompt.id),
    about: prompt.about ?? "a prompt of your own",
    path: prompt.path,
    state: prompt.state ?? null,
    shipped: prompt.shipped === true,
    problem: prompt.problem ?? null,
    word: prompt.problem
      ? "Unreadable"
      : prompt.state === "edited"
        ? "Edited"
        : "Default",
    edited: prompt.state === "edited",
  }));
}

/**
 * One prompt's row for the editor's header: what the file is and what saving
 * does to it.
 *
 * `saved` is the answer a save or a reset gave, when one just happened; its
 * words say whether the file is the default text now, so a person never has to
 * guess whether Reset did anything.
 */
export function promptLine(prompt, saved = null) {
  const at = saved ?? prompt;
  if (!at) return "";
  return at.state === "edited"
    ? "Your words: an edit is never replaced by an upgrade."
    : "The default words: saving your own stores them over this file.";
}

/**
 * What the roles page says about the settings as a whole: the service's own
 * refusals and the reason the roles could not be checked, when either is there.
 *
 * Both are read straight from the document: a page that hid them would invite a
 * person to work in a pair of roles the engine will refuse at the next review.
 * The service's effort-level *warnings* are not on this answer — its check runs
 * without the catalogs, which is what a warning about a level needs — so the
 * page does not pretend to show them.
 */
export function health(document) {
  return {
    problems: document?.problems ?? [],
    notChecked: document?.not_checked ?? null,
  };
}

// -- the dialog's frame ------------------------------------------------------
//
// Settings is a window, not a tab (Shepherdr's own `openSettings()`): a list of
// pages on the left, the page on the right, and on the left of each page that a
// repository can own the level its writes go to. The pages, the level switch and
// the rows are data here so the dialog, the tests and the words a person reads
// are one thing.
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/shepherdr-settings/core/settings.js` (`SECTIONS`, `LEVEL`,
//   `TITLE`), `mods/shepherdr-settings/main.js` (`levelBar`, `row`, `scopeLine`)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik

/**
 * The window's pages, in the order its list draws them.
 *
 * Shepherdr's list is Agents, Tasks, Autopilot, Notify, Secrets, Machines,
 * Window, Web; shprd has four of those subjects today (`Remote` is Shepherdr's
 * Web). A page shprd has nothing to put in is not invented: the list is the
 * window's, and a person reading it is reading what is there.
 *
 * `icon` is the line icon beside the page's name (`mods/shprd-tabs/icons`).
 * `repo: true` marks a page whose settings a repository can own, which is what
 * puts the level switch over it.
 */
export const PAGES = [
  { id: "agents", title: "Agents", icon: "bot", repo: true },
  { id: "tasks", title: "Tasks", icon: "list-checks", repo: true },
  { id: "prompts", title: "Prompts", icon: "file-text" },
  { id: "remote", title: "Remote", icon: "globe" },
];

/** The window's title, over the pages (Shepherdr's `TITLE`). */
export const TITLE = "Settings";

/**
 * Where a write goes: your own settings, or this repository's entry.
 *
 * It is Shepherdr's level switch with shprd's two layers: `You` is the person's
 * own file (every repository, unless one sets its own), and `This repo` is the
 * entry for the repository this service serves. `for_repo` is what the write
 * sends, so the words and the write can never disagree.
 */
export const SCOPES = [
  { id: "you", word: "You", for_repo: false },
  { id: "repo", word: "This repo", for_repo: true },
];

/** The level switch's words, under it (Shepherdr's `LEVEL`). */
export const LEVEL = {
  label: "Applies to",
  you: "Your own settings: every repository, unless it sets its own.",
  repo: (name) =>
    name
      ? `Only this repository (${name}): what it does not set is inherited from you.`
      : "Only this repository: what it does not set is inherited from you.",
  one: "The same for every repository.",
};

/** The words a row shows when this scope set its own value, and the way back. */
export const CHANGED_HERE = "Set for this repository";
/** The one write that puts a row back to what it inherits. */
export const RESET = "Reset";

/** The page an id names, or the first page the window has. */
export function pageOf(id) {
  return PAGES.find((page) => page.id === id && !page.soon) ?? PAGES[0];
}

/** The scope an id names, or the person's own. */
export function scopeOf(id) {
  return SCOPES.find((scope) => scope.id === id) ?? SCOPES[0];
}

/** The words under the level switch for one scope. */
export function levelNote(scope, repository = null) {
  const found = scopeOf(scope);
  return found.for_repo ? LEVEL.repo(repository) : LEVEL.you;
}

/**
 * One number's bounds, as the service checks them (`TaskKey::range`).
 *
 * The dialog's stepper refuses to step outside these and the service refuses the
 * write anyway; both read the same numbers, so a click can never propose a value
 * the service would answer with a sentence instead of a write.
 */
export const RANGE = {
  "tasks.review_rounds": [0, 10],
  "tasks.max_running": [1, 10],
  "tasks.archive_after": [0, 3650],
  "tasks.tick_secs": [1, 3600],
  "tasks.news_settle_secs": [0, 3600],
};

/** The days a finished task may stay before it is archived (Shepherdr's `ARCHIVE`). */
export const ARCHIVE_PRESETS = [
  [0, "Off"],
  [1, "1 day"],
  [3, "3 days"],
  [7, "1 week"],
  [30, "1 month"],
];

/**
 * Settings keys the service stores but nothing reads, so no row draws them
 * rather than a control that does nothing. Empty now: `tasks.start_notes` and
 * `tasks.finish_notes` went into every worker's brief (SHPR-24), so they are
 * rows like the rest.
 */
export const HIDDEN = [];

/**
 * What is wrong with a branch-name pattern, in a sentence, or null when it is
 * one git accepts and one every task gets its own name from.
 *
 * The pattern may use `{run}` (the run's id), `{task}` (the task's id), `{n}`
 * (its number) and `{slug}` (its id, lowercased); it must use `{task}` or
 * `{slug}`, or two tasks would share a branch (`{n}` alone is the same for `T1` and `SHPR-1`). The rest is git's
 * own rule for a ref name (`git check-ref-format`).
 */
export function branchProblem(text) {
  const pattern = String(text ?? "").trim();
  if (pattern === "") return null;
  const left = pattern.replace(/\{(run|task|n|slug)\}/g, "");
  if (/[{}]/.test(left)) return "only {run}, {task}, {n} and {slug} may be used between braces";
  if (!/\{(task|slug)\}/.test(pattern)) return "use {task} or {slug}, or two tasks would share a branch";
  return refProblem(pattern.replace(/\{(run|task|n|slug)\}/g, "x"));
}

/** What is wrong with a git ref name (a base branch, or a pattern with its words filled), or null. */
export function refProblem(text) {
  const name = String(text ?? "").trim();
  if (name === "") return null;
  if (/[\s~^:?*[\\\x00-\x1f\x7f]/.test(name)) return "a branch name has no spaces or any of ~ ^ : ? * [ \\";
  if (name.startsWith("/") || name.endsWith("/") || name.includes("//")) {
    return "a branch name does not start or end with /, or hold //";
  }
  if (name.includes("..") || name.includes("@{")) return "a branch name has no .. or @{";
  if (name === "@" || name.startsWith("-")) return "a branch name does not start with - or be just @";
  for (const part of name.split("/")) {
    if (part.startsWith(".") || part.endsWith(".lock")) return "no part of a branch name starts with . or ends with .lock";
  }
  if (name.endsWith(".")) return "a branch name does not end with .";
  return null;
}

/** The check a key's box makes before it writes, or null: a sentence when the words are refused. */
export function boxProblem(key, text) {
  if (key === "tasks.branch") return branchProblem(text);
  if (key === "tasks.base") return refProblem(text);
  return null;
}

/**
 * The Tasks page's rows: the workflow keys `GET /api/settings` exposes, in the
 * core's order (`TASK_KEYS`, then the review's own switch).
 *
 * Each row says what it is, what it does in one line, and what kind of control
 * it takes: `choice` (a fixed set of words), `switch` (a yes or a no), `number`
 * (a stepper inside `RANGE`) or `text` (a box under the row, written when the
 * person leaves it).
 */
export const WORKFLOW = [
  {
    key: "tasks.merge",
    title: "When a task lands",
    about: "When a finished task is merged: when you accept it, once its checks pass, or at once.",
    kind: "choice",
    choices: [
      ["ask", "On Accept"],
      ["checks", "After checks pass"],
      ["auto", "Right away"],
    ],
  },
  {
    key: "tasks.review_rounds",
    title: "Review rounds",
    about: "How many times a task may go back to its worker before it needs you; Off skips the review.",
    kind: "number",
    zero: "Off",
  },
  {
    key: "tasks.must_fix",
    title: "Must-fix findings",
    about: "Findings this severe or worse fail a review.",
    kind: "choice",
    needsReview: true,
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
    about: "How long the janitor keeps an archived task's branch before letting it expire; Off keeps it for ever.",
    kind: "presets",
    choices: ARCHIVE_PRESETS,
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
    about: "The branch a task is cut from; empty cuts it from the run's integration branch.",
    kind: "text",
  },
  {
    key: "tasks.branch",
    title: "Branch names",
    about: "The pattern a task's branch is named after, under the archive branch prefix: {run}, {task}, {n} (its number) and {slug} (its id, lowercased).",
    kind: "text",
  },
  {
    key: "tasks.start_notes",
    title: "Start notes",
    about: "Words every worker reads before its task.",
    kind: "notes",
  },
  {
    key: "tasks.finish_notes",
    title: "Finish notes",
    about: "Words every worker reads before it reports.",
    kind: "notes",
  },
  {
    key: "tasks.worktree_files",
    title: "Files for worktrees",
    about: "Files copied into every worker's worktree from the main checkout, one per line (for example .env); write link: before a path to link it instead of copying.",
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
    key: "tasks.self_update",
    title: "Update shprd after a change to it",
    about: "When an accepted task changes shprd itself, shprd rebuilds, swaps in the new version and restarts once. A build that fails changes nothing; a new version that does not start is rolled back.",
    kind: "switch",
  },
  {
    key: "tasks.gh_user",
    title: "GitHub account",
    about: "The gh account landing and every forge call use for this repository (also written land.gh_user); empty uses whichever account gh has active.",
    kind: "text",
  },
  {
    key: "review.cross_vendor",
    title: "Cross-vendor review",
    about: "A reviewer never shares the worker's vendor, so the review is independent.",
    kind: "switch",
  },
];

/** The value one row holds: a string, a number, a boolean or a list. */
function valueOf(row) {
  if (!row) return null;
  const value = row.value;
  if (typeof value === "string" || typeof value === "boolean" || typeof value === "number") {
    return value;
  }
  return Array.isArray(value) ? value : null;
}

/** How a row's value reads at the right of its line. */
function displayOf(spec, value) {
  if (spec.kind === "switch") return value === true ? "On" : "Off";
  if (spec.kind === "choice") {
    const found = (spec.choices ?? []).find(([choice]) => choice === value);
    return found ? found[1] : String(value ?? "not set");
  }
  if (spec.kind === "presets") {
    if (value === null) return "not set";
    const found = (spec.choices ?? []).find(([choice]) => choice === value);
    return found ? found[1] : `${value} days`;
  }
  if (spec.kind === "number") {
    if (value === null) return "not set";
    if (spec.zero !== undefined && value === 0) return spec.zero;
    return spec.unit ? `${value} ${spec.unit}` : String(value);
  }
  const text = typeof value === "string" ? value : "";
  return text.trim() === "" ? "not set" : text;
}

/**
 * The Tasks page's rows, as the page draws them: what each key is set to, the
 * layer that answered it, and the choices or bounds its control offers.
 *
 * A key the service does not answer is still a row, with `value: null` and no
 * layer: a page that silently dropped it would let a person believe the setting
 * was gone. `control` says which widget: `segmented` (a `choice`), `switch`,
 * `stepper` (a `number`) or `box` (a `text` or `notes` field).
 */
export function workflowRows(document) {
  const rows = rowsOf(document);
  // With no review rounds nothing is reviewed, so what a review must fix has
  // nothing to decide and its row is not drawn.
  const reviewed = valueOf(rows.get("tasks.review_rounds")) !== 0;
  return WORKFLOW.filter((spec) => reviewed || !spec.needsReview).map((spec) => {
    const row = rows.get(spec.key);
    const value = valueOf(row);
    const control =
      spec.kind === "choice" || spec.kind === "presets"
        ? "segmented"
        : spec.kind === "switch"
          ? "switch"
          : spec.kind === "number"
            ? "stepper"
            : "box";
    const bounds = RANGE[spec.key] ?? null;
    // A value no preset names (set by hand) is still a segment, so the row shows it.
    const choices =
      spec.kind === "presets" && typeof value === "number" && !spec.choices.some(([choice]) => choice === value)
        ? [...spec.choices, [value, `${value} days`]]
        : spec.choices;
    return {
      ...spec,
      choices,
      control,
      value,
      display: displayOf(spec, value),
      layer: layerOf(row?.source),
      source: row?.source ?? null,
      min: bounds?.[0] ?? null,
      max: bounds?.[1] ?? null,
    };
  });
}

/**
 * The number a step of a stepper would write, clamped to the row's own bounds.
 *
 * The stepper's two buttons are the only way the dialog writes a number, so a
 * value the service would refuse cannot be proposed: the bounds are the Rust
 * `TaskKey::range`'s, and stepping past an end stays at it.
 */
export function stepped(value, by, key) {
  const [min, max] = RANGE[key] ?? [0, Number.MAX_SAFE_INTEGER];
  const from = Number.isFinite(Number(value)) ? Math.trunc(Number(value)) : min;
  return Math.max(min, Math.min(max, from + by));
}

/**
 * What the level switch writes with: whether a write goes to this repository's
 * entry, as `PUT /api/settings` takes it.
 */
export function forRepo(scope) {
  return scopeOf(scope).for_repo;
}

// -- the Remote page ---------------------------------------------------------
//
// Remote is the web app's Remote page (`web/src/settings/RemotePage.tsx`) in the
// window: the tunnel (`GET /api/tunnels`, `POST /api/tunnels/start|stop`), the
// devices signed in (`GET /api/sessions`, `DELETE /api/sessions/<id>`) and Log
// out of all of them. The service decides every rule; the page only shows its
// answers, and a tunnel's public link is masked until the person asks for it and
// is never written to a log.

/** The link with its secret part hidden: the scheme and the last label remain. */
export function maskLink(link) {
  const match = /^(https?:\/\/)([^/]+)(.*)$/.exec(String(link ?? ""));
  if (!match) return "••••••••";
  const host = match[2];
  const dot = host.indexOf(".");
  const rest = dot > 0 ? host.slice(dot) : "";
  return `${match[1]}••••••${rest}/••••`;
}

/** "5 min ago" for a time in epoch seconds. */
export function ago(seconds, now = Date.now() / 1000) {
  const gap = Math.max(0, Math.round(now - Number(seconds)));
  if (gap < 60) return "just now";
  if (gap < 3600) return `${Math.floor(gap / 60)} min ago`;
  if (gap < 86_400) return `${Math.floor(gap / 3600)} h ago`;
  return `${Math.floor(gap / 86_400)} d ago`;
}

/**
 * The tunnel section as the page draws it: the state word, whether a start is
 * possible with the provider picked, and the words that say why not.
 */
export function tunnelView(status, picked = null) {
  if (!status) return null;
  const provider = picked ?? status.default_provider ?? status.providers?.[0]?.id ?? null;
  const chosen = (status.providers ?? []).find((entry) => entry.id === provider) ?? null;
  const needsPassword = chosen?.public_internet === true && status.password_set !== true;
  return {
    on: status.on === true,
    starting: status.starting === true,
    word: status.on ? "On" : status.starting ? "Starting…" : "Off",
    provider,
    providers: (status.providers ?? []).map((entry) => ({
      id: entry.id,
      word: entry.public_internet ? `${entry.id} · public` : entry.id,
    })),
    live: status.on ? (status.provider ?? null) : null,
    version: status.version ?? null,
    link: status.on ? (status.link ?? null) : null,
    why: status.on ? null : (status.why ?? null),
    tailnetUser: status.tailnet_user ?? null,
    needsPassword,
    note: needsPassword ? "A public tunnel needs a login password first: run `shprd web password set`." : null,
  };
}

/** The signed-in devices, one row each, with this one marked. */
export function sessionRows(list, now = Date.now() / 1000) {
  return (list?.sessions ?? []).map((session) => ({
    id: session.id,
    label: session.label,
    here: session.id === list.current,
    remembered: session.remember === true,
    seen: `last seen ${ago(session.last_seen, now)}`,
  }));
}

/** The longest name a role may have: the service's own cap (`TITLE_MAX` in `shprd-core`). */
export const TITLE_MAX = 24;

/**
 * A typed role name as it is written: trimmed, runs of space as one, cut to
 * [`TITLE_MAX`]. Empty means "no name", which the service takes as "back to the
 * built-in one".
 */
export function cleanTitle(text) {
  return String(text ?? "")
    .split(/\s+/)
    .filter(Boolean)
    .join(" ")
    .slice(0, TITLE_MAX)
    .trimEnd();
}

/**
 * Whether a change to this key reaches a coordinator that is already running only
 * when it starts again: its program and everything that shapes how it starts.
 */
export function needsCoordinatorRestart(key) {
  return /^roles\.coordinator\.(agent|model|effort|fast|perm)$/.test(String(key ?? ""));
}

// -- the model menu ----------------------------------------------------------
//
// The Agents page is one compact row per role with a single "<Model> · <Effort>"
// button, and the button opens a popover: program tabs, a search over every
// program's models, the models with a check on the current one, Reasoning, Fast
// and Permissions. This is Shepherdr's model menu (`mods/chat-models/main.js`,
// `core/settings.js` `modelLabel`), with one difference the person asked for:
// Shepherdr's tabs are a fixed Claude/Codex pair, and these are the programs the
// service's roster says are installed on this machine (`ProgramDto.installed`,
// from `GET /api/roster`). A program that is not installed has no tab, so a
// choice that cannot start is never offered. The pure rules are here so the
// popover, the page and the tests are one thing; none of it touches `herdr`.
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/chat-models/main.js` (the menu), `core/settings.js`
//   (`modelLabel`, `chooseModel`)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik

/** The words for an effort level, as Shepherdr's menu spells them. */
export const LEVEL_WORDS = {
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
export function levelWord(level) {
  return LEVEL_WORDS[level] ?? String(level ?? "");
}

/**
 * The programs the menu has tabs for: the roster's programs that are installed,
 * in the roster's order. Undetected programs are hidden, never shown disabled.
 */
export function detectedPrograms(roster) {
  return (roster?.programs ?? []).filter((program) => program.installed === true);
}

/**
 * The menu's data from the roster: `{ order, programs }`, one entry per
 * detected program with its models, levels, access modes and whether it has a
 * fast mode. `order` is the tab order.
 */
export function menusOf(roster) {
  const programs = {};
  const order = [];
  for (const program of detectedPrograms(roster)) {
    order.push(program.id);
    programs[program.id] = {
      id: program.id,
      name: program.name,
      vendor: program.vendor,
      needsProvider: program.needs_provider === true,
      note: program.models_note ?? null,
      efforts: program.efforts ?? [],
      fast: program.fast === true,
      perms: (program.permissions ?? []).map((entry) => ({ id: entry.id, label: entry.label ?? entry.id })),
      defaultPerm: program.default_permission ?? null,
      models: (program.models ?? []).map((model) => ({
        id: model.id,
        name: model.name || model.id,
        efforts: model.efforts ?? [],
        defaultEffort: model.default_effort ?? null,
        fast: model.fast === true,
      })),
    };
  }
  return { order, programs };
}

/**
 * A role's current choice, as the service resolved it:
 * `{ agent, model, effort, fast, perm }` (a missing value is `null`).
 */
export function roleSelection(document, roleId) {
  const rows = rowsOf(document);
  const text = (field) => textOf(rows.get(`roles.${roleId}.${field}`));
  return {
    agent: text("agent"),
    model: text("model"),
    effort: text("effort"),
    fast: rows.get(`roles.${roleId}.fast`)?.value === true,
    perm: text("perm"),
  };
}

/** The model of a selection in a menu, or null (an id the catalog does not list). */
export function modelIn(menus, sel) {
  return menus?.programs?.[sel?.agent]?.models?.find((model) => model.id === sel.model) ?? null;
}

/**
 * The levels the Reasoning row offers for a selection: the model's own, else the
 * program's. `[]` hides the row.
 */
export function levelsFor(menus, sel) {
  const program = menus?.programs?.[sel?.agent];
  if (!program) return [];
  const model = modelIn(menus, sel);
  return model?.efforts?.length ? model.efforts : program.efforts;
}

/** The level that applies: the chosen one, else the model's default. */
export function levelOf(menus, sel) {
  return sel?.effort ?? modelIn(menus, sel)?.defaultEffort ?? null;
}

/** Whether the Fast switch is offered: where the model, or an unlisted model's program, has it. */
export function fastOffered(menus, sel) {
  const program = menus?.programs?.[sel?.agent];
  if (!program) return false;
  const model = modelIn(menus, sel);
  return model ? model.fast : program.fast;
}

/**
 * What the role's button says: "Opus 5.5 · Medium", the model alone where there
 * is no level, "Select Harness" while nothing is chosen. The program's name
 * stands for a model the role leaves at the program's default.
 */
export function selectionLabel(menus, sel, roster = null) {
  if (!sel?.agent) return SELECT_HARNESS;
  const program = menus?.programs?.[sel.agent];
  const known = programOf(roster, sel.agent);
  const base = program?.name ?? known?.name ?? sel.agent;
  const model = modelIn(menus, sel);
  const name = sel.model ? (model?.name ?? sel.model) : base;
  const level = program ? levelOf(menus, sel) : sel.effort;
  return level ? `${name} · ${levelWord(level)}` : name;
}

/**
 * The rows of the model list: the models of the tab's program, or — when the
 * person has typed — every detected program's models that match, then, for a
 * word no model has as its id, one "Use <id>" row for the tab's program (the way
 * to pick a model of a program that lists none, like `pi`).
 */
export function modelRows(menus, tab, query) {
  const word = String(query ?? "").trim();
  const lower = word.toLowerCase();
  const rows = [];
  const from = lower === "" ? [tab] : menus.order;
  for (const agent of from) {
    for (const model of menus.programs[agent]?.models ?? []) {
      if (lower === "" || model.name.toLowerCase().includes(lower) || model.id.toLowerCase().includes(lower)) {
        rows.push({ agent, id: model.id, name: model.name, typed: false });
      }
    }
  }
  // "Use <word>" unless the word is already a listed model's id or name.
  const exact = rows.some((row) => row.id.toLowerCase() === lower || row.name.toLowerCase() === lower);
  if (word !== "" && !exact && menus.programs[tab]) rows.push({ agent: tab, id: word, name: `Use ${word}`, typed: true });
  return rows;
}

/** The selection after a model is picked: effort and speed stay where the model has them. */
export function chooseModel(menus, sel, agent, id) {
  const next = { ...sel, agent, model: id };
  const model = modelIn(menus, next);
  const levels = levelsFor(menus, next);
  if (next.effort && !levels.includes(next.effort)) next.effort = null;
  if (!fastOffered(menus, next)) next.fast = false;
  // Another program has its own access modes: the old one is kept only if it has it.
  if (agent !== sel.agent && !(menus.programs[agent]?.perms ?? []).some((entry) => entry.id === next.perm)) next.perm = null;
  if (agent !== sel.agent && model === null) next.fast = false;
  return next;
}

/**
 * The writes that take a role from one selection to another, in the order the
 * service should hear them: program, model, effort, speed, access. A value that
 * did not change is not written; `null` takes the key out of the scope's layer
 * (it inherits again).
 */
export function selectionWrites(roleId, before, after) {
  const writes = [];
  const put = (field, value, prior) => writes.push({ key: `roles.${roleId}.${field}`, value, prior });
  const same = (field) =>
    field === "fast" ? Boolean(after.fast) === Boolean(before.fast) : (after[field] ?? null) === (before[field] ?? null);
  if ((after.agent ?? null) !== (before.agent ?? null)) {
    // A new program is never asked to take the old one's model, level or access
    // mode (`pi` refuses `pi/opus`, and `ultra` is not a level it lists), so what
    // the old program had is taken out first and the new values go in after it.
    if (before.model) put("model", null, before.model);
    if (before.effort) put("effort", null, before.effort);
    if (before.fast) put("fast", false, true);
    if (before.perm) put("perm", null, before.perm);
    put("agent", after.agent ?? null, before.agent ?? null);
    if (after.model) put("model", after.model, null);
    if (after.effort) put("effort", after.effort, null);
    if (after.fast) put("fast", true, false);
    if (after.perm) put("perm", after.perm, null);
    return writes;
  }
  if (!same("model")) put("model", after.model ?? null, before.model ?? null);
  if (!same("effort")) put("effort", after.effort ?? null, before.effort ?? null);
  if (!same("fast")) put("fast", Boolean(after.fast), Boolean(before.fast));
  if (!same("perm")) put("perm", after.perm ?? null, before.perm ?? null);
  return writes;
}

/**
 * What a write put back would be to leave a scope's layer as it was: the value the
 * layer itself held, or `null` (take the key out) when the value came from another
 * layer or nothing set it. Used to undo the writes of a choice whose later write
 * the service refused, so a role is never left half switched.
 */
export function priorInScope(document, key, forRepo) {
  const row = (document?.values ?? []).find((entry) => entry.key === key);
  const mine = forRepo ? "repo_entry" : "you";
  return row && row.source === mine ? (row.value ?? null) : null;
}

/**
 * The program a Reviewer's "Use other CLI" switches to: an installed program of
 * another vendor than the worker's, the first the roster lists; null when the
 * reviewer already is one or there is none (the button is then not drawn).
 */
export function otherCli(document, roster) {
  const worker = textOf(rowsOf(document).get("roles.worker.agent"));
  const reviewer = textOf(rowsOf(document).get("roles.reviewer.agent"));
  const vendor = programOf(roster, worker)?.vendor ?? null;
  if (vendor === null) return null;
  if (reviewer !== null && programOf(roster, reviewer)?.vendor !== vendor) return null;
  return detectedPrograms(roster).find((program) => program.vendor !== vendor)?.id ?? null;
}

/**
 * Whether a coordinator is running now, from `GET /api/state`'s `live_agents`.
 * A state that does not say (an older service) counts as running, so the notice
 * errs towards offering the restart.
 */
export function coordinatorRunning(state) {
  if (!Array.isArray(state?.live_agents)) return true;
  return state.live_agents.some(
    (agent) => agent.role === "coordinator" && !agent.ended_ms && agent.state !== "done",
  );
}

/**
 * Whether the inline "restart the coordinator" notice shows: the coordinator's
 * program, model, effort, speed or access was changed here since it started, and
 * one is running. Nothing else shows it.
 */
export function restartNotice({ changed, running }) {
  return Boolean(changed) && Boolean(running);
}

/** The keys of a role a repository sets for itself (what "Reset" would put back). */
export function repoKeysOf(document, roleId) {
  return (document?.values ?? [])
    .filter((row) => row.source === "repo_entry" && String(row.key).startsWith(`roles.${roleId}.`))
    .map((row) => row.key);
}

/** The shared key a popup answers on, read by the Settings window: `{ key, field, value, at }`. */
export const PICK_KEY = "shprd.settings.pick";

/** The model menu's width, and its height at its tallest: border and padding, the program dropdown, search, heading, 8 rows, rule, Reasoning, Fast, Permissions, a note. */
export const MENU_W = 460;
export const MENU_H = 2 + 10 + 34 + 36 + 26 + 8 * 31 + 20 + 9 + 26 + 34 + 31 + 31;

/**
 * The popup's height for these menus: as tall as the menu gets for the longest
 * model list a tab has (at most 8 rows, then it scrolls), so the transparent
 * popup is not much taller than what it shows and a click beside the menu
 * reaches the page.
 */
export function menuHeight(menus, { light = false, none = false } = {}) {
  // The program list takes the models' place while it is open, so it counts too.
  const longest = Math.max(1, menus.order.length, ...menus.order.map((id) => menus.programs[id].models.length));
  const full = MENU_H - (8 - Math.min(8, longest)) * 31;
  // A light menu has no rule, Reasoning, Fast or Permissions.
  const lighter = light ? full - (9 + 26 + 34 + 31 + 31) : full;
  return lighter + (none ? 31 : 0);
}

/**
 * The words a model button shows for a selection: "Opus 5.5 · Medium", the model's
 * name and the reasoning level that applies (Shepherdr's chat button), "Fast"
 * after them when it is on. A model the catalog does not list shows its id; no
 * model at all shows the program's name.
 */
export function pickLabel(menus, sel) {
  if (!sel?.agent) return "";
  const program = menus?.programs?.[sel.agent];
  const model = modelIn(menus, sel);
  // A model the catalog does not list shows its own name, not its provider path (`pi`'s ids are paths).
  const name = model?.name ?? (sel.model ? String(sel.model).split("/").pop() : null) ?? program?.name ?? sel.agent;
  const level = levelOf(menus, sel);
  return [name, level ? levelWord(level) : null, sel.fast ? "Fast" : null].filter(Boolean).join(" · ");
}
