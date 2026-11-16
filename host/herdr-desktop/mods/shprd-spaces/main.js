// The spaces sidebar: the run's space, then one row per task space, one line each
// — a status glyph, the task, and the red/green counts that need the person.
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
// - Paths: `mods/spaces-chat/main.js` (a 30 px row per space: a mark, the name and
//   the git tail, the red/green counts at the end, the "Spaces" heading with its
//   "+", the rail under 160 px, right-click for the row's menu), `mods/spaces-chat/
//   popups.js` (the menu's popup, its height, and the close confirm), `core/menus.js`
//   (`spaceMenu`'s items and order), `lib/coordinator.js` (the confirm dialog's
//   words and size)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// The data path is shprd's: `GET /api/state` for the tasks and their words, the
// service's change file for "look again" (`lib/watch.js`), and Herdr's live
// `spaces()` for the rows a click can land on. Nothing here decides what a task
// is doing — `lib/spaces.js` joins the two and the service's rules win.
//
// The row menu and the close confirm are this mod opened as an overlay
// (`props().view`), which is Shepherdr's own shape: its sidebar draws its menus
// from `popups.js` the same way, because a menu has to float over the window and
// at the pointer, not inside the column it is drawn in.
//
// Dragging is not ported. Shepherdr drags a space among its own, into a section
// or into Pinned, and keeps that order in its own `shared` state; shprd's order
// is the service's (needs-you first, then ready, and so on), the engine makes and
// names the spaces (`ensure_space`), and a hand-moved order would be undone by
// the next pass. The order a person can rely on here is the board's.

import { View, div } from "gpui-kit";
import { h_flex, v_flex } from "gpui-base";
import { closeOverlay, layout, openOverlay, props, run, setShared, spaces, theme } from "herdr";

import { attach, reason } from "./lib/client.js";
import { KIND, MENU, attention, closeWords, menuHeight, menuItems, narrowSidebar, sidebarRows } from "./lib/spaces.js";
import { attentionBadge, note, statusGlyph } from "./lib/panel.js";
import { H, RADIUS, SPACE, TEXT, ref, tone } from "./lib/tokens.js";
import { followChanges } from "./lib/watch.js";

/** How often the sidebar reads the board when the change file says nothing. */
const FLOOR_MS = 5000;
/**
 * A row's height: one line, as Shepherdr's space rows are.
 *
 * 30 px, not the 28 of the brief: Shepherdr's `rowHeight` gives 30 to a space and
 * 28 only to a worktree child under its checkout (`mods/spaces-chat/main.js:66`),
 * and shprd has no worktree rows.
 */
const ROW_H = 30;
/** The rail's row (under 160 px): a number and a glyph, Shepherdr's `compact()`. */
const RAIL_H = 26;
/** The face shprd's ids are set in. */
const MONO = "JetBrains Mono";
/** The close confirm's size: Shepherdr's `closeSpace` asks in a 400×168 dialog. */
const CONFIRM = { width: 400, height: 168 };

export default class Spaces extends View {
  init(p, cx) {
    this.cx = cx;
    // Opened as an overlay this is one of the mod's own views, not the list: the
    // row's menu or the close confirm, named by `props().view`. The list's reads
    // never start then, so a menu does not poll the service to draw two rows.
    const given = { ...(props() ?? {}), ...(p ?? {}) };
    this.popup = given.view ? given : null;
    if (this.popup) {
      this.at = 0;
      this.handle = cx.focus_handle();
      this.handle.focus();
      return;
    }
    this.state = null;
    this.failure = null;
    this.client = null;
    this.watching = false;
    this.refresh();
    cx.timer.every(FLOOR_MS, (cx) => this.refresh(cx));
  }

  /**
   * Reads the board once; a service that restarted or went away is looked for
   * again.
   *
   * `attach` is what notices a restart: it compares the `id` in `service.json`
   * with the one this sidebar's client was made from, so a new `shprd serve` on
   * the same port is a new client and a whole re-read rather than a sidebar that
   * keeps painting old rows while the run has moved on (M10 gate bug 5).
   */
  async refresh(cx = this.cx) {
    try {
      const next = await attach(this.client, { wait: (ms) => cx.sleep(ms) });
      if (!next.client) {
        this.client = null;
        this.state = null;
        this.failure = "shprd serve is not running";
        cx.notify();
        return;
      }
      this.client = next.client;
      this.follow();
      this.state = await this.client.state();
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

  /**
   * The heading's "+": a new task, which is shprd's counterpart of Shepherdr's
   * "New space" — a task is what makes a space here, and the form that writes one
   * lives in the Tasks panel.
   *
   * U1 gives the "+" a menu of its own (Terminal, New task…, Start an agent). It
   * is not in this build: when it lands, this one call becomes
   * `openOverlay({ mod: "shprd-new-menu", kind: "popup", … })` and nothing else
   * here changes. Until then the "+" does the row that menu would offer for a
   * space, so the heading's add is never a dead press.
   */
  addTask() {
    run("layout.select", { stack: "panel-body", child: "tasks" });
    run("layout.show", { id: "panel" });
    run("shprd-tasks.add");
  }

  /** Opens the row's menu as this mod's own popup, at the pointer. */
  menu(row) {
    const items = menuItems(row);
    openOverlay({
      mod: "shprd-spaces",
      kind: "popup",
      width: MENU.width,
      height: menuHeight(items),
      at: "pointer",
      props: {
        view: "space",
        task: row.id,
        title: row.title,
        space: row.space?.id ?? null,
      },
    });
  }

  /**
   * Opens the task in the Tasks panel: its id is shared (`tasks.thread`, the
   * panel's own contract) and the panel runs its drill-in from its own command,
   * so this mod never names a stack child that a later task might move.
   *
   * The thread's only body today is the Accept card, so a task that is not ready
   * lands on that card's own "pick a ready one" line; U3 gives the thread its
   * views and the same row then opens the task itself.
   */
  openTask(p) {
    setShared("tasks.thread", { task: p.task, title: p.title ?? "", at: Date.now() });
    run("shprd-tasks.open");
  }

  /** Asks before closing the space, in Shepherdr's words and dialog size. */
  askClose(p) {
    openOverlay({
      mod: "shprd-spaces",
      kind: "dialog",
      modal: true,
      width: CONFIRM.width,
      height: CONFIRM.height,
      props: { view: "confirm", task: p.task, space: p.space },
    });
  }

  /** Runs the item the menu or the keyboard picked. */
  choose(items, at) {
    const item = items.filter((entry) => !entry.sep)[at];
    if (!item) return;
    closeOverlay(this.popup.overlay);
    if (item.id === "open") this.openTask(this.popup);
    else if (item.id === "close") this.askClose(this.popup);
  }

  /** The confirm's verb: close the space and every program in it, as asked. */
  yes(p) {
    closeOverlay(p.overlay);
    if (p.space) run("space.close", { id: p.space, confirmed: true });
  }

  render() {
    if (this.popup) return this.popup.view === "confirm" ? this.confirm() : this.menuView();
    const t = theme();
    const colors = tone(t);
    // The rail goes by the column's width, as Shepherdr's does: dragging the edge
    // narrows it too, so no flag has to be kept in step with the layout.
    this.narrowed = narrowSidebar(layout());
    const rows = this.state ? sidebarRows(this.state, spaces()) : null;
    const sections = [];
    let no = 0;
    const row = (t, colors, entry) => this.row(t, colors, entry, ++no);
    if (!rows) {
      sections.push(
        note(t, this.failure ?? "No run yet."),
        note(t, this.failure ? "Start it from the footer below." : "Tasks appear here as the run adds them."),
      );
    } else if (this.narrowed) {
      if (rows.coordinator) sections.push(row(t, colors, rows.coordinator));
      for (const entry of [...rows.tasks, ...rows.waiting]) sections.push(row(t, colors, entry));
    } else {
      if (rows.coordinator) sections.push(row(t, colors, rows.coordinator));
      if (rows.tasks.length > 0) {
        sections.push(this.heading(t, "Task spaces", rows.tasks.length));
        for (const entry of rows.tasks) sections.push(row(t, colors, entry));
      }
      if (rows.waiting.length > 0) {
        sections.push(this.heading(t, "Waiting for a space", rows.waiting.length));
        for (const entry of rows.waiting) sections.push(row(t, colors, entry));
      }
    }
    return v_flex()
      .size_full()
      .min_h(0)
      .bg(t.chrome)
      .text_size(TEXT.body)
      .child(this.title(t))
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

  /** "Spaces", and the "+" at the right that adds one (`S:mods/spaces-chat/main.js:818`). */
  title(t) {
    return h_flex()
      .h(31)
      .flex_none()
      .pl(16)
      .pr(10)
      .gap(8)
      .items_center()
      .justify_between()
      .text_size(TEXT.label)
      .font_medium()
      .text_color(t.dim)
      .when(!this.narrowed, (el) => el.child("Spaces"))
      .child(this.addButton(t));
  }

  /**
   * The heading's "+": 24 px square, dim, bright on hover, its glyph 15 px.
   *
   * Shepherdr draws a 14 px `plus.svg` in it. shprd ships no SVG assets yet (T5
   * brings the icon set), so the glyph is the character, in the same box and the
   * same colour rules — a shape that will not move when the asset arrives.
   */
  addButton(t) {
    return h_flex()
      .id("space-new")
      .w(H.sm)
      .h(H.sm)
      .flex_none()
      .items_center()
      .justify_center()
      .rounded(RADIUS.control)
      .cursor_pointer()
      .text_size(TEXT.title - 1)
      .text_color(t.dim)
      .hover((style) => style.bg(t.hover).text_color(t.foreground))
      .on_click(() => this.addTask())
      .child("+");
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
      .child(String(count));
  }

  /**
   * One space on one line: the status glyph, the task, the count that needs the
   * person (`S:mods/spaces-chat/main.js:711-772`).
   *
   * The status is the glyph's shape and colour only — no word and no second line,
   * which is what Shepherdr's rows do; the board in the Tasks panel is where a
   * task says what it is doing. The coordinator's row carries the run in the slot
   * Shepherdr gives a repo's git state, because that is the one thing about it a
   * person cannot see anywhere else in this column.
   */
  row(t, colors, entry, no) {
    if (this.narrowed) return this.rail(t, colors, entry, no);
    const clickable = Boolean(entry.space?.id);
    const counts = attention(entry);
    const coordinator = entry.kind === KIND.coordinator;
    return h_flex()
      .id(`space-${entry.id}`)
      .h(ROW_H)
      .flex_none()
      .px(8)
      .gap(9)
      .items_center()
      .rounded(RADIUS.card)
      .when(entry.active, (el) => el.bg(t.selected))
      .when(!entry.active, (el) => el.hover((style) => style.bg(t.hover)))
      .when(clickable, (el) => el.cursor_pointer().on_click(() => this.focus(entry)))
      .on_mouse_down("right", () => this.menu(entry))
      .child(statusGlyph(t, entry, colors))
      .child(
        div()
          .flex_none()
          .when(!coordinator, (el) => el.font_family(MONO))
          .text_color(entry.kind === KIND.waiting ? t.muted : t.foreground)
          .child(coordinator ? entry.label : ref(entry.label)),
      )
      .when(coordinator, (el) => el.child(div().flex_1().min_w(0)))
      .when(!coordinator, (el) =>
        el.child(
          div()
            .flex_1()
            .min_w(0)
            .truncate()
            .text_size(TEXT.label)
            .text_color(t.muted)
            .child(entry.title),
        ),
      )
      .when(Boolean(entry.tail), (el) =>
        el.child(div().flex_none().text_size(TEXT.label).text_color(t.dim).child(entry.tail)),
      )
      .when(counts.need > 0 || counts.ready > 0, (el) =>
        el.child(attentionBadge(t, counts.need, counts.ready, colors, entry.active ? t.selected : t.chrome)),
      );
  }

  /**
   * The rail's row, under 160 px: the row's number and a glyph, nothing else
   * (`S:mods/spaces-chat/main.js:768-783`). Names do not fit, and the order is the
   * order, so a number is enough to find a space again; right-click still works.
   */
  rail(t, colors, entry, no) {
    return h_flex()
      .id(`space-${entry.id}`)
      .h(RAIL_H)
      .flex_none()
      .justify_center()
      .items_center()
      .gap(4)
      .rounded(RADIUS.card)
      .when(entry.active, (el) => el.bg(t.raised))
      .when(!entry.active, (el) => el.hover((style) => style.bg(t.hover)))
      .cursor_pointer()
      .on_click(() => this.focus(entry))
      .on_mouse_down("right", () => this.menu(entry))
      .child(
        div()
          .text_size(TEXT.label)
          .text_color(entry.active ? t.foreground : t.muted)
          .child(String(no)),
      )
      .child(statusGlyph(t, entry, colors, 12));
  }

  // -- the overlay views ----------------------------------------------------

  /** The row's menu: 32 px rows, ↑↓ move, Enter runs, Esc closes (`popups.js`). */
  menuView() {
    const t = theme();
    const p = this.popup;
    const items = menuItems(p);
    const count = items.filter((entry) => !entry.sep).length;
    let n = -1;
    return v_flex()
      .id("space-menu")
      .track_focus(this.handle)
      .w_full()
      .min_h(0)
      .p(MENU.pad)
      .bg(t.popover)
      .text_size(TEXT.body)
      .on_key_down((e, cx) => {
        if (e.key === "down") this.at = (this.at + 1) % count;
        else if (e.key === "up") this.at = (this.at + count - 1) % count;
        else if (e.key === "enter") this.choose(items, this.at);
        else if (e.key === "escape") closeOverlay(p.overlay);
        else return;
        cx.notify();
      })
      .children(
        items.map((item, k) => {
          if (item.sep) return div().id(`space-menu-sep-${k}`).mx(7).my(4).h(1).flex_none().bg(t.border);
          const at = ++n;
          return this.menuRow(t, items, item, at);
        }),
      );
  }

  /** One menu row: 32 px, the label, a hover behind it, the picked one selected. */
  menuRow(t, items, item, at) {
    return h_flex()
      .id(`space-menu-${item.id}`)
      .h(MENU.row)
      .w_full()
      .flex_none()
      .px(8)
      .gap(8)
      .items_center()
      .rounded(RADIUS.control)
      .text_size(TEXT.body)
      .text_color(t.foreground)
      .cursor_pointer()
      .when(at === this.at, (el) => el.bg(t.selected))
      .hover((style) => style.bg(t.hover))
      .on_click((_e, cx) => {
        cx.stop_propagation();
        this.choose(items, at);
      })
      .child(div().flex_1().min_w(0).truncate().child(item.label));
  }

  /**
   * The close confirm: the question, what goes with it, then Cancel and the verb.
   *
   * It is Shepherdr's `confirm-chat` dialog in the words `closeWords` gives it:
   * Enter does nothing on a destructive ask (only the click closes), Esc cancels,
   * and the button says what it does — "Close space", never "Confirm".
   */
  confirm() {
    const t = theme();
    const p = this.popup;
    const words = closeWords(p);
    return v_flex()
      .id("close-space")
      .track_focus(this.handle)
      .on_key_down((e, cx) => {
        if (e.key !== "escape") return;
        closeOverlay(p.overlay);
        cx.stop_propagation();
      })
      .size_full()
      .min_h(0)
      .p(18)
      .gap(8)
      .bg(t.popover)
      .text_size(TEXT.body)
      .child(div().text_size(TEXT.title).font_semibold().text_color(t.foreground).child(words.title))
      .child(div().flex_1().min_h(0).text_color(t.muted).child(words.detail))
      .child(div().h(8).flex_none())
      .child(
        h_flex()
          .h(H.lg)
          .w_full()
          .flex_none()
          .gap(8)
          .items_center()
          .child(div().flex_1())
          .child(this.button(t, "dialog-cancel", "Cancel", { onClick: () => closeOverlay(p.overlay) }))
          .child(this.button(t, "dialog-primary", words.verb, { primary: true, onClick: () => this.yes(p) })),
      );
  }

  /** A dialog's button: plain Cancel, one solid primary (Shepherdr's `button`). */
  button(t, id, label, { primary = false, onClick }) {
    return h_flex()
      .id(id)
      .h(H.lg)
      .flex_none()
      .px(primary ? 12 : 8)
      .items_center()
      .rounded(RADIUS.control)
      .text_size(primary ? TEXT.body : TEXT.label)
      .font_medium()
      .cursor_pointer()
      .when(primary, (el) => el.bg(t.solid).text_color(t.solid_foreground).hover((style) => style.opacity(0.88)))
      .when(!primary, (el) => el.text_color(t.muted).hover((style) => style.bg(t.hover).text_color(t.foreground)))
      .on_click((_e, cx) => {
        cx.stop_propagation();
        onClick();
      })
      .child(label);
  }
}
