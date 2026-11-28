// Reading and sending one space's conversation.

import { useCallback, useEffect, useRef, useState } from "react";

import { ApiError, type ApiClient } from "../api/client";
import { useLiveRefresh } from "../api/useLive";
import { empty, fold, outgoing, type Chat } from "./chatModel";

/** How often the chat reads when nothing woke it, ms (the window's floor). */
export const FLOOR_MS = 3000;

/** How many pages one look takes before leaving the rest to the next. */
const PAGES = 4;

/** What the chat screen is showing. */
export interface ChatState {
  /** Nothing has been read for this space yet. */
  loading: boolean;
  chat: Chat;
  /** Why the last read failed; the messages already read stay drawn. */
  failure: string | null;
  /** Why the last send was refused; the words stay in the composer. */
  refusal: string | null;
  sending: boolean;
  /** Types `text` into the agent; resolves true when it went. */
  send: (text: string) => Promise<boolean>;
  /** Reads again now. */
  retry: () => void;
}

function wording(err: unknown): string {
  if (err instanceof ApiError) {
    return err.message;
  }
  return err instanceof Error ? err.message : "the service did not answer";
}

/** The conversation of `space`, kept fresh. */
export function useChat(client: ApiClient, space: string): ChatState {
  const [chat, setChat] = useState<Chat>(() => empty(space));
  const [loading, setLoading] = useState(true);
  const [failure, setFailure] = useState<string | null>(null);
  const [refusal, setRefusal] = useState<string | null>(null);
  const [sending, setSending] = useState(false);
  const held = useRef<Chat>(chat);
  const current = useRef(space);
  const busy = useRef(false);

  const refresh = useCallback(async () => {
    if (busy.current) {
      return;
    }
    busy.current = true;
    const asked = current.current;
    try {
      for (let page = 0; page < PAGES; page += 1) {
        const answer = await client.chat(asked, held.current.cursor);
        // The person moved to another space while this was in flight.
        if (asked !== current.current) {
          return;
        }
        held.current = fold(held.current, answer);
        setChat(held.current);
        if (!held.current.more) {
          break;
        }
      }
      setFailure(null);
    } catch (err) {
      if (asked === current.current) {
        setFailure(wording(err));
      }
    } finally {
      busy.current = false;
      if (asked === current.current) {
        setLoading(false);
      }
    }
  }, [client]);

  // A different space is a different conversation: start over from its first page.
  useEffect(() => {
    current.current = space;
    held.current = empty(space);
    setChat(held.current);
    setLoading(true);
    setFailure(null);
    setRefusal(null);
    busy.current = false;
    void refresh();
  }, [space, refresh]);

  useEffect(() => {
    const timer = setInterval(() => void refresh(), FLOOR_MS);
    return () => clearInterval(timer);
  }, [refresh]);

  useLiveRefresh(client, () => void refresh());

  const send = useCallback(
    async (text: string) => {
      const body = outgoing(current.current, text);
      if (body === null) {
        return false;
      }
      setSending(true);
      setRefusal(null);
      try {
        await client.sendChat(body);
        setSending(false);
        void refresh();
        return true;
      } catch (err) {
        // The words stay in the composer: a refused line is retyped, not lost.
        setRefusal(wording(err));
        setSending(false);
        return false;
      }
    },
    [client, refresh],
  );

  return { loading, chat, failure, refusal, sending, send, retry: () => void refresh() };
}
