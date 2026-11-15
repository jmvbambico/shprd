// The tab bar: the shown space's tabs as pills, then the side panel's three
// buttons — the shape Shepherdr's window uses.
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/tabs-tasks/main.js` (the pill row, the Files/Changes/Tasks
//   buttons at the right, `window_drag` on the bar)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// M10.3 builds the bar's frame and its panel buttons; M10.4 fills the agent's
// own state on a pill (a spinner, a count, a close mark) and the `+` menu. What
// is here is real, not a placeholder: the tabs are Herdr's live list, a click
// selects, and the three buttons open the side panel's stacks.

import { View, div } from "gpui-kit";
import { h_flex } from "gpui-base";
import { run, spaces, theme } from "herdr";

import { RADIUS, SPACE, TEXT } from "./lib/tokens.js";

export default class Tabs extends View {
  init(_props, cx) {
    this.cx = cx;
    this.shown = "tasks";
  }

  /** Opens one of the side panel's stacks. */
  panel(child) {
    this.shown = child;
    run("layout.select", { stack: "panel-body", child });
    run("layout.show", { id: "panel" });
    this.cx?.notify?.();
  }

  render() {
    const t = theme();
    const space = spaces().find((s) => s.active);
    const tabs = space?.tabs ?? [];
    const pills = tabs.map((tab, index) => {
      const active = Boolean(tab.active);
      return h_flex()
        .id(`tab-${tab.id}`)
        .h(26)
        .flex_none()
        .px(10)
        .gap(6)
        .items_center()
        .rounded(RADIUS.control)
        .text_size(TEXT.body)
        .text_color(active ? t.foreground : t.muted)
        .when(active, (el) => el.bg(t.raised))
        .when(!active, (el) => el.cursor_pointer().hover((s) => s.bg(t.hover)))
        .when(!active, (el) => el.on_click(() => run("tab.select", { index: index + 1 })))
        .child(tab.name || String(index + 1));
    });
    return h_flex()
      .size_full()
      .px(10)
      .gap(6)
      .items_center()
      .bg(t.chrome)
      .text_size(TEXT.label)
      .children(pills)
      .child(div().flex_1())
      .child(this.button(t, "Files", "files"))
      .child(this.button(t, "Changes", "changes"))
      .child(this.button(t, "Tasks", "tasks"))
      .child(this.button(t, "Asks", "asks"))
      .child(this.button(t, "Settings", "settings"));
  }

  /** One side-panel button, marked when that panel is the one shown. */
  button(t, word, child) {
    const active = this.shown === child;
    return h_flex()
      .id(`panel-${child}`)
      .h(26)
      .flex_none()
      .px(8)
      .items_center()
      .rounded(RADIUS.control)
      .text_color(active ? t.foreground : t.muted)
      .when(active, (el) => el.bg(t.selected))
      .when(!active, (el) => el.cursor_pointer().hover((s) => s.bg(t.hover)))
      .on_click(() => this.panel(child))
      .child(word);
  }
}

// `SPACE` documents the bar's own height, which the layout file owns; a mod
// reads it only to keep its pills inside it.
void SPACE;
