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

import { attach, reason, singleFlight } from "./lib/client.js";
import { taskById } from "./lib/board.js";
import { card as surface, cardHeader, note } from "./lib/panel.js";
import { PR_WORDS, branchLine, openUrl, prSize, taskPr } from "./lib/taskpr.js";
import { RADIUS, TEXT, tone } from "./lib/tokens.js";
import { followChanges } from "./lib/watch.js";

/** How often the card reads the board when the change file says nothing. */
const FLOOR_MS = 4000;
/** How old the branch's changes may be before they are read again, ms. */
const CHANGES_MS = 20_000;

export default class TaskPr extends View {
  init(_props, cx) {
    this.cx = cx;
    this.state = null;
    this.client = null;
    this.failure = null;
    this.watching = false;
    this.changes = null;
    this.changesFor = null;
    // The link last copied, so its button says "Copied" until another task opens.
    this.copied = null;
    this.refresh();
    cx.timer.every(FLOOR_MS, (cx) => this.refresh(cx));
  }

  /** The task the panel picked, from the one shared selection. */
  selected() {
    const thread = shared("tasks.thread");
    return taskById(this.state, thread?.task ?? null);
  }

  /**
   * Reads the board once; a service that restarted or went away is looked for
   * again, and the card's whole state is re-read rather than kept (M10 gate
   * bug 5).
   */
  refresh(cx = this.cx) {
    // One read at a time: a refresh asked for while another runs is folded into
    // one more run after it, never started beside it.
    return singleFlight(this, () => this.readOnce(cx));
  }

  /** One read of the service and one repaint; [`refresh`] is what callers use. */
  async readOnce(cx) {
    try {
      const next = await attach(this.client, { wait: (ms) => cx.sleep(ms) });
      if (!next.client) {
        this.client = null;
        this.failure = "shprd serve is not running";
        this.state = null;
        cx.notify();
        return;
      }
      this.client = next.client;
      this.follow();
      this.state = await this.client.state();
      this.failure = null;
      await this.readChanges();
    } catch (err) {
      this.client = null;
      this.failure = reason(err);
    }
    cx.notify();
  }

  /**
   * The task's branch, for the size the bar shows and the base the branch line
   * ends in. A git read at the service, so it is made once per task and attempt
   * and only for a task that has a pull request.
   */
  async readChanges() {
    const task = this.selected();
    if (!this.client || !task?.pr) {
      this.changes = null;
      this.changesFor = null;
      return;
    }
    // An agent report or a status change is the log moving: the branch may have
    // new commits, so the read is made again (and not on every wake-up besides).
    const key = `${task.id}:${(task.attempts ?? []).length}:${task.status}:${task.last_status?.seq ?? 0}`;
    // A branch can gain commits with nothing in the log moving, so a read older
    // than CHANGES_MS is made again even under the same key.
    if (key === this.changesFor && Date.now() - (this.changesAt ?? 0) < CHANGES_MS) return;
    this.changesAt = Date.now();
    this.changesFor = key;
    this.changes = null;
    try {
      this.changes = await this.client.taskChanges(task.id);
    } catch {
      // The size and the base are extras: a pull request is still shown without them.
      this.changes = null;
    }
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
    const size = prSize(this.changes);
    const url = openUrl(card_);
    const button = (id, label, onClick) =>
      h_flex()
        .id(id)
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
        .on_click(onClick)
        .child(label);
    return surface(t, [
      header,
      v_flex()
        .flex_1()
        .min_w(0)
        .p(12)
        .gap(8)
        // The bar: the state in its own tint, the number, the size of the change.
        // The service reports open or merged only, so there is no Draft or Closed.
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
            .when(size !== null, (el) =>
              el
                .child(div().flex_none().text_size(TEXT.label).text_color(t.muted).child(size.files))
                .child(div().flex_none().font_family("JetBrains Mono").text_size(TEXT.label).text_color(colors.green).child(size.added))
                .child(div().flex_none().font_family("JetBrains Mono").text_size(TEXT.label).text_color(colors.red).child(size.removed)),
            )
            .child(div().flex_1())
            .child(
              div()
                .flex_none()
                .text_size(TEXT.id)
                .text_color(card_.approved ? colors.green : t.dim)
                .child(card_.line),
            ),
        )
        .child(div().min_w(0).text_size(TEXT.body).font_medium().text_color(t.foreground).child(task.title ?? ""))
        .child(
          h_flex()
            .gap(6)
            .items_center()
            .text_size(TEXT.label)
            .text_color(t.muted)
            .child(div().min_w(0).truncate().font_family("JetBrains Mono").child(branchLine(card_, this.changes))),
        )
        .child(
          h_flex()
            .gap(8)
            .items_center()
            .child(
              button("task-pr-open", PR_WORDS.openOnGitHub, (_event, cx) => {
                if (url !== null) cx.open_url(url);
              }),
            )
            .child(
              button("task-pr-copy", this.copied === card_.url ? PR_WORDS.copied : PR_WORDS.copyLink, (_event, cx) => {
                if (url === null) return;
                cx.write_to_clipboard(url);
                this.copied = card_.url;
                cx.notify();
              }),
            ),
        ),
    ]);
  }
}
