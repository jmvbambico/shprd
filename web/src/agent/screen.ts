// The terminal widget: xterm.js, configured to be a window onto a pane.
//
// It never decides what a pane does and never resizes one: the service pushes
// the pane's whole screen, and this draws it at the size the screen needs (so a
// line the agent wrapped stays wrapped as it was) inside a box that scrolls.
//
// Hardening, each of them a rule of M7d §4:
//
// - **OSC 52 is swallowed**: a program in the pane may not write the person's
//   clipboard, so the sequence is handled (and ignored) before xterm sees it;
// - **links are web pages only**: an OSC 8 hyperlink is opened only when its
//   address is http(s), in a new tab with no opener;
// - the keyboard's data goes to `onData` and from there through
//   `inputToMessages`, which caps a paste.

import { Terminal } from "@xterm/xterm";

import { safeTerminalLink } from "./terminalSession";

/** The least columns and rows a screen is drawn at. */
const MIN_COLS = 80;
const MIN_ROWS = 24;

/** What the view needs from a drawn terminal; a test passes a fake. */
export interface Screen {
  /** Draws the pane's whole screen. */
  show(text: string): void;
  /** Gives the keyboard to the terminal. */
  focus(): void;
  /** Releases it. */
  dispose(): void;
}

/** Makes a [`Screen`] inside `host`; `onData` gets what the keyboard sends. */
export type ScreenFactory = (host: HTMLElement, onData: (data: string) => void) => Screen;

/** The columns and rows `text` needs. */
export function sizeOf(text: string): { cols: number; rows: number } {
  const lines = text.split("\n");
  const widest = lines.reduce((most, line) => Math.max(most, line.length), 0);
  return { cols: Math.max(MIN_COLS, widest), rows: Math.max(MIN_ROWS, lines.length) };
}

/** Opens a hyperlink a pane's program wrote, when it is a web page. */
export function openLink(uri: string, open: (url: string, target: string, features: string) => unknown): boolean {
  if (!safeTerminalLink(uri)) {
    return false;
  }
  open(uri, "_blank", "noopener,noreferrer");
  return true;
}

/** The real thing: xterm.js. */
export const xtermScreen: ScreenFactory = (host, onData) => {
  const term = new Terminal({
    convertEol: true,
    cursorBlink: false,
    cursorStyle: "bar",
    scrollback: 0,
    // A concrete list: xterm measures a cell with it, and a CSS variable does not resolve there.
    fontFamily: 'ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace',
    fontSize: 12,
    allowProposedApi: false,
    linkHandler: {
      activate: (_event, uri) => {
        openLink(uri, (url, target, features) => window.open(url, target, features));
      },
    },
    theme: { background: "#1b1f24", foreground: "#abb2bf", cursor: "#61afef" },
  });
  // OSC 52 sets the clipboard: handled here, and so never acted on.
  term.parser.registerOscHandler(52, () => true);
  term.open(host);
  term.onData(onData);
  let size = { cols: 0, rows: 0 };
  return {
    show(text) {
      const next = sizeOf(text);
      if (next.cols !== size.cols || next.rows !== size.rows) {
        term.resize(next.cols, next.rows);
        size = next;
      }
      // One write: clear, home, draw. Nothing flickers between frames.
      term.write(`\x1b[2J\x1b[3J\x1b[H${text.replace(/\n/g, "\r\n")}`);
    },
    focus() {
      term.focus();
    },
    dispose() {
      term.dispose();
    },
  };
};
