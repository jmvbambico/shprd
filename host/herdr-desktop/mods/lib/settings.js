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
export const BUILTIN = ["coordinator", "worker", "reviewer", "backup"];

/** The three fields a role's pick is made of, in the order a page draws them. */
export const PICKS = ["agent", "model", "effort"];

/** What one pick is called, and the words under an empty one. */
export const PICK = {
  agent: { label: "Program", empty: "Not chosen yet" },
  model: { label: "Model", empty: "The program's default" },
  effort: { label: "Effort", empty: "The program's default" },
};

/** The words for what one write did (`SettingsOutcomeDto`). */
export const OUTCOME = {
  stored: "Saved",
  equal_to_inherited: "That is what it would inherit, so nothing was stored",
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
    const title = textOf(rows.get(`roles.${id}.title`)) ?? id;
    const about = textOf(rows.get(`roles.${id}.about`)) ?? "";
    const kind = textOf(rows.get(`roles.${id}.kind`)) ?? "helper";
    const readonly = rows.get(`roles.${id}.readonly`)?.value === true;
    const base = { id, title, about, kind, readonly, custom: !BUILTIN.includes(id) };
    return {
      ...base,
      picks: PICKS.map((field) => pickOf(base, field, rows, roster)),
    };
  });
}

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

/** The templates page's rows: one per prompt this machine has. */
export function promptRows(list) {
  return (list?.prompts ?? []).map((prompt) => ({
    id: prompt.id,
    about: prompt.about ?? "a prompt of your own",
    path: prompt.path,
    state: prompt.state ?? null,
    shipped: prompt.shipped === true,
    problem: prompt.problem ?? null,
    word: prompt.problem
      ? "Unreadable"
      : prompt.state === "edited"
        ? "Edited"
        : "Shipped",
    edited: prompt.state === "edited",
  }));
}

/**
 * One prompt's row for the editor's header: what the file is and what saving
 * does to it.
 *
 * `saved` is the answer a save or a reset gave, when one just happened; its
 * words say whether the file is the shipped text now, so a person never has to
 * guess whether Reset did anything.
 */
export function promptLine(prompt, saved = null) {
  const at = saved ?? prompt;
  if (!at) return "";
  return at.state === "edited"
    ? "Your words: an edit is never replaced by an upgrade."
    : "The shipped words: saving your own stores them over this file.";
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
