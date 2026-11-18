import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { TerminalClientMsg, TerminalServerMsg } from "../gen";
import type { TerminalHandlers, TerminalSocket } from "../api/client";
import {
  LOST_INPUT,
  PASTE_MAX,
  TerminalSession,
  inputToMessages,
  safeTerminalLink,
  stripOsc52,
  type TerminalStatus,
} from "./terminalSession";

describe("inputToMessages", () => {
  it("maps keys to the words the service accepts", () => {
    const keys = (data: string) => inputToMessages(data).messages;
    expect(keys("\r")).toEqual([{ type: "key", key: "enter" }]);
    expect(keys("\x7f")).toEqual([{ type: "key", key: "backspace" }]);
    expect(keys("\x03")).toEqual([{ type: "key", key: "ctrl+c" }]);
    expect(keys("\x1b")).toEqual([{ type: "key", key: "esc" }]);
    expect(keys("\x1b[A")).toEqual([{ type: "key", key: "up" }]);
    expect(keys("\x1b[6~")).toEqual([{ type: "key", key: "pagedown" }]);
    expect(keys("\t")).toEqual([{ type: "key", key: "tab" }]);
  });

  it("sends typed text as text", () => {
    expect(inputToMessages("a").messages).toEqual([{ type: "input", data: "a" }]);
  });

  it("drops keys the service refuses instead of sending them", () => {
    expect(inputToMessages("\x04").messages).toEqual([]);
    expect(inputToMessages("\x1b[15~").messages.some((m) => m.type === "key")).toBe(false);
  });

  it("turns a pasted newline into a space, so a paste never presses Enter", () => {
    const { messages } = inputToMessages("rm -rf x\nls\r\npwd");
    expect(messages).toEqual([{ type: "input", data: "rm -rf x ls  pwd" }]);
  });

  it("caps a paste and says it did", () => {
    const { messages, cut } = inputToMessages("x".repeat(PASTE_MAX + 50));
    expect(cut).toBe(true);
    expect(messages).toHaveLength(1);
    const first = messages[0];
    expect(first?.type === "input" && first.data.length).toBe(PASTE_MAX);
  });
});

describe("what a pane may not do", () => {
  it("strips OSC 52 clipboard writes, with either terminator", () => {
    expect(stripOsc52("a\x1b]52;c;aGVsbG8=\x07b")).toBe("ab");
    expect(stripOsc52("a\x1b]52;c;aGVsbG8=\x1b\\b")).toBe("ab");
    expect(stripOsc52("plain\x1b[31mred")).toBe("plain\x1b[31mred");
  });

  it("opens web links only", () => {
    expect(safeTerminalLink("https://example.com")).toBe(true);
    expect(safeTerminalLink("javascript:alert(1)")).toBe(false);
    expect(safeTerminalLink("ssh://host")).toBe(false);
  });
});

/** A socket the test drives by hand. */
interface Fake {
  handlers: TerminalHandlers;
  sent: TerminalClientMsg[];
  closed: boolean;
}

function harness() {
  const sockets: Fake[] = [];
  const notices: string[] = [];
  const statuses: TerminalStatus[] = [];
  const screens: string[] = [];
  const session = new TerminalSession(
    (handlers) => {
      const fake: Fake = { handlers, sent: [], closed: false };
      sockets.push(fake);
      const socket: TerminalSocket = {
        send: (message) => fake.sent.push(message),
        close: () => {
          fake.closed = true;
        },
      };
      return socket;
    },
    {
      screen: (text) => screens.push(text),
      status: (status) => statuses.push(status),
      notice: (message) => notices.push(message),
    },
  );
  const hello = (fake: Fake) => {
    fake.handlers.open?.();
    fake.handlers.message?.({ type: "hello", space: "coordinator", pane: "%1" } as TerminalServerMsg);
  };
  return { session, sockets, notices, statuses, screens, hello };
}

describe("TerminalSession", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("only watches until the first key, which opens input once", () => {
    const h = harness();
    h.session.start();
    const socket = h.sockets[0]!;
    h.hello(socket);
    expect(socket.sent).toEqual([]);
    expect(h.statuses.at(-1)).toBe("watching");
    h.session.type("l");
    h.session.type("s");
    expect(socket.sent).toEqual([
      { type: "input_open" },
      { type: "input", data: "l" },
      { type: "input", data: "s" },
    ]);
    expect(h.statuses.at(-1)).toBe("typing");
  });

  it("draws screens without the clipboard sequence", () => {
    const h = harness();
    h.session.start();
    const socket = h.sockets[0]!;
    h.hello(socket);
    socket.handlers.message?.({ type: "screen", text: "hi\x1b]52;c;eA==\x07", seq: 1, cut: false });
    expect(h.screens).toEqual(["hi"]);
  });

  it("drops a key typed with no socket, and says so", () => {
    const h = harness();
    h.session.start();
    h.session.type("x");
    expect(h.sockets[0]!.sent).toEqual([]);
    expect(h.notices).toEqual(["Not connected: nothing was typed."]);
  });

  it("reconnects, says the last input may not have arrived, and never replays it", () => {
    const h = harness();
    h.session.start();
    const first = h.sockets[0]!;
    h.hello(first);
    h.session.type("secret");
    first.handlers.close?.(1006);
    vi.advanceTimersByTime(2000);
    const second = h.sockets[1]!;
    expect(second).toBeDefined();
    h.hello(second);
    expect(h.notices).toContain(LOST_INPUT);
    expect(second.sent).toEqual([]);
    // Input must be opened again on the new socket, by a new key.
    h.session.type("a");
    expect(second.sent[0]).toEqual({ type: "input_open" });
  });

  it("says nothing about lost input when nothing was typed", () => {
    const h = harness();
    h.session.start();
    h.hello(h.sockets[0]!);
    h.sockets[0]!.handlers.close?.(1001);
    vi.advanceTimersByTime(2000);
    h.hello(h.sockets[1]!);
    expect(h.notices).toEqual([]);
  });

  it("waits longer after a 1013 and gives up after three refused handshakes", () => {
    const h = harness();
    h.session.start();
    h.hello(h.sockets[0]!);
    h.sockets[0]!.handlers.close?.(1013);
    vi.advanceTimersByTime(5000);
    expect(h.sockets).toHaveLength(1);
    vi.advanceTimersByTime(6000);
    expect(h.sockets).toHaveLength(2);
    for (let i = 0; i < 3; i += 1) {
      h.sockets.at(-1)!.handlers.close?.(1006);
      vi.advanceTimersByTime(20_000);
    }
    expect(h.statuses.at(-1)).toBe("refused");
    const count = h.sockets.length;
    vi.advanceTimersByTime(60_000);
    expect(h.sockets).toHaveLength(count);
  });

  it("stops for good when disposed", () => {
    const h = harness();
    h.session.start();
    h.hello(h.sockets[0]!);
    h.session.dispose();
    expect(h.sockets[0]!.closed).toBe(true);
    h.sockets[0]!.handlers.close?.(1006);
    vi.advanceTimersByTime(60_000);
    expect(h.sockets).toHaveLength(1);
  });

  it("tells the person when a paste was cut", () => {
    const h = harness();
    h.session.start();
    h.hello(h.sockets[0]!);
    h.session.type("y".repeat(PASTE_MAX + 1));
    expect(h.notices.some((n) => n.includes("cut"))).toBe(true);
  });
});
