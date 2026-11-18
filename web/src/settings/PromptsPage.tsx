// Prompts: the list of role prompts this machine has, or the one being edited.
//
// There is no Save button: the words are written a moment after typing stops,
// when the box loses focus, and when the page is left. Reset is not an edit — it
// asks the service to put the words this build ships back.

import { useCallback, useEffect, useRef, useState } from "react";

import type { ApiClient } from "../api/client";
import type { PromptDto } from "../gen";
import { promptLine, promptRows } from "./model";
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
        <p className="snote">The service writes the shipped ones on first read.</p>
      </div>
    );
  }
  return (
    <div className="spage" data-page="prompts">
      <h2 className="spage-title">Prompts</h2>
      <ul className="prompt-list">
        {rows.map((row) => (
          <li key={row.id}>
            <button type="button" className="prompt-row" onClick={() => void openPrompt(row.id)}>
              <span className="prompt-main">
                <span className="prompt-id">{row.id}</span>
                <span
                  className={
                    row.problem !== null ? "prompt-word tone-red" : row.edited ? "prompt-word tone-amber" : "prompt-word"
                  }
                >
                  {row.word}
                </span>
              </span>
              <span className="prompt-about">{row.problem ?? row.about}</span>
              <span className="prompt-go" aria-hidden="true">
                ›
              </span>
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
  const flight = useRef<Promise<void> | null>(null);
  const alive = useRef(true);
  const stateRef = useRef(state);
  stateRef.current = state;

  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  const save = useCallback((): Promise<void> => {
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
    if (latest.current === stored.current) {
      return Promise.resolve();
    }
    if (alive.current) {
      setWriting(true);
    }
    // The write loops until what is stored is what is in the box: words typed
    // while a request was in flight are written by the same call, so a later
    // caller that was handed this promise never leaves them behind.
    const run = async () => {
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
      } catch (error) {
        // A refusal keeps the words in the box, unsaved, with the service's reason.
        stateRef.current.refuse(`prompts.${prompt.id}`, error);
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
    if (timer.current !== null) {
      clearTimeout(timer.current);
      timer.current = null;
    }
    state.clearNotices();
    try {
      const reply = await client.resetPrompt(prompt.id);
      stored.current = reply.text;
      latest.current = reply.text;
      setText(reply.text);
      setShown(reply);
      state.say("Reset: the shipped words are back");
    } catch (error) {
      state.refuse(`prompts.${prompt.id}`, error);
    }
  }

  return (
    <div className="spage" data-page="prompts">
      <h2 className="spage-title">{prompt.id}</h2>
      <div className="prompt-bar">
        <span className={shown.state === "edited" ? "badge tone-amber" : "badge"}>
          {shown.state === "edited" ? "edited" : "shipped"}
        </span>
        <span className="grow" />
        {writing || justSaved ? (
          <span className={writing ? "saving" : "saved"}>{writing ? "saving…" : "Saved"}</span>
        ) : null}
        <button
          type="button"
          className="link"
          onClick={() => {
            void save().then(onBack);
          }}
        >
          Back
        </button>
      </div>
      <div className="prompt-path">{shown.path}</div>
      <div className="snote">{promptLine(shown.state)}</div>
      <textarea
        className="input prompt-editor"
        aria-label={`Prompt ${prompt.id}`}
        rows={16}
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
        <button type="button" className="link" disabled={writing} onClick={() => void reset()}>
          Reset to the shipped words
        </button>
      </div>
    </div>
  );
}
