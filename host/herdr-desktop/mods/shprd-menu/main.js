// The window's context menus: a tab's, the app's own, and a pane's.
//
// They are one mod because they are one surface — the same 236 px popup, the same
// 32 px rows, the same keys — and because the rows are the same data whichever
// way a menu was opened (`lib/menu.js`). Which menu is drawn comes from `props()`:
//
//   { tab: "tab", target: { id, space } }   a tab's menu (right-click a pill)
//   { tab: "pane", target: { pane, terminal } }   a pane's (right-click a header,
//                                          or the app's own `terminal_menu`)
//   { tab: "global" }                     the app's (right-click the bar's empty part)
//
// A copy opened with no props at all (`herdr-app do mod.open shprd-menu`) draws
// the app's menu, so the surface can be looked at on its own.
//
// Ported from Shepherdr's `mods/menu-tasks` (MIT):
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/menu-tasks/main.js` (the rows, ↑↓ Enter Esc, the separators and
//   the selected row), `lib/ui.js` (`menuRow`)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// What is shprd's own: *Restart* on a tab restarts the task's attempt through the
// service (`POST /api/action {"action":"task.retry"}`) rather than relaunching a
// program in the tab, and *New task…* and *Keybinds* are this window's
// counterparts of Shepherdr's *New space* and *Keybinds* (see `lib/menu.js`).

import { View, div } from "gpui-kit";
import { v_flex } from "gpui-base";
import { closeOverlay, openOverlay, panes, props, run, spaces, terminals, theme } from "herdr";

import { attach, reason } from "./lib/client.js";
import { appMenu, paneMenu, restartable, tabMenu } from "./lib/menu.js";
import { menuRow } from "./lib/panel.js";
import { parseTabName } from "./lib/tabstrip.js";
import { RADIUS, TEXT } from "./lib/tokens.js";

/**
 * The rows' glyphs, as paths in this mod's own folder: Shepherdr's own SVGs (see
 * the header of `icons/*.svg`), which every shprd install carries.
 */
const ICON = {
  "square-pen": "mods/shprd-menu/icons/square-pen.svg",
  "rotate-ccw": "mods/shprd-menu/icons/rotate-ccw.svg",
  x: "mods/shprd-menu/icons/x.svg",
  plus: "mods/shprd-menu/icons/plus.svg",
  keyboard: "mods/shprd-menu/icons/keyboard.svg",
  terminal: "mods/shprd-menu/icons/terminal.svg",
  "split-right": "mods/shprd-menu/icons/split-right.svg",
  "split-down": "mods/shprd-menu/icons/split-down.svg",
  zoom: "mods/shprd-menu/icons/zoom.svg",
};

export default class Menu extends View {
  init(_props, cx) {
    this.cx = cx;
    this.at = 0;
    this.handle = cx.focus_handle();
    this.handle.focus();
    this.client = null;
    this.failure = null;
  }

  /** What this copy is a menu for, from its props. */
  kind() {
    const wired = props();
    if (wired?.tab === "tab" || wired?.tab === "pane") return wired.tab;
    return "global";
  }

  /** The tab this menu is about, or null when it is gone. */
  tab() {
    const target = props()?.target ?? {};
    for (const space of spaces() ?? []) {
      const found = (space.tabs ?? []).find((tab) => tab.id === target.id);
      if (found) return { tab: found, space };
    }
    return null;
  }

  /** The rows this menu draws, in the order `lib/menu.js` gives them. */
  rows() {
    const wired = props();
    switch (this.kind()) {
      case "tab": {
        const found = this.tab();
        if (!found) return appMenu();
        return tabMenu({
          id: found.tab.id,
          restartable: restartable(found.tab, panes()),
        });
      }
      case "pane": {
        const target = wired?.target ?? {};
        const all = terminals() ?? [];
        const term = all.find((entry) => entry?.address === target.terminal);
        const parent = term?.parent
          ? all.find((entry) => entry?.address === term.parent)
          : null;
        return paneMenu({
          pane: target.pane ?? null,
          sendBack: parent
            ? { name: parent.name || parent.address, terminal: parent.address }
            : null,
        });
      }
      default:
        return appMenu();
    }
  }

  /**
   * Runs one row and closes the menu.
   *
   * The commands are the app's own; the three that are not are this mod's:
   * *Rename…* opens the prompt dialog, *Restart* is the service's `task.retry`,
   * *New task…* shows the Tasks panel's own form and *Keybinds* is Herdr's keys
   * dialog (our own list is U6's).
   */
  choose(index) {
    const row = this.rows()[index];
    if (!row || row.sep || row.heading || row.note || row.disabled) return;
    closeOverlay(props()?.overlay);
    if (row.command) {
      run(row.command.name, row.command.args ?? {});
      return;
    }
    if (row.id === "rename") {
      const found = this.tab();
      if (!found) return;
      openOverlay({
        mod: "shprd-prompt",
        kind: "dialog",
        modal: true,
        width: 400,
        height: 196,
        props: {
          title: "Rename tab",
          value: found.tab.named ? found.tab.name : "",
          placeholder: found.tab.name,
          command: { name: "tab.rename", args: { id: found.tab.id } },
          verb: "Save",
        },
      });
      return;
    }
    if (row.id === "restart") {
      const found = this.tab();
      const task = parseTabName(found?.tab?.name).task;
      if (task) this.restart(task);
      return;
    }
    if (row.id === "new-task") {
      run("layout.select", { stack: "panel-body", child: "tasks" });
      run("layout.show", { id: "panel" });
      run("shprd-tasks.add");
      return;
    }
    if (row.id === "keybinds") {
      run("app.help");
    }
  }

  /**
   * Restarts the task's agent: the service's retry, not a program started here.
   *
   * A refusal (a task that already landed, a second attempt while one is open) is
   * the service's words and is drawn under the rows rather than swallowed: a
   * click that did nothing has to say why.
   */
  async restart(task) {
    try {
      const next = await attach(this.client, { wait: (ms) => this.cx?.sleep?.(ms) });
      if (!next.client) {
        this.failure = "shprd serve is not running";
        this.cx?.notify?.();
        return;
      }
      this.client = next.client;
      // The action route's envelope is `{kind, task}`: the kind names the verb
      // (`task.retry`), exactly as `shprd-tasks` and the Accept card send it.
      await this.client.action({ kind: "task.retry", task });
      this.failure = null;
    } catch (err) {
      this.failure = reason(err);
    }
    this.cx?.notify?.();
  }

  render() {
    const t = theme();
    const list = this.rows();
    return v_flex()
      .id("menu")
      .track_focus(this.handle)
      .size_full()
      .min_h(0)
      .p(5)
      .bg(t.popover)
      .text_size(TEXT.body)
      .on_key_down((event, cx) => {
        if (event.key === "down") this.at = (this.at + 1) % Math.max(1, list.length);
        else if (event.key === "up")
          this.at = (this.at + Math.max(1, list.length) - 1) % Math.max(1, list.length);
        else if (event.key === "enter") this.choose(this.at);
        else if (event.key === "escape") closeOverlay(props()?.overlay);
        else return;
        cx?.notify?.();
      })
      .children(list.map((row, index) => this.row(t, row, index)))
      .when(Boolean(this.failure), (el) =>
        el.child(
          div()
            .px(8)
            .pt(4)
            .text_size(TEXT.label)
            .text_color(t.blocked)
            .child(this.failure),
        ),
      );
  }

  /** One line of a menu: a separator, a heading, a note, or a row. */
  row(t, item, index) {
    if (item?.sep) return div().mx(8).my(4).h(1).bg(t.border);
    if (item?.heading) {
      return div()
        .h(28)
        .flex_none()
        .px(8)
        .flex()
        .items_center()
        .text_size(TEXT.label)
        .font_medium()
        .text_color(t.dim)
        .child(item.heading);
    }
    if (item?.note) {
      return div()
        .flex_none()
        .px(8)
        .py(6)
        .text_size(TEXT.label)
        .text_color(t.dim)
        .child(item.note);
    }
    return div()
      .flex_none()
      .when(index === this.at, (el) => el.bg(t.selected).rounded(RADIUS.control))
      .child(
        menuRow(t, `mi-${index}`, item.label, {
          icon: item.icon ? (ICON[item.icon] ?? null) : null,
          hint: item.hint ?? null,
          disabled: Boolean(item.disabled),
          onClick: () => this.choose(index),
        }),
      );
  }
}
