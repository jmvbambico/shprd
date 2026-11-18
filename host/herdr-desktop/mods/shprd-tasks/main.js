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
import { nextCommand, run, setShared, shared, theme } from "herdr";

import { attach, reason, singleFlight } from "./lib/client.js";
import {
  counts,
  etaMinutes,
  filterChips,
  listView,
  look,
  newTask,
  rowActions,
  secondLine,
} from "./lib/board.js";
import { badge, card, cardHeader, countBadge, keycap, note, openAccept, openThread, segmented, statusGlyph } from "./lib/panel.js";
import { graphRows, scopeToSpace } from "./lib/tree.js";
import { H, RADIUS, TEXT, ref, SPACE, tone } from "./lib/tokens.js";
import { followChanges } from "./lib/watch.js";

/** How often the panel reads the board when the change file says nothing. */
const FLOOR_MS = 4000;

/** The font shprd's ids and the graph's guide lines are set in. */
const MONO = "JetBrains Mono";

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
    // The list's own state, remembered across reloads like Shepherdr's (`shut`,
    // `filter`, `alldone`): folded headings, the filter chip, Done opened and
    // Done shown whole. `localStorage` may be missing or throw, so every use is
    // guarded and the panel draws the same without it.
    this.shut = new Set(this.load("shut", []));
    this.filter = this.load("filter", "all");
    this.doneOpen = this.load("doneopen", false) === true;
    this.allDone = this.load("alldone", false) === true;
    this.moreOpen = false;
    this.searching = false;
    // PROTOTYPE (SHPR-18): one panel, two views of the same board.
    this.view = this.load("view", "tasks") === "graph" ? "graph" : "tasks";
    // The search field: "Filter tasks", filters as you type. Esc clears it; F gets
    // to it while the list holds the keyboard (Shepherdr `mods/tasks/main.js`).
    this.query = InputState.new({ placeholder: "Filter tasks" });
    this.query.on("change", (_e, cx) => cx.notify());
    this.query.on("focus", (_e, cx) => {
      this.searching = true;
      cx.notify();
    });
    this.query.on("blur", (_e, cx) => {
      this.searching = false;
      cx.notify();
    });
    this.refresh();
    cx.timer.every(FLOOR_MS, (cx) => this.refresh(cx));
    // New task from anywhere (`openAddTask` in `lib/panel.js`) is a shared request,
    // because this mod's commands do not exist while it is not the shown child.
    this.takeAdd(cx);
    cx.timer.every(300, (cx) => this.takeAdd(cx));
    // The app's own way into a focusable mod: every command is delivered here by
    // name (`run("shprd-tasks.add")`, the palette, a key), and the loop is one
    // `nextCommand()` at a time so a slow read never queues a second press.
    cx.spawn(async (cx) => {
      for (;;) {
        const command = await nextCommand();
        if (command === "add") this.beginAdd(cx);
        else if (command === "open") this.openShared();
        else if (command === "graph") this.show("graph");
        else if (command === "tasks") this.show("tasks");
        else if (command === "focus") {
          this.handle.focus();
          cx.notify();
        } else if (command === "search") this.query.focus();
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
   * Opens a task in the panel's drill-in: its head over its chat.
   *
   * Which task is shown is shared (`tasks.thread`, Shepherdr's own contract), so
   * the card, the head and the PR view all read the same one rather than each
   * keeping a copy that a reload can lose.
   */
  open(task, view = "chat") {
    openThread({ task: task?.id ?? null, title: task?.title ?? "" }, view);
    this.cx.notify();
  }

  /**
   * Opens the task the window already shares as `tasks.thread`.
   *
   * It is how another mod asks for this panel's drill-in — the sidebar's row menu
   * "Open task" — without naming a stack child here, which the thread's own views
   * may move: the shared id is the contract (Shepherdr's, `tasks.thread`), and this
   * panel owns what opening one shows.
   */
  openShared() {
    const thread = shared("tasks.thread");
    if (!thread?.task) return;
    this.open({ id: thread.task, title: thread.title ?? "" }, thread.view ?? "chat");
  }

  /** Switches the panel between its Tasks list and its Graph, remembered. */
  show(view) {
    this.view = view === "graph" ? "graph" : "tasks";
    this.save("view", this.view);
    run("layout.select", { stack: "tasks", child: "tasks-list" });
    this.cx.notify();
  }

  /** Reads one remembered value, or `fallback` when storage is missing or empty. */
  load(key, fallback) {
    try {
      const raw = localStorage.getItem(`tasks.${key}`);
      return raw === null ? fallback : JSON.parse(raw);
    } catch {
      return fallback;
    }
  }

  /** Remembers one value; a storage that refuses is not an error. */
  save(key, value) {
    try {
      localStorage.setItem(`tasks.${key}`, JSON.stringify(value));
    } catch {
      // Nothing to do: the panel works the same, it just forgets on reload.
    }
  }

  /** Folds or unfolds one heading, remembered. */
  fold(key) {
    if (this.shut.has(key)) this.shut.delete(key);
    else this.shut.add(key);
    this.save("shut", [...this.shut]);
    this.cx.notify();
  }

  /** Picks a filter chip (a status word, or `all`), remembered. */
  pick(key) {
    this.filter = key;
    this.moreOpen = false;
    this.save("filter", key);
    this.cx.notify();
  }

  /** Clears the search and, when it was already empty, gives the keyboard back to the list. */
  clearSearch() {
    if ((this.query.value() ?? "") === "") {
      this.handle.focus();
    } else {
      this.query.set_value("");
    }
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

  /** Whether there is more than one space to choose between. */
  hasSpaces() {
    return (this.state?.spaces ?? []).length > 1 || (shared("spaces.picked") ?? null) !== null;
  }

  /** Opens the add-task form for a fresh `tasks.add` request, once. */
  takeAdd(cx = this.cx) {
    const req = shared("tasks.add");
    if (!req || req.at === this.lastAdd) return;
    this.lastAdd = req.at;
    if (Date.now() - req.at < 10_000 && !this.adding) {
      this.show("tasks");
      this.beginAdd(cx);
    }
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
    // The picked space (`spaces.picked`) narrows the list and the Graph; the
    // "All spaces" chip puts every space back.
    const board = scopeToSpace(this.state, shared("spaces.picked") ?? null);
    const tally = counts(board);
    const query = this.query.value() ?? "";
    const chips = filterChips(board);
    // A chip kept for a word nobody is in any more would show an empty list: fall back.
    const known = [...chips.primary, ...chips.more].some((c) => c.key === this.filter);
    const filter = known ? this.filter : "all";
    const view = listView(board, {
      query,
      filter,
      shut: this.shut,
      doneOpen: this.doneOpen,
      allDone: this.allDone,
    });
    const empty = view.groups.length === 0 && view.doneLine === null;
    const head = h_flex()
      .gap(6)
      .items_center()
      .child(
        segmented(
          t,
          "tasks-view",
          [
            ["tasks", "Tasks"],
            ["graph", "Graph"],
          ],
          this.view,
          (choice) => this.show(choice),
        ),
      )
      .when(tally.needs > 0, (el) =>
        el.child(badge(t, countBadge(tally.needs), colors.red, colors.on)),
      )
      .when(tally.ready > 0, (el) =>
        el.child(badge(t, countBadge(tally.ready), colors.green, colors.on)),
      )
      .child(div().flex_1())
      .when(!this.adding, (el) => el.child(this.icon("add", "+", t, () => this.beginAdd())));

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
          : this.state === null || (this.state.tasks ?? []).length === 0
            ? [note(t, "No tasks yet."), note(t, "Add one with + above; the engine starts it when it may.")]
            : empty
              ? [note(t, query.trim() !== "" ? `No tasks match “${query.trim()}”.` : "Nothing here.")]
              : [
                  ...view.groups.map((group) => this.group(t, colors, group, query.trim() !== "")),
                  ...(view.doneLine !== null ? [this.doneLine(t, view.doneLine)] : []),
                ]),
      ]);

    if (this.view === "graph") return this.graph(t, colors, head);
    return card(t, [cardHeader(t, head), this.search(t, query), this.filterRow(t, chips, filter), body])
      .id("tasks-card")
      .track_focus(this.handle)
      .on_key_down((e, cx) => {
        if (e.key === "escape" && (this.searching || query !== "")) this.clearSearch();
        else if ((e.key === "f" || e.key === "/") && !this.searching) this.query.focus();
        else return;
        cx.notify();
      });
  }

  /**
   * The Graph view: the same board as a layered tree, grouped by phase, each task
   * with a right-aligned word (`done`, `working`, `waits SHPR-23`). Drawn here from
   * `graphRows`; nothing is asked of a graph program.
   */
  graph(t, colors, head) {
    const rows = this.state ? graphRows(this.state, shared("spaces.picked") ?? null) : [];
    const body =
      this.failure !== null
        ? [note(t, this.failure)]
        : rows.length === 0
          ? [note(t, "No tasks yet.")]
          : rows.map((row) => (row.kind === "phase" ? this.phase(t, row) : this.node(t, colors, row)));
    return card(t, [
      cardHeader(t, head),
      v_flex().flex_1().min_h(0).px(8).py(6).gap(1).overflow_y_scrollbar().mode("always").children(body),
    ]).id("tasks-card");
  }

  /** A phase's heading: its name in capitals and how many of its tasks are done. */
  phase(t, row) {
    return h_flex()
      .h(30)
      .flex_none()
      .mt(6)
      .px(8)
      .gap(8)
      .items_center()
      .text_size(TEXT.label)
      .font_semibold()
      .text_color(t.foreground)
      .child(div().font_family(MONO).text_color(t.dim).child("▾"))
      .child(row.name)
      .child(div().flex_1())
      .child(div().text_size(TEXT.id).font_normal().text_color(t.dim).child(`${row.done}/${row.total} done`));
  }

  /** One task in the graph: guide lines, glyph, id, title, and its status word at the right. */
  node(t, colors, row) {
    const word =
      row.status === "working"
        ? colors.amber
        : row.status === "needs_you"
          ? colors.red
          : row.status === "ready"
            ? colors.green
            : row.waits || row.status === "done"
              ? t.dim
              : t.muted;
    return h_flex()
      .id(`graph-${row.id}`)
      .h(26)
      .flex_none()
      .px(8)
      .gap(6)
      .items_center()
      .rounded(RADIUS.control)
      .cursor_pointer()
      .hover((style) => style.bg(t.hover))
      .on_click(() => this.open({ id: row.id, title: row.title }))
      .text_size(TEXT.label)
      .child(div().flex_none().font_family(MONO).text_color(t.border).whitespace_nowrap().child(`  ${row.guide}`))
      .child(statusGlyph(t, row, colors, 12))
      .child(div().flex_none().font_family(MONO).text_color(t.foreground).child(ref(row.id)))
      .child(div().flex_1().min_w(0).truncate().text_color(row.waits ? t.dim : t.muted).child(row.title))
      .child(div().flex_none().text_color(word).font_medium().child(row.word));
  }

  /**
   * The search field: a magnifier, the input, an F key cap while it is empty and
   * unfocused, and × Clear once there is something to clear.
   */
  search(t, query) {
    const field = Input.new(this.query)
      .flex_1()
      .min_w(0)
      .h(H.sm)
      .text_size(TEXT.label)
      .text_color(t.foreground);
    return h_flex()
      .id("tasks-search")
      .h(32)
      .flex_none()
      .mx(12)
      .mt(8)
      .px(8)
      .gap(6)
      .items_center()
      .rounded(RADIUS.control)
      .border_1()
      .border_color(this.searching ? t.accent : t.input)
      .child(div().flex_none().text_size(TEXT.body).text_color(t.dim).child("⌕"))
      .child(field)
      .when(query === "" && !this.searching, (el) => el.child(keycap(t, "F")))
      .when(query !== "", (el) =>
        el.child(
          h_flex()
            .id("tasks-search-clear")
            .h(H.xs)
            .flex_none()
            .px(6)
            .items_center()
            .rounded(RADIUS.control)
            .text_size(TEXT.label)
            .text_color(t.muted)
            .cursor_pointer()
            .hover((s) => s.bg(t.hover).text_color(t.foreground))
            .on_click(() => this.clearSearch())
            .child("× Clear"),
        ),
      );
  }

  /**
   * The filter row: "All statuses", the chips that have rows (Needs you, Working,
   * Ready) and "More ⌄" for the rest, each with its count. A chip stays while
   * searching, so a search can be narrowed by status.
   *
   * Shepherdr opens More as a popup mod; this panel opens the rest as a list under
   * the row, since shprd ships no menu mod for it (assumption, brief U2).
   */
  filterRow(t, chips, filter) {
    const pickedMore = chips.more.find((c) => c.key === filter) ?? null;
    return v_flex()
      .flex_none()
      .child(
        h_flex()
          .id("tasks-filter")
          .flex_none()
          .px(6)
          .pt(6)
          .gap(2)
          .overflow_hidden()
          .when(this.hasSpaces(), (el) =>
            el.child(
              this.chip(t, "space", "All spaces", 0, (shared("spaces.picked") ?? null) === null, () => {
                setShared("spaces.picked", null);
                this.cx.notify();
              }),
            ),
          )
          .child(this.chip(t, "all", "All statuses", 0, filter === "all", () => this.pick("all")))
          .children(
            chips.primary.map((c) =>
              this.chip(t, c.key, c.word, c.count, filter === c.key, () => this.pick(c.key)),
            ),
          )
          .when(chips.more.length > 0, (el) =>
            el.child(
              this.chip(
                t,
                "more",
                pickedMore ? pickedMore.word : "More ⌄",
                pickedMore ? pickedMore.count : 0,
                pickedMore !== null || this.moreOpen,
                () => {
                  this.moreOpen = !this.moreOpen;
                  this.cx.notify();
                },
              ),
            ),
          ),
      )
      .when(this.moreOpen, (el) =>
        el.child(
          v_flex()
            .id("tasks-more")
            .mx(12)
            .mt(4)
            .p(4)
            .rounded(RADIUS.card)
            .border_1()
            .border_color(t.border)
            .bg(t.popover)
            .children(
              chips.more.map((c) =>
                h_flex()
                  .id(`tasks-more-${c.key}`)
                  .h(28)
                  .px(8)
                  .gap(6)
                  .items_center()
                  .rounded(RADIUS.control)
                  .text_size(TEXT.label)
                  .text_color(filter === c.key ? t.foreground : t.muted)
                  .cursor_pointer()
                  .hover((s) => s.bg(t.hover))
                  .on_click(() => this.pick(c.key))
                  .child(div().flex_1().child(c.word))
                  .child(div().text_color(t.dim).child(String(c.count))),
              ),
            ),
        ),
      );
  }

  /** One filter chip: its words and how many hold it; the chosen one sits on a soft background. */
  chip(t, key, label, n, on, onClick) {
    return h_flex()
      .id(`tasks-chip-${key}`)
      .h(H.sm)
      .px(8)
      .gap(5)
      .flex_none()
      .items_center()
      .rounded(RADIUS.control)
      .cursor_pointer()
      .text_size(TEXT.label)
      .bg(on ? (t.selected ?? t.raised) : t.background)
      .text_color(on ? t.foreground : t.muted)
      .hover((s) => s.bg(on ? (t.selected ?? t.raised) : t.hover).text_color(t.foreground))
      .on_click(onClick)
      .child(label)
      .when(n > 0, (el) => el.child(div().text_color(t.dim).child(String(n))));
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

  /**
   * One group: a heading that folds it (a chevron, the label, the count), then its
   * rows. Done past the newest few waits behind "Show N more". `open` (searching)
   * shows every row and makes the heading a plain heading.
   */
  group(t, colors, group, open) {
    const rows = group.tasks.map((task) => this.taskRow(t, colors, task));
    const isDone = group.key === "done";
    const onClick = () => {
      // An opened Done goes back to its count line; every other heading folds.
      if (isDone) {
        this.doneOpen = false;
        this.save("doneopen", false);
        this.cx.notify();
      } else this.fold(group.key);
    };
    const folded = group.shut;
    return v_flex()
      .flex_none()
      .gap(1)
      .pt(6)
      .child(
        h_flex()
          .id(`tasks-head-${group.key}`)
          .h(28)
          .flex_none()
          .px(8)
          .gap(8)
          .items_center()
          .rounded(RADIUS.card)
          .text_size(TEXT.label)
          .when(!group.flat && !open, (el) =>
            el.cursor_pointer().hover((s) => s.bg(t.hover)).on_click(onClick),
          )
          .child(
            div()
              .w(14)
              .flex_none()
              .text_color(t.dim)
              .child(group.flat || open ? "" : folded ? "›" : "⌄"),
          )
          .child(
            div()
              .min_w(0)
              .truncate()
              .when(group.top, (el) =>
                el.text_size(TEXT.body).font_semibold().text_color(t.foreground),
              )
              .when(!group.top, (el) => el.font_medium().text_color(t.muted))
              .child(group.label),
          )
          .child(div().flex_none().text_color(t.dim).child(String(group.count))),
      )
      .when(!folded, (el) => el.children(rows))
      .when(!folded && isDone && group.more && !open, (el) =>
        el.child(
          h_flex()
            .id("tasks-done-more")
            .h(28)
            .pl(30)
            .pr(8)
            .items_center()
            .rounded(RADIUS.card)
            .cursor_pointer()
            .text_size(TEXT.label)
            .text_color(t.muted)
            .hover((s) => s.bg(t.hover).text_color(t.foreground))
            .on_click(() => {
              this.allDone = !this.allDone;
              this.save("alldone", this.allDone);
              this.cx.notify();
            })
            .child(this.allDone ? "Show less" : `Show ${group.cut} more`),
        ),
      );
  }

  /** Done, folded: one quiet line, "N done"; a click shows them. */
  doneLine(t, line) {
    return h_flex()
      .id("tasks-done-line")
      .flex_none()
      .mt(6)
      .h(28)
      .px(8)
      .gap(8)
      .items_center()
      .rounded(RADIUS.card)
      .cursor_pointer()
      .text_size(TEXT.label)
      .text_color(t.dim)
      .hover((s) => s.bg(t.hover).text_color(t.muted))
      .on_click(() => {
        this.doneOpen = true;
        this.save("doneopen", true);
        this.cx.notify();
      })
      .child(div().w(14).flex_none().child("›"))
      .child(line.label);
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
      .child(
        v_flex()
          .w(28)
          .flex_none()
          .items_center()
          .gap(1)
          .child(statusGlyph(t, shape, colors))
          .when(etaMinutes(task) !== null, (el) =>
            el.child(
              div()
                .text_size(TEXT.id)
                .text_color(colors.amber)
                .child(`${etaMinutes(task)}m`),
            ),
          ),
      )
      .child(v_flex().flex_1().min_w(0).child(left))
      .when(acts.length > 0, (el) =>
        el.child(this.act(t, task, acts[0])),
      );
  }

  /**
   * The one act a row ends with: a 24 px pill.
   *
   * A press stops at the pill — the row's own click opens the card — and posts
   * the engine's verb. `Review` is not a verb: it opens the Accept dialog, which is
   * where the decision is made.
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
      // Review is the card, as a dialog: the panel underneath stays as it was.
      if (act.action === null) openAccept({ task: task.id, title: task.title });
      else this.verb(task, act.action);
    });
  }
}
