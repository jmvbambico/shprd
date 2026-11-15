// The Tasks panel: the run's board, one row a task, grouped by the word shprd's
// rules give it.
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/tasks/main.js` (the header, the filter row, the grouped list of
//   two-line rows, the `F` search key), `lib/dot.js` (the 14 px status glyph)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// The data path is shprd's: the service folds the ledger and derives each task's
// word (`GET /api/state`), so this panel never decides what a task is doing. Its
// live arms are the change file the service touches after every write
// (`lib/watch.js`: `watchFolder`) and a timer as the floor; both re-read the one
// aggregate document, never one row at a time — a Herdr `fetch` costs ~115 ms
// (M7ef §2.11).
//
// What M10.5 fills: the Accept card a row opens. A row is one click today, and
// what it opens is the card's placeholder in this same bracket.

import { View, div } from "gpui-kit";
import { h_flex, v_flex } from "gpui-base";
import { run, theme } from "herdr";

import { Client, discover, reason } from "./lib/client.js";
import { counts, grouped, look, secondLine } from "./lib/board.js";
import { card, cardHeader, countBadge, keycap, note } from "./lib/panel.js";
import { ref, SPACE, TEXT, tone } from "./lib/tokens.js";
import { followChanges } from "./lib/watch.js";

/** How often the panel reads the board when the change file says nothing. */
const FLOOR_MS = 4000;

export default class Tasks extends View {
  props;

  init(_props, cx) {
    this.cx = cx;
    this.state = null;
    this.failure = null;
    this.client = null;
    this.watching = false;
    this.refresh();
    cx.timer.every(FLOOR_MS, (cx) => this.refresh(cx));
  }

  /** Reads the board once; a service that went away is looked for again. */
  async refresh(cx = this.cx) {
    try {
      if (!this.client) {
        const found = await discover({ wait: (ms) => cx.sleep(ms) });
        if (!found) {
          this.state = null;
          this.failure = "shprd serve is not running";
          cx.notify();
          return;
        }
        this.client = new Client(found, { wait: (ms) => cx.sleep(ms) });
        this.follow();
      }
      const state = await this.client.state();
      if (state) this.state = state;
      this.failure = null;
    } catch (err) {
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

  /** Opens a task in the panel's thread: the Accept card's slot. */
  open(task) {
    run("layout.select", { stack: "tasks", child: "tasks-thread" });
    run("layout.select", { stack: "tasks-thread-body", child: "tasks-thread-accept" });
    // What M10.5 fills: the card reads the task the panel shows. Until then the
    // selection is shared, so the next task's panel already knows what to open.
    run("layout.show", { id: "panel" });
    this.selected = task?.id ?? null;
    this.cx.notify();
  }

  /** Opens one of the panel's own stacks: the board, the cards, the settings. */
  panel(child) {
    run("layout.select", { stack: "panel-body", child });
    run("layout.show", { id: "panel" });
    this.cx.notify();
  }

  render() {
    const t = theme();
    const colors = tone(t);
    const tally = counts(this.state);
    const groups = grouped(this.state);
    const head = h_flex()
      .gap(6)
      .items_center()
      .child(div().text_size(TEXT.body).font_medium().text_color(t.foreground).child("Tasks"))
      .when(tally.needs > 0, (el) =>
        el.child(countBadge(t, countBadge(tally.needs), colors.red, colors.on)),
      )
      .when(tally.ready > 0, (el) =>
        el.child(countBadge(t, countBadge(tally.ready), colors.green, colors.on)),
      )
      .child(div().flex_1())
      .child(keycap(t, "F"));

    const filter = h_flex()
      .h(SPACE.panelHeader)
      .flex_none()
      .px(12)
      .gap(6)
      .items_center()
      .border_b(1)
      .border_color(t.border)
      .text_size(TEXT.label)
      .text_color(t.muted)
      .child(this.tab(t, "Board", "tasks", true))
      .child(this.tab(t, "Asks", "asks", (this.state?.asks ?? []).length > 0))
      .child(this.tab(t, "Settings", "settings", false));

    const body = v_flex().flex_1().min_h(0).py(6).gap(2).children(
      this.failure !== null
        ? [note(t, this.failure), note(t, "Start it from the sidebar's footer, then this fills in.")]
        : groups.length === 0
          ? [note(t, "No tasks yet."), note(t, "The coordinator adds them; this fills in as they arrive.")]
          : groups.map((group) => this.group(t, colors, group)),
    );

    return card(t, [cardHeader(t, head), filter, body]);
  }

  /** One of the three things the panel shows, as a small text button. */
  tab(t, word, child, live) {
    const active = (this.shown ?? "tasks") === child;
    return h_flex()
      .h(24)
      .flex_none()
      .px(8)
      .items_center()
      .rounded(6)
      .text_color(active ? t.foreground : t.muted)
      .when(active, (el) => el.bg(t.raised))
      .when(live && !active, (el) => el.cursor_pointer().hover((s) => s.bg(t.hover)))
      .when(live || active, (el) =>
        el.on_click(() => {
          this.shown = child;
          this.panel(child);
        }),
      )
      .child(word);
  }

  /** One status group: its heading with a count, then its rows. */
  group(t, colors, group) {
    const rows = group.tasks.map((task) => this.taskRow(t, colors, task));
    return v_flex().flex_none().children([
      h_flex()
        .h(28)
        .flex_none()
        .px(12)
        .gap(6)
        .items_center()
        .text_size(TEXT.label)
        .text_color(t.dim)
        .child(group.word)
        .child(String(group.tasks.length)),
      ...rows,
    ]);
  }

  /** One task: a two-line row, its glyph at the left, the act at the end. */
  taskRow(t, colors, task) {
    const shape = look(task);
    const fill = shape.tone === "dim" ? t.dim : colors[shape.tone];
    const glyph = this.glyph(shape.glyph, fill, t);
    const left = v_flex()
      .gap(2)
      .child(
        h_flex()
          .gap(8)
          .items_center()
          .child(
            div()
              .text_size(TEXT.body)
              .text_color(t.foreground)
              .child(task.title ?? task.id),
          ),
      )
      .child(
        h_flex()
          .h(16)
          .gap(6)
          .items_center()
          .text_size(TEXT.label)
          .text_color(t.muted)
          .child(
            div().font_family("JetBrains Mono").text_color(t.dim).child(ref(task.id)),
          )
          .child(secondLine(task)),
      );
    const act =
      task.derived?.status === "ready"
        ? h_flex()
            .h(20)
            .flex_none()
            .px(6)
            .items_center()
            .rounded(6)
            .border_1()
            .border_color(t.input)
            .text_size(TEXT.label)
            .text_color(t.foreground)
            .child("Review")
        : null;
    // Shepherdr's row: the glyph at the left in a fixed box, then the two lines,
    // then the one act at the end (M7b §2.3: "each row = 14 px status glyph,
    // title, second line … and at the end the one act").
    return h_flex()
      .h(SPACE.rowTwo)
      .flex_none()
      .px(12)
      .gap(8)
      .items_center()
      .rounded(8)
      .cursor_pointer()
      .hover((style) => style.bg(t.hover))
      .on_click(() => this.open(task))
      .child(glyph)
      .child(v_flex().flex_1().gap(2).child(left))
      .when(act !== null, (el) => el.child(act));
  }

  /**
   * The status glyph, in a fixed 14 px box so nothing moves when a state changes.
   *
   * It is a drawn shape rather than an SVG asset: the layout ships no icons, and
   * a Herdr mod's `svg()` needs a file beside it. The shapes are Shepherdr's
   * (`lib/dot.js`): a ring for what waits, a filled dot for *Needs you*, a disc
   * for *Ready*, a half-filled ring for *Working*, a paused ring for *Merging*.
   */
  glyph(kind, color, t) {
    const box = div().w(14).h(14).flex_none().flex().items_center().justify_center();
    const inner =
      kind === "dot"
        ? div().w(10).h(10).rounded(5).bg(color)
        : kind === "tick"
          ? div().w(14).h(14).rounded(7).bg(color)
          : kind === "pie"
            ? div().w(14).h(14).rounded(7).border_1().border_color(color)
            : kind === "clock" || kind === "calendar" || kind === "spin"
              ? div().w(14).h(14).rounded(7).border_1().border_color(color)
              : div().w(14).h(14).rounded(7).border_1().border_color(t.dim);
    return box.child(inner);
  }
}
