// The model menu and the pick list: the popovers every dropdown of the Settings
// window opens.
//
// A dropdown is a popup of its own, a separate mod over the window, as Shepherdr's
// is (`mods/chat-models`): a popup is above the dialog, takes its own clicks, and
// closes on a click beside it or on `Esc`. Drawing the list inside the dialog's own
// panel is what made the old dropdowns dead (see `press()` in `lib/panel.js`).
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/chat-models/main.js` (the menu: tabs, search, models, Reasoning,
//   Fast, Permissions; the pick list), `core/settings.js`
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// Two modes, by props:
//
// - `{ mode: "models", key, current: { agent, model, effort, fast, perm }, menus,
//   icons, overlay, light?, allowNone?, taken? }` — the model menu. `menus` is
//   `menusOf(roster)`: every program the roster says is installed, listed in a
//   dropdown at the top (a row of tabs does not scale to many CLIs). Every change
//   goes back at once as shared `shprd.settings.pick` = `{ key, field:
//   "selection", value, at }`, which the window applies as writes. `light` is a
//   program-and-model choice only (the reviewer's fallback): no Reasoning, Fast or
//   Permissions, and the menu closes on a pick; `allowNone` adds a "None" row, which
//   answers `value: null`; `taken` is the programs another line of the same role
//   already runs, shown but not pickable.
// - `{ mode: "pick", key, field, options: [{ id, label, hint? }], current, overlay }`
//   — a plain list with a check on the current one; the choice goes back as
//   `{ key, field, value, at }`.

import { View, div, svg, image } from "gpui-kit";
import { h_flex, v_flex, InputState, Input } from "gpui-base";
import { theme, props, setShared, closeOverlay } from "herdr";

import {
  chooseModel,
  fastOffered,
  levelOf,
  levelsFor,
  levelWord,
  modelRows,
  PICK_KEY as PICK,
} from "./lib/settings.js";
import { iconFile, press } from "./lib/panel.js";


// Sizes, as the window counts them for the popup's height.
const ROW = 31;
const HEAD = 26;
const SEP = 9;
const SEG = 34;
const PROGRAM = 34;
const SEARCH = 36;
const LIST_ROWS = 8;

export default class ModelMenu extends View {
  init(_props, cx) {
    const p = props() ?? {};
    this.p = p;
    this.handle = cx.focus_handle();
    if (p.mode === "pick") {
      cx.timer.after(0, () => this.handle.focus());
      return;
    }
    this.sel = { ...(p.current ?? {}) };
    this.tab = p.menus?.programs?.[this.sel.agent] ? this.sel.agent : (p.menus?.order ?? [])[0] ?? null;
    this.permOpen = false;
    this.progOpen = false;
    this.cursor = 0;
    this.input = InputState.new({ placeholder: "Search every agent’s models" });
    this.input.on("change", (_e, cx2) => {
      this.cursor = 0;
      cx2.notify();
    });
    this.input.on("submit", (_e, cx2) => {
      const row = this.rows()[this.cursor];
      if (row) this.pickRow(row);
      cx2.notify();
    });
    this.input.focus();
    cx.timer.after(0, () => this.input.focus());
  }

  close() {
    this.input?.release?.();
  }

  menus() {
    return this.p.menus ?? { order: [], programs: {} };
  }

  rows() {
    return this.tab ? modelRows(this.menus(), this.tab, this.input?.value?.() ?? "") : [];
  }

  /** The window hears every change at once. */
  tell() {
    setShared(PICK, { key: this.p.key, field: "selection", value: { ...this.sel }, at: Date.now() });
  }

  pickRow(row) {
    const menus = this.menus();
    if (row.typed) {
      const program = menus.programs[row.agent];
      if (program && !program.models.some((m) => m.id === row.id)) {
        program.models = [{ id: row.id, name: row.id, efforts: [], defaultEffort: null, fast: false }, ...program.models];
      }
    }
    this.sel = chooseModel(menus, this.sel, row.agent, row.id);
    this.tab = row.agent;
    this.input.set_value("");
    this.cursor = 0;
    this.tell();
    if (this.p.light === true) closeOverlay(this.p.overlay);
  }

  /** The program dropdown's choice: that program's models are listed. */
  pickProgram(agent) {
    if ((this.p.taken ?? []).includes(agent)) return;
    this.tab = agent;
    this.progOpen = false;
    this.input.set_value("");
    this.cursor = 0;
  }

  /** "None" (the reviewer's fallback): nothing, and the menu closes. */
  pickNone() {
    setShared(PICK, { key: this.p.key, field: "selection", value: null, at: Date.now() });
    closeOverlay(this.p.overlay);
  }

  icon(agent, t) {
    const ic = this.p.icons?.[agent];
    if (!ic?.icon) return div().size(14).flex_none();
    return ic.tint ? svg(ic.icon).size(14).flex_none().text_color(t.foreground) : image(ic.icon).size(14).flex_none();
  }

  check(t, on) {
    return div().size(14).flex_none().when(on, (el) => el.child(svg(iconFile("check")).size(14).text_color(t.foreground)));
  }

  head(t, label) {
    return div().h(HEAD).flex_none().px(9).pt(8).text_size(11).text_color(t.dim).child(label);
  }

  row(t, id, fn) {
    return h_flex()
      .id(id)
      .h(ROW)
      .flex_none()
      .px(9)
      .gap(10)
      .rounded(7)
      .items_center()
      .text_color(t.foreground)
      .cursor_pointer()
      .hover((s) => s.bg(t.selected ?? t.hover))
      .on_click(press(fn));
  }

  render() {
    const t = theme();
    if (this.p.mode === "pick") return this.pickList(t);
    const menus = this.menus();
    const sel = this.sel;
    const program = menus.programs[sel.agent] ?? null;
    const rows = this.rows();
    this.cursor = Math.max(0, Math.min(this.cursor, rows.length - 1));
    const light = this.p.light === true;
    const levels = levelsFor(menus, sel);
    const level = levelOf(menus, sel);
    const fast = fastOffered(menus, sel);
    const perms = program?.perms ?? [];
    const perm = perms.find((x) => x.id === sel.perm) ?? perms.find((x) => x.id === program?.defaultPerm) ?? null;
    const listH = Math.min(LIST_ROWS, Math.max(rows.length, 1)) * ROW;
    const note = this.tab ? (menus.programs[this.tab]?.note ?? null) : null;

    // The program is a dropdown, not a row of tabs: it holds as many CLIs as the
    // machine has. Opened, its list takes the models' place below the search.
    const taken = this.p.taken ?? [];
    const programSelect = h_flex()
      .h(PROGRAM)
      .flex_none()
      .px(4)
      .pb(4)
      .items_center()
      .child(
        h_flex()
          .id("program-select")
          .w_full()
          .h(30)
          .px(10)
          .gap(8)
          .rounded(7)
          .bg(t.raised)
          .items_center()
          .cursor_pointer()
          .hover((s2) => s2.bg(t.selected ?? t.hover))
          .when(this.progOpen, (el) => el.bg(t.selected ?? t.hover))
          .on_click(
            press(() => {
              this.progOpen = !this.progOpen;
            }),
          )
          .child(this.tab ? this.icon(this.tab, t) : div().size(14).flex_none())
          .child(
            div()
              .flex_1()
              .min_w(0)
              .truncate()
              .font_medium()
              .child(this.tab ? (menus.programs[this.tab]?.name ?? this.tab) : "Select program"),
          )
          .child(svg(iconFile("chevron-down")).size(12).flex_none().text_color(t.dim)),
      );
    const programList = v_flex()
      .id("program-list")
      .flex_none()
      .h(Math.min(LIST_ROWS, Math.max(menus.order.length, 1)) * ROW)
      .overflow_y_scrollbar()
      .children(
        menus.order.map((a) =>
          this.row(t, `program-${a}`, () => this.pickProgram(a))
            .when(taken.includes(a), (el) => el.opacity(0.5))
            .child(this.icon(a, t))
            .child(div().flex_1().min_w(0).truncate().child(menus.programs[a].name))
            .when(taken.includes(a), (el) =>
              el.child(div().flex_none().text_size(11).text_color(t.dim).child("another line has it")),
            )
            .child(this.check(t, a === this.tab)),
        ),
      );

    const search = h_flex()
      .h(SEARCH)
      .flex_none()
      .px(4)
      .pb(4)
      .items_center()
      .child(
        h_flex()
          .w_full()
          .h(30)
          .px(10)
          .gap(8)
          .rounded(7)
          .bg(t.raised)
          .items_center()
          .child(svg(iconFile("search")).size(13).flex_none().text_color(t.dim))
          .child(Input.new(this.input).flex_1().h_full().text_size(12.8).text_color(t.foreground)),
      );

    const noneRow = this.p.allowNone
      ? this.row(t, "none", () => this.pickNone())
          .child(div().size(14).flex_none())
          .child(div().flex_1().child("None"))
          .child(this.check(t, !sel.agent))
      : null;
    const list =
      rows.length === 0
        ? div().min_h(ROW).flex_none().px(9).py(6).text_size(11.5).text_color(t.dim).child("No models.")
        : v_flex()
            .id("model-list")
            .flex_none()
            .h(listH)
            .overflow_y_scrollbar()
            .children(
              rows.map((x, i) =>
                this.row(t, `m-${x.agent}-${x.id}`, () => this.pickRow(x))
                  .when(i === this.cursor && (this.input?.value?.() ?? "") !== "", (el) => el.bg(t.selected ?? t.hover))
                  .child(this.icon(x.agent, t))
                  .child(div().flex_1().min_w(0).truncate().child(x.name))
                  .when(x.typed, (el) =>
                    el.child(
                      div().flex_none().text_size(11).text_color(t.dim).child(`${menus.programs[x.agent]?.name ?? x.agent} id`),
                    ),
                  )
                  .child(this.check(t, !x.typed && x.agent === sel.agent && x.id === sel.model)),
              ),
            );

    const reasoning =
      levels.length === 0
        ? null
        : v_flex()
            .flex_none()
            .child(this.head(t, "Reasoning"))
            .child(
              h_flex()
                .h(SEG)
                .flex_none()
                .px(4)
                .items_center()
                .child(
                  h_flex()
                    .w_full()
                    .h(28)
                    .p(2)
                    .gap(2)
                    .rounded(7)
                    .bg(t.raised)
                    .children(
                      levels.map((e) =>
                        h_flex()
                          .id(`level-${e}`)
                          .flex_auto()
                          .min_w(0)
                          .px(4)
                          .h_full()
                          .justify_center()
                          .items_center()
                          .rounded(5)
                          .text_size(11.5)
                          .when(level === e, (el) => el.bg(t.background).text_color(t.foreground).font_medium())
                          .when(level !== e, (el) =>
                            el
                              .text_color(t.muted)
                              .cursor_pointer()
                              .hover((s) => s.text_color(t.foreground))
                              .on_click(
                                press(() => {
                                  this.sel = { ...this.sel, effort: e };
                                  this.tell();
                                }),
                              ),
                          )
                          .child(div().truncate().child(levelWord(e))),
                      ),
                    ),
                ),
            );

    const box = v_flex()
      .id("model-menu")
      .relative()
      .w_full()
      .flex_none()
      .p(5)
      .rounded(8)
      .bg(t.popover)
      .border_1()
      .border_color(t.border)
      .shadow_md()
      .text_size(12.8)
      .text_color(t.foreground)
      .when(menus.order.length > 0, (el) => el.child(programSelect))
      .child(search)
      .when(noneRow !== null && !this.progOpen, (el) => el.child(noneRow))
      .child(this.head(t, this.progOpen ? "Programs" : "Models"))
      .child(this.progOpen ? programList : list)
      .when(Boolean(note) && !this.progOpen, (el) =>
        el.child(div().px(9).py(3).text_size(11).text_color(t.dim).child(note)),
      )
      .when(!light, (el) =>
        el.child(div().h(SEP).flex_none().py(4).px(4).child(div().h(1).w_full().bg(t.border))),
      )
      .when(!light && reasoning !== null, (el) => el.child(reasoning))
      .when(!light && fast, (el) =>
        el.child(
          this.row(t, "fast", () => {
            this.sel = { ...this.sel, fast: !this.sel.fast };
            this.tell();
          })
            .child(svg(iconFile("zap")).size(14).flex_none().text_color(t.dim))
            .child(div().flex_1().child("Fast"))
            .child(
              div()
                .relative()
                .w(28)
                .h(16)
                .flex_none()
                .rounded(8)
                .bg(sel.fast ? t.accent : t.input ?? t.hover)
                .child(
                  div()
                    .id("fast-knob")
                    .absolute()
                    .top(2)
                    .left(sel.fast ? 14 : 2)
                    .size(12)
                    .rounded(6)
                    .bg(t.background),
                ),
            ),
        ),
      )
      .when(!light && perms.length > 0, (el) =>
        el.child(
          this.row(t, "perms", () => {
            this.permOpen = !this.permOpen;
          })
            .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
            .when(this.permOpen, (e) => e.bg(t.selected ?? t.hover))
            .child(svg(iconFile("shield")).size(14).flex_none().text_color(t.dim))
            .child(div().flex_1().child("Permissions"))
            .child(div().flex_none().text_size(12).text_color(t.dim).child(perm?.label ?? ""))
            .child(svg(iconFile("chevron-right")).size(12).flex_none().text_color(t.dim)),
        ),
      )
      .when(!light && this.permOpen, (el) => el.child(this.permMenu(t, perms)));

    const col = v_flex()
      .id("model-menu-area")
      .w(this.p.width ?? 340)
      .h(this.p.height ?? 600)
      .when(this.p.below !== true, (el) => el.justify_end())
      .track_focus(this.handle)
      .on_key_down((e, cx) => {
        if (e.key === "escape") {
          cx.stop_propagation();
          if (this.permOpen || this.progOpen) {
            this.permOpen = false;
            this.progOpen = false;
            cx.notify();
          } else closeOverlay(this.p.overlay);
          return;
        }
        if (e.key === "down") this.cursor = Math.min(rows.length - 1, this.cursor + 1);
        else if (e.key === "up") this.cursor = Math.max(0, this.cursor - 1);
        else if (e.key === "tab" && menus.order.length > 1) {
          // Tab steps to the next program, as it stepped along the tabs.
          const at = menus.order.indexOf(this.tab);
          this.tab = menus.order[(at + (e.modifiers?.shift ? menus.order.length - 1 : 1)) % menus.order.length];
          this.input.set_value("");
          this.cursor = 0;
        } else return;
        cx.stop_propagation();
        cx.notify();
      })
      .on_mouse_down("left", (_e, cx) => {
        if (this.permOpen) {
          this.permOpen = false;
          cx.notify();
        }
      })
      .child(box);
    return col;
  }

  /** The access modes, over the Permissions row; it takes its own presses. */
  permMenu(t, perms) {
    return v_flex()
      .id("perm-menu")
      .absolute()
      .right(5)
      .bottom(5 + ROW)
      .w(190)
      .p(5)
      .rounded(8)
      .bg(t.popover)
      .border_1()
      .border_color(t.border)
      .shadow_md()
      .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
      .children(
        perms.map((x) =>
          this.row(t, `perm-${x.id}`, () => {
            this.sel = { ...this.sel, perm: x.id };
            this.permOpen = false;
            this.tell();
          })
            .child(div().flex_1().truncate().child(x.label))
            .child(this.check(t, this.sel.perm === x.id)),
        ),
      );
  }

  /** Pick mode: one list, the current one checked. */
  pickList(t) {
    const p = this.p;
    const pick = (o) => {
      setShared(PICK, { key: p.key, field: p.field, value: o.id, at: Date.now() });
      closeOverlay(p.overlay);
    };
    return v_flex()
      .id("pick-list")
      .w(p.width ?? 260)
      .h(p.height ?? 200)
      .p(5)
      .rounded(8)
      .bg(t.popover)
      .border_1()
      .border_color(t.border)
      .shadow_md()
      .text_size(12.8)
      .text_color(t.foreground)
      .overflow_y_scrollbar()
      .track_focus(this.handle)
      .on_key_down((e, cx) => {
        if (e.key === "escape") {
          cx.stop_propagation();
          closeOverlay(p.overlay);
        }
      })
      .children(
        (p.options ?? []).map((o) =>
          this.row(t, `pick-${o.id}`, () => pick(o))
            .when(Boolean(o.icon), (el) => el.child(svg(iconFile(o.icon)).size(14).flex_none().text_color(t.dim)))
            .child(div().flex_1().min_w(0).truncate().child(o.label))
            .when(Boolean(o.hint), (el) => el.child(div().flex_none().text_size(11).text_color(t.dim).child(o.hint)))
            .child(this.check(t, o.id === p.current)),
        ),
      );
  }
}
