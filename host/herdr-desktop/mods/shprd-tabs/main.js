// The tab bar: the shown space's tabs as pills — the agent that runs in each, its
// role in shprd's loop, and the state it is in — then the side panel's buttons.
//
// In shprd the tabs of one space are one task's terminals: its worker and its
// reviewer (`shprd_mux`'s one-space-per-task rule). The tab's title is what says
// which (`<agent>·<task>`, `<agent>·<task>·review`), and Herdr's live status is
// what says how it is doing; `lib/tabstrip.js` joins those with the service's
// own word for the task, so a pill is never a guess. A backup that takes a task
// over keeps the worker's own tab and title and only changes the agent on it,
// so its pill reads as the worker it now is.
//
// Ported from Shepherdr's tab bar (MIT):
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/tabs-tasks/main.js` (the pill row: the program's icon, the name,
//   a spinner while working, a dot when something needs the person, the close
//   mark on the shown pill; then Files / Changes / Tasks with its badge)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// The side panel's buttons open the layout's stacks — `layout.select` then
// `layout.show` — because the panel is a stack in `layouts/shprd.json`, not
// something a mod draws. Settings is not one of them: it opens over the window
// as Shepherdr's own settings do (`openSettings()` in `lib/panel.js`), from the
// sidebar's footer row, so the panel keeps its stacks for the work.
import { View, div } from "gpui-kit";
import { h_flex } from "gpui-base";
import { layout, panes, run, spaces, theme } from "herdr";

import { attach, reason } from "./lib/client.js";
import { badge, countBadge } from "./lib/panel.js";
import { panelButtons, tabRows } from "./lib/tabstrip.js";
import { RADIUS, TEXT, tone } from "./lib/tokens.js";

/** How often the bar re-reads the board the pills borrow their words from. */
const FLOOR_MS = 5000;

export default class Tabs extends View {
  init(_props, cx) {
    this.cx = cx;
    this.state = null;
    this.client = null;
    this.shown = "tasks";
    this.refresh();
    cx.timer.every(FLOOR_MS, (cx) => this.refresh(cx));
  }

  /**
   * Reads the board once; the pills fall back to Herdr's own status alone.
   *
   * A restarted service is noticed by `attach` (the id in `service.json` moved),
   * so the bar takes the new client and re-reads the board instead of keeping the
   * old words beside it (M10 gate bug 5).
   */
  async refresh(cx = this.cx) {
    try {
      const next = await attach(this.client, { wait: (ms) => cx.sleep(ms) });
      if (!next.client) {
        this.client = null;
        cx.notify();
        return;
      }
      this.client = next.client;
      this.state = await this.client.state();
    } catch (err) {
      // A restarted service answers with another id, so the next look finds the
      // new port rather than dialling a dead one; the pills keep working without
      // it, on the host's status alone.
      this.client = null;
      void reason(err);
    }
    cx.notify();
  }

  /** Shows one of the side panel's stacks, or hides the panel when it is up. */
  panel(child) {
    const open = this.shown === child && this.panelShown();
    this.shown = open ? null : child;
    if (open) {
      run("layout.hide", { id: "panel" });
    } else {
      run("layout.select", { stack: "panel-body", child });
      run("layout.show", { id: "panel" });
    }
    this.cx?.notify?.();
  }

  /** Whether the layout's panel block is showing, from the layout itself. */
  panelShown() {
    const panel = find(layout(), "panel");
    return !panel?.hidden;
  }

  render() {
    const t = theme();
    const colors = tone(t);
    const space = spaces().find((entry) => entry.active);
    const pills = tabRows(space, panes(), this.state).map((row) => this.pill(t, colors, row));
    const buttons = panelButtons(this.state, this.shown).map((button) =>
      this.button(t, colors, button),
    );
    return h_flex()
      .size_full()
      .min_w(0)
      .px(10)
      .gap(3)
      .items_center()
      .bg(t.chrome)
      .border_b(1)
      .border_color(t.border)
      .text_size(TEXT.label)
      .child(
        h_flex()
          .flex_1()
          .min_w(0)
          .h_full()
          .gap(3)
          .items_center()
          .overflow_x_scrollbar()
          .children(pills),
      )
      .child(h_flex().gap(3).items_center().flex_none().children(buttons));
  }

  /**
   * One pill: the state dot, the agent and the task it is on, the role when it is
   * not the worker's, the word, and the close mark on the shown tab.
   *
   * The agent and the task are the title shprd gave the tab, so a person reading
   * the bar sees exactly what the run opened — a pill never renames anything.
   */
  pill(t, colors, row) {
    const fill = row.tone === "dim" ? t.dim : (colors[row.tone] ?? t.dim);
    return h_flex()
      .id(`tab-${row.id}`)
      .h(26)
      .flex_none()
      .px(9)
      .gap(6)
      .items_center()
      .rounded(RADIUS.control)
      .border_1()
      .border_color(row.active ? t.border : t.chrome)
      .bg(row.active ? t.raised : t.chrome)
      .text_size(TEXT.body)
      .text_color(row.active ? t.foreground : t.muted)
      .hover((style) => style.bg(t.hover))
      .on_click(() => run("tab.select", { id: row.id }))
      .child(
        div()
          .w(7)
          .h(7)
          .flex_none()
          .rounded(4)
          .bg(fill),
      )
      .child(
        div()
          .flex_none()
          .when(row.agent !== null, (el) => el.font_family("JetBrains Mono"))
          .child(row.label),
      )
      .when(row.roleWord !== null && row.role !== "worker", (el) =>
        el.child(
          div()
            .flex_none()
            .px(4)
            .rounded(4)
            .bg(t.raised)
            .text_size(TEXT.id)
            .text_color(t.dim)
            .child(row.roleWord.toLowerCase()),
        ),
      )
      .child(div().flex_none().text_size(TEXT.id + 0.5).text_color(fill).child(row.word))
      .when(row.zoomed, (el) => el.child(div().text_color(t.dim).child("Z")))
      .when(row.active, (el) =>
        el.child(
          div()
            .id(`close-${row.id}`)
            .px(3)
            .flex_none()
            .rounded(4)
            .text_color(t.dim)
            .hover((style) => style.bg(t.hover).text_color(t.foreground))
            .on_click((_event, cx) => {
              cx?.stop_propagation?.();
              run("tab.close", { id: row.id });
            })
            .child("×"),
        ),
      )
      .when(row.needs, (el) =>
        el.child(badge(t, "!", colors.red, colors.on)),
      );
  }

  /** One side-panel button, marked when that stack is the one showing. */
  button(t, colors, button) {
    return h_flex()
      .id(`panel-${button.id}`)
      .h(26)
      .flex_none()
      .px(8)
      .gap(5)
      .items_center()
      .rounded(RADIUS.control)
      .bg(button.active ? t.selected : t.chrome)
      .text_color(button.active ? t.foreground : t.muted)
      .hover((style) => style.bg(t.hover))
      .on_click(() => this.panel(button.id))
      .child(button.word)
      .when(button.needs > 0, (el) =>
        el.child(badge(t, countBadge(button.needs), colors.red, colors.on)),
      )
      .when(button.ready > 0, (el) =>
        el.child(badge(t, countBadge(button.ready), colors.green, colors.on)),
      );
  }
}

/** The layout node called `id`, or null. */
function find(node, id) {
  if (node?.id === id) return node;
  for (const child of node?.children ?? []) {
    const found = find(child, id);
    if (found) return found;
  }
  return null;
}
