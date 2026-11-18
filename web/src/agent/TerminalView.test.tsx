// The terminal view's states, against a fake socket and a fake screen.

// @vitest-environment jsdom

import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { TerminalHandlers, TerminalSocket } from "../api/client";
import type { TerminalClientMsg, TerminalServerMsg } from "../gen";
import type { Screen } from "./screen";
import { STATUS_WORDS, TerminalView } from "./TerminalView";

// xterm.js probes a canvas when it loads; these tests draw through a fake screen.
vi.mock("@xterm/xterm", () => ({ Terminal: class {} }));

afterEach(cleanup);

function setup() {
  const sockets: Array<{ space: string; handlers: TerminalHandlers; sent: TerminalClientMsg[] }> = [];
  const shown: string[] = [];
  let onData: (data: string) => void = () => undefined;
  const client = {
    terminal: (space: string, handlers: TerminalHandlers): TerminalSocket => {
      const entry = { space, handlers, sent: [] as TerminalClientMsg[] };
      sockets.push(entry);
      return { send: (message) => entry.sent.push(message), close: () => undefined };
    },
  };
  const factory = (_host: HTMLElement, data: (value: string) => void): Screen => {
    onData = data;
    return { show: (text) => shown.push(text), focus: () => undefined, dispose: () => undefined };
  };
  const say = (message: TerminalServerMsg) => act(() => sockets.at(-1)?.handlers.message?.(message));
  return { sockets, shown, client, factory, say, type: (data: string) => act(() => onData(data)) };
}

describe("TerminalView", () => {
  it("waits for the screen, then draws it, and only watches", () => {
    const t = setup();
    render(<TerminalView client={t.client} space="coordinator" screenFactory={t.factory} />);
    expect(screen.getByText("Waiting for the agent's screen…")).toBeDefined();
    expect(screen.getByText(STATUS_WORDS.connecting)).toBeDefined();
    t.say({ type: "hello", space: "coordinator", pane: "%1" });
    t.say({ type: "screen", text: "$ ls", seq: 1, cut: false });
    expect(t.shown).toEqual(["$ ls"]);
    expect(screen.queryByText("Waiting for the agent's screen…")).toBeNull();
    expect(screen.getByText(STATUS_WORDS.watching)).toBeDefined();
    expect(t.sockets[0]?.sent).toEqual([]);
  });

  it("opens input on the first key and types it", () => {
    const t = setup();
    render(<TerminalView client={t.client} space="T1" screenFactory={t.factory} />);
    t.say({ type: "hello", space: "T1", pane: "%2" });
    t.type("y");
    expect(t.sockets[0]?.sent).toEqual([{ type: "input_open" }, { type: "input", data: "y" }]);
    expect(screen.getByText(STATUS_WORDS.typing)).toBeDefined();
  });

  it("sends the on-screen keys a phone lacks", () => {
    const t = setup();
    render(<TerminalView client={t.client} space="T1" screenFactory={t.factory} />);
    t.say({ type: "hello", space: "T1", pane: "%2" });
    fireEvent.click(screen.getByRole("button", { name: "Control C" }));
    fireEvent.click(screen.getByRole("button", { name: "Escape" }));
    expect(t.sockets[0]?.sent).toEqual([
      { type: "input_open" },
      { type: "key", key: "ctrl+c" },
      { type: "key", key: "esc" },
    ]);
  });

  it("shows the service's notices, and that the pane ended", () => {
    const t = setup();
    render(<TerminalView client={t.client} space="T1" screenFactory={t.factory} />);
    t.say({ type: "hello", space: "T1", pane: "%2" });
    t.say({ type: "notice", message: "too much to type at once: nothing was typed" });
    expect(screen.getByText(/too much to type at once/)).toBeDefined();
    t.say({ type: "ended", reason: "pane not found" });
    expect(screen.getByText(/The pane ended: pane not found/)).toBeDefined();
  });

  it("says it is reconnecting when the socket drops", () => {
    const t = setup();
    render(<TerminalView client={t.client} space="T1" screenFactory={t.factory} />);
    t.say({ type: "hello", space: "T1", pane: "%2" });
    act(() => t.sockets[0]?.handlers.close?.(1006));
    expect(screen.getByText(STATUS_WORDS.reconnecting)).toBeDefined();
  });

  it("says the terminal could not be opened after three refused handshakes", () => {
    vi.useFakeTimers();
    try {
      const t = setup();
      render(<TerminalView client={t.client} space="T9" screenFactory={t.factory} />);
      for (let i = 0; i < 3; i += 1) {
        act(() => t.sockets.at(-1)?.handlers.close?.(1006));
        act(() => vi.advanceTimersByTime(20_000));
      }
      expect(screen.getAllByText(STATUS_WORDS.refused).length).toBeGreaterThan(0);
    } finally {
      vi.useRealTimers();
    }
  });

  it("opens a new socket for another space", () => {
    const t = setup();
    const { rerender } = render(<TerminalView client={t.client} space="coordinator" screenFactory={t.factory} />);
    rerender(<TerminalView client={t.client} space="T1" screenFactory={t.factory} />);
    expect(t.sockets.map((s) => s.space)).toEqual(["coordinator", "T1"]);
  });
});
