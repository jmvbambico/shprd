// A stand-in for `shprd serve`, for looking at the desktop mods without a run.
//
//   XDG_STATE_HOME=$(mktemp -d) node test/serve-fixture.mjs [fixture.json]
//
// It writes the discovery record and token the mods look for under
// `$XDG_STATE_HOME/shprd/`, answers `/api/health` and `/api/state` from a fixture
// (default `fixtures/proto.json`; `chat` there is each task's conversation for
// `/api/chat`), and nothing else. A task's `live_agents` entry
// has `since_s` (seconds ago); it is turned into the absolute `since_ms` the
// contract names (`mods/lib/tree.js`). Refuses to run without `XDG_STATE_HOME`,
// so it can never write into a person's own state directory.

import http from "node:http";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const base = process.env.XDG_STATE_HOME;
if (!base) {
  console.error("serve-fixture: set XDG_STATE_HOME to a scratch folder");
  process.exit(2);
}
const dir = join(base, "shprd");
mkdirSync(dir, { recursive: true });
const fixture = process.argv[2] ?? join(here, "fixtures", "proto.json");
const id = `fixture-${process.pid}`;

/** The fixture, with ages turned into absolute times as of now. */
function state() {
  const doc = JSON.parse(readFileSync(fixture, "utf8"));
  const now = Date.now();
  for (const agent of doc.live_agents ?? []) {
    if (agent.since_s !== undefined) agent.since_ms = now - agent.since_s * 1000;
  }
  return doc;
}

const server = http.createServer((req, res) => {
  const path = new URL(req.url, "http://x").pathname;
  const send = (code, body) => {
    res.writeHead(code, { "content-type": "application/json" });
    res.end(JSON.stringify(body));
  };
  if (path === "/api/health") return send(200, { ok: true, id, version: "fixture" });
  if (path === "/api/state") return send(200, state());
  if (path === "/api/chat") {
    const query = new URL(req.url, "http://x").searchParams;
    const space = query.get("space") ?? "coordinator";
    const known = state().chat?.[space];
    const from = Number((query.get("since") ?? "").split("@")[1] ?? 0) || 0;
    return send(200, {
      space,
      program: known?.program ?? null,
      session: known ? "s1" : null,
      messages: known ? known.messages.slice(from) : [],
      cursor: known ? `s1@${known.messages.length}` : "",
      more: false,
      note: known ? null : "No messages yet.",
    });
  }
  if (path === "/api/asks") return send(200, []);
  if (path === "/api/changes") return setTimeout(() => send(204, {}), 3000);
  return send(404, { error: "the fixture service has no such route" });
});

server.listen(0, "127.0.0.1", () => {
  const { port } = server.address();
  writeFileSync(join(dir, "service.json"), JSON.stringify({ port, id, pid: process.pid, version: "fixture" }));
  writeFileSync(join(dir, "token"), "fixture-token\n");
  writeFileSync(join(dir, "changes"), JSON.stringify({ seq: 1, at_ms: Date.now() }));
  console.log(`serve-fixture: http://127.0.0.1:${port} (state ${dir})`);
});
