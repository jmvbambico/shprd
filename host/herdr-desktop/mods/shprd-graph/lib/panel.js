// The little pieces every shprd panel is built from: a card, its header, a row,
// a count badge and a quiet note.
//
// The values are Shepherdr's (`lib/ui.js`), whose window uses one closed system:
// a 42 px panel header, 24 px controls, two-line rows of 32 and 44 px, one
// primary per surface, no tooltips and nothing that moves on hover.
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `lib/ui.js` (`cardHeader`, `countBadge`, `heading`, `menuRow`,
//   `button`, `link`, `segmented`, `toggleSwitch`, `openSettings`), `lib/UI.md`
//   (the house rules)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// Shared: the source is `mods/lib/panel.js`; `bin/sync-shared` copies it into
// every mod folder as `lib/panel.js`. Edit the source, never a copy.

import { div, svg } from "gpui-kit";
import { h_flex, v_flex } from "gpui-base";
import { openOverlay, run, setShared, shared } from "herdr";

import { H, RADIUS, SPACE, TEXT, red } from "./tokens.js";

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

/**
 * The two-number count badge: what needs the person in red, what is ready in
 * green, one oval split down the middle when both, and nothing when neither.
 *
 * Shepherdr's `countBadge` (`lib/ui.js:167-176`), drawn rather than written so the
 * digits can sit on the fill: it is always 16 tall, so a row never jumps when a
 * count changes kind, and each number stops at `9+`. `line` is the colour behind
 * the badge (the row's own), so the split reads as a line rather than a gap.
 * `colors` is `tone(theme())`: `{red, green, on}`.
 */
export function countsBadge(t, need, ready, line, colors) {
  const value = (n) => (Number.isFinite(n) && n > 9 ? "9+" : String(Math.max(0, Math.floor(n || 0))));
  const num = (n) =>
    div()
      .line_height("16px")
      .when(n === 1, (el) => el.ml(1))
      .child(value(n));
  const part = (n, fill) =>
    div()
      .h(16)
      .flex()
      .items_center()
      .justify_center()
      .bg(fill)
      .child(num(n));
  const badge = h_flex()
    .h(16)
    .flex_none()
    .text_color(colors?.on ?? t.background)
    .text_size(TEXT.id)
    .font_medium();
  if (need > 0 && ready > 0) {
    return badge
      .child(part(need, colors.red).w(need > 9 ? 19 : 15).rounded_l(8))
      .child(div().w(1).h(16).bg(line ?? t.chrome))
      .child(part(ready, colors.green).w(ready > 9 ? 19 : 15).rounded_r(8));
  }
  const n = need > 0 ? need : ready;
  return badge.child(
    part(n, need > 0 ? colors.red : colors.green)
      .min_w(16)
      .px(n > 9 ? 4 : 0)
      .rounded(8),
  );
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
 * A quiet text link: muted, foreground on hover, sized to its words.
 *
 * Shepherdr's `link()` (`lib/ui.js`): the words a row puts beside a value
 * ("Reset", "Use an id typed") rather than a button, so a page of settings is
 * not a page of boxes. `busy` dims it and takes its click, which is what a write
 * in flight shows.
 */
export function link(t, id, word, onClick, busy = false) {
  const base = div()
    .id(id)
    .flex_none()
    .whitespace_nowrap()
    .text_size(TEXT.label)
    .text_color(busy ? t.dim : t.muted);
  const live = busy
    ? base
    : base.cursor_pointer().hover((style) => style.text_color(t.foreground)).on_click(() => onClick());
  return live.child(word);
}

/**
 * A segmented control: a `raised` track, the picked segment in the page's own
 * background and `foreground`, the rest `muted`.
 *
 * Shepherdr's `segmented()` (`lib/ui.js`): accent-free (the accent is for the one
 * primary and a live dot), every segment as wide as its words and never cut.
 * `options` is `[[value, word], ...]`; `value` is what the setting holds now,
 * so a value the choices do not name shows no segment picked rather than the
 * wrong one.
 */
export function segmented(t, id, options, value, onPick, { disabled = false } = {}) {
  return h_flex()
    .h(H.sm)
    .flex_none()
    .p(2)
    .gap(2)
    .rounded(RADIUS.control)
    .bg(t.raised)
    .when(disabled, (el) => el.opacity(0.5))
    .children(
      options.map(([choice, word, own]) => {
        const on = choice === value;
        // A choice with `{ disabled: true }` is drawn dim and does nothing — a
        // view that cannot open now (Shepherdr's `segmented()` takes it too).
        const off = Boolean(own?.disabled);
        const base = h_flex()
          .id(`${id}-${choice}`)
          .h_full()
          .px(8)
          .flex_none()
          .justify_center()
          .items_center()
          .rounded(RADIUS.control - 2)
          .whitespace_nowrap()
          .text_size(TEXT.label)
          .text_color(on ? t.foreground : t.muted);
        const styled = on ? base.bg(t.background).font_medium() : base;
        const live =
          on || disabled
            ? styled
            : off
              ? styled.text_color(t.dim)
            : styled.cursor_pointer().hover((style) => style.text_color(t.foreground)).on_click(() => onPick(choice));
        return live.child(String(word));
      }),
    );
}

/**
 * A switch: 36 × 20, the accent track when on, a 16 px knob at the side it is on.
 *
 * Shepherdr's `toggleSwitch()` (`lib/ui.js`). A switch writes at once, so it is
 * the whole of a setting that is a yes or a no.
 */
export function toggleSwitch(t, id, on, onToggle) {
  return div()
    .id(id)
    .w(36)
    .h(20)
    .flex_none()
    .rounded(10)
    .p(2)
    .flex()
    .bg(on ? t.accent : t.hover)
    .when(on, (el) => el.justify_end())
    .cursor_pointer()
    .on_click(() => onToggle(!on))
    .child(div().w(16).h(16).rounded(8).bg(on ? t.background : t.muted));
}

/**
 * The shared key the settings window records itself under, as `{ id, page, at }`.
 *
 * It is a *shared* value and not the app's `sessionStorage`, because the app
 * gives every mod instance a storage of its own (measured: a key one copy writes
 * is invisible to the next), and what this record has to answer is asked by a
 * different copy every time — the footer's row, a menu, a key.
 */
export const SETTINGS_WINDOW = "shprd.settings.window";

/**
 * How long a window's word that it is open is believed, in ms.
 *
 * A live window renews it on its reads of the service (every ten seconds), so it
 * is never doubted; a copy that went away without saying so — the app was
 * killed, an overlay was closed by something that never told it — is forgotten
 * within twenty seconds instead of shutting the settings out for the app's run.
 */
const ALIVE_MS = 20_000;

/** How stale the record may be before a read of the service renews it. */
const RENEW_MS = 8_000;

/** The settings window that is on screen, as `{ id, page, at }`, or null. */
export function settingsWindow() {
  let kept = null;
  try {
    kept = shared(SETTINGS_WINDOW);
    if (typeof kept === "string") kept = JSON.parse(kept);
  } catch {
    return null;
  }
  if (!kept || typeof kept !== "object" || typeof kept.id !== "string") return null;
  if (!Number.isFinite(kept.at) || Date.now() - kept.at > ALIVE_MS) return null;
  return kept;
}

/**
 * Records that the settings window `id` is on screen, on `page`.
 *
 * The window calls this when it opens and again as it reads the service, and
 * only when what is recorded has gone stale: the value is shared with every mod,
 * so writing it is a repaint of the whole window.
 */
export function settingsWindowOpened(id, page = null) {
  const kept = settingsWindow();
  if (kept !== null && kept.id === id && Date.now() - kept.at < RENEW_MS) return;
  setShared(SETTINGS_WINDOW, { id, page, at: Date.now() });
}

/** Forgets the settings window, when it closes. */
export function settingsWindowClosed(id) {
  const kept = settingsWindow();
  if (kept !== null && id !== undefined && id !== null && kept.id !== id) return;
  setShared(SETTINGS_WINDOW, null);
}

/**
 * The one Settings window (`mods/shprd-settings`), opened over the window.
 *
 * Shepherdr's `openSettings()` (`lib/ui.js`): a dialog most of the window's
 * height, at most 900 px, with `frame: "none"` so the mod draws its own rounded
 * panel inside it. A click beside the panel and `Esc` close it.
 *
 * **One copy.** Herdr opens a second dialog of the same mod when it is asked
 * twice, and two settings windows over one another is not what a click means, so
 * a copy that is already up keeps its place and this asks for nothing (the mod
 * puts it away again in its own `close(cx)`).
 *
 * `p` is `{ page }`, the page it opens on: `agents`, `tasks` or `prompts`.
 */
export function openSettings(p = {}) {
  const open = settingsWindow();
  if (open !== null) return open.id;
  const id = openOverlay({
    mod: "shprd-settings",
    kind: "dialog",
    frame: "none",
    width: 860,
    height: 0.92,
    props: p,
  });
  settingsWindowOpened(id, p.page ?? null);
  return id;
}

/**
 * Opens a task's thread in the Tasks panel from anywhere: the sidebar's task and
 * agent rows, and the panel's own rows.
 *
 * It selects the stacks itself and never calls `shprd-tasks.open`: a stack shows
 * one child at a time and an unselected child's mod is not loaded, so while a
 * thread is open the list's commands do not exist (`unknown command`). The task
 * is shared as `tasks.thread` (`{task, title, view}`), which the thread's head and
 * chat read. `view` is `chat` or `terminal`.
 */
export function openThread(p, view = "chat") {
  setShared("tasks.thread", { task: p.task, title: p.title ?? "", view, at: Date.now() });
  run("layout.select", { stack: "panel-body", child: "tasks" });
  run("layout.select", { stack: "tasks", child: "tasks-thread" });
  run("layout.select", { stack: "tasks-thread-body", child: view === "terminal" ? "tasks-thread-term" : "tasks-thread-chat" });
  run("layout.show", { id: "panel" });
}

/**
 * Opens the New task form from anywhere, with no toast.
 *
 * `shprd-tasks.add` is unknown while the list is not the shown child (a stack
 * shows one child at a time and an unselected child's mod is not loaded), so the
 * request is shared as `tasks.add` (`{at}`) and the list takes it: a list that
 * loads after this call reads it on start, one that is already up reads it on its
 * short timer.
 */
export function openAddTask() {
  setShared("tasks.add", { at: Date.now() });
  run("layout.select", { stack: "panel-body", child: "tasks" });
  run("layout.select", { stack: "tasks", child: "tasks-list" });
  run("layout.show", { id: "panel" });
}

/** The New space dialog: a folder, Browse… and Create (`shprd-newspace`). */
export function openNewSpace() {
  return openOverlay({
    mod: "shprd-newspace",
    kind: "dialog",
    modal: true,
    width: 460,
    height: 252,
  });
}

/**
 * The Accept card, opened as a dialog over the window.
 *
 * Shepherdr's `accept()` (`mods/tasks/main.js`): a 640 × 640 modal dialog with
 * its default frame, which `shprd-accept` fills. It is opened from a Ready row's
 * Review pill, the task head's Review button and a toast about a task that is
 * ready (the toast is shprd's own entry point). `p` is `{ task, title }`; the
 * dialog reads its task from its own props, never from the panel's selection, so
 * opening it leaves the panel underneath as it was.
 */
export function openAccept(p) {
  return openOverlay({
    mod: "shprd-accept",
    kind: "dialog",
    modal: true,
    width: 640,
    height: 640,
    props: { task: p.task, title: p.title ?? "" },
  });
}

/** Where the window's line icons live: one folder, so a mod needs no assets of its own. */
export const ICONS = "mods/shprd-tabs/icons";

/** An icon's file, by name (`settings`, `plus`, `split-right`…). */
export const iconFile = (name) => `${ICONS}/${name}.svg`;

/**
 * The pie's picture for a progress: one file per 5 %, at least 5 (Shepherdr's
 * `lib/dot.js`, `mods/tasks/pie/`). No progress reads half full.
 */
export function pieFile(pct) {
  const steps = Number.isFinite(pct) ? Math.min(100, Math.max(5, Math.round(pct / 5) * 5)) : 50;
  return `${ICONS}/pie/${steps}.svg`;
}

/**
 * The file a glyph name draws, and whether it is a plain line shape: the rings
 * (`ring`, `clock`, `calendar`, `off`) are strokes in the status's colour.
 */
const RINGS = {
  ring: "circle",
  clock: "queued",
  calendar: "scheduled",
  off: "off",
};

/**
 * A line icon, `size` px, in `color` (a mod's own, 14 by default).
 *
 * The SVGs are Lucide-style line art in `currentColor`, so the colour is the
 * text colour a caller passes and a hover only needs to change that.
 */
export function icon(name, size, color) {
  return svg(iconFile(name)).size(size ?? 14).flex_none().text_color(color);
}

/**
 * The status glyph, in a fixed 14 px box so nothing moves when a state changes.
 *
 * Shepherdr's shapes (`lib/dot.js`, ported with its SVGs): an amber pie filling
 * with the progress (`shape.pct`, one picture per 5 %), a red dot with a faint
 * halo for *Needs you*, a green disc with a tick for *Ready* (grey for *Done*), a
 * green ring with a gap, turning, for *Merging*, and plain rings for what waits.
 * `shape` is a row's `{glyph, tone, pct}`, as `lib/spaces.js` and
 * `lib/tabstrip.js` build it, and `colors` is `tone(theme())`.
 */
export function statusGlyph(t, shape, colors, size = 14) {
  const color = !shape || shape.tone === "dim" ? t.dim : (colors?.[shape.tone] ?? t.dim);
  const kind = shape?.glyph ?? "ring";
  const box = div().relative().w(size).h(size).flex_none().flex().items_center().justify_center();
  if (kind === "pie") {
    return svg(pieFile(shape?.pct)).size(size).flex_none().text_color(colors?.amber ?? color);
  }
  if (kind === "dot") {
    const dot = size - 4;
    return box
      .child(div().w(dot).h(dot).flex_none().rounded(Math.floor(dot / 2)).bg(color))
      .child(
        div()
          .absolute()
          .top(-1)
          .left(-1)
          .w(size + 2)
          .h(size + 2)
          .rounded(Math.floor((size + 2) / 2))
          .border(1.5)
          .border_color(`${color}4d`),
      );
  }
  if (kind === "tick") {
    return box
      .rounded(Math.floor(size / 2))
      .bg(color)
      .child(svg(iconFile("tick")).size(size).flex_none().text_color("#ffffff"));
  }
  if (kind === "spin") {
    return svg(iconFile("merging")).size(size).flex_none().text_color(color).spin(1400);
  }
  return svg(iconFile(RINGS[kind] ?? "circle")).size(size).flex_none().text_color(color);
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

/**
 * The 32 px a menu's row takes, which is also what `lib/menu.js` counts with.
 *
 * It is `H.lg`; the constant is here so a row and the menu's height can never
 * disagree about it.
 */
export const MENU_ROW = 32;

/**
 * A popup menu's row, 32 px: an icon's 16 px slot (empty when there is none, so
 * labels line up), the label, a dim hint and, for a choice list, a check at the
 * right.
 *
 * Shepherdr's `menuRow` (`lib/ui.js:114-127`). An icon is an `svg` path inside
 * this mod's own folder or the app's, drawn at 16; a row with nothing to do is
 * dim and takes no click, which is how a menu says *not here* without a tooltip
 * (the window has none).
 */
export function menuRow(
  t,
  id,
  label,
  { icon = null, hint = null, check = null, danger = false, disabled = false, onClick = null } = {},
) {
  const live = !disabled && Boolean(onClick);
  const tone = disabled ? t.dim : danger ? red(t) : t.foreground;
  let el = h_flex()
    .id(id)
    .h(MENU_ROW)
    .w_full()
    .flex_none()
    .px(8)
    .gap(8)
    .items_center()
    .rounded(RADIUS.control)
    .text_size(TEXT.body)
    .text_color(tone)
    .when(live, (style) =>
      style
        .cursor_pointer()
        .hover((s) => s.bg(t.hover))
        .on_click((event, cx) => {
          cx?.stop_propagation?.();
          onClick(event, cx);
          cx?.notify?.();
        }),
    )
    .child(
      h_flex()
        .w(16)
        .h(16)
        .flex_none()
        .items_center()
        .justify_center()
        .when(Boolean(icon), (slot) =>
          slot.child(svg(icon).size(16).text_color(danger ? red(t) : t.muted)),
        ),
    )
    .child(div().flex_1().min_w(0).truncate().child(label))
    .when(Boolean(hint), (slot) =>
      slot.child(div().flex_none().pl(8).text_size(TEXT.label).text_color(t.dim).child(hint)),
    );
  if (check !== null) {
    // A tick drawn as a glyph, not shipped as an SVG: the slot is the same 16 px
    // whether it is checked or not, so a choice list never moves when a pick
    // changes. No menu in this window has one yet; the shape is here because
    // `lib/ui.js`'s row has it.
    el = el.child(
      div()
        .w(16)
        .h(16)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .text_color(t.foreground)
        .when(Boolean(check), (slot) => slot.child("✓")),
    );
  }
  return el;
}

/**
 * A button, sized to its words and never cut: `kind` is `primary` (solid, one per
 * surface), `secondary` (bordered) or `plain` (words only), and `size` is 20
 * inside a row's second line, 24 in a panel, 32 in a dialog.
 *
 * Shepherdr's `button` (`lib/ui.js:45-62`). An icon's side is padded like its
 * top, so it sits square in the corner.
 */
export function button(
  t,
  id,
  label,
  { kind = "secondary", size = "sm", icon = null, disabled = false, onClick = null } = {},
) {
  const lg = size === "lg";
  const xs = size === "xs";
  const height = lg ? H.lg : xs ? H.xs : H.sm;
  const iconSize = lg ? 16 : xs ? 12 : 14;
  const pad = lg ? 12 : xs ? 6 : 8;
  const live = !disabled && Boolean(onClick);
  return h_flex()
    .id(id)
    .h(height)
    .flex_none()
    .gap(6)
    .items_center()
    .rounded(RADIUS.control)
    .whitespace_nowrap()
    .text_size(lg ? TEXT.body : TEXT.label)
    .font_medium()
    .pl(icon ? (height - iconSize) / 2 : pad)
    .pr(pad)
    .when(kind === "primary", (el) =>
      el
        .bg(t.solid)
        .text_color(t.solid_foreground)
        .when(live, (style) => style.hover((s) => s.opacity(0.88))),
    )
    .when(kind === "secondary", (el) =>
      el
        .border_1()
        .border_color(t.input)
        .bg(t.background)
        .text_color(t.foreground)
        .when(live, (style) => style.hover((s) => s.bg(t.hover))),
    )
    .when(kind === "plain", (el) =>
      el
        .text_color(t.muted)
        .when(live, (style) => style.hover((s) => s.bg(t.hover).text_color(t.foreground))),
    )
    .when(disabled, (el) => el.opacity(0.5))
    .when(live, (el) =>
      el.cursor_pointer().on_click((event, cx) => {
        cx?.stop_propagation?.();
        onClick(event, cx);
        cx?.notify?.();
      }),
    )
    .when(Boolean(icon), (el) => el.child(svg(icon).size(iconSize).flex_none()))
    .child(label);
}

/**
 * A dialog's footer, 32 px: a dim note at the left (red when `error`), then a
 * plain Cancel and the one solid primary.
 *
 * Shepherdr's `dialogFooter` (`lib/ui.js:151-158`). There is no "Esc" hint: that
 * Esc closes a dialog goes unsaid in this window.
 */
export function dialogFooter(t, { onCancel, primary = null, cancelLabel = "Cancel", note = "", error = false } = {}) {
  return h_flex()
    .h(H.lg)
    .w_full()
    .flex_none()
    .gap(8)
    .items_center()
    .child(
      div()
        .flex_1()
        .min_w(0)
        .truncate()
        .text_size(TEXT.label)
        .text_color(error ? red(t) : t.dim)
        .child(note || ""),
    )
    .child(button(t, "dialog-cancel", cancelLabel, { kind: "plain", size: "lg", onClick: onCancel }))
    .when(Boolean(primary), (el) =>
      el.child(
        button(t, primary.id ?? "dialog-primary", primary.label, {
          kind: "primary",
          size: "lg",
          disabled: Boolean(primary.disabled),
          onClick: primary.onClick,
        }),
      ),
    );
}
