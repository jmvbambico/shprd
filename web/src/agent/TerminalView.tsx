// The terminal view: watch a pane, type on demand.
//
// It is the pane's Terminal side of the Chat │ Terminal switch. The page only
// watches until a key is pressed; the first key opens input on the socket
// (`TerminalSession`). A bar of the keys a phone's keyboard lacks sits under it.

import { useEffect, useRef, useState } from "react";

import type { ApiClient } from "../api/client";
import { xtermScreen, type Screen, type ScreenFactory } from "./screen";
import { TerminalSession, type TerminalStatus } from "./terminalSession";

/** The words under the terminal, by where the connection is. */
export const STATUS_WORDS: Record<TerminalStatus, string> = {
  connecting: "Connecting…",
  watching: "Watching. Tap the terminal to type.",
  typing: "Typing into the agent's terminal.",
  reconnecting: "Connection lost. Reconnecting…",
  refused: "This terminal could not be opened. The space may have no agent running.",
};

/** The keys a phone's keyboard cannot press, as the socket names them. */
const KEYS: Array<[label: string, data: string, name: string]> = [
  ["Esc", "\x1b", "Escape"],
  ["Tab", "\t", "Tab"],
  ["↑", "\x1b[A", "Up"],
  ["↓", "\x1b[B", "Down"],
  ["Ctrl-C", "\x03", "Control C"],
  ["Enter", "\r", "Enter"],
];

/** Props of [`TerminalView`]. */
export interface TerminalViewProps {
  client: Pick<ApiClient, "terminal">;
  /** `coordinator` or a task id. */
  space: string;
  /** Draws the screen; xterm.js unless a test passes its own. */
  screenFactory?: ScreenFactory;
}

/** The watch-and-type terminal of one space. */
export function TerminalView({ client, space, screenFactory = xtermScreen }: TerminalViewProps) {
  const host = useRef<HTMLDivElement>(null);
  const session = useRef<TerminalSession | null>(null);
  const [status, setStatus] = useState<TerminalStatus>("connecting");
  const [notice, setNotice] = useState<string | null>(null);
  const [blank, setBlank] = useState(true);
  const [wide, setWide] = useState(false);
  const box = useRef<HTMLDivElement>(null);

  // The pane is drawn at its own width and never resized to the screen, so on a
  // phone a wide screen scrolls sideways inside its box. Say so, rather than
  // letting the right edge look cut off.
  useEffect(() => {
    const measure = () => {
      const el = box.current;
      setWide(el !== null && el.scrollWidth > el.clientWidth + 1);
    };
    const timer = window.setInterval(measure, 500);
    window.addEventListener("resize", measure);
    measure();
    return () => {
      window.clearInterval(timer);
      window.removeEventListener("resize", measure);
    };
  }, []);

  useEffect(() => {
    const el = host.current;
    if (!el) {
      return undefined;
    }
    setStatus("connecting");
    setNotice(null);
    setBlank(true);
    const live = new TerminalSession((handlers) => client.terminal(space, handlers), {
      screen: (text, cut) => {
        setBlank(false);
        if (cut) {
          setNotice("The screen was longer than 1 MiB, so its start is cut.");
        }
        drawn?.show(text);
      },
      status: setStatus,
      notice: setNotice,
    });
    let drawn: Screen | null = screenFactory(el, (data) => live.type(data));
    session.current = live;
    live.start();
    return () => {
      live.dispose();
      session.current = null;
      drawn?.dispose();
      drawn = null;
    };
    // The factory and client are stable for a page; a different space is a new socket.
  }, [client, space]);

  return (
    <section className="terminal" aria-label={`Terminal of ${space}`}>
      <div ref={box} className="terminal-screen" data-blank={blank} data-wide={wide}>
        <div ref={host} className="terminal-host" />
        {blank ? (
          <p className="terminal-wait" role="status">
            {status === "refused" ? STATUS_WORDS.refused : "Waiting for the agent's screen…"}
          </p>
        ) : null}
      </div>
      <div className="terminal-keys" role="group" aria-label="Keys">
        {KEYS.map(([label, data, name]) => (
          <button
            key={label}
            type="button"
            className="quiet terminal-key"
            aria-label={name}
            onClick={() => session.current?.type(data)}
          >
            {label}
          </button>
        ))}
      </div>
      <p className="terminal-status" role="status" data-status={status}>
        {notice ?? STATUS_WORDS[status]}
        {wide ? " Scroll sideways for the rest of the line." : ""}
      </p>
    </section>
  );
}
