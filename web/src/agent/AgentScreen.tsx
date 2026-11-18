// The Chat and Terminal screens: one agent, two views of it.
//
// On the Herdr desktop a pane has a Chat │ Terminal switch (`shprd-pane`), and
// the coordinator's pane has the same; the web's two screens are that switch.
// The space picker chooses whose agent it is: the coordinator's, or a task's.
// A picker that is empty of tasks still offers the coordinator, because its chat
// does not depend on a run having tasks.

import { Suspense, lazy, useCallback, useEffect, useState } from "react";

import type { ApiClient } from "../api/client";
import { useLiveRefresh } from "../api/useLive";
import type { TaskDto } from "../gen";
import { COORDINATOR } from "./chatModel";
import { ChatView } from "./ChatView";
import type { ScreenFactory } from "./screen";

// The terminal pulls in xterm.js, which is most of the app's weight and which the
// chat and the board never need: it loads when the Terminal view is first opened.
const TerminalView = lazy(() => import("./TerminalView").then((module) => ({ default: module.TerminalView })));

/** The two views of an agent. */
export type AgentView = "chat" | "terminal";

/** One entry of the space picker. */
export interface SpaceChoice {
  id: string;
  label: string;
}

/** The coordinator, then every task of the run, in plan order. */
export function spaceChoices(tasks: readonly Pick<TaskDto, "id" | "title">[]): SpaceChoice[] {
  return [
    { id: COORDINATOR, label: "Coordinator" },
    ...tasks.map((task) => ({ id: task.id, label: `${task.id} · ${task.title}` })),
  ];
}

/** Props of [`AgentScreen`]. */
export interface AgentScreenProps {
  client: ApiClient;
  view: AgentView;
  /** The Chat │ Terminal switch was pressed. */
  onView: (view: AgentView) => void;
  /** The space shown, kept by the shell so the two screens agree. */
  space: string;
  onSpace: (space: string) => void;
  /** Draws the terminal; xterm.js unless a test passes its own. */
  screenFactory?: ScreenFactory;
}

/** The Chat or Terminal screen. */
export function AgentScreen({ client, view, onView, space, onSpace, screenFactory }: AgentScreenProps) {
  const [tasks, setTasks] = useState<Pick<TaskDto, "id" | "title">[]>([]);

  const loadTasks = useCallback(async () => {
    try {
      const snapshot = await client.state();
      setTasks(snapshot.tasks);
    } catch {
      // The picker keeps what it had; the coordinator is always there.
    }
  }, [client]);

  useEffect(() => {
    void loadTasks();
  }, [loadTasks]);
  useLiveRefresh(client, () => void loadTasks());

  const choices = spaceChoices(tasks);
  // A task that left the run falls back to the coordinator, never a dead chat.
  const known = choices.some((choice) => choice.id === space);
  useEffect(() => {
    if (!known && tasks.length > 0) {
      onSpace(COORDINATOR);
    }
  }, [known, tasks.length, onSpace]);

  return (
    <section className="card agent" data-view={view}>
      <header className="agent-head">
        <h1 id="screen-heading" className="agent-title">
          {view === "chat" ? "Chat" : "Terminal"}
        </h1>
        <label className="agent-pick">
          <span className="visually-hidden">Space</span>
          <select value={known ? space : COORDINATOR} onChange={(event) => onSpace(event.target.value)}>
            {choices.map((choice) => (
              <option key={choice.id} value={choice.id}>
                {choice.label}
              </option>
            ))}
          </select>
        </label>
        <div className="agent-switch" role="group" aria-label="View">
          {(["chat", "terminal"] as const).map((name) => (
            <button
              key={name}
              type="button"
              className="agent-switch-button"
              aria-pressed={view === name}
              onClick={() => onView(name)}
            >
              {name === "chat" ? "Chat" : "Terminal"}
            </button>
          ))}
        </div>
      </header>
      {view === "chat" ? (
        <ChatView client={client} space={space} />
      ) : (
        <Suspense fallback={<p className="terminal-wait" role="status">Opening the terminal…</p>}>
          <TerminalView client={client} space={space} screenFactory={screenFactory} />
        </Suspense>
      )}
    </section>
  );
}
