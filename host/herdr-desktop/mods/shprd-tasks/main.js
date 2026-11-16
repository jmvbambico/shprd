// The Tasks panel: the run's board, one row a task, grouped by the word shprd's
// rules give it — with the person's own verbs on each row and a form that adds a
// task.
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/tasks/main.js` (the header, the filter row, the grouped list of
//   two-line rows, the one act at the end of a row, and the drill-in that opens
//   the task in this panel rather than beside it), `lib/dot.js` (the 14 px status
//   glyph)
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
// The verbs are the engine's own: Start is `task.start`, Retry `task.retry`, Stop
// `task.stop` — each on `POST /api/action`, where the same transition rules the
// CLI enforces decide whether it may happen. A refusal is drawn where the person
// pressed, in the service's words, because that is what says why nothing moved.

import { View, div } from "gpui-kit";
import { Input, InputState, h_flex, v_flex } from "gpui-base";
import { nextCommand, run, setShared, theme } from "herdr";

import { attach, reason } from "./lib/client.js";
import {
  counts,
  grouped,
  look,
  newTask,
  rowActions,
  secondLine,
} from "./lib/board.js";
import { badge, card, cardHeader, countBadge, note, statusGlyph } from "./lib/panel.js";
import { H, RADIUS, TEXT, ref, SPACE, tone } from "./lib/tokens.js";
import { followChanges } from "./lib/watch.js";

/** How often the panel reads the board when the change file says nothing. */
const FLOOR_MS = 4000;

/** The stack child a row's click opens: the Accept card, in this panel. */
const CARD = "tasks-thread-accept";

export default class Tasks extends View {
  init(_props, cx) {
    this.cx = cx;
    this.state = null;
    this.failure = null;
    this.client = null;
    this.watching = false;
    // The add-task form: closed until the header's `+` is pressed, then its three
    // fields are state that lives as long as the form is open.
    this.adding = false;
    this.addError = null;
    this.sending = false;
    // The last verb's refusal, kept until the next press or the next read.
    this.actionError = null;
    this.handle = cx.focus_handle();
    this.refresh();
    cx.timer.every(FLOOR_MS, (cx) => this.refresh(cx));
    // The app's own way into a focusable mod: every command is delivered here by
    // name (`run("shprd-tasks.add")`, the palette, a key), and the loop is one
    // `nextCommand()` at a time so a slow read never queues a second press.
    cx.spawn(async (cx) => {
      for (;;) {
        const command = await nextCommand();
        if (command === "add") this.beginAdd(cx);
        else if (command === "focus") {
          this.handle.focus();
          cx.notify();
        }
      }
    });
  }

  /**
   * Reads the board once; a service that restarted or went away is noticed and
   * looked for again, so the panel never keeps painting an old board.
   *
   * Two things make that true and both are in `lib/client.js`: `attach` compares
   * the `service.json` on disk with the id this panel's client was made from (a
   * restarted service writes a new one, even on the same port), and `state()`
   * throws rather than answering `null` when nothing answers at all. A restart
   * clears what this panel cached, and the board is read whole again — which is
   * why a task added while the service was down appears without reloading the
   * layout (bug 5 of the M10 gate).
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
      if (next.restarted) this.forget();
      this.client = next.client;
      this.follow();
      this.state = await this.client.state();
      this.failure = null;
    } catch (err) {
      this.client = null;
      this.failure = reason(err);
    }
    cx.notify();
  }

  /**
   * Drops what was read from the service that just went away.
   *
   * The board itself is replaced by the next read; what is kept is what was said
   * *about* a board — a refusal from a verb, a half-typed task — and neither is
   * true of the new service's run.
   */
  forget() {
    this.state = null;
    this.actionError = null;
    this.addError = null;
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

  /**
   * Opens a task in the panel's drill-in: the Accept card's slot.
   *
   * Which task is shown is shared (`tasks.thread`, Shepherdr's own contract), so
   * the card, the head and the PR view all read the same one rather than each
   * keeping a copy that a reload can lose.
   */
  open(task) {
    setShared("tasks.thread", { task: task?.id ?? null, title: task?.title ?? "", at: Date.now() });
    run("layout.select", { stack: "tasks", child: "tasks-thread" });
    run("layout.select", { stack: "tasks-thread-body", child: CARD });
    run("layout.show", { id: "panel" });
    this.cx.notify();
  }

  /** Opens one of the panel's own stacks: the board, the cards, the settings. */
  panel(child) {
    run("layout.select", { stack: "panel-body", child });
    run("layout.show", { id: "panel" });
    this.shown = child;
    this.cx.notify();
  }

  /**
   * Posts one of the person's verbs and says what happened.
   *
   * A refusal is kept and drawn: a press that moved nothing must not look like
   * one that did. The board is read again after success, so the row shows the
   * status the engine's own pass would give it.
   */
  async verb(task, action) {
    if (!this.client) return;
    this.actionError = null;
    try {
      await this.client.action({ kind: action, task: task.id });
      await this.refresh();
    } catch (err) {
      this.actionError = `${action} ${task.id}: ${reason(err)}`;
    }
    this.cx.notify();
  }

  /** Opens the add-task form with three empty fields. */
  beginAdd(cx = this.cx) {
    this.adding = true;
    this.addError = null;
    this.sending = false;
    this.titleField = InputState.new({ placeholder: "What needs doing" });
    this.scopeField = InputState.new({ placeholder: "host/herdr-desktop, crates/shprd-server" });
    this.acceptanceField = InputState.new({ placeholder: "The board adds a task; a test says so" });
    for (const field of [this.titleField, this.scopeField, this.acceptanceField]) {
      field.on("change", (_e, cx) => cx.notify());
    }
    // A mod has no `setTimeout`: the form's first field takes the keyboard on the
    // app's own timer, exactly as the app's own prompt mod does it.
    cx.timer.after(50, () => this.titleField.focus());
    cx.notify();
  }

  /** Closes the form and forgets its fields. */
  endAdd() {
    this.adding = false;
    this.addError = null;
    this.sending = false;
    this.titleField = null;
    this.scope_ = null;
    this.acceptance_ = null;
    this.cx.notify();
  }

  /** Sends the form to `POST /api/tasks`. */
  async submitAdd() {
    if (!this.client || this.sending) return;
    const body = newTask({
      title: this.titleField?.value(),
      scope: this.scopeField?.value(),
      acceptance: this.acceptanceField?.value(),
    });
    if (!body.ready) {
      this.addError = body.why;
      this.cx.notify();
      return;
    }
    this.sending = true;
    this.addError = null;
    try {
      await this.client.addTask({
        title: body.title,
        scope: body.scope,
        acceptance: body.acceptance,
      });
      await this.refresh();
      this.endAdd();
      return;
    } catch (err) {
      this.addError = reason(err);
    }
    this.sending = false;
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
        el.child(badge(t, countBadge(tally.needs), colors.red, colors.on)),
      )
      .when(tally.ready > 0, (el) =>
        el.child(badge(t, countBadge(tally.ready), colors.green, colors.on)),
      )
      .child(div().flex_1())
      .when(!this.adding, (el) => el.child(this.icon("add", "+", t, () => this.beginAdd())));

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
      .child(this.tab(t, "Asks", "asks", (this.state?.asks ?? []).length > 0));

    const body = v_flex()
      .flex_1()
      .min_h(0)
      .py(6)
      .gap(2)
      .children([
        ...(this.actionError !== null ? [this.wrong(t, this.actionError)] : []),
        ...(this.adding ? [this.form(t, colors)] : []),
        ...(this.failure !== null
          ? [note(t, this.failure), note(t, "Start it from the sidebar's footer, then this fills in.")]
          : groups.length === 0
            ? [note(t, "No tasks yet."), note(t, "Add one with + above; the engine starts it when it may.")]
            : groups.map((group) => this.group(t, colors, group))),
      ]);

    return card(t, [cardHeader(t, head), filter, body]);
  }

  /** A small square button in the header: the `+` that opens the form. */
  icon(id, glyph, t, onClick) {
    return h_flex()
      .id(`tasks-${id}`)
      .h(H.sm)
      .w(H.sm)
      .flex_none()
      .items_center()
      .justify_center()
      .rounded(RADIUS.control)
      .text_size(TEXT.body)
      .text_color(t.muted)
      .cursor_pointer()
      .hover((style) => style.bg(t.hover).text_color(t.foreground))
      .on_click(onClick)
      .child(glyph);
  }

  /** A refusal, in the service's words, where the person pressed. */
  wrong(t, text) {
    return h_flex()
      .mx(12)
      .my(4)
      .px(8)
      .py(6)
      .rounded(RADIUS.card)
      .border_1()
      .border_color(t.border)
      .text_size(TEXT.label)
      .text_color(tone(t).red)
      .child(div().min_w(0).child(text));
  }

  /**
   * The add-task form: a title, the paths it may change, and what must be true
   * when it is done.
   *
   * Three fields rather than Shepherdr's `herdr-task new` dialog, because three
   * are what the rules require and what the service can name a task from. The
   * form says why it cannot be sent yet instead of failing after the press.
   */
  form(t, colors) {
    const body = newTask({
      title: this.titleField?.value(),
      scope: this.scopeField?.value(),
      acceptance: this.acceptanceField?.value(),
    });
    const field = (state, first) => {
      const el = Input.new(state)
        .h(28)
        .px(8)
        .rounded(RADIUS.control)
        .border_1()
        .border_color(first ? t.accent : t.input)
        .bg(t.background)
        .text_size(TEXT.label)
        .text_color(t.foreground);
      return el;
    };
    return v_flex()
      .mx(12)
      .my(4)
      .p(10)
      .gap(6)
      .rounded(RADIUS.card)
      .border_1()
      .border_color(t.border)
      .bg(t.raised)
      .child(div().text_size(TEXT.label).font_medium().text_color(t.foreground).child("New task"))
      .child(field(this.titleField, true))
      .child(field(this.scopeField, false))
      .child(field(this.acceptanceField, false))
      .child(
        h_flex()
          .h(H.xs)
          .items_center()
          .gap(8)
          .child(
            div()
              .flex_1()
              .min_w(0)
              .text_size(TEXT.id)
              .text_color(this.addError !== null ? colors.red : t.dim)
              .child(this.addError ?? body.why ?? "A title, the paths, and what must be true"),
          )
          .child(this.button(t, "cancel", "Cancel", false, () => this.endAdd()))
          .child(
            this.button(
              t,
              this.sending ? "adding" : "add",
              this.sending ? "Adding…" : "Add task",
              true,
              () => this.submitAdd(),
            ),
          ),
      );
  }

  /** One button: the form's and the footer's, primary or plain (Shepherdr's three heights). */
  button(t, id, label, primary, onClick) {
    const el = h_flex()
      .id(`tasks-${id}`)
      .h(H.sm)
      .flex_none()
      .px(10)
      .items_center()
      .rounded(RADIUS.control)
      .cursor_pointer()
      .text_size(TEXT.label)
      .when(primary, (e) => e.bg(t.solid).text_color(t.solid_foreground).hover((s) => s.opacity(0.9)))
      .when(!primary, (e) =>
        e
          .border_1()
          .border_color(t.input)
          .bg(t.background)
          .text_color(t.foreground)
          .hover((s) => s.bg(t.hover)),
      )
      .child(label);
    return el.on_click(onClick);
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
    return v_flex()
      .flex_none()
      .children([
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
    const acts = rowActions(task);
    const left = v_flex()
      .gap(2)
      .child(
        div()
          .text_size(TEXT.body)
          .text_color(t.foreground)
          .truncate()
          .child(task.title ?? task.id),
      )
      .child(
        h_flex()
          .h(16)
          .gap(6)
          .items_center()
          .text_size(TEXT.label)
          .text_color(t.muted)
          .child(div().font_family("JetBrains Mono").text_color(t.dim).child(ref(task.id)))
          .child(div().min_w(0).truncate().child(secondLine(task))),
      );
    // Shepherdr's row: the glyph at the left in a fixed box, then the two lines,
    // then the one act at the end (M7b §2.3).
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
      .child(statusGlyph(t, shape, colors))
      .child(v_flex().flex_1().min_w(0).child(left))
      .when(acts.length > 0, (el) =>
        el.child(this.act(t, task, acts[0])),
      );
  }

  /**
   * The one act a row ends with: a 24 px pill.
   *
   * A press stops at the pill — the row's own click opens the card — and posts
   * the engine's verb. `Review` is not a verb: it opens the card, which is where
   * the decision is made.
   */
  act(t, task, act) {
    const el = h_flex()
      .id(`task-${task.id}-${act.id}`)
      .h(H.xs)
      .flex_none()
      .px(8)
      .items_center()
      .rounded(RADIUS.control)
      .border_1()
      .border_color(t.input)
      .text_size(TEXT.label)
      .text_color(t.foreground)
      .cursor_pointer()
      .hover((style) => style.bg(t.hover))
      .child(act.label);
    return el.on_click((_event, cx) => {
      cx.stop_propagation();
      if (act.action === null) this.open(task);
      else this.verb(task, act.action);
    });
  }
}
