// The pane header: one copy per terminal pane, drawn by the layout's pane frame.
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/pane-chat/main.js` (the header branch: the program's icon and
//   name, the folder, the agent's status dot, split/zoom/close)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// In shprd a pane *is* a task's agent terminal, so the header names the task the
// pane belongs to (`herdr.task` label) and the agent's own state, which is what a
// person checks before they click into it. Herdr hands each copy `props().pane`
// and `props().terminal`; everything else comes from `panes()`.
//
// Right-clicking the header opens the pane's menu — Split right, Split down,
// Zoom, Send back to its parent (a kept sub-agent only), Close pane — which is
// Shepherdr's pane menu (`mods/menu-tasks/main.js:29-52`, ported in
// `lib/menu.js`). It opens at the pointer, at the size its own rows need, so a
// menu is never taller than what is in it.
//
// M10.4 fills the Chat│Terminal switch and the Autopilot switch. What is here is
// the header itself: the name, the task, the dot, and the three buttons.

import { View, div } from "gpui-kit";
import { h_flex } from "gpui-base";
import { openOverlay, panes, props, run, terminals, theme } from "herdr";

import { menuHeight, MENU, paneMenu } from "./lib/menu.js";
import { RADIUS, TEXT } from "./lib/tokens.js";

const STATUS = {
  working: "working",
  blocked: "needs you",
  done: "finished",
  idle: "idle",
};

export default class Pane extends View {
  /** A short folder: `~` at home, else the last two parts. */
  short(folder) {
    if (!folder) return "";
    const parts = folder.replace(/\/+$/, "").split("/");
    return parts.length <= 2 ? folder : `…/${parts.slice(-2).join("/")}`;
  }

  /**
   * The pane's menu, opened over the header at the pointer.
   *
   * The rows come from `lib/menu.js` and the height is theirs, so the same list
   * is measured here and drawn by `shprd-menu`. *Send back* appears only when
   * the terminal has a parent: a pane that was parked under another one is the
   * only thing that can go back to it.
   */
  menu(wired) {
    const all = terminals();
    const term = all.find((entry) => entry?.address === wired?.terminal);
    const parent = term?.parent ? all.find((entry) => entry?.address === term.parent) : null;
    const rows = paneMenu({
      pane: wired?.pane ?? null,
      sendBack: parent ? { name: parent.name || parent.address, terminal: parent.address } : null,
    });
    if (rows.length === 0) return;
    openOverlay({
      mod: "shprd-menu",
      kind: "popup",
      width: MENU.pane,
      height: menuHeight(rows),
      props: {
        tab: "pane",
        target: { pane: wired?.pane ?? null, terminal: wired?.terminal ?? null },
      },
    });
  }

  render() {
    const t = theme();
    const wired = props();
    const pane = panes().find((p) => p.id === wired?.pane);
    const status = pane?.agent?.status ?? pane?.status ?? "idle";
    const dot = status === "working" ? t.working : status === "blocked" ? t.blocked : t.dim;
    const task = pane?.labels?.["herdr.task"];
    return h_flex()
      .size_full()
      .px(8)
      .gap(8)
      .items_center()
      .bg(t.background)
      .border_b(1)
      .border_color(t.border)
      .text_size(TEXT.label)
      .text_color(t.muted)
      .on_mouse_down("right", (_event, cx) => {
        cx?.stop_propagation?.();
        this.menu(wired);
      })
      .child(div().w(6).h(6).flex_none().rounded(3).bg(dot))
      .child(
        div()
          .text_color(t.foreground)
          .child(pane?.title ?? pane?.program?.name ?? "Terminal"),
      )
      .when(Boolean(task), (el) => el.child(div().text_color(t.dim).child(task)))
      .child(div().text_color(t.dim).child(this.short(pane?.cwd)))
      .when(status in STATUS, (el) => el.child(div().text_color(dot).child(STATUS[status])))
      .child(div().flex_1())
      .child(this.action(t, "split", "⇥", () => run("pane.split", { direction: "right", pane: wired?.pane })))
      .child(this.action(t, "zoom", "⤢", () => run("pane.zoom", { pane: wired?.pane })))
      .child(this.action(t, "close", "✕", () => run("pane.close", { pane: wired?.pane })));
  }

  /** One header button: a glyph, a hover, and the command it runs. */
  action(t, id, glyph, onClick) {
    return div()
      .id(`pane-${id}`)
      .w(20)
      .h(20)
      .flex_none()
      .flex()
      .items_center()
      .justify_center()
      .rounded(RADIUS.control)
      .text_color(t.dim)
      .cursor_pointer()
      .hover((s) => s.bg(t.hover).text_color(t.foreground))
      .on_click(onClick)
      .child(glyph);
  }
}
