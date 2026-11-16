// The coordinator: the run's planning agent, above the spaces list.
//
// It is the one conversation in the window that belongs to the whole run rather
// than to a task, so it sits in the sidebar area M10.3 reserved rather than in
// the side panel: the transcript, and the composer that answers it.
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/coordinator/main.js` (the pane and its composer),
//   `mods/chat/main.js` (the bubble list: the agent's words at the left in
//   `raised` bubbles, the person's at the right in an accent tint, tool calls
//   folded under the reply, the round send at the end of the composer)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// What is shprd's own: the conversation is read from the agent's **own record**
// by the service (`GET /api/chat`, which knows each program's file and turns it
// into messages), and a turn is typed into the coordinator's pane by the service
// too (`POST /api/chat`) — a mod has no terminal of its own, and the service is
// where the host is. The panel never parses a transcript and never types into a
// pane itself.
//
// It follows what it reads two ways, as every shprd mod does: the change file
// the service touches after every ledger write (`lib/watch.js`), and a timer as
// the floor, because an agent writing its own record changes no ledger event.

import { View, div } from "gpui-kit";
import { Input, InputState, h_flex, v_flex } from "gpui-base";
import { props, theme } from "herdr";

import { attach, reason } from "./lib/client.js";
import {
  COMPACT_MAX,
  COORDINATOR,
  bubble,
  empty,
  fold,
  header,
  latest,
  mine,
  nextCursor,
  outgoing,
  status,
  toolLine,
  when,
} from "./lib/chat.js";
import { card, cardHeader, note } from "./lib/panel.js";
import { RADIUS, SPACE, TEXT, tone } from "./lib/tokens.js";
import { followChanges } from "./lib/watch.js";

/** How often the panel reads the conversation when nothing wakes it, ms. */
const FLOOR_MS = 3000;

/** How many messages the sidebar's short panel draws; a taller one draws more. */
const SHOWN = { compact: 2, tall: 8 };

/** How many pages one refresh takes before leaving the rest to the next look. */
const PAGES = 4;

/** How wide a bubble may be: the sidebar minus its own padding, so words wrap. */
const BUBBLE_W = 340;

export default class Coordinator extends View {
  init(_props, cx) {
    this.cx = cx;
    // A task's chat is the same panel with another space named; the layout mounts
    // this one for the coordinator, and M10.5's task thread may mount it with a
    // task's id.
    this.space = props()?.space || COORDINATOR;
    this.compact = props()?.compact !== false;
    this.chat = empty(this.space);
    this.client = null;
    this.watching = false;
    this.failure = null;
    // A refused send keeps its words and its reason until the next send: the
    // reads that follow would otherwise clear the line before a person read it.
    this.refusal = null;
    this.sending = false;
    this.input = InputState.new({ placeholder: "Message the coordinator" });
    // A submit is Enter in the composer: the same act as the send button.
    this.input.on("submit", () => this.send());
    this.refresh();
    cx.timer.every(FLOOR_MS, (cx) => this.refresh(cx));
  }

  /**
   * Reads the pages after the cursor, then shows them.
   *
   * A service that restarted has a new `id`, and its agent's record is a new one
   * too: `attach` notices the restart and the panel starts the conversation over
   * from its first page instead of asking a new service for a cursor it never
   * wrote (M10 gate bug 5). A read that answers nothing is a failure, so a dead
   * port is looked for again rather than kept.
   */
  async refresh(cx = this.cx) {
    try {
      const next = await attach(this.client, { wait: (ms) => cx.sleep(ms) });
      if (!next.client) {
        this.client = null;
        this.failure = "shprd serve is not running";
        cx.notify();
        return;
      }
      if (next.restarted) this.forget();
      this.client = next.client;
      this.follow();
      // `more` means the page filled up: a long conversation arrives over a few
      // calls rather than one slow one, and the next timer catches any left over.
      for (let page = 0; page < PAGES; page += 1) {
        const answer = await this.client.call(
          "GET",
          `/api/chat?space=${encodeURIComponent(this.space)}&since=${encodeURIComponent(nextCursor(this.chat))}`,
          { strict: true },
        );
        this.chat = fold(this.chat, answer);
        if (!this.chat.more) break;
      }
      this.failure = null;
    } catch (err) {
      // A restarted service answers with another id: forget the client so the
      // next look finds the new port instead of dialling a dead one.
      this.client = null;
      this.failure = reason(err);
    }
    cx.notify();
  }

  /**
   * Starts the conversation again: a new service's agent wrote a new record, so
   * a cursor from the old one names nothing it has.
   */
  forget() {
    this.chat = empty(this.space);
    this.refusal = null;
    this.sending = false;
  }

  /** Follows the service's change file for as long as this copy lives. */
  follow() {
    if (this.watching || !this.client) return;
    this.watching = true;
    const cx = this.cx;
    cx.spawn(async () => {
      await followChanges(this.client.dir, () => this.refresh(cx));
    });
  }

  /** Types the composer's words into the coordinator, as the person. */
  async send() {
    const body = outgoing(this.space, this.input.value());
    if (body === null || this.sending) return;
    if (!this.client) {
      this.refusal = "shprd serve is not running";
      this.cx.notify();
      return;
    }
    this.sending = true;
    this.refusal = null;
    this.cx.notify();
    try {
      await this.client.call("POST", "/api/chat", { body });
      this.input.set_value("");
    } catch (err) {
      // The words stay in the composer: a refused line is retyped, not lost.
      this.refusal = reason(err);
    }
    this.sending = false;
    this.cx.notify();
    this.refresh();
  }

  render() {
    const t = theme();
    const colors = tone(t);
    const shown = latest(this.chat, this.compact ? SHOWN.compact : SHOWN.tall);
    const said = this.refusal ?? this.failure ?? status(this.chat);
    return card(t, [
      // The header names the panel and the program it reads, nothing else: a
      // key cap here would promise a binding this panel does not have.
      cardHeader(
        t,
        h_flex()
          .gap(6)
          .items_center()
          .child(div().text_size(TEXT.body).font_medium().text_color(t.foreground).child("Coordinator"))
          .child(
            div()
              .text_size(TEXT.label)
              .text_color(t.dim)
              .child(this.chat.program ?? "no program set"),
          ),
      ),
      v_flex()
        .flex_1()
        .min_h(0)
        .px(12)
        .py(6)
        .gap(6)
        // The newest message is the one a person looks at, so the column grows
        // upwards: what does not fit is the oldest at the top, not the answer
        // that just arrived.
        .justify_end()
        .overflow_hidden()
        .children(shown.length === 0 ? [] : shown.map((message) => this.message(t, colors, message))),
      ...(said === null ? [] : [note(t, said)]),
      this.composer(t, colors),
    ]);
  }

  /**
   * One message: the agent's at the left, the person's at the right.
   *
   * A tool call is a dim mono line under the reply that ran it rather than a
   * bubble of its own (Shepherdr's rule): a person reads what the agent said and
   * glances at what it ran.
   */
  message(t, colors, message) {
    const words = bubble(message.text, this.compact ? COMPACT_MAX : undefined);
    const parts = [];
    if (words !== "") {
      parts.push(
        div()
          .px(8)
          .py(4)
          .rounded(RADIUS.bubble)
          .text_size(TEXT.label)
          .text_color(mine(message) ? t.foreground : t.muted)
          // The agent's words sit in a raised bubble; the person's are marked in
          // the accent, which is how a long column of one voice stays readable.
          .when(!mine(message), (el) => el.bg(t.raised))
          .when(mine(message), (el) => el.border_1().border_color(t.accent))
          .child(words),
      );
    }
    for (const tool of message.tools) {
      parts.push(
        div()
          .px(8)
          .text_size(TEXT.id)
          .font_family("JetBrains Mono")
          .text_color(t.dim)
          .child(`→ ${toolLine(tool)}`),
      );
    }
    if (parts.length === 0) return div();
    return h_flex()
      .w_full()
      .flex_none()
      .justify_end()
      .when(!mine(message), (el) => el.justify_start())
      .child(
        v_flex()
          .max_w(BUBBLE_W)
          .gap(2)
          .children(parts)
          // A short panel has no room for a clock under every bubble; the taller
          // copy (a task's thread) shows it.
          .when(!this.compact && message.at_ms !== null, (el) =>
            el.child(
              div()
                .text_size(TEXT.id)
                .text_color(t.dim)
                .child(when(message.at_ms) || ""),
            ),
          ),
      );
  }

  /** The composer: one line of words, and the send at its end. */
  composer(t, colors) {
    const sendable = this.input.value().trim() !== "" && !this.sending;
    return h_flex()
      .h(SPACE.sidebarRow)
      .flex_none()
      .px(12)
      .pb(8)
      .gap(6)
      .items_center()
      .child(
        h_flex()
          .flex_1()
          .h(24)
          .px(6)
          .items_center()
          .rounded(RADIUS.control)
          .bg(t.input)
          .border_1()
          .border_color(t.border)
          .on_click(() => this.input.focus())
          .child(div().flex_1().text_size(TEXT.label).child(Input.new(this.input))),
      )
      .child(
        h_flex()
          .h(20)
          .w(20)
          .flex_none()
          .items_center()
          .justify_center()
          .rounded(10)
          .bg(sendable ? t.accent : t.raised)
          .text_size(TEXT.label)
          .text_color(sendable ? colors.on : t.dim)
          .cursor_pointer()
          .on_click(() => this.send())
          .child(this.sending ? "…" : "↑"),
      );
  }
}
