// The graph block's reading of the service's answer, as plain data: no Herdr, no
// `fetch`, no clock. Everything a person sees in the block is decided here, so a
// unit test can prove the missing-dagr message and the words around it without
// an app.
//
// The block is two layout nodes in one stack: `graph-note` (this mod) and
// `graph-term` (a terminal). The note is what shows first, so it is what says
// where dagr is, what it will run, and — when there is no dagr — which setting
// fixes that. Once dagr is running the note selects the terminal, which shows
// `dagr view <run.json>` in dagr's own follow mode.
//
// Ids and the terminal's name are here rather than in the two files that use
// them, so the layout, the mod and the test cannot drift apart.

/** The stack the layout keeps the block in: the note, and the terminal it adds. */
export const GRAPH_STACK = "graph";

/** The note: this mod, drawn while there is nothing to show yet. */
export const GRAPH_NOTE = "graph-note";

/**
 * The terminal block, whose content is `{ "terminal": GRAPH_TERMINAL }`.
 *
 * It is in `layouts/shprd.json` and the note selects it: a Herdr layout terminal
 * block resolves its terminal once, by name, and keeps it, so the terminal has to
 * be up before the block first renders — which is what the note is for.
 */
export const GRAPH_TERM = "graph-term";

/**
 * The herdr-core terminal the block is bound to.
 *
 * A layout terminal block resolves its terminal **once**, by address or by name,
 * and keeps it: the app removes the block from the layout when that terminal
 * ends. So there is exactly one name here, the terminal is started before the
 * block first renders, and it is never killed and replaced — a programme that
 * exits is restarted *inside* it (see [`loopArgv`]).
 */
export const GRAPH_TERMINAL = "shprd-graph";

/** The settings key the service names, and the one this block's message names. */
export const SETTING = "graph.dagr";

/** How many words of another repository's path a message shows. */
const PATH_W = 48;

/**
 * The shell command the block's terminal runs: dagr's own follow mode, in a
 * loop.
 *
 * `dagr view <run.json>` watches the document and redraws when the service
 * rewrites it, so no per-write relaunch is needed. The loop is for the terminal
 * rather than for the file: `q` or a crash ends dagr, and the layout's block is
 * bound to this terminal's address, so restarting the program in place is what
 * keeps the graph on screen. One second between attempts, so a dagr that cannot
 * start at all does not spin.
 */
export function loopArgv(dagr, runJson) {
  return [
    "sh",
    "-c",
    'while :; do "$0" view "$1"; sleep 1; done',
    String(dagr ?? ""),
    String(runJson ?? ""),
  ];
}

/** The arguments `hc run` needs to start the block's terminal, or null. */
export function startArgv(graph) {
  const dagr = graph?.dagr?.path;
  const runJson = graph?.run_json;
  if (!dagr || !runJson) return null;
  return [
    "run",
    "--bg",
    "--no-parent",
    "--name",
    GRAPH_TERMINAL,
    "--",
    ...loopArgv(dagr, runJson),
  ];
}

/**
 * The `shprd-graph` terminals that are alive, and whether they are drawing this
 * run.
 *
 * A live terminal with the block's name is reused when it is running this
 * repository's document. One that is running another repository's is reported
 * instead of being killed: the person may be looking at it in another window,
 * and a block that silently swapped what it shows would be worse than a message.
 */
export function graphTerminal(terminals, runJson) {
  const named = (Array.isArray(terminals) ? terminals : []).filter(
    (terminal) => terminal?.name === GRAPH_TERMINAL,
  );
  const live = named.filter((terminal) => terminal.state === "live");
  const ours = live.find((terminal) => names(terminal, runJson));
  if (ours) return { live: ours, other: null };
  const other = live.find((terminal) => !names(terminal, runJson));
  return { live: null, other: other === undefined ? null : otherPath(other, runJson) };
}

/**
 * The addresses of *finished* terminals carrying the block's name.
 *
 * They have to go before the block is shown: Herdr resolves a layout terminal
 * block's name against the core's terminal list, which keeps finished terminals,
 * and it resolves to the first match. A killed `shprd-graph` left in that list
 * would take the name from the live one — measured: the block then shows nothing
 * and the app drops it from the layout. `hc rm` forgets a finished terminal, so
 * the block's name is only ever the live one.
 */
export function finishedTerminals(terminals) {
  return (Array.isArray(terminals) ? terminals : [])
    .filter((terminal) => terminal?.name === GRAPH_TERMINAL && terminal.state !== "live")
    .map((terminal) => terminal.address)
    .filter((address) => typeof address === "string" && address.length > 0);
}

/** Whether a terminal's argv names `runJson`. */
function names(terminal, runJson) {
  if (!runJson) return false;
  const argv = Array.isArray(terminal?.argv) ? terminal.argv : [];
  return argv.includes(runJson) || argv.join(" ").includes(runJson);
}

/** The path a terminal is drawing, for the message about another repository. */
function otherPath(terminal, runJson) {
  const argv = Array.isArray(terminal?.argv) ? terminal.argv : [];
  const found = argv.find((part) => String(part).endsWith(".json")) ?? argv.join(" ");
  const text = String(found).replace(`/${String(runJson).split("/").pop() ?? "run.json"}`, "");
  return text.length > PATH_W ? `…${text.slice(-PATH_W)}` : text;
}

/**
 * Whether the shown layout still has `id` anywhere in it.
 *
 * The app removes a layout terminal block when its terminal ends, so the note
 * reads the layout back rather than assuming the block is there: a stack that
 * lost its terminal child has nothing to select, and the person needs to be told
 * that instead of watching a button do nothing.
 */
export function hasNode(node, id) {
  if (!node || typeof node !== "object") return false;
  if (node.id === id) return true;
  const children = Array.isArray(node.children) ? node.children : [];
  return children.some((child) => hasNode(child, id));
}

/** One line about a probe's answer, for the note's right-hand side. */
export function originWord(graph) {
  switch (graph?.dagr?.origin) {
    case "env":
      return "$SHPRD_DAGR_BIN";
    case "setting":
      return SETTING;
    case "path":
      return "PATH";
    case "plugin":
      return "herdr plugin";
    default:
      return "no dagr";
  }
}

/**
 * What the block shows now, as words and one of three kinds.
 *
 * `kind` is `wait` (something is not there yet: the service, a run, a first
 * answer), `stop` (nothing can start until the person does something) or `ready`
 * (dagr is there and the terminal is about to be shown). `message` always names
 * the thing to fix, and `setting` is always the settings key, so a person who
 * reads the block knows where to type.
 */
export function graphView(facts = {}) {
  const { graph = null, service = null, failure = null, other = null, blockGone = false } = facts;
  const setting = graph?.setting ?? SETTING;
  const origin = originWord(graph);
  const wait = (title, message, hint = setting) => ({
    kind: "wait",
    title,
    message,
    hint,
    setting,
    origin,
  });
  const stop = (title, message, hint = setting) => ({
    kind: "stop",
    title,
    message,
    hint,
    setting,
    origin,
  });

  if (failure) {
    return wait(
      "shprd serve is not running",
      `${failure}. The graph is drawn by the run's own service: the block starts with it.`,
      "shprd serve",
    );
  }
  if (!service) {
    return wait("Looking for the run…", "this window's service has not answered yet");
  }
  if (!graph) {
    return wait("Looking for the run…", "the service has not answered about the graph yet");
  }
  if (blockGone) {
    return stop(
      "The graph block is gone from the layout",
      "Herdr removes a terminal block whose terminal it cannot find, and writes the shortened layout back. Move `layouts/shprd.json` aside and run `shprd install herdr-desktop`: the installer never overwrites a layout Herdr has edited, and the next window starts the block again.",
      "shprd install herdr-desktop",
    );
  }
  if (graph.message) {
    return stop("dagr is not installed", graph.message, setting);
  }
  if (!graph.run) {
    return wait(
      "No run yet",
      "The graph draws a run's tasks, attempts and gates. `shprd run start` begins one.",
      "shprd run start",
    );
  }
  if (other) {
    return stop(
      "The graph terminal is showing another repository",
      `\`${GRAPH_TERMINAL}\` is drawing ${other}. Close that terminal, then reopen the window: the block's terminal is bound once.`,
      GRAPH_TERMINAL,
    );
  }
  return {
    kind: "ready",
    title: "Starting dagr…",
    message: Array.isArray(graph.argv) ? graph.argv.join(" ") : "",
    hint: `dagr from ${origin}`,
    setting,
    origin,
  };
}
