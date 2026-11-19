// Agents: one block a role — its name, what it does, and the program, model and
// effort it runs — then the one row that makes another.
//
// Mirrors the Herdr desktop's `shprd-settings` Agents page. The roles and their
// words are the service's answer to `GET /api/settings`; the programs and their
// models are `GET /api/roster`'s. A refused write (a cross-vendor review, say) is
// shown as the service's own sentence.

import { useState } from "react";

import type { SettingsState } from "./useSettings";
import {
  forRepo,
  roles,
  withRole,
  withoutRole,
  type Pick,
  type Role,
  type ScopeId,
} from "./model";
import { cleanTitle } from "../roles";
import { Row } from "./rows";
import { TextBox } from "./TasksPage";

/** The Agents page. */
export function AgentsPage({ state, scope }: { state: SettingsState; scope: ScopeId }) {
  const list = roles(state.settings, state.roster);
  const [open, setOpen] = useState<string | null>(null);
  const [typed, setTyped] = useState<{ key: string; text: string } | null>(null);
  const [adding, setAdding] = useState<string | null>(null);
  const busy = state.writing !== null;

  async function apply(key: string, value: string | string[] | null) {
    const ok = await state.write(key, value, forRepo(scope));
    if (ok) {
      setOpen(null);
      setTyped(null);
    }
  }

  async function addRole() {
    const order = withRole(state.settings, adding ?? "");
    if (!order) {
      state.refuse("roles.order", new Error("a role id is letters, digits, `-` or `_`, and one no role has yet"));
      return;
    }
    if (await state.write("roles.order", order, forRepo(scope))) {
      setAdding(null);
    }
  }

  return (
    <div className="spage" data-page="agents">
      <h2 className="spage-title">Agents</h2>
      {list.map((role) => (
        <RoleBlock
          key={role.id}
          role={role}
          scope={scope}
          open={open}
          typed={typed}
          busy={busy}
          writing={state.writing}
          onToggle={(key) => {
            setOpen(open === key ? null : key);
            setTyped(null);
            state.clearNotices();
          }}
          onApply={(key, value) => void apply(key, value)}
          onTyped={setTyped}
          onName={(text) => void apply(role.name.key, cleanTitle(text) === "" ? null : cleanTitle(text))}
          restart={state.restartCoordinator}
          onRemove={() => void apply("roles.order", withoutRole(state.settings, role.id))}
        />
      ))}
      {adding === null ? (
        <button type="button" className="link add-role" onClick={() => setAdding("")}>
          Add a role
        </button>
      ) : (
        <form
          className="add-form"
          onSubmit={(event) => {
            event.preventDefault();
            void addRole();
          }}
        >
          <input
            className="input"
            aria-label="Role id"
            placeholder="role id, e.g. docs"
            autoFocus
            value={adding}
            onChange={(event) => setAdding(event.target.value)}
          />
          <button type="submit" className="link" disabled={busy}>
            Add
          </button>
          <button type="button" className="link" onClick={() => setAdding(null)}>
            Cancel
          </button>
        </form>
      )}
    </div>
  );
}

function RoleBlock({
  role,
  scope,
  open,
  typed,
  busy,
  writing,
  onToggle,
  onApply,
  onTyped,
  onName,
  restart,
  onRemove,
}: {
  role: Role;
  scope: ScopeId;
  open: string | null;
  typed: { key: string; text: string } | null;
  busy: boolean;
  writing: string | null;
  onToggle: (key: string) => void;
  onApply: (key: string, value: string | null) => void;
  onTyped: (typed: { key: string; text: string } | null) => void;
  /** The name box was left or Enter pressed: write it. */
  onName: (text: string) => void;
  /** Restarts the coordinator; the coordinator's block offers it. */
  restart: () => Promise<string | null>;
  onRemove: () => void;
}) {
  const [restarting, setRestarting] = useState<"working" | "done" | string | null>(null);
  async function restartNow() {
    setRestarting("working");
    const failed = await restart();
    setRestarting(failed === null ? "done" : `failed:${failed}`);
  }
  return (
    <section className="role" data-role={role.id} aria-label={role.title}>
      <header className="role-head">
        <span className="role-title">{role.title}</span>
        <span className="role-kind">{role.kind}</span>
        {role.custom ? <span className="badge">custom</span> : null}
        {role.readonly ? <span className="role-kind">reads only</span> : null}
        <span className="grow" />
        {role.custom ? (
          <button type="button" className="link" disabled={busy} onClick={onRemove}>
            Remove
          </button>
        ) : null}
      </header>
      {role.about !== "" ? <p className="role-about">{role.about}</p> : null}
      <Row
        id={role.name.key}
        title="Name"
        about={`What the app calls this role. Empty goes back to “${role.name.fallback}”.`}
        scopeId={scope}
        scope={{ key: role.name.key, layer: role.name.layer, mine: role.name.source === "repo_entry" }}
        onReset={() => onApply(role.name.key, null)}
        busy={busy}
        below={<TextBox label={`${role.title} name`} value={role.name.value} rows={1} debounce={false} onCommit={onName} />}
      />
      {role.picks.map((pick) => {
        const key = `${role.id}.${pick.field}`;
        return (
          <PickRow
            key={key}
            pick={pick}
            scope={scope}
            isOpen={open === key}
            typed={typed?.key === pick.key ? typed.text : null}
            busy={busy}
            saving={writing === pick.key}
            onToggle={() => onToggle(key)}
            onApply={(value) => onApply(pick.key, value)}
            onTyped={(text) => onTyped(text === null ? null : { key: pick.key, text })}
          />
        );
      })}
      {role.kind === "coordinator" ? (
        <Row
          id="restart-coordinator"
          scopeId={scope}
          title="Restart the coordinator"
          about="A new program or model reaches the coordinator when it next starts."
          value={restarting === "working" ? "Restarting…" : restarting === "done" ? "Restarted" : undefined}
          control={
            <button type="button" className="link" disabled={restarting === "working"} onClick={() => void restartNow()}>
              Restart the coordinator now
            </button>
          }
          below={
            restarting?.startsWith("failed:") ? (
              <div className="refusal" role="alert">
                {restarting.slice(7)}
              </div>
            ) : null
          }
        />
      ) : null}
    </section>
  );
}

function PickRow({
  pick,
  scope,
  isOpen,
  typed,
  busy,
  saving,
  onToggle,
  onApply,
  onTyped,
}: {
  pick: Pick;
  scope: ScopeId;
  isOpen: boolean;
  typed: string | null;
  busy: boolean;
  saving: boolean;
  onToggle: () => void;
  onApply: (value: string | null) => void;
  onTyped: (text: string | null) => void;
}) {
  return (
    <Row
      id={pick.key}
      title={pick.label}
      about={pick.about}
      value={pick.display}
      scopeId={scope}
      scope={{ key: pick.key, layer: pick.layer, mine: pick.source === "repo_entry" }}
      onReset={() => onApply(null)}
      busy={busy}
      control={
        <span className="pick-control">
          {saving ? <span className="saving">saving…</span> : null}
          <button
            type="button"
            className="pick-button"
            aria-expanded={isOpen}
            aria-label={`Change ${pick.label.toLowerCase()} (${pick.key})`}
            onClick={onToggle}
          >
            {isOpen ? "▴" : "▾"}
          </button>
        </span>
      }
      below={
        isOpen ? (
          <div className="choices" role="listbox" aria-label={`${pick.label} choices`}>
            {pick.choices.length === 0 ? (
              <div className="choice-note">{pick.note ?? "nothing to pick"}</div>
            ) : null}
            {pick.choices.map((choice) => {
              const current = (pick.value ?? null) === choice.value;
              return (
                <button
                  key={choice.value ?? "default"}
                  type="button"
                  role="option"
                  aria-selected={current}
                  className="choice"
                  disabled={busy}
                  onClick={() => onApply(choice.value)}
                >
                  <span className={choice.value === null ? "choice-label muted" : "choice-label"}>
                    {choice.label}
                  </span>
                  {choice.hint ? <span className="choice-hint">{choice.hint}</span> : null}
                  {current ? <span className="choice-check">✓</span> : null}
                </button>
              );
            })}
            {pick.field !== "agent" ? (
              typed !== null ? (
                <form
                  className="typed-form"
                  onSubmit={(event) => {
                    event.preventDefault();
                    if (typed.trim() !== "") {
                      onApply(typed.trim());
                    }
                  }}
                >
                  <input
                    className="input"
                    aria-label="Type an id"
                    placeholder="Type an id"
                    autoFocus
                    value={typed}
                    onChange={(event) => onTyped(event.target.value)}
                  />
                  <button type="submit" className="link" disabled={busy}>
                    Use
                  </button>
                </form>
              ) : (
                <button type="button" className="link typed-link" onClick={() => onTyped("")}>
                  Use an id typed
                </button>
              )
            ) : null}
            {pick.note && pick.choices.length > 0 ? (
              <div className="choice-note">{pick.note}</div>
            ) : null}
          </div>
        ) : null
      }
    />
  );
}
