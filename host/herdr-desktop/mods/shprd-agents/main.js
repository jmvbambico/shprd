// The Agents pane: the sidebar's second section, one two-line row per live agent.
//
// PROTOTYPE (SHPR-18). Agents have no tab or space of their own: workers and
// reviewers run headless, and a person reaches one through its task. So this pane
// is a list of what is alive right now — a dot, the task (or the space, for a
// coordinator), its role and program, and under it its state and age — and a click
// opens that task's thread in the Tasks panel.
//
// The rows are `lib/tree.js`'s `agentRows` over `GET /api/state`'s `live_agents`
// (the contract is written there). "Grouped" folds the rows under their space.
//
// Layout: the Spaces mod's own column in `layouts/shprd.json`, below it.

import { View, div } from "gpui-kit";
import { h_flex, v_flex } from "gpui-base";
import { run, setShared, theme } from "herdr";

import { attach, reason } from "./lib/client.js";
import { note, statusGlyph } from "./lib/panel.js";
import { H, RADIUS, TEXT, tone } from "./lib/tokens.js";
import { agentRows } from "./lib/tree.js";
import { followChanges } from "./lib/watch.js";

/** How often the pane reads the board when the change file says nothing. */
const FLOOR_MS = 5000;

export default class Agents extends View {
  init(_props, cx) {
    this.cx = cx;
    this.state = null;
    this.failure = null;
    this.client = null;
    this.watching = false;
    this.grouped = this.load("grouped", false) === true;
    this.picked = null;
    this.refresh();
    cx.timer.every(FLOOR_MS, (cx) => this.refresh(cx));
  }

  /** Reads the board once; a service that went away is looked for again. */
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

  /** One remembered value, or `fallback` when storage is missing. */
  load(key, fallback) {
    try {
      const raw = localStorage.getItem(`agents.${key}`);
      return raw === null ? fallback : JSON.parse(raw);
    } catch {
      return fallback;
    }
  }

  /** Remembers one value; a storage that refuses is not an error. */
  save(key, value) {
    try {
      localStorage.setItem(`agents.${key}`, JSON.stringify(value));
    } catch {
      // The pane works the same, it only forgets on reload.
    }
  }

  /** Flips "Grouped". */
  toggle() {
    this.grouped = !this.grouped;
    this.save("grouped", this.grouped);
    this.cx.notify();
  }

  /**
   * Opens what the row stands for: a worker's or reviewer's task thread in the
   * Tasks panel, a coordinator's space (it has no task). The thread's view is the
   * agent's chat; the Terminal is one click away on the thread's own switch.
   */
  open(row) {
    this.picked = row.id;
    if (row.task) {
      setShared("tasks.thread", { task: row.task, title: row.title, view: "chat", at: Date.now() });
      run("shprd-tasks.open");
    } else if (row.space) {
      setShared("spaces.picked", row.space);
    }
    this.cx.notify();
  }

  render() {
    const t = theme();
    const colors = tone(t);
    const rows = this.state ? agentRows(this.state, { grouped: this.grouped }) : null;
    const body =
      rows === null
        ? [note(t, this.failure ?? "No run yet.")]
        : rows.length === 0
          ? [note(t, "No agents running.")]
          : rows.map((row) => (row.kind === "head" ? this.head(t, row) : this.row(t, colors, row)));
    return v_flex()
      .size_full()
      .min_h(0)
      .bg(t.chrome)
      .text_size(TEXT.body)
      .child(this.title(t, rows === null ? 0 : rows.filter((row) => row.kind === "agent").length))
      .child(v_flex().flex_1().min_h(0).px(8).pb(10).gap(1).overflow_y_scrollbar().mode("always").children(body));
  }

  /** "Agents", how many, and the "Grouped" toggle at the right. */
  title(t, count) {
    return h_flex()
      .h(31)
      .flex_none()
      .pl(16)
      .pr(10)
      .gap(6)
      .items_center()
      .text_size(TEXT.label)
      .font_medium()
      .text_color(t.dim)
      .child("Agents")
      .child(div().text_size(TEXT.id).child(String(count)))
      .child(div().flex_1())
      .child(
        h_flex()
          .id("agents-grouped")
          .h(H.sm)
          .px(6)
          .gap(6)
          .items_center()
          .rounded(RADIUS.control)
          .cursor_pointer()
          .text_color(this.grouped ? t.foreground : t.dim)
          .hover((style) => style.bg(t.hover).text_color(t.foreground))
          .on_click(() => this.toggle())
          .child("Grouped")
          .child(this.switch(t)),
      );
  }

  /** The toggle's track and knob: 22 × 12, the accent when on. */
  switch(t) {
    return h_flex()
      .w(22)
      .h(12)
      .flex_none()
      .px(2)
      .items_center()
      .rounded(6)
      .bg(this.grouped ? t.accent : t.raised)
      .when(this.grouped, (el) => el.justify_end())
      .child(div().w(8).h(8).rounded(4).bg(this.grouped ? t.background : t.dim));
  }

  /** A group's heading: the space's name and how many agents are in it. */
  head(t, row) {
    return h_flex()
      .h(24)
      .flex_none()
      .mt(4)
      .px(8)
      .gap(6)
      .items_center()
      .text_size(TEXT.id)
      .text_color(t.dim)
      .child(row.name)
      .child(String(row.count));
  }

  /**
   * One agent, two lines: the dot, the task (or space), then its role and
   * program at the right; under it the state and how long it has been so.
   */
  row(t, colors, row) {
    const picked = this.picked === row.id;
    return h_flex()
      .id(`agent-${row.id}`)
      .h(44)
      .flex_none()
      .px(8)
      .gap(8)
      .items_center()
      .rounded(RADIUS.card)
      .when(picked, (el) => el.bg(t.selected))
      .when(!picked, (el) => el.hover((style) => style.bg(t.hover)))
      .cursor_pointer()
      .on_click(() => this.open(row))
      .child(statusGlyph(t, row, colors))
      .child(
        v_flex()
          .flex_1()
          .min_w(0)
          .child(
            h_flex()
              .h(22)
              .gap(8)
              .items_center()
              .child(div().flex_1().min_w(0).truncate().text_color(t.foreground).child(row.name))
              .child(
                div()
                  .flex_none()
                  .text_size(TEXT.label)
                  .text_color(t.dim)
                  .child(`${row.role} · ${row.program}`),
              ),
          )
          .child(
            div()
              .h(16)
              .flex()
              .items_center()
              .text_size(TEXT.label)
              .text_color(row.state === "needs_you" ? colors.red : t.dim)
              .child(row.line),
          ),
      );
  }
}
