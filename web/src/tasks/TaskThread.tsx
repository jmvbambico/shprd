// A task's thread: the head (back, Review, the task and its last status line, the
// Chat · Terminal · Pull request switch) over the view the switch picked.
//
// It mirrors the desktop's task thread (`host/herdr-desktop/mods/shprd-accept`'s
// head over `shprd-chat`, the terminal and `shprd-task-pr`). Chat is what a
// thread opens on. The terminal has its own screen in the web app; until that
// is in, Terminal says where to watch the agent.
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/chat/panel.js` (the head and its switch), `mods/task-pr/main.js`
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik

import { useEffect, useState } from "react";

import type { ApiClient } from "../api/client";
import type { TaskChangesDto } from "../gen";
import { PR_WORDS, canReview, openUrl, prSize, reportLine, taskPr, threadViews, type ThreadView } from "./accept";
import { look, progressOf, ref, taskById } from "./board";
import { ChatView } from "../agent/ChatView";
import { cards, forTask } from "./asks";
import { Glyph } from "./Glyph";
import { reason, type Board } from "./useBoard";

/** What the thread needs. */
export interface TaskThreadProps {
  /** The one client every screen calls. */
  client: ApiClient;
  /** The live board. */
  board: Board;
  /** The task it is about. */
  taskId: string;
  /** Back to the list. */
  onBack: () => void;
  /** Opens the Accept card. */
  onReview: (id: string) => void;
}

/** The thread. */
export function TaskThread({ client, board, taskId, onBack, onReview }: TaskThreadProps) {
  const task = taskById(board.state, taskId);
  const [view, setView] = useState<ThreadView>("chat");

  // Showing the pull request of a task that has none: back to its chat.
  useEffect(() => {
    if (view === "pr" && !task?.pr) {
      setView("chat");
    }
  }, [view, task]);

  // A different task opens on its chat.
  useEffect(() => setView("chat"), [taskId]);

  const live = task?.status === "working";
  const segs = threadViews(task, live, view);
  const shape = look(task);
  const said = reportLine(task);

  return (
    <section className="card thread" aria-label={task ? `${task.id} thread` : "Task thread"}>
      <header className="thread-head">
        <div className="thread-top">
          <button type="button" className="back" onClick={onBack}>
            ‹ Tasks
          </button>
          <span className="grow" />
          {task && canReview(task) ? (
            <button type="button" className="primary-button" onClick={() => onReview(task.id)}>
              Review
            </button>
          ) : null}
        </div>
        <div className="thread-what">
          <span className="thread-glyph">
            <Glyph shape={shape.glyph} tone={shape.tone} progress={progressOf(task)} label={shape.word} />
          </span>
          <div className="thread-text">
            <p className="thread-title">
              {task ? <span className="mono dim">{ref(task.id)}</span> : null}
              <span className="thread-title-words">{task?.title ?? ""}</span>
            </p>
            <p className="thread-says">{said ?? ""}</p>
          </div>
        </div>
        <div className="seg" role="tablist" aria-label="Thread view">
          {segs.map((seg) => (
            <button
              key={seg.key}
              type="button"
              role="tab"
              aria-selected={seg.on}
              data-dead={seg.dead}
              className="seg-button"
              onClick={() => setView(seg.key)}
            >
              {seg.word}
            </button>
          ))}
        </div>
      </header>

      <div className="thread-body">
        {task === null ? (
          <p className="note">
            {board.phase === "loading" ? "Reading the task…" : "This task is no longer on the board."}
          </p>
        ) : view === "chat" ? (
          <ChatView
            client={client}
            space={task.id}
            cards={forTask(cards(board.state, board.asks), task.id)}
            onAnswered={board.reload}
          />
        ) : view === "term" ? (
          <div className="note" data-view="terminal">
            <p>{live ? "The agent is working." : "No agent is up for this task."}</p>
            <p className="dim">
              Open the task’s space on the Herdr desktop to watch or type in its terminal.
            </p>
          </div>
        ) : (
          <PullRequest client={client} task={task} />
        )}
      </div>
    </section>
  );
}

/** The task's pull request: its number, its state, the size of the change and the link. */
function PullRequest({ client, task }: { client: ApiClient; task: NonNullable<ReturnType<typeof taskById>> }) {
  const [changes, setChanges] = useState<TaskChangesDto | null>(null);
  const [failure, setFailure] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const key = `${task.id}:${task.attempts.length}:${task.status}`;

  useEffect(() => {
    let live = true;
    client
      .taskChanges(task.id)
      .then((next) => live && setChanges(next))
      .catch((error: unknown) => live && setFailure(reason(error)));
    return () => {
      live = false;
    };
    // The task's own moves are the key; a new object each render is not.
  }, [client, task.id, key]);

  const card = taskPr(task);
  const url = openUrl(card);
  const size = prSize(changes);

  async function copy() {
    if (url === null) {
      return;
    }
    try {
      await globalThis.navigator?.clipboard?.writeText(url);
      setCopied(true);
    } catch {
      setCopied(false);
    }
  }

  return (
    <div className="pr" data-kind={card.kind}>
      <h2 className="pr-title">{PR_WORDS.title}</h2>
      {card.kind === "pr" ? (
        <>
          <p className="pr-line">
            <span className="pr-state" data-tone={card.tone}>
              {card.state}
            </span>
            <span className="mono">{card.number}</span>
            {size !== null ? (
              <>
                <span className="muted">{size.files}</span>
                <span className="mono add">{size.added}</span>
                <span className="mono del">{size.removed}</span>
              </>
            ) : null}
          </p>
          {card.head !== "" ? (
            <p className="mono dim small">
              {card.head}
              {changes?.base ? ` → ${changes.base}` : ""}
            </p>
          ) : null}
          <p className="muted">{card.line}</p>
          {failure !== null ? <p className="dim small">{failure}</p> : null}
          {url !== null ? (
            <div className="pr-actions">
              <a className="plain-button" href={url} target="_blank" rel="noreferrer noopener">
                {PR_WORDS.openOnGitHub}
              </a>
              <button type="button" className="plain-button" onClick={() => void copy()}>
                {copied ? PR_WORDS.copied : PR_WORDS.copyLink}
              </button>
            </div>
          ) : null}
        </>
      ) : (
        <p className="muted">{card.line}</p>
      )}
    </div>
  );
}
