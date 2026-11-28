// A screen's live arm: the socket, and the long poll behind it.
//
// Every screen that shows the run has to redraw when the run changes. `/api/ws`
// is the first answer — one socket, pushed change notices, reconnected when the
// service caps a session — and `/api/changes` (the same notices, long-polled) is
// the one behind it: a browser without WebSockets, or one whose socket never
// opened, still follows the run, only a little later. The Herdr mods have no
// choice (a mod has no socket), which is why the service answers both.
//
// What is fed to the screen is one signal — *something changed* — and never the
// events themselves: a screen reads what it draws through the client, so a
// notice it missed changes nothing but the wait for the next one.
//
// It is a plain class rather than a React hook so the failure paths (a socket
// that never opens, a closed socket, a signed-out poll) are tested without a
// DOM; `useLiveRefresh` is the three-line hook over it.

import type { ChangeDto } from "../gen";

import { ApiError, type Socket, type SocketHandlers } from "./client";

/** What a feed needs from the client: the two ways to hear about a change. */
export interface LiveClient {
  /** Opens the change socket. */
  connect(handlers: SocketHandlers): Socket;
  /** Long-polls for changes after `since`. */
  changes(since: number, waitMs?: number): Promise<ChangeDto | null>;
}

/** How a feed behaves; every field has a default. */
export interface LiveOptions {
  /** How long one change long-poll waits, ms. */
  waitMs?: number;
  /** How long to wait before another socket try or poll, ms. */
  retryMs?: number;
  /** How many socket tries may fail before the long poll takes over. */
  socketTries?: number;
}

/** The defaults: the service's own long-poll wait, and a short retry. */
export const LIVE_DEFAULTS = { waitMs: 10_000, retryMs: 2_000, socketTries: 2 } as const;

/**
 * Follows one service's changes and calls `refresh` after each one.
 *
 * One instance per screen, started when it mounts and stopped when it unmounts:
 * a socket is a connection, and a screen that left one behind would keep a phone
 * awake.
 */
export class LiveFeed {
  readonly #client: LiveClient;
  readonly #refresh: (change: ChangeDto) => void;
  readonly #waitMs: number;
  readonly #retryMs: number;
  readonly #socketTries: number;
  #socket: Socket | null = null;
  #timer: ReturnType<typeof setTimeout> | null = null;
  #sleepers: (() => void)[] = [];
  #since = 0;
  #tries = 0;
  #running = false;

  /** Builds a feed; nothing runs until [`LiveFeed::start`]. */
  constructor(client: LiveClient, refresh: (change: ChangeDto) => void, options: LiveOptions = {}) {
    this.#client = client;
    this.#refresh = refresh;
    this.#waitMs = options.waitMs ?? LIVE_DEFAULTS.waitMs;
    this.#retryMs = options.retryMs ?? LIVE_DEFAULTS.retryMs;
    this.#socketTries = options.socketTries ?? LIVE_DEFAULTS.socketTries;
  }

  /** Starts following: one socket, or the long poll when it cannot open. */
  start(): void {
    if (this.#running) {
      return;
    }
    this.#running = true;
    this.#tries = 0;
    this.#listen();
  }

  /** Stops following: closes the socket, ends a poll and clears any retry. */
  stop(): void {
    this.#running = false;
    if (this.#timer !== null) {
      clearTimeout(this.#timer);
      this.#timer = null;
    }
    this.#socket?.close();
    this.#socket = null;
    const sleepers = this.#sleepers;
    this.#sleepers = [];
    for (const wake of sleepers) {
      wake();
    }
  }

  /** Whether the feed is running, for a caller that has to know. */
  get running(): boolean {
    return this.#running;
  }

  /** The last sequence a change named; a poll asks from here. */
  get since(): number {
    return this.#since;
  }

  /** Opens one socket and decides what its close means. */
  #listen(): void {
    if (!this.#running) {
      return;
    }
    let opened = false;
    // `onclose` and `onerror` both arrive as a close, and a browser may send
    // both: the first one is the socket's end, the second is noise.
    let settled = false;
    const closed = () => {
      if (settled) {
        return;
      }
      settled = true;
      this.#closed(opened);
    };
    try {
      this.#socket = this.#client.connect({
        open: () => {
          opened = true;
          this.#tries = 0;
        },
        change: (change) => {
          this.#since = change.seq;
          this.#refresh(change);
        },
        close: closed,
      });
    } catch {
      // No `WebSocket` in this browser at all: the long poll is the answer, and
      // there is nothing to wait for first.
      closed();
    }
  }

  /** What a closed (or never opened) socket means: another try, or the poll. */
  #closed(opened: boolean): void {
    this.#socket = null;
    if (!this.#running) {
      return;
    }
    // A socket that opened and later closed is the service's session cap, which
    // is normal: reconnect. A socket that never opens is this browser or this
    // network, and after a couple of tries the long poll is the honest answer.
    if (!opened) {
      this.#tries += 1;
      if (this.#tries >= this.#socketTries) {
        void this.#poll();
        return;
      }
    }
    this.#after(this.#retryMs, () => this.#listen());
  }

  /** Long-polls until the feed stops; a signed-out answer ends it. */
  async #poll(): Promise<void> {
    while (this.#running) {
      try {
        const change = await this.#client.changes(this.#since, this.#waitMs);
        if (!this.#running) {
          return;
        }
        if (change !== null) {
          this.#since = change.seq;
          this.#refresh(change);
        }
      } catch (error) {
        // The session is gone: the app's own listener shows the login screen,
        // and a poll that kept asking would be a request per retry from a page
        // that is no longer signed in.
        if (error instanceof ApiError && error.status === 401) {
          return;
        }
        await this.#sleep(this.#retryMs);
      }
    }
  }

  /** Runs `work` after `ms`, unless the feed stops first. */
  #after(ms: number, work: () => void): void {
    if (this.#timer !== null) {
      clearTimeout(this.#timer);
    }
    this.#timer = setTimeout(() => {
      this.#timer = null;
      if (this.#running) {
        work();
      }
    }, ms);
  }

  /** Waits `ms`, or until [`LiveFeed::stop`] wakes it. */
  #sleep(ms: number): Promise<void> {
    return new Promise((resolve) => {
      let timer: ReturnType<typeof setTimeout> | null = null;
      const wakeUp = () => {
        if (timer !== null) {
          clearTimeout(timer);
        }
        resolve();
      };
      timer = setTimeout(() => {
        this.#sleepers = this.#sleepers.filter((wake) => wake !== wakeUp);
        resolve();
      }, ms);
      this.#sleepers.push(wakeUp);
    });
  }
}
