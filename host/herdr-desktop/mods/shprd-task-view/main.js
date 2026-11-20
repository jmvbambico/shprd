// The task thread's two plain views: the agent's screen as read-only text (what
// the Terminal word shows when the window cannot hold the agent's own terminal)
// and the task's artifacts, the hand-ins and reviews its agents left behind.
//
// Which one is drawn is the block's own prop, `view`: `stream` or `artifacts`;
// the layout mounts the mod twice (`tasks-thread-stream`,
// `tasks-thread-artifacts`). Both follow the one task the Tasks panel shared
// (`tasks.thread`).
//
// The stream is the fallback of M11's rule that Terminal shows the agent's real,
// typeable terminal: it is read-only text from `GET /api/agents/{id}/output`, and
// it says so in its header, so nobody types into a picture of a terminal.

import { View, div } from "gpui-kit";
import { h_flex, v_flex } from "gpui-base";
import { props, shared, theme } from "herdr";

import { attach, downNotice, reason, singleFlight } from "./lib/client.js";
import { taskById } from "./lib/board.js";
import { VIEW_WORDS, artifactSections, streamLines } from "./lib/taskview.js";
import { threadAgent } from "./lib/accept.js";
import { card as surface, cardHeader, note } from "./lib/panel.js";
import { TEXT, tone } from "./lib/tokens.js";
import { followChanges } from "./lib/watch.js";

/** How often the stream re-reads the agent's screen, ms. */
const STREAM_MS = 1500;
/** How often the board is read when the change file says nothing, ms. */
const FLOOR_MS = 4000;

export default class TaskView extends View {
  init(_props, cx) {
    this.cx = cx;
    // A copy's props never change, so which view this is is read once.
    this.kind = props()?.view === "artifacts" ? "artifacts" : "stream";
    this.state = null;
    this.client = null;
    this.failure = null;
    this.watching = false;
    this.screen = null;
    this.screenFor = null;
    this.refresh();
    cx.timer.every(this.kind === "stream" ? STREAM_MS : FLOOR_MS, (cx) => this.refresh(cx));
  }

  /** The task the panel picked, from the one shared selection. */
  selected() {
    return taskById(this.state, shared("tasks.thread")?.task ?? null);
  }

  /** One read at a time; a refresh asked for meanwhile is folded into one more. */
  refresh(cx = this.cx) {
    return singleFlight(this, () => this.readOnce(cx));
  }

  /** One read of the service (and of the screen, for the stream), then a repaint. */
  async readOnce(cx) {
    try {
      const next = await attach(this.client, { wait: (ms) => cx.sleep(ms) });
      if (!next.client) {
        this.client = null;
        this.failure = downNotice();
        this.state = null;
        cx.notify();
        return;
      }
      this.client = next.client;
      this.follow();
      this.state = await this.client.state();
      this.failure = null;
      if (this.kind === "stream") await this.readScreen();
    } catch (err) {
      this.client = null;
      this.failure = reason(err);
    }
    cx.notify();
  }

  /** The agent's recent screen, for the task and agent that are open. */
  async readScreen() {
    const task = this.selected();
    const agent = task ? threadAgent(this.state, task.id) : null;
    if (!agent) {
      this.screen = null;
      this.screenFor = null;
      return;
    }
    try {
      const out = await this.client.agentOutput(agent.id);
      this.screen = streamLines(out?.text);
      this.screenFor = agent.id;
    } catch {
      // A screen that cannot be read now is kept as it was.
    }
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

  render() {
    return this.kind === "artifacts" ? this.artifacts() : this.stream();
  }

  /** The agent's screen, as text, with its header saying it is read-only. */
  stream() {
    const t = theme();
    const task = this.selected();
    const agent = task ? threadAgent(this.state, task.id) : null;
    const header = cardHeader(
      t,
      h_flex()
        .gap(8)
        .items_center()
        .child(div().text_size(TEXT.body).font_medium().text_color(t.foreground).child(VIEW_WORDS.streamTitle))
        .child(div().text_size(TEXT.label).text_color(t.dim).child("read-only")),
    );
    if (!task || !agent) {
      return surface(t, [header, note(t, this.failure ?? VIEW_WORDS.noAgent), div().flex_1()]);
    }
    const lines = this.screenFor === agent.id ? (this.screen ?? []) : [];
    return surface(t, [
      header,
      note(t, VIEW_WORDS.streamFallback),
      v_flex()
        .id("task-stream")
        .flex_1()
        .min_h(0)
        .px(12)
        .py(6)
        .overflow_y_scrollbar()
        .font_family("JetBrains Mono")
        .text_size(TEXT.label)
        .text_color(t.foreground)
        .children(
          lines.length === 0
            ? [div().text_color(t.muted).child(VIEW_WORDS.noOutput)]
            : lines.map((line) => div().whitespace_nowrap().child(line === "" ? " " : line)),
        ),
    ]);
  }

  /** The hand-ins and reviews the task's agents left behind. */
  artifacts() {
    const t = theme();
    const colors = tone(t);
    const task = this.selected();
    const header = cardHeader(
      t,
      h_flex()
        .gap(8)
        .items_center()
        .child(div().text_size(TEXT.body).font_medium().text_color(t.foreground).child(VIEW_WORDS.artifactsTitle))
        .when(task, (el) => el.child(div().text_size(TEXT.label).text_color(t.dim).child(task.id))),
    );
    if (!task) return surface(t, [header, note(t, this.failure ?? "No task is open."), div().flex_1()]);
    const sections = artifactSections(task);
    if (sections.length === 0) return surface(t, [header, note(t, VIEW_WORDS.noArtifacts), div().flex_1()]);
    const paint = { green: colors.green, red: colors.red, dim: t.muted };
    return surface(t, [
      header,
      v_flex()
        .id("task-artifacts")
        .flex_1()
        .min_h(0)
        .p(12)
        .gap(12)
        .overflow_y_scrollbar()
        .children(
          sections.map((section) =>
            v_flex()
              .gap(4)
              .child(div().text_size(TEXT.body).font_medium().text_color(t.foreground).child(section.title))
              .children(
                section.rows.map((row) =>
                  h_flex()
                    .gap(8)
                    .items_start()
                    .child(div().w(96).flex_none().text_size(TEXT.label).text_color(t.dim).child(row.key))
                    .child(
                      div()
                        .flex_1()
                        .min_w(0)
                        .text_size(TEXT.label)
                        .text_color(paint[row.tone] ?? t.muted)
                        .child(row.value),
                    ),
                ),
              ),
          ),
        ),
    ]);
  }
}
