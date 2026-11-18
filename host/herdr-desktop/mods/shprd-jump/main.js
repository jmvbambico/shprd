// Jump (Ctrl+K): one find box over the coordinator, the task spaces and the
// tasks, a popup at the top of the window. `mod.open:shprd-jump` opens it and
// opens it again to close it; ↑ ↓ move, Enter goes, Esc closes, a click goes.
//
// What it lists and what a pick does is `lib/jump.js`'s (pure, tested); this
// mod reads the board once as it opens, draws the rows and runs the pick:
//
// - a space: Herdr's `space.select`;
// - a task: its thread in the Tasks panel (`openThread`);
// - "Show N more": unfolds the group.
//
// Ported from Shepherdr's Jump (MIT):
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Path: `mods/jump/main.js` (the box, the 48 px rows, the 30 px group heads,
//   the keys, the empty words) and `mods/jump/mod.json` (the popup)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik

import { View, div, svg } from "gpui-kit";
import {
  Input,
  InputState,
  Scrollbar,
  VirtualListScrollHandle,
  h_flex,
  v_flex,
  v_virtual_list,
} from "gpui-base";
import { closeOverlay, props, run, spaces, theme } from "herdr";

import { attach } from "./lib/client.js";
import { JUMP, JUMP_WORDS, jumpItems, jumpRows, jumpTarget, rowHeight } from "./lib/jump.js";
import { openThread, statusGlyph } from "./lib/panel.js";
import { RADIUS, TEXT, status as statusOf, tone } from "./lib/tokens.js";

const SEARCH_ICON = "mods/shprd-jump/icons/search.svg";
/** The list's tallest height: the popup less the 48 px box. */
const LIST_MAX = JUMP.height - 48 - 8;
/** The monospace column a task's id sits in, so the titles line up. */
const ID_FONT = "JetBrains Mono";

export default class Jump extends View {
  init(_props, cx) {
    this.cx = cx;
    this.client = null;
    this.state = null;
    this.loading = true;
    this.down = false;
    this.more = new Set();
    this.sel = null;
    this.input = InputState.new({ placeholder: JUMP_WORDS.placeholder });
    this.input.on("change", (_event, cx) => {
      this.sel = null;
      this.more.clear();
      this.scroll.scroll_to_item(0, "top");
      cx.notify();
    });
    this.input.on("submit", (_event, cx) => this.go(this.current, cx));
    this.scroll = VirtualListScrollHandle.new();
    this.input.focus();
    cx.timer.after(0, () => this.input.focus());
    cx.timer.after(600, () => this.input.focus());
    this.read();
  }

  /** Releases the field and the list's handle when the popup goes away. */
  close() {
    this.input.release();
    this.scroll.release();
  }

  /** Reads the board once, as the popup opens; a service that is down is said. */
  async read() {
    try {
      const next = await attach(this.client, { wait: (ms) => this.cx?.sleep?.(ms) });
      if (next.client) {
        this.client = next.client;
        this.state = await this.client.state();
      } else {
        this.down = true;
      }
    } catch {
      this.down = true;
    }
    this.loading = false;
    this.cx?.notify?.();
  }

  /** Runs a row's pick; the popup closes unless the pick only unfolds a group. */
  go(row, cx) {
    const target = jumpTarget(row);
    if (!target) return;
    if (target.more) {
      this.more.add(target.more);
      this.sel = null;
      cx?.notify?.();
      return;
    }
    closeOverlay(props()?.overlay);
    if (target.command) {
      run(target.command, target.args ?? {});
      return;
    }
    // A task: its thread, Chat first.
    openThread({ task: target.task, title: target.title ?? "" }, "chat");
  }

  /** The row's leading mark: a task's status glyph, or a space's "#". */
  mark(t, colors, row) {
    if (row.kind === "task") return statusGlyph(t, statusOf(row.status), colors, 14);
    return div().w(14).flex_none().text_center().text_size(15).text_color(t.muted).child(row.mark);
  }

  /** One drawn row: a group head, "Show N more", or an item. */
  line(t, colors, row, on, idWidth) {
    if (row.kind === "head") {
      return h_flex()
        .w_full()
        .h(JUMP.head)
        .px(14)
        .pt(10)
        .gap(6)
        .items_center()
        .text_size(TEXT.label)
        .font_medium()
        .text_color(t.dim)
        .child(row.label)
        .child(div().text_color(t.dim).child(String(row.count)));
    }
    if (row.kind === "more") {
      return h_flex()
        .w_full()
        .h(JUMP.more)
        .px(6)
        .child(
          h_flex()
            .flex_1()
            .h_full()
            .px(8)
            .items_center()
            .rounded(RADIUS.card)
            .text_size(TEXT.label)
            .text_color(t.accent)
            .cursor_pointer()
            .when(on, (el) => el.bg(t.selected))
            .hover((s) => s.bg(t.hover))
            .child(`Show ${row.count} more`),
        );
    }
    return h_flex()
      .w_full()
      .h(JUMP.row)
      .px(6)
      .child(
        h_flex()
          .flex_1()
          .min_w(0)
          .h_full()
          .px(8)
          .gap(10)
          .items_center()
          .rounded(RADIUS.card)
          .cursor_pointer()
          .when(on, (el) => el.bg(t.selected))
          .hover((s) => s.bg(t.hover))
          .child(this.mark(t, colors, row))
          .child(
            v_flex()
              .flex_1()
              .min_w(0)
              .gap(2)
              .child(
                h_flex()
                  .min_w(0)
                  .gap(8)
                  .items_center()
                  .when(row.kind === "task", (el) =>
                    el.child(
                      div()
                        .flex_none()
                        .w(idWidth)
                        .font_family(ID_FONT)
                        .text_size(TEXT.id)
                        .text_color(t.dim)
                        .child(row.ref),
                    ),
                  )
                  .child(
                    div()
                      .flex_1()
                      .min_w(0)
                      .truncate()
                      .text_size(TEXT.body)
                      .text_color(t.foreground)
                      .when(Boolean(row.active), (d) => d.font_semibold())
                      .child(row.label),
                  ),
              )
              .when(Boolean(row.sub), (el) =>
                el.child(
                  div().min_w(0).truncate().text_size(TEXT.label).text_color(t.dim).child(row.sub),
                ),
              ),
          ),
      );
  }

  render() {
    const t = theme();
    const colors = tone(t);
    const query = this.input.value().trim();
    const rows = jumpRows(jumpItems(this.state, spaces() ?? [], query), this.more);
    const pickable = rows.filter((row) => row.kind !== "head");
    const at = Math.max(0, pickable.findIndex((row) => row.key === this.sel));
    const current = (this.current = pickable[at] ?? null);
    // Task ids in one column, 0.6 em a character in the monospace face.
    const idWidth =
      Math.ceil(Math.max(0, ...rows.map((row) => row.ref?.length ?? 0)) * TEXT.id * 0.6) + 1;
    const move = (delta, cx) => {
      if (pickable.length === 0) return;
      const next = pickable[Math.max(0, Math.min(pickable.length - 1, at + delta))];
      this.sel = next.key;
      const index = rows.indexOf(next);
      // The group's heading stays in view over its first row.
      this.scroll.scroll_to_item(rows[index - 1]?.kind === "head" && delta < 0 ? index - 1 : index);
      cx.notify();
    };
    const empty =
      rows.length === 0
        ? v_flex()
            .h(120)
            .items_center()
            .justify_center()
            .gap(4)
            .text_size(TEXT.body)
            .text_color(t.dim)
            .child(this.loading ? JUMP_WORDS.loading : query ? JUMP_WORDS.none : JUMP_WORDS.empty)
        : null;
    const sizes = rows.map(rowHeight);
    const listHeight = Math.min(LIST_MAX, sizes.reduce((a, b) => a + b, 0));
    return v_flex()
      .id("jump")
      .w_full()
      .bg(t.popover)
      .text_size(TEXT.body)
      .on_key_down((event, cx) => {
        if (event.key === "escape") {
          closeOverlay(props()?.overlay);
          cx.stop_propagation();
          return;
        }
        if (event.key === "down" || (event.key === "n" && event.modifiers?.control)) move(1, cx);
        else if (event.key === "up" || (event.key === "p" && event.modifiers?.control)) move(-1, cx);
        else return;
        cx.stop_propagation();
      })
      .child(
        h_flex()
          .h(48)
          .flex_none()
          .px(16)
          .gap(10)
          .items_center()
          .border_b(1)
          .border_color(t.border)
          .child(svg(SEARCH_ICON).size(15).flex_none().text_color(t.dim))
          .child(Input.new(this.input).flex_1().h_full().text_size(14).text_color(t.foreground)),
      )
      .when(this.down && !this.loading, (el) =>
        el.child(
          div().px(16).py(6).text_size(TEXT.label).text_color(t.dim).child(JUMP_WORDS.down),
        ),
      )
      .child(
        empty ??
          div()
            .relative()
            .h(listHeight + 8)
            .py(4)
            .child(
              v_virtual_list(
                "jump-list",
                rows.length,
                sizes,
                (i) => rows[i].key,
                (range) =>
                  rows
                    .slice(range.start, range.end)
                    .map((row) => this.line(t, colors, row, row === current, idWidth)),
              )
                .track_scroll(this.scroll)
                .size_full()
                // Rows take no handlers: the list says which one was pressed.
                .on_item_click((key, cx) =>
                  this.go(
                    jumpRows(jumpItems(this.state, spaces() ?? [], this.input.value().trim()), this.more).find(
                      (row) => row.key === key && row.kind !== "head",
                    ),
                    cx,
                  ),
                ),
            )
            .child(Scrollbar.vertical("jump-list").absolute().inset_0()),
      );
  }
}
