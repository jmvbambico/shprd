// A question, permission or plan card (or an escalation) in a chat timeline,
// answered through `POST /api/asks/answer`.
//
// It mirrors the desktop chat's cards (`host/herdr-desktop/mods/shprd-chat`).
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/chat/cards.js`
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik

import { useState } from "react";

import type { ApiClient } from "../api/client";
import { answerBody, footer, type Card } from "./asks";
import { reason } from "./useBoard";

/** A question, permission or plan card (or an escalation) in the timeline. */
export function AskCard({
  client,
  card,
  onAnswered,
}: {
  client: ApiClient;
  card: Card;
  onAnswered: () => Promise<void>;
}) {
  const [busy, setBusy] = useState(false);
  const [typing, setTyping] = useState(false);
  const [draft, setDraft] = useState("");
  const [refusal, setRefusal] = useState<string | null>(null);

  async function answer(text: string) {
    const body = answerBody(card, text);
    if (body === null || busy) {
      return;
    }
    setBusy(true);
    setRefusal(null);
    try {
      await client.answerAsk(body);
      await onAnswered();
    } catch (error) {
      setRefusal(reason(error));
    }
    setBusy(false);
  }

  const note = footer(card);
  return (
    <article className="ask-card" data-from={card.from} data-refused={refusal !== null} aria-label={card.title}>
      <header className="ask-card-head">
        <span className="ask-card-kind">{card.title}</span>
        <span className="grow" />
        {note !== "" ? <span className="dim small">{note}</span> : null}
      </header>
      <p className="ask-card-text">{card.text}</p>
      {card.verb !== null ? <p className="mono amber small">{card.verb}</p> : null}
      {card.options.length > 0 ? (
        <div className="ask-card-options">
          {card.options.map((option) => (
            <button
              key={option.answer}
              type="button"
              className="plain-button"
              disabled={busy}
              onClick={() => void answer(option.answer)}
            >
              {option.label}
            </button>
          ))}
        </div>
      ) : null}
      {card.freeText ? (
        typing ? (
          <form
            className="ask-card-type"
            onSubmit={(event) => {
              event.preventDefault();
              void answer(draft);
            }}
          >
            <input
              className="input"
              aria-label="Your answer"
              placeholder="Type your answer"
              autoFocus
              value={draft}
              onChange={(event) => setDraft(event.target.value)}
            />
            <button type="submit" className="primary-button" disabled={busy || draft.trim() === ""}>
              Send
            </button>
            <button type="button" className="plain-button" onClick={() => setTyping(false)}>
              Cancel
            </button>
          </form>
        ) : (
          <button type="button" className="plain-button" disabled={busy} onClick={() => setTyping(true)}>
            Type an answer
          </button>
        )
      ) : null}
      {refusal !== null ? (
        <p className="accept-error" role="alert">
          {refusal}
        </p>
      ) : null}
    </article>
  );
}
