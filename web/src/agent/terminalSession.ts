// The terminal's connection: one socket to a pane, reconnected when it drops.
//
// Rules this keeps (M7d §4, M11.6):
//
// - watch by default: nothing is sent until the person types, and the first key
//   opens input (`input_open`) on that socket;
// - input is **never replayed**: a key pressed while there is no socket is
//   dropped with a notice, and after a reconnect the page says the last input
//   may not have arrived instead of re-sending anything;
// - a paste is capped, and a pasted newline is a space, so a paste can never
//   run a line the person did not press Enter on;
// - nothing the person typed is kept, logged or echoed back.

import type { TerminalClientMsg, TerminalServerMsg } from "../gen";
import type { TerminalHandlers, TerminalSocket } from "../api/client";

/** The most characters one paste may send; the rest is cut. */
export const PASTE_MAX = 4096;

/** What the page tells a person after a reconnect that may have lost a key. */
export const LOST_INPUT = "Reconnected. The last input may not have arrived.";

/** The longest the page waits to reconnect, ms. */
export const MAX_BACKOFF_MS = 15_000;

/** Where the connection is, for the status line. */
export type TerminalStatus = "connecting" | "watching" | "typing" | "reconnecting" | "refused";

/** What a chunk of keyboard data came to. */
export interface Typed {
  messages: TerminalClientMsg[];
  /** True when a paste was longer than [`PASTE_MAX`] and its end was dropped. */
  cut: boolean;
}

/** The escape sequences a terminal keyboard sends, and the key each one is. */
const SEQUENCES: Array<[string, string]> = [
  ["\x1b[A", "up"], ["\x1bOA", "up"],
  ["\x1b[B", "down"], ["\x1bOB", "down"],
  ["\x1b[C", "right"], ["\x1bOC", "right"],
  ["\x1b[D", "left"], ["\x1bOD", "left"],
  ["\x1b[5~", "pageup"], ["\x1b[6~", "pagedown"],
  ["\x1b[3~", "delete"],
  ["\x1b[H", "home"], ["\x1bOH", "home"], ["\x1b[1~", "home"],
  ["\x1b[F", "end"], ["\x1bOF", "end"], ["\x1b[4~", "end"],
];

/** The single bytes that are keys. */
const SINGLES: Record<string, string> = {
  "\r": "enter",
  "\t": "tab",
  "\x7f": "backspace",
  "\b": "backspace",
  "\x03": "ctrl+c",
  "\x1b": "esc",
};

/**
 * Turns what xterm hands over (`onData`) into the socket's messages.
 *
 * A chunk longer than one key is text, a paste included: it is capped at
 * [`PASTE_MAX`], its newlines become spaces, and its other control characters are
 * dropped. Keys the service does not accept (Ctrl-D, F-keys) are dropped here
 * rather than sent to be refused.
 */
export function inputToMessages(data: string): Typed {
  let cut = false;
  let rest = data;
  if (rest.length > PASTE_MAX) {
    rest = rest.slice(0, PASTE_MAX);
    cut = true;
  }
  const messages: TerminalClientMsg[] = [];
  let text = "";
  const flush = () => {
    if (text !== "") {
      messages.push({ type: "input", data: text });
      text = "";
    }
  };
  const multi = rest.length > 1 && !SEQUENCES.some(([sequence]) => sequence === rest);
  let i = 0;
  while (i < rest.length) {
    const sequence = SEQUENCES.find(([seq]) => rest.startsWith(seq, i));
    if (sequence && !multi) {
      flush();
      messages.push({ type: "key", key: sequence[1] });
      i += sequence[0].length;
      continue;
    }
    const char = rest[i] ?? "";
    const single = SINGLES[char];
    if (single && !multi) {
      flush();
      messages.push({ type: "key", key: single });
    } else if (char === "\r" || char === "\n") {
      text += " ";
    } else if (char >= " " && char !== "\x7f") {
      text += char;
    }
    i += 1;
  }
  flush();
  return { messages, cut };
}

/** `text` without any OSC 52 (clipboard) sequence: a pane may not write the person's clipboard. */
export function stripOsc52(text: string): string {
  // eslint-disable-next-line no-control-regex
  return text.replace(/\x1b\]52;[^\x07\x1b]*(?:\x07|\x1b\\)?/g, "");
}

/** Whether a link a terminal holds may be opened: web pages only. */
export function safeTerminalLink(url: string): boolean {
  return /^https?:\/\/[^\s]+$/i.test(url.trim());
}

/** What a session reports. */
export interface SessionEvents {
  /** The screen, whole. */
  screen: (text: string, cut: boolean) => void;
  /** The connection's state changed. */
  status: (status: TerminalStatus) => void;
  /** One line for the person (a refusal, a lost key, a paste that was cut). */
  notice: (message: string) => void;
}

/** How a session opens a socket: `ApiClient.terminal`, bound to a space. */
export type Open = (handlers: TerminalHandlers) => TerminalSocket;

/** One page's watch of one pane. */
export class TerminalSession {
  readonly #open: Open;
  readonly #events: SessionEvents;
  #socket: TerminalSocket | null = null;
  #live = false;
  #inputOpen = false;
  #sentInput = false;
  #lostInput = false;
  #failures = 0;
  #timer: ReturnType<typeof setTimeout> | null = null;
  #disposed = false;

  /** Builds a session; nothing connects until [`start`](TerminalSession.start). */
  constructor(open: Open, events: SessionEvents) {
    this.#open = open;
    this.#events = events;
  }

  /** Connects. */
  start(): void {
    this.#connect();
  }

  /** Types what the keyboard sent; dropped (with a notice) when not connected. */
  type(data: string): void {
    const { messages, cut } = inputToMessages(data);
    if (cut) {
      this.#events.notice(`The paste was cut to ${PASTE_MAX.toLocaleString("en-US")} characters.`);
    }
    if (messages.length === 0) {
      return;
    }
    const socket = this.#socket;
    if (!socket || !this.#live) {
      this.#events.notice("Not connected: nothing was typed.");
      return;
    }
    if (!this.#inputOpen) {
      this.#inputOpen = true;
      socket.send({ type: "input_open" });
      this.#events.status("typing");
    }
    this.#sentInput = true;
    for (const message of messages) {
      socket.send(message);
    }
  }

  /** Closes for good. */
  dispose(): void {
    this.#disposed = true;
    if (this.#timer !== null) {
      clearTimeout(this.#timer);
      this.#timer = null;
    }
    this.#socket?.close();
    this.#socket = null;
  }

  #connect(): void {
    if (this.#disposed) {
      return;
    }
    this.#inputOpen = false;
    this.#events.status(this.#failures > 0 || this.#lostInput ? "reconnecting" : "connecting");
    let opened = false;
    this.#socket = this.#open({
      open: () => {
        opened = true;
      },
      message: (message) => {
        if (!this.#live) {
          this.#live = true;
          this.#failures = 0;
          this.#events.status("watching");
          if (this.#lostInput) {
            this.#lostInput = false;
            this.#events.notice(LOST_INPUT);
          }
        }
        this.#message(message);
      },
      close: (code) => {
        if (this.#disposed) {
          return;
        }
        const wasLive = this.#live;
        this.#live = false;
        this.#socket = null;
        if (this.#sentInput && wasLive) {
          this.#lostInput = true;
        }
        this.#sentInput = false;
        this.#failures = wasLive ? 0 : this.#failures + 1;
        // A handshake that never opened is a refusal (no pane, no host, not
        // signed in): three of them and the page stops asking.
        if (!opened && !wasLive && this.#failures >= 3) {
          this.#events.status("refused");
          return;
        }
        const base = Math.min(MAX_BACKOFF_MS, 1000 * 2 ** Math.min(this.#failures, 4));
        const wait = code === 1013 ? Math.max(base, 10_000) : base;
        this.#events.status("reconnecting");
        this.#timer = setTimeout(() => {
          this.#timer = null;
          this.#connect();
        }, wait);
      },
    });
  }

  #message(message: TerminalServerMsg): void {
    switch (message.type) {
      case "screen":
        this.#events.screen(stripOsc52(message.text), message.cut);
        break;
      case "notice":
        this.#events.notice(message.message);
        break;
      case "ended":
        this.#events.notice(`The pane ended: ${message.reason}`);
        break;
      default:
        break;
    }
  }
}
