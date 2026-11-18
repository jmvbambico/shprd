// The xterm.js widget's hardening, against a recording stand-in for xterm.

// @vitest-environment jsdom

import { beforeEach, describe, expect, it, vi } from "vitest";

interface Recorded {
  options: { linkHandler?: { activate: (event: unknown, uri: string) => void } } & Record<string, unknown>;
  osc: Array<[number, () => boolean]>;
  writes: string[];
  resized: Array<[number, number]>;
  data: ((data: string) => void) | null;
}

const made: Recorded[] = [];

vi.mock("@xterm/xterm", () => ({
  Terminal: class {
    readonly rec: Recorded;
    readonly parser = {
      registerOscHandler: (id: number, handler: () => boolean) => {
        this.rec.osc.push([id, handler]);
      },
    };
    constructor(options: Recorded["options"]) {
      this.rec = { options, osc: [], writes: [], resized: [], data: null };
      made.push(this.rec);
    }
    open() {}
    onData(handler: (data: string) => void) {
      this.rec.data = handler;
    }
    resize(cols: number, rows: number) {
      this.rec.resized.push([cols, rows]);
    }
    write(text: string) {
      this.rec.writes.push(text);
    }
    focus() {}
    dispose() {}
  },
}));

import { openLink, sizeOf, xtermScreen } from "./screen";

beforeEach(() => {
  made.length = 0;
});

describe("sizeOf", () => {
  it("never draws smaller than 80x24, and grows to fit the screen", () => {
    expect(sizeOf("hi")).toEqual({ cols: 80, rows: 24 });
    expect(sizeOf(`${"x".repeat(120)}\n${"\n".repeat(40)}`)).toEqual({ cols: 120, rows: 42 });
  });
});

describe("openLink", () => {
  it("opens web pages in a new tab with no opener, and nothing else", () => {
    const open = vi.fn();
    expect(openLink("https://example.com/a", open)).toBe(true);
    expect(open).toHaveBeenCalledWith("https://example.com/a", "_blank", "noopener,noreferrer");
    for (const uri of ["javascript:alert(1)", "file:///etc/passwd", "ssh://h", "data:text/html,x"]) {
      expect(openLink(uri, open)).toBe(false);
    }
    expect(open).toHaveBeenCalledTimes(1);
  });
});

describe("xtermScreen", () => {
  it("swallows OSC 52 so a pane cannot write the clipboard", () => {
    xtermScreen(document.createElement("div"), () => undefined);
    const [[id, handler]] = made[0]!.osc as [[number, () => boolean]];
    expect(id).toBe(52);
    expect(handler()).toBe(true);
  });

  it("routes link clicks through the http(s) check", () => {
    const open = vi.spyOn(window, "open").mockImplementation(() => null);
    xtermScreen(document.createElement("div"), () => undefined);
    const handler = made[0]!.options.linkHandler!;
    handler.activate({}, "javascript:alert(1)");
    expect(open).not.toHaveBeenCalled();
    handler.activate({}, "https://example.com");
    expect(open).toHaveBeenCalledWith("https://example.com", "_blank", "noopener,noreferrer");
    open.mockRestore();
  });

  it("draws a screen in one write and resizes only when the size changes", () => {
    const shown = xtermScreen(document.createElement("div"), () => undefined);
    shown.show("a\nb");
    shown.show("a\nc");
    expect(made[0]!.writes).toEqual(["\x1b[2J\x1b[3J\x1b[H" + "a\r\nb", "\x1b[2J\x1b[3J\x1b[H" + "a\r\nc"]);
    expect(made[0]!.resized).toEqual([[80, 24]]);
  });

  it("hands the keyboard's data to the caller", () => {
    const got: string[] = [];
    xtermScreen(document.createElement("div"), (d) => got.push(d));
    made[0]!.data?.("x");
    expect(got).toEqual(["x"]);
  });
});
