// The `herdr` module, stubbed for a unit test: the few functions `lib/` imports
// (`home`, `readTextFile`, `watchFolder`, `exec`), answering from a table a test
// fills.
//
// A Herdr mod imports `herdr` and nothing else of the app's; the unit test runs
// under node (or bun), so `test/hooks.mjs` points that one specifier here and
// every other import stays the real file.

/** The home `home()` answers; a test sets it. */
export let HOME = "/tmp/shprd-test-home";

/** The files `readTextFile` answers with: path to text, or a function of it. */
export const FILES = new Map();

/** Which paths were read, in order, so a test can prove a look happened. */
export const READS = [];

/** What `readTextFile` does for a path with nothing in [`FILES`]. */
export let MISSING = null;

export function home() {
  return HOME;
}

export async function readTextFile(path) {
  READS.push(path);
  const entry = FILES.get(path);
  if (typeof entry === "function") return entry(path);
  return entry ?? MISSING;
}

/** Sets the home the next `home()` call answers. */
export function setHome(dir) {
  HOME = dir;
}

/** Sets one file's content (or a function of the path). */
export function putFile(path, text) {
  FILES.set(path, text);
}

/** Forgets every file and every read. */
export function reset() {
  HOME = "/tmp/shprd-test-home";
  FILES.clear();
  READS.length = 0;
  MISSING = null;
  WATCH = async () => [];
  RUN = async () => ({ code: 0, stdout: "", stderr: "" });
  HC = async () => ({ code: 0, stdout: "", stderr: "" });
  HC_CALLS.length = 0;
  SPACES = [];
  RUNS.length = 0;
  SHARED.clear();
  PROPS = {};
}

/** What `exec` answers; a test replaces it. */
export let RUN = async () => ({ code: 0, stdout: "", stderr: "" });

/** Sets what `exec` answers next. */
export function setExec(next) {
  RUN = next;
}

export async function exec(command, args, options) {
  return RUN(command, args, options);
}

/** The folder watcher, stubbed: a test supplies what it answers. */
export let WATCH = async () => [];

/** Sets what `watchFolder` answers next; its argument is ignored. */
export function setWatch(next) {
  WATCH = next;
}

export async function watchFolder(dir) {
  return WATCH(dir);
}

/** What `hc` answers; a test replaces it and can read the calls it got. */
export let HC = async () => ({ code: 0, stdout: "", stderr: "" });

/** Every `hc` call, in order. */
export const HC_CALLS = [];

/** Sets what `hc` answers next. */
export function setHc(next) {
  HC = next;
}

export async function hc(args) {
  HC_CALLS.push(args);
  return HC(args);
}

/** What `props()` answers; a test sets it. */
export let PROPS = {};

/** Sets what `props()` answers next: the layout's own props for a copy. */
export function setProps(next) {
  PROPS = next ?? {};
}

export function props() {
  return PROPS;
}

/** Herdr's live space list; a test sets it. */
export let SPACES = [];

/** Sets what `spaces()` answers. */
export function setSpaces(next) {
  SPACES = next;
}

export function spaces() {
  return SPACES;
}

/** Runs an app command; a test can read what was asked for. */
export const RUNS = [];

export function run(name, args) {
  RUNS.push({ name, args });
  return null;
}

/** The shared key/value space, as `shared(k)` and `setShared(k, v)` see it. */
export const SHARED = new Map();

/** What `shared(key)` answers: the value last set, or null. */
export function shared(key) {
  return SHARED.has(key) ? SHARED.get(key) : null;
}

/** Sets one shared key, as a click in another mod would. */
export function setShared(key, value) {
  SHARED.set(key, value);
  return value;
}

/**
 * `nextCommand()`: the app's command queue for a focusable mod.
 *
 * A mod awaits it in a loop, so a test that never presses a command must leave
 * it waiting rather than resolving: a resolved `null` would spin the loop.
 */
export function nextCommand() {
  return new Promise(() => {});
}

/** The theme colours the footer draws with: one per name it uses. */
export function theme() {
  return {
    foreground: "#eeeeee",
    muted: "#999999",
    dim: "#777777",
    border: "#333333",
    raised: "#222222",
    hover: "#2a2a2a",
    done: "#3dd68c",
    working: "#f5b544",
    blocked: "#ff6369",
    accent: "#5b8def",
  };
}
