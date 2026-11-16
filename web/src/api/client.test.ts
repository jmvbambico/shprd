// The client is the only door to the service, so these tests pin the three
// things it must never forget: same-origin credentials, `x-shprd` on writes,
// and what it does with a refusal.

import { describe, expect, it, vi } from "vitest";

import type { ChangeDto, WsMessage } from "../gen";
import {
  ApiClient,
  type FetchLike,
  type WebSocketLike,
  type WebSocketLikeCtor,
} from "./client";

/** One recorded `fetch` call. */
interface Call {
  url: string;
  init: RequestInit | undefined;
}

/** A fetch that answers the queued responses in order and records the calls. */
function scripted(responses: Response[]): { fetch: FetchLike; calls: Call[] } {
  const calls: Call[] = [];
  const fetch: FetchLike = (url, init) => {
    calls.push({ url, init });
    const response = responses.shift();
    if (!response) {
      return Promise.reject(new Error("no response queued"));
    }
    return Promise.resolve(response);
  };
  return { fetch, calls };
}

/** A JSON answer with the service's content type. */
function json(status: number, body?: unknown, headers: Record<string, string> = {}): Response {
  return new Response(body === undefined ? null : JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json", ...headers },
  });
}

/** The headers a call carried, as a plain object. */
function headersOf(call: Call): Record<string, string> {
  return (call.init?.headers ?? {}) as Record<string, string>;
}

describe("requests", () => {
  it("reads with same-origin credentials and no write header", async () => {
    const { fetch, calls } = scripted([
      json(200, {
        seq: 1,
        run: null,
        tasks: [],
        agents: [],
        needs: [],
        asks: [],
        finished: false,
      }),
    ]);
    const client = new ApiClient({ fetch });

    await client.state();

    const call = calls[0];
    expect(call?.url).toBe("/api/state");
    expect(call?.init?.method).toBe("GET");
    expect(call?.init?.credentials).toBe("same-origin");
    expect(headersOf(call!).accept).toBe("application/json");
    expect(headersOf(call!)["x-shprd"]).toBeUndefined();
    expect(call?.init?.body).toBeUndefined();
  });

  it("writes carry x-shprd and a JSON body", async () => {
    const { fetch, calls } = scripted([json(200, { ok: true, seq: 4 })]);
    const client = new ApiClient({ fetch });

    await client.action({ kind: "task.start", task: "T1" });

    const call = calls[0];
    expect(call?.url).toBe("/api/action");
    expect(call?.init?.method).toBe("POST");
    expect(call?.init?.credentials).toBe("same-origin");
    expect(headersOf(call!)["x-shprd"]).toBe("1");
    expect(headersOf(call!)["content-type"]).toBe("application/json");
    expect(JSON.parse(String(call?.init?.body))).toEqual({ kind: "task.start", task: "T1" });
  });

  it("a PUT is a write too, so it carries x-shprd", async () => {
    const { fetch, calls } = scripted([
      json(200, { outcome: "stored", settings: { values: [], problems: [] } }),
    ]);
    const client = new ApiClient({ fetch });

    await client.putSetting({ key: "tasks.merge", value: "auto" });

    expect(calls[0]?.init?.method).toBe("PUT");
    expect(headersOf(calls[0]!)["x-shprd"]).toBe("1");
  });

  it("a change that times out is null, not an error", async () => {
    const { fetch } = scripted([new Response(null, { status: 204 })]);
    const client = new ApiClient({ fetch });

    await expect(client.changes(7)).resolves.toBeNull();
  });

  it("a change carries the wait it was asked for in seconds", async () => {
    const change: ChangeDto = { seq: 9, events: [] };
    const { fetch, calls } = scripted([json(200, change)]);
    const client = new ApiClient({ fetch });

    await client.changes(7, 2_500);

    expect(calls[0]?.url).toBe("/api/changes?since=7&timeout=3");
  });
});

describe("refusals", () => {
  it("a 401 shows the login screen", async () => {
    const { fetch } = scripted([
      json(401, { code: "unauthorized", message: "no session" }),
    ]);
    const client = new ApiClient({ fetch });
    const signedOut = vi.fn();
    client.onUnauthorized(signedOut);

    await expect(client.state()).rejects.toMatchObject({ status: 401, code: "unauthorized" });
    expect(signedOut).toHaveBeenCalledOnce();
  });

  it("a wrong password does not loop the login screen", async () => {
    const { fetch } = scripted([
      json(401, { code: "unauthorized", message: "that password did not work" }),
    ]);
    const client = new ApiClient({ fetch });
    const signedOut = vi.fn();
    client.onUnauthorized(signedOut);

    await expect(client.login("wrong", false)).rejects.toMatchObject({
      status: 401,
      message: "that password did not work",
    });
    expect(signedOut).not.toHaveBeenCalled();
  });

  it("reauth asks for the password again, not for a new session", async () => {
    const { fetch } = scripted([
      json(403, { code: "reauth", message: "enter your password again" }),
    ]);
    const client = new ApiClient({ fetch });
    const signedOut = vi.fn();
    const reauth = vi.fn();
    client.onUnauthorized(signedOut);
    client.onReauth(reauth);

    await expect(client.action({ kind: "task.accept", task: "T1" })).rejects.toMatchObject({
      code: "reauth",
    });
    expect(reauth).toHaveBeenCalledOnce();
    expect(signedOut).not.toHaveBeenCalled();
  });

  it("a rate limit carries its Retry-After", async () => {
    const { fetch } = scripted([
      json(429, { code: "refused", message: "too many attempts" }, { "retry-after": "90" }),
    ]);
    const client = new ApiClient({ fetch });

    await expect(client.login("guess", true)).rejects.toMatchObject({
      status: 429,
      retryAfter: 90,
    });
  });

  it("an unreachable service is a typed network error", async () => {
    const fetch: FetchLike = () => Promise.reject(new Error("connection refused"));
    const client = new ApiClient({ fetch });

    await expect(client.state()).rejects.toMatchObject({ status: 0, code: "network" });
  });

  it("a session is null when the service has no login route", async () => {
    const { fetch } = scripted([json(404, { code: "not_found", message: "no such route" })]);
    const client = new ApiClient({ fetch });

    await expect(client.session()).resolves.toBeNull();
  });
});

/** A WebSocket the test drives by hand. */
class FakeSocket implements WebSocketLike {
  static last: FakeSocket | undefined;
  readonly url: string;
  readonly sent: string[] = [];
  closed = false;
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;

  constructor(url: string) {
    this.url = url;
    FakeSocket.last = this;
  }

  send(data: string) {
    this.sent.push(data);
  }

  close() {
    this.closed = true;
    this.onclose?.();
  }

  /** Delivers one frame, exactly as the browser would. */
  receive(frame: unknown) {
    this.onmessage?.({ data: typeof frame === "string" ? frame : JSON.stringify(frame) });
  }
}

/** The fake's constructor, typed the way the client takes it. */
const FakeCtor = FakeSocket as unknown as WebSocketLikeCtor;

describe("the socket", () => {
  it("ignores message types this build does not know", () => {
    const client = new ApiClient({ base: "http://127.0.0.1:7787", WebSocket: FakeCtor });
    const hello = vi.fn();
    const change = vi.fn();
    const socket = client.connect({ hello, change });
    const fake = FakeSocket.last;
    expect(fake?.url).toBe("ws://127.0.0.1:7787/api/ws");

    // A newer service: a kind from after this build.
    expect(() => fake?.receive({ type: "needs.updated", needs: [] })).not.toThrow();
    // Not JSON at all, and JSON that is not an object.
    expect(() => fake?.receive("not json")).not.toThrow();
    expect(() => fake?.receive("[1,2,3]")).not.toThrow();
    expect(() => fake?.receive({ no: "type" })).not.toThrow();

    expect(hello).not.toHaveBeenCalled();
    expect(change).not.toHaveBeenCalled();

    // The kinds it does know still arrive.
    fake?.receive({ type: "hello", id: "abc", version: "0.0.0", seq: 3 });
    expect(hello).toHaveBeenCalledOnce();
    fake?.receive({ type: "change", change: { seq: 4, events: [] } });
    expect(change).toHaveBeenCalledOnce();

    socket.close();
    expect(fake?.closed).toBe(true);
  });

  it("sends pings as JSON", () => {
    const client = new ApiClient({ base: "http://127.0.0.1:7787", WebSocket: FakeCtor });
    const socket = client.connect({});

    socket.send({ type: "ping", at: 12 });

    expect(FakeSocket.last?.sent).toEqual([JSON.stringify({ type: "ping", at: 12 })]);
  });

  it("keeps an unknown message from reaching a handler", () => {
    const client = new ApiClient({ base: "http://127.0.0.1:7787", WebSocket: FakeCtor });
    const messages: WsMessage["type"][] = [];
    const socket = client.connect({
      hello: (message) => messages.push(message.type),
      change: () => messages.push("change"),
    });

    for (const type of ["pong", "future.kind", "another.one"]) {
      FakeSocket.last?.receive({ type });
    }

    expect(messages).toEqual([]);
    socket.close();
  });
});
