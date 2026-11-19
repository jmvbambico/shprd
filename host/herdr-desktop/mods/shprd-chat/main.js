// The chat: one agent's conversation as bubbles, with the cards that are waiting
// for an answer in the timeline and a composer that sends a turn.
//
// It is mounted twice, because Shepherdr's chat is (`mods/chat/main.js`):
//
// - **in the task thread** (`props.thread`), where it is the thread's default
//   view. Block props are static, so the task it shows is the one the Tasks panel
//   shares (`tasks.thread`), and it follows that as the person moves between tasks;
// - **in a pane** (`props.space`, `props.terminal`), where `shprd-pane`'s
//   Chat · Terminal switch parked the agent's terminal and opened this in its
//   place. That is the coordinator's chat too: its pane has the same switch.
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/chat/main.js` (the bubble rows: the agent's words at the left on
//   `raised`, the person's at the right in an accent tint, markdown in a text view,
//   tool calls folded under the reply, "Show more" / "Show less" under a long
//   reply, the composer and its round send), `mods/chat/cards.js` (the question,
//   permission and plan cards drawn in the timeline), `core/chat-view.js` (the
//   fixed words and `literal`)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// What is shprd's own: the conversation is read from the agent's own record by the
// service (`GET /api/chat`) and a turn is typed into its pane by the service too
// (`POST /api/chat`); the cards are the service's asks and escalations
// (`GET /api/asks`, `GET /api/state`), answered through `POST /api/asks/answer`,
// exactly once. The panel never parses a transcript and never types into a pane.
// Markdown is drawn by the window's own text view, never as HTML: what an agent
// wrote is shown as text, and only a web link may be opened from it.

import { View, div } from "gpui-kit";
import { Input, InputState, ScrollHandle, TextView, h_flex, v_flex } from "gpui-base";
import { exec, props, shared, theme } from "herdr";

import { attach, reason, singleFlight } from "./lib/client.js";
import { answerBody, cards as cardsOf, footer, forSpace } from "./lib/asks.js";
import {
  COORDINATOR,
  WORDS,
  emptyWords,
  empty,
  firstCard,
  fold,
  folding,
  markdownOf,
  mine,
  nextCursor,
  outgoing,
  safeLink,
  spaceOf,
  status,
  timeline,
  toolLine,
  when,
} from "./lib/chat.js";
import { RADIUS, TEXT, tone } from "./lib/tokens.js";
import { followChanges } from "./lib/watch.js";

/** How often the chat reads the conversation when nothing wakes it, ms. */
const FLOOR_MS = 3000;

/** How many pages one refresh takes before leaving the rest to the next look. */
const PAGES = 4;

/** How wide a bubble may be: Shepherdr's readable column, narrower in a panel. */
const BUBBLE_W = 560;

export default class Chat extends View {
  init(_props, cx) {
    this.cx = cx;
    this.thread = props()?.thread === true;
    this.fixed = props()?.space ?? null;
    this.space = this.wanted();
    this.chat = empty(this.space ?? COORDINATOR);
    this.state = null;
    this.asks = null;
    this.client = null;
    this.watching = false;
    this.failure = null;
    // A refused send keeps its words and its reason until the next send: the
    // reads that follow would otherwise clear the line before a person read it.
    this.refusal = null;
    this.sending = false;
    /** Messages opened with "Show more", by key. */
    this.opened = new Set();
    /** The card whose free-text box is open, the card with an answer in flight,
     * and what the service said to a refused answer. */
    this.typing = null;
    this.answering = null;
    this.answerRefusal = null;
    this.draft = null;
    this.scroll = ScrollHandle.new();
    this.pinned = "bottom";
    this.drawn = { rows: 0, cards: 0 };
    this.loading = false;
    this.loadedAt = Date.now();
    this.input = InputState.new({ placeholder: this.placeholder() });
    // A submit is Enter in the composer: the same act as the send button.
    this.input.on("submit", () => this.send());
    this.refresh();
    cx.timer.every(FLOOR_MS, (cx) => this.refresh(cx));
  }

  /** The space to show now: the thread's task (it moves), a pane's own, or the coordinator's. */
  wanted() {
    return spaceOf({ props: { ...props(), space: this.fixed, thread: this.thread }, thread: shared("tasks.thread") });
  }

  /** The composer's hint, by whose chat this is. */
  placeholder() {
    return this.space === COORDINATOR || this.space === null ? "Message the coordinator" : "Message the agent";
  }

  /**
   * The thread follows the task the Tasks panel has open. A different task is a
   * different conversation, so what was read is dropped and the first page asked
   * for again; the newest card, if any, is what it opens on.
   */
  follow_space() {
    const next = this.wanted();
    // Opening the same task again (a needs-you row, a toast) is a new open: it
    // lands on the card again.
    const at = this.thread ? (shared("tasks.thread")?.at ?? null) : null;
    if (next === this.space) {
      if (at !== this.openedAt) {
        this.openedAt = at;
        this.pinned = "card";
      }
      return;
    }
    this.openedAt = at;
    this.space = next;
    this.forget();
    this.pinned = "card";
    this.refresh();
  }

  /**
   * Reads the pages after the cursor and the cards, then shows them.
   *
   * A service that restarted has a new `id`, and its agent's record is a new one
   * too: `attach` notices the restart and the chat starts over from its first
   * page instead of asking a new service for a cursor it never wrote (M10 gate
   * bug 5). A read that answers nothing is a failure, so a dead port is looked
   * for again rather than kept.
   */
  refresh(cx = this.cx) {
    // One read at a time: a refresh asked for while another runs is folded into
    // one more run after it, never started beside it.
    return singleFlight(this, () => this.readOnce(cx));
  }

  /** One read of the service and one repaint; [`refresh`] is what callers use. */
  async readOnce(cx) {
    const space = this.space;
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
      this.state = await this.client.state();
      this.asks = await this.client.asks();
      if (space !== null) {
        // `more` means the page filled up: a long conversation arrives over a few
        // calls rather than one slow one, and the next timer catches any left over.
        for (let page = 0; page < PAGES; page += 1) {
          const answer = await this.client.call(
            "GET",
            `/api/chat?space=${encodeURIComponent(space)}&since=${encodeURIComponent(nextCursor(this.chat))}`,
            { strict: true },
          );
          // The person moved to another task while this was in flight: this answer
          // is for the one they left.
          if (space !== this.space) return;
          this.chat = fold(this.chat, answer);
          if (!this.chat.more) break;
        }
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

  /** Starts the conversation again: a new record names nothing the old cursor did. */
  forget() {
    this.chat = empty(this.space ?? COORDINATOR);
    this.opened = new Set();
    this.refusal = null;
    this.answerRefusal = null;
    this.typing = null;
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

  /** Types the composer's words into the agent, as the person. */
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
      this.pinned = "bottom";
    } catch (err) {
      // The words stay in the composer: a refused line is retyped, not lost.
      this.refusal = reason(err);
    }
    this.sending = false;
    this.cx.notify();
    this.refresh();
  }

  /** Opens or folds one long reply. */
  toggle(key) {
    if (this.opened.has(key)) this.opened.delete(key);
    else this.opened.add(key);
    this.cx.notify();
  }

  /** Opens a link a bubble holds: web pages only, through the system. */
  link(url) {
    if (!safeLink(url)) return;
    exec("open", [url]).catch(() => exec("xdg-open", [url]).catch(() => {}));
  }

  /** Opens the free-text box on a card, making the one input on first use. */
  type(card) {
    this.typing = this.typing === card.id ? null : card.id;
    if (this.typing !== null && !this.draft) {
      this.draft = InputState.new({ placeholder: "Type your answer", value: "" });
    }
    this.answerRefusal = null;
    this.cx.notify();
    // Opening the box gives it the keyboard: without this the keys keep going to
    // the composer that had it. The box is drawn by the render this notify asks
    // for, so the focus is taken again once it is on screen (a mod has no
    // `setTimeout`; the app's timer is the way, as the add-task form does it).
    if (this.typing !== null) {
      const id = this.typing;
      this.draft.focus();
      this.cx.timer?.after?.(50, () => {
        if (this.typing === id) this.draft?.focus();
      });
    }
  }

  /** Answers one card; the service takes it exactly once. */
  async answer(card, text) {
    const body = answerBody(card, text);
    if (!body || !this.client || this.answering !== null) return;
    this.answering = card.id;
    this.answerRefusal = null;
    this.cx.notify();
    try {
      await this.client.answerAsk(body.task, body.ask, body.answer);
      this.typing = null;
      this.draft?.set_value?.("");
      this.answering = null;
      await this.refresh();
      return;
    } catch (err) {
      this.answerRefusal = { card: card.id, text: reason(err) };
    }
    this.answering = null;
    this.cx.notify();
  }

  /** Runs the one action the service has for an escalation, when it has one. */
  async act(card) {
    if (!card.action || !this.client || this.answering !== null) return;
    this.answering = card.id;
    this.answerRefusal = null;
    this.cx.notify();
    try {
      await this.client.action(card.action);
      this.answering = null;
      await this.refresh();
      return;
    } catch (err) {
      this.answerRefusal = { card: card.id, text: reason(err) };
    }
    this.answering = null;
    this.cx.notify();
  }

  /**
   * Reads again when the chat is drawn with an old answer.
   *
   * A block that is hidden in a stack (the thread's chat, while its terminal is
   * shown) is not guaranteed to keep the timer `init` started, so being drawn is
   * also when a stale chat catches up. `loading` keeps one read in flight and the
   * floor keeps a repaint from becoming a poll.
   */
  revalidate() {
    if (this.loading || Date.now() - this.loadedAt < FLOOR_MS) return;
    this.loading = true;
    this.refresh().finally(() => {
      this.loading = false;
      this.loadedAt = Date.now();
    });
  }

  render() {
    if (this.thread || this.fixed === null) this.follow_space();
    this.revalidate();
    const t = theme();
    const colors = tone(t);
    const list = this.space === null ? [] : forSpace(cardsOf(this.state, this.asks), this.space);
    const rows = timeline(this.chat, list);
    this.reveal(rows, list);
    const said = this.refusal ?? this.failure ?? (rows.length === 0 ? status(this.chat) : null);
    return v_flex()
      .size_full()
      .bg(t.background)
      .child(
        v_flex()
          .id("chat-rows")
          .flex_1()
          .min_h(0)
          .overflow_y_scrollbar()
          .track_scroll(this.scroll)
          .children(
            rows.length === 0
              ? [this.blank(t, said)]
              : [...rows.map((row) => this.row(t, colors, row)), div().h(10).flex_none()],
          ),
      )
      .when(said !== null && rows.length > 0, (el) =>
        el.child(div().flex_none().px(14).pb(4).text_size(TEXT.label).text_color(t.muted).child(said)),
      )
      .child(this.composer(t, colors));
  }

  /**
   * Scrolls to what the person came for. A new task opens on its first waiting
   * card (the "needs you" row and the toast land there), anything else on the
   * newest row; a card that arrived while it was shown is looked at too.
   */
  reveal(rows, list) {
    const cards = list.length;
    const grew = rows.length > this.drawn.rows || cards > this.drawn.cards;
    if (!grew && this.pinned === null) {
      this.drawn = { rows: rows.length, cards };
      return;
    }
    if (rows.length > 0 && (grew || this.pinned !== null)) {
      const card = firstCard(rows);
      // The newest is the spacer after the last row, so a tall last reply shows its end.
      const at = (this.pinned === "card" || cards > this.drawn.cards) && card !== -1 ? card : rows.length;
      this.scroll.scroll_to_item(at);
      // A "card" request waits for the rows it names; the rest are spent now.
      if (this.pinned !== "card" || card !== -1 || this.chat.messages.length > 0) this.pinned = null;
    }
    this.drawn = { rows: rows.length, cards };
  }

  /** What an empty timeline says, in the panel and not in a tooltip. */
  blank(t, said) {
    return v_flex()
      .w_full()
      .flex_1()
      .px(24)
      .py(32)
      .gap(6)
      .items_center()
      .child(div().text_size(TEXT.body).font_medium().text_color(t.foreground).child(WORDS.emptyTitle))
      .child(div().text_size(TEXT.label).text_color(t.muted).child(said ?? emptyWords(this.space)));
  }

  /** One row of the timeline. */
  row(t, colors, row) {
    if (row.kind === "top") {
      return div()
        .w_full()
        .flex_none()
        .py(8)
        .flex()
        .justify_center()
        .text_size(TEXT.label)
        .text_color(t.dim)
        .child(WORDS.top);
    }
    if (row.kind === "card") return this.card(t, colors, row.card);
    return this.message(t, row);
  }

  /**
   * One message: the agent's at the left, the person's at the right.
   *
   * A tool call is a dim mono line under the reply that ran it rather than a
   * bubble of its own (Shepherdr's rule); a long reply folds under a "Show more".
   */
  message(t, row) {
    const message = row.message;
    const open = this.opened.has(row.key);
    const shown = folding(message.text, open);
    const parts = [];
    if (message.text.trim() !== "") {
      parts.push(
        div()
          .px(12)
          .py(8)
          .min_w(0)
          .rounded(RADIUS.bubble)
          .text_size(TEXT.body)
          .text_color(t.foreground)
          // The agent's words sit in a raised bubble; the person's are an accent
          // tint, which is how a long column of one voice stays readable.
          .when(!mine(message) && message.role !== "news", (el) => el.bg(t.raised))
          // What the service told the agent is a quiet news line, not a bubble.
          .when(message.role === "news", (el) => el.text_color(t.muted).text_size(TEXT.label).border_1().border_color(t.dim + "66"))
          .when(mine(message), (el) => el.bg(t.accent + "33").border_1().border_color(t.accent + "4d"))
          .child(
            TextView.markdown(`chat-md-${row.key}${open ? "" : shown.folded ? "-f" : ""}`, markdownOf(message, shown.text) || " ")
              .selectable(true)
              .on_link_click((url) => this.link(url)),
          ),
      );
    }
    if (shown.foldable) {
      parts.push(
        div()
          .id(`chat-fold-${row.key}`)
          .px(4)
          .text_size(TEXT.label)
          .text_color(t.muted)
          .cursor_pointer()
          .hover((s) => s.text_color(t.foreground))
          .on_click(() => this.toggle(row.key))
          .child(open ? WORDS.showLess : WORDS.showMore),
      );
    }
    for (const tool of message.tools) {
      parts.push(
        div()
          .px(4)
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
      .px(14)
      .py(5)
      .justify_end()
      .when(!mine(message), (el) => el.justify_start())
      .child(
        v_flex()
          .max_w(BUBBLE_W)
          .min_w(0)
          .gap(3)
          .when(mine(message), (el) => el.items_end())
          .children(parts)
          .when(message.at_ms !== null, (el) =>
            el.child(div().px(4).text_size(TEXT.id).text_color(t.dim).child(when(message.at_ms) || "")),
          ),
      );
  }

  /**
   * One waiting card, in the timeline (Shepherdr's `chat/cards.js`): what the agent
   * asks, the choices as buttons, and a box for words where the card takes them.
   * An escalation is the engine's, with the verb a person still has to run.
   */
  card(t, colors, card_) {
    const tone_ = card_.from === "ask" ? colors.red : colors.amber;
    const refused = this.answerRefusal?.card === card_.id;
    const busy = this.answering === card_.id;
    return div()
      .w_full()
      .flex_none()
      .px(14)
      .py(5)
      .child(
        v_flex()
          .id(`chat-card-${card_.id}`)
          .p(10)
          .gap(6)
          .rounded(RADIUS.card)
          .border_1()
          .border_color(refused ? colors.red : tone_)
          .bg(t.raised)
          .child(
            h_flex()
              .gap(6)
              .items_center()
              .child(div().flex_none().text_size(TEXT.id).font_medium().text_color(tone_).child(card_.title))
              .child(div().flex_1())
              .when(footer(card_) !== "", (el) =>
                el.child(div().flex_none().text_size(TEXT.id).text_color(t.dim).child(footer(card_))),
              ),
          )
          .child(div().text_size(TEXT.body).text_color(t.foreground).child(card_.text))
          .when(card_.verb !== null, (el) =>
            el.child(
              h_flex()
                .gap(6)
                .items_center()
                .child(
                  div()
                    .flex_1()
                    .min_w(0)
                    .font_family("JetBrains Mono")
                    .text_size(TEXT.id)
                    .text_color(colors.amber)
                    .child(card_.verb),
                )
                .when(card_.action !== null, (inner) => inner.child(this.button(t, card_.actionLabel ?? "Run", () => this.act(card_), busy))),
            ),
          )
          .when(card_.verb === null && card_.action !== null, (el) =>
            el.child(
              h_flex()
                .gap(6)
                .child(this.button(t, card_.actionLabel ?? "Run", () => this.act(card_), busy)),
            ),
          )
          .when(card_.options.length > 0, (el) =>
            el.child(
              h_flex()
                .gap(6)
                .flex_wrap()
                .children(
                  card_.options.map((option) =>
                    this.button(t, option.label, () => this.answer(card_, option.answer), busy),
                  ),
                ),
            ),
          )
          .when(card_.freeText, (el) =>
            el.child(
              this.typing === card_.id
                ? h_flex()
                    .gap(6)
                    .items_center()
                    .child(div().flex_1().min_w(0).child(Input.new(this.draft)))
                    .child(this.button(t, "Send", () => this.answer(card_, this.draft?.value?.() ?? ""), busy))
                    .child(this.button(t, "Cancel", () => this.type(card_), false))
                : this.button(t, "Type an answer", () => this.type(card_), busy),
            ),
          )
          .when(refused, (el) =>
            el.child(div().text_size(TEXT.label).text_color(colors.red).child(this.answerRefusal.text)),
          ),
      );
  }

  /** One small text button: the card's only act. */
  button(t, word, onClick, busy) {
    return h_flex()
      .h(24)
      .flex_none()
      .px(8)
      .items_center()
      .rounded(RADIUS.control)
      .border_1()
      .border_color(t.input ?? t.border)
      .text_size(TEXT.label)
      .text_color(busy ? t.dim : t.foreground)
      .when(!busy, (el) => el.cursor_pointer().hover((style) => style.bg(t.hover)).on_click(() => onClick()))
      .child(word);
  }

  /** The composer: one line of words, and the round send at its end. */
  composer(t, colors) {
    const sendable = this.input.value().trim() !== "" && !this.sending && this.space !== null;
    return h_flex()
      .flex_none()
      .px(14)
      .pt(6)
      .pb(12)
      .gap(8)
      .items_center()
      .child(
        h_flex()
          .flex_1()
          .h(32)
          .px(10)
          .items_center()
          .rounded(RADIUS.bubble)
          .bg(t.input)
          .border_1()
          .border_color(t.border)
          .on_click(() => this.input.focus())
          .child(div().flex_1().text_size(TEXT.body).child(Input.new(this.input))),
      )
      .child(
        h_flex()
          .id("chat-send")
          .h(28)
          .w(28)
          .flex_none()
          .items_center()
          .justify_center()
          .rounded(14)
          .bg(sendable ? t.accent : t.raised)
          .text_size(TEXT.body)
          .text_color(sendable ? colors.on : t.dim)
          .cursor_pointer()
          .on_click(() => this.send())
          .child(this.sending ? "…" : "↑"),
      );
  }
}
