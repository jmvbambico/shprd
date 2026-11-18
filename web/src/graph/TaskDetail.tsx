// One task, opened from its node.
//
// This is the graph's own detail, not M11.4's task sheet: it reads the run
// document's record of the task — its state, what it waits on, and every
// attempt with the evidence that settled it — and nothing that would need the
// board's snapshot. When M11.4's sheet lands, this is what it replaces.
//
// A phone gets it as a sheet over the graph (the shape Shepherdr's window uses
// for a row's panel items), a desktop as a card beside the drawing.

import { useEffect, type ReactNode } from "react";

import {
  criteria,
  evidenceMark,
  latestAttempt,
  runTaskTone,
  taskTone,
  type RunDocument,
  type RunTask,
} from "../api/rundoc";
import type { Layout } from "../layout/layout";
import { when } from "./format";
import { waitsOn } from "./layout";

/** What the detail needs from the screen. */
export interface TaskDetailProps {
  /** The task it opens. */
  task: RunTask;
  /** The run, so a dependency can be found and followed. */
  document: RunDocument;
  /** The layout class: a sheet on a phone, a card on a desktop. */
  device: Layout;
  /** Opens another task, so a dependency can be followed by tapping it. */
  onSelect: (id: string) => void;
  /** Closes the detail. */
  onClose: () => void;
}

/** One task's detail, as a sheet or a card. */
export function TaskDetail({ task, document, device, onSelect, onClose }: TaskDetailProps) {
  const sheet = device !== "desktop";

  // Escape closes it, as a dialog does everywhere else in the app.
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        onClose();
      }
    };
    globalThis.addEventListener("keydown", onKey);
    return () => globalThis.removeEventListener("keydown", onKey);
  }, [onClose]);

  const content = (
    <section
      className={sheet ? "task-detail sheet" : "task-detail card"}
      role="dialog"
      aria-modal={sheet ? true : undefined}
      aria-labelledby="task-detail-title"
      data-device={device}
    >
      <header className="task-detail-head">
        <div className="task-detail-what">
          <p className="task-detail-id">
            {task.id}
            <span className="task-detail-kind">{task.kind}</span>
          </p>
          <h2 className="task-detail-title" id="task-detail-title">
            {task.title}
          </h2>
        </div>
        <button type="button" className="quiet task-detail-close" onClick={onClose}>
          Close
        </button>
      </header>

      <TaskBody task={task} document={document} onSelect={onSelect} />
    </section>
  );

  if (!sheet) {
    return content;
  }
  return (
    <div className="sheet-backdrop" onClick={onClose}>
      <div onClick={(event) => event.stopPropagation()}>{content}</div>
    </div>
  );
}

/** The detail's facts and attempts, shared by both shapes. */
function TaskBody({
  task,
  document,
  onSelect,
}: {
  task: RunTask;
  document: RunDocument;
  onSelect: (id: string) => void;
}) {
  const state = runTaskTone(task);
  const attempt = latestAttempt(task);
  const mark = attempt?.outcome === undefined ? null : evidenceMark(attempt.outcome.evidence);
  const deps = waitsOn(task, document.tasks);
  const known = new Set(document.tasks.map((other) => other.id));
  const missing = task.deps.filter((dep) => !known.has(dep));
  const accepted = criteria(task);

  return (
    <div className="task-detail-body">
      <p className={`task-detail-state tone-${state.tone}`}>
        <span className="graph-dot" aria-hidden="true">
          ●
        </span>{" "}
        {state.word}
        {task.note === undefined ? null : <span className="task-detail-note"> · {task.note}</span>}
      </p>

      <dl className="task-detail-facts">
        {task.project === undefined ? null : (
          <Fact
            word="Project"
            value={
              document.projects.find((project) => project.id === task.project)?.title ??
              task.project
            }
          />
        )}
        {task.owner === undefined ? null : <Fact word="Owner" value={task.owner} />}
        {task.unblock === undefined ? null : <Fact word="Waits for" value={task.unblock} />}
        <Fact
          word="Waits on"
          value={
            deps.length === 0 && missing.length === 0 ? (
              "Nothing"
            ) : (
              <span className="task-detail-deps">
                {deps.map((dep) => (
                  <button
                    key={dep.id}
                    type="button"
                    className="task-detail-dep"
                    onClick={() => onSelect(dep.id)}
                  >
                    {dep.id}
                  </button>
                ))}
                {missing.map((dep) => (
                  <span key={dep} className="task-detail-missing" title="not in this run">
                    {dep}
                  </span>
                ))}
              </span>
            )
          }
        />
        {mark === null ? null : (
          <Fact
            word="Evidence"
            value={
              <span className={`tone-${mark.tone}`}>
                <span aria-hidden="true">{mark.glyph}</span> {mark.word}
              </span>
            }
          />
        )}
      </dl>

      <section className="task-detail-section" aria-labelledby="task-detail-attempts">
        <h3 className="task-detail-heading" id="task-detail-attempts">
          Attempts
        </h3>
        {task.attempts.length === 0 ? (
          <p className="task-detail-empty">No attempt yet.</p>
        ) : (
          <ol className="attempt-list">
            {task.attempts.map((entry) => {
              const tone = taskTone(entry.state);
              const evidence =
                entry.outcome === undefined ? null : evidenceMark(entry.outcome.evidence);
              return (
                <li key={entry.id} className="attempt">
                  <p className="attempt-head">
                    <span className="attempt-id">a{entry.n}</span>
                    <span className="attempt-actor">
                      {entry.actor === "" ? "unknown agent" : entry.actor}
                      {entry.model === undefined ? "" : ` · ${entry.model}`}
                    </span>
                    <span className={`attempt-state tone-${tone.tone}`}>{tone.word}</span>
                  </p>
                  <p className="attempt-when">
                    {when(entry.started_at)}
                    {entry.ended_at === undefined ? " → running" : ` → ${when(entry.ended_at)}`}
                    {entry.cause.type === "initial" ? "" : ` · ${entry.cause.type.replace("_", " ")}`}
                  </p>
                  {evidence === null ? null : (
                    <p className={`attempt-evidence tone-${evidence.tone}`}>
                      <span aria-hidden="true">{evidence.glyph}</span> {evidence.word}
                    </p>
                  )}
                </li>
              );
            })}
          </ol>
        )}
      </section>

      {accepted.length === 0 ? null : (
        <section className="task-detail-section" aria-labelledby="task-detail-acceptance">
          <h3 className="task-detail-heading" id="task-detail-acceptance">
            Acceptance
          </h3>
          <ul className="task-detail-criteria">
            {accepted.map((line) => (
              <li key={line}>{line}</li>
            ))}
          </ul>
        </section>
      )}
    </div>
  );
}

/** One fact row. */
function Fact({ word, value }: { word: string; value: ReactNode }) {
  return (
    <>
      <dt>{word}</dt>
      <dd>{value}</dd>
    </>
  );
}
