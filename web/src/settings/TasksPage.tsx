// Tasks: the workflow keys `GET /api/settings` exposes, one row each, in the
// core's order. A pick, a switch, a step or leaving a box writes at once; there
// is no Save button (Shepherdr's rule for every page of its settings).

import { useEffect, useRef, useState } from "react";

import type { SettingsState } from "./useSettings";
import { forRepo, stepped, workflowRows, type ScopeId, type WorkflowRow } from "./model";
import { Dropdown } from "./Dropdown";
import { Row, Switch } from "./rows";

/** How long a note waits after the last key before it saves itself. */
export const NOTE_MS = 500;

/** The Tasks page. */
export function TasksPage({ state, scope }: { state: SettingsState; scope: ScopeId }) {
  const rows = workflowRows(state.settings);
  const busy = state.writing !== null;
  return (
    <div className="spage" data-page="tasks">
      <h2 className="spage-title">Tasks</h2>
      {rows.map((row) => (
        <WorkflowRowView
          key={row.key}
          row={row}
          scope={scope}
          busy={busy}
          saving={state.writing === row.key}
          write={(value) => void state.write(row.key, value, forRepo(scope))}
        />
      ))}
    </div>
  );
}

function WorkflowRowView({
  row,
  scope,
  busy,
  saving,
  write,
}: {
  row: WorkflowRow;
  scope: ScopeId;
  busy: boolean;
  saving: boolean;
  write: (value: string | number | boolean | null) => void;
}) {
  let control: React.ReactNode = null;
  let below: React.ReactNode = null;
  if (row.kind === "choice") {
    control = (
      <Dropdown
        label={row.title}
        value={typeof row.value === "string" ? row.value : null}
        options={(row.choices ?? []).map(([id, word]) => ({ id, label: word }))}
        disabled={busy}
        onPick={write}
      />
    );
  } else if (row.kind === "switch") {
    control = (
      <Switch
        label={row.title}
        on={row.value === true}
        disabled={busy}
        onToggle={() => write(row.value !== true)}
      />
    );
  } else if (row.kind === "number") {
    control = (
      <span className="stepper">
        <button
          type="button"
          className="step"
          aria-label={`Less: ${row.title}`}
          disabled={busy || (row.min !== null && Number(row.value ?? row.min) <= row.min)}
          onClick={() => write(stepped(row.value, -1, row.key))}
        >
          −
        </button>
        <span className="step-value">{String(row.value ?? row.min ?? 0)}</span>
        <button
          type="button"
          className="step"
          aria-label={`More: ${row.title}`}
          disabled={busy || (row.max !== null && Number(row.value ?? row.min ?? 0) >= row.max)}
          onClick={() => write(stepped(row.value, 1, row.key))}
        >
          +
        </button>
      </span>
    );
  } else {
    below = (
      <TextBox
        label={row.title}
        value={typeof row.value === "string" ? row.value : ""}
        rows={row.kind === "notes" ? 3 : 1}
        debounce={row.kind === "notes"}
        onCommit={(text) => write(text.trim() === "" ? null : text)}
      />
    );
  }
  return (
    <Row
      id={row.key}
      title={row.title}
      about={row.about}
      scopeId={scope}
      scope={{ key: row.key, layer: row.layer, mine: row.source === "repo_entry" }}
      onReset={() => write(null)}
      busy={busy}
      control={
        control === null ? (
          saving ? <span className="saving">saving…</span> : null
        ) : (
          <span className="pick-control">
            {saving ? <span className="saving">saving…</span> : null}
            {control}
          </span>
        )
      }
      below={below}
    />
  );
}

/**
 * A box under a row: writes when the person leaves it (or presses Enter in a
 * line) and, for notes, a moment after typing stops. A value that came from the
 * service never overwrites a box the person is in.
 */
export function TextBox({
  label,
  value,
  rows,
  debounce,
  onCommit,
}: {
  label: string;
  value: string;
  rows: number;
  debounce: boolean;
  onCommit: (text: string) => void;
}) {
  const [draft, setDraft] = useState(value);
  const focused = useRef(false);
  const saved = useRef(value);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const latest = useRef(onCommit);
  latest.current = onCommit;

  useEffect(() => {
    if (!focused.current) {
      setDraft(value);
      saved.current = value;
    }
  }, [value]);

  useEffect(
    () => () => {
      if (timer.current !== null) {
        clearTimeout(timer.current);
      }
    },
    [],
  );

  function commit(text: string) {
    if (timer.current !== null) {
      clearTimeout(timer.current);
      timer.current = null;
    }
    if (text !== saved.current) {
      saved.current = text;
      latest.current(text);
    }
  }

  const common = {
    "aria-label": label,
    className: "input box",
    value: draft,
    onFocus: () => {
      focused.current = true;
    },
    onBlur: () => {
      focused.current = false;
      commit(draft);
    },
  };
  return rows > 1 ? (
    <textarea
      {...common}
      rows={rows}
      onChange={(event) => {
        setDraft(event.target.value);
        if (debounce) {
          if (timer.current !== null) {
            clearTimeout(timer.current);
          }
          const text = event.target.value;
          timer.current = setTimeout(() => commit(text), NOTE_MS);
        }
      }}
    />
  ) : (
    <input
      {...common}
      type="text"
      onChange={(event) => setDraft(event.target.value)}
      onKeyDown={(event) => {
        if (event.key === "Enter") {
          commit(draft);
        }
      }}
    />
  );
}
