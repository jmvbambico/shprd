// The task thread's Chat view: the agent's conversation as it writes it, with the
// cards that wait for an answer in the timeline, and a box to send it a turn.
//
// It mirrors the desktop's chat (`host/herdr-desktop/mods/shprd-chat`): the
// service reads the agent's own record and hands back pages and a cursor, the
// view folds them together, a long reply folds to "Show more", and a question,
// permission or plan card sits among the messages and is answered through
// `POST /api/asks/answer`.
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/chat/main.js` (bubbles, cards, composer), `mods/chat/cards.js`
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik

import { useCallback, useEffect, useRef, useState, type FormEvent } from "react";

import type { ApiClient } from "../api/client";
import type { ChatDto } from "../gen";
import { answerBody, cards, footer, forTask, type Card } from "./asks";
import {
  WORDS,
  fold,
  folding,
  mine,
  outgoing,
  status,
  timeline,
  toolLine,
  when,
} from "./chat";
import { reason, type Board } from "./useBoard";

/** What the view needs. */
export interface ChatViewProps {
  /** The one client every screen calls. */
  client: ApiClient;
  /** The live board: its change is when the chat is read again. */
  board: Board;
  /** The task whose conversation this is. */
  task: string;
}

/** The task's chat. */
export function ChatView({ client, board, task }: ChatViewProps) {
  const [chat, setChat] = useState<ChatDto | null>(null);
  const [failure, setFailure] = useState<string | null>(null);
  const [opened, setOpened] = useState<ReadonlySet<string>>(new Set());
  const held = useRef<ChatDto | null>(null);
  const reading = useRef(0);
  const scroller = useRef<HTMLDivElement | null>(null);
  const stuck = useRef(true);

  const read = useCallback(async () => {
    const id = ++reading.current;
    try {
      const answer = await client.chat(task, held.current?.cursor ?? "");
      if (id !== reading.current) {
        return;
      }
      held.current = fold(held.current, answer);
      setChat(held.current);
      setFailure(null);
    } catch (error) {
      if (id === reading.current) {
        setFailure(reason(error));
      }
    }
  }, [client, task]);

  // A different task is a different conversation.
  useEffect(() => {
    held.current = null;
    setChat(null);
    setFailure(null);
    void read();
  }, [read]);

  // The board moving is the agent writing: read what came after the cursor.
  const seq = board.state?.seq ?? 0;
  useEffect(() => {
    if (seq !== 0) {
      void read();
    }
  }, [seq, read]);

  const waiting = forTask(cards(board.state, board.asks), task);
  const rows = timeline(chat, waiting);
  const empty = status(chat);

  // New words keep the bottom in view, unless the person scrolled up to read.
  useEffect(() => {
    const el = scroller.current;
    if (el !== null && stuck.current) {
      el.scrollTop = el.scrollHeight;
    }
  }, [rows.length]);

  return (
    <div className="chat" aria-label="Chat">
      <div
        className="chat-scroll"
        ref={scroller}
        onScroll={(event) => {
          const el = event.currentTarget;
          stuck.current = el.scrollHeight - el.scrollTop - el.clientHeight < 48;
        }}
      >
        {failure !== null && chat === null ? (
          <p className="note" role="alert">
            {failure}
          </p>
        ) : chat === null ? (
          <p className="note" aria-busy="true">
            Reading the conversation…
          </p>
        ) : rows.length === 0 ? (
          <div className="note">
            <p>{WORDS.emptyTitle}</p>
            <p className="dim">{empty}</p>
          </div>
        ) : (
          rows.map((row) => {
            if (row.kind === "top") {
              return (
                <p key={row.key} className="chat-top">
                  {WORDS.top}
                </p>
              );
            }
            if (row.kind === "card") {
              return <AskCard key={row.key} client={client} card={row.card} onAnswered={board.reload} />;
            }
            const message = row.message;
            const view = folding(message.text, opened.has(row.key));
            return (
              <div key={row.key} className="bubble-row" data-mine={mine(message)}>
                <div className="bubble-col">
                  {message.text.trim() !== "" ? (
                    <p className="bubble" data-mine={mine(message)}>
                      {view.text}
                      {view.folded ? "…" : ""}
                    </p>
                  ) : null}
                  {view.foldable ? (
                    <button
                      type="button"
                      className="quiet-text"
                      onClick={() =>
                        setOpened((set) => {
                          const next = new Set(set);
                          if (!next.delete(row.key)) {
                            next.add(row.key);
                          }
                          return next;
                        })
                      }
                    >
                      {view.folded ? WORDS.showMore : WORDS.showLess}
                    </button>
                  ) : null}
                  {message.tools.map((tool, index) => (
                    <p key={`${row.key}:${index}`} className="chat-tool mono dim">
                      → {toolLine(tool)}
                    </p>
                  ))}
                  {message.at_ms !== null ? <p className="chat-time dim">{when(message.at_ms)}</p> : null}
                </div>
              </div>
            );
          })
        )}
      </div>
      <Composer client={client} task={task} onSent={read} />
    </div>
  );
}

/** A question, permission or plan card (or an escalation) in the timeline. */
function AskCard({
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

/** The composer: one line of words, and the round send at its end. */
function Composer({
  client,
  task,
  onSent,
}: {
  client: ApiClient;
  task: string;
  onSent: () => Promise<void>;
}) {
  const [text, setText] = useState("");
  const [sending, setSending] = useState(false);
  const [refusal, setRefusal] = useState<string | null>(null);

  async function send(event: FormEvent) {
    event.preventDefault();
    const body = outgoing(task, text);
    if (body === null || sending) {
      return;
    }
    setSending(true);
    setRefusal(null);
    try {
      await client.sendChat(body);
      // The words stay only when the turn was refused: a refused line is retyped, not lost.
      setText("");
      await onSent();
    } catch (error) {
      setRefusal(reason(error));
    }
    setSending(false);
  }

  return (
    <form className="composer" onSubmit={send}>
      {refusal !== null ? (
        <p className="accept-error" role="alert">
          {refusal}
        </p>
      ) : null}
      <div className="composer-row">
        <input
          className="composer-input"
          aria-label="Message the agent"
          placeholder="Message the agent"
          value={text}
          onChange={(event) => setText(event.target.value)}
        />
        <button
          type="submit"
          className="send-button"
          aria-label="Send"
          disabled={sending || text.trim() === ""}
        >
          {sending ? "…" : "↑"}
        </button>
      </div>
    </form>
  );
}
