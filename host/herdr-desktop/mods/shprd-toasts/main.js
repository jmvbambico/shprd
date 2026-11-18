// Toasts: the news from the service — a task landed, something needs you, a
// review failed, a pass failed — floated over the work area, three at a time.
//
// It is mounted as a floating block over the work area, and it renders nothing
// until there is something to say: a float sizes to its content, so an empty
// render is how it hides.
//
// The transport is the one the README describes and the rest of the window uses:
// find the service, read `/api/state` once, then follow `/api/changes` with the
// long poll (`lib/client.js`), waking also on the change file the service writes
// after every ledger write (`lib/watch.js`). Nothing here polls a fixed fast
// clock; a toast appears when the service says something happened, and the one
// timer is a once-a-second check for a toast whose time is up.
//
// Ported from Shepherdr's toasts (MIT):
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/toasts/main.js` (a dot in the level's colour, a one-line title,
//   a body at three lines, an × on hover, a click that goes to the agent),
//   `core/toasts.js` (`SHOWN`, `WIDTH`, `BODY_MAX`, `timeoutOf`, `clip`)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// What is shprd's own is the dedupe: every toast carries the seq of the event
// that made it (`lib/toasts.js`), so a batch replayed after a restart, or a long
// poll that answers twice, adds nothing.

import { View, div } from "gpui-kit";
import { v_flex, h_flex } from "gpui-base";
import { run, setShared, spaces, theme } from "herdr";

import { attach, alive, POLL_WAIT } from "./lib/client.js";
import { canReview } from "./lib/accept.js";
import { openAccept } from "./lib/panel.js";
import { spaceIndex } from "./lib/spaces.js";
import {
  BODY_MAX,
  LINES,
  WIDTH,
  clip,
  dismiss,
  levelTone,
  mergeNews,
  newsItems,
  passItem,
  visible,
} from "./lib/toasts.js";
import { RADIUS, TEXT, tone } from "./lib/tokens.js";
import { followChanges } from "./lib/watch.js";

/** The body's line height, so three lines are a known height. */
const LINE = 18;
/** How often a toast whose time is up is noticed, ms. */
const EXPIRE_MS = 1000;
/** How long to wait before looking for a service that is not there, ms. */
const RETRY_MS = 5000;

export default class Toasts extends View {
  init(_props, cx) {
    this.cx = cx;
    this.state = null;
    this.client = null;
    this.items = [];
    this.dismissed = new Set();
    this.since = null;
    this.shown = "";
    this.watching = false;
    this.run();
    cx.timer.every(EXPIRE_MS, (cx) => this.expire(cx));
  }

  /**
   * Follows the service for as long as this copy lives: one read of the board,
   * then the long poll, with the change file as the arm that wakes a poll the
   * service restarted under.
   *
   * A restart is noticed before anything is read (`attach` compares the id in
   * `service.json`), and the `since` cursor starts again from the new service's
   * board: a sequence from another run's log names nothing this one wrote (M10
   * gate bug 5).
   */
  run() {
    const cx = this.cx;
    cx.spawn(async () => {
      for (;;) {
        try {
          const next = await attach(this.client, { wait: (ms) => cx.sleep(ms) });
          if (!next.client) {
            this.client = null;
            this.since = null;
            await cx.sleep(RETRY_MS);
            continue;
          }
          if (next.restarted) this.forget();
          this.client = next.client;
          // The change file is watched only once there is a client to follow.
          this.follow();
          if (this.since === null) {
            // What happened before the window opened is the board's to tell, not
            // a toast's: the list starts at the sequence this reading is at.
            const first = await this.client.state();
            this.state = first;
            this.since = Number.isInteger(first?.seq) ? first.seq : 0;
            this.absorb([], this.state);
            cx.notify();
          }
          const change = await this.client.changes(this.since, POLL_WAIT);
          if (change) {
            const ahead = Number.isInteger(change.seq) ? change.seq : this.since;
            this.since = Math.max(this.since, ahead);
            const events = change.events ?? [];
            if (events.length > 0) {
              // The events say what happened; the snapshot says in whose words.
              this.state = await this.client.state();
              this.absorb(events, this.state);
              cx.notify();
            }
          } else if (!(await alive(this.client))) {
            // A long poll that answers nothing is either "nothing happened yet"
            // or a port that has gone away, and only one of those is worth
            // waiting on: ask the handshake, and start again when it fails.
            this.client = null;
            this.since = null;
            await cx.sleep(RETRY_MS);
          }
        } catch {
          // A service that restarted answers with another id, so the next look
          // finds the new port instead of dialling a dead one.
          this.client = null;
          this.since = null;
          await cx.sleep(RETRY_MS);
        }
      }
    });
  }

  /**
   * Forgets the old service's board and cursor.
   *
   * The toasts already shown stay: they are what the person has been told, and a
   * restart is not a reason to say it again. The ones still waiting to be drawn
   * are kept by `mergeNews`' seq, so no line arrives twice.
   */
  forget() {
    this.state = null;
    this.since = null;
  }

  /** Follows the service's change file, so a poll that died is woken anyway. */
  follow() {
    if (this.watching || !this.client) return;
    this.watching = true;
    const cx = this.cx;
    const dir = this.client.dir;
    cx.spawn(async () => {
      await followChanges(dir, () => {
        this.watching = false;
        this.reload(cx);
      });
    });
  }

  /** Re-reads the board once, without waiting for the long poll to answer. */
  reload(cx) {
    cx.spawn(async (cx) => {
      try {
        if (!this.client) return;
        this.state = await this.client.state();
        this.absorb([], this.state);
        cx.notify();
        this.follow();
      } catch {
        this.client = null;
        this.since = null;
      }
    });
  }

  /** Adds what the events and the snapshot say, deduped by their seq. */
  absorb(events, state) {
    const items = newsItems(events, state);
    const pass = passItem(state);
    if (pass) items.push(pass);
    if (items.length === 0) return;
    this.items = mergeNews(this.items, items);
  }

  /** Notices a toast whose time is up, and redraws only when one went. */
  expire(cx) {
    const list = visible(this.items, Date.now(), this.dismissed);
    const key = list.map((item) => item.id).join("|");
    if (key === this.shown) return;
    this.shown = key;
    cx.notify();
  }

  /**
   * A click on a toast goes to the thing it is about.
   *
   * A toast about a task that is ready for review opens the Accept dialog, so the
   * decision it announces is one click away. That entry point is shprd's own:
   * Shepherdr's toast click goes to the agent.
   */
  open(item) {
    const task = (this.state?.tasks ?? []).find((entry) => entry.id === item.task);
    if (task && canReview(task)) {
      openAccept({ task: task.id, title: task.title });
      return;
    }
    // A toast that says a task needs the person opens that task's thread, where
    // the card that asks is in the timeline (the Asks panel is gone).
    if (item.level === "warning" && item.task && task) {
      setShared("tasks.thread", { task: task.id, title: task.title ?? "", at: Date.now() });
      run("shprd-tasks.open");
      return;
    }
    const space = spaceIndex(spaces()).tasks.get(item.task);
    if (space?.id) {
      run("space.select", { id: space.id });
      return;
    }
    run("layout.select", { stack: "panel-body", child: "tasks" });
    run("layout.show", { id: "panel" });
  }

  /** The × on a toast: this one is read, and the person said so. */
  dismiss(id, cx) {
    this.dismissed.add(id);
    this.items = dismiss(this.items, id);
    this.shown = "";
    cx?.notify?.();
  }

  render(cx) {
    const t = theme();
    const colors = tone(t);
    const list = visible(this.items, Date.now(), this.dismissed);
    // A float sizes to its content: nothing to say is nothing drawn.
    if (list.length === 0) return div();
    return v_flex()
      .id("toasts")
      .w(WIDTH)
      .gap(8)
      .children(list.map((item) => this.item(t, colors, item, cx)));
  }

  /**
   * One toast: a dot in its level's colour, a one-line title, a body at three
   * lines, and the × that dismisses it.
   *
   * The dot is filled rather than an icon, so a toast reads at a glance without
   * an asset, and the × always keeps its place so nothing moves under the
   * pointer.
   */
  item(t, colors, item, cx) {
    const shape = levelTone(item.level);
    const fill = shape === "accent" ? t.accent : (colors[shape] ?? t.accent);
    return h_flex()
      .id(`toast-${item.id}`)
      .w_full()
      .px(12)
      .py(10)
      .gap(10)
      .items_start()
      .rounded(RADIUS.card)
      .bg(t.popover)
      .border_1()
      .border_color(t.border)
      .cursor_pointer()
      .hover((style) => style.bg(t.hover))
      .on_click(() => this.open(item))
      .child(div().w(8).h(8).mt(5).flex_none().rounded(4).bg(fill))
      .child(
        v_flex()
          .flex_1()
          .min_w(0)
          .gap(2)
          .child(
            div()
              .min_w(0)
              .truncate()
              .text_size(TEXT.body)
              .font_semibold()
              .text_color(t.foreground)
              .child(item.title ?? ""),
          )
          .when(Boolean(item.body), (el) =>
            el.child(
              div()
                .max_h(LINE * LINES)
                .overflow_hidden()
                .line_height(`${LINE}px`)
                .text_size(TEXT.label)
                .text_color(t.muted)
                .child(clip(item.body, BODY_MAX)),
            ),
          ),
      )
      .child(
        div()
          .id(`toast-x-${item.id}`)
          .size(20)
          .flex_none()
          .flex()
          .items_center()
          .justify_center()
          .rounded(RADIUS.control)
          .text_size(TEXT.body)
          .text_color(t.dim)
          .hover((style) => style.bg(t.hover).text_color(t.foreground))
          .on_click((_event, own) => {
            own?.stop_propagation?.();
            this.dismiss(item.id, own ?? cx);
          })
          .child("×"),
      );
  }
}
