// The graph block: what it says, and that the layout still gives it somewhere to
// say it.
//
// The layout file is read, not invented: `bin/check` proves the mods load, and
// this proves the two halves of the block — the note and the terminal — are still
// the nodes the mod selects, with the terminal name the mod starts.

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import {
  GRAPH_NOTE,
  GRAPH_STACK,
  GRAPH_TERM,
  GRAPH_TERMINAL,
  SETTING,
  finishedTerminals,
  graphTerminal,
  graphView,
  hasNode,
  loopArgv,
  originWord,
  startArgv,
} from "../mods/shprd-graph/view.js";

/** The installed layout, as the app reads it. */
const layout = JSON.parse(
  readFileSync(new URL("../layouts/shprd.json", import.meta.url), "utf8"),
);

/** The `/api/graph` answer a service with dagr sends. */
const ready = {
  repo: "/tmp/repo",
  run_json: "/tmp/repo/.dagr/run.json",
  run: "r1",
  dagr: { path: "/opt/dagr/bin/dagr", origin: "plugin" },
  argv: ["/opt/dagr/bin/dagr", "view", "/tmp/repo/.dagr/run.json"],
  setting: SETTING,
};

/** The answer a machine with no dagr sends. */
const missing = {
  repo: "/tmp/repo",
  run_json: "/tmp/repo/.dagr/run.json",
  run: "r1",
  argv: [],
  setting: SETTING,
  message:
    "dagr was not found: set `graph.dagr` to its path, put `dagr` on PATH, or install the herdr-dagr plugin",
};

test("the layout keeps the block the mod selects", () => {
  assert.ok(hasNode(layout, GRAPH_STACK), "the graph stack is in the layout");
  assert.ok(hasNode(layout, GRAPH_NOTE), "the note half is in the layout");
  assert.ok(hasNode(layout, GRAPH_TERM), "the terminal half is in the layout");

  const find = (node, id) =>
    node && typeof node === "object"
      ? node.id === id
        ? node
        : (node.children ?? []).map((child) => find(child, id)).find(Boolean)
      : undefined;
  const stack = find(layout, GRAPH_STACK);
  assert.equal(stack.type, "stack", "the block is a stack: it shows one of two");
  assert.equal(
    stack.active,
    GRAPH_NOTE,
    "the note is what a window shows first, so it can start the terminal",
  );
  assert.deepEqual(
    stack.children.map((child) => child.id),
    [GRAPH_NOTE, GRAPH_TERM],
  );
  const term = find(layout, GRAPH_TERM);
  assert.deepEqual(
    term.content,
    { terminal: GRAPH_TERMINAL },
    "the terminal block names the terminal the mod starts",
  );
});

test("a block Herdr pruned says how to put it back", () => {
  const seen = graphView({ graph: ready, service: { port: 1 }, blockGone: true });
  assert.equal(seen.kind, "stop");
  assert.match(seen.title, /gone from the layout/);
  assert.match(seen.hint, /shprd install herdr-desktop/);
  // Herdr shortens the layout file itself, so the installer refuses it until the
  // app's copy is moved aside; the message says that rather than sending a person
  // to a command that would only refuse.
  assert.match(seen.message, /layouts\/shprd\.json/);
});

test("without dagr the block names the setting that fixes it", () => {
  const seen = graphView({ graph: missing, service: { port: 1 } });
  assert.equal(seen.kind, "stop");
  assert.equal(seen.setting, SETTING);
  assert.equal(seen.title, "dagr is not installed");
  assert.match(seen.message, /graph\.dagr/, "the message names the key");
  assert.match(seen.message, /herdr-dagr/, "and the plugin it comes in");
  assert.equal(seen.hint, SETTING, "the foot repeats the key a person types");
});

test("the block waits rather than pretending, before the service answers", () => {
  const down = graphView({ failure: "connection refused" });
  assert.equal(down.kind, "wait");
  assert.equal(down.title, "shprd serve is not running");
  assert.match(down.message, /connection refused/);

  const looking = graphView({ service: { port: 1 } });
  assert.equal(looking.kind, "wait");

  const noRun = graphView({ graph: { ...ready, run: null }, service: { port: 1 } });
  assert.equal(noRun.kind, "wait");
  assert.equal(noRun.title, "No run yet");
  assert.equal(noRun.hint, "shprd run start");
});

test("with dagr the block starts it and shows the command it runs", () => {
  const seen = graphView({ graph: ready, service: { port: 1 } });
  assert.equal(seen.kind, "ready");
  assert.equal(seen.message, "/opt/dagr/bin/dagr view /tmp/repo/.dagr/run.json");
  assert.equal(seen.hint, "dagr from herdr plugin");
  assert.equal(seen.origin, "herdr plugin");
});

test("a terminal is reused only when it draws this run", () => {
  const ours = {
    name: GRAPH_TERMINAL,
    address: "t7",
    state: "live",
    argv: loopArgv("/opt/dagr/bin/dagr", ready.run_json),
  };
  const found = graphTerminal([ours], ready.run_json);
  assert.equal(found.live, ours, "the block's own terminal is reused");
  assert.equal(found.other, null);

  const theirs = {
    name: GRAPH_TERMINAL,
    address: "t8",
    state: "live",
    argv: loopArgv("/opt/dagr/bin/dagr", "/elsewhere/.dagr/run.json"),
  };
  const clash = graphTerminal([theirs], ready.run_json);
  assert.equal(clash.live, null, "another repository's terminal is not reused");
  assert.match(clash.other, /elsewhere/, "and it is named in the message");
  const seen = graphView({ graph: ready, service: { port: 1 }, other: clash.other });
  assert.equal(seen.kind, "stop");
  assert.match(seen.title, /another repository/);
  assert.match(seen.message, /elsewhere/);

  const dead = { ...ours, state: "finished" };
  assert.equal(graphTerminal([dead], ready.run_json).live, null, "a ended terminal is no terminal");
  assert.equal(graphTerminal([], ready.run_json).live, null);
  assert.equal(graphTerminal(undefined, ready.run_json).live, null);
});

test("a dead terminal with the blocks name is forgotten before the block shows", () => {
  // Herdr resolves the block's terminal by name against a list that keeps
  // finished terminals, and takes the first match: a killed `shprd-graph` would
  // shadow the live one. `hc rm` is what forgets it.
  const dead = { name: GRAPH_TERMINAL, address: "t3", state: "finished" };
  const live = { name: GRAPH_TERMINAL, address: "t9", state: "live" };
  const other = { name: "shprd-serve", address: "t1", state: "finished" };
  assert.deepEqual(finishedTerminals([dead, live, other]), ["t3"]);
  assert.deepEqual(finishedTerminals([live, other]), []);
  assert.deepEqual(finishedTerminals(undefined), []);
  assert.deepEqual(finishedTerminals([{ name: GRAPH_TERMINAL, state: "finished" }]), []);
});

test("the terminal runs dagrs own follow mode in one long-lived shell", () => {
  const argv = startArgv(ready);
  assert.deepEqual(argv.slice(0, 7), [
    "run",
    "--bg",
    "--no-parent",
    "--name",
    GRAPH_TERMINAL,
    "--",
    "sh",
  ]);
  assert.equal(argv[7], "-c", "a shell, so a dagr that ends is restarted in place");
  assert.match(argv[8], /view/, "the script is dagr view");
  assert.equal(argv[9], "/opt/dagr/bin/dagr");
  assert.equal(argv[10], ready.run_json);
  assert.equal(startArgv(missing), null, "nothing to run without a program");
});

test("each probe is named in the words a person knows", () => {
  assert.equal(originWord({ dagr: { origin: "env" } }), "$SHPRD_DAGR_BIN");
  assert.equal(originWord({ dagr: { origin: "setting" } }), "graph.dagr");
  assert.equal(originWord({ dagr: { origin: "path" } }), "PATH");
  assert.equal(originWord({ dagr: { origin: "plugin" } }), "herdr plugin");
  assert.equal(originWord(null), "no dagr");
});
