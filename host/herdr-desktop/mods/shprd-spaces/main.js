// The spaces sidebar (PROTOTYPE, SHPR-18): one two-line row per space (name, branch
// dimmed), and under the open space a tree of its tasks — a status glyph, the id
// and title, and for a working task its agent and progress. Spaces and their tasks
// come from `lib/tree.js` over `GET /api/state`; the Agents pane is its own mod
// (`shprd-agents`). The older notes below describe the row menu and confirm, which
// are kept.
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
import { closeOverlay, nextCommand, openOverlay, props, run, setShared, shared, spaces, theme } from "herdr";

import { attach, downNotice, reason, singleFlight } from "./lib/client.js";
import { MENU, closeWords, menuHeight, menuItems } from "./lib/spaces.js";
import { plainSpaces, spaceTree } from "./lib/tree.js";
import { note, openNewSpace, openThread, statusGlyph } from "./lib/panel.js";
import { H, RADIUS, SPACE, TEXT, ref, tone } from "./lib/tokens.js";
import { followChanges } from "./lib/watch.js";

/** How often the sidebar reads the board when the change file says nothing. */
const FLOOR_MS = 5000;
/** The face shprd's ids are set in. */
const MONO = "JetBrains Mono";
/** The close confirm's size: Shepherdr's `closeSpace` asks in a 400×168 dialog. */
const CONFIRM = { width: 400, height: 168 };

/**
 * The one badge a row ends with: *Needs you* red at the left, *Ready* green at
 * the right — Shepherdr's `AttentionBadge` (`lib/ui.js:167-176`).
 *
 * One count is a single rounded pill in that count's colour; both are one oval
 * split down the middle by a 1 px line in `line`, the colour behind the badge, so
 * a row never jumps when the second count appears. It is always 16 tall, each
 * count stops at `9+`, and the digits sit on the fill in `colors.on`.
 *
 * It is drawn here rather than in `lib/panel.js` on purpose: the badge is the
 * sidebar row's own part, and a shared module is copied into every mod by
 * `bin/sync-shared` — twelve of which would carry a function none of them draws.
 */
function rowBadge(t, counts, colors, line) {
  const num = (n) =>
    div()
      .line_height("16px")
      .when(n === 1, (el) => el.ml(1))
      .child(n > 9 ? "9+" : String(n));
  const part = (n, fill) => div().h(16).flex().items_center().justify_center().bg(fill).child(num(n));
  const shape = h_flex().h(16).flex_none().text_color(colors.on).text_size(TEXT.id).font_semibold();
  const need = Math.max(0, Math.floor(counts.need ?? 0));
  const ready = Math.max(0, Math.floor(counts.ready ?? 0));
  if (need > 0 && ready > 0) {
    return shape
      .child(part(need, colors.red).w(need > 9 ? 19 : 15).rounded_l(8))
      .child(div().w(1).h(16).bg(line))
      .child(part(ready, colors.green).w(ready > 9 ? 19 : 15).rounded_r(8));
  }
  const only = need || ready;
  return shape.child(part(only, need ? colors.red : colors.green).min_w(16).px(only > 9 ? 4 : 0).rounded(8));
}

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
    // The picked space is shared (`spaces.picked`: the Tasks list and the Graph
    // follow it; none is "All spaces"). The folded ones are this copy's own state.
    this.folded = new Set();
    this.refresh();
    cx.timer.every(FLOOR_MS, (cx) => this.refresh(cx));
    // Commands, for a key, the palette or a script: `pick:<space id>` and
    // `fold` act as their clicks do.
    cx.spawn(async () => {
      for (;;) {
        const command = String(await nextCommand());
        const space = spaceTree(this.state ?? {}, spaces()).find((entry) => command === `pick:${entry.id}`);
        if (space) this.pick(space);
        else if (command === "fold") {
          const picked = spaceTree(this.state ?? {}, spaces()).find((entry) => entry.id === this.selected);
          if (picked) this.fold(picked);
        }
      }
    });
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
        this.state = null;
        this.failure = downNotice();
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
    if (!this.client || this.watching === this.client.dir) return;
    // The service follows the shown repository: a new directory is a new watch,
    // and the loop on the old one ends at its next look.
    this.watching = this.client.dir;
    const cx = this.cx;
    const dir = this.client.dir;
    cx.spawn(async () => {
      await followChanges(dir, () => this.refresh(cx), { stop: () => this.watching !== dir });
    });
  }

  /** The picked space's id, or null for "All spaces". */
  get selected() {
    return shared("spaces.picked") ?? null;
  }

  /**
   * Picks a space: its tree opens, and the pick is shared with the other panes.
   * A space Herdr made has no tree and nothing to filter by, so a click there
   * just takes the window to it. A served space's own Herdr space (`shprd · repo`,
   * where its coordinator runs) is shown too.
   */
  pick(space) {
    if (space.plain) {
      // A plain space in a served repository's folder shows that space's tasks
      // (the Tasks list resolves it); any other has none, so those go empty and
      // "All spaces" brings them back.
      setShared("spaces.picked", space.id);
      run("space.select", { id: space.herdr });
      return;
    }
    this.folded.delete(space.id);
    setShared("spaces.picked", space.id);
    // Herdr's real space for the row, as the service reported it; a service that
    // did not falls back to the space named `shprd · <repo>`.
    const home = space.herdr
      ? { id: space.herdr }
      : spaces().find((entry) => entry.name === `shprd · ${space.name}`);
    if (home) run("space.select", { id: home.id });
    this.cx.notify();
  }

  /** Folds or unfolds a space's tree. */
  fold(space) {
    if (this.folded.has(space.id)) this.folded.delete(space.id);
    else this.folded.add(space.id);
    this.cx.notify();
  }

  /** The heading's "+": the New space dialog (a folder, Browse…, Create). */
  addSpace() {
    openNewSpace();
  }

  /**
   * Opens the row's menu as this mod's own popup, at the pointer.
   *
   * A task row offers Open task; a space Herdr made offers Open space and Close…
   * with its own id. A served repo's space has no menu: closing it would end its
   * coordinator and every agent in it, which is not the row's to do.
   */
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
        task: row.plain ? null : row.id,
        title: row.title,
        space: row.plain ? row.herdr : (row.space?.id ?? null),
        plain: Boolean(row.plain),
      },
    });
  }

  /**
   * Opens the task's thread in the Tasks panel (`openThread` in `lib/panel.js`
   * selects the stacks itself, so it works while another thread is open).
   */
  openTask(p) {
    openThread(p);
    this.cx.notify();
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
    if (item.id === "switch") run("space.select", { id: this.popup.space });
    else if (item.id === "open") this.openTask(this.popup);
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
    const tree = this.state ? spaceTree(this.state, spaces()) : null;
    const sections = [];
    if (!tree) {
      // The service is down or has no run: Herdr's own spaces still show.
      const plain = plainSpaces([], spaces());
      for (const space of plain) sections.push(this.spaceRow(t, colors, { ...space, branch: "" }));
      sections.push(
        note(t, this.failure ?? "No run yet."),
        note(t, this.failure ? "This fills in when the service answers." : "Tasks appear here as the run adds them."),
      );
    } else {
      for (const space of tree) {
        sections.push(this.spaceRow(t, colors, space));
        if (this.isOpen(space)) space.tasks.forEach((task) => sections.push(this.taskRow(t, colors, task)));
      }
    }
    return v_flex()
      .size_full()
      .min_h(0)
      .bg(t.chrome)
      .text_size(TEXT.body)
      .child(this.title(t))
      .child(
        v_flex().flex_1().min_h(0).px(8).pb(10).gap(1).overflow_y_scrollbar().mode("always").children(sections),
      );
  }

  /** Whether a space shows its tasks: the picked one (every one under "All spaces"), unless its ▾ folded it. */
  isOpen(space) {
    if (space.plain || this.folded.has(space.id)) return false;
    const picked = this.selected;
    return picked === null || picked === space.id;
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
      .child("Spaces")
      .child(this.addButton(t));
  }

  /**
   * The heading's "+": 24 px square, dim, bright on hover, its glyph 15 px.
   *
   * Shepherdr draws a 14 px `plus.svg` in it; the character is the same box and
   * colour rules, a shape that will not move when the asset arrives.
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
      .on_click(() => this.addSpace())
      .child("+");
  }

  /**
   * A space: two lines, its name and its branch dimmed under it, the badge of what
   * needs the person, and a ▾ on the open one that folds its tasks.
   */
  spaceRow(t, colors, space) {
    const open = this.isOpen(space);
    const picked = this.selected === space.id;
    return h_flex()
      .id(`space-${space.id}`)
      .h(44)
      .flex_none()
      .mt(3)
      .px(8)
      .gap(8)
      .items_center()
      .rounded(RADIUS.card)
      .when(picked, (el) => el.bg(t.selected))
      .when(!picked, (el) => el.hover((style) => style.bg(t.hover)))
      .cursor_pointer()
      .on_click(() => this.pick(space))
      .on_mouse_down("right", () => {
        if (space.plain) this.menu({ id: space.id, title: space.name, plain: true, herdr: space.herdr, space: null });
      })
      .child(
        v_flex()
          .flex_1()
          .min_w(0)
          .child(div().h(22).flex().items_center().truncate().text_color(t.foreground).child(space.name))
          .child(
            div()
              .h(16)
              .flex()
              .items_center()
              .truncate()
              .text_size(TEXT.label)
              .text_color(t.dim)
              .child(space.branch || space.folder),
          ),
      )
      .when(space.needs > 0 || space.ready > 0, (el) =>
        el.child(rowBadge(t, { need: space.needs, ready: space.ready }, colors, picked ? t.selected : t.chrome)),
      )
      .when(!space.plain && space.tasks.length > 0, (el) =>
        el.child(
          h_flex()
            .id(`space-fold-${space.id}`)
            .w(H.sm)
            .h(H.sm)
            .flex_none()
            .items_center()
            .justify_center()
            .rounded(RADIUS.control)
            .text_size(TEXT.label)
            .text_color(t.dim)
            .hover((style) => style.bg(t.hover).text_color(t.foreground))
            .on_click((_e, cx) => {
              cx.stop_propagation();
              this.fold(space);
            })
            .child(open ? "▾" : "▸"),
        ),
      );
  }

  /**
   * One task in a space's tree: ├─ or └─ guide, the status glyph, its id and
   * title, and for a working task a dim second line with its agent and progress.
   */
  taskRow(t, colors, task) {
    const picked = (shared("tasks.thread")?.task ?? null) === task.id;
    const guide = (text) => div().h(text.h).flex().items_center().child(text.s);
    return h_flex()
      .id(`task-${task.id}`)
      .flex_none()
      .ml(14)
      .pl(4)
      .pr(8)
      .items_start()
      .rounded(RADIUS.card)
      .when(picked, (el) => el.bg(t.selected))
      .when(!picked, (el) => el.hover((style) => style.bg(t.hover)))
      .cursor_pointer()
      .on_click(() => this.openTask({ task: task.id, title: task.title }))
      .on_mouse_down("right", () => this.menu({ id: task.id, title: task.title, space: null }))
      .child(
        v_flex()
          .w(24)
          .flex_none()
          .font_family(MONO)
          .text_size(TEXT.label)
          .text_color(t.border)
          .child(guide({ h: 28, s: task.last ? "└─" : "├─" }))
          .when(task.sub, (el) => el.child(guide({ h: 16, s: task.last ? "" : "│" }))),
      )
      .child(
        v_flex()
          .flex_1()
          .min_w(0)
          .child(
            h_flex()
              .h(28)
              .gap(8)
              .items_center()
              .child(statusGlyph(t, task, colors))
              .child(div().flex_none().font_family(MONO).text_size(TEXT.label).text_color(t.foreground).child(ref(task.id)))
              .child(div().flex_1().min_w(0).truncate().text_size(TEXT.label).text_color(t.muted).child(task.title)),
          )
          .when(task.sub, (el) =>
            el.child(
              div().h(16).pl(22).flex().items_center().truncate().text_size(TEXT.id).text_color(t.dim).child(task.sub),
            ),
          ),
      );
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
