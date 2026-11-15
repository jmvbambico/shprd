// The needs-you panel: every open ask and every escalation, each with the answer
// it takes.
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/chat/main.js` (the question, permission, plan and secret cards
//   drawn in the conversation: the question, the numbered options, the composer
//   that answers one), `core/asks.js` (the queue's open/answered split)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// shprd has two sources for a card and the panel draws both, because the run
// records two different things (see `lib/asks.js`): an **ask** is a file a
// program's hook raised and is waiting on (`GET /api/asks`, answered through
// `POST /api/asks/answer`), and an **escalation** is a log line where the engine
// stopped (`GET /api/state`'s `needs`). A spent review's escalation names the verb
// a person still has to run — M10.0's `shprd_views::cards` writes it — and the
// card shows that verb on its own line.
//
// The service is the only writer: this panel answers an ask through the same
// route the web app and the CLI do, and never answers one itself.

import { View, div } from "gpui-kit";
import { Input, InputState, h_flex, v_flex } from "gpui-base";
import { theme } from "herdr";

import { Client, discover, reason } from "./lib/client.js";
import { SECTION, answerBody, cards, footer, section, tally } from "./lib/asks.js";
import { badge, card, cardHeader, countBadge, keycap, note } from "./lib/panel.js";
import { RADIUS, TEXT, ref, tone } from "./lib/tokens.js";
import { followChanges } from "./lib/watch.js";

/** How often the panel re-reads the service when the change file says nothing. */
const FLOOR_MS = 4000;

export default class Asks extends View {
  init(_props, cx) {
    this.cx = cx;
    this.state = null;
    this.asks = null;
    this.failure = null;
    this.client = null;
    this.watching = false;
    /** The card whose free-text box is open, by id. */
    this.typing = null;
    /** The card with an answer in flight, by id. */
    this.answering = null;
    /** What the service said to a refused answer: its words, unedited. */
    this.refusal = null;
    /** The one input a typed answer goes in. It is made on the first click that
     * needs it: a retained text state needs the host, and `render` may not ask. */
    this.draft = null;
    this.loading = false;
    this.loadedAt = 0;
    this.looping = false;
    this.refresh();
    this.ensureLoop();
  }

  /** Reads the cards once; a service that went away is looked for again. */
  async refresh(cx = this.cx) {
    try {
      if (!this.client) {
        const found = await discover({ wait: (ms) => cx.sleep(ms) });
        if (!found) {
          this.state = null;
          this.asks = null;
          this.failure = "shprd serve is not running";
          cx.notify();
          return;
        }
        this.client = new Client(found, { wait: (ms) => cx.sleep(ms) });
        this.follow();
      }
      const state = await this.client.state();
      if (state) this.state = state;
      const asks = await this.client.asks();
      if (asks) this.asks = asks;
      this.failure = null;
    } catch (err) {
      // A restarted service answers with another id, so the next look finds the
      // new port rather than dialling a dead one.
      this.client = null;
      this.failure = reason(err);
    }
    cx.notify();
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

  /** Opens the free-text box on `card`, making the one input on first use. */
  type(card) {
    this.typing = this.typing === card.id ? null : card.id;
    if (this.typing !== null && !this.draft) {
      this.draft = InputState.new({ placeholder: "Type your answer", value: "" });
    }
    this.refusal = null;
    this.cx.notify();
  }

  /** Answers one card; the service takes it exactly once. */
  async answer(card, text) {
    const body = answerBody(card, text);
    if (!body || !this.client || this.answering !== null) return;
    this.answering = card.id;
    this.refusal = null;
    this.cx.notify();
    try {
      await this.client.answerAsk(body.task, body.ask, body.answer);
      this.typing = null;
      this.draft?.set_value?.("");
      this.answering = null;
      await this.refresh();
      return;
    } catch (err) {
      this.refusal = { card: card.id, text: reason(err) };
    }
    this.answering = null;
    this.cx.notify();
  }

  /** Sends the typed answer of `card`. */
  send(card) {
    return this.answer(card, this.draft?.value?.() ?? "");
  }

  /** Runs the one action the service has for a card, when it has one. */
  async act(card) {
    if (!card.action || !this.client || this.answering !== null) return;
    this.answering = card.id;
    this.refusal = null;
    this.cx.notify();
    try {
      await this.client.action(card.action);
      this.answering = null;
      await this.refresh();
      return;
    } catch (err) {
      this.refusal = { card: card.id, text: reason(err) };
    }
    this.answering = null;
    this.cx.notify();
  }

  /**
   * Reads again when the panel is drawn with an old answer.
   *
   * A block that is hidden in a stack is not guaranteed to keep its timer, so a
   * panel shown after a while must not sit on what it read before: the moment
   * it is drawn is the moment to catch up. `loading` keeps one read in flight,
   * and the floor keeps a repaint from becoming a poll.
   */
  revalidate() {
    // A block hidden in a stack is not guaranteed to keep the timer `init`
    // started, so being drawn is also when the poll is (re)started.
    this.ensureLoop();
    this.follow();
    if (this.loading) return;
    if (Date.now() - this.loadedAt < FLOOR_MS) return;
    this.loading = true;
    this.refresh().finally(() => {
      this.loading = false;
      this.loadedAt = Date.now();
    });
  }

  /** Polls the service while this copy is alive, once. */
  ensureLoop() {
    if (this.looping) return;
    this.looping = true;
    const cx = this.cx;
    cx.timer.every(FLOOR_MS, (cx) => this.refresh(cx));
  }

  render() {
    this.revalidate();
    const t = theme();
    const colors = tone(t);
    const list = cards(this.state, this.asks);
    const counted = tally(list);
    const head = h_flex()
      .gap(6)
      .items_center()
      .child(div().text_size(TEXT.body).font_medium().text_color(t.foreground).child("Needs you"))
      .when(counted.ask > 0, (el) =>
        el.child(badge(t, countBadge(counted.ask), colors.red, colors.on)),
      )
      .child(div().flex_1())
      .child(keycap(t, "M10.6"));

    const body = v_flex().flex_1().min_h(0).py(6).gap(8).children(
      this.failure !== null
        ? [
            note(t, this.failure),
            note(t, "Start it from the sidebar's footer; the cards fill in when it answers."),
          ]
        : list.length === 0
          ? [
              note(t, "Nothing needs you."),
              note(t, "A card lands here when an agent asks, or when the engine stops on a decision."),
            ]
          : [
              ...this.group(t, colors, SECTION.ask, section(list, "ask")),
              ...this.group(t, colors, SECTION.escalation, section(list, "escalation")),
            ],
    );

    return card(t, [cardHeader(t, head), body]);
  }

  /** One section: its heading with a count, then its cards. */
  group(t, colors, word, list) {
    if (list.length === 0) return [];
    return [
      h_flex()
        .flex_none()
        .px(12)
        .gap(6)
        .items_center()
        .text_size(TEXT.label)
        .text_color(t.dim)
        .child(word)
        .child(String(list.length)),
      ...list.map((card_) => this.card(t, colors, card_)),
    ];
  }

  /** One card, in an outlined block: its heading, its words, and its answer. */
  card(t, colors, card_) {
    const tone_ = card_.from === "ask" ? colors.red : colors.amber;
    const refused = this.refusal?.card === card_.id;
    const busy = this.answering === card_.id;
    return v_flex()
      .flex_none()
      .mx(8)
      .p(10)
      .gap(6)
      .rounded(RADIUS.card)
      .border_1()
      .border_color(refused ? colors.red : t.border)
      .bg(t.raised)
      .child(
        h_flex()
          .gap(6)
          .items_center()
          .child(
            div()
              .flex_none()
              .font_family("JetBrains Mono")
              .text_size(TEXT.id)
              .text_color(t.muted)
              .child(card_.task ? ref(card_.task) : "the run"),
          )
          .child(
            div().flex_none().text_size(TEXT.id).text_color(tone_).child(card_.title),
          )
          .child(div().flex_1())
          .when(footer(card_) !== "", (el) =>
            el.child(
              div().flex_none().text_size(TEXT.id).text_color(t.dim).child(footer(card_)),
            ),
          ),
      )
      .child(
        div()
          .text_size(TEXT.label)
          .text_color(t.foreground)
          .child(card_.text),
      )
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
            .when(card_.action !== null, (inner) =>
              inner.child(this.button(t, "Run", () => this.act(card_), busy)),
            ),
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
                .child(this.button(t, "Send", () => this.send(card_), busy))
                .child(this.button(t, "Cancel", () => this.type(card_), false))
            : this.button(t, "Type an answer", () => this.type(card_), busy),
        ),
      )
      .when(refused, (el) =>
        el.child(
          div()
            .text_size(TEXT.label)
            .text_color(colors.red)
            .child(this.refusal.text),
        ),
      );
  }

  /** One small text button: the panel's only act. */
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
      .when(!busy, (el) =>
        el.cursor_pointer().hover((style) => style.bg(t.hover)).on_click(() => onClick()),
      )
      .child(word);
  }
}
