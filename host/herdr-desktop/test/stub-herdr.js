// The `herdr` module, stubbed for a unit test: the two functions `lib/` imports
// (`home`, `readTextFile`), answering from a table a test fills.
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
