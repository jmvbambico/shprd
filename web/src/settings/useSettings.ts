// The settings dialog's state: what the service resolved, the writes in flight,
// and the refusal a write got.
//
// Every write goes through one queue, one key at a time, because the service
// takes one key per call and a second answer would overwrite the first's
// settings. A refusal is the service's own sentence, kept verbatim: a
// cross-vendor review is refused in words that name both roles and both vendors,
// and a person needs exactly those words.

import { useCallback, useEffect, useRef, useState } from "react";

import { ApiError, type ApiClient } from "../api/client";
import type { PromptListDto, RosterDto, SettingsDto } from "../gen";
import { outcomeOf, type SettingValue } from "./model";
import { coordinatorRunning, needsCoordinatorRestart, priorInScope, restartNotice, type Write } from "./menu";

/** How often the dialog re-reads the service, so another window's write shows. */
export const POLL_MS = 10_000;

/** How long "Saved" stays beside a write. */
export const SAVED_MS = 2100;

/** A refused write: the key it was about, and the service's words. */
export interface Refusal {
  /** The key (or `prompts.<id>`) that was refused. */
  key: string;
  /** The service's sentence, unedited. */
  text: string;
}

/** The words of a failure, for the person. */
export function reason(error: unknown): string {
  if (error instanceof ApiError) {
    return error.status === 0 ? "shprd serve is not running" : error.message;
  }
  return error instanceof Error ? error.message : "something went wrong";
}

/** What the dialog reads and does. */
export interface SettingsState {
  /** The resolved settings, or null before the first answer. */
  settings: SettingsDto | null;
  /** The programs and their models. */
  roster: RosterDto | null;
  /** The prompt list. */
  prompts: PromptListDto | null;
  /** Why nothing could be read, when nothing could. */
  failure: string | null;
  /** The last refused write. */
  refusal: Refusal | null;
  /** What the last accepted write did, in words. */
  saved: string | null;
  /** The key being written now. */
  writing: string | null;
  /**
   * Writes one key; `null` removes it so it inherits again.
   *
   * Resolves once the write has been answered, whether stored or refused.
   */
  write: (key: string, value: SettingValue, forRepo: boolean, undo?: boolean) => Promise<boolean>;
  /**
   * Writes several keys as one choice (program, model, level…), stopping at the
   * first the service refuses, so a role is never left half switched.
   */
  writeAll: (writes: Write[], forRepo: boolean) => Promise<boolean>;
  /** Whether the inline "restart the coordinator" notice shows. */
  restartNotice: boolean;
  /** Clears the refusal and the saved word. */
  clearNotices: () => void;
  /** Restarts the coordinator; the service's sentence when it cannot, else null. */
  restartCoordinator: () => Promise<string | null>;
  /** Reads the prompt list again. */
  reloadPrompts: () => Promise<void>;
  /** Sets the prompt list directly (after an edit or a reset). */
  setPrompts: (list: PromptListDto) => void;
  /** Records a refusal from outside the settings keys (a prompt write). */
  refuse: (key: string, error: unknown) => void;
  /** Records a saved word from outside the settings keys. */
  say: (text: string) => void;
}

/** Reads the settings, the roster and the prompt list, and owns the write queue. */
export function useSettings(client: ApiClient): SettingsState {
  const [settings, setSettingsState] = useState<SettingsDto | null>(null);
  // The latest settings, for a choice that needs what a write left behind before its next one.
  const settingsRef = useRef<SettingsDto | null>(null);
  const setSettings = useCallback((next: SettingsDto | null) => {
    settingsRef.current = next;
    setSettingsState(next);
  }, []);
  const [roster, setRoster] = useState<RosterDto | null>(null);
  const [prompts, setPrompts] = useState<PromptListDto | null>(null);
  const [failure, setFailure] = useState<string | null>(null);
  const [refusal, setRefusal] = useState<Refusal | null>(null);
  const [saved, setSaved] = useState<string | null>(null);
  const [writing, setWriting] = useState<string | null>(null);
  const [coordChanged, setCoordChanged] = useState(false);
  const [coordRunning, setCoordRunning] = useState(false);
  const running = useRef(false);
  const tail = useRef<Promise<unknown>>(Promise.resolve());
  const pending = useRef(0);
  const alive = useRef(true);
  const savedTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
      if (savedTimer.current !== null) {
        clearTimeout(savedTimer.current);
      }
    };
  }, []);

  const say = useCallback((text: string) => {
    setSaved(text);
    if (savedTimer.current !== null) {
      clearTimeout(savedTimer.current);
    }
    savedTimer.current = setTimeout(() => {
      if (alive.current) {
        setSaved(null);
      }
    }, SAVED_MS);
  }, []);

  // The three reads are independent on purpose: the roster asks each program
  // for its models and can take seconds, and the settings must not wait for it.
  const refresh = useCallback(async () => {
    const reads = [
      client.settings().then((next) => {
        if (!alive.current) {
          return;
        }
        // A read that raced a write and finished after it carries the old words;
        // while a write is queued the write's own answer is the newer one.
        if (pending.current === 0) {
          setSettings(next);
        }
        setFailure(null);
      }).catch((error: unknown) => {
        if (alive.current) {
          setFailure(reason(error));
        }
      }),
      client.roster().then((next) => {
        if (alive.current) {
          setRoster(next);
        }
      }).catch(() => {
        // Without the roster the pickers offer nothing; the settings still show.
      }),
      client.prompts().then((next) => {
        if (alive.current) {
          setPrompts(next);
        }
      }).catch(() => {
        // The Prompts page says it is still reading.
      }),
      client.state().then((next) => {
        if (alive.current) {
          running.current = coordinatorRunning(next);
          setCoordRunning(running.current);
        }
      }).catch(() => {
        // Not knowing whether a coordinator runs leaves the notice off.
      }),
    ];
    await Promise.all(reads);
  }, [client, setSettings]);

  useEffect(() => {
    void refresh();
    const timer = setInterval(() => {
      if (pending.current === 0) {
        void refresh();
      }
    }, POLL_MS);
    return () => clearInterval(timer);
  }, [refresh]);

  const write = useCallback(
    (key: string, value: SettingValue, forRepo: boolean, undo = false): Promise<boolean> => {
      pending.current += 1;
      const run = async (): Promise<boolean> => {
        if (alive.current) {
          setWriting(key);
          // An undo runs behind the refusal that caused it and must not wipe it.
          if (!undo) {
            setRefusal(null);
          }
          setSaved(null);
        }
        try {
          const body: { key: string; value?: Exclude<SettingValue, null>; for_repo?: boolean } = {
            key,
          };
          if (value !== null) {
            body.value = value;
          }
          if (forRepo) {
            body.for_repo = true;
          }
          const reply = await client.putSetting(body);
          if (alive.current) {
            setSettings(reply.settings);
            say(outcomeOf(reply.outcome));
            if (needsCoordinatorRestart(key) && running.current) {
              setCoordChanged(true);
            }
          }
          return true;
        } catch (error) {
          if (alive.current) {
            if (!undo) {
              setRefusal({ key, text: reason(error) });
            } else {
              // Putting a write back was refused too: say so, so the person knows the
              // role is still half switched and which key to look at.
              setRefusal((was) => ({
                key: was?.key ?? key,
                text: `${was?.text ?? ""} (and putting ${key} back was refused too: ${reason(error)}; it still holds the new value)`.trim(),
              }));
            }
          }
          return false;
        } finally {
          pending.current -= 1;
          if (alive.current && pending.current === 0) {
            setWriting(null);
          }
        }
      };
      const next = tail.current.then(run, run);
      tail.current = next;
      return next;
    },
    [client, say, setSettings],
  );

  const writeAll = useCallback(
    async (writes: Write[], forRepo: boolean): Promise<boolean> => {
      const done: Array<{ key: string; prior: string | boolean | null }> = [];
      let current = settingsRef.current;
      for (const next of writes) {
        const prior = priorInScope(current, next.key, forRepo);
        if (!(await write(next.key, next.value as SettingValue, forRepo))) {
          // A later write of the choice was refused: put back what it already wrote,
          // newest first, so the role is not left half switched.
          for (const undo of done.reverse()) {
            await write(undo.key, undo.prior as SettingValue, forRepo, true);
          }
          return false;
        }
        done.push({ key: next.key, prior });
        current = settingsRef.current;
      }
      return true;
    },
    [write],
  );

  const reloadPrompts = useCallback(async () => {
    try {
      const list = await client.prompts();
      if (alive.current) {
        setPrompts(list);
      }
    } catch {
      // The list is re-read on the next poll; a failed reload keeps what is shown.
    }
  }, [client]);

  return {
    settings,
    roster,
    prompts,
    failure,
    refusal,
    saved,
    writing,
    write,
    writeAll,
    restartNotice: restartNotice({ changed: coordChanged, running: coordRunning }),
    reloadPrompts,
    setPrompts,
    refuse: (key, error) => setRefusal({ key, text: reason(error) }),
    restartCoordinator: async () => {
      try {
        await client.restartCoordinator();
        setCoordChanged(false);
        return null;
      } catch (error) {
        return reason(error);
      }
    },
    say,
    clearNotices: () => {
      setRefusal(null);
      setSaved(null);
    },
  };
}
