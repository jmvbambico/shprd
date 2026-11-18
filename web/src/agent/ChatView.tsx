// The chat view: one agent's conversation as bubbles, and a composer.
//
// Words and shapes are the Herdr window's (`shprd-chat`, ported from
// Shepherdr's `mods/chat/main.js`, MIT, commit a3f327a): the agent's words at the
// left, the person's at the right in an accent tint, markdown in the bubble, tool
// calls folded under the reply, "Show more" under a long reply, and a composer
// with a round send. Markdown is drawn through DOMPurify, never as raw HTML.

import { useEffect, useMemo, useRef, useState, type FormEvent, type KeyboardEvent } from "react";

import type { ApiClient } from "../api/client";
import type { ChatMessageDto } from "../gen";
import {
  COORDINATOR,
  WORDS,
  emptyWords,
  folding,
  messageKey,
  mine,
  toolLine,
  when,
} from "./chatModel";
import { renderMarkdown } from "./markdown";
import { useChat } from "./useChat";

/** Props of [`ChatView`]. */
export interface ChatViewProps {
  client: ApiClient;
  /** `coordinator` or a task id. */
  space: string;
}

/** The composer's hint, by whose chat this is. */
export function placeholderFor(space: string): string {
  return space === COORDINATOR ? "Message the coordinator" : "Message the agent";
}

/** One space's chat. */
export function ChatView({ client, space }: ChatViewProps) {
  const { loading, chat, failure, refusal, sending, send, retry } = useChat(client, space);
  const [draft, setDraft] = useState("");
  const [opened, setOpened] = useState<ReadonlySet<string>>(new Set());
  const scroller = useRef<HTMLDivElement>(null);
  const pinned = useRef(true);

  // A different space starts with an empty composer and folds closed.
  useEffect(() => {
    setDraft("");
    setOpened(new Set());
    pinned.current = true;
  }, [space]);

  // Stay at the newest message unless the person scrolled up to read.
  useEffect(() => {
    const el = scroller.current;
    if (el && pinned.current) {
      el.scrollTop = el.scrollHeight;
    }
  }, [chat.messages.length, space]);

  function toggle(key: string) {
    setOpened((now) => {
      const next = new Set(now);
      if (next.has(key)) {
        next.delete(key);
      } else {
        next.add(key);
      }
      return next;
    });
  }

  async function submit(event?: FormEvent) {
    event?.preventDefault();
    if (draft.trim() === "" || sending) {
      return;
    }
    if (await send(draft)) {
      setDraft("");
      pinned.current = true;
    }
  }

  function onKey(event: KeyboardEvent<HTMLTextAreaElement>) {
    // Enter sends, as in the window's composer; Shift-Enter is a new line.
    if (event.key === "Enter" && !event.shiftKey && !event.nativeEvent.isComposing) {
      event.preventDefault();
      void submit();
    }
  }

  const said = refusal ?? failure ?? (chat.messages.length === 0 && !loading ? chat.note : null);

  return (
    <section className="chat" aria-label={`Chat with ${space === COORDINATOR ? "the coordinator" : space}`}>
      <div
        className="chat-rows"
        ref={scroller}
        onScroll={(event) => {
          const el = event.currentTarget;
          pinned.current = el.scrollHeight - el.scrollTop - el.clientHeight < 48;
        }}
      >
        {loading && chat.messages.length === 0 ? (
          <p className="chat-empty" role="status" aria-busy="true">
            Reading the conversation…
          </p>
        ) : chat.messages.length === 0 ? (
          <div className="chat-empty" data-state="empty">
            <p className="chat-empty-title">{WORDS.emptyTitle}</p>
            <p className="chat-empty-note">{emptyWords(space)}</p>
          </div>
        ) : (
          <>
            {!chat.more ? <p className="chat-top">{WORDS.top}</p> : null}
            {chat.messages.map((message) => (
              <Bubble
                key={messageKey(message) + String(message.at_ms)}
                message={message}
                open={opened.has(messageKey(message))}
                onToggle={() => toggle(messageKey(message))}
              />
            ))}
          </>
        )}
      </div>

      {said !== null ? (
        <p className="chat-said" role="status" data-kind={refusal !== null || failure !== null ? "problem" : "note"}>
          {said}
          {failure !== null && refusal === null ? (
            <button type="button" className="quiet chat-retry" onClick={retry}>
              Try again
            </button>
          ) : null}
        </p>
      ) : null}

      <form className="chat-composer" onSubmit={(event) => void submit(event)}>
        <textarea
          className="chat-input"
          rows={1}
          value={draft}
          placeholder={placeholderFor(space)}
          aria-label={placeholderFor(space)}
          onChange={(event) => setDraft(event.target.value)}
          onKeyDown={onKey}
        />
        <button className="chat-send" type="submit" aria-label="Send" disabled={sending || draft.trim() === ""}>
          <span aria-hidden="true">↑</span>
        </button>
      </form>
    </section>
  );
}

/** One message: its words, its tool calls, its time. */
function Bubble({ message, open, onToggle }: { message: ChatMessageDto; open: boolean; onToggle: () => void }) {
  const shown = useMemo(() => folding(message.text, open), [message.text, open]);
  const html = useMemo(() => renderMarkdown(shown.text, mine(message)), [shown.text, message]);
  const time = when(message.at_ms);
  return (
    <article className="chat-row" data-from={mine(message) ? "you" : "agent"}>
      {message.text.trim() !== "" ? (
        <div className="chat-bubble">
          {/* The markup is DOMPurify's output: a short tag list, http(s) links only. */}
          <div className="md" dangerouslySetInnerHTML={{ __html: html }} />
          {shown.foldable ? (
            <button type="button" className="quiet chat-fold" onClick={onToggle}>
              {open ? WORDS.showLess : WORDS.showMore}
            </button>
          ) : null}
        </div>
      ) : null}
      {message.tools.map((tool, index) => (
        <p key={index} className="chat-tool">
          → {toolLine(tool)}
        </p>
      ))}
      {time !== "" ? <p className="chat-time">{time}</p> : null}
    </article>
  );
}
