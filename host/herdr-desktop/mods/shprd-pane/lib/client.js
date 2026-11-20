// The mod-to-service client: find `shprd serve`, prove it is the one the
// discovery file names, and then talk to it with the token from the state
// directory.
//
// This is Shepherdr's pattern (`lib/local.js`, `lib/bridge.js`, `lib/service.js`),
// ported because the Herdr mod runtime leaves no other transport open:
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `lib/local.js` (port and token discovery, the `id` handshake),
//   `lib/bridge.js` (its poll-and-reconcile shape), `service/main.ts` (the
//   discovery files a window reads)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// What is shprd's own here: each repository has its own service, and each service
// its own state directory, `$XDG_STATE_HOME/shprd/repos/<name>-<hash>` (else under
// `<home>/.local/state`). A mod finds the service for the space the window shows
// by reading every `repos/*/service.json` and matching the record's `root` with
// that space's folder (`pickService`), so a second repository never needs the first
// one stopped. `GET /api/health` answers
// `{ok, id, version}`, a write needs `x-shprd: 1` beside the bearer token, and
// the service touches `<its state dir>/changes` after every ledger write so a mod
// can watch a file instead of guessing when to read again (`lib/watch.js`).
//
// A mod has no environment of its own (the M7ef spike measured it), so the state
// directory is found by asking this computer's shell once (`probeStateDir`).
// Until that answers, XDG's defaults are used, which is the same place unless
// the person moved `$XDG_STATE_HOME`.
//
// **No timers.** A Herdr mod's `setTimeout` throws; the runtime's only clock is
// `cx.timer`/`cx.sleep`, so every wait here is passed in as `io.wait(ms)`
// (`cx.sleep` in a mod, a real timer in a test) and a missing one means "wait as
// long as the call takes".
//
// Shared: the source is `mods/lib/client.js`; `bin/sync-shared` copies it into
// every mod folder as `lib/client.js`. Edit the source, never a copy.
//
// # A service that restarted, and a panel that must not freeze
//
// `GET /api/state` against a service that has gone away does **not** throw: a
// guarded read runs out of time and answers `null` (the M10 gate's bug 5). A mod
// that treated that `null` as "nothing new" kept painting the boards of a
// service that was gone, for ever, while the new one had moved on. So there are
// two rules here, and every mod follows them:
//
// - **[`attach`]**: the client is keyed on the discovery record's `id`. Before a
//   panel reads, `attach` compares the `service.json` on disk with the id its
//   client was made from; a different id — a restarted service, even one that
//   took the same port — is a new client, and the panel re-reads everything.
// - **A read that must not paint stale is strict** ([`Client.call`]'s `strict`
//   option, which is what `state()` and `asks()` use): a read that runs out of
//   time is a **failure** (a [`noAnswer`]), not an empty answer. The panel's
//   `catch` drops the client, the next look rediscovers, and the panel fills in
//   again.

import { exec, home, readTextFile, shared, spaces } from "herdr";

/** The first look at a service gives up after this many ms. */
export const HEALTH_WAIT = 1500;
/** A read gives up after this many ms, so one slow call never wedges a panel. */
export const READ_WAIT = 4000;
/** A write is one event, so it may take longer than a read. */
export const WRITE_WAIT = 6000;
/**
 * How long the roster may take.
 *
 * It is the one read that can start a program — a program that lists its own
 * models answers only when its CLI does — so it is bounded by four programs'
 * listings, not by a file read. The service bounds each listing itself; this is
 * the outer bound a page waits under.
 */
export const ROSTER_WAIT = 20_000;
/** How long the long poll waits at the service before it answers `204`. */
export const POLL_WAIT = 20;

/** The error a failed call raises, carrying what the service said. */
export class ServiceError extends Error {
  /**
   * @param {string} message what went wrong, in words a person can read
   * @param {{status?: number, code?: string, detail?: unknown}} [info] the answer
   */
  constructor(message, info = {}) {
    super(message);
    this.name = "ServiceError";
    this.status = info.status ?? 0;
    this.code = info.code ?? "failed";
    this.detail = info.detail ?? null;
  }
}

/**
 * The error a failed read raises when the reason is that nothing answered.
 *
 * It is distinct from a [`ServiceError`] carrying a status: a refusal (a 422, a
 * 401) is the service talking, and a panel keeps its client; no answer at all is
 * how a service that restarted or died looks, and the client must go.
 */
export function noAnswer(message = "the service did not answer") {
  return new ServiceError(message, { status: 0, code: "no-answer" });
}

/**
 * `work`, or [`noAnswer`] when it answers nothing at all.
 *
 * A read a panel must not paint stale goes through this: `null` from a guarded
 * call means the wait ran out, which is exactly what a dead service looks like.
 */
export async function answered(work, message) {
  const answer = await work;
  if (answer === null || answer === undefined) throw noAnswer(message);
  return answer;
}

/**
 * The state directory a service's discovery files live in, from a home and the
 * caller's XDG answers.
 *
 * `xdg` is what the shell said: `{state}` or `{}` when it said nothing usable.
 * An empty `$XDG_STATE_HOME` is unset, which is what the XDG specification says.
 */
export function stateDir(homeDir, xdg = {}) {
  const base = xdg?.state || `${homeDir}/.local/state`;
  return `${base}/shprd`;
}

/** Where `service.json` is, under a state directory. */
export function serviceFile(dir) {
  return `${dir}/service.json`;
}

/** Where the bearer token is, under a state directory. */
export function tokenFile(dir) {
  return `${dir}/token`;
}

/**
 * Where the service touches a file after every ledger write, under a state
 * directory. A mod watches the *folder* and reads this file, because a Herdr
 * mod only gets "something under this folder changed".
 */
export function changeFile(dir) {
  return `${dir}/changes`;
}

/** The shell snippet that answers this computer's shprd paths, as one JSON line. */
export const PROBE = 'printf \'{"state":"%s"}\\n\' "${XDG_STATE_HOME:-}"';

/**
 * Asks this computer's shell where its state directory is, or `{}` when the
 * shell cannot say.
 *
 * `run` is `exec` from `herdr`; the last line of its stdout is the answer, so a
 * shell profile that prints a greeting does not break the parse.
 */
export async function probeStateDir(run = exec) {
  const result = await run("sh", ["-c", PROBE]).catch(() => null);
  const text = String(result?.stdout ?? "").trim();
  const line = text === "" ? "" : text.split("\n").pop();
  try {
    const answer = JSON.parse(line || "null");
    return answer && typeof answer === "object" ? answer : {};
  } catch {
    return {};
  }
}

/** How long the shell's answer is kept before it is asked again, ms. */
export const PROBE_MS = 30_000;

/** The last answer the shell gave, and when it gave it. */
let probed = null;

/**
 * This computer's XDG answers, as the shell sees them.
 *
 * A mod has no environment: it inherits nothing of the person's shell, so
 * `$XDG_STATE_HOME` is invisible to it even though `shprd serve` obeys it (the
 * M7ef spike measured that a mod may `exec` but reads no environment of its
 * own). Shepherdr's `lib/dirs.js` solves it the same way — ask `sh -c` once and
 * keep the answer for a while — and so does this: the shell's last line, cached
 * for [`PROBE_MS`], and a failed probe is no answer at all.
 */
export async function stateHome(run = exec) {
  const now = Date.now();
  if (probed !== null && now - probed.at < PROBE_MS) return probed.xdg;
  const xdg = await probeStateDir(run);
  probed = { xdg, at: now };
  return xdg;
}

/** Forgets the shell's answer, so the next call asks again. */
export function forgetStateHome() {
  probed = null;
}

/**
 * Checks the text of a `service.json`: `{port, id, pid, version, root}` or null.
 *
 * A file that names no usable port is not a service: a stale or half-written
 * record must leave a mod looking rather than dialling whatever holds a port.
 */
export function parseServiceRecord(text) {
  if (typeof text !== "string" || text.trim() === "") return null;
  let record;
  try {
    record = JSON.parse(text);
  } catch {
    return null;
  }
  if (!record || typeof record !== "object") return null;
  const port = record.port;
  if (!Number.isInteger(port) || port <= 0 || port > 65535) return null;
  if (typeof record.id !== "string" || record.id === "") return null;
  return {
    port,
    id: record.id,
    pid: Number.isInteger(record.pid) ? record.pid : 0,
    version: typeof record.version === "string" ? record.version : "",
    root: typeof record.root === "string" ? record.root : "",
  };
}

/**
 * Reads and checks `service.json`: `{port, id, pid, version, root}` or null.
 *
 * `read` is injected so a test can drive it without Herdr (`readTextFile`
 * otherwise).
 */
export async function readServiceRecord(path, read = readTextFile) {
  const text = await read(path).catch(() => null);
  return parseServiceRecord(text);
}

/** The shell loop that prints every service's directory and record, a line each. */
export const LIST_SCRIPT =
  'for f in "$1"/repos/*/service.json; do [ -f "$f" ] || continue; ' +
  'printf "%s\\t" "${f%/service.json}"; tr -d "\\n" < "$f"; printf "\\n"; done';

/**
 * Every service this computer has written a record for: `[{dir, record}]`.
 *
 * It is one `exec` of a shell loop, because a mod has no directory listing. A
 * panel asks only when it has no service or the window moved to another space
 * (a service it already talks to is checked by reading its own record), so it is
 * not on the hot path. A record does not mean the service is up: [`discover`]
 * checks it answers.
 */
export async function listServices(base, run = exec) {
  const result = await run("sh", ["-c", LIST_SCRIPT, "sh", base]).catch(() => null);
  const entries = [];
  for (const line of String(result?.stdout ?? "").split("\n")) {
    const tab = line.indexOf("\t");
    if (tab < 1) continue;
    const record = parseServiceRecord(line.slice(tab + 1));
    if (record) entries.push({ dir: line.slice(0, tab), record });
  }
  return entries;
}

/** Kept for callers that used to drop a cached list: there is none now. */
export function forgetServices() {}

/**
 * A folder as it is compared: no trailing slash, and macOS's `/private` prefix of
 * `/var`, `/tmp` and `/etc` taken off (a service reports the real path, a space
 * often the symlinked one).
 */
export function normalizeFolder(folder) {
  let text = String(folder ?? "").trim();
  while (text.length > 1 && text.endsWith("/")) text = text.slice(0, -1);
  return text.replace(/^\/private(?=\/(var|tmp|etc)(\/|$))/, "");
}

/** The last part of a folder: the repository's name, as a person calls it. */
export function repoName(folder) {
  const text = normalizeFolder(folder);
  return text.split("/").filter(Boolean).pop() ?? "";
}

/**
 * The service for `folder` among `entries`: the one whose `root` is that folder
 * or holds it (the deepest wins), else null.
 *
 * A window with no shown folder uses the one service there is, so a person who
 * runs one repository sees what they always did; with several it is ambiguous and
 * answers null rather than a guess.
 */
export function pickService(entries, folder) {
  const want = folder ? normalizeFolder(folder) : "";
  if (want === "") return entries.length === 1 ? entries[0] : null;
  let best = null;
  for (const entry of entries) {
    const root = normalizeFolder(entry.record.root);
    if (root === "") continue;
    if (want === root || want.startsWith(`${root}/`)) {
      if (!best || root.length > normalizeFolder(best.record.root).length) best = entry;
    }
  }
  return best;
}

const roots = new Map();
/** How long a folder that resolved to no repository is left alone, ms. */
const ROOT_RETRY_MS = 10_000;

/**
 * The repository a folder belongs to, as git writes it (every symlink resolved,
 * a subfolder or a linked worktree answered by its repository): the path a
 * service records as its `root`. Null when it is no repository.
 *
 * The footer's names for a folder (`~/dev` through a symlink) and the service's
 * canonical root differ, and a string match alone would miss the service that is
 * running. The answer is kept: a repository does not move.
 */
export async function canonicalRoot(folder, run = exec) {
  const kept = roots.get(folder);
  if (kept && (kept.root || Date.now() - kept.at < ROOT_RETRY_MS)) return kept.root;
  const answer = await run("git", ["-C", folder, "rev-parse", "--path-format=absolute", "--git-common-dir"]).catch(() => null);
  const line = String(answer?.stdout ?? "").trim().split("\n").pop() ?? "";
  const root = answer?.code === 0 && line.endsWith("/.git") ? line.slice(0, -"/.git".length) : null;
  roots.set(folder, { root, at: Date.now() });
  return root;
}

/** The folder of the space the window shows, or null. */
export function activeFolder() {
  try {
    const space = (spaces() ?? []).find((entry) => entry.active);
    return space?.folder ? String(space.folder) : null;
  } catch {
    return null;
  }
}

/**
 * The name of the terminal one repository's service runs in: `hc` takes 1-32 of
 * `a-z 0-9 - _`, and two folders of one name must not share a terminal.
 */
export function terminalName(folder) {
  const path = normalizeFolder(folder);
  let hash = 0x811c9dc5;
  for (let i = 0; i < path.length; i += 1) {
    hash ^= path.charCodeAt(i);
    hash = Math.imul(hash, 0x01000193) >>> 0;
  }
  const name = (repoName(path).toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "") || "repo").slice(0, 14);
  return `sv-${name}-${hash.toString(16).padStart(8, "0")}`;
}

/** The sentence a dead service's last words give: the last line, without colour codes. */
export function lastReason(text) {
  const lines = String(text ?? "")
    .replace(/\u001b\[[0-9;?]*[A-Za-z]/g, "")
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean);
  const hit = [...lines].reverse().find((line) => /error|refus|cannot|could not|failed|not a /i.test(line));
  return (hit ?? lines.pop() ?? "").replace(/^error:\s*/i, "").slice(0, 160);
}

/** The shared key the footer publishes the shown repository's service state under. */
export const SERVICE_STATUS = "service.status";

/**
 * The shared key the chat publishes under when a message could not be sent
 * (`{folder, text}`, or `null` once one is), so the sidebar's foot says why in the
 * same plain words the chat does.
 */
export const SEND_PROBLEM = "chat.problem";

/**
 * Words for a service that is not answering, for the folder the window shows.
 *
 * The footer owns starting the service and publishes how it is going; every
 * other panel says that in one plain sentence instead of telling the person to run
 * a command. `folder` defaults to the shown space's.
 */
export function downNotice(folder = activeFolder()) {
  if (!folder) return "Open a repository's space to use shprd";
  const name = repoName(folder) || "this repository";
  const status = shared(SERVICE_STATUS);
  if (status && normalizeFolder(status.folder) === normalizeFolder(folder)) {
    if (status.phase === "starting") return `Starting the service for ${name}…`;
    if (status.phase === "failed") {
      return `The service for ${name} could not start: ${status.reason || "no reason given"}`;
    }
  }
  return `The service for ${name} is not up yet`;
}

/**
 * Reads the bearer token, or null when there is none.
 *
 * It is trimmed: the service writes it with a trailing newline, and a header
 * with a newline in it is not the token.
 */
export async function readToken(path, read = readTextFile) {
  const text = await read(path).catch(() => null);
  if (typeof text !== "string") return null;
  const token = text.trim();
  return token === "" ? null : token;
}

/** The address a record names, as a base URL with no trailing slash. */
export function baseUrl(record) {
  return `http://127.0.0.1:${record.port}`;
}

/**
 * One JSON GET, or null on a transport failure, a non-JSON answer, or a wait
 * that ran out.
 */
async function getJson(doFetch, url, wait) {
  const call = (async () => {
    const answer = await doFetch(url);
    if (!answer || !answer.ok) return null;
    return await answer.json();
  })().catch(() => null);
  return wait ? guard(call, wait, HEALTH_WAIT) : call;
}

/**
 * `work`, or null once `wait(ms)` resolves first.
 *
 * Inside a mod `wait` is `cx.sleep`, which is the only clock a mod has
 * (`setTimeout` throws there); a test passes a real `setTimeout`. A `work` that
 * rejects later must not become an unhandled rejection, so its failure is
 * swallowed here: the call site says what to show.
 */
function guard(work, wait, ms) {
  const deadline = after(wait, ms);
  const settled = work.then(
    (value) => value,
    () => null,
  );
  return Promise.race([settled, deadline.promise.then(() => null)])
    .catch(() => null)
    .finally(() => deadline.cancel());
}

/** How long the shared clock sleeps at most, so a nearer deadline is met late by this much. */
export const TICK_MS = 500;

const deadlines = new Set();
let ticking = false;
let latestWait = null;
let skew = 0;

/**
 * One deadline on the shared clock: `{promise, cancel}`.
 *
 * A mod's only clock is `cx.sleep`, and the runtime allows 1024 outstanding host
 * tasks per mod. A `cx.sleep(ms)` per guarded call left every sleep outstanding
 * until it ran out, long after its call had answered, and a busy panel reached
 * the limit in minutes ("cx.sleep(ms) exceeded the per-runtime outstanding host
 * task limit"). So all deadlines in this copy share **one** sleep at a time, of
 * at most [`TICK_MS`], that exists only while a deadline is waiting; a call that
 * answers cancels its deadline and leaves nothing behind.
 */
export function after(wait, ms) {
  latestWait = wait;
  let entry;
  const promise = new Promise((resolve) => {
    entry = { at: Date.now() + skew + ms, resolve };
  });
  deadlines.add(entry);
  tick();
  return {
    promise,
    cancel() {
      deadlines.delete(entry);
    },
  };
}

/** How many deadlines are waiting on the shared clock (for the stress test). */
export function pendingDeadlines() {
  return deadlines.size;
}

function expire(now) {
  for (const entry of [...deadlines]) {
    if (entry.at <= now) {
      deadlines.delete(entry);
      entry.resolve();
    }
  }
}

function tick() {
  if (ticking) return;
  ticking = true;
  (async () => {
    try {
      while (deadlines.size > 0) {
        const now = Date.now() + skew;
        let next = Infinity;
        for (const entry of deadlines) next = Math.min(next, entry.at);
        const nap = Math.max(10, Math.min(TICK_MS, next - now));
        const began = Date.now();
        await latestWait(nap);
        // A wait that returns has let `nap` pass, whatever the clock says (a
        // test's instant wait must still run a deadline out, not spin): the
        // shortfall is kept as skew so later deadlines see the same time.
        skew += Math.max(0, nap - (Date.now() - began));
        expire(Date.now() + skew);
      }
    } catch {
      // The copy that owned the clock is gone: nothing will wake these, so they
      // all run out now rather than hang a panel for ever.
      expire(Infinity);
    } finally {
      ticking = false;
      // A deadline added while the loop was ending still needs a clock.
      if (deadlines.size > 0) tick();
    }
  })();
}

/**
 * Runs `work` unless a run is in flight, in which case one more run is queued
 * for when it ends; any number of callers meanwhile share that one.
 *
 * A refresh started by the timer, by the change file and by a button must not
 * overlap: each one used to hold its own reads and sleeps.
 */
export function singleFlight(owner, work) {
  if (owner.__flight) {
    owner.__again = true;
    return owner.__flight;
  }
  owner.__flight = (async () => {
    try {
      do {
        owner.__again = false;
        await work();
      } while (owner.__again);
    } finally {
      owner.__flight = null;
    }
  })();
  return owner.__flight;
}

/**
 * Finds the service for the space the window shows: `{base, dir, record, token,
 * health, folder}` or null when there is none.
 *
 * Several repositories can be served at once, each with its own state directory,
 * so the record is the one whose `root` holds the shown space's folder
 * ([`pickService`]).
 *
 * The `id` check is the point of the handshake (Shepherdr's GitHub #635): a
 * record left behind by a service that died names a port something else may now
 * hold, and only the service that wrote the record answers its `id`. The token
 * is read *after* that check, so a squatter never receives it.
 *
 * `io` is `{read, fetch, wait, dir, folder, run}` when a caller supplies its own
 * (a test); the defaults are Herdr's `readTextFile` and the mod runtime's `fetch`.
 * `io.dir` names one state directory outright and skips the search.
 */
export async function discover(io = {}) {
  const read = io.read ?? readTextFile;
  const doFetch = io.fetch ?? fetch;
  const folder = io.folder === undefined ? activeFolder() : io.folder;
  let dir = io.dir;
  let record = null;
  if (dir) {
    record = await readServiceRecord(serviceFile(dir), read);
  } else {
    const xdg = io.xdg ?? (await stateHome(io.run));
    const entries = await listServices(stateDir(home(), xdg), io.run);
    let entry = pickService(entries, folder);
    if (!entry && folder && entries.length > 0) {
      const root = await canonicalRoot(folder, io.run);
      if (root) entry = pickService(entries, root);
    }
    if (entry) {
      dir = entry.dir;
      record = entry.record;
    }
  }
  if (!record) return null;
  const base = baseUrl(record);
  const health = await getJson(doFetch, `${base}/api/health`, io.wait ?? null);
  if (!health || health.id !== record.id) return null;
  const token = await readToken(tokenFile(dir), read);
  if (!token) return null;
  return { base, dir, record, token, health, folder: folder ?? null };
}

/**
 * One service, as a mod uses it: reads, writes and the long poll.
 *
 * A client is made from a `discover()` result and reused. When the service
 * restarts, its `id` changes and every call fails at the gate: the mod drops the
 * client and calls `discover()` again rather than holding a dead port.
 */
export class Client {
  /**
   * @param {{base: string, dir: string, record: object, token: string, folder?: string}} found
   *   what `discover` answered
   * @param {{fetch?: Function, wait?: Function}} [io] the runtime's own `fetch`
   *   and its "give up after" helper; a test passes its own
   */
  constructor(found, io = {}) {
    this.base = found.base;
    this.dir = found.dir;
    this.record = found.record;
    this.folder = found.folder ?? null;
    this.token = found.token;
    this.fetch = io.fetch ?? fetch;
    this.wait = io.wait ?? null;
  }

  /** The headers every call carries; a write adds `x-shprd: 1`. */
  headers(write = false) {
    const headers = { authorization: `Bearer ${this.token}` };
    if (write) {
      headers["content-type"] = "application/json";
      headers["x-shprd"] = "1";
    }
    return headers;
  }

  /**
   * This client's identity: the `id` in the `service.json` it was made from.
   *
   * [`attach`] compares it with the record on disk to notice a restart, so a
   * panel can tell "the service I was talking to" from "the service there is
   * now" without waiting for a call to fail.
   */
  id() {
    return this.record?.id ?? null;
  }

  /**
   * One call, answered as JSON (or `null` when a read ran out of time).
   *
   * A refusal is a [`ServiceError`] carrying what the service said, because the
   * person needs the words ("a transition rule refused this") and not a status
   * code. A read that ran out of time answers `null` unless `strict` is set, in
   * which case it throws [`noAnswer`]: a panel that must not paint stale asks
   * for that, because a service that went away is exactly a read that never
   * answers.
   */
  async call(method, path, { body = null, wait = READ_WAIT, strict = false } = {}) {
    const write = method !== "GET";
    const url = `${this.base}${path}`;
    const init = { method, headers: this.headers(write) };
    if (body !== null) init.body = JSON.stringify(body);
    const call = (async () => {
      const answer = await this.fetch(url, init);
      const text = await answer.text();
      let parsed = null;
      if (typeof text === "string" && text.trim() !== "") {
        try {
          parsed = JSON.parse(text);
        } catch {
          parsed = null;
        }
      }
      if (!answer.ok) {
        throw new ServiceError(describe(parsed, answer.status), {
          status: answer.status,
          code: parsed?.code ?? "http",
          detail: parsed,
        });
      }
      return parsed;
    })();
    // A write is never given up on by the client: the person pressed a button and
    // has to hear what happened, so it waits for the service's own answer.
    if (write || !this.wait) return call;
    const bounded = guard(call, this.wait, wait);
    return strict ? answered(bounded) : bounded;
  }

  /**
   * `GET /api/state`: the whole board, folded now.
   *
   * A panel paints this, so it is read strictly: the answer is the board or a
   * [`noAnswer`], never `null` left to be mistaken for "nothing new" (bug 5 of
   * the M10 gate: a panel kept painting a service that had restarted).
   */
  state() {
    return this.call("GET", "/api/state", { strict: true });
  }

  /**
   * `GET /api/graph`: where the graph's program is and the command that draws
   * the run.
   *
   * The service answers with what the binary found (`graph.dagr`, `PATH`, the
   * herdr plugin), so a mod never probes the machine itself: a mod has no
   * environment, and running `dagr check` from one would be a second answer to
   * the question the service already answered.
   */
  graph() {
    return this.call("GET", "/api/graph");
  }

  /** `GET /api/asks`: every card still waiting for the person. */
  asks() {
    return this.call("GET", "/api/asks", { strict: true });
  }

  /** `POST /api/asks/answer`: answer one card, exactly once. */
  answerAsk(task, ask, answer) {
    return this.call("POST", "/api/asks/answer", { body: { task, ask, answer } });
  }

  /** `POST /api/action`: one of the person's actions on a task. */
  action(payload) {
    return this.call("POST", "/api/action", { body: payload });
  }

  /** `POST /api/action` `task.resend_brief`: type the task's brief into its agent again. */
  resendBrief(task) {
    return this.action({ kind: "task.resend_brief", task });
  }

  /**
   * `POST /api/tasks`: add one task, as `shprd agent task add` does.
   *
   * The service names it from the title when `body.id` is left out, so a form
   * that asks for a title, a scope and an acceptance line needs nothing else;
   * `deps` (task ids it waits for) is passed through.
   */
  addTask(body) {
    return this.call("POST", "/api/tasks", { body });
  }

  /**
   * `GET /api/tasks/<id>/changes`: the task's branch against its base.
   *
   * It reads git rather than the fold, so it is slower than a snapshot read and
   * asked for the one task a card is showing, never for the board.
   */
  taskChanges(id) {
    return this.call("GET", `/api/tasks/${encodeURIComponent(id)}/changes`);
  }

  /**
   * `GET /api/agents/{id}/output`: the agent's recent screen, as text.
   *
   * It is the read-only fallback of a thread's Terminal view, for an agent whose
   * terminal the window cannot show (it has ended, or lives on another machine).
   */
  agentOutput(id, lines = 120) {
    return this.call("GET", `/api/agents/${encodeURIComponent(id)}/output?lines=${lines}`);
  }

  /** `GET /api/settings`: every key, its value and the layer it came from. */
  settings() {
    return this.call("GET", "/api/settings");
  }

  /** `PUT /api/settings`: write one key, and answer the settings as they now are. */
  putSetting(key, value, forRepo = null) {
    const body = { key };
    if (value !== null && value !== undefined) body.value = value;
    if (forRepo) body.for_repo = forRepo;
    return this.call("PUT", "/api/settings", { body });
  }

  /**
   * `GET /api/roster`: the programs a role may run, with the models and effort
   * levels each one's catalog and harness table say it takes.
   *
   * It reads the shipped data and may ask a program for its own model listing,
   * so it can take a moment the first time; a page reads it once and keeps it.
   */
  roster() {
    return this.call("GET", "/api/roster", { wait: ROSTER_WAIT });
  }

  /**
   * `POST /api/agents`: start a program's tab, as the tab bar's "+" menu asks.
   *
   * The service opens it — a mod starts no program — and answers the tab it
   * opened: `{program, name, tab}`. The roster's `installed` is what says whether
   * a program can be asked for at all.
   */
  openAgent(program, space = null) {
    const body = space ? { program, space } : { program };
    return this.call("POST", "/api/agents", { body });
  }

  /**
   * `POST /api/coordinator/restart`: end the space's coordinator and start it
   * again from its role, in the same tab. A new program or model reaches a
   * coordinator that is running only this way.
   */
  restartCoordinator() {
    return this.call("POST", "/api/coordinator/restart", { body: {} });
  }

  /** `GET /api/prompts`: every template this machine has, without its words. */
  prompts() {
    return this.call("GET", "/api/prompts");
  }

  /** `GET /api/prompts/:id`: one role prompt's words. */
  prompt(id) {
    return this.call("GET", `/api/prompts/${encodeURIComponent(id)}`);
  }

  /** `PUT /api/prompts/:id`: the person's words over that prompt's file. */
  putPrompt(id, text) {
    return this.call("PUT", `/api/prompts/${encodeURIComponent(id)}`, {
      body: { text },
    });
  }

  /**
   * `DELETE /api/prompts/:id`: the shipped words put back over an edit.
   *
   * It is the only route that replaces the person's words, and it does so with
   * the words this build ships (`shprd prompts reset`), never with text a page
   * sends.
   */
  resetPrompt(id) {
    return this.call("DELETE", `/api/prompts/${encodeURIComponent(id)}`);
  }

  /**
   * `GET /api/changes?since=<seq>`: the events after `seq`, or `null` when the
   * wait ran out (`204`, which is not a failure: nothing happened yet).
   *
   * The wait is bounded at the service (`timeout` seconds, 30 at most) and the
   * client waits a little longer than that for the answer.
   */
  changes(since = 0, timeout = POLL_WAIT) {
    const bounded = Math.max(1, Math.min(30, Math.floor(timeout)));
    const path = `/api/changes?since=${encodeURIComponent(since)}&timeout=${bounded}`;
    return this.call("GET", path, { wait: (bounded + 5) * 1000 });
  }
}

/**
 * The service a panel should talk to now, and whether it changed under it.
 *
 * A panel keeps one [`Client`] between reads, and a `shprd serve` that restarted
 * is a different service: its `id` is new, its port may be the same one, and its
 * board has moved on. Waiting for a call to fail is not enough to notice — a
 * guarded read against a dead port answers `null` and looks like nothing new
 * (the M10 gate's bug 5) — so the discovery record on disk is read before every
 * read of the board, and a record that names another `id` (or is gone) is a
 * restart.
 *
 * `current` is the panel's client, or `null` when it has none yet. `io` is what
 * [`discover`] takes (`read`, `fetch`, `wait`, `dir`, `run`), so a test drives
 * this with a table of files and a fake fetch.
 *
 * The answer is `{client, restarted}`: `client` is `null` when no service is
 * running, `restarted` is true when the panel's old client is no longer the
 * service on disk and every cache it kept must be thrown away.
 */
export async function attach(current, io = {}) {
  const read = io.read ?? readTextFile;
  const make = (found) => new Client(found, { fetch: io.fetch, wait: io.wait });
  // One empty answer is not "not running": a busy service misses a health check
  // now and then, so a null is looked for once more before it is believed.
  const find = async () => (await discover(io)) ?? (await discover(io));
  if (!current) {
    const found = await find();
    return { client: found ? make(found) : null, restarted: false };
  }
  // The window moved to another repository's space: that is another service, and
  // everything the panel kept belongs to the one it left.
  const folder = io.folder === undefined ? activeFolder() : io.folder;
  if ((folder ?? null) !== (current.folder ?? null)) {
    const found = await find();
    return { client: found ? make(found) : null, restarted: true };
  }
  const record = await readServiceRecord(serviceFile(current.dir), read);
  if (record && record.id === current.id()) return { client: current, restarted: false };
  const found = await find();
  return { client: found ? make(found) : null, restarted: true };
}

/** One sentence from a refusal body, for a toast or an inline error. */export function describe(body, status = 0) {
  if (body && typeof body === "object") {
    const message = body.message ?? body.error;
    if (typeof message === "string" && message.trim() !== "") return message.trim();
    if (typeof body.code === "string") return body.code;
  }
  if (typeof body === "string" && body.trim() !== "") return body.trim();
  return status > 0 ? `the service answered ${status}` : "the service did not answer";
}

/**
 * Whether `client` is still the service that answers its `id` at `/api/health`.
 *
 * [`attach`] reads the discovery record, which is the cheap check and catches a
 * restart that wrote a new one. This is the other half: a service killed outright
 * leaves its record behind, and the only honest answer then is whether anything
 * answers that record's `id` any more. A long poll that fails is checked with
 * this before it is retried, so a dead port is looked for again rather than
 * spun on.
 */
export async function alive(client) {
  const health = await getJson(client.fetch, `${client.base}/api/health`, client.wait);
  return Boolean(health && health.id === client.id());
}

/** One sentence for whatever a failed call threw, so a panel can show it. */
export function reason(err) {
  if (err instanceof ServiceError) return err.message;
  const text = err?.message ?? String(err ?? "");
  return text.trim() === "" ? "the service did not answer" : text.trim();
}
