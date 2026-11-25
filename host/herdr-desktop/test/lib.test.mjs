// Unit tests for the mod-to-service client: where it looks for a service, how it
// decides the service answering is the one the discovery file names, and how it
// follows the change file.
//
//   node --import ./test/boot.mjs --test test/          (node 22+)
//   bun test test/                                      (bun)
//
// `test/stub-herdr.js` stands in for the app's `herdr` module (its `home` and
// `readTextFile`), and every `fetch` here is a plain function: no window, no
// network, no service.

import test from "node:test";
import assert from "node:assert/strict";

import * as herdr from "herdr";
import {
  Client,
  attach,
  forgetRegistered,
  forgetWoken,
  WAKE_EVERY_MS,
  pendingDeadlines,
  singleFlight,
  ServiceError,
  baseUrl,
  changeFile,
  describe,
  discover,
  downNotice,
  forgetStateHome,
  normalizeFolder,
  repoName,
  probeStateDir,
  readServiceRecord,
  readToken,
  reason,
  serviceFile,
  stateDir,
  stateHome,
  tokenFile,
} from "../mods/lib/client.js";
import {
  changeMatters,
  followChanges,
  parseChange,
  readChange,
  sameChange,
} from "../mods/lib/watch.js";

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

test.beforeEach(() => herdr.reset());

// -- where the state directory is ------------------------------------------

test("the state directory follows XDG, and falls back to the home", () => {
  assert.equal(
    stateDir("/home/ana", { state: "/var/state" }),
    "/var/state/shprd",
  );
  assert.equal(stateDir("/home/ana", {}), "/home/ana/.local/state/shprd");
  // An empty XDG_STATE_HOME is unset, which is what the specification says.
  assert.equal(stateDir("/home/ana", { state: "" }), "/home/ana/.local/state/shprd");
  assert.equal(stateDir("/home/ana"), "/home/ana/.local/state/shprd");
});

test("the three files live under the state directory", () => {
  const dir = "/state/shprd";
  assert.equal(serviceFile(dir), "/state/shprd/service.json");
  assert.equal(tokenFile(dir), "/state/shprd/token");
  assert.equal(changeFile(dir), "/state/shprd/changes");
});

// -- port discovery ---------------------------------------------------------

test("service.json is accepted only with a usable port and an id", async () => {
  const good = '{"port": 7788, "id": "abc", "pid": 12, "version": "0.1.0"}';
  assert.deepEqual(
    await readServiceRecord("/f", async () => good),
    { port: 7788, id: "abc", pid: 12, version: "0.1.0", root: "" },
  );

  const read = (text) => async () => text;
  assert.equal(await readServiceRecord("/f", read("")), null, "empty");
  assert.equal(await readServiceRecord("/f", read("not json")), null, "not JSON");
  assert.equal(await readServiceRecord("/f", read('{"id": "abc"}')), null, "no port");
  assert.equal(
    await readServiceRecord("/f", read('{"port": 0, "id": "abc"}')),
    null,
    "port zero",
  );
  assert.equal(
    await readServiceRecord("/f", read('{"port": 70000, "id": "abc"}')),
    null,
    "port too large",
  );
  assert.equal(
    await readServiceRecord("/f", read('{"port": 7788}')),
    null,
    "no id",
  );
  assert.equal(
    await readServiceRecord("/f", async () => null),
    null,
    "a missing file",
  );
});

test("a token is trimmed, and an empty one is no token", async () => {
  assert.equal(await readToken("/t", async () => "  deadbeef\n"), "deadbeef");
  assert.equal(await readToken("/t", async () => "   \n"), null);
  assert.equal(await readToken("/t", async () => null), null);
});

test("the handshake refuses a service whose id is not the record's", async () => {
  const dir = stateDir("/home/ana", {});
  herdr.putFile(serviceFile(dir), '{"port": 7788, "id": "the-one"}');
  herdr.putFile(tokenFile(dir), "secret\n");

  // Another app holds the port and answers somebody else's health.
  const squatter = fakeFetch({
    "/api/health": { body: { ok: true, id: "somebody-else" } },
  });
  assert.equal(
    await discover({ dir, fetch: squatter }),
    null,
    "a squatter must not be taken for the service",
  );
  // And it was never given the token: only health was asked.
  assert.equal(squatter.calls.length, 1);
  assert.equal(squatter.calls[0].url, "http://127.0.0.1:7788/api/health");
});

test("a service that is not there answers nothing, and is not an error", async () => {
  const dir = stateDir("/home/ana", {});
  assert.equal(await discover({ dir, fetch: fakeFetch({}) }), null);
});

test("discovery answers the address, the token and the health", async () => {
  const dir = stateDir("/home/ana", {});
  herdr.putFile(serviceFile(dir), '{"port": 7788, "id": "abc", "pid": 1}');
  herdr.putFile(tokenFile(dir), "token-1\n");
  const doFetch = fakeFetch({
    "/api/health": { body: { ok: true, id: "abc", version: "0.0.0" } },
  });

  const found = await discover({ dir, fetch: doFetch });
  assert.equal(found.base, "http://127.0.0.1:7788");
  assert.equal(found.token, "token-1");
  assert.equal(found.record.port, 7788);
  assert.equal(found.health.id, "abc");
  // It read the record, then health, then the token — in that order, so the
  // token only goes out after the id matched.
  assert.deepEqual(herdr.READS, [serviceFile(dir), tokenFile(dir)]);
});

test("a record with no token beside it is no service", async () => {
  const dir = stateDir("/home/ana", {});
  herdr.putFile(serviceFile(dir), '{"port": 7788, "id": "abc"}');
  const doFetch = fakeFetch({
    "/api/health": { body: { ok: true, id: "abc" } },
  });
  assert.equal(await discover({ dir, fetch: doFetch }), null);
});

test("discovery is probed from the home when no directory is given", async () => {
  herdr.setHome("/home/ana");
  const dir = stateDir("/home/ana", {});
  herdr.putFile(serviceFile(dir), '{"port": 9000, "id": "abc"}');
  herdr.putFile(tokenFile(dir), "token-1");
  const doFetch = fakeFetch({ "/api/health": { body: { id: "abc" } } });
  const found = await discover({ fetch: doFetch });
  assert.equal(found.base, "http://127.0.0.1:9000");
  assert.equal(baseUrl({ port: 9000 }), "http://127.0.0.1:9000");
});

test("a mod with no environment asks the shell where the state directory is", async () => {
  // Inside Herdr a mod inherits nothing of the person's shell, so
  // `$XDG_STATE_HOME` is invisible to it even though `shprd serve` obeys it.
  // The probe is how the window finds a service whose state directory moved.
  forgetStateHome();
  const asked = [];
  const run = async (command, args) => {
    asked.push([command, ...(args ?? [])]);
    return { code: 0, stdout: '{"state":"/var/state"}\n', stderr: "" };
  };
  assert.deepEqual(await stateHome(run), { state: "/var/state" });
  assert.deepEqual(asked[0], ["sh", "-c", 'printf \'{"state":"%s"}\\n\' "${XDG_STATE_HOME:-}"']);

  // It is asked once, then kept for a while: a panel calls `discover` on every
  // tick, and a shell spawn per tick is not something to spend on a path.
  await stateHome(run);
  assert.equal(asked.length, 1, "the answer is cached");
  forgetStateHome();

  // And `discover` uses it, which is the whole point: the record is looked for
  // under what the shell said, not under the XDG default.
  herdr.putFile("/var/state/shprd/service.json", '{"port": 7788, "id": "abc"}');
  herdr.putFile("/var/state/shprd/token", "token-1");
  const exec = herdr.exec;
  const found = await discover({
    run: (command, args) => run(command, args),
    fetch: fakeFetch({ "/api/health": { body: { id: "abc" } } }),
  });
  assert.equal(found.dir, "/var/state/shprd");
  assert.equal(found.base, "http://127.0.0.1:7788");
  forgetStateHome();
});

test("the state directory probe reads the shell's last line", async () => {
  const said = async (_command, _args) => ({
    code: 0,
    stdout: 'a greeting\n{"state":"/var/state"}\n',
    stderr: "",
  });
  assert.deepEqual(await probeStateDir(said), { state: "/var/state" });
  // A shell that says nothing usable is no answer, not a failure.
  assert.deepEqual(await probeStateDir(async () => ({ stdout: "" })), {});
  assert.deepEqual(await probeStateDir(async () => ({ stdout: "junk" })), {});
  assert.deepEqual(
    await probeStateDir(async () => {
      throw new Error("no shell");
    }),
    {},
  );
});

// -- the client -------------------------------------------------------------

test("every call carries the token, and a write carries x-shprd too", async () => {
  const doFetch = fakeFetch({
    "/api/state": { body: { seq: 3, tasks: [] } },
    "/api/action": { body: { ok: true, seq: 4 } },
  });
  const client = new Client(
    {
      base: "http://127.0.0.1:7788",
      dir: "/state/shprd",
      record: { port: 7788, id: "abc" },
      token: "token-1",
    },
    { fetch: doFetch },
  );

  const state = await client.state();
  assert.equal(state.seq, 3);
  assert.equal(doFetch.calls[0].init.headers.authorization, "Bearer token-1");
  assert.equal(doFetch.calls[0].init.headers["x-shprd"], undefined);

  await client.action({ kind: "task.status", task: "T1", msg: "working" });
  assert.equal(doFetch.calls[1].init.headers["x-shprd"], "1");
  assert.equal(
    doFetch.calls[1].init.headers["content-type"],
    "application/json",
  );
  assert.deepEqual(JSON.parse(doFetch.calls[1].init.body), {
    kind: "task.status",
    task: "T1",
    msg: "working",
  });
});

test("a shown repository with no running coordinator asks the service to start one, at most once in a while", async () => {
  forgetWoken();
  let live = [];
  const calls = [];
  const doFetch = async (url, init = {}) => {
    calls.push(`${init.method ?? "GET"} ${new URL(url).pathname}`);
    const path = new URL(url).pathname;
    const body = path === "/api/state" ? { seq: 1, tasks: [], live_agents: live } : { id: "r1", root: "/p", dir: "/d" };
    return { ok: true, status: 200, text: async () => JSON.stringify(body), json: async () => body };
  };
  const client = new Client(
    { base: "http://127.0.0.1:7788", dir: "/state/shprd", record: { port: 7788, id: "abc" }, token: "t", folder: "/p", repoId: "r1" },
    { fetch: doFetch },
  );
  await client.state();
  await client.state();
  await new Promise((resolve) => setTimeout(resolve, 5));
  assert.equal(calls.filter((c) => c === "POST /api/repos").length, 1, `${calls}`);
  // A running coordinator is left alone.
  forgetWoken();
  calls.length = 0;
  live = [{ role: "coordinator", program: "claude" }];
  await client.state();
  await new Promise((resolve) => setTimeout(resolve, 5));
  assert.equal(calls.filter((c) => c === "POST /api/repos").length, 0, `${calls}`);
  assert.ok(WAKE_EVERY_MS >= 10_000);
});

test("starting an agent is one write to the service, with the program in it", async () => {
  const doFetch = fakeFetch({
    "/api/agents": { body: { program: "claude", name: "Claude Code", tab: "t7" } },
  });
  const client = new Client(
    {
      base: "http://127.0.0.1:7788",
      dir: "/state/shprd",
      record: { port: 7788, id: "abc" },
      token: "token-1",
    },
    { fetch: doFetch },
  );
  const opened = await client.openAgent("claude");
  assert.deepEqual(opened, { program: "claude", name: "Claude Code", tab: "t7" });
  const call = doFetch.calls[0];
  assert.equal(new URL(call.url).pathname, "/api/agents");
  assert.equal(call.init.method, "POST");
  // It is a write: the app's own header, and the token.
  assert.equal(call.init.headers["x-shprd"], "1");
  assert.equal(call.init.headers.authorization, "Bearer token-1");
  assert.deepEqual(JSON.parse(call.init.body), { program: "claude" });
});

test("a refusal is a ServiceError carrying the service's words", async () => {
  const doFetch = fakeFetch({
    "/api/action": {
      status: 422,
      body: { code: "refused", message: "a rule refused this" },
    },
  });
  const client = new Client(
    {
      base: "http://127.0.0.1:7788",
      dir: "/state/shprd",
      record: { port: 7788, id: "abc" },
      token: "token-1",
    },
    { fetch: doFetch },
  );

  await assert.rejects(
    () => client.action({ kind: "task.add", task: "T1" }),
    (err) => {
      assert.ok(err instanceof ServiceError);
      assert.equal(err.status, 422);
      assert.equal(err.code, "refused");
      assert.equal(err.message, "a rule refused this");
      assert.equal(reason(err), "a rule refused this");
      return true;
    },
  );
});

test("a read that runs out of time is no answer, and a strict read says so", async () => {
  const never = async () => new Promise(() => {});
  const bounds = [];
  // The runtime's own "give up after" helper, shortened for the test: what is
  // asserted is the bound the client asked for, not the wall clock.
  const wait = async (ms) => {
    bounds.push(ms);
    return null;
  };
  const client = new Client(
    {
      base: "http://127.0.0.1:7788",
      dir: "/state/shprd",
      record: { port: 7788, id: "abc" },
      token: "token-1",
    },
    { fetch: never, wait },
  );

  // A plain read is bounded at 4 s (`READ_WAIT`) and answers null.
  assert.equal(await client.call("GET", "/api/state"), null);
  // The shared clock sleeps in ticks; together they cover READ_WAIT.
  assert.ok(bounds.every((ms) => ms <= 500), "one short sleep at a time");
  const asked = bounds.reduce((sum, ms) => sum + ms, 0);
  assert.ok(asked >= 3990 && asked <= 4010, `a read asks for READ_WAIT, asked ${asked}`);

  // The board is not a plain read: a panel paints it, so no answer at all is a
  // failure (the M10 gate's bug 5 — a `null` board looked like "nothing new"
  // and froze the panels for ever). It throws, and the panel's `catch` drops the
  // client and looks for the service again.
  await assert.rejects(
    () => client.state(),
    (err) => {
      assert.equal(err.name, "ServiceError");
      assert.equal(err.code, "no-answer");
      assert.equal(err.status, 0);
      return true;
    },
  );

  // A write is not bounded by the client at all: the person pressed a button,
  // so it waits for whatever the service says.
  const before = bounds.length;
  let settled = false;
  const write = client.action({ kind: "task.status", task: "T1", msg: "x" }).then(() => {
    settled = true;
  });
  await new Promise((resolve) => setTimeout(resolve, 20));
  assert.equal(settled, false, "the write is still waiting, as it must be");
  assert.equal(bounds.length, before, "and it was never handed to the wait");
  void write;
});

test("the long poll names the sequence and the wait it wants", async () => {
  const doFetch = fakeFetch({
    "/api/changes": { body: { seq: 9, events: [] } },
  });
  const client = new Client(
    {
      base: "http://127.0.0.1:7788",
      dir: "/state/shprd",
      record: { port: 7788, id: "abc" },
      token: "token-1",
    },
    { fetch: doFetch },
  );
  const answer = await client.changes(8, 20);
  assert.equal(answer.seq, 9);
  const url = new URL(doFetch.calls[0].url);
  assert.equal(url.pathname, "/api/changes");
  assert.equal(url.searchParams.get("since"), "8");
  assert.equal(url.searchParams.get("timeout"), "20");

  // A wait is bounded at the service (30 s) whatever the caller asks for.
  await client.changes(0, 600);
  assert.equal(
    new URL(doFetch.calls[1].url).searchParams.get("timeout"),
    "30",
  );
});

test("a 204 from the long poll is an empty answer, not a failure", async () => {
  const doFetch = fakeFetch({ "/api/changes": { status: 204, body: undefined } });
  const client = new Client(
    {
      base: "http://127.0.0.1:7788",
      dir: "/state/shprd",
      record: { port: 7788, id: "abc" },
      token: "token-1",
    },
    { fetch: doFetch },
  );
  assert.equal(await client.changes(3, 5), null);
});

test("a refusal body without words still says something", () => {
  assert.equal(describe({ code: "unauthorized" }, 401), "unauthorized");
  assert.equal(describe({ message: "  no token  " }, 401), "no token");
  assert.equal(describe(null, 500), "the service answered 500");
  assert.equal(describe(null, 0), "the service did not answer");
  assert.equal(reason(new Error("boat")), "boat");
  assert.equal(reason(null), "the service did not answer");
});

// -- the change file --------------------------------------------------------

test("the change file is parsed, and a partial one is not a change", () => {
  assert.deepEqual(parseChange('{"seq": 4, "at_ms": 100}'), { seq: 4, at: 100 });
  assert.deepEqual(parseChange('{"seq": 4}'), { seq: 4, at: null });
  assert.deepEqual(parseChange('{"at_ms": 100}'), { seq: null, at: 100 });
  assert.equal(parseChange(""), null);
  assert.equal(parseChange("{"), null);
  assert.equal(parseChange('{"other": 1}'), null);
  assert.equal(parseChange(null), null);
  assert.equal(sameChange({ seq: 1, at: 2 }, { seq: 1, at: 2 }), true);
  assert.equal(sameChange({ seq: 1, at: 2 }, { seq: 2, at: 2 }), false);
  assert.equal(sameChange(null, null), true);
  assert.equal(sameChange(null, { seq: 1, at: 2 }), false);
});

test("a batch matters unless it names only other files", () => {
  const dir = "/state/shprd";
  // The change file itself, by either of its spellings.
  assert.equal(changeMatters(["/state/shprd/changes"], dir), true);
  assert.equal(changeMatters(["/private/var/state/shprd/changes"], dir), true);
  // The folder itself, and an empty batch, both mean "look again".
  assert.equal(changeMatters(["/state/shprd"], dir), true);
  assert.equal(changeMatters([], dir), true);
  // The token and the discovery record are not a ledger write.
  assert.equal(changeMatters(["/state/shprd/token"], dir), false);
  assert.equal(changeMatters(["/state/shprd/service.json"], dir), false);
});

test("the change file is read through the injected reader", async () => {
  const dir = "/state/shprd";
  herdr.putFile(changeFile(dir), '{"seq": 7, "at_ms": 70}');
  assert.deepEqual(await readChange(dir), { seq: 7, at: 70 });
  // A service that never wrote one is no change, not a failure.
  assert.equal(await readChange("/nowhere"), null);
});

test("following reports the first reading and then only real changes", async () => {
  const dir = "/state/shprd";
  const file = changeFile(dir);
  let text = '{"seq": 1, "at_ms": 10}';
  const seen = [];
  const batches = [
    ["/state/shprd/token"], // not the change file: no re-read
    [file], // the first change
    [file], // the same content again: nothing to report
    [file, "/state/shprd/token"],
  ];
  let index = 0;
  const watch = async () => {
    const batch = batches[index++] ?? [file];
    if (index === 2) text = '{"seq": 2, "at_ms": 20}';
    if (index > batches.length + 1) {
      // End the loop the way a closed copy does: the promise never resolves.
      return new Promise(() => {});
    }
    return batch;
  };
  const read = async () => text;

  const loop = followChanges(
    dir,
    (change) => {
      seen.push(change);
      if (seen.length === 2) {
        // Two readings is all this test needs; let the loop hang like a closed
        // copy rather than reach for a timer.
      }
    },
    { read, watch },
  );
  await new Promise((resolve) => setTimeout(resolve, 30));
  assert.deepEqual(seen, [
    { seq: 1, at: 10 },
    { seq: 2, at: 20 },
  ]);
  void loop;
});

test("a watcher that rejects ends the loop instead of the panel", async () => {
  let read = 0;
  await followChanges(
    "/state/shprd",
    () => {
      read += 1;
    },
    {
      read: async () => '{"seq": 1, "at_ms": 1}',
      watch: async () => {
        throw new Error("the folder is not there");
      },
    },
  );
  assert.equal(read, 1, "the first reading still happened");
});

// SHPR-19: a mod's only clock is `cx.sleep` and the runtime allows 1024
// outstanding ones. Guarded calls share one sleep at a time.
test("a busy client never has more than one sleep outstanding", async () => {
  let outstanding = 0;
  let peak = 0;
  let total = 0;
  const wait = (ms) => {
    outstanding += 1;
    total += 1;
    peak = Math.max(peak, outstanding);
    return new Promise((resolve) =>
      setTimeout(() => {
        outstanding -= 1;
        resolve();
      }, ms),
    );
  };
  const client = new Client(
    { base: "http://127.0.0.1:1", dir: "/s", record: { id: "a" }, token: "t" },
    { fetch: async () => ({ ok: true, text: async () => '{"a":1}' }), wait },
  );
  for (let round = 0; round < 40; round += 1) {
    await Promise.all(Array.from({ length: 50 }, () => client.state()));
  }
  assert.equal(pendingDeadlines(), 0, "every answered call cancelled its deadline");
  assert.ok(peak <= 1, `peak outstanding sleeps was ${peak}`);
  await new Promise((r) => setTimeout(r, 700));
  assert.equal(outstanding, 0, "the clock stops when nothing waits");
  assert.ok(total < 5, `2000 calls used ${total} sleeps`);
});

test("a call that never answers still runs out", async () => {
  const wait = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
  const client = new Client(
    { base: "http://127.0.0.1:1", dir: "/s", record: { id: "a" }, token: "t" },
    { fetch: () => new Promise(() => {}), wait },
  );
  const started = Date.now();
  assert.equal(await client.call("GET", "/x", { wait: 300 }), null);
  assert.ok(Date.now() - started < 1500);
  await assert.rejects(client.call("GET", "/x", { wait: 200, strict: true }));
});

test("singleFlight never overlaps and folds callers into one rerun", async () => {
  const owner = {};
  let running = 0;
  let peak = 0;
  let runs = 0;
  const work = async () => {
    running += 1;
    runs += 1;
    peak = Math.max(peak, running);
    await new Promise((r) => setTimeout(r, 20));
    running -= 1;
  };
  await Promise.all(Array.from({ length: 10 }, () => singleFlight(owner, work)));
  assert.equal(peak, 1);
  assert.equal(runs, 2, "one run plus one folded rerun");
});

test("attach looks once more before believing a null", async () => {
  let health = 0;
  const files = {
    "/s/shprd/service.json": '{"port": 7788, "id": "abc"}',
    "/s/shprd/token": "tok",
  };
  const io = {
    dir: "/s/shprd",
    read: async (path) => files[path] ?? null,
    fetch: async () => {
      health += 1;
      return health === 1
        ? null
        : { ok: true, json: async () => ({ ok: true, id: "abc" }) };
    },
  };
  const next = await attach(null, io);
  assert.ok(next.client, "the second look found the service");
});

// -- several repositories, one service (SHPR-77) -------------------------------

/** The one service's files, and a fake `fetch` that answers health and registration. */
function oneService(extra = {}) {
  const dir = stateDir("/home/ana", {});
  herdr.setHome("/home/ana");
  herdr.putFile(serviceFile(dir), JSON.stringify({ port: 7001, id: "id-one", pid: 1 }));
  herdr.putFile(tokenFile(dir), "token-one\n");
  const registered = [];
  const doFetch = fakeFetch({
    "/api/health": { body: { id: "id-one" } },
    "/api/state": { body: { seq: 1, tasks: [] } },
    "/api/roster": { body: {} },
    ...extra,
    "/api/repos": (_url, init) => {
      const folder = JSON.parse(init.body).folder;
      registered.push(folder);
      const id = `${folder.split("/").pop()}-hash`;
      return { body: { id, root: folder, dir: `${dir}/repos/${id}` } };
    },
  });
  doFetch.registered = registered;
  return doFetch;
}

test("one record finds the one service whatever space the window shows", async () => {
  const doFetch = oneService();
  const shop = await discover({ fetch: doFetch, folder: "/work/shop" });
  const pad = await discover({ fetch: doFetch, folder: "/work/scratchpad" });
  const none = await discover({ fetch: doFetch, folder: null });
  assert.equal(shop.base, "http://127.0.0.1:7001");
  assert.equal(pad.base, shop.base, "the same service");
  assert.equal(pad.token, "token-one");
  assert.ok(none, "even with no space shown the service is there");
  assert.equal(normalizeFolder("/private/tmp/x/"), "/tmp/x");
  assert.equal(repoName("/work/shop/"), "shop");
});

test("a repository's calls name it, and a global route does not", async () => {
  const doFetch = oneService();
  forgetRegistered();
  const made = await attach(null, { fetch: doFetch, folder: "/work/shop" });
  await made.client.state();
  await made.client.call("GET", "/api/roster");
  const urls = doFetch.calls.map((call) => call.url);
  assert.ok(urls.some((url) => url.endsWith("/api/state?repo=shop-hash")), urls.join("\n"));
  assert.ok(urls.some((url) => url.endsWith("/api/roster")), "no repo on a global route");
  assert.equal(made.client.dir, `${stateDir("/home/ana", {})}/repos/shop-hash`, "the change file is the repository's own");
});

// The service's repo-scoped route prefixes (`repos.rs` `is_scoped`).
const SERVICE_SCOPED = ["state", "changes", "asks", "chat", "terminal", "action", "graph", "tasks", "settings", "coordinator", "agents", "ws"];

test("every client call about a repository names it and every write sends x-shprd", async () => {
  // Only this test answers every route: the others fail on a call they did not expect.
  const doFetch = oneService({ "*": { body: {} } });
  forgetRegistered();
  const { client } = await attach(null, { fetch: doFetch, folder: "/work/shop" });
  doFetch.calls.length = 0;
  const run = [
    () => client.graph(),
    () => client.asks(),
    () => client.answerAsk("T1", "ask-1", "yes"),
    () => client.action({ kind: "task.drop", task: "T1" }),
    () => client.addTask({ title: "t" }),
    () => client.taskChanges("T1"),
    () => client.agentOutput("a1"),
    () => client.restartCoordinator(),
    () => client.call("POST", "/api/chat", { body: {} }),
  ];
  for (const go of run) await go();
  assert.ok(doFetch.calls.length >= run.length);
  for (const { url, init } of doFetch.calls) {
    const parsed = new URL(url);
    const first = parsed.pathname.split("/")[2];
    if (SERVICE_SCOPED.includes(first)) {
      assert.equal(parsed.searchParams.get("repo"), "shop-hash", `${init.method} ${url} names its repo`);
    }
    if (init.method !== "GET") {
      assert.equal(init.headers["x-shprd"], "1", `${init.method} ${url} sends x-shprd`);
    }
  }
});

test("switching to another space re-points the client: no discovery, no health check", async () => {
  const doFetch = oneService();
  forgetRegistered();
  const made = await attach(null, { fetch: doFetch, folder: "/work/shop" });
  const same = await attach(made.client, { fetch: doFetch, folder: "/work/shop" });
  assert.equal(same.client, made.client);
  assert.equal(same.restarted, false);

  const moved = await attach(made.client, { fetch: doFetch, folder: "/work/scratchpad" });
  assert.equal(moved.client.base, made.client.base, "the same service and token");
  assert.equal(moved.client.folder, "/work/scratchpad");
  assert.equal(moved.restarted, true, "everything kept for the repo left is thrown away");
  assert.deepEqual(doFetch.registered, ["/work/shop", "/work/scratchpad"], "each repo is registered on its first visit");

  const before = doFetch.calls.length;
  const back = await attach(moved.client, { fetch: doFetch, folder: "/work/shop" });
  assert.equal(back.client.folder, "/work/shop");
  assert.equal(doFetch.calls.length, before, "a return visit makes no call at all");
  assert.equal(doFetch.registered.length, 2, "and registers nothing again");
});

test("a restarted service is found again and every repository registered again", async () => {
  const doFetch = oneService();
  forgetRegistered();
  const made = await attach(null, { fetch: doFetch, folder: "/work/shop" });
  const dir = stateDir("/home/ana", {});
  herdr.putFile(serviceFile(dir), JSON.stringify({ port: 7001, id: "id-two", pid: 2 }));
  const fresh = fakeFetch({
    "/api/health": { body: { id: "id-two" } },
    "/api/repos": { body: { id: "shop-hash", root: "/work/shop", dir: `${dir}/repos/shop-hash` } },
  });
  const next = await attach(made.client, { fetch: fresh, folder: "/work/shop" });
  assert.equal(next.restarted, true);
  assert.equal(next.client.id(), "id-two");
  assert.ok(fresh.calls.some((call) => call.url.endsWith("/api/repos")), "the new service is told");
});

test("a panel without a service says why in one plain sentence, never a command", () => {
  herdr.setSpaces([{ id: "s1", active: true, folder: "/work/shop" }]);
  assert.equal(downNotice(), "The shprd service is not up yet");
  herdr.setShared("service.status", { folder: "/work/shop", phase: "starting" });
  assert.equal(downNotice(), "Starting the shprd service…");
  herdr.setShared("service.status", { folder: "/work/shop", phase: "failed", reason: "no git repository" });
  assert.equal(downNotice(), "The shprd service isn't running (the reason is in the footer)");
  herdr.setSpaces([]);
  assert.equal(downNotice(), "Open a repository's space to use shprd");
  assert.doesNotMatch(downNotice("/work/shop"), /shprd serve/);
});

test("a ready task's needs-you words point at the Tasks pane, never a command or the forge", async () => {
  const { desktopWords } = await import("../mods/lib/tokens.js");
  for (const body of [
    "T5 is ready to land and its merge policy asks: approve its pull request on the forge, or run `shprd approve T5`",
    "T1 is ready to land: run `shprd approve T1`",
  ]) {
    const words = desktopWords(body, "T5");
    assert.match(words, /Accept it in the Tasks pane/);
    assert.doesNotMatch(words, /shprd approve|forge/);
  }
  assert.equal(desktopWords("which format?", "T1"), "which format?");
});

// SHPR-75: the sidebar's chevron and name always toggle, picked or not.
import { isOpen, pickOpen, toggleOpen } from "../mods/lib/spaces.js";

test("the chevron toggles a space whether or not it is the picked one", () => {
  const space = { id: "sp-scratch", plain: false };
  const open = new Map();
  assert.equal(isOpen(open, space, "sp-other"), false, "another space is picked: shut");
  toggleOpen(open, space, "sp-other");
  assert.equal(isOpen(open, space, "sp-other"), true, "its chevron opens it from the other pick");
  toggleOpen(open, space, "sp-other");
  assert.equal(isOpen(open, space, "sp-other"), false);
  assert.equal(isOpen(new Map(), space, null), true, "no pick: every one is open");
  toggleOpen(open, space, null);
  assert.equal(isOpen(open, space, null), true, "shut then opened under All spaces: a flip each time");
  assert.equal(isOpen(new Map(), { id: "h", plain: true }, null), false);
});

test("clicking the name opens a shut space and shuts the open picked one", () => {
  const space = { id: "sp-scratch", plain: false };
  const open = new Map();
  pickOpen(open, space, "sp-other");
  assert.equal(isOpen(open, space, "sp-scratch"), true);
  pickOpen(open, space, "sp-scratch");
  assert.equal(isOpen(open, space, "sp-scratch"), false, "the picked, open row's name shuts it");
  pickOpen(open, space, "sp-scratch");
  assert.equal(isOpen(open, space, "sp-scratch"), true);
});

test("dropReason trims what was typed and defaults a blank one", async () => {
  const { dropReason } = await import("../mods/lib/accept.js");
  assert.equal(dropReason("  too slow \n"), "too slow");
  assert.equal(dropReason(""), "No reason given");
  assert.equal(dropReason("   "), "No reason given");
  assert.equal(dropReason(undefined), "No reason given");
});
