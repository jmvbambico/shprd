// The Accept card: what a person is approving when a task is ready — its hand-in,
// the files its branch changed, the checks it ran, and the reviewer's findings —
// with the three verbs the engine has for a task in front of a person.
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/accept-card/main.js` (the summary first, then Changes, Checks and
//   a folded Details, then the footer's one primary), `core/accept.js` (the card's
//   words and the summary that folds away), and `mods/chat/main.js`'s `head`
//   branch (the back link into the list, the task's reference and its status line,
//   and the switcher between the task's views)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// What is shprd's own: the changes are read from the task's branch
// (`GET /api/tasks/<id>/changes`), the checks are the ones the hand-in recorded
// rather than a forge's, and the verbs are the engine's — Accept is the approval
// `shprd approve --task` records, Send back is the spawn a failed review's
// send-back runs, Drop is `shprd drop` with the person's reason. Accept is drawn
// only where `tasks.merge` is `ask`; where it is not, the card says so once.
//
// The layout mounts two copies: the head (`props.head`, 127 px) and the card
// itself. Both read the one task the Tasks panel shared (`tasks.thread`), so the
// drill-in is one selection with two views of it, never two selections.

import { View, div } from "gpui-kit";
import { Input, InputState, h_flex, v_flex } from "gpui-base";
import { props, run, setShared, shared, theme } from "herdr";

import { Client, discover, reason } from "./lib/client.js";
import {
  WORDS,
  card,
  changeRows,
  changesLine,
  checks,
  findings,
  head as cardHead,
  reviewLine,
  verbs,
  whereNow,
  words,
} from "./lib/accept.js";
import { card as surface, cardHeader, note } from "./lib/panel.js";
import { H, RADIUS, TEXT, ref, SPACE, tone } from "./lib/tokens.js";
import { followChanges } from "./lib/watch.js";

/** How often the card reads the board when the change file says nothing. */
const FLOOR_MS = 4000;
/** How long a merge policy is trusted before it is asked for again, ms. */
const POLICY_MS = 60_000;

/** Which body stack child each switcher word shows. */
const VIEWS = [
  { id: "tasks-thread-accept", word: "Accept" },
  { id: "tasks-thread-pr", word: "Pull request" },
];

export default class Accept extends View {
  init(_props, cx) {
    this.cx = cx;
    // The layout mounts this mod twice: the head (`props.head`) and the card.
    // A copy's props never change, so this is read once and never again.
    this.headCopy = Boolean(props()?.head);
    this.state = null;
    this.client = null;
    this.failure = null;
    this.watching = false;
    this.policy = null;
    this.policyAt = 0;
    this.changes = null;
    this.changesFor = null;
    this.changesError = null;
    this.details = false;
    this.dropping = false;
    this.busy = null;
    this.error = null;
    this.refresh();
    cx.timer.every(FLOOR_MS, (cx) => this.refresh(cx));
  }

  /** The task this copy is about, from the panel's shared selection. */
  selected() {
    const thread = shared("tasks.thread");
    return card(this.state, thread?.task ?? null);
  }

  /** Reads the board once; a service that went away is looked for again. */
  async refresh(cx = this.cx) {
    try {
      if (!this.client) {
        const found = await discover({ wait: (ms) => cx.sleep(ms) });
        if (!found) {
          this.failure = "shprd serve is not running";
          this.state = null;
          cx.notify();
          return;
        }
        this.client = new Client(found, { wait: (ms) => cx.sleep(ms) });
        this.follow();
      }
      const state = await this.client.state();
      if (state) this.state = state;
      this.failure = null;
      await this.readPolicy();
      await this.readChanges();
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

  /**
   * The merge policy, cached for a minute.
   *
   * It is the one setting this card's footer depends on: `tasks.merge` decides
   * whether an acceptance is a decision at all. A service that cannot answer is
   * not a reason to draw nothing — the footer then shows Accept and the engine
   * refuses it in words.
   */
  async readPolicy() {
    if (!this.client) return;
    const now = Date.now();
    if (this.policy !== null && now - this.policyAt < POLICY_MS) return;
    try {
      const settings = await this.client.settings();
      const row = (settings?.values ?? []).find((entry) => entry.key === "tasks.merge");
      this.policy = typeof row?.value === "string" ? row.value : null;
    } catch {
      this.policy = null;
    }
    this.policyAt = now;
  }

  /**
   * The task's branch, read when the task or its attempt changes.
   *
   * It is a git read at the service, so it is asked for the one task on the card
   * and again when a new attempt starts — not on every wake-up of the change
   * file, which would be a git call every few seconds for numbers that move only
   * when the agent commits.
   */
  async readChanges() {
    const task = this.selected();
    if (!this.client || !task) {
      this.changes = null;
      return;
    }
    const attemptCount = (task.attempts ?? []).length;
    const key = `${task.id}:${attemptCount}`;
    if (key === this.changesFor) return;
    this.changesFor = key;
    this.changesError = null;
    try {
      this.changes = await this.client.taskChanges(task.id);
    } catch (err) {
      this.changes = null;
      this.changesError = reason(err);
    }
  }

  /** Posts one verb and says what the engine answered. */
  async verb(action, extra = {}) {
    const task = this.selected();
    if (!this.client || !task || this.busy) return;
    this.busy = action;
    this.error = null;
    try {
      await this.client.action({ kind: action, task: task.id, ...extra });
      this.dropping = false;
      this.details = false;
      await this.refresh();
    } catch (err) {
      this.error = reason(err);
    }
    this.busy = null;
    this.cx.notify();
  }

  /** Back to the list, and the selection cleared with it. */
  back() {
    setShared("tasks.thread", { task: null, title: "", at: Date.now() });
    run("layout.select", { stack: "tasks", child: "tasks-list" });
    this.cx.notify();
  }

  /** Shows one of the task's views inside this panel. */
  view(id) {
    run("layout.select", { stack: "tasks-thread-body", child: id });
    this.cx.notify();
  }

  render() {
    const t = theme();
    return this.headCopy ? this.head(t) : this.body(t);
  }

  /**
   * The head: back to the list, the task's reference, its title and its status,
   * then the switcher the task's other views hang off.
   */
  head(t) {
    const task = this.selected();
    const shape = cardHead(task);
    const colors = tone(t);
    const toneName = shape.tone === "dim" ? t.dim : (colors[shape.tone] ?? t.dim);
    const back = h_flex()
      .id("accept-back")
      .h(H.xs)
      .flex_none()
      .px(6)
      .gap(4)
      .items_center()
      .rounded(RADIUS.control)
      .text_size(TEXT.label)
      .text_color(t.muted)
      .cursor_pointer()
      .hover((style) => style.bg(t.hover).text_color(t.foreground))
      .on_click(() => this.back())
      .child("‹ Tasks");
    const title = h_flex()
      .min_w(0)
      .items_center()
      .gap(8)
      .child(div().flex_none().font_family("JetBrains Mono").text_size(TEXT.label).text_color(t.dim).child(ref(shape.id ?? "—")))
      .child(div().min_w(0).truncate().text_size(TEXT.body).text_color(t.foreground).child(shape.title))
      .when(shape.id !== null, (el) =>
        el.child(
          h_flex()
            .flex_none()
            .gap(5)
            .items_center()
            .text_size(TEXT.id)
            .text_color(toneName)
            .child(div().w(6).h(6).rounded(3).bg(toneName))
            .child(shape.word),
        ),
      );
    const switcher = h_flex()
      .h(H.sm)
      .flex_none()
      .gap(2)
      .children(VIEWS.map((view) => this.viewTab(t, view)));
    return v_flex()
      .size_full()
      .bg(t.background)
      .border_1()
      .border_color(t.border)
      .rounded(RADIUS.panel)
      .overflow_hidden()
      .child(
        h_flex()
          .h(SPACE.panelHeader)
          .flex_none()
          .px(10)
          .gap(8)
          .items_center()
          .border_b(1)
          .border_color(t.border)
          .child(back)
          .child(title)
          .child(div().flex_1()),
      )
      .child(
        h_flex()
          .flex_1()
          .min_h(0)
          .px(10)
          .gap(6)
          .items_center()
          .child(switcher)
          .child(div().flex_1())
          .when(this.failure !== null, (el) =>
            el.child(div().text_size(TEXT.id).text_color(colors.red).child(this.failure)),
          ),
      );
  }

  /** One switcher word: the view this panel is showing, or one to show. */
  viewTab(t, view) {
    const active = view.id === "tasks-thread-accept";
    return h_flex()
      .id(`accept-tab-${view.id}`)
      .h(H.xs + 4)
      .px(8)
      .items_center()
      .rounded(RADIUS.control)
      .text_size(TEXT.label)
      .text_color(active ? t.foreground : t.muted)
      .when(active, (el) => el.bg(t.raised))
      .when(!active, (el) =>
        el
          .cursor_pointer()
          .hover((style) => style.bg(t.hover).text_color(t.foreground))
          .on_click(() => this.view(view.id)),
      )
      .child(view.word);
  }

  /** The card itself. */
  body(t) {
    const colors = tone(t);
    const task = this.selected();
    const wired = verbs(task, this.policy);
    const said = words(task);
    const review = reviewLine(task);
    const listed = changeRows(this.changes);
    const checked = checks(task);
    const found = findings(task);
    // The question is only a question where there is something to accept: a task
    // that failed, or one that landed, is named by its reference instead (the
    // same distinction Shepherdr's card makes between "Accept X?" and the ref).
    const title = !task
      ? WORDS.title
      : wired.accept
        ? WORDS.question(task.id)
        : task.id;
    const header = cardHeader(
      t,
      h_flex()
        .gap(6)
        .items_center()
        .child(div().text_size(TEXT.body).font_medium().text_color(t.foreground).child(title))
        .when(task && !wired.accept, (el) =>
          el.child(div().text_size(TEXT.id).text_color(t.dim).child(WORDS.notYet)),
        ),
    );
    if (!task) {
      return surface(t, [
        header,
        note(t, this.failure ?? WORDS.reading),
        note(t, "Pick a task in the list; a ready one opens here."),
        div().flex_1(),
      ]);
    }
    const sections = [
      // The summary first, one line for the person: it is what the agent says it
      // did, and it is the top of the card in Shepherdr's too.
      div()
        .px(12)
        .py(8)
        .text_size(TEXT.body)
        .text_color(said.summary === "" ? t.muted : t.foreground)
        .child(said.summary === "" ? WORDS.noSummary : said.summary),
      this.section(t, WORDS.changes, [
        div().text_size(TEXT.label).text_color(t.muted).child(
          this.changesError !== null ? this.changesError : changesLine(this.changes),
        ),
        ...listed.rows.map((row) =>
          h_flex()
            .h(20)
            .gap(8)
            .items_center()
            .child(div().flex_1().min_w(0).truncate().font_family("JetBrains Mono").text_size(TEXT.id).text_color(t.muted).child(row.path))
            .child(div().flex_none().font_family("JetBrains Mono").text_size(TEXT.id).text_color(t.dim).child(row.stat)),
        ),
        ...(listed.rest > 0
          ? [div().text_size(TEXT.id).text_color(t.dim).child(`and ${listed.rest} more`)]
          : []),
      ]),
      this.section(t, WORDS.checks, [
        ...checked.rows.map((row) =>
          h_flex()
            .h(20)
            .gap(8)
            .items_center()
            .child(
              div()
                .flex_none()
                .text_size(TEXT.id)
                .text_color(row.state === "passed" ? colors.green : row.state === "failed" ? colors.red : t.dim)
                .child(row.word),
            )
            .child(div().flex_1().min_w(0).truncate().text_size(TEXT.label).text_color(t.foreground).child(row.command))
            .when(row.detail !== "", (el) => el.child(div().flex_none().text_size(TEXT.id).text_color(t.dim).child(row.detail))),
        ),
        ...(checked.rows.length === 0
          ? [
              div().text_size(TEXT.label).text_color(t.dim).child(WORDS.noChecks),
              ...(checked.alsoRan.length > 0
                ? [div().text_size(TEXT.id).text_color(t.dim).child(`It ran: ${checked.alsoRan.join(", ")}`)]
                : []),
            ]
          : []),
      ]),
      this.section(t, WORDS.review, [
        div()
          .text_size(TEXT.label)
          .text_color(review.degraded ? colors.amber : t.muted)
          .child(review.line),
        ...found.map((finding) =>
          v_flex()
            .gap(2)
            .py(4)
            .child(
              h_flex()
                .gap(6)
                .items_center()
                .child(
                  h_flex()
                    .h(H.xs - 4)
                    .px(5)
                    .items_center()
                    .rounded(RADIUS.control)
                    .bg(finding.tone === "red" ? colors.red : finding.tone === "amber" ? colors.amber : t.raised)
                    .text_size(TEXT.id)
                    .font_medium()
                    .text_color(finding.tone === "dim" ? t.muted : colors.on)
                    .child(finding.word),
                )
                .child(div().min_w(0).flex_1().text_size(TEXT.label).text_color(t.foreground).child(finding.title)),
            )
            .when(finding.location !== null, (el) =>
              el.child(div().font_family("JetBrains Mono").text_size(TEXT.id).text_color(t.dim).child(finding.location)),
            )
            .when(finding.detail !== "", (el) =>
              el.child(div().text_size(TEXT.label).text_color(t.muted).child(finding.detail)),
            ),
        ),
      ]),
      ...(said.details !== "" ? [this.detailsSection(t, said.details)] : []),
    ];
    return surface(t, [
      header,
      v_flex()
        .flex_1()
        .min_h(0)
        .overflow_y_scrollbar()
        .py(2)
        .children(sections),
      this.footer(t, colors, task, wired),
    ]);
  }

  /** One section: a quiet label, then what it says. */
  section(t, label, children) {
    return v_flex()
      .px(12)
      .py(6)
      .gap(4)
      .border_t(1)
      .border_color(t.border)
      .child(div().text_size(TEXT.label).font_medium().text_color(t.dim).child(label))
      .children(children);
  }

  /**
   * The agent's long summary, folded: its header is the toggle.
   *
   * It is Shepherdr's Details, with the same reason: a summary an older build
   * wrote can be a paragraph, and the top of the card is one line.
   */
  detailsSection(t, text) {
    const open = this.details;
    return v_flex()
      .px(12)
      .py(6)
      .gap(4)
      .border_t(1)
      .border_color(t.border)
      .child(
        h_flex()
          .id("accept-details")
          .gap(4)
          .items_center()
          .text_size(TEXT.label)
          .font_medium()
          .text_color(t.dim)
          .cursor_pointer()
          .hover((style) => style.text_color(t.foreground))
          .on_click(() => {
            this.details = !open;
            this.cx.notify();
          })
          .child(open ? "▾" : "▸")
          .child(WORDS.details),
      )
      .when(open, (el) => el.child(div().text_size(TEXT.label).text_color(t.muted).child(text)));
  }

  /**
   * The footer: the one primary, then the two verbs that move a task, and the
   * reason a verb is missing when one is.
   *
   * Accept is the primary and only where `tasks.merge` is `ask`; Send back is
   * drawn on a task whose attempt failed, and Drop always opens a reason field
   * first — the reason is the person's, and it is what the run's pull request
   * lists.
   */
  footer(t, colors, task, wired) {
    const settled = task.status === "integrated" || task.status === "dropped";
    const line = h_flex()
      .flex_1()
      .min_w(0)
      .text_size(TEXT.id)
      .text_color(this.error !== null ? colors.red : settled ? t.muted : t.dim)
      .child(this.error ?? (settled ? whereNow(task) : (wired.note ?? "")));
    if (settled) {
      return h_flex()
        .h(SPACE.panelHeader)
        .flex_none()
        .px(12)
        .gap(8)
        .items_center()
        .border_t(1)
        .border_color(t.border)
        .child(line);
    }
    const buttons = [];
    if (wired.drop) {
      buttons.push(this.button(t, "drop", this.dropping ? WORDS.cancel : WORDS.drop, false, () => {
        this.dropping = !this.dropping;
        if (this.dropping) this.reason = InputState.new({ placeholder: WORDS.reasonPlaceholder });
        this.cx.notify();
      }));
    }
    if (wired.sendBack) {
      buttons.push(
        this.button(t, "sendback", this.busy === "task.retry" ? WORDS.sendingBack : WORDS.sendBack, false, () =>
          this.verb("task.retry"),
        ),
      );
    }
    if (wired.accept) {
      buttons.push(
        this.button(t, "accept", this.busy === "task.accept" ? WORDS.accepting : WORDS.accept, true, () =>
          this.verb("task.accept"),
        ),
      );
    }
    return v_flex()
      .flex_none()
      .border_t(1)
      .border_color(t.border)
      .child(
        h_flex()
          .h(SPACE.panelHeader)
          .flex_none()
          .px(12)
          .gap(8)
          .items_center()
          .child(line)
          .children(buttons),
      )
      .when(this.dropping, (el) =>
        el.child(
          h_flex()
            .px(12)
            .pb(8)
            .gap(8)
            .items_center()
            .child(
              Input.new(this.reason)
                .h(28)
                .flex_1()
                .px(8)
                .rounded(RADIUS.control)
                .border_1()
                .border_color(t.input)
                .bg(t.background)
                .text_size(TEXT.label)
                .text_color(t.foreground),
            )
            .child(
              this.button(t, "dropnow", this.busy === "task.drop" ? WORDS.dropping : WORDS.drop, false, () => {
                const reason = String(this.reason?.value() ?? "").trim();
                if (reason === "") {
                  this.error = "A drop needs a reason: it is what the run's pull request lists.";
                  this.cx.notify();
                  return;
                }
                this.verb("task.drop", { reason });
              }),
            ),
        ),
      );
  }

  /** One button: the footer's primary or a secondary with the verb's own words. */
  button(t, id, label, primary, onClick) {
    return h_flex()
      .id(`accept-${id}`)
      .h(H.sm)
      .flex_none()
      .px(10)
      .items_center()
      .rounded(RADIUS.control)
      .cursor_pointer()
      .text_size(TEXT.label)
      .when(primary, (el) => el.bg(t.solid).text_color(t.solid_foreground).hover((style) => style.opacity(0.9)))
      .when(!primary, (el) =>
        el
          .border_1()
          .border_color(t.input)
          .bg(t.background)
          .text_color(t.foreground)
          .hover((style) => style.bg(t.hover)),
      )
      .on_click(onClick)
      .child(label);
  }
}
