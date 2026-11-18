// The board, live: one read of the run (`/api/state`, `/api/asks`), kept fresh
// by the change feed, with the toasts its events ask for.
//
// One hook feeds the whole shell — the list, the thread, the needs-you column
// and the toasts — so a change is read once and every view agrees about it. A
// failed read keeps what is drawn and says so; it never blanks the board.

import { useCallback, useEffect, useRef, useState } from "react";

import type { ApiClient } from "../api/client";
import { ApiError } from "../api/client";
import { useLiveRefresh } from "../api/useLive";
import type { AskListDto, ChangeDto, SnapshotDto } from "../gen";
import { dismiss, fromSnapshot, mergeNews, type Toast } from "./toasts";

/** What a screen can show about the board. */
export interface Board {
  /** `loading` until the first answer, then `ready`, or `error` when there never was one. */
  phase: "loading" | "ready" | "error";
  /** The folded run, once read. */
  state: SnapshotDto | null;
  /** The asks waiting for an answer. */
  asks: AskListDto | null;
  /** Why the last read failed, in the service's words. */
  error: string | null;
  /** Reads the board again now. */
  reload: () => Promise<void>;
  /** The toasts held, newest last (the view filters by time). */
  toasts: Toast[];
  /** The toast ids a person closed. */
  dismissed: ReadonlySet<string>;
  /** Closes one toast. */
  dismissToast: (id: string) => void;
}

/** A failure in words a person can read. */
export function reason(error: unknown): string {
  if (error instanceof ApiError) {
    return error.status === 0 ? "Can't reach shprd. Is the service running?" : error.message;
  }
  return error instanceof Error ? error.message : "Something went wrong.";
}

/** Follows the run for as long as the caller is mounted. */
export function useBoard(client: ApiClient): Board {
  const [state, setState] = useState<SnapshotDto | null>(null);
  const [asks, setAsks] = useState<AskListDto | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [toasts, setToasts] = useState<Toast[]>([]);
  const [dismissed, setDismissed] = useState<ReadonlySet<string>>(new Set());
  const loadId = useRef(0);
  const alive = useRef(true);

  const absorb = useCallback((events: ChangeDto["events"], snapshot: SnapshotDto) => {
    const items = fromSnapshot(events, snapshot);
    if (items.length > 0) {
      setToasts((list) => mergeNews(list, items));
    }
  }, []);

  const load = useCallback(
    async (events: ChangeDto["events"] = []) => {
      const id = ++loadId.current;
      try {
        const [next, nextAsks] = await Promise.all([
          client.state(),
          // The asks are a second file read; a failure of it must not blank the board.
          client.asks().catch(() => null),
        ]);
        if (!alive.current) {
          return;
        }
        // A superseded read still tells its events: their toasts are news even
        // when a newer read has already drawn the board.
        absorb(events, next);
        if (id !== loadId.current) {
          return;
        }
        setState(next);
        setAsks((held) => nextAsks ?? held);
        setError(null);
      } catch (failure) {
        if (alive.current && id === loadId.current) {
          setError(reason(failure));
        }
      }
    },
    [client, absorb],
  );

  useEffect(() => {
    alive.current = true;
    void load();
    return () => {
      alive.current = false;
    };
  }, [load]);

  useLiveRefresh(client, (change) => {
    void load(change.events);
  });

  const reload = useCallback(() => load(), [load]);
  const dismissToast = useCallback((id: string) => {
    setDismissed((set) => new Set(set).add(id));
    setToasts((list) => dismiss(list, id));
  }, []);

  return {
    phase: state !== null ? "ready" : error !== null ? "error" : "loading",
    state,
    asks,
    error,
    reload,
    toasts,
    dismissed,
    dismissToast,
  };
}
