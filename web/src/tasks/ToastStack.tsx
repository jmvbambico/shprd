// Toasts: the news from the service — a task landed, something needs you, a
// review failed, a pass failed — floated over the work, three at a time.
//
// It mirrors the desktop's float (`host/herdr-desktop/mods/shprd-toasts`): a dot
// in the level's colour, a one-line title, a body at three lines and an ×; a
// toast goes by itself when its time is up. A tap on a toast about a task that
// is ready for review opens the Accept card, one about a task that needs you opens
// its thread, and any other goes to the Tasks list.

import { useEffect, useState } from "react";

import { clip, levelTone, visible, BODY_MAX, type Toast } from "./toasts";

/** How often a toast whose time is up is noticed, ms. */
const EXPIRE_MS = 1000;

/** What the float needs. */
export interface ToastsProps {
  /** The toasts held. */
  toasts: Toast[];
  /** The ids a person closed. */
  dismissed: ReadonlySet<string>;
  /** Closes one. */
  onDismiss: (id: string) => void;
  /** A tap on a toast's words. */
  onOpen: (toast: Toast) => void;
}

/** The float; it draws nothing until there is something to say. */
export function ToastStack({ toasts, dismissed, onDismiss, onOpen }: ToastsProps) {
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    if (toasts.length === 0) {
      return undefined;
    }
    const timer = setInterval(() => setNow(Date.now()), EXPIRE_MS);
    return () => clearInterval(timer);
  }, [toasts.length]);

  const list = visible(toasts, now, dismissed);
  if (list.length === 0) {
    return null;
  }
  return (
    <ul className="toasts" aria-label="Notifications" aria-live="polite">
      {list.map((toast) => (
        <li key={toast.id} className="toast" data-level={toast.level}>
          <span className="toast-dot" data-tone={levelTone(toast.level)} aria-hidden="true" />
          <button type="button" className="toast-main" onClick={() => onOpen(toast)}>
            <span className="toast-title">{toast.title}</span>
            {toast.body !== "" ? <span className="toast-body">{clip(toast.body, BODY_MAX)}</span> : null}
          </button>
          <button type="button" className="toast-x" aria-label="Dismiss" onClick={() => onDismiss(toast.id)}>
            ×
          </button>
        </li>
      ))}
    </ul>
  );
}
