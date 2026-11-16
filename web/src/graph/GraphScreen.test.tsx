// The screen's states, and the path from a tap to the task: every answer the
// service can give has a screen, and none of them is a blank page.

// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { ApiClient, type FetchLike, type WebSocketLike, type WebSocketLikeCtor } from "../api/client";
import { GraphScreen } from "./GraphScreen";

/** A socket that opens and then says nothing, so the feed stays on it. */
class QuietSocket implements WebSocketLike {
  static last: QuietSocket | null = null;
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;

  constructor() {
    QuietSocket.last = this;
  }

  send(): void {
    // Nothing a screen does needs the socket's writes.
  }

  close(): void {
    // The feed closes it on unmount; nothing to release here.
  }
}

/** A JSON answer with the service's content type. */
function json(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

/** A document with one run and the tasks it is given. */
function runDocument(tasks: unknown[]): Record<string, unknown> {
  return {
    dagr: 3,
    run: { id: "r1", title: "shprd · r1", started_at: "2026-11-16T10:00:00Z" },
    generated_at: "2026-11-16T10:05:00Z",
    projects: [],
    tasks,
    events: [],
  };
}

/** A task the graph draws. */
function task(id: string, deps: string[], state = "queued"): Record<string, unknown> {
  return {
    id,
    title: `task ${id}`,
    kind: "impl",
    state,
    deps,
    criteria: "it works",
    attempts: [],
  };
}

/** A client whose answers the test scripts. */
function client(answers: (url: string) => Response | Promise<Response>): {
  api: ApiClient;
  calls: string[];
} {
  const calls: string[] = [];
  const fetch: FetchLike = (url) => {
    calls.push(url);
    return Promise.resolve(answers(url));
  };
  return {
    api: new ApiClient({ fetch, WebSocket: QuietSocket as unknown as WebSocketLikeCtor }),
    calls,
  };
}

/** A letter from the service: a refusal, as `ApiError` reads it. */
function refusal(status: number, code: string, message: string): Response {
  return json(status, { code, message });
}

afterEach(() => {
  cleanup();
  QuietSocket.last = null;
});

describe("GraphScreen", () => {
  it("says it is reading while the answer is in flight", () => {
    const { api } = client(() => new Promise<Response>(() => undefined));
    render(<GraphScreen client={api} />);
    expect(screen.getByRole("heading", { name: /Reading the run/ })).toBeDefined();
  });

  it("says how to begin a run when there is none", async () => {
    const { api } = client((url) =>
      url === "/api/graph/run"
        ? refusal(404, "not_found", "no run document yet")
        : json(200, {
            repo: "/repo",
            run_json: "/repo/.dagr/run.json",
            run: null,
            argv: [],
            setting: "graph.dagr",
          }),
    );
    render(<GraphScreen client={api} />);
    expect(
      await screen.findByRole("heading", { name: "No run to draw yet" }),
    ).toBeDefined();
    expect(screen.getByText(/shprd run start/)).toBeDefined();
  });

  it("names the document a started run has not written yet", async () => {
    const { api } = client((url) =>
      url === "/api/graph/run"
        ? refusal(404, "not_found", "no run document yet")
        : json(200, {
            repo: "/repo",
            run_json: "/repo/.dagr/run.json",
            run: "r1",
            argv: [],
            setting: "graph.dagr",
          }),
    );
    render(<GraphScreen client={api} />);
    const note = await screen.findByText(/has not been written yet/);
    expect(note.textContent).toContain("/repo/.dagr/run.json");
  });

  it("offers a retry when the service refuses the document", async () => {
    const { api, calls } = client(() =>
      refusal(500, "internal", "/repo/.dagr/run.json is not a dagr run document: it has no `run`"),
    );
    render(<GraphScreen client={api} />);
    expect(
      await screen.findByRole("heading", { name: "The graph could not be read" }),
    ).toBeDefined();
    expect(screen.getByText(/is not a dagr run document/)).toBeDefined();

    const before = calls.length;
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    await waitFor(() => expect(calls.length).toBeGreaterThan(before));
  });

  it("draws the run the document states", async () => {
    const { api } = client(() =>
      json(200, runDocument([task("A", []), task("B", ["A"], "working")])),
    );
    render(<GraphScreen client={api} />);

    expect(await screen.findByRole("heading", { name: "shprd · r1" })).toBeDefined();
    expect(screen.getByText(/2 tasks · 1 dependency/)).toBeDefined();
    expect(screen.getByRole("button", { name: "A: task A. Queued." })).toBeDefined();
    expect(screen.getByRole("button", { name: "B: task B. Working." })).toBeDefined();
  });

  it("opens a task's detail from its node, and closes it again", async () => {
    const { api } = client(() =>
      json(
        200,
        runDocument([
          task("A", []),
          {
            ...task("B", ["A"], "review"),
            owner: "pi",
            attempts: [
              {
                id: "B·a1",
                n: 1,
                cause: { type: "initial" },
                actor: "pi",
                state: "done",
                started_at: "2026-11-16T10:00:00Z",
                ended_at: "2026-11-16T10:01:00Z",
                outcome: { result: "done", evidence: "verified" },
              },
            ],
          },
        ]),
      ),
    );
    render(<GraphScreen client={api} />);

    fireEvent.click(await screen.findByRole("button", { name: "B: task B. Review." }));
    const detail = await screen.findByRole("dialog", { name: "task B" });
    expect(detail.textContent).toContain("Attempts");
    expect(detail.textContent).toContain("◆");
    expect(detail.textContent).toContain("verified");

    fireEvent.keyDown(document, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });

  it("follows a change notice and redraws", async () => {
    let tasks = [task("A", [])];
    const { api } = client(() => json(200, runDocument(tasks)));
    render(<GraphScreen client={api} />);
    expect(await screen.findByRole("button", { name: "A: task A. Queued." })).toBeDefined();

    tasks = [task("A", []), task("B", ["A"])];
    QuietSocket.last?.onmessage?.({
      data: JSON.stringify({ type: "change", change: { seq: 4, events: [] } }),
    });

    // The screen waits out the post-write step before reading again.
    expect(await screen.findByRole("button", { name: "B: task B. Queued." })).toBeDefined();
  });

  it("draws a document whose tasks all depend on nothing", async () => {
    const { api } = client(() => json(200, runDocument([])));
    render(<GraphScreen client={api} />);
    expect(await screen.findByText(/0 tasks · 0 dependencies/)).toBeDefined();
    expect(screen.getByRole("group", { name: "The run's graph: 0 tasks" })).toBeDefined();
  });

  it("keeps the drawing when a refresh fails, and says so", async () => {
    let first = true;
    const { api } = client(() => {
      if (first) {
        first = false;
        return json(200, runDocument([task("A", [])]));
      }
      return refusal(500, "internal", "the log could not be read");
    });
    render(<GraphScreen client={api} />);
    expect(await screen.findByRole("button", { name: "A: task A. Queued." })).toBeDefined();

    QuietSocket.last?.onmessage?.({
      data: JSON.stringify({ type: "change", change: { seq: 5, events: [] } }),
    });

    expect(await screen.findByText(/the log could not be read/)).toBeDefined();
    expect(screen.getByRole("button", { name: "A: task A. Queued." })).toBeDefined();
  });
});
