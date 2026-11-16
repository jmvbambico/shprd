// The app shell: one column on a phone, two on a tablet, three on a desktop.
//
// M11.4–M11.7 fill the screens; this task ships their placeholders, the
// navigation, and the frame the screens will live in.

import { useState } from "react";

import type { ApiClient } from "../api/client";
import { SCREENS, SCREEN_LABEL, type Screen } from "../layout/layout";
import { useLayout } from "../layout/useLayout";
import { GraphScreen } from "../graph/GraphScreen";
import { Login } from "./Login";

/** What one screen's placeholder says: which milestone fills it. */
const PLACEHOLDER: Record<Screen, { lead: string; note: string }> = {
  tasks: {
    lead: "The board lands here.",
    note: "M11.4: every task with its derived status, the accept card, and the needs-you cards.",
  },
  graph: {
    lead: "The graph lands here.",
    note: "M11.5: the run document the dagr block draws, as a graph.",
  },  terminal: {
    lead: "The terminal lands here.",
    note: "M11.6: watch a task's agent by default, type on demand.",
  },
  chat: {
    lead: "The coordinator chat lands here.",
    note: "M11.6: the same conversation the Herdr space shows.",
  },
  settings: {
    lead: "Settings land here.",
    note: "M11.7: agents, roles, prompts, and Remote (tunnels, sessions, QR).",
  },
};

/** What the shell needs from the app above it. */
export interface ShellProps {
  /** The one client every screen calls. */
  client: ApiClient;
  /** True while the service wants a fresh password for a destructive action. */
  reauth: boolean;
  /** Dismisses the re-authenticate dialog. */
  onReauthClose: () => void;
  /** The person signed out (or their session ended). */
  onSignedOut: () => void;
}

/** The signed-in frame. */
export function Shell({ client, reauth, onReauthClose, onSignedOut }: ShellProps) {
  const layout = useLayout();
  const [screen, setScreen] = useState<Screen>("tasks");
  const placeholder = PLACEHOLDER[screen];

  function signOut() {
    void client.logout().catch(() => {
      // A failed logout still means this page shows the login screen; the
      // service's session is revoked by expiry or by `shprd web sessions`.
    });
    onSignedOut();
  }

  return (
    <div className="shell" data-layout={layout} data-screen={screen}>
      <header className="topbar">
        <span className="brand">
          <span className="brand-mark" aria-hidden="true" />
          <span className="brand-word">shprd</span>
        </span>
        {layout === "desktop" ? <span className="topbar-note">M11 web app · scaffold</span> : null}
        <button className="quiet" type="button" onClick={signOut}>
          Sign out
        </button>
      </header>

      {layout === "desktop" ? (
        <nav className="rail" aria-label="Screens">
          <ScreenList screen={screen} onScreen={setScreen} />
        </nav>
      ) : null}

      <main className="board" aria-labelledby="screen-heading">
        {screen === "graph" ? (
          <GraphScreen client={client} />
        ) : (
          <section className="card placeholder" data-screen={screen}>
            <p className="placeholder-kicker">{SCREEN_LABEL[screen]}</p>
            <h1 id="screen-heading" className="placeholder-lead">
              {placeholder.lead}
            </h1>
            <p className="placeholder-note">{placeholder.note}</p>
            <p className="placeholder-empty">
              Nothing to show yet — this build has no run to draw here.
            </p>
          </section>
        )}
      </main>

      {layout === "desktop" ? (
        <aside className="side" aria-label="Needs you">
          <section className="card side-card">
            <h2 className="side-title">Needs you</h2>
            <p className="side-note">Asks and blockers land here in M11.4.</p>
          </section>
          <section className="card side-card">
            <h2 className="side-title">Chat</h2>
            <p className="side-note">The coordinator's conversation lands here in M11.6.</p>
          </section>
        </aside>
      ) : null}

      {layout === "phone" ? (
        <nav className="tabs" aria-label="Screens">
          <ScreenList screen={screen} onScreen={setScreen} compact />
        </nav>
      ) : null}

      {layout === "tablet" ? (
        <nav className="rail rail-compact" aria-label="Screens">
          <ScreenList screen={screen} onScreen={setScreen} compact />
        </nav>
      ) : null}

      {reauth ? (
        <div className="overlay" role="dialog" aria-modal="true" aria-label="Enter your password again">
          <Login client={client} reauth onSignedIn={onReauthClose} />
        </div>
      ) : null}
    </div>
  );
}

/** The screen switcher: a sidebar on a desktop, tabs on a phone. */
function ScreenList({
  screen,
  onScreen,
  compact = false,
}: {
  screen: Screen;
  onScreen: (screen: Screen) => void;
  compact?: boolean;
}) {
  return (
    <ul className={compact ? "screen-list compact" : "screen-list"}>
      {SCREENS.map((name) => (
        <li key={name}>
          <button
            type="button"
            className="screen-button"
            aria-current={name === screen ? "page" : undefined}
            onClick={() => onScreen(name)}
          >
            <span className="screen-glyph" aria-hidden="true" />
            <span className="screen-label">{SCREEN_LABEL[name]}</span>
          </button>
        </li>
      ))}
    </ul>
  );
}
