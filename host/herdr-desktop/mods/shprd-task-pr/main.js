// The task's pull request: when a task lands through a forge, its number, its
// state and the link a person opens.
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/task-pr/main.js` (the state word in a soft tint beside the
//   number, the size of the change, the link, and a quiet line when there is
//   nothing to show)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// What is shprd's own: nothing is asked of a forge here. shprd lands every task
// through its own pull request when the repository has one and merges locally
// when it has none, and the log already holds the number, the URL and whether it
// merged — `GET /api/state` hands all three to a client. So this view reads the
// board and never `gh`: no thirty-second poll, and nothing to look at when the
// forge is down.
//
// A local landing gets one quiet line, not an empty pane: there will never be a
// pull request for it, and a person looking for one should learn that here.

import { View, div } from "gpui-kit";
import { h_flex, v_flex } from "gpui-base";
import { shared, theme } from "herdr";

import { Client, discover, reason } from "./lib/client.js";
import { taskById } from "./lib/board.js";
import { card as surface, cardHeader, note } from "./lib/panel.js";
import { PR_WORDS, openUrl, taskPr } from "./lib/taskpr.js";
import { RADIUS, TEXT, tone } from "./lib/tokens.js";
import { followChanges } from "./lib/watch.js";

/** How often the card reads the board when the change file says nothing. */
const FLOOR_MS = 4000;

export default class TaskPr extends View {
  init(_props, cx) {
    this.cx = cx;
    this.state = null;
    this.client = null;
    this.failure = null;
    this.watching = false;
    this.refresh();
    cx.timer.every(FLOOR_MS, (cx) => this.refresh(cx));
  }

  /** The task the panel picked, from the one shared selection. */
  selected() {
    const thread = shared("tasks.thread");
    return taskById(this.state, thread?.task ?? null);
  }

  /** Reads the board once; a service that went away is looked for again. */
  async refresh(cx = this.cx) {
    try {
      if (!this.client) {
        const found = await discover({ wait: (ms) => cx.sleep(ms) });
        if (!found) {
          this.failure = "shprd serve is not running";
          this.state = null;
          cx.notify();
          return;
        }
        this.client = new Client(found, { wait: (ms) => cx.sleep(ms) });
        this.follow();
      }
      const state = await this.client.state();
      if (state) this.state = state;
      this.failure = null;
    } catch (err) {
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

  render() {
    const t = theme();
    const colors = tone(t);
    const task = this.selected();
    const header = cardHeader(
      t,
      h_flex()
        .gap(6)
        .items_center()
        .child(div().text_size(TEXT.body).font_medium().text_color(t.foreground).child(PR_WORDS.title))
        .when(task?.pr, (el) =>
          el.child(div().text_size(TEXT.label).text_color(t.dim).child(task.id)),
        ),
    );
    if (!task) {
      return surface(t, [
        header,
        note(t, this.failure ?? "No task is open."),
        note(t, "Pick one in the list; its pull request shows here."),
        div().flex_1(),
      ]);
    }
    const card_ = taskPr(task);
    if (card_.kind !== "pr") {
      return surface(t, [header, note(t, card_.line), div().flex_1()]);
    }
    const fill = card_.tone === "green" ? colors.green : colors.amber;
    return surface(t, [
      header,
      v_flex()
        .flex_1()
        .min_h(0)
        .p(12)
        .gap(8)
        // The state word in its own tint, the forge's number, then the link: what
        // a person decides by, in the order Shepherdr's card has them.
        .child(
          h_flex()
            .gap(8)
            .items_center()
            .child(
              h_flex()
                .h(20)
                .px(8)
                .items_center()
                .rounded(RADIUS.control)
                .bg(t.raised)
                .text_size(TEXT.label)
                .font_medium()
                .text_color(fill)
                .child(card_.state),
            )
            .child(div().flex_none().font_family("JetBrains Mono").text_size(TEXT.body).text_color(t.foreground).child(card_.number))
            .child(div().flex_1())
            .child(
              div()
                .flex_none()
                .text_size(TEXT.id)
                .text_color(card_.approved ? colors.green : t.dim)
                .child(card_.line),
            ),
        )
        .child(
          h_flex()
            .gap(6)
            .items_center()
            .text_size(TEXT.label)
            .text_color(t.muted)
            .child(div().min_w(0).truncate().font_family("JetBrains Mono").child(card_.head)),
        )
        .child(
          h_flex()
            .id("task-pr-open")
            .h(28)
            .flex_none()
            .px(10)
            .gap(6)
            .items_center()
            .rounded(RADIUS.control)
            .border_1()
            .border_color(t.input)
            .text_size(TEXT.label)
            .text_color(t.foreground)
            .cursor_pointer()
            .hover((style) => style.bg(t.hover))
            .when(openUrl(card_) !== null, (el) =>
              el.on_click((_event, cx) => cx.open_url(openUrl(card_))),
            )
            .child(PR_WORDS.openOnTheForge)
            .child(div().text_size(TEXT.id).text_color(t.dim).child(card_.url)),
        ),
    ]);
  }
}
