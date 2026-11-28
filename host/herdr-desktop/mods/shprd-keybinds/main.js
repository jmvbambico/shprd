// Keybinds (Ctrl+Shift+/): every key that does something, in plain words,
// grouped (Tabs, Spaces, Tasks, Panes, …), type to filter. A 560 × 620 dialog.
//
// The keys are read live from Herdr (`keys()`): what the shown layout's own
// section binds replaces the global key and other layouts' sections are left
// out. The words, the groups and the filter are `lib/keybinds.js`'s.
//
// Ported from Shepherdr's Keybinds (MIT):
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Path: `mods/keybinds/main.js` (the dialog: title, filter field, the grouped
//   rows with a key cap each) and `mods/keybinds/mod.json` (560 × 620)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik

import { View, div, svg } from "gpui-kit";
import { Input, InputState, h_flex, v_flex } from "gpui-base";
import { closeOverlay, commands, keys, props, shared, theme, window } from "herdr";

import { KEYBINDS, grouped, keyFound } from "./lib/keybinds.js";
import { heading, keycap } from "./lib/panel.js";
import { H, RADIUS, TEXT } from "./lib/tokens.js";

const SEARCH_ICON = "mods/shprd-keybinds/icons/search.svg";

export default class Keybinds extends View {
  init(_props, cx) {
    this.filter = "";
    this.focused = false;
    this.input = InputState.new({ placeholder: KEYBINDS.filter });
    this.input.on("change", (_event, cx) => {
      this.filter = this.input.value();
      cx.notify();
    });
    this.input.on("focus", (_event, cx) => {
      this.focused = true;
      cx.notify();
    });
    this.input.on("blur", (_event, cx) => {
      this.focused = false;
      cx.notify();
    });
    this.input.focus();
    cx.timer.after(0, () => this.input.focus());
    cx.timer.after(300, () => this.input.focus());
    this.handle = cx.focus_handle();
  }

  /** Releases the field when the dialog goes away. */
  close() {
    this.input.release();
  }

  render() {
    const t = theme();
    const groups = grouped(keys() ?? [], {
      // The shown layout's name; this is shprd's dialog, so its own layout when
      // the window does not say.
      layout: shared("layout") || "shprd",
      about: new Map((commands() ?? []).map((c) => [c.name, c.about])),
      mac: window()?.platform === "macos",
    })
      .map((g) => ({ ...g, rows: g.rows.filter((row) => keyFound(this.filter, g.group, row)) }))
      .filter((g) => g.rows.length > 0);
    return v_flex()
      .id("keybinds")
      .track_focus(this.handle)
      .on_key_down((event, cx) => {
        if (event.key === "escape") {
          closeOverlay(props()?.overlay);
          cx?.stop_propagation?.();
        }
      })
      .size_full()
      .min_h(0)
      .p(18)
      .gap(12)
      .bg(t.popover)
      .text_color(t.foreground)
      .child(div().flex_none().text_size(TEXT.title).font_semibold().child(KEYBINDS.title))
      .child(
        h_flex()
          .h(H.lg)
          .flex_none()
          .px(10)
          .gap(8)
          .items_center()
          .rounded(RADIUS.control)
          .border_1()
          .border_color(this.focused ? t.accent : t.input)
          .bg(t.background)
          .child(svg(SEARCH_ICON).size(14).flex_none().text_color(t.dim))
          .child(Input.new(this.input).flex_1().h_full().text_size(TEXT.body).text_color(t.foreground)),
      )
      .child(
        v_flex()
          .id("keybinds-list")
          .flex_1()
          .min_h(0)
          .overflow_y_scrollbar()
          .gap(8)
          .when(groups.length === 0, (el) =>
            el.child(
              div().py(24).text_center().text_size(TEXT.body).text_color(t.dim).child(KEYBINDS.none),
            ),
          )
          .children(
            groups.map((g) =>
              v_flex()
                .flex_none()
                .child(heading(t, g.group))
                .children(
                  g.rows.map((row) =>
                    h_flex()
                      .h(H.lg)
                      .flex_none()
                      .px(12)
                      .gap(12)
                      .items_center()
                      .child(
                        div().flex_1().min_w(0).truncate().text_size(TEXT.body).child(row.name),
                      )
                      .child(
                        h_flex()
                          .flex_none()
                          .gap(4)
                          .items_center()
                          .children(row.keys.map((text) => keycap(t, text))),
                      ),
                  ),
                ),
            ),
          ),
      );
  }
}
