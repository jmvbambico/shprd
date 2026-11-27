// The Accept card: what a person is approving when a task is ready — its
// hand-in, the files its branch changed, the checks it ran and the reviewer's
// findings — with the three verbs the engine has for a task in front of a person.
//
// It mirrors the desktop's Accept dialog (`host/herdr-desktop/mods/shprd-accept`):
// the summary first, then Changes, Checks and Review, a folded Details, and a
// footer of Cancel, Drop task, Send back and the one primary, Accept. A desktop
// gets it as a modal, a phone as a sheet from the bottom.
//
// Accept, Send back and Drop are destructive, so the service may answer
// `{code: "reauth"}`: the app asks for the password in its own dialog, and the
// verb that was refused is sent again once it is given — nothing is half done.
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/accept-card/main.js`, `core/accept.js`
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik

import { useCallback, useEffect, useRef, useState } from "react";

import { ApiError, type ApiClient } from "../api/client";
import type { TaskChangesDto } from "../gen";
import type { Layout } from "../layout/layout";
import {
  WORDS,
  changeCounts,
  changeRows,
  checks,
  proofs,
  commitRows,
  findings,
  reviewLine,
  verbs,
  whereNow,
  words,
} from "./accept";
import { taskById } from "./board";
import { reason, type Board } from "./useBoard";

/** The engine's verbs this card posts. */
type Verb = "task.accept" | "task.retry" | "task.drop";

/** What the card needs. */
export interface AcceptDialogProps {
  /** The one client every screen calls. */
  client: ApiClient;
  /** The live board. */
  board: Board;
  /** The task the card is about. */
  taskId: string;
  /** The layout: a modal on a desktop, a sheet on a phone. */
  device: Layout;
  /** Counts up each time the password was entered again; a refused verb is then sent again. */
  reauthTick: number;
  /** Opens the card on its Drop step, as a row's Drop does. */
  startDropping?: boolean;
  /** Closes the card. */
  onClose: () => void;
}

/** How long the branch's changes are trusted before they are read again, ms. */
const CHANGES_MS = 20_000;

/** The Accept card. */
export function AcceptDialog({
  client,
  board,
  taskId,
  device,
  reauthTick,
  startDropping = false,
  onClose,
}: AcceptDialogProps) {
  const task = taskById(board.state, taskId);
  const [policy, setPolicy] = useState<string | null>(null);
  const [changes, setChanges] = useState<TaskChangesDto | null>(null);
  const [changesError, setChangesError] = useState<string | null>(null);
  const [details, setDetails] = useState(false);
  const [dropping, setDropping] = useState(startDropping);
  const [why, setWhy] = useState("");
  const [busy, setBusy] = useState<Verb | null>(null);
  const [error, setError] = useState<string | null>(null);
  const pending = useRef<{ verb: Verb; extra: { reason?: string } } | null>(null);
  const readAt = useRef(0);

  // The merge policy decides whether an acceptance is a decision at all; a
  // service that cannot say leaves Accept showing and lets the engine refuse it.
  useEffect(() => {
    let live = true;
    client
      .settings()
      .then((settings) => {
        const row = settings.values.find((entry) => entry.key === "tasks.merge");
        if (live) {
          setPolicy(typeof row?.value === "string" ? row.value : null);
        }
      })
      .catch(() => undefined);
    return () => {
      live = false;
    };
  }, [client]);

  // The branch is a git read at the service: asked for when the task or its
  // attempt moves, not on every wake-up.
  const key = task
    ? `${task.id}:${task.attempts.length}:${task.status}:${task.last_status?.seq ?? 0}`
    : taskId;
  useEffect(() => {
    let live = true;
    readAt.current = Date.now();
    setChanges(null);
    client
      .taskChanges(taskId)
      .then((next) => {
        if (live) {
          setChanges(next);
          setChangesError(null);
        }
      })
      .catch((failure: unknown) => {
        if (live) {
          setChanges(null);
          setChangesError(reason(failure));
        }
      });
    return () => {
      live = false;
    };
    // `key` is the task's own moves; CHANGES_MS is the floor for a quiet task.
  }, [client, taskId, key]);

  useEffect(() => {
    // An answer that arrives after the card moved to another task is not that
    // task's changes: only the effect that asked may use it.
    let live = true;
    const timer = setInterval(() => {
      if (Date.now() - readAt.current >= CHANGES_MS) {
        readAt.current = Date.now();
        client
          .taskChanges(taskId)
          .then((next) => {
            if (live) {
              setChanges(next);
            }
          })
          .catch(() => undefined);
      }
    }, CHANGES_MS);
    return () => {
      live = false;
      clearInterval(timer);
    };
  }, [client, taskId]);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        onClose();
      }
    };
    globalThis.addEventListener("keydown", onKey);
    return () => globalThis.removeEventListener("keydown", onKey);
  }, [onClose]);

  const send = useCallback(
    async (verb: Verb, extra: { reason?: string } = {}) => {
      if (busy !== null) {
        return;
      }
      setBusy(verb);
      setError(null);
      pending.current = null;
      try {
        if (verb === "task.drop") {
          await client.action({ kind: "task.drop", task: taskId, reason: extra.reason ?? "" });
        } else {
          await client.action({ kind: verb, task: taskId });
        }
        setBusy(null);
        await board.reload();
        onClose();
      } catch (failure) {
        setBusy(null);
        if (failure instanceof ApiError && failure.code === "reauth") {
          // The app's own dialog is asking for the password; the verb goes again after.
          pending.current = { verb, extra };
          setError("Enter your password to go ahead; nothing was done yet.");
        } else {
          setError(reason(failure));
        }
      }
    },
    [busy, client, taskId, board, onClose],
  );

  // The password was entered again: send the verb that was refused for it.
  const lastTick = useRef(reauthTick);
  useEffect(() => {
    if (reauthTick !== lastTick.current) {
      lastTick.current = reauthTick;
      const again = pending.current;
      if (again !== null) {
        void send(again.verb, again.extra);
      }
    }
  }, [reauthTick, send]);

  const wired = verbs(task, policy);
  const said = words(task);
  const settled = task !== null && (task.status === "integrated" || task.status === "dropped");
  const title = !task ? WORDS.title : wired.accept ? WORDS.question(task.id) : task.id;
  const counts = changeCounts(changes);
  const commits = commitRows(changes);
  const listed = changeRows(changes);
  const checked = checks(task);
  const proved = proofs(task);
  const review = reviewLine(task);
  const found = findings(task);
  const sheet = device === "phone";

  return (
    <div
      className="accept-backdrop"
      data-device={device}
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) {
          onClose();
        }
      }}
    >
      <section
        className={sheet ? "accept sheet" : "accept card"}
        role="dialog"
        aria-modal="true"
        aria-labelledby="accept-title"
      >
        <header className="accept-head">
          <div className="accept-what">
            <h2 id="accept-title" className="accept-title">
              {title}
            </h2>
            {task ? <p className="accept-sub">{task.title}</p> : null}
          </div>
          <button type="button" className="icon-button" aria-label="Close card" onClick={onClose}>
            ×
          </button>
        </header>

        <div className="accept-body">
          {!task ? (
            <p className="dim">{board.error ?? (board.phase === "loading" ? WORDS.reading : whereNow(null))}</p>
          ) : (
            <>
              <p className="accept-summary" data-empty={said.summary === ""}>
                {said.summary === "" ? WORDS.noSummary : said.summary}
              </p>

              <Section label={WORDS.changes}>
                {changesError !== null ? (
                  <p className="dim" role="alert">
                    {changesError}
                  </p>
                ) : counts === null ? (
                  <p className="dim">{changes === null ? WORDS.reading : WORDS.noChanges}</p>
                ) : (
                  <>
                    <p className="accept-counts">
                      {counts.commits !== null ? <span>{counts.commits} · </span> : null}
                      <span>{counts.files}</span> <span className="add">{counts.added}</span>{" "}
                      <span className="del">{counts.removed}</span>
                    </p>
                    {commits.rows.length > 0 ? (
                      <ul className="accept-commits">
                        {commits.rows.map((row) => (
                          <li key={row.sha}>
                            <span className="mono dim">{row.sha}</span>
                            <span className="accept-commit-subject">{row.subject}</span>
                          </li>
                        ))}
                        {commits.rest > 0 ? <li className="dim">{WORDS.moreLine(commits.rest)}</li> : null}
                      </ul>
                    ) : null}
                    <ul className="accept-files">
                      {listed.rows.map((row) => (
                        <li key={row.path}>
                          <span className="mono accept-path">{row.path}</span>
                          <span className="mono dim">{row.stat}</span>
                        </li>
                      ))}
                      {listed.rest > 0 ? <li className="dim">{WORDS.moreLine(listed.rest)}</li> : null}
                    </ul>
                  </>
                )}
              </Section>

              <Section label={WORDS.checks}>
                {checked.rows.length === 0 ? (
                  <>
                    <p className="dim">{WORDS.noChecks}</p>
                    {checked.alsoRan.length > 0 ? (
                      <p className="dim small">It ran: {checked.alsoRan.join(", ")}</p>
                    ) : null}
                  </>
                ) : (
                  <ul className="accept-checks">
                    {checked.rows.map((row, index) => (
                      <li key={`${row.command}:${index}`}>
                        <span className="accept-check-state" data-state={row.state}>
                          {row.word}
                        </span>
                        <span className="mono accept-check-command">{row.command}</span>
                        {row.detail !== "" ? <span className="dim small">{row.detail}</span> : null}
                      </li>
                    ))}
                  </ul>
                )}
              </Section>

              {proved.length > 0 ? (
                <Section label={WORDS.proof}>
                  <ul className="accept-checks accept-proof">
                    {proved.map((row, index) => (
                      <li key={`${row.path}:${index}`}>
                        <span className="mono accept-check-command">{row.name}</span>
                        <span className="dim small mono">{row.path}</span>
                      </li>
                    ))}
                  </ul>
                </Section>
              ) : null}

              <Section label={WORDS.review}>
                <p className={review.degraded ? "accept-review amber" : "accept-review"}>{review.line}</p>
                {found.map((finding, index) => (
                  <div key={`${finding.title}:${index}`} className="accept-finding">
                    <p>
                      <span className="severity" data-tone={finding.tone}>
                        {finding.word}
                      </span>{" "}
                      {finding.title}
                    </p>
                    {finding.location !== null ? <p className="mono dim small">{finding.location}</p> : null}
                    {finding.detail !== "" ? <p className="muted">{finding.detail}</p> : null}
                  </div>
                ))}
              </Section>

              {said.details !== "" ? (
                <section className="accept-section">
                  <button
                    type="button"
                    className="accept-details-toggle"
                    aria-expanded={details}
                    onClick={() => setDetails(!details)}
                  >
                    {details ? "▾" : "▸"} {WORDS.details}
                  </button>
                  {details ? <p className="muted">{said.details}</p> : null}
                </section>
              ) : null}
            </>
          )}
        </div>

        <footer className="accept-foot">
          {dropping ? (
            <div className="accept-drop">
              <input
                className="input"
                aria-label="Why drop it"
                placeholder={WORDS.reasonPlaceholder}
                autoFocus
                value={why}
                onChange={(event) => setWhy(event.target.value)}
              />
            </div>
          ) : null}
          {error !== null ? (
            <p className="accept-error" role="alert">
              {error}
            </p>
          ) : task && settled ? (
            <p className="accept-where">{whereNow(task)}</p>
          ) : wired.note !== null ? (
            <p className="dim small">{wired.note}</p>
          ) : null}
          <div className="accept-buttons">
            {!task || settled ? (
              <button type="button" className="plain-button" onClick={onClose}>
                {WORDS.close}
              </button>
            ) : dropping ? (
              <>
                <button type="button" className="plain-button" onClick={() => setDropping(false)}>
                  {WORDS.cancel}
                </button>
                <button
                  type="button"
                  className="danger-button"
                  disabled={busy !== null || why.trim() === ""}
                  onClick={() => void send("task.drop", { reason: why.trim() })}
                >
                  {busy === "task.drop" ? WORDS.dropping : WORDS.drop}
                </button>
              </>
            ) : (
              <>
                <button type="button" className="plain-button" onClick={onClose}>
                  {WORDS.cancel}
                </button>
                <span className="grow" />
                {wired.drop ? (
                  <button type="button" className="plain-button" onClick={() => setDropping(true)}>
                    {WORDS.drop}
                  </button>
                ) : null}
                {wired.sendBack ? (
                  <button
                    type="button"
                    className="plain-button"
                    disabled={busy !== null}
                    onClick={() => void send("task.retry")}
                  >
                    {busy === "task.retry" ? WORDS.sendingBack : WORDS.sendBack}
                  </button>
                ) : null}
                {wired.accept ? (
                  <button
                    type="button"
                    className="primary-button"
                    disabled={busy !== null || wired.acceptBlocked !== null}
                    title={wired.acceptBlocked ?? undefined}
                    onClick={() => void send("task.accept")}
                  >
                    {busy === "task.accept" ? WORDS.accepting : WORDS.accept}
                  </button>
                ) : null}
              </>
            )}
          </div>
        </footer>
      </section>
    </div>
  );
}

/** One section: a hairline above, a quiet label, then what it says. */
function Section({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <section className="accept-section">
      <h3 className="accept-section-label">{label}</h3>
      {children}
    </section>
  );
}
