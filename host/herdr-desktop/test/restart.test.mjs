// Unit tests for the one thing the M10 gate caught the panels doing wrong: they
// kept painting a `shprd serve` that had restarted (bug 5).
//
//   node --import ./test/boot.mjs --test test/          (node 22+)
//   bun test --preload ./test/boot-bun.mjs test/        (bun)
//
// Two halves, both without a window, a core or a real service:
//
// - the shared client's `attach` / `state` / `alive` (the fix itself), driven by
//   a table of files and a fake `fetch`;
// - a real panel (the tasks mod) through a restart: the board it paints is the
//   new service's, read whole, with no layout reload and nothing left over from
//   the old run.

import test from "node:test";
import assert from "node:assert/strict";

import * as herdr from "herdr";
import Tasks from "../mods/shprd-tasks/main.js";
import {
  Client,
  alive,
  attach,
  changeFile,
  discover,
  forgetStateHome,
  noAnswer,
  serviceFile,
  stateDir,
  tokenFile,
} from "../mods/lib/client.js";

/** The home every test here discovers the service under. */
const HOME = "/home/ana";

/** The state directory under [`HOME`], with the XDG defaults. */
const DIR = stateDir(HOME, {});

/** A fake `fetch`: `routes` maps a URL path to `{status, body}` or a function. */
function fakeFetch(routes) {
  const calls = [];
  const doFetch = async (url, init = {}) => {
    calls.push({ url, init });
    const path = new URL(url).pathname;
    const route = routes[path] ?? routes["*"];
    const answer = typeof route === "function" ? route(url, init) : route;
    if (!answer) throw new Error(`no route for ${path}`);
    const status = answer.status ?? 200;
    const text = answer.body === undefined ? "" : JSON.stringify(answer.body);
    return {
      ok: status >= 200 && status < 300,
      status,
      json: async () => JSON.parse(text),
      text: async () => text,
    };
  };
  doFetch.calls = calls;
  return doFetch;
}

/** A context that answers `cx.sleep`, `cx.notify`, `cx.spawn` and `cx.timer`. */
function context() {
  const every = [];
  const spawns = [];
  return {
    cx: {
      // The mod's own clock: it resolves at once, so a guarded read that has to
      // wait for an answer gives up at once, which is exactly the "no answer"
      // this file is about.
      sleep: async () => {},
      notify: () => {},
      timer: {
        every: (ms, fn) => {
          every.push({ ms, fn });
          return { cancel: () => {} };
        },
        after: (ms, fn) => {
          return { cancel: () => {} };
        },
      },
      spawn: (work) => {
        spawns.push(work);
        return { cancel: () => {} };
      },
    },
    every,
    spawns,
  };
}

/**
 * Puts a service's discovery files under [`HOME`]: `id`, on `port`, with a token.
 *
 * It is a whole service's handshake in one function because that is what a
 * restart replaces: the record, the token and the health answer behind it.
 */
function serve(id, port = 7788) {
  herdr.putFile(serviceFile(DIR), JSON.stringify({ port, id, pid: 42, root: "/work/shop" }));
  herdr.putFile(tokenFile(DIR), "token-1\n");
  return {
    "/api/health": { body: { ok: true, id, version: "0.0.0" } },
    // The window registers the shown repository once per service: it answers at once.
    "/api/repos": { body: { id: "shop-1", root: "/work/shop", dir: `${DIR}/repos/shop-1` } },
  };
}

/** The board a service answers with: one task per id, at `seq`. */
function board(seq, ids) {
  return {
    seq,
    run: null,
    tasks: ids.map((id) => ({ id, title: id, status: "queued", attempts: [] })),
    agents: [],
    needs: [],
    asks: [],
    finished: false,
  };
}

test.beforeEach(() => {
  herdr.reset();
  herdr.setHome(HOME);
  forgetStateHome();
});

// -- the client's half ------------------------------------------------------

test("attach makes a client from the service on disk, and keeps it while it is the same one", async () => {
  const first = serve("one");
  const fetch = fakeFetch({ ...first, "/api/state": { body: board(1, ["T1"]) } });

  const made = await attach(null, { dir: DIR, fetch });
  assert.equal(made.restarted, false);
  assert.equal(made.client.id(), "one");
  assert.equal(made.client.base, "http://127.0.0.1:7788");

  // The same record, read again: the same client, and no restart to announce.
  const again = await attach(made.client, { dir: DIR, fetch });
  assert.equal(again.client, made.client, "the client is reused");
  assert.equal(again.restarted, false);
});

test("attach notices a restarted service by the id in the record, even on the same port", async () => {
  const first = serve("one");
  const fetch = fakeFetch({ ...first, "/api/trusted": { body: {} } });
  const made = await attach(null, { dir: DIR, fetch });

  // `shprd serve` restarts: the port is the one it had, the id is new, and its
  // health answers the new one. This is the case the gate reproduced.
  const second = serve("two");
  const restartedFetch = fakeFetch(second);
  const next = await attach(made.client, { dir: DIR, fetch: restartedFetch });

  assert.equal(next.restarted, true, "a new id is a restart");
  assert.notEqual(next.client, made.client, "a new client, on the new service");
  assert.equal(next.client.id(), "two");
  assert.equal(next.client.base, "http://127.0.0.1:7788", "the port was reused");
});

test("attach answers no client when the service is gone, and says it restarted if it had one", async () => {
  const first = serve("one");
  const fetch = fakeFetch(first);
  const made = await attach(null, { dir: DIR, fetch });

  // The service stopped without leaving a record (the ordinary shutdown).
  herdr.FILES.delete(serviceFile(DIR));
  const gone = await attach(made.client, { dir: DIR, fetch: fakeFetch({}) });
  assert.equal(gone.client, null);
  assert.equal(gone.restarted, true, "the panel's client is stale and must go");

  // And with no client at all there is nothing to restart: just no service.
  const cold = await attach(null, { dir: DIR, fetch: fakeFetch({}) });
  assert.equal(cold.client, null);
  assert.equal(cold.restarted, false);
});

test("attach refuses a record whose id nothing answers, and a token that is not there", async () => {
  // A service killed outright leaves its record behind: the id at health is the
  // only honest answer to whether it is still there.
  serve("one");
  const squatter = fakeFetch({ "/api/health": { body: { ok: true, id: "other" } } });
  assert.equal((await attach(null, { dir: DIR, fetch: squatter })).client, null);

  const fetch = fakeFetch(serve("one"));
  herdr.FILES.delete(tokenFile(DIR));
  assert.equal((await attach(null, { dir: DIR, fetch })).client, null);
});

test("the board read throws when nothing answers, and a refusal keeps its words", async () => {
  const client = new Client(
    { base: "http://127.0.0.1:7788", dir: DIR, record: { port: 7788, id: "one" }, token: "t" },
    { fetch: async () => new Promise(() => {}), wait: async () => null },
  );
  // This is bug 5 in one assertion: a `null` board used to be taken for
  // "nothing new", so a panel kept the old one for ever.
  await assert.rejects(() => client.state(), (err) => {
    assert.equal(err.code, "no-answer");
    return true;
  });

  const refusing = new Client(
    { base: "http://127.0.0.1:7788", dir: DIR, record: { port: 7788, id: "one" }, token: "t" },
    { fetch: fakeFetch({ "/api/state": { status: 401, body: { code: "unauthorized" } } }) },
  );
  await assert.rejects(() => refusing.state(), (err) => {
    assert.equal(err.status, 401);
    assert.equal(err.message, "unauthorized");
    return true;
  });
});

test("a service is alive only while something answers its own id", async () => {
  const client = new Client(
    { base: "http://127.0.0.1:7788", dir: DIR, record: { port: 7788, id: "one" }, token: "t" },
    { fetch: fakeFetch({ "/api/health": { body: { id: "one" } } }) },
  );
  assert.equal(await alive(client), true);
  client.fetch = fakeFetch({ "/api/health": { body: { id: "two" } } });
  assert.equal(await alive(client), false);
  client.fetch = fakeFetch({});
  assert.equal(await alive(client), false, "an unreachable port is not alive");
});

// -- a real panel, through a restart ----------------------------------------

/**
 * The tasks panel, ready to read: a `cx` of its own, and `watching` set so the
 * change-file follower is not started (a test drives `refresh` itself).
 *
 * Its clock is a real timer rather than one that resolves at once: a read has to
 * be *able* to answer, or every read here would be the "no answer" case.
 */
function panel() {
  const run = context();
  run.cx.sleep = (ms) => new Promise((resolve) => setTimeout(resolve, Math.min(ms, 20)));
  const view = new Tasks();
  view.cx = run.cx;
  view.state = null;
  view.failure = null;
  view.client = null;
  view.watching = true;
  view.actionError = null;
  view.addError = null;
  return { view, ...run };
}

test("a panel repaints the whole board after the service restarts", async () => {
  const first = serve("one");
  const boards = [board(22, ["alpha"])];
  let health = first;
  const fetch = fakeFetch({
    get "/api/health"() {
      return health["/api/health"];
    },
    get "/api/state"() {
      return { body: boards[boards.length - 1] };
    },
  });
  const real = globalThis.fetch;
  globalThis.fetch = fetch;
  try {
    const { view } = panel();
    await view.refresh();
    assert.equal(view.state.seq, 22);
    assert.deepEqual(
      view.state.tasks.map((task) => task.id),
      ["alpha"],
    );
    assert.equal(view.failure, null);

    // `shprd serve` restarts on the same port with a new id, and the run has
    // moved on: a task landed and a new one was added (the gate's own case).
    health = serve("two");
    boards.push(board(35, ["alpha", "beta"]));
    await view.refresh();

    assert.equal(view.state.seq, 35, "the board is the new service's");
    assert.deepEqual(
      view.state.tasks.map((task) => task.id),
      ["alpha", "beta"],
      "the new task's row is there without reloading the layout",
    );
    assert.equal(view.client.id(), "two", "the panel is talking to the new service");
    assert.equal(view.failure, null);
  } finally {
    globalThis.fetch = real;
  }
});

test("a panel whose service stopped says so instead of keeping the old board", async () => {
  serve("one");
  const fetch = fakeFetch({ "/api/health": { body: { id: "one" } }, "/api/state": { body: board(7, ["alpha"]) } });
  const real = globalThis.fetch;
  globalThis.fetch = fetch;
  try {
    const { view } = panel();
    await view.refresh();
    assert.equal(view.state.seq, 7);

    // The service died: its record is gone and nothing answers.
    herdr.FILES.delete(serviceFile(DIR));
    globalThis.fetch = fakeFetch({});
    await view.refresh();

    assert.equal(view.state, null, "no old board is left to paint");
    assert.equal(view.client, null);
    assert.match(view.failure, /space|service/);
    assert.doesNotMatch(view.failure, /shprd serve/, "the panel never tells a person to run a command");
  } finally {
    globalThis.fetch = real;
  }
});

test("a board read that runs out of time is a failure, not the board the panel had", async () => {
  serve("one");
  let answering = true;
  const fetch = fakeFetch({
    "/api/health": { body: { id: "one" } },
    "/api/state": () => (answering ? { body: board(7, ["alpha"]) } : new Promise(() => {})),
  });
  const real = globalThis.fetch;
  globalThis.fetch = fetch;
  try {
    const { view } = panel();
    await view.refresh();
    assert.equal(view.state.seq, 7);

    // The service is still in `service.json`, but its board never answers. The
    // wait (`cx.sleep`) resolves at once, so the guarded read gives up, and the
    // strict read turns that into a failure the panel drops its client over.
    answering = false;
    await view.refresh();
    assert.equal(view.client, null, "a dead read is a dead client");
    assert.notEqual(view.failure, null, "and it is said, not silently kept");
  } finally {
    globalThis.fetch = real;
  }
});

test("a panel forgets what a verb said about the run that went away", async () => {
  let health = serve("one");
  const fetch = fakeFetch({
    get "/api/health"() {
      return health["/api/health"];
    },
    "/api/state": { body: board(3, ["alpha"]) },
  });
  const real = globalThis.fetch;
  globalThis.fetch = fetch;
  try {
    const { view } = panel();
    await view.refresh();
    view.actionError = "task.start alpha: a transition rule refused this";

    health = serve("two");
    await view.refresh();
    assert.equal(view.client.id(), "two", "the new service is the one read");
    assert.equal(view.actionError, null, "a new run has said nothing yet");
  } finally {
    globalThis.fetch = real;
  }
});

test("a discovery that has not happened yet reads the board in one go", async () => {
  const first = serve("one");
  const fetch = fakeFetch({ ...first, "/api/state": { body: board(1, ["alpha"]) } });
  const made = await attach(null, { dir: DIR, fetch });
  assert.equal(made.client.state instanceof Function, true);
  const state = await made.client.state();
  assert.equal(state.seq, 1);
  // The change file the panel follows sits under the same directory.
  assert.equal(changeFile(made.client.dir), `${DIR}/changes`);
  // And `discover` and `attach` agree on the record they found.
  const direct = await discover({ dir: DIR, fetch });
  assert.equal(direct.record.id, made.client.id());
});

test("noAnswer carries no status, so a panel can tell a dead service from a refusal", () => {
  const err = noAnswer();
  assert.equal(err.name, "ServiceError");
  assert.equal(err.status, 0);
  assert.equal(err.code, "no-answer");
  assert.equal(err.message, "the service did not answer");
});
