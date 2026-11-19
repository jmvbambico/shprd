// Agents, in Shepherdr's shape: one compact row a role — its name (click it to
// rename in place) and one line about it, a single "<Model> · <Effort>" button that
// opens the model menu, and Edit instructions — then the Backup agent switch.
//
// Mirrors the Herdr desktop's `shprd-settings` Agents page. The roles and their
// words are the service's answer to `GET /api/settings`; the programs and their
// models are `GET /api/roster`'s, and the model menu's tabs are only the programs
// installed on this machine. A refused write (a cross-vendor review, say) is shown
// as the service's own sentence.

import { useRef, useState } from "react";

import type { SettingsState } from "./useSettings";
import {
  forRepo,
  roles,
  withRole,
  withoutRole,
  type Role,
  type ScopeId,
} from "./model";
import { cleanTitle } from "../roles";
import { ModelMenu } from "./Dropdown";
import {
  backupChoice,
  menusOf,
  otherCli,
  repoKeysOf,
  roleSelection,
  selectionLabel,
  selectionWrites,
  type Selection,
} from "./menu";
import { Switch } from "./rows";

/** The Agents page. */
export function AgentsPage({
  state,
  scope,
  onEditInstructions,
}: {
  state: SettingsState;
  scope: ScopeId;
  /** Opens a role's prompt on the Prompts page. */
  onEditInstructions: (promptId: string) => void;
}) {
  const list = roles(state.settings, state.roster);
  const [adding, setAdding] = useState<string | null>(null);
  const busy = state.writing !== null;
  const menus = menusOf(state.roster);

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
      {list.map((role) =>
        role.id === "backup" ? (
          <BackupRow key={role.id} role={role} state={state} scope={scope} busy={busy} />
        ) : (
          <RoleRow
            key={role.id}
            role={role}
            state={state}
            scope={scope}
            busy={busy}
            menus={menus}
            onEditInstructions={onEditInstructions}
          />
        ),
      )}
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

/** The role's name: click it (or the pencil) to edit in place; Enter saves, Esc cancels, empty is the built-in name. */
function RoleName({
  role,
  onCommit,
}: {
  role: Role;
  onCommit: (text: string) => void;
}) {
  const [editing, setEditing] = useState(false);
  const [text, setText] = useState("");
  const cancelled = useRef(false);

  function start() {
    cancelled.current = false;
    setText(role.title);
    setEditing(true);
  }
  function finish() {
    if (!cancelled.current && text !== role.title) {
      onCommit(text);
    }
    setEditing(false);
  }
  if (editing) {
    return (
      <input
        className="input role-name-input"
        aria-label={`${role.title} name`}
        placeholder={role.name.fallback}
        autoFocus
        value={text}
        onChange={(event) => setText(event.target.value)}
        onBlur={finish}
        onKeyDown={(event) => {
          if (event.key === "Enter") {
            event.preventDefault();
            event.currentTarget.blur();
          } else if (event.key === "Escape") {
            // Esc leaves the field and writes nothing; it does not close the window.
            event.stopPropagation();
            cancelled.current = true;
            event.currentTarget.blur();
          }
        }}
      />
    );
  }
  return (
    <button type="button" className="role-name" aria-label={`Rename ${role.title}`} onClick={start}>
      <span className="role-title">{role.title}</span>
      <span className="role-pencil" aria-hidden="true">
        ✎
      </span>
    </button>
  );
}

function RoleRow({
  role,
  state,
  scope,
  busy,
  menus,
  onEditInstructions,
}: {
  role: Role;
  state: SettingsState;
  scope: ScopeId;
  busy: boolean;
  menus: ReturnType<typeof menusOf>;
  onEditInstructions: (promptId: string) => void;
}) {
  const repo = forRepo(scope);
  const sel = roleSelection(state.settings, role.id);
  const mine = repoKeysOf(state.settings, role.id).length > 0;
  const other = role.id === "reviewer" ? otherCli(state.settings, state.roster) : null;
  const [restarting, setRestarting] = useState<"working" | "done" | string | null>(null);

  async function choose(next: Selection) {
    await state.writeAll(selectionWrites(role.id, sel, next), repo);
  }
  async function restartNow() {
    setRestarting("working");
    const failed = await state.restartCoordinator();
    setRestarting(failed === null ? "done" : `failed:${failed}`);
    if (failed === null) {
      setTimeout(() => setRestarting((now) => (now === "done" ? null : now)), 2000);
    }
  }
  const loading = state.roster === null;
  const none = !loading && menus.order.length === 0;
  const showNotice = role.kind === "coordinator" && (state.restartNotice || restarting !== null);

  return (
    <section className="role-row" data-role={role.id} aria-label={role.title}>
      <div className="role-main">
        <div className="srow-words">
          <div className="role-line">
            <RoleName
              role={role}
              onCommit={(text) => void state.write(role.name.key, cleanTitle(text) === "" ? null : cleanTitle(text), repo)}
            />
            {role.readonly ? <span className="role-kind">reads only</span> : null}
            {role.custom ? <span className="badge">custom</span> : null}
            {mine ? <span className="badge">repo</span> : null}
          </div>
          {role.about !== "" ? <div className="srow-about">{role.about}</div> : null}
        </div>
        <div className="role-controls">
          {mine && scope === "repo" ? (
            <button
              type="button"
              className="link"
              aria-label={`Reset ${role.id}`}
              disabled={busy}
              onClick={() => {
                void state.writeAll(
                  repoKeysOf(state.settings, role.id).map((key) => ({ key, value: null })),
                  true,
                );
              }}
            >
              Reset
            </button>
          ) : null}
          <ModelMenu
            label={`${role.title} model`}
            shown={loading ? "Loading…" : none ? "No program found" : selectionLabel(menus, sel, state.roster)}
            menus={menus}
            sel={sel}
            disabled={loading || none}
            onChange={(next) => void choose(next)}
          />
          {other !== null ? (
            <button
              type="button"
              className="btn plain"
              disabled={busy}
              onClick={() =>
                void choose({ agent: other, model: null, effort: null, fast: false, perm: null })
              }
            >
              Use other CLI
            </button>
          ) : null}
          {role.promptId ? (
            <button type="button" className="btn" onClick={() => onEditInstructions(role.promptId!)}>
              Edit instructions
            </button>
          ) : null}
          {role.custom ? (
            <button
              type="button"
              className="btn plain"
              disabled={busy}
              onClick={() => void state.write("roles.order", withoutRole(state.settings, role.id), repo)}
            >
              Remove
            </button>
          ) : null}
        </div>
      </div>
      {showNotice ? (
        <div className="restart-notice" role="status">
          <span className={restarting?.startsWith("failed:") ? "tone-red" : ""}>
            {restarting === "working"
              ? "Restarting…"
              : restarting === "done"
                ? "Restarted"
                : restarting?.startsWith("failed:")
                  ? restarting.slice(7)
                  : "Applies when the coordinator next starts."}
          </span>
          {restarting !== "working" && restarting !== "done" ? (
            <button type="button" className="link" onClick={() => void restartNow()}>
              Restart the coordinator now
            </button>
          ) : null}
        </div>
      ) : null}
    </section>
  );
}

function BackupRow({
  role,
  state,
  scope,
  busy,
}: {
  role: Role;
  state: SettingsState;
  scope: ScopeId;
  busy: boolean;
}) {
  const menus = menusOf(state.roster);
  const sel = roleSelection(state.settings, role.id);
  const on = sel.agent !== null;
  const repo = forRepo(scope);
  async function flip() {
    if (on) {
      await state.write("roles.backup.agent", null, repo);
      return;
    }
    const program = backupChoice(state.settings, state.roster);
    if (program === null) {
      state.refuse("roles.backup.agent", new Error("there is no other program to take a task over"));
      return;
    }
    await state.write("roles.backup.agent", program, repo);
  }
  return (
    <section className="role-row" data-role={role.id} aria-label={role.title}>
      <div className="role-main">
        <div className="srow-words">
          <div className="role-line">
            <span className="role-title">{role.title}</span>
          </div>
          <div className="srow-about">The other CLI takes a task over once when its agent can't go on.</div>
        </div>
        <div className="role-controls">
          {on ? (
            <ModelMenu
              label={`${role.title} model`}
              shown={selectionLabel(menus, sel, state.roster)}
              menus={menus}
              sel={sel}
              onChange={(next) => void state.writeAll(selectionWrites(role.id, sel, next), repo)}
            />
          ) : null}
          <Switch label={role.title} on={on} disabled={busy} onToggle={() => void flip()} />
        </div>
      </div>
    </section>
  );
}
