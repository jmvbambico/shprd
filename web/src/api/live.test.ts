// The live feed's two arms and its failure paths. A screen that stops following
// the run looks exactly like a run that stopped moving, so each one is pinned.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { ChangeDto } from "../gen";

import { ApiError, type Socket, type SocketHandlers } from "./client";
import { LiveFeed, type LiveClient } from "./live";

/** One socket the fake client handed out. */
interface FakeSocket {
  handlers: SocketHandlers;
  socket: Socket;
  closed: boolean;
}

/** A client whose sockets and polls the test drives by hand. */
function fakeClient(): {
  client: LiveClient;
  sockets: FakeSocket[];
  polls: { since: number; waitMs: number | undefined; resolve: (change: ChangeDto | null) => void; reject: (error: unknown) => void }[];
} {
  const sockets: FakeSocket[] = [];
  const polls: ReturnType<typeof fakeClient>["polls"] = [];
  const client: LiveClient = {
    connect: (handlers) => {
      const entry: FakeSocket = {
        handlers,
        socket: { send: () => undefined, close: () => undefined },
        closed: false,
      };
      sockets.push(entry);
      return entry.socket;
    },
    changes: (since, waitMs) =>
      new Promise<ChangeDto | null>((resolve, reject) => {
        polls.push({ since, waitMs, resolve, reject });
      }),
  };
  return { client, sockets, polls };
}

/** A change notice. */
function change(seq: number): ChangeDto {
  return { seq, events: [] };
}

describe("LiveFeed", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("refreshes on a change pushed over the socket", () => {
    const { client, sockets } = fakeClient();
    const refresh = vi.fn();
    const feed = new LiveFeed(client, refresh);
    feed.start();
    expect(sockets).toHaveLength(1);

    sockets[0]?.handlers.open?.();
    sockets[0]?.handlers.change?.(change(7));

    expect(refresh).toHaveBeenCalledTimes(1);
    expect(feed.since).toBe(7);
  });

  it("reconnects a socket that opened and then closed", async () => {
    const { client, sockets } = fakeClient();
    const feed = new LiveFeed(client, vi.fn(), { retryMs: 50 });
    feed.start();
    sockets[0]?.handlers.open?.();
    sockets[0]?.handlers.close?.();

    await vi.advanceTimersByTimeAsync(50);
    expect(sockets).toHaveLength(2);
    expect(feed.running).toBe(true);
  });

  it("falls back to the long poll when the socket never opens", async () => {
    const { client, sockets, polls } = fakeClient();
    const refresh = vi.fn();
    const feed = new LiveFeed(client, refresh, { retryMs: 50, socketTries: 2 });
    feed.start();

    // Two sockets that close without ever opening: this browser has no socket
    // worth waiting for, so the poll takes over.
    sockets[0]?.handlers.close?.();
    await vi.advanceTimersByTimeAsync(50);
    sockets[1]?.handlers.close?.();
    await vi.advanceTimersByTimeAsync(50);

    expect(polls).toHaveLength(1);
    expect(polls[0]?.since).toBe(0);

    polls[0]?.resolve(change(9));
    await vi.advanceTimersByTimeAsync(0);
    expect(refresh).toHaveBeenCalledTimes(1);
    expect(feed.since).toBe(9);
    expect(polls[1]?.since).toBe(9);
  });

  it("keeps polling after a timeout, which changed nothing", async () => {
    const { client, sockets, polls } = fakeClient();
    const refresh = vi.fn();
    const feed = new LiveFeed(client, refresh, { retryMs: 0, socketTries: 1 });
    feed.start();
    sockets[0]?.handlers.close?.();
    await vi.advanceTimersByTimeAsync(0);

    polls[0]?.resolve(null);
    await vi.advanceTimersByTimeAsync(0);
    expect(refresh).not.toHaveBeenCalled();
    expect(polls).toHaveLength(2);
    expect(polls[1]?.since).toBe(0);
  });

  it("ends a poll that was refused as signed out", async () => {
    const { client, sockets, polls } = fakeClient();
    const feed = new LiveFeed(client, vi.fn(), { retryMs: 0, socketTries: 1 });
    feed.start();
    sockets[0]?.handlers.close?.();
    await vi.advanceTimersByTimeAsync(0);
    expect(polls).toHaveLength(1);

    polls[0]?.reject(new ApiError(401, "unauthorized", "no session"));
    await vi.advanceTimersByTimeAsync(60_000);
    expect(polls).toHaveLength(1);
  });

  it("retries a poll that failed for another reason", async () => {
    const { client, sockets, polls } = fakeClient();
    const feed = new LiveFeed(client, vi.fn(), { retryMs: 100, socketTries: 1 });
    feed.start();
    sockets[0]?.handlers.close?.();
    await vi.advanceTimersByTimeAsync(0);

    polls[0]?.reject(new ApiError(0, "network", "connection refused"));
    await vi.advanceTimersByTimeAsync(100);
    expect(polls).toHaveLength(2);
  });

  it("closes the socket and stops reconnecting when stopped", async () => {
    const { client, sockets } = fakeClient();
    const feed = new LiveFeed(client, vi.fn(), { retryMs: 50 });
    feed.start();
    sockets[0]?.handlers.open?.();
    feed.stop();

    sockets[0]?.handlers.close?.();
    await vi.advanceTimersByTimeAsync(500);
    expect(sockets).toHaveLength(1);
    expect(feed.running).toBe(false);
  });

  it("falls back to the poll when the browser has no socket at all", async () => {
    const polls: { since: number }[] = [];
    const client: LiveClient = {
      connect: () => {
        throw new ApiError(0, "unsupported", "this browser has no WebSocket");
      },
      changes: (since) => {
        polls.push({ since });
        // Never answers, as the real long poll does until the service has
        // something to say: the feed is not spinning, it is waiting.
        return new Promise<ChangeDto | null>(() => undefined);
      },
    };
    const feed = new LiveFeed(client, vi.fn(), { retryMs: 0, socketTries: 1 });
    feed.start();
    await vi.advanceTimersByTimeAsync(0);
    expect(polls).toEqual([{ since: 0 }]);
  });
});
