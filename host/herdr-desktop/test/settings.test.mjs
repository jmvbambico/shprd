// Unit tests for the settings page's view model: the roles table, the layers its
// values came from, the choices the pickers offer, a refusal's words and the
// template list.
//
//   node --import ./test/boot.mjs --test test/          (node 22+)
//   bun test --preload ./test/boot-bun.mjs test/        (bun)
//
// The fixtures are a real `shprd serve`'s answers, captured from a throwaway run
// (`test/fixtures/settings.json`, `roster.json`, `prompts.json`): a person's own
// file with picks for every role and one custom role, the shipped harness table
// with its catalogs, and the four templates this build ships. Nothing here
// invents a shape the service does not send.

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { ServiceError, reason } from "../mods/lib/client.js";
import {
  BUILTIN,
  LAYER,
  PICKS,
  UNSET_LAYER,
  effortChoices,
  effortLevels,
  health,
  layerOf,
  modelChoices,
  outcomeOf,
  programChoices,
  programOf,
  promptLine,
  promptRows,
  putBody,
  refused,
  roleIds,
  roleOf,
  roles,
  withRole,
  withoutRole,
} from "../mods/lib/settings.js";

/** One fixture, parsed. */
function fixture(name) {
  return JSON.parse(readFileSync(fileURLToPath(new URL(`./fixtures/${name}`, import.meta.url)), "utf8"));
}

/** The settings document a real `GET /api/settings` answered. */
const SETTINGS = fixture("settings.json");
/** The programs a real `GET /api/roster` answered. */
const ROSTER = fixture("roster.json");
/** The templates a real `GET /api/prompts` answered. */
const PROMPTS = fixture("prompts.json");

/** One role from the page's rows, by id. */
function role(id) {
  return roles(SETTINGS, ROSTER).find((entry) => entry.id === id);
}

/** One pick of one role. */
function pick(id, field) {
  return role(id).picks.find((entry) => entry.field === field);
}

// -- the roles table -------------------------------------------------------

test("the roles are the ones the settings resolve, in their order", () => {
  assert.deepEqual(roleIds(SETTINGS), ["coordinator", "worker", "reviewer", "backup", "docs"]);
  assert.deepEqual(
    roles(SETTINGS, ROSTER).map((entry) => entry.title),
    ["Assistant", "Worker", "Reviewer", "Backup agent", "docs"],
  );
  assert.deepEqual(role("worker").picks.map((entry) => entry.field), PICKS);
});

test("a role this build does not ship reads as the person's own", () => {
  assert.equal(role("docs").custom, true);
  assert.equal(role("docs").kind, "helper");
  for (const id of BUILTIN) {
    assert.equal(role(id).custom, false, id);
  }
  assert.equal(role("reviewer").readonly, true, "the reviewer only reads");
});

test("one settings file with no order still shows the roles this build ships", () => {
  const shipped = SETTINGS.values.filter(
    (row) => row.key !== "roles.order" && !row.key.startsWith("roles.docs."),
  );
  assert.deepEqual(roleIds({ values: shipped }), BUILTIN);
  // A role whose keys are in the file but not in the order is kept, not lost.
  const stray = {
    values: [...shipped, { key: "roles.handyman.agent", value: "pi", source: "you" }],
  };
  assert.deepEqual(roleIds(stray), [...BUILTIN, "handyman"]);
});

test("every value says the layer that answered it, in the service's words", () => {
  assert.equal(layerOf("you"), "you");
  assert.equal(layerOf("built_in"), "built in");
  assert.equal(layerOf("repo_entry"), "this repo");
  assert.equal(layerOf(null), UNSET_LAYER);
  assert.deepEqual(Object.keys(LAYER), [
    "task",
    "repo_entry",
    "you",
    "legacy",
    "repo_file",
    "built_in",
  ]);
  assert.equal(pick("worker", "agent").layer, "you");
  assert.equal(pick("worker", "effort").layer, UNSET_LAYER, "nothing set the effort");
  assert.equal(pick("backup", "model").value, null);
});

test("a program is shown by its name, and written by its id", () => {
  const agent = pick("worker", "agent");
  assert.equal(agent.key, "roles.worker.agent");
  assert.equal(agent.value, "pi");
  assert.equal(agent.display, "Pi");
  assert.equal(agent.note, null);
  const backup = pick("backup", "agent");
  assert.equal(backup.value, "codex");
  assert.equal(backup.display, "Codex");
  // An unset role reads as one that has not chosen yet, not as a broken one.
  const empty = roles({ values: SETTINGS.values.filter((row) => row.key !== "roles.worker.agent") }, ROSTER);
  const worker = empty.find((entry) => entry.id === "worker");
  const unset = worker.picks.find((entry) => entry.field === "agent");
  assert.equal(unset.value, null);
  assert.equal(unset.display, "Not chosen yet");
  assert.match(unset.note, /Pick the program/);
});

test("the program picker offers every program the harness table defines", () => {
  assert.deepEqual(
    programChoices(ROSTER).map((choice) => choice.value),
    ["claude", "codex", "kilo", "opencode", "pi"],
  );
  assert.deepEqual(programChoices(ROSTER)[0], {
    value: "claude",
    label: "Claude Code",
    hint: "anthropic",
  });
  assert.equal(programOf(ROSTER, "pi").name, "Pi");
  assert.equal(programOf(ROSTER, "nope"), null);
});

test("a model picker offers the catalog's models after the program's default", () => {
  const claude = programOf(ROSTER, "claude");
  const choices = modelChoices(claude);
  assert.equal(choices.length, 4);
  assert.deepEqual(choices[0], { value: null, label: "The program's default", hint: null });
  assert.deepEqual(
    choices.slice(1).map((choice) => choice.value),
    ["fable", "opus", "sonnet"],
  );
  assert.equal(choices[2].label, "Opus");

  const model = pick("coordinator", "model");
  assert.equal(model.value, "opus");
  assert.equal(model.display, "opus");
  assert.equal(model.source, "you");
  assert.equal(model.choices.length, 4);
});

test("a program whose catalog named no model still offers the default, and says why", () => {
  // pi lists its models itself; this service was started without its provider
  // configured, so the catalog came back with nothing.
  const pi = programOf(ROSTER, "pi");
  assert.deepEqual(pi.models, []);
  assert.match(pi.models_note, /type a model id/);
  const model = pick("worker", "model");
  assert.equal(model.value, "commandcode/z-ai/glm-5.3-flash");
  assert.equal(model.choices.length, 1, "the default is still a choice");
  assert.match(model.note, /type a model id/);
  assert.deepEqual(modelChoices(null), [], "no program, no choices");
});

test("an effort picker offers the model's own levels, and nothing where it takes none", () => {
  const claude = programOf(ROSTER, "claude");
  assert.deepEqual(effortChoices(claude, "opus").map((choice) => choice.value), [
    null,
    "low",
    "medium",
    "high",
    "xhigh",
    "max",
  ]);
  assert.deepEqual(effortLevels(claude, "opus"), ["low", "medium", "high", "xhigh", "max"]);
  // A model the catalog does not name falls back to the program's own levels.
  assert.deepEqual(effortLevels(claude, "typed-by-hand"), ["low", "medium", "high", "xhigh", "max"]);
  // kilo takes no effort at all, so there is nothing to pick, and the row says so.
  assert.deepEqual(effortChoices(programOf(ROSTER, "kilo"), null), []);
  const kilo = roles(
    {
      values: SETTINGS.values.map((row) =>
        row.key === "roles.worker.agent" ? { ...row, value: "kilo" } : row,
      ),
    },
    ROSTER,
  ).find((entry) => entry.id === "worker");
  const effort = kilo.picks.find((entry) => entry.field === "effort");
  assert.deepEqual(effort.choices, []);
  assert.match(effort.note, /takes no effort level/);
  // A role with no program picked gets the picker's own hint.
  const noProgram = roles(
    { values: SETTINGS.values.filter((row) => row.key !== "roles.worker.agent") },
    ROSTER,
  ).find((entry) => entry.id === "worker");
  assert.match(
    noProgram.picks.find((entry) => entry.field === "effort").note,
    /Pick a program first/,
  );
});

test("a model that wants its provider says so", () => {
  const empty = roles({ values: SETTINGS.values.filter((row) => row.key !== "roles.worker.model") }, ROSTER);
  const worker = empty.find((entry) => entry.id === "worker");
  const model = worker.picks.find((entry) => entry.field === "model");
  assert.match(model.note, /provider\/model/);
});

// -- writing ---------------------------------------------------------------

test("a write names one key, and only sends a value when there is one", () => {
  assert.deepEqual(putBody("roles.worker.agent", "pi"), {
    key: "roles.worker.agent",
    value: "pi",
  });
  assert.deepEqual(putBody("roles.worker.model", null, true), {
    key: "roles.worker.model",
    for_repo: true,
  });
  assert.deepEqual(putBody("roles.order", ["worker", "reviewer"]), {
    key: "roles.order",
    value: ["worker", "reviewer"],
  });
});

test("a role is added and removed through the order alone", () => {
  assert.deepEqual(withRole(SETTINGS, "tester"), [
    "coordinator",
    "worker",
    "reviewer",
    "backup",
    "docs",
    "tester",
  ]);
  assert.equal(withRole(SETTINGS, "worker"), null, "an id already there is not added twice");
  assert.equal(withRole(SETTINGS, "order"), null, "`order` is the list, not a role");
  assert.equal(withRole(SETTINGS, "a.b"), null);
  assert.equal(withRole(SETTINGS, "  "), null);
  assert.deepEqual(withoutRole(SETTINGS, "docs"), [
    "coordinator",
    "worker",
    "reviewer",
    "backup",
  ]);
});

test("a refusal is the service's sentence, kept word for word", () => {
  // The exact text the service refuses a same-vendor review with.
  const service =
    "`roles.reviewer.agent` would be refused: worker role is anthropic (pi/claude-opus-5-5), " +
    "reviewer role is anthropic (claude/opus): they are both anthropic, so the review would not " +
    "be independent";
  const error = new ServiceError(service, { status: 409, code: "refused" });
  const shown = refused("roles.reviewer.agent", error);
  assert.equal(shown.key, "roles.reviewer.agent");
  assert.equal(shown.text, service, "the words are passed through, not summarised");
  assert.equal(shown.text, reason(error));
  // A transport failure says what it can, and never nothing.
  assert.match(refused("roles.worker.agent", new Error("the service did not answer")).text, /did not answer/);
});

test("a write says what it did", () => {
  assert.equal(outcomeOf({ outcome: "stored" }).text, "Saved");
  assert.equal(
    outcomeOf({ outcome: "equal_to_inherited" }).text,
    "That is what it would inherit, so nothing was stored",
  );
  assert.equal(outcomeOf({ outcome: "removed" }).text, "Removed: it inherits again");
  assert.equal(outcomeOf(null).text, "Saved");
});

test("the settings' own problems and skipped checks are shown, not hidden", () => {
  assert.deepEqual(health(SETTINGS), { problems: [], warnings: [], notChecked: null });
  const broken = {
    ...SETTINGS,
    problems: ["worker role is anthropic, reviewer role is anthropic: they are both anthropic"],
    not_checked: "no harness data directory: set SHPRD_DATA_DIR",
  };
  assert.deepEqual(health(broken).problems, broken.problems);
  assert.match(health(broken).notChecked, /SHPRD_DATA_DIR/);
});

// -- templates -------------------------------------------------------------

test("the template list says which prompts are the person's own", () => {
  const rows = promptRows(PROMPTS);
  assert.deepEqual(
    rows.map((row) => row.id),
    ["assistant", "worker", "reviewer", "backup"],
  );
  for (const row of rows) {
    assert.equal(row.word, "Shipped");
    assert.equal(row.edited, false);
    assert.equal(row.shipped, true);
    assert.equal(row.problem, null);
  }
  assert.equal(rows[1].about, "Does the work of one task on its own branch.");
  assert.match(rows[1].path, /prompts\/worker\.md$/);
});

test("an edited or unreadable template is marked as such", () => {
  const edited = promptRows({
    prompts: [
      { id: "worker", about: "the worker", path: "/p/worker.md", state: "edited", shipped: true },
      { id: "mine", path: "/p/mine.md", shipped: false },
      { id: "bad", path: "/p/bad.md", shipped: false, problem: "cannot read /p/bad.md" },
    ],
  });
  assert.equal(edited[0].word, "Edited");
  assert.equal(edited[0].edited, true);
  assert.equal(edited[1].word, "Shipped");
  assert.equal(edited[1].about, "a prompt of your own");
  assert.equal(edited[2].word, "Unreadable");
  assert.equal(edited[2].state, null);
  assert.match(edited[2].problem, /cannot read/);
});

test("the editor says what saving and resetting do", () => {
  assert.match(promptLine({ state: "edited" }), /never replaced by an upgrade/);
  assert.match(promptLine({ state: "default" }), /saving your own stores them/);
  // What a save just answered wins over what was read before it.
  assert.match(promptLine({ state: "edited" }, { state: "default" }), /shipped words/);
  assert.equal(promptLine(null), "");
});

test("a role key is read back as its role and field", () => {
  assert.deepEqual(roleOf("roles.worker.agent"), { id: "worker", field: "agent" });
  assert.equal(roleOf("roles.order"), null);
  assert.equal(roleOf("tasks.merge"), null);
  assert.equal(roleOf(undefined), null);
});
