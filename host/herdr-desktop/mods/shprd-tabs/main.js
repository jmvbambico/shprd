// The tab bar: the shown space's tabs as Shepherdr draws them — 24 px pills with
// the program's icon, the tab's name, a spinner while it works, a count badge and
// the close mark on the shown one — then the "+" menu, the side panel's buttons
// and the window's own.
//
// In shprd the tabs of one space are one task's terminals: its worker and its
// reviewer (`shprd_mux`'s one-space-per-task rule). The tab's title is what says
// which (`<agent>·<task>`, `<agent>·<task>·review`), and Herdr's live status is
// what says how it is doing; `lib/tabstrip.js` joins those with the service's own
// word for the task, so a pill is never a guess. A backup that takes a task over
// keeps the worker's own tab and title and only changes the agent on it, so its
// pill reads as the worker it now is.
//
// Ported from Shepherdr's tab bar (MIT):
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/tabs-tasks/main.js` (the pill row: the program's icon, the name
//   cut at 170 px, a spinner while working, the count badge, the dot, the close
//   mark on the shown pill; double-click renames, right-click opens the tab menu,
//   drag moves, wheel steps tabs; then the "+", Files / Changes / Tasks with their
//   icons and counts, and the window's buttons), `lib/ui.js` (`H`, `countBadge`)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// The side panel's buttons open the layout's stacks — `layout.select` then
// `layout.show` — because the panel is a stack in `layouts/shprd.json`, not
// something a mod draws. Settings is not one of them: it opens over the window
// as Shepherdr's own settings do (`openSettings()` in `lib/panel.js`), from the
// sidebar's footer row, so the panel keeps its stacks for the work. The "+" menu
// and the two context menus are mods of their own (`shprd-new-menu`, `shprd-menu`),
// opened over this bar as popups; what *Starting an agent* runs is the service's
// (`POST /api/agents`), never a program this mod starts.

import { View, div, image, svg } from "gpui-kit";
import { h_flex } from "gpui-base";
import {
  layout,
  nextCommand,
  openOverlay,
  panes,
  run,
  settings,
  spaces,
  theme,
  window,
} from "herdr";

import { attach, reason } from "./lib/client.js";
import { MENU, appMenu, menuHeight, newMenu, restartable, tabMenu } from "./lib/menu.js";
import { countsBadge } from "./lib/panel.js";
import { panelButtons, tabRows } from "./lib/tabstrip.js";
import { RADIUS, TEXT, tone } from "./lib/tokens.js";

/** How often the bar re-reads the board the pills borrow their words from. */
const FLOOR_MS = 5000;

/**
 * How wide the bar has to be for a panel button to keep its word.
 *
 * Shepherdr's 1100 (`mods/tabs-tasks/main.js:187`): below it the buttons are their
 * icons and their counts, and the open one keeps its label.
 */
const ROOMY = 1100;

/** The program icon's size in a pill, and the spinner's (Shepherdr's 14 and 12). */
const ICON = 14;
const SPINNER = 12;

/**
 * The side panel's icons, in this mod's own folder.
 *
 * They are Shepherdr's own SVGs (see the header of `icons/*.svg`): the app's
 * shipped `mods/files/file.svg` and `mods/changes/changes.svg` would be Herdr's
 * Apache-2.0 assets, which this repository may not carry, and a path into
 * another mod's folder is a file `herdr-app check` cannot see. Changes takes
 * `git-compare`, which is the glyph Shepherdr's own icon set has for it.
 */
const PANEL_ICON = {
  files: "mods/shprd-tabs/icons/files.svg",
  changes: "mods/shprd-tabs/icons/git-compare.svg",
  tasks: "mods/shprd-tabs/icons/tasks.svg",
  asks: "mods/shprd-tabs/icons/file-text.svg",
  settings: "mods/shprd-tabs/icons/settings.svg",
};

/** The pill's spinner and its close mark, from the same set. */
const SPINNER_ICON = "mods/shprd-tabs/icons/spinner.svg";
const CLOSE_ICON = "mods/shprd-tabs/icons/x.svg";

export default class Tabs extends View {
  init(_props, cx) {
    this.cx = cx;
    this.state = null;
    this.client = null;
    this.shown = "tasks";
    // The roster, read once from the service: the "+" menu's rows are its
    // programs, and the bar asks for the popup's height before it opens, so the
    // two must agree about how many rows there are. `undefined` is "not read
    // yet"; `null` is "the service could not say", which the menu shows.
    this.roster = undefined;
    // The bar's width, from its own size change: a button keeps its word only
    // when there is room for it (Shepherdr's rule).
    this.width = 0;
    // The pill a left press went down on: a drag to another pill is a `tab.move`,
    // a release on the same one is a `tab.select`.
    this.pressed = null;
    this.refresh();
    cx.timer.every(FLOOR_MS, (cx) => this.refresh(cx));
    // The keys' way in (`layout:shprd` in keys.json): Ctrl+Shift+N comes here as
    // `new-task`, because this bar is always on screen and the Tasks panel's own
    // mod is not when the panel is hidden — so it is shown first, then asked for
    // its form (Shepherdr's `tabs-tasks.newspace`). One `nextCommand` at a time.
    cx.spawn(async () => {
      for (;;) {
        const command = await nextCommand();
        if (command === "new-task") {
          run("layout.select", { stack: "panel-body", child: "tasks" });
          run("layout.show", { id: "panel" });
          run("shprd-tasks.add");
        }
      }
    });
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
      // The pills are painted before the roster is asked for: the roster can take
      // a moment (a program that lists its own models is a process), and a bar
      // that waits for it is a bar with no counts on the first frame. The "+"
      // menu gets it when it is there, and reads it itself when it is not.
      cx.notify();
      if (this.roster === undefined) {
        this.roster = await this.client.roster().catch(() => null);
      }
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

  /** The space the bar is drawing, or null. */
  space() {
    return spaces().find((entry) => entry.active) ?? null;
  }

  /**
   * The tab menu: Rename…, Restart when the tab can, Close tab.
   *
   * It opens at the pointer, 236 wide as Shepherdr's does
   * (`mods/tabs-tasks/main.js:166`), and its height comes from the rows it will
   * draw, so a menu is never taller or shorter than what is in it.
   */
  tabMenu(row) {
    const rows = tabMenu({ id: row.id, restartable: restartable(row, panes()) });
    openOverlay({
      mod: "shprd-menu",
      kind: "popup",
      width: MENU.tab,
      height: menuHeight(rows),
      props: { tab: "tab", target: { id: row.id, space: this.space()?.id ?? null } },
    });
  }

  /** The app menu: a right-click on the bar's own empty part. */
  appMenu() {
    const rows = appMenu();
    openOverlay({
      mod: "shprd-menu",
      kind: "popup",
      width: MENU.app,
      height: menuHeight(rows),
      props: { tab: "global" },
    });
  }

  /**
   * The "+" menu: Terminal, New task…, and the roster's programs (1–9).
   *
   * The popup is as tall as its rows — the roster was read when the bar started,
   * so the height is known here and the menu is never a box with dead space at
   * the bottom. What the rows run is the menu mod's business; the roster is passed
   * with them so the two cannot disagree.
   */
  newMenu() {
    const rows = newMenu({ roster: this.roster ?? null, loading: this.roster === undefined });
    openOverlay({
      mod: "shprd-new-menu",
      kind: "popup",
      width: MENU.new,
      height: menuHeight(rows),
      props: { roster: this.roster ?? null },
    });
  }

  /**
   * The rename prompt, as Shepherdr's *Rename tab*: 400 × 196, the field holding
   * the name given (empty when nobody named it, so the tab keeps its number).
   *
   * `tab.rename` is Herdr's own verb and the app's own title ownership: shprd
   * names a tab `<agent>·<task>` when it opens it, and a rename is the person
   * taking the name back. The pill then stops reading the name as a title
   * (`lib/tabstrip.js` `parseTabName`), which is exactly what a renamed tab is.
   */
  rename(row) {
    openOverlay({
      mod: "shprd-prompt",
      kind: "dialog",
      modal: true,
      width: 400,
      height: 196,
      props: {
        title: "Rename tab",
        // The field holds the name someone gave it; a tab nobody named keeps its
        // number, which is the placeholder (Shepherdr's `renameTab`).
        value: row.named ? row.name : "",
        placeholder: row.name,
        command: { name: "tab.rename", args: { id: row.id } },
        verb: "Save",
      },
    });
  }

  render() {
    const t = theme();
    const colors = tone(t);
    const space = this.space();
    const rows = tabRows(space, panes(), this.state);
    const pills = rows.map((row) => this.pill(t, colors, row));
    // The buttons are data until they are drawn: the same list is what says which
    // stacks the panel has, and each becomes one element below.
    const buttons = panelButtons(this.state, this.shown);
    const w = window();
    // A bare window with its own title bar keeps the traffic lights clear: the
    // bar's left padding grows by whatever the sidebar does not cover.
    const lightsPadding = this.lightsPadding(w);
    return h_flex()
      .id("tabs-bar")
      .size_full()
      .min_w(0)
      .px(10)
      .pl(lightsPadding)
      .gap(3)
      .items_center()
      .bg(t.chrome)
      .border_b(1)
      .border_color(t.border)
      .text_size(TEXT.label)
      .on_size_change((size, cx) => {
        this.width = size?.width ?? 0;
        cx?.notify?.();
      })
      .on_mouse_up("left", () => {
        this.pressed = null;
      })
      // A wheel over the bar steps the tabs: wheeling down (a negative delta.y in
      // gpui, where a positive one scrolls the content down) goes to the next one,
      // as every tab strip does; less than one line of movement is a stray event.
      .on_scroll_wheel((event) => {
        if (Math.abs(event?.delta?.y ?? 0) >= 1) {
          run(event.delta.y > 0 ? "tab.prev" : "tab.next");
        }
      })
      // The bar's own right-click: the app's menu (New task…, Keybinds). A pill's
      // own press stops here first, so a tab's menu is the tab's.
      .on_mouse_down("right", (_event, cx) => {
        cx?.stop_propagation?.();
        this.appMenu();
      })
      .child(
        h_flex()
          .id("tab-scroll")
          .flex_1()
          .min_w(0)
          .h_full()
          .gap(3)
          .items_center()
          .overflow_x_scrollbar()
          .children(pills),
      )
      .child(this.addButton(t))
      .child(
        h_flex()
          .gap(3)
          .items_center()
          .flex_none()
          .children(buttons.map((button) => this.button(t, colors, button))),
      )
      .when(Boolean(w?.buttons), (el) =>
        el
          .child(this.windowButton(t, "minimize", "−", () => run("window.minimize")))
          .child(
            this.windowButton(t, "maximize", w?.maximized ? "❐" : "□", () =>
              run("window.maximize"),
            ),
          )
          .child(this.windowButton(t, "close-window", "✕", () => run("window.close"))),
      );
  }

  /**
   * The bar's left padding: 10, unless this is a macOS window drawing its own
   * title bar with the sidebar hidden, and then enough to clear the lights.
   *
   * Shepherdr's `lightsPadding` (`mods/tabs-tasks/main.js:174-178`), with the
   * layout's own `traffic_lights` and the app's zoom, because a bar under the
   * window's traffic lights is a bar whose first pill cannot be clicked.
   */
  lightsPadding(w) {
    const root = layout();
    const sidebar = find(root, "sidebar");
    if (
      w?.platform !== "macos" ||
      w?.titlebar !== "none" ||
      w?.fullscreen ||
      !Array.isArray(root?.traffic_lights)
    ) {
      return 10;
    }
    const zoom = Number(settings()?.zoom) || 1;
    const covered = sidebar?.hidden ? 0 : (sidebar?.size?.px ?? 0);
    return Math.max(10, (root.traffic_lights[0] + 64) / zoom - covered);
  }

  /**
   * The 24 px "+" after the pills: Shepherdr's `tab-new` button, which opens the
   * `+` menu (`mods/tabs-tasks/main.js:208`).
   */
  addButton(t) {
    return div()
      .id("tab-new")
      .w(24)
      .h(24)
      .flex_none()
      .flex()
      .items_center()
      .justify_center()
      .rounded(RADIUS.control)
      .cursor_pointer()
      .text_size(TEXT.body)
      .text_color(t.dim)
      .hover((style) => style.bg(t.hover).text_color(t.foreground))
      .on_click(() => this.newMenu())
      .child("+");
  }

  /** One window button (only where the system draws none: Windows and Linux). */
  windowButton(t, id, glyph, onClick) {
    return div()
      .id(id)
      .w(24)
      .h(24)
      .flex_none()
      .flex()
      .items_center()
      .justify_center()
      .rounded(RADIUS.control)
      .cursor_pointer()
      .text_color(t.dim)
      .hover((style) => style.bg(t.hover).text_color(t.foreground))
      .on_click(onClick)
      .child(glyph);
  }

  /**
   * One pill: the program's icon, the name cut at 170 px, a spinner while it
   * works, the count badge, and the close mark on the shown tab.
   *
   * 24 px tall, as Shepherdr's is (`H.sm`, `mods/tabs-tasks/main.js:157`). The
   * agent and the task are the title shprd gave the tab, so a person reading the
   * bar sees exactly what the run opened; the role and the status word are not
   * drawn, because Shepherdr's pill has neither and the badge and the spinner say
   * what matters.
   */
  pill(t, colors, row) {
    const fill = row.tone === "dim" ? t.dim : (colors[row.tone] ?? t.dim);
    const line = row.active ? t.raised : t.chrome;
    return h_flex()
      .id(`tab-${row.id}`)
      .h(24)
      .flex_none()
      .px(8)
      .gap(6)
      .items_center()
      .rounded(RADIUS.control)
      .border_1()
      .border_color(row.active ? t.border : t.chrome)
      .bg(row.active ? t.raised : t.chrome)
      .text_size(TEXT.body)
      .text_color(row.active ? t.foreground : t.muted)
      .cursor_pointer()
      .hover((style) => style.bg(t.hover))
      .on_mouse_down("left", (event, cx) => {
        cx?.stop_propagation?.();
        this.pressed = row;
        // A double-click renames, as Shepherdr's pill does.
        if ((event?.click_count ?? 0) === 2) this.rename(row);
      })
      .on_mouse_up("left", () => {
        const from = this.pressed;
        this.pressed = null;
        if (from && from.index !== row.index) {
          // A drag across the bar moves the pressed tab to where it was dropped.
          // The host counts a tab's index from one, so the target is the dropped
          // tab's own position in its space (Shepherdr's `tab.move`, `:163`).
          run("tab.move", { id: from.id, index: row.index + 1 });
        } else {
          run("tab.select", { id: row.id });
        }
      })
      .on_mouse_down("right", (_event, cx) => {
        cx?.stop_propagation?.();
        this.tabMenu(row);
      })
      .when(Boolean(row.icon), (el) =>
        el.child(
          row.tint
            ? svg(row.icon).size(ICON).flex_none().text_color(t.muted)
            : image(row.icon).size(ICON).flex_none(),
        ),
      )
      .child(
        div()
          .max_w(170)
          .flex_none()
          .truncate()
          .child(row.zoomed ? `${row.label} Z` : row.label),
      )
      .when(row.status === "working", (el) =>
        el.child(
          svg(SPINNER_ICON)
            .size(SPINNER)
            .flex_none()
            .text_color(t.working)
            .spin(900),
        ),
      )
      .when(row.need > 0 || row.ready > 0, (el) =>
        el.child(countsBadge(t, row.need, row.ready, line, colors)),
      )
      .when(
        !(row.need > 0 || row.ready > 0) && (row.status === "blocked" || row.status === "done"),
        (el) =>
          el.child(
            div()
              .w(6)
              .h(6)
              .flex_none()
              .rounded(3)
              .bg(fill),
          ),
      )
      .when(row.active, (el) =>
        el.child(
          div()
            .id(`close-${row.id}`)
            .flex_none()
            .cursor_pointer()
            .text_color(t.dim)
            .hover((style) => style.text_color(t.foreground))
            .on_mouse_down("left", (_event, cx) => cx?.stop_propagation?.())
            .on_click((_event, cx) => {
              cx?.stop_propagation?.();
              run("tab.close", { id: row.id });
            })
            .child(svg(CLOSE_ICON).size(12)),
        ),
      );
  }

  /**
   * One side-panel button: the icon first, the word when there is room (or when
   * that stack is the one showing), then its counts.
   *
   * Shepherdr's `panelButton` (`mods/tabs-tasks/main.js:188-200`). The counts are
   * one number a button, red what needs the person and green what is ready, so a
   * button reads as a place to look rather than a decoration.
   */
  button(t, colors, button) {
    if (!button) return div();
    const roomy = this.width >= ROOMY;
    const badge = button.needs + button.ready;
    return h_flex()
      .id(`panel-${button.id}`)
      .h(24)
      .flex_none()
      .pl(5)
      .pr(roomy || button.active || badge > 0 ? 8 : 5)
      .gap(5)
      .items_center()
      .rounded(RADIUS.control)
      .text_size(TEXT.label)
      .bg(button.active ? t.selected : t.chrome)
      .text_color(button.active ? t.foreground : t.muted)
      .cursor_pointer()
      .hover((style) => style.bg(button.active ? t.selected : t.hover).text_color(t.foreground))
      .on_click(() => this.panel(button.id))
      .when(Boolean(PANEL_ICON[button.id]), (el) =>
        el.child(svg(PANEL_ICON[button.id]).size(14).flex_none()),
      )
      .when(roomy || button.active, (el) => el.child(button.word))
      .when(badge > 0, (el) =>
        el.child(
          countsBadge(
            t,
            button.needs,
            button.ready,
            button.active ? t.selected : t.chrome,
            colors,
          ),
        ),
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
