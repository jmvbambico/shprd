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
import { Input, InputState, ScrollHandle, Textarea, TextareaState, TextView, h_flex, v_flex } from "gpui-base";
import { exec, props, setShared, shared, theme } from "herdr";

import { RepoCache, SEND_PROBLEM, activeFolder, attach, downNotice, press, reason, singleFlight } from "./lib/client.js";
import { answeredAsks, errorLine, noticeLine, overlay, read as readOps } from "./lib/optimistic.js";
import { answerBody, cards as cardsOf, footer, forSpace } from "./lib/asks.js";
import {
  COORDINATOR,
  GUTTER,
  bubbleSpan,
  WORDS,
  emptyWords,
  coordinatorDown,
  empty,
  firstCard,
  lineCount,
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
  toolsSummary,
  when,
} from "./lib/chat.js";
import { RADIUS, TEXT, tone } from "./lib/tokens.js";
import { followChanges } from "./lib/watch.js";

/** How often the chat reads the conversation when nothing wakes it, ms. */
const FLOOR_MS = 3000;

/** The longest one read of the service may take before the chat says so, ms. */
const READ_MS = 20000;

/** The longest "Loading…" is drawn for a conversation, ms: after it, the empty chat or the reason. */
const LOADING_MS = 4000;

/** How many pages one refresh takes before leaving the rest to the next look. */
const PAGES = 4;


export default class Chat extends View {
  init(_props, cx) {
    this.cx = cx;
    /** The pane's width as the window reports it; Shepherdr starts at 600 too. */
    this.width = 600;
    this.thread = props()?.thread === true;
    this.fixed = props()?.space ?? null;
    this.space = this.wanted();
    this.chat = empty(this.space ?? COORDINATOR);
    // Each conversation (repository + agent) is kept for the window's life, so
    // coming back to one shows it at once and the read only adds what is new; one
    // never read says "Loading…", not "No messages yet".
    this.keys = new RepoCache();
    this.chats = new Map();
    this.loaded = new Set();
    this.slot = this.slotKey();
    this.slotSince = Date.now();
    this.state = null;
    this.asks = null;
    this.client = null;
    this.watching = false;
    this.failure = null;
    // A refused send keeps its words and its reason until the next send: the
    // reads that follow would otherwise clear the line before a person read it.
    this.refusal = null;
    /** What shprd did first on the last send (made the folder a repository). */
    this.notice = null;
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
    // A text area, because the person pastes whole prompts: Enter sends, Shift+Enter
    // is a new line, and the line breaks they typed reach the agent.
    this.input = TextareaState.new({ placeholder: this.placeholder() });
    this.input.set_rows?.(1);
    this.inputRows = 1;
    this.input.on("submit", (e) => {
      if (!e?.shift) this.send();
    });
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
    return singleFlight(this, () => this.bounded(cx));
  }

  /**
   * One read that cannot hang the chat: a call that never settles would keep the
   * one read in flight for ever (and with it "Loading…"), so after [`READ_MS`] the
   * read is given up as a failure, the client is forgotten and the next beat
   * looks again.
   */
  async bounded(cx) {
    const start = Date.now();
    // Each read has a generation; a read given up on is stale and changes nothing
    // if it ever finishes (a newer read owns the state by then).
    this.gen = (this.gen ?? 0) + 1;
    const read = this.readOnce(cx, this.gen).then(() => "done");
    // A clock that fires early (a stub's) is not a read that took too long.
    const gave = (cx?.sleep?.(READ_MS) ?? new Promise(() => {})).then(() => (Date.now() - start >= READ_MS - 100 ? "late" : "early"));
    let outcome = await Promise.race([read, gave]);
    if (outcome === "early") outcome = await read;
    if (outcome === "late") {
      this.gen += 1;
      this.client = null;
      this.failure = downNotice();
      cx.notify();
    }
  }

  /** One read of the service and one repaint; [`refresh`] is what callers use. */
  async readOnce(cx, gen = this.gen) {
    const space = this.space;
    const stale = () => gen !== this.gen;
    try {
      const next = await attach(this.client, { wait: (ms) => cx.sleep(ms) });
      if (stale()) return;
      if (!next.client) {
        this.client = null;
        this.failure = downNotice();
        cx.notify();
        return;
      }
      if (next.restarted) {
        // The same service shown for another repository keeps every conversation;
        // a service that restarted wrote none of the cursors held.
        const sameService = this.client !== null && this.client.id() === next.client.id();
        this.forget(!sameService);
      }
      this.client = next.client;
      this.follow();
      // The conversation this read is for: a switch of space while it runs makes
      // its answer another conversation's, which is not folded into this one.
      if (this.slot !== undefined && this.slotKey() !== this.slot) this.forget(false);
      const slot = this.slot;
      const board = await this.client.state();
      const asks = await this.client.asks();
      if (stale()) return;
      this.state = board;
      this.asks = asks;
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
          if (space !== this.space || slot !== this.slot || stale()) return;
          this.chat = fold(this.chat, answer);
          if (!this.chat.more) break;
        }
      }
      this.failure = null;
      if (space === this.space && slot === this.slot) this.loaded?.add(slot);
    } catch (err) {
      if (stale()) return;
      // A restarted service answers with another id: forget the client so the
      // next look finds the new port instead of dialling a dead one.
      this.client = null;
      this.failure = reason(err);
    }
    cx.notify();
  }

  /** Starts the conversation again: a new record names nothing the old cursor did. */
  forget(reset = false) {
    const now = this.slotKey();
    if (this.chats) {
      if (this.slot !== undefined) this.chats.set(this.slot, { chat: this.chat, opened: this.opened });
      if (reset) {
        this.chats.clear();
        this.loaded.clear();
      }
    }
    const kept = this.chats?.get(now) ?? null;
    this.slot = now;
    this.slotSince = Date.now();
    this.chat = kept?.chat ?? empty(this.space ?? COORDINATOR);
    this.opened = kept?.opened ?? new Set();
    this.refusal = null;
    this.answerRefusal = null;
    this.typing = null;
    this.sending = false;
  }

  /** The key of the conversation shown: the repository's folder and the agent's space. */
  slotKey() {
    return `${this.keys?.key() ?? ""}|${this.space ?? ""}`;
  }

  /** Follows the service's change file for as long as this copy lives. */
  follow() {
    if (!this.client || this.watching === this.client.dir) return;
    // The service follows the shown repository: a new directory is a new watch,
    // and the loop on the old one ends at its next look.
    this.watching = this.client.dir;
    const cx = this.cx;
    const dir = this.client.dir;
    cx.spawn(async () => {
      await followChanges(dir, () => this.refresh(cx), { stop: () => this.watching !== dir });
    });
  }

  /** Types the composer's words into the agent, as the person. */
  async send() {
    const body = outgoing(this.space, this.input.value());
    if (body === null || this.sending) return;
    if (!this.client) {
      this.refusal = downNotice();
      this.cx.notify();
      return;
    }
    this.sending = true;
    this.refusal = null;
    this.cx.notify();
    try {
      const reply = await this.client.call("POST", "/api/chat", { body });
      this.input.set_value("");
      this.pinned = "bottom";
      // One plain line about what shprd did first (it made the folder a git
      // repository); it stays above the composer for this conversation.
      if (typeof reply?.notice === "string" && reply.notice !== "") this.notice = reply.notice;
      setShared(SEND_PROBLEM, null);
    } catch (err) {
      // The words stay in the composer: a refused line is retyped, not lost.
      this.refusal = reason(err);
      // The sidebar's foot says the same sentence, for the folder shown.
      setShared(SEND_PROBLEM, { folder: this.client?.folder ?? activeFolder(), text: this.refusal });
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
    // The card is gone and its task resumes at once; a refusal puts it back.
    const remaining = (this.asks?.asks ?? []).filter((ask) => ask.task === body.task).length;
    const sent = await press(this, {
      kind: "ask.answer",
      tasks: [body.task],
      extra: { ask: body.ask, remaining },
      send: () => this.client.answerAsk(body.task, body.ask, body.answer),
    });
    if (sent.ok) {
      this.typing = null;
      this.draft?.set_value?.("");
      this.answering = null;
      await this.refresh();
      return;
    }
    if (!sent.ignored) this.answerRefusal = { card: card.id, text: sent.reason };
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
    // Another repository's space: its conversation as last read is drawn now and
    // the read adds what is new (the same service, so its cursors still hold).
    if (this.slot !== undefined && this.slotKey() !== this.slot) {
      this.forget(false);
      this.refresh();
    }
    this.revalidate();
    const t = theme();
    const colors = tone(t);
    // What the person just pressed shows at once: the board with the pending actions
    // applied, and the questions they answered already gone (`tasks.optimistic`).
    const folder = this.client?.folder ?? activeFolder();
    const board = overlay(this.state, folder, readOps().ops);
    const gone = answeredAsks(folder);
    const open = this.asks ? { ...this.asks, asks: (this.asks.asks ?? []).filter((ask) => !gone.has(`${ask.task}/${ask.id}`)) } : this.asks;
    const list = this.space === null ? [] : forSpace(cardsOf(board, open), this.space);
    const rows = timeline(this.chat, list);
    this.reveal(rows, list);
    const said =
      this.refusal ??
      this.failure ??
      errorLine(board) ??
      noticeLine(board) ??
      coordinatorDown(this.space, this.state) ??
      (rows.length === 0 ? status(this.chat) : null);
    return v_flex()
      .size_full()
      .bg(t.background)
      .on_size_change((size, cx) => {
        const w = size?.width ?? 0;
        if (Math.abs(w - this.width) > 1) {
          this.width = w;
          cx?.notify?.();
        }
      })
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
      .when(this.space === COORDINATOR && Boolean(this.notice), (el) =>
        el.child(this.column(div().pb(4).text_size(TEXT.label).text_color(t.muted).child(this.notice))),
      )
      .when(said !== null && rows.length > 0, (el) =>
        el.child(this.column(div().pb(4).text_size(TEXT.label).text_color(t.muted).child(said))),
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

  /**
   * One row's place: the pane's full width, `GUTTER` px in from each edge (as
   * Shepherdr's 36). Every row, card and the composer sit in it, so the edges line
   * up at every pane width.
   */
  column(el) {
    return h_flex()
      .w_full()
      .flex_none()
      .justify_center()
      .child(div().w_full().px(GUTTER).child(el));
  }

  /** What an empty timeline says, in the panel and not in a tooltip. */
  blank(t, said) {
    // "Loading…" is for the first moments of a conversation never read: once the
    // read has failed, or taken longer than LOADING_MS, the chat is the empty one
    // (or says why), never a spinner for ever.
    if (this.loaded && !this.loaded.has(this.slot) && this.failure === null) {
      if (Date.now() - this.slotSince < LOADING_MS) {
        // Repaint when the wait is over, whatever the read did.
        if (this.timed !== this.slotSince) {
          this.timed = this.slotSince;
          this.cx.spawn(async () => {
            await this.cx.sleep(LOADING_MS);
            this.cx.notify();
          });
        }
        return v_flex().w_full().flex_1().px(24).py(32).items_center().child(div().text_size(TEXT.label).text_color(t.muted).child("Loading…"));
      }
    }
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
    if (row.kind === "tools") return this.tools(t, row);
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
    if (parts.length === 0) return div();
    return this.column(
      h_flex()
      .w_full()
      .py(5)
      .justify_end()
      .when(!mine(message), (el) => el.justify_start())
      .child(
        v_flex()
          // A definite width: sized by its content alone, a short message collapsed to one
          // letter per row (see `bubbleSpan`).
          .w(bubbleSpan(shown.text, this.width))
          .min_w(0)
          .gap(3)
          .children(parts)
          .when(message.at_ms !== null, (el) =>
            el.child(
              div()
                .w_full()
                .flex()
                .px(4)
                .when(mine(message), (inner) => inner.justify_end())
                .text_size(TEXT.id)
                .text_color(t.dim)
                .child(when(message.at_ms) || ""),
            ),
          ),
      ),
    );
  }

  /**
   * A run of tool calls as one folded row (Shepherdr's "tools" row): "Ran a
   * command", "Read a file, ran 2 commands", opening to each call on a click.
   */
  tools(t, row) {
    const open = this.opened.has(row.key);
    return this.column(
      v_flex()
      .id(`chat-tools-${row.key}`)
      .w_full()
      .py(3)
      .child(
        v_flex()
          .min_w(0)
          .px(4)
          .gap(3)
          .child(
            h_flex()
              .gap(6)
              .items_center()
              .cursor_pointer()
              .text_size(TEXT.label)
              .text_color(t.dim)
              .hover((s) => s.text_color(t.foreground))
              .on_click(() => this.toggle(row.key))
              .child(div().flex_none().w(10).child(open ? "⌄" : "›"))
              .child(div().flex_1().min_w(0).truncate().child(toolsSummary(row.calls))),
          )
          .when(open, (el) =>
            el.child(
              v_flex()
                .pl(16)
                .gap(2)
                .children(
                  row.calls.map((call) =>
                    div()
                      .text_size(TEXT.id)
                      .font_family("JetBrains Mono")
                      .text_color(t.muted)
                      .truncate()
                      .child(toolLine(call)),
                  ),
                ),
            ),
          ),
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
    return this.column(
      div()
      .w_full()
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
    // The box grows with what is in it, up to eight lines, then scrolls.
    const lines = Math.min(8, Math.max(1, lineCount(this.input.value(), 70)));
    if (lines !== this.inputRows) {
      this.inputRows = lines;
      this.input.set_rows?.(lines);
    }
    const sendable = this.input.value().trim() !== "" && !this.sending && this.space !== null;
    return this.column(
      h_flex()
      .pt(6)
      .pb(12)
      .gap(8)
      .items_center()
      .child(
        h_flex()
          .flex_1()
          .min_h(32)
          .px(10)
          .py(4)
          .items_center()
          .rounded(RADIUS.bubble)
          .bg(t.input)
          .border_1()
          .border_color(t.border)
          .on_click(() => this.input.focus())
          .child(div().flex_1().min_w(0).text_size(TEXT.body).child(Textarea.new(this.input))),
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
      ),
    );
  }
}
