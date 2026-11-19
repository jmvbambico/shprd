// The Tasks screen: the run's board, one row a task, grouped by the word shprd's
// rules give it, with the person's own verbs on each row and a form that adds a
// task.
//
// It mirrors the Herdr desktop's Tasks panel (`host/herdr-desktop/mods/shprd-tasks`):
// a search field, a filter row (All statuses, then the chips that have rows, then
// More), folding groups with "For you" on top and Done as one quiet line, a
// two-line row with its glyph and its one act. The service derives every word;
// nothing here decides what a task is doing.
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/tasks/main.js` (the header, the filter row, the grouped list)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik

import { useCallback, useId, useState, type FormEvent } from "react";

import type { ApiClient } from "../api/client";
import type { TaskDto } from "../gen";
import { counts, filterChips, isLive, listView, look, newTask, progressOf, ref, rowActions, secondLine } from "./board";
import { Glyph } from "./Glyph";
import { reason, type Board } from "./useBoard";

/** What the screen needs. */
export interface TasksScreenProps {
  /** The one client every screen calls. */
  client: ApiClient;
  /** The live board. */
  board: Board;
  /** Opens a task's thread. */
  onOpen: (id: string) => void;
  /** Opens a task's Accept card. */
  onReview: (id: string) => void;
  /** Opens the card on its Drop step. */
  onDrop?: (id: string) => void;
}

/** Reads one remembered value; storage may be missing or throw. */
function load<T>(key: string, fallback: T): T {
  try {
    const raw = globalThis.localStorage?.getItem(`tasks.${key}`);
    return raw === null || raw === undefined ? fallback : (JSON.parse(raw) as T);
  } catch {
    return fallback;
  }
}

/** Remembers one value; a storage that refuses is not an error. */
function save(key: string, value: unknown): void {
  try {
    globalThis.localStorage?.setItem(`tasks.${key}`, JSON.stringify(value));
  } catch {
    // The screen works the same, it just forgets on reload.
  }
}

/** The Tasks screen. */
export function TasksScreen({ client, board, onOpen, onReview, onDrop }: TasksScreenProps) {
  const { state } = board;
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<string>(() => load("filter", "all"));
  const [shut, setShut] = useState<ReadonlySet<string>>(() => new Set(load<string[]>("shut", [])));
  const [doneOpen, setDoneOpen] = useState<boolean>(() => load<boolean>("doneopen", false));
  const [allDone, setAllDone] = useState<boolean>(() => load<boolean>("alldone", false));
  const [moreOpen, setMoreOpen] = useState(false);
  const [adding, setAdding] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);

  const tally = counts(state);
  const chips = filterChips(state);
  // A chip kept for a word nobody is in any more would show an empty list: fall back.
  const known = [...chips.primary, ...chips.more].some((c) => c.key === filter);
  const active = known ? filter : "all";
  const view = listView(state, { query, filter: active, shut, doneOpen, allDone });
  const searching = query.trim() !== "";
  const nothing = view.groups.length === 0 && view.doneLine === null;

  const fold = useCallback((key: string) => {
    setShut((held) => {
      const next = new Set(held);
      if (!next.delete(key)) {
        next.add(key);
      }
      save("shut", [...next]);
      return next;
    });
  }, []);

  function pick(key: string) {
    setFilter(key);
    setMoreOpen(false);
    save("filter", key);
  }

  async function verb(task: TaskDto, action: "task.start" | "task.retry" | "task.stop") {
    setActionError(null);
    try {
      await client.action({ kind: action, task: task.id });
      await board.reload();
    } catch (failure) {
      setActionError(`${action} ${task.id}: ${reason(failure)}`);
    }
  }

  return (
    <section className="card tasks" aria-labelledby="screen-heading">
      <header className="tasks-head">
        <h1 id="screen-heading" className="tasks-title">
          Tasks
        </h1>
        {tally.needs > 0 ? (
          <span className="count-badge" data-tone="red" aria-label={`${tally.needs} need you`}>
            {tally.needs}
          </span>
        ) : null}
        {tally.ready > 0 ? (
          <span className="count-badge" data-tone="green" aria-label={`${tally.ready} ready`}>
            {tally.ready}
          </span>
        ) : null}
        <span className="grow" />
        {!adding ? (
          <button type="button" className="icon-button" aria-label="Add a task" onClick={() => setAdding(true)}>
            +
          </button>
        ) : null}
      </header>

      <div className="tasks-search">
        <span aria-hidden="true" className="tasks-search-glyph">
          ⌕
        </span>
        <input
          type="search"
          className="tasks-search-input"
          placeholder="Filter tasks"
          aria-label="Filter tasks"
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Escape") {
              setQuery("");
            }
          }}
        />
        {query !== "" ? (
          <button type="button" className="quiet-text" onClick={() => setQuery("")}>
            × Clear
          </button>
        ) : null}
      </div>

      <div className="tasks-filter" role="group" aria-label="Filter by status">
        <Chip on={active === "all"} onClick={() => pick("all")}>
          All statuses
        </Chip>
        {chips.primary.map((chip) => (
          <Chip key={chip.key} on={active === chip.key} count={chip.count} onClick={() => pick(chip.key)}>
            {chip.word}
          </Chip>
        ))}
        {chips.more.length > 0 ? (
          <Chip
            on={moreOpen || chips.more.some((c) => c.key === active)}
            onClick={() => setMoreOpen((open) => !open)}
            expanded={moreOpen}
          >
            {chips.more.find((c) => c.key === active)?.word ?? "More ⌄"}
          </Chip>
        ) : null}
      </div>
      {moreOpen ? (
        <ul className="tasks-more" aria-label="More statuses">
          {chips.more.map((chip) => (
            <li key={chip.key}>
              <button type="button" aria-pressed={active === chip.key} onClick={() => pick(chip.key)}>
                <span>{chip.word}</span>
                <span className="dim">{chip.count}</span>
              </button>
            </li>
          ))}
        </ul>
      ) : null}

      <div className="tasks-body">
        {actionError !== null ? (
          <p className="tasks-wrong" role="alert">
            {actionError}
          </p>
        ) : null}
        {adding ? (
          <AddTask
            client={client}
            known={(board.state?.tasks ?? []).filter(isLive).map((task) => task.id)}
            onDone={async () => {
              setAdding(false);
              await board.reload();
            }}
            onCancel={() => setAdding(false)}
          />
        ) : null}

        {board.phase === "loading" ? (
          <p className="note" aria-busy="true">
            Reading the board…
          </p>
        ) : board.phase === "error" ? (
          <div className="note" role="alert">
            <p>{board.error}</p>
            <p className="dim">Start it with `shprd serve`, then this fills in.</p>
            <button type="button" className="quiet-text" onClick={() => void board.reload()}>
              Try again
            </button>
          </div>
        ) : (state?.tasks ?? []).length === 0 ? (
          <div className="note">
            <p>No tasks yet.</p>
            <p className="dim">Add one with + above; the engine starts it when it may.</p>
          </div>
        ) : nothing ? (
          <p className="note">{searching ? `No tasks match “${query.trim()}”.` : "Nothing here."}</p>
        ) : (
          <>
            {view.groups.map((group) => {
              const isDone = group.key === "done";
              const headable = !group.flat && !searching;
              return (
                <section key={group.key} className="tasks-group" data-group={group.key}>
                  <h2 className="tasks-group-head" data-top={group.top}>
                    {headable ? (
                      <button
                        type="button"
                        aria-expanded={!group.shut}
                        onClick={() => {
                          if (isDone) {
                            setDoneOpen(false);
                            save("doneopen", false);
                          } else {
                            fold(group.key);
                          }
                        }}
                      >
                        <span className="chev" aria-hidden="true">
                          {group.shut ? "›" : "⌄"}
                        </span>
                        <span className="tasks-group-label">{group.label}</span>
                        <span className="dim">{group.count}</span>
                      </button>
                    ) : (
                      <span className="tasks-group-plain">
                        <span className="tasks-group-label">{group.label}</span>
                        <span className="dim">{group.count}</span>
                      </span>
                    )}
                  </h2>
                  {group.shut ? null : (
                    <ul className="tasks-rows">
                      {group.tasks.map((task) => (
                        <TaskRow key={task.id} task={task} onOpen={onOpen} onReview={onReview} onDrop={onDrop} onVerb={verb} />
                      ))}
                    </ul>
                  )}
                  {!group.shut && isDone && group.more && !searching ? (
                    <button
                      type="button"
                      className="tasks-done-more"
                      onClick={() => {
                        setAllDone(!allDone);
                        save("alldone", !allDone);
                      }}
                    >
                      {allDone ? "Show less" : `Show ${group.cut} more`}
                    </button>
                  ) : null}
                </section>
              );
            })}
            {view.doneLine !== null ? (
              <button
                type="button"
                className="tasks-done-line"
                onClick={() => {
                  setDoneOpen(true);
                  save("doneopen", true);
                }}
              >
                <span className="chev" aria-hidden="true">
                  ›
                </span>
                {view.doneLine.label}
              </button>
            ) : null}
          </>
        )}
      </div>
    </section>
  );
}

/** One filter chip. */
function Chip({
  on,
  count = 0,
  expanded,
  onClick,
  children,
}: {
  on: boolean;
  count?: number;
  expanded?: boolean;
  onClick: () => void;
  children: string;
}) {
  return (
    <button
      type="button"
      className="chip"
      aria-pressed={expanded === undefined ? on : undefined}
      aria-expanded={expanded}
      data-on={on}
      onClick={onClick}
    >
      {children}
      {count > 0 ? <span className="dim">{count}</span> : null}
    </button>
  );
}

/** One task: a two-line row, its glyph at the left, the act at the end. */
function TaskRow({
  task,
  onOpen,
  onReview,
  onDrop,
  onVerb,
}: {
  task: TaskDto;
  onOpen: (id: string) => void;
  onReview: (id: string) => void;
  onDrop?: (id: string) => void;
  onVerb: (task: TaskDto, action: "task.start" | "task.retry" | "task.stop") => void;
}) {
  const shape = look(task);
  const acts = rowActions(task);
  const act = acts.find((a) => a.id !== "drop");
  const drop = acts.find((a) => a.id === "drop");
  return (
    <li className="task-row" data-status={task.derived.status}>
      <button type="button" className="task-row-main" onClick={() => onOpen(task.id)}>
        <span className="task-row-glyph">
          <Glyph shape={shape.glyph} tone={shape.tone} progress={progressOf(task)} label={shape.word} />
        </span>
        <span className="task-row-text">
          <span className="task-row-title">{task.title || task.id}</span>
          <span className="task-row-line">
            <span className="mono dim">{ref(task.id)}</span>
            <span className="task-row-says">{secondLine(task)}</span>
          </span>
        </span>
      </button>
      {act ? (
        <button
          type="button"
          className="pill"
          aria-label={`${act.label} ${task.id}`}
          onClick={() => {
            if (act.action === null) {
              onReview(task.id);
            } else {
              onVerb(task, act.action);
            }
          }}
        >
          {act.label}
        </button>
      ) : null}
      {drop && onDrop ? (
        <button
          type="button"
          className="pill"
          aria-label={`${drop.label} ${task.id}`}
          onClick={() => onDrop(task.id)}
        >
          {drop.label}
        </button>
      ) : null}
    </li>
  );
}

/**
 * The add-task form: a title, the paths it may change, and what must be true when
 * it is done. It says why it cannot be sent yet instead of failing after the press.
 */
function AddTask({
  client,
  known,
  onDone,
  onCancel,
}: {
  client: ApiClient;
  /** The ids of the tasks on the board, which "After" may name. */
  known: readonly string[];
  onDone: () => Promise<void>;
  onCancel: () => void;
}) {
  const [title, setTitle] = useState("");
  const [scope, setScope] = useState("");
  const [acceptance, setAcceptance] = useState("");
  const [after, setAfter] = useState("");
  const [sending, setSending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const id = useId();
  const check = newTask({ title, scope, acceptance, deps: after }, known);

  async function submit(event: FormEvent) {
    event.preventDefault();
    if (sending) {
      return;
    }
    if (!check.ready) {
      setError(check.why);
      return;
    }
    setSending(true);
    setError(null);
    try {
      await client.addTask({
        title: check.title,
        scope: check.scope,
        acceptance: check.acceptance,
        tags: [],
        skills: [],
        deps: check.deps,
      });
      await onDone();
    } catch (failure) {
      setError(reason(failure));
      setSending(false);
    }
  }

  return (
    <form className="add-task" onSubmit={submit} aria-label="New task">
      <p className="add-task-title">New task</p>
      <input
        id={`${id}-title`}
        className="input"
        placeholder="What needs doing"
        aria-label="Title"
        autoFocus
        value={title}
        onChange={(event) => setTitle(event.target.value)}
      />
      <input
        className="input"
        placeholder="src/parser, docs/usage.md"
        aria-label="Paths it may change"
        value={scope}
        onChange={(event) => setScope(event.target.value)}
      />
      <input
        className="input"
        placeholder="The board adds a task; a test says so"
        aria-label="What must be true when it is done"
        value={acceptance}
        onChange={(event) => setAcceptance(event.target.value)}
      />
      <input
        className="input"
        placeholder="After (optional): ids of tasks it waits for, e.g. fix-parser"
        aria-label="After: tasks it waits for"
        value={after}
        onChange={(event) => setAfter(event.target.value)}
      />
      <div className="add-task-foot">
        <span className="add-task-why" data-bad={error !== null} role={error === null ? undefined : "alert"}>
          {error ?? (check.why || "A title, the paths, and what must be true")}
        </span>
        <button type="button" className="plain-button" onClick={onCancel}>
          Cancel
        </button>
        <button type="submit" className="primary-button" disabled={sending}>
          {sending ? "Adding…" : "Add task"}
        </button>
      </div>
    </form>
  );
}
