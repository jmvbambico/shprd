// The little pieces every shprd panel is built from: a card, its header, a row,
// a count badge and a quiet note.
//
// The values are Shepherdr's (`lib/ui.js`), whose window uses one closed system:
// a 42 px panel header, 24 px controls, two-line rows of 32 and 44 px, one
// primary per surface, no tooltips and nothing that moves on hover.
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `lib/ui.js` (`cardHeader`, `countBadge`, `heading`, `menuRow`,
//   `button`), `lib/UI.md` (the house rules)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// Shared: the source is `mods/lib/panel.js`; `bin/sync-shared` copies it into
// every mod folder as `lib/panel.js`. Edit the source, never a copy.

import { div } from "gpui-kit";
import { h_flex, v_flex } from "gpui-base";

import { H, RADIUS, SPACE, TEXT } from "./tokens.js";

/**
 * A card: the panel's rounded, bordered background. Every panel in the layout is
 * one, and a pane frame uses the same 10/1 treatment so a panel and a pane read
 * as siblings (Shepherdr's rule).
 */
export function card(t, children) {
  return v_flex()
    .size_full()
    .bg(t.background)
    .border_1()
    .border_color(t.border)
    .rounded(RADIUS.panel)
    .overflow_hidden()
    .children(children);
}

/** A panel's 42 px header row: its title, then whatever sits at the right. */
export function cardHeader(t, title, right = null) {
  return h_flex()
    .h(SPACE.panelHeader)
    .flex_none()
    .px(12)
    .gap(8)
    .items_center()
    .border_b(1)
    .border_color(t.border)
    .text_size(TEXT.body)
    .font_medium()
    .text_color(t.foreground)
    .child(title)
    .child(div().flex_1())
    .when(right !== null, (el) => el.child(right));
}

/** A quiet section heading: 28 px, dim, sentence case. */
export function heading(t, text) {
  return h_flex()
    .h(SPACE.sidebarRow)
    .flex_none()
    .px(12)
    .items_center()
    .text_size(TEXT.label)
    .text_color(t.dim)
    .child(text);
}

/** A row: `left` fills, `right` sits at the end. Height is the caller's. */
export function row(t, { height = SPACE.row, left, right = null, hover = false, onClick = null } = {}) {
  const base = h_flex()
    .h(height)
    .flex_none()
    .px(12)
    .gap(8)
    .items_center()
    .rounded(RADIUS.card)
    .when(hover, (el) => el.cursor_pointer().hover((style) => style.bg(t.hover)));
  const withClick = onClick ? base.on_click(onClick) : base;
  return withClick
    .child(div().flex_1().child(left))
    .when(right !== null, (el) => el.child(right));
}

/**
 * A count badge: up to two numbers, filled in the tone they name, text on the
 * fill. It is the one place a saturated colour appears in a row.
 */
export function badge(t, text, fill, textColor) {
  return h_flex()
    .h(H.xs - 2)
    .px(5)
    .flex_none()
    .items_center()
    .rounded(RADIUS.control)
    .bg(fill)
    .text_size(TEXT.id)
    .font_medium()
    .text_color(textColor)
    .child(text);
}

/**
 * A dot of `color` and a word: what a status line puts beside a count.
 *
 * It is a filled circle rather than an icon, so a line reads at 12 px without an
 * asset, and nothing shifts when the count changes.
 */
export function label(t, color, text) {
  return h_flex()
    .gap(6)
    .items_center()
    .child(div().w(6).h(6).rounded(3).bg(color))
    .child(text);
}

/** A count as a badge shows it: `9` up to nine, then `9+` (Shepherdr's cap). */
export function countBadge(n) {
  const value = Number.isFinite(n) && n > 0 ? Math.floor(n) : 0;
  return value > 9 ? "9+" : String(value);
}

/** A quiet line of words inside a panel, wrapped by the layout. */
export function note(t, text) {
  return div()
    .px(12)
    .py(6)
    .text_size(TEXT.label)
    .text_color(t.muted)
    .child(text);
}

/** A key cap, as a row of words shows a binding (`F`, `⌘K`). */
export function keycap(t, text) {
  return h_flex()
    .h(H.xs - 2)
    .px(4)
    .flex_none()
    .items_center()
    .rounded(RADIUS.control)
    .bg(t.raised)
    .border_1()
    .border_color(t.border)
    .text_size(TEXT.id)
    .text_color(t.muted)
    .child(text);
}

/**
 * The status glyph, in a fixed box so nothing moves when a state changes.
 *
 * It is drawn rather than shipped: the layout carries no icon assets, and a mod's
 * `svg()` needs a file beside it. The shapes are Shepherdr's (`lib/dot.js`): a
 * ring for what waits, a filled dot for *Needs you*, a disc for *Ready*, a
 * half-filled ring for *Working*, a paused ring for *Merging*. `shape` is a row's
 * `{glyph, tone}`, as `lib/spaces.js` and `lib/tabstrip.js` build it, and `colors`
 * is `tone(theme())`.
 */
export function statusGlyph(t, shape, colors, size = 14) {
  const color = !shape || shape.tone === "dim" ? t.dim : (colors?.[shape.tone] ?? t.dim);
  const box = div().w(size).h(size).flex_none().flex().items_center().justify_center();
  const kind = shape?.glyph ?? "ring";
  const round = Math.floor(size / 2);
  const inner =
    kind === "dot"
      ? div().w(size - 4).h(size - 4).rounded(Math.floor((size - 4) / 2)).bg(color)
      : kind === "tick"
        ? div().w(size).h(size).rounded(round).bg(color)
        : div().w(size).h(size).rounded(round).border_1().border_color(color);
  return box.child(inner);
}

/**
 * The body of a panel that M10.4–M10.6–M10.8 will fill.
 *
 * It is a placeholder that says so, in the panel and not in a tooltip (the
 * window has none: Shepherdr's house rule, and this layout keeps it): a person
 * looking at the region learns which task fills it, and nothing claims to work
 * before it does.
 */
export function placeholder(t, title, note_, task) {
  return v_flex()
    .size_full()
    .child(cardHeader(t, title, keycap(t, task)))
    .child(heading(t, "Not built yet"))
    .child(note(t, note_))
    .child(div().flex_1());
}
