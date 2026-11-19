// Prompts: the list of role prompts this machine has, or the one being edited.
//
// There is no Save button: the words are written a moment after typing stops,
// when the box loses focus, and when the page is left. Reset is not an edit — it
// asks the service to put the words this build ships back.

import { useCallback, useEffect, useRef, useState } from "react";

import type { ApiClient } from "../api/client";
import type { PromptDto } from "../gen";
import { promptLine, promptName, promptRows } from "./model";
import { SAVED_MS, type SettingsState } from "./useSettings";

/** How long an edited prompt waits before it saves itself. */
export const PROMPT_MS = 700;

/** The Prompts page. */
export function PromptsPage({
  client,
  state,
}: {
  client: ApiClient;
  state: SettingsState;
}) {
  const [open, setOpen] = useState<PromptDto | null>(null);
  const rows = promptRows(state.prompts);

  async function openPrompt(id: string) {
    state.clearNotices();
    try {
      setOpen(await client.prompt(id));
    } catch (error) {
      state.refuse(`prompts.${id}`, error);
    }
  }

  if (open !== null) {
    return (
      <PromptEditor
        client={client}
        state={state}
        prompt={open}
        onBack={() => {
          setOpen(null);
          void state.reloadPrompts();
        }}
      />
    );
  }
  if (state.prompts === null) {
    return (
      <div className="spage" data-page="prompts">
        <h2 className="spage-title">Prompts</h2>
        <p className="snote">Reading the prompts…</p>
      </div>
    );
  }
  if (rows.length === 0) {
    return (
      <div className="spage" data-page="prompts">
        <h2 className="spage-title">Prompts</h2>
        <p className="snote">No prompts yet.</p>
      </div>
    );
  }
  return (
    <div className="spage" data-page="prompts">
      <h2 className="spage-title">Prompts</h2>
      <p className="snote">What each agent is told before it starts. Edit any of them: an edit is kept.</p>
      <ul className="prompt-list">
        {rows.map((row) => (
          <li key={row.id} className="prompt-item" data-prompt={row.id}>
            <div className="prompt-words">
              <div className="prompt-main">
                <span className="prompt-id">{row.name}</span>
                <span
                  className={
                    row.problem !== null ? "tag tone-red" : row.edited ? "tag tone-amber" : "tag"
                  }
                >
                  {row.word}
                </span>
              </div>
              <div className="prompt-about">{row.problem ?? row.about}</div>
            </div>
            <button
              type="button"
              className="btn"
              aria-label={`Edit ${row.name}`}
              onClick={() => void openPrompt(row.id)}
            >
              Edit
            </button>
          </li>
        ))}
      </ul>
    </div>
  );
}

function PromptEditor({
  client,
  state,
  prompt,
  onBack,
}: {
  client: ApiClient;
  state: SettingsState;
  prompt: PromptDto;
  onBack: () => void;
}) {
  const [shown, setShown] = useState(prompt);
  const [text, setText] = useState(prompt.text);
  const [writing, setWriting] = useState(false);
  const [justSaved, setJustSaved] = useState(false);
  const stored = useRef(prompt.text);
  const latest = useRef(prompt.text);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const flight = useRef<Promise<boolean> | null>(null);
  const [failed, setFailed] = useState(false);
  const [resetting, setResetting] = useState(false);
  const resettingRef = useRef(false);
  const alive = useRef(true);
  const stateRef = useRef(state);
  stateRef.current = state;

  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  const save = useCallback((): Promise<boolean> => {
    if (timer.current !== null) {
      clearTimeout(timer.current);
      timer.current = null;
    }
    // One write at a time: a caller that asks while one is in flight gets that
    // write's promise, so a leave and a debounce firing together cannot store the
    // same words twice.
    if (flight.current !== null) {
      return flight.current;
    }
    // A reset is in progress: the words it brings back win over anything typed.
    if (resettingRef.current || latest.current === stored.current) {
      return Promise.resolve(true);
    }
    if (alive.current) {
      setWriting(true);
    }
    // The write loops until what is stored is what is in the box: words typed
    // while a request was in flight are written by the same call, so a later
    // caller that was handed this promise never leaves them behind.
    const run = async (): Promise<boolean> => {
      try {
        while (latest.current !== stored.current) {
          const words = latest.current;
          const reply = await client.putPrompt(prompt.id, { text: words });
          stored.current = words;
          if (alive.current) {
            setShown(reply);
            setJustSaved(true);
            setTimeout(() => {
              if (alive.current) {
                setJustSaved(false);
              }
            }, SAVED_MS);
          }
        }
        if (alive.current) {
          setFailed(false);
        }
        return true;
      } catch (error) {
        // A refusal keeps the words in the box, unsaved, with the service's reason.
        stateRef.current.refuse(`prompts.${prompt.id}`, error);
        if (alive.current) {
          setFailed(true);
        }
        return false;
      } finally {
        flight.current = null;
        if (alive.current) {
          setWriting(false);
        }
      }
    };
    flight.current = run();
    return flight.current;
  }, [client, prompt.id]);

  // Leaving the page (or closing the dialog) stores the last words.
  useEffect(
    () => () => {
      void save();
    },
    [save],
  );

  async function reset() {
    state.clearNotices();
    // Reset is serialized with the autosave: nothing new starts while it runs, and
    // a save already in flight finishes first, so it cannot land after the reset
    // and write the old words back as an edit.
    resettingRef.current = true;
    setResetting(true);
    if (timer.current !== null) {
      clearTimeout(timer.current);
      timer.current = null;
    }
    try {
      if (flight.current !== null) {
        await flight.current;
      }
      const reply = await client.resetPrompt(prompt.id);
      stored.current = reply.text;
      latest.current = reply.text;
      setText(reply.text);
      setShown(reply);
      setFailed(false);
      state.say("Reset: the default words are back");
    } catch (error) {
      state.refuse(`prompts.${prompt.id}`, error);
    } finally {
      resettingRef.current = false;
      setResetting(false);
    }
  }

  return (
    <div className="spage" data-page="prompts">
      <div className="prompt-bar">
        <button
          type="button"
          className="link prompt-back"
          aria-label={failed && text !== stored.current ? undefined : "Back"}
          onClick={() => {
            // Words the service refused stay in the box: the first Back keeps
            // them and shows why, and only a second one (now "Discard…") leaves.
            if (failed && latest.current !== stored.current) {
              onBack();
              return;
            }
            void save().then((stored_) => {
              if (stored_) {
                onBack();
              }
            });
          }}
        >
          {failed && text !== stored.current ? "Discard and go back" : "‹ Prompts"}
        </button>
        <span className="grow" />
        {writing || justSaved ? (
          <span className={writing ? "saving" : "saved"}>{writing ? "saving…" : "Saved"}</span>
        ) : null}
      </div>
      <div className="prompt-head">
        <h2 className="spage-title">{promptName(prompt.id)}</h2>
        <span className={shown.state === "edited" ? "tag tone-amber" : "tag"}>
          {shown.state === "edited" ? "Edited" : "Default"}
        </span>
      </div>
      <div className="snote">{promptLine(shown.state)}</div>
      <div className="prompt-path">{shown.path}</div>
      <textarea
        className="input prompt-editor"
        aria-label={`Prompt ${prompt.id}`}
        rows={16}
        disabled={resetting}
        value={text}
        onChange={(event) => {
          setText(event.target.value);
          latest.current = event.target.value;
          if (timer.current !== null) {
            clearTimeout(timer.current);
          }
          timer.current = setTimeout(() => void save(), PROMPT_MS);
        }}
        onBlur={() => void save()}
      />
      <div>
        <button type="button" className="btn" disabled={writing || resetting} onClick={() => void reset()}>
          Reset to default
        </button>
      </div>
    </div>
  );
}
