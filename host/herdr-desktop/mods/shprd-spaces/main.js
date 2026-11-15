// The spaces sidebar: the coordinator's space, then one row per task space, each
// with the word shprd's rules give that task and the count that needs the person.
//
// It replaces Herdr's own `spaces` list in this layout (the layout's `spaces`
// block names this mod), because in shprd a space *is* work: one task, one space,
// named from the task (`shprd_mux::task_space_name`). What a person needs from
// the left column is therefore not where their terminals are but where the run
// stands — which is the service's answer, and this mod draws it.
//
// Ported from Shepherdr's sidebar (MIT):
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/spaces-chat/main.js` (a row per space: a coloured avatar, the
//   name, and the red/green counts at the end; the section headings and the
//   scroll area), `mods/spaces-chat/sections.js` (the quiet "Spaces" heading)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// The data path is shprd's: `GET /api/state` for the tasks and their words, the
// service's change file for "look again" (`lib/watch.js`), and Herdr's live
// `spaces()` for the rows a click can land on. Nothing here decides what a task
// is doing — `lib/spaces.js` joins the two and the service's rules win.

import { View, div } from "gpui-kit";
import { v_flex, h_flex } from "gpui-base";
import { run, spaces, theme } from "herdr";

import { Client, discover, reason } from "./lib/client.js";
import { rowCounts, sidebarRows } from "./lib/spaces.js";
import { badge, countBadge, note, statusGlyph } from "./lib/panel.js";
import { SPACE, TEXT, tone } from "./lib/tokens.js";
import { followChanges } from "./lib/watch.js";

/** How often the sidebar reads the board when the change file says nothing. */
const FLOOR_MS = 5000;
/** A row's height: two lines of text, the widest row the sidebar draws. */
const ROW_H = 42;

export default class Spaces extends View {
  init(_props, cx) {
    this.cx = cx;
    this.state = null;
    this.failure = null;
    this.client = null;
    this.watching = false;
    this.refresh();
    cx.timer.every(FLOOR_MS, (cx) => this.refresh(cx));
  }

  /** Reads the board once; a service that went away is looked for again. */
  async refresh(cx = this.cx) {
    try {
      if (!this.client) {
        const found = await discover({ wait: (ms) => cx.sleep(ms) });
        if (!found) {
          this.state = null;
          this.failure = "shprd serve is not running";
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
      // A service that restarted answers with another id: forget the client so
      // the next look finds the new port instead of dialling a dead one.
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

  /** Brings a space to the front, which is what a row is for. */
  focus(row) {
    if (!row?.space?.id) return;
    run("space.select", { id: row.space.id });
  }

  render() {
    const t = theme();
    const colors = tone(t);
    const rows = sidebarRows(this.state, spaces());
    const tally = rowCounts(rows);
    const sections = [];
    if (rows.coordinator) sections.push(this.row(t, colors, rows.coordinator));
    if (rows.tasks.length > 0) {
      sections.push(this.heading(t, "Task spaces", String(rows.tasks.length)));
      for (const row of rows.tasks) sections.push(this.row(t, colors, row));
    }
    if (rows.waiting.length > 0) {
      sections.push(this.heading(t, "Waiting for a space", String(rows.waiting.length)));
      for (const row of rows.waiting) sections.push(this.row(t, colors, row));
    }
    if (sections.length === 0) {
      sections.push(
        note(t, this.failure ?? "No run yet."),
        note(t, this.failure ? "Start it from the footer below." : "Tasks appear here as the run adds them."),
      );
    }
    return v_flex()
      .size_full()
      .min_h(0)
      .bg(t.chrome)
      .text_size(TEXT.body)
      .child(
        h_flex()
          .h(31)
          .flex_none()
          .pl(16)
          .pr(10)
          .gap(8)
          .items_center()
          .text_size(TEXT.id + 1)
          .text_color(t.dim)
          .child("Spaces")
          .child(div().flex_1())
          .when(tally.needs > 0, (el) =>
            el.child(
              h_flex()
                .gap(5)
                .items_center()
                .child(badge(t, countBadge(tally.needs), colors.red, colors.on))
                .child(div().text_color(colors.red).child("need you")),
            ),
          ),
      )
      .child(
        v_flex()
          .flex_1()
          .min_h(0)
          .px(8)
          .pb(10)
          .gap(1)
          .overflow_y_scrollbar()
          .mode("always")
          .children(sections),
      );
  }

  /** A quiet section heading: a word, and how many rows are under it. */
  heading(t, word, count) {
    return h_flex()
      .h(SPACE.sidebarRow)
      .flex_none()
      .mt(4)
      .px(8)
      .gap(6)
      .items_center()
      .text_size(TEXT.id)
      .text_color(t.dim)
      .child(word)
      .child(count);
  }

  /**
   * One space: its status glyph, the task it is, the word the rules give it, and
   * a line saying what it is doing now.
   *
   * The second line is the service's own — the task's reason, its agent's last
   * progress line or its title (`lib/board.js`, `secondLine`) — so a row never
   * says something the board would not.
   */
  row(t, colors, row) {
    const muted = row.kind === "waiting";
    const fill = row.tone === "dim" ? t.dim : (colors[row.tone] ?? t.dim);
    const clickable = Boolean(row.space?.id);
    return v_flex()
      .id(`space-${row.id}`)
      .h(ROW_H)
      .flex_none()
      .px(8)
      .py(5)
      .gap(1)
      .justify_center()
      .rounded(8)
      .when(row.active, (el) => el.bg(t.selected))
      .when(!row.active, (el) => el.hover((style) => style.bg(t.hover)))
      .when(clickable, (el) => el.cursor_pointer().on_click(() => this.focus(row)))
      .child(
        h_flex()
          .gap(8)
          .items_center()
          .child(statusGlyph(t, row, colors))
          .child(
            div()
              .flex_none()
              .font_family("JetBrains Mono")
              .text_color(muted ? t.muted : t.foreground)
              .child(row.label),
          )
          .when(row.kind === "task", (el) =>
            el.child(
              div()
                .min_w(0)
                .truncate()
                .text_size(TEXT.label)
                .text_color(t.muted)
                .child(row.title),
            ),
          )
          .child(div().flex_1())
          .when(row.needs, (el) => el.child(badge(t, "!", colors.red, colors.on)))
          .child(div().flex_none().text_size(TEXT.label).text_color(fill).child(row.word)),
      )
      .child(
        h_flex()
          .pl(22)
          .h(14)
          .items_center()
          .text_size(TEXT.id + 0.5)
          .text_color(t.dim)
          .child(div().min_w(0).truncate().child(row.sub ?? "")),
      );
  }
}
