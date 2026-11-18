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
    const title = textOf(rows.get(`roles.${id}.title`)) ?? id;
    const about = textOf(rows.get(`roles.${id}.about`)) ?? "";
    const kind = textOf(rows.get(`roles.${id}.kind`)) ?? "helper";
    const readonly = rows.get(`roles.${id}.readonly`)?.value === true;
    const base = { id, title, about, kind, readonly, custom: !BUILTIN.includes(id) };
    return {
      ...base,
      picks: PICKS.map((field) => pickOf(base, field, rows, roster)),
      ...extrasOf(base, rows, roster),
    };
  });
}

/**
 * The rest of a role's fields as controls: Fast, Permissions and Fallback.
 *
 * Each is offered only where it means something, so none is drawn dead:
 * Fast where the role's program has a fast mode, Permissions where its harness
 * table lists access modes, and Fallback on the worker role (the tick
 * reads the worker's list, `roles.worker.fallback`). `backup` is the Backup
 * agent role's own switch: on while the role names a program.
 */
function extrasOf(role, rows, roster) {
  const agentRow = rows.get(`roles.${role.id}.agent`);
  const agent = textOf(agentRow);
  const program = programOf(roster, agent);
  const out = { fast: null, perm: null, fallback: null, backup: null };
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
  const fallbackRow = rows.get(`roles.${role.id}.fallback`);
  if (role.id === "worker") {
    const value = listOf(fallbackRow) ?? [];
    out.fallback = {
      key: `roles.${role.id}.fallback`,
      label: "Fallback",
      about: "Programs tried in this order when the first cannot go on.",
      value,
      display: value.length === 0 ? "None" : value.map((id) => programOf(roster, id)?.name ?? id).join(" → "),
      source: fallbackRow?.source ?? null,
      layer: layerOf(fallbackRow?.source),
      choices: (roster?.programs ?? [])
        .filter((entry) => entry.id !== agent)
        .map((entry) => ({ value: entry.id, label: entry.name, hint: entry.installed ? entry.vendor : "not installed" })),
    };
  }
  if (role.id === "backup") {
    out.backup = {
      key: "roles.backup.agent",
      label: "Backup agent",
      about: "The other program takes a task over, once, when its worker cannot go on.",
      on: agent !== null,
      source: agentRow?.source ?? null,
      layer: layerOf(agentRow?.source),
    };
  }
  return out;
}

/**
 * The program a Backup switch turns on with: another program than the worker's,
 * preferably another vendor and one that is installed, so a takeover is both
 * possible and independent. Null when the roster has nothing to offer.
 */
export function backupChoice(document, roster) {
  const worker = textOf(rowsOf(document).get("roles.worker.agent"));
  const others = (roster?.programs ?? []).filter((program) => program.id !== worker);
  const vendor = programOf(roster, worker)?.vendor ?? null;
  const rank = (program) => (program.installed ? 0 : 2) + (program.vendor !== vendor ? 0 : 1);
  const sorted = [...others].sort((a, b) => rank(a) - rank(b));
  return sorted[0]?.id ?? null;
}

/** The list a Fallback pick would write after `id` is toggled in `list`. */
export function toggled(list, id) {
  const have = Array.isArray(list) ? list : [];
  return have.includes(id) ? have.filter((entry) => entry !== id) : [...have, id];
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
 * (its number) and `{slug}` (its id, lowercased); it must use `{task}`, `{n}` or
 * `{slug}`, or two tasks would share a branch. The rest is git's
 * own rule for a ref name (`git check-ref-format`).
 */
export function branchProblem(text) {
  const pattern = String(text ?? "").trim();
  if (pattern === "") return null;
  const left = pattern.replace(/\{(run|task|n|slug)\}/g, "");
  if (/[{}]/.test(left)) return "only {run}, {task}, {n} and {slug} may be used between braces";
  if (!/\{(task|n|slug)\}/.test(pattern)) return "use {task}, {n} or {slug}, or two tasks would share a branch";
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
    about: "How long a finished task stays in your lists before the janitor archives it.",
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
