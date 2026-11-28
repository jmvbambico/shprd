// The app shell: one column on a phone, two on a tablet, three on a desktop.
//
// Every screen is real: the shell owns the navigation, the open thread and the
// dialogs over them.

import { useCallback, useState } from "react";

import type { ApiClient } from "../api/client";
import { SCREENS, SCREEN_LABEL, type Screen } from "../layout/layout";
import { useLayout } from "../layout/useLayout";
import { AgentScreen } from "../agent/AgentScreen";
import { COORDINATOR } from "../agent/chatModel";
import { roleTitle } from "../roles";
import { GraphScreen } from "../graph/GraphScreen";
import { SettingsDialog } from "../settings/SettingsDialog";
import { AcceptDialog } from "../tasks/AcceptDialog";
import { canReview } from "../tasks/accept";
import { NEED, ref, taskById } from "../tasks/board";
import { TaskThread } from "../tasks/TaskThread";
import { TasksScreen } from "../tasks/TasksScreen";
import type { Toast } from "../tasks/toasts";
import { ToastStack } from "../tasks/ToastStack";
import { useBoard } from "../tasks/useBoard";
import { Login } from "./Login";

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
  const [selected, setSelected] = useState<Screen>("tasks");
  // Settings is a dialog over the app, not a place the board moves to: the screen
  // under it stays as it was, and closing it goes back there.
  const [under, setUnder] = useState<Screen>("tasks");
  // The space the Chat and Terminal screens show: one choice for both, as a
  // pane's Chat │ Terminal switch is one pane.
  const [agentSpace, setAgentSpace] = useState<string>(COORDINATOR);
  const screen = selected === "settings" ? under : selected;
  const settingsOpen = selected === "settings";
  const board = useBoard(client);
  // The task whose thread is open, and the one whose Accept card is.
  const [thread, setThread] = useState<string | null>(null);
  const [accepting, setAccepting] = useState<string | null>(null);
  // Whether the card opens on its Drop step: a row's Drop goes straight there.
  const [dropFirst, setDropFirst] = useState(false);
  const review = (id: string) => {
    setDropFirst(false);
    setAccepting(id);
  };
  const dropTask = (id: string) => {
    setDropFirst(true);
    setAccepting(id);
  };
  // Counts up each time the password was entered again, so a refused verb is sent again.
  const [reauthTick, setReauthTick] = useState(0);

  function setScreen(next: Screen) {
    if (next === "settings") {
      if (selected !== "settings") {
        setUnder(selected);
      }
    }
    setSelected(next);
  }

  const openThread = useCallback((id: string) => {
    setSelected("tasks");
    setThread(id);
  }, []);

  // A tap on a toast: the card for a task ready to review, the thread for one
  // that needs the person, the list for anything else.
  function openToast(toast: Toast) {
    const task = taskById(board.state, toast.task);
    if (task && canReview(task)) {
      setSelected("tasks");
      setThread(task.id);
      setAccepting(task.id);
    } else if (task && toast.level === "warning") {
      openThread(task.id);
    } else {
      setSelected("tasks");
      setThread(null);
    }
  }

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
        <button className="quiet" type="button" onClick={signOut}>
          Sign out
        </button>
      </header>

      {layout === "desktop" ? (
        <nav className="rail" aria-label="Screens">
          <ScreenList screen={selected} onScreen={setScreen} />
        </nav>
      ) : null}

      <main className="board" aria-labelledby="screen-heading">
        {screen === "graph" ? (
          <GraphScreen client={client} />
        ) : screen === "chat" || screen === "terminal" ? (
          <AgentScreen
            client={client}
            view={screen}
            onView={setScreen}
            space={agentSpace}
            onSpace={setAgentSpace}
          />
        ) : screen === "tasks" ? (
          thread !== null && layout !== "phone" ? (
            <TaskThread
              client={client}
              board={board}
              taskId={thread}
              onBack={() => setThread(null)}
              onReview={review}
            />
          ) : (
            <TasksScreen client={client} board={board} onOpen={openThread} onReview={review} onDrop={dropTask} />
          )
        ) : null}
      </main>

      {layout === "desktop" ? (
        <aside className="side" aria-label="Needs you">
          <section className="card side-card">
            <h2 className="side-title">Needs you</h2>
            <NeedsYou board={board} onOpen={openThread} />
          </section>
          <section className="card side-card">
            <h2 className="side-title">Chat</h2>
            <button type="button" className="quiet" onClick={() => setScreen("chat")}>
              Open {roleTitle(board.state?.role_titles, "coordinator")}&rsquo;s chat
            </button>
          </section>
        </aside>
      ) : null}

      {layout === "phone" ? (
        <nav className="tabs" aria-label="Screens">
          <ScreenList screen={selected} onScreen={setScreen} compact />
        </nav>
      ) : null}

      {layout === "tablet" ? (
        <nav className="rail rail-compact" aria-label="Screens">
          <ScreenList screen={selected} onScreen={setScreen} compact />
        </nav>
      ) : null}

      {/* On a phone a task opens as a full-screen sheet over the board. */}
      {screen === "tasks" && thread !== null && layout === "phone" ? (
        <div className="thread-sheet" role="dialog" aria-modal="true" aria-label="Task">
          <TaskThread
            client={client}
            board={board}
            taskId={thread}
            onBack={() => setThread(null)}
            onReview={review}
          />
        </div>
      ) : null}

      {accepting !== null ? (
        <AcceptDialog
          client={client}
          board={board}
          taskId={accepting}
          startDropping={dropFirst}
          device={layout}
          reauthTick={reauthTick}
          onClose={() => setAccepting(null)}
        />
      ) : null}

      <ToastStack
        toasts={board.toasts}
        dismissed={board.dismissed}
        onDismiss={board.dismissToast}
        onOpen={openToast}
      />

      {settingsOpen ? (
        <SettingsDialog
          client={client}
          onClose={() => setSelected(under)}
          onSignedOut={onSignedOut}
        />
      ) : null}

      {reauth ? (
        <div className="overlay" role="dialog" aria-modal="true" aria-label="Enter your password again">
          <Login
            client={client}
            reauth
            onSignedIn={() => {
              onReauthClose();
              setReauthTick((n) => n + 1);
            }}
          />
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

/** The tasks that need the person, as the desktop's needs-you list shows them. */
function NeedsYou({ board, onOpen }: { board: ReturnType<typeof useBoard>; onOpen: (id: string) => void }) {
  const waiting = (board.state?.tasks ?? []).filter((task) => task.derived.status === "needs_you");
  if (waiting.length === 0) {
    return <p className="side-note">Nothing needs you right now.</p>;
  }
  return (
    <ul className="needs-list">
      {waiting.map((task) => (
        <li key={task.id}>
          <button type="button" onClick={() => onOpen(task.id)}>
            <span className="mono dim">{ref(task.id)}</span>
            <span className="needs-says">
              {task.derived.reason ?? (task.derived.need ? NEED[task.derived.need] : task.title)}
            </span>
          </button>
        </li>
      ))}
    </ul>
  );
}
