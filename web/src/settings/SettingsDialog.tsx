// Settings, as the Herdr window has it: a list of pages (Agents, Tasks, Prompts,
// Remote) and the page beside it, the "Applies to" switch over each page a
// repository can own, and no Save step anywhere.
//
// On a desktop it opens as a modal over the app; on a phone it is a full-screen
// sheet whose first screen is the page list. It is not a column of cards.

import { useEffect, useId, useRef, useState } from "react";

import type { ApiClient } from "../api/client";
import { useLayout } from "../layout/useLayout";
import { AgentsPage } from "./AgentsPage";
import {
  LEVEL,
  PAGES,
  SCOPES,
  TITLE,
  health,
  levelNote,
  type Page,
  type ScopeId,
} from "./model";
import { PromptsPage } from "./PromptsPage";
import { RemotePage } from "./RemotePage";
import { Dropdown } from "./Dropdown";
import { TasksPage } from "./TasksPage";
import { useSettings } from "./useSettings";

/** What the dialog needs. */
export interface SettingsDialogProps {
  /** The one client every screen calls. */
  client: ApiClient;
  /** Closes the dialog. */
  onClose: () => void;
  /** The person logged out from the Remote page. */
  onSignedOut: () => void;
  /** The page to open on (desktop widths), `agents` when absent. */
  page?: Page["id"];
}

/** The settings dialog. */
export function SettingsDialog({ client, onClose, onSignedOut, page: first }: SettingsDialogProps) {
  const layout = useLayout();
  const phone = layout === "phone";
  const [page, setPage] = useState<Page["id"]>(first ?? "agents");
  // On a phone the page list is the first screen; a page is a second one.
  const [listing, setListing] = useState(first === undefined);
  const [scope, setScope] = useState<ScopeId>("you");
  const state = useSettings(client);
  const titleId = useId();
  const panel = useRef<HTMLDivElement>(null);

  useEffect(() => {
    panel.current?.focus();
  }, []);

  useEffect(() => {
    function onKey(event: KeyboardEvent) {
      if (event.key === "Escape") {
        onClose();
      }
    }
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [onClose]);

  const current = PAGES.find((entry) => entry.id === page) ?? PAGES[0]!;
  const showList = !phone || listing;
  const showPage = !phone || !listing;

  function pick(id: Page["id"]) {
    setPage(id);
    setListing(false);
    state.clearNotices();
  }

  return (
    <div
      className="settings-backdrop"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget && !phone) {
          onClose();
        }
      }}
    >
      <div
        ref={panel}
        tabIndex={-1}
        className="settings"
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        data-layout={layout}
        data-view={phone ? (listing ? "list" : "page") : "both"}
      >
        <header className="settings-head">
          {phone && !listing ? (
            <button type="button" className="link" onClick={() => setListing(true)}>
              ‹ {TITLE}
            </button>
          ) : null}
          <h1 id={titleId} className="settings-title">
            {phone && !listing ? current.title : TITLE}
          </h1>
          <span className="grow" />
          {state.writing !== null ? <span className="saving">saving…</span> : null}
          <button type="button" className="settings-close" aria-label="Close settings" onClick={onClose}>
            ×
          </button>
        </header>

        <div className="settings-split">
          {showList ? (
            <nav className="settings-pages" aria-label="Settings pages">
              <ul>
                {PAGES.map((entry) => (
                  <li key={entry.id}>
                    <button
                      type="button"
                      className="settings-page-button"
                      aria-current={!phone && entry.id === page ? "page" : undefined}
                      onClick={() => pick(entry.id)}
                    >
                      <span>{entry.title}</span>
                      {phone ? (
                        <span className="prompt-go" aria-hidden="true">
                          ›
                        </span>
                      ) : null}
                    </button>
                  </li>
                ))}
              </ul>
            </nav>
          ) : null}

          {showPage ? (
            <div className="settings-body">
              <Notices state={state} />
              {current.id === "remote" ? null : <LevelBar page={current} scope={scope} onScope={setScope} state={state} />}
              {state.failure !== null && current.id !== "remote" ? (
                <p className="snote">
                  {state.failure}. Start it with `shprd serve`; this page fills in when it answers.
                </p>
              ) : null}
              <PageBody
                client={client}
                page={current.id}
                scope={scope}
                state={state}
                onSignedOut={onSignedOut}
              />
            </div>
          ) : null}
        </div>
      </div>
    </div>
  );
}

function PageBody({
  client,
  page,
  scope,
  state,
  onSignedOut,
}: {
  client: ApiClient;
  page: Page["id"];
  scope: ScopeId;
  state: ReturnType<typeof useSettings>;
  onSignedOut: () => void;
}) {
  if (page === "remote") {
    return <RemotePage client={client} onSignedOut={onSignedOut} />;
  }
  if (page === "prompts") {
    return <PromptsPage client={client} state={state} />;
  }
  if (state.settings === null) {
    return state.failure === null ? <p className="snote">Reading the settings…</p> : null;
  }
  return page === "tasks" ? (
    <TasksPage state={state} scope={scope} />
  ) : (
    <AgentsPage state={state} scope={scope} />
  );
}

/** The refusal (the service's own sentence), the last write's word, and the checks. */
function Notices({ state }: { state: ReturnType<typeof useSettings> }) {
  const checks = health(state.settings);
  return (
    <div className="notices">
      {state.refusal !== null ? (
        <div className="refusal" role="alert">
          <div className="refusal-key">{state.refusal.key}</div>
          <div className="refusal-text">{state.refusal.text}</div>
        </div>
      ) : null}
      {checks.problems.map((problem) => (
        <p key={problem} className="problem">
          {problem}
        </p>
      ))}
      {checks.notChecked !== null ? (
        <p className="problem tone-amber">the roles were not checked: {checks.notChecked}</p>
      ) : null}
    </div>
  );
}

/** `Applies to: You | This repo`, at the top of each page a repository can own. */
function LevelBar({
  page,
  scope,
  onScope,
  state,
}: {
  page: Page;
  scope: ScopeId;
  onScope: (scope: ScopeId) => void;
  state: ReturnType<typeof useSettings>;
}) {
  if (!page.repo) {
    return <p className="level-one">{LEVEL.one}</p>;
  }
  const repository = state.settings?.repo ?? null;
  return (
    <div className="level">
      <div className="level-line">
        <span className="level-label">{LEVEL.label}</span>
        <Dropdown
          label={LEVEL.label}
          value={scope}
          options={SCOPES.map((entry) => ({
            id: entry.id,
            label: entry.id === "repo" ? "This repo" : "Everywhere",
            ...(entry.id === "repo" && repository !== null ? { hint: repository } : {}),
          }))}
          onPick={(id) => {
            onScope(id as ScopeId);
            state.clearNotices();
          }}
        />
        <span className="grow" />
      </div>
      <p className="level-note">{levelNote(scope, repository)}</p>
    </div>
  );
}
