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
// Two shapes of one mod. The layout mounts the **head** (`props.head`, 127 px:
// ‹ Tasks, the task, the switcher and Review) at the top of the task thread. The
// **card** is not in the layout at all: Review opens it as a 640 × 640 modal
// dialog (`openAccept`, `mods/lib/panel.js`) whose props name the task, and its ×,
// Cancel and Esc close it — Shepherdr's own shape (`mods/accept-card/main.js`).
// The head reads the one task the Tasks panel shared (`tasks.thread`); the dialog
// reads the task it was opened for, so opening it never moves the panel beneath.

import { View, div } from "gpui-kit";
import { Input, InputState, h_flex, v_flex } from "gpui-base";
import { closeOverlay, focused, layout, nextCommand, props, run, setShared, shared, terminals, theme } from "herdr";

import { RepoCache, attach, downNotice, press, reason, singleFlight } from "./lib/client.js";
import { finishReview, startReview } from "./lib/optimistic.js";
import { OVERRIDE_REASON, extraActions as more } from "./lib/board.js";
import {
  WORDS,
  card,
  changeRows,
  changeCounts,
  checks,
  proofs,
  commitRows,
  protectedLine,
  canReview,
  dropReason,
  findings,
  VIEW,
  mustLeavePr,
  reportLine,
  reviewLine,
  terminalChild,
  threadAgent,
  threadViews,
  verbs,
  whereNow,
  words,
} from "./lib/accept.js";
import { button as panelButton, openAccept, segmented, statusGlyph } from "./lib/panel.js";
import { taskLook } from "./lib/spaces.js";
import { H, RADIUS, TEXT, ref, tone } from "./lib/tokens.js";
import { followChanges } from "./lib/watch.js";

/** How often the card reads the board when the change file says nothing. */
const FLOOR_MS = 4000;
/** How old the branch's changes may be before they are read again, ms. */
const CHANGES_MS = 20_000;
/** How long a merge policy is trusted before it is asked for again, ms. */
const POLICY_MS = 60_000;

/** The thread's body stack, whose active child the switcher moves. */
const BODY = "tasks-thread-body";
/** The thread head's left edge, where its three rows line up. */
const EDGE = 12;

export default class Accept extends View {
  init(_props, cx) {
    this.cx = cx;
    // The layout mounts this mod twice: the head (`props.head`) and the card.
    // A copy's props never change, so this is read once and never again.
    this.headCopy = Boolean(props()?.head);
    // The dialog is opened for one task and keeps it; `overlay` is its own id, for
    // closing it. The head has neither: it follows the panel's selection.
    // Opened with no task (`mod.open`, the bare overlay a key command gives), the
    // dialog is about the one the panel has open.
    this.taskId = props()?.task ?? (this.headCopy ? null : (shared("tasks.thread")?.task ?? null));
    this.overlay = props()?.overlay ?? null;
    this.handle = cx.focus_handle();
    if (!this.headCopy) this.handle.focus();
    this.viewing = null;
    this.focusedTerminal = null;
    // The last board read per repository (see `RepoCache`): the head keeps drawing
    // the shared task's row across a switch of space instead of going blank.
    this.cache = new RepoCache();
    this.cache.label = "Accept";
    this.shownKey = this.cache.key();
    this.state = this.cache.get();
    // The dialog's first picture: the task as the Tasks panel held it, drawn until
    // the service has answered.
    this.snapshot = props()?.snapshot ?? null;
    this.title = props()?.title ?? "";
    this.client = null;
    this.failure = null;
    this.watching = false;
    this.policy = null;
    this.policyAt = 0;
    this.changes = null;
    this.changesFor = null;
    this.changesError = null;
    this.details = false;
    // A row's Drop opens the card on its Drop step, with the reason box ready.
    this.dropping = Boolean(props()?.drop);
    // Where the Drop step was entered: "row" (the dialog opened on it) or "footer".
    this.dropFrom = this.dropping ? "row" : null;
    this.reason = null;
    if (this.dropping) this.openDrop(this.dropFrom);
    this.busy = null;
    this.error = null;
    this.refresh();
    cx.timer.every(FLOOR_MS, (cx) => this.refresh(cx));
    // The head answers the thread's commands by name (`run("shprd-accept.view-term")`,
    // the palette, a key): the switcher's four words and Review, for whoever has no
    // pointer to hand. Only the head listens; the dialog has no commands.
    if (this.headCopy) {
      // The shown view is Herdr's (`layout()`), not this mod's state, so a view picked
      // from outside (a row's click, a command) is noticed on a short beat: the head
      // repaints, and its render settles the Terminal view onto the task's agent.
      cx.timer.every(500, (cx) => {
        this.settle();
        cx.notify();
      });
      cx.spawn(async () => {
        for (;;) {
          const command = await nextCommand();
          const task = this.selected();
          const views = { "view-chat": VIEW.chat.id, "view-term": terminalChild(task ? threadAgent(this.state, task.id) : null, terminals()), "view-pr": VIEW.pr.id, "view-artifacts": VIEW.artifacts.id };
          if (command in views && task) this.view(views[command], task);
          else if (command === "review" && task) this.review(task);
        }
      });
    }
  }

  /**
   * The task this copy is about: the panel's shared selection for the head, the
   * task it was opened for for the dialog.
   */
  selected() {
    const id = this.headCopy ? (shared("tasks.thread")?.task ?? null) : this.taskId;
    const key = this.cache?.key();
    if (this.cache) {
      this.shownKey = key;
      // Drawn again every time: an action pressed anywhere changes what the board says.
      this.state = this.cache.get();
    }
    const found = card(this.state, id);
    // Until the first read answers, the dialog draws the task it was opened with.
    if (!found && this.state === null && !this.headCopy && this.snapshot?.id === id) return this.snapshot;
    return found;
  }

  /**
   * Reads the board once; a service that restarted or went away is looked for
   * again, and everything cached about it is dropped.
   *
   * `attach` notices the restart by its `id`, and `state()` refuses to answer
   * `null`, so the card never shows an old board's changes beside a new
   * service's run (M10 gate bug 5).
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
        this.failure = downNotice();
        cx.notify();
        return;
      }
      if (next.restarted) this.forget();
      this.client = next.client;
      this.follow();
      const board = await this.client.state();
      this.cache.set(this.client.folder ?? null, board);
      this.state = this.cache.get();
      this.failure = null;
      // Only the dialog needs the merge policy and the branch's changes: the head
      // would otherwise make a git read at the service for numbers it never draws.
      if (!this.headCopy) {
        await this.readPolicy();
        await this.readChanges();
      }
    } catch (err) {
      this.client = null;
      this.failure = reason(err);
    }
    cx.notify();
  }

  /**
   * Drops what was read from the service that went away: the policy and the
   * task's changes are reads of that run, and a new service has its own.
   */
  forget() {
    this.policy = null;
    this.policyAt = 0;
    this.changes = null;
    this.changesFor = null;
    this.changesError = null;
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
    // An agent report or a status change is the log moving: the branch may have
    // new commits, so the read is made again (and not on every wake-up besides).
    const key = `${task.id}:${attemptCount}:${task.status}:${task.last_status?.seq ?? 0}`;
    // A branch can gain commits with nothing in the log moving, so a read older
    // than CHANGES_MS is made again even under the same key.
    if (key === this.changesFor && Date.now() - (this.changesAt ?? 0) < CHANGES_MS) return;
    this.changesAt = Date.now();
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
    // The button says what it is doing at once, and so does every other pane: the
    // row, the Graph, the status bar and the chat show the result before the
    // service has answered (`press`).
    this.cx.notify();
    const sent = await press(this, {
      kind: action,
      tasks: [task.id],
      send: () => this.client.action({ kind: action, task: task.id, ...extra }),
    });
    this.busy = null;
    if (sent.ignored) {
      this.cx.notify();
      return;
    }
    if (sent.ok) {
      this.dropping = false;
      this.details = false;
      // The decision is made: the dialog goes, as Shepherdr's does after Accept.
      this.close();
      return;
    }
    // Refused: the panes are back as they were and one plain line says why.
    this.error = sent.error;
    this.cx.notify();
  }

  /** Opens the card on a task, its row saying "Reviewing…" meanwhile. */
  review(task) {
    startReview(task, this.client?.folder ?? null);
    openAccept({ task: task.id, title: task.title, snapshot: task });
  }

  /**
   * Opens the Drop step with the reason box focused. Entered from the footer, Back
   * returns to the footer and keeps what was typed; opened on the step, it closes.
   */
  openDrop(from) {
    this.dropping = true;
    this.dropFrom = from;
    this.error = null;
    if (!this.reason) {
      this.reason = InputState.new({ placeholder: WORDS.reasonPlaceholder });
      // Enter in the field arrives as the input's `submit`.
      this.reason.on("submit", () => this.dropNow());
    }
    this.reason.focus();
    this.cx?.timer?.after?.(50, () => this.reason?.focus());
    this.cx?.notify?.();
  }

  /** Back from the Drop step, or closes the dialog where it was opened on the step. Never drops. */
  dropBack() {
    if (this.busy === "task.drop") return;
    if (this.dropFrom === "footer") {
      this.dropping = false;
      this.error = null;
      this.cx?.notify?.();
    } else {
      this.close();
    }
  }

  /** Drops the task: a blank reason sends the default, a typed one is sent trimmed. */
  dropNow() {
    if (!this.dropping) return;
    this.verb("task.drop", { reason: dropReason(this.reason?.value()) });
  }

  /** Closes the dialog: × , Cancel, Esc and a finished verb all end here. */
  close() {
    // A Review's "Reviewing…" ends with the card.
    const id = this.headCopy ? null : this.taskId;
    if (id) finishReview(id, this.client?.folder ?? null);
    closeOverlay(this.overlay);
  }

  /** Back to the list, and the selection cleared with it. */
  back() {
    setShared("tasks.thread", { task: null, title: "", at: Date.now() });
    run("layout.select", { stack: "tasks", child: "tasks-list" });
    this.cx.notify();
  }

  /**
   * Shows one of the task's views inside this panel.
   *
   * The Terminal view is the agent's own terminal: the block is
   * `{"agent":"focused"}`, which shows the agent Herdr last focused, so the task's
   * agent is focused (`agent.focus`) before the block is shown. A task's agents
   * have no tab or space of their own, so there is no space to select instead.
   */
  view(id, task) {
    if (id === VIEW.term.id) this.focusAgent(task);
    run("layout.select", { stack: BODY, child: id });
    this.viewing = id;
    this.cx.notify();
  }

  /**
   * Settles the shown view onto what the task can show, on a short beat (never in
   * `render`, which may only read): the pull request or artifacts of a task that
   * has none go back to the chat; the Terminal view is the agent's own terminal
   * while the window holds it and the read-only stream when it does not; and a
   * shown terminal is focused on the task's agent.
   */
  settle() {
    const task = this.selected();
    if (!task) return;
    const agent = threadAgent(this.state, task.id);
    const termChild = terminalChild(agent, terminals());
    const shown = this.shownChild();
    if (mustLeavePr(task, shown)) this.view(VIEW.chat.id, task);
    else if ((shown === VIEW.term.id || shown === VIEW.stream.id) && shown !== termChild) this.view(termChild, task);
    else if (shown === VIEW.term.id) this.focusAgent(task, false);
  }

  /** Focuses the task's agent in the window, once per terminal, so the block shows it. */
  focusAgent(task, force = true) {
    const agent = task ? threadAgent(this.state, task.id) : null;
    if (!agent?.terminal) return;
    // The agent block shows the agent Herdr last focused, which the person may have
    // changed since this head last asked: ask Herdr, not a cache of this head's own.
    if (!force && focused()?.agent === agent.terminal) return;
    this.focusedTerminal = agent.terminal;
    run("agent.focus", { terminal: agent.terminal });
  }

  render() {
    const t = theme();
    return this.headCopy ? this.head(t) : this.dialog(t);
  }

  /**
   * The head, three rows on one left edge (Shepherdr's `chat/panel.js` head):
   * ‹ Tasks with the one primary Review at the right; the task's glyph, id and
   * title with the agent's last status line under them; then the switcher.
   *
   * There is no "→ base": a shprd task has no base of its own — every task lands
   * in the run's integration branch — so there is nothing that "differs" to say.
   */
  head(t) {
    const task = this.selected();
    const colors = tone(t);
    const agent = task ? threadAgent(this.state, task.id) : null;
    const termChild = terminalChild(agent, terminals());
    const shown = this.shownChild();
    const segs = threadViews(task, { live: agent !== null, shown, termChild });
    const shape = task ? taskLook(task) : { glyph: "ring", tone: "dim" };
    const said = reportLine(task);
    const back = h_flex()
      .id("accept-back")
      .h(24)
      .pl(2)
      .pr(8)
      .gap(2)
      .flex_none()
      .items_center()
      .rounded(RADIUS.control)
      .text_size(TEXT.label)
      .text_color(t.muted)
      .cursor_pointer()
      .hover((style) => style.bg(t.hover).text_color(t.foreground))
      .on_click(() => this.back())
      .child("‹ Tasks");
    const review = canReview(task)
      ? h_flex()
          .id("accept-review")
          .h(H.sm)
          .flex_none()
          .px(10)
          .items_center()
          .rounded(RADIUS.control)
          .cursor_pointer()
          .text_size(TEXT.label)
          .bg(t.solid)
          .text_color(t.solid_foreground)
          .hover((style) => style.opacity(0.9))
          .on_click(() => this.review(task))
          .child("Review")
      : null;
    return v_flex()
      .size_full()
      .pt(8)
      .pb(12)
      .gap(12)
      .bg(t.background)
      .border_1()
      .border_color(t.border)
      .rounded(RADIUS.panel)
      .overflow_hidden()
      .child(
        v_flex()
          .flex_none()
          .gap(8)
          .child(
            h_flex()
              .h(24)
              .flex_none()
              .pl(EDGE - 6)
              .pr(EDGE)
              .gap(4)
              .items_center()
              .child(back)
              .child(div().flex_1())
              .when(this.failure !== null, (el) =>
                el.child(div().text_size(TEXT.id).text_color(colors.red).child(this.failure)),
              )
              .when(review !== null, (el) => el.child(review)),
          )
          .child(
            h_flex()
              .flex_none()
              .px(EDGE)
              .gap(8)
              .items_start()
              .text_color(t.foreground)
              .child(
                h_flex()
                  .w(16)
                  .h(22)
                  .flex_none()
                  .items_center()
                  .justify_center()
                  .child(statusGlyph(t, shape, colors)),
              )
              .child(
                v_flex()
                  .flex_1()
                  .min_w(0)
                  .gap(2)
                  .child(
                    h_flex()
                      .h(22)
                      .min_w(0)
                      .gap(8)
                      .items_center()
                      .when(task !== null, (el) =>
                        el.child(
                          div()
                            .flex_none()
                            .font_family("JetBrains Mono")
                            .text_size(TEXT.label)
                            .text_color(t.dim)
                            .child(ref(task.id)),
                        ),
                      )
                      .child(
                        div()
                          .flex_1()
                          .min_w(0)
                          .truncate()
                          .text_size(TEXT.title)
                          .font_medium()
                          .child(task?.title ?? ""),
                      ),
                  )
                  .child(
                    h_flex()
                      .h(16)
                      .min_w(0)
                      .items_center()
                      .child(
                        div()
                          .flex_1()
                          .min_w(0)
                          .truncate()
                          .text_size(TEXT.label)
                          .text_color(t.muted)
                          .child(said ?? ""),
                      ),
                  ),
              ),
          ),
      )
      .child(
        h_flex()
          .id("accept-views")
          .flex_none()
          .px(EDGE)
          .child(
            segmented(
              t,
              "accept-view",
              segs.map((seg) => [seg.key, seg.word, { disabled: seg.dead }]),
              segs.find((seg) => seg.on)?.key ?? null,
              (key) => {
                const seg = segs.find((entry) => entry.key === key);
                if (seg?.child) this.view(seg.child, task);
              },
            ),
          ),
      );
  }

  /** The stack child the thread's body shows now, from the layout Herdr holds. */
  shownChild() {
    const find = (node) => {
      if (!node) return null;
      if (node.id === BODY) return node;
      for (const child of node.children ?? []) {
        const hit = find(child);
        if (hit) return hit;
      }
      return null;
    };
    return find(layout())?.active ?? this.viewing ?? "tasks-thread-chat";
  }

  /**
   * The card, as a 640 × 640 modal dialog (Shepherdr's `accept-card`): the title
   * ("Accept T1?", or the bare reference once there is nothing to accept) with the
   * task's own title under it and a × at the right, the sections in a scrolling
   * body, and a footer of Cancel and the verbs. Esc closes it like Cancel.
   */
  dialog(t) {
    const colors = tone(t);
    const task = this.selected();
    const wired = verbs(task, this.policy, this.changes);
    const said = words(task);
    const settled = Boolean(task) && (task.status === "integrated" || task.status === "dropped");
    // The question is only a question where there is something to accept: a task
    // that failed, or one that landed, is named by its reference instead (the
    // same distinction Shepherdr's card makes between "Accept X?" and the ref).
    const title = !task ? (this.taskId ? WORDS.question(this.taskId) : WORDS.title) : wired.accept ? WORDS.question(task.id) : task.id;
    const closer = div()
      .id("accept-close")
      .w(H.sm)
      .h(H.sm)
      .flex_none()
      .flex()
      .items_center()
      .justify_center()
      .rounded(RADIUS.control)
      .text_color(t.muted)
      .cursor_pointer()
      .hover((style) => style.bg(t.hover).text_color(t.foreground))
      .on_click((_event, cx) => {
        cx.stop_propagation();
        this.close();
      })
      .child("×");
    const body = !task
      ? div().text_color(t.dim).child(this.failure ?? WORDS.reading)
      : v_flex()
          .w_full()
          .gap(16)
          .pb(4)
          // The summary first, one line for the person: it is what the agent says it
          // did, and it is the top of the card in Shepherdr's too.
          .child(
            div()
              .max_w(560)
              .text_size(TEXT.body)
              .text_color(said.summary === "" ? t.muted : t.foreground)
              .child(said.summary === "" ? WORDS.noSummary : said.summary),
          )
          .child(this.section(t, WORDS.changes, this.changesBody(t, colors)))
          .child(this.section(t, WORDS.checks, this.checksBody(t, colors, task)))
          .when(proofs(task).length > 0, (el) => el.child(this.section(t, WORDS.proof, this.proofBody(t, task))))
          .child(this.section(t, WORDS.review, this.reviewBody(t, colors, task)))
          .when(said.details !== "", (el) => el.child(this.detailsSection(t, said.details)));
    return v_flex()
      .id("accept-card")
      .track_focus(this.handle)
      .on_key_down((event, cx) => {
        if (this.dropping && event?.key === "enter" && (event.modifiers?.platform || event.modifiers?.control)) {
          cx.stop_propagation();
          this.dropNow();
          return;
        }
        if (event?.key !== "escape") return;
        cx.stop_propagation();
        if (this.dropping) this.dropBack();
        else this.close();
      })
      .size_full()
      .min_h(0)
      .p(22)
      .gap(18)
      .bg(t.popover ?? t.background)
      .text_size(TEXT.body)
      .child(
        h_flex()
          .flex_none()
          .gap(12)
          .items_start()
          .child(
            v_flex()
              .flex_1()
              .min_w(0)
              .gap(4)
              .child(div().text_size(TEXT.title).font_semibold().text_color(t.foreground).child(title))
              .child(div().min_w(0).truncate().text_color(t.muted).child(task?.title ?? this.title)),
          )
          .child(div().mt(-2).mr(-6).child(closer)),
      )
      .child(div().id("accept-body").flex_1().min_h(0).overflow_y_scrollbar().pr(6).child(body))
      .child(this.footer(t, colors, task, wired, settled));
  }

  /**
   * Changes: one line of counts, the newest commits, the biggest files by name.
   * "3 commits · 4 files changed +12 −4", then "And 2 more" for what was folded.
   */
  changesBody(t, colors) {
    if (this.changesError !== null) {
      return div().text_color(t.muted).child(this.changesError);
    }
    const counts = changeCounts(this.changes);
    if (counts === null) return div().text_color(t.muted).child(WORDS.noChanges);
    const commits = commitRows(this.changes);
    const listed = changeRows(this.changes);
    const flagged = protectedLine(this.changes);
    const mono = (el) => el.font_family("JetBrains Mono");
    return v_flex()
      .gap(8)
      .min_w(0)
      .when(flagged !== null, (el) => el.child(div().text_color(colors.amber).child(flagged)))
      .child(
        h_flex()
          .gap(8)
          .items_center()
          .flex_wrap()
          .when(counts.commits !== null, (el) =>
            el
              .child(div().text_color(t.foreground).child(counts.commits))
              .child(div().text_color(t.dim).child("·")),
          )
          .child(div().text_color(t.foreground).child(counts.files))
          .child(mono(div()).text_size(TEXT.label).text_color(colors.green).child(counts.added))
          .child(mono(div()).text_size(TEXT.label).text_color(colors.red).child(counts.removed)),
      )
      .when(commits.rows.length > 0, (el) =>
        el.child(
          v_flex()
            .gap(3)
            .min_w(0)
            .children(
              commits.rows.map((row) =>
                h_flex()
                  .gap(8)
                  .min_w(0)
                  .items_center()
                  .child(mono(div()).flex_none().text_size(12).text_color(t.dim).child(row.sha))
                  .child(div().flex_1().min_w(0).truncate().text_color(t.foreground).child(row.subject)),
              ),
            )
            .when(commits.rest > 0, (more) =>
              more.child(div().text_size(12).text_color(t.dim).child(WORDS.moreLine(commits.rest))),
            ),
        ),
      )
      .child(
        v_flex()
          .gap(2)
          .min_w(0)
          .children(
            listed.rows.map((row) =>
              h_flex()
                .gap(8)
                .items_center()
                .child(mono(div()).flex_1().min_w(0).truncate().text_size(12).text_color(t.muted).child(row.path))
                .child(mono(div()).flex_none().text_size(TEXT.id).text_color(t.dim).child(row.stat)),
            ),
          )
          .when(listed.rest > 0, (more) =>
            more.child(div().text_size(12).text_color(t.dim).child(WORDS.moreLine(listed.rest))),
          ),
      );
  }

  /** Checks: the gates the hand-in ran, with the state each ended in. */
  checksBody(t, colors, task) {
    const checked = checks(task);
    return v_flex()
      .gap(4)
      .children([
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
      ]);
  }

  /** Proof: the files the hand-in named as its proof. */
  proofBody(t, task) {
    return v_flex()
      .gap(4)
      .children(
        proofs(task).map((row) =>
          h_flex()
            .h(20)
            .gap(8)
            .items_center()
            .child(div().flex_none().text_size(TEXT.label).text_color(t.foreground).child(row.name))
            .child(div().flex_1().min_w(0).truncate().text_size(TEXT.id).text_color(t.dim).child(row.path)),
        ),
      );
  }

  /** Review: who reviewed it and what they found — shprd's own section. */
  reviewBody(t, colors, task) {
    const review = reviewLine(task);
    const found = findings(task);
    return v_flex()
      .gap(4)
      .children([
        div().text_size(TEXT.label).text_color(review.degraded ? colors.amber : t.muted).child(review.line),
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
      ]);
  }

  /** One section: a hairline above, a quiet label, then what it says. */
  section(t, label, content) {
    return v_flex()
      .w_full()
      .gap(6)
      .pt(14)
      .border_t(1)
      .border_color(t.border)
      .child(div().text_size(TEXT.label).font_medium().text_color(t.muted).child(label))
      .child(content);
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
      .w_full()
      .gap(8)
      .pt(14)
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
   * The footer: a dim note at the left, plain Cancel, then the verbs — Drop task
   * and Send back as secondaries and Accept the one primary (Shepherdr's dialog
   * footer is Cancel and the primary; the two extra verbs are the engine's).
   *
   * Accept is the primary and only where `tasks.merge` is `ask`; Send back is
   * drawn on a task whose attempt failed, and Drop task opens a reason field
   * first — the reason is the person's, and it is what the run's pull request
   * lists. A task that is settled has nothing to decide: its footer is the line
   * with a dot saying where it is, and **Close** alone.
   */
  footer(t, colors, task, wired, settled) {
    const noteColor = this.error !== null ? colors.red : t.dim;
    if (!task || settled) {
      return v_flex()
        .flex_none()
        .gap(10)
        .when(Boolean(task), (el) =>
          el.child(
            h_flex()
              .gap(8)
              .items_center()
              .child(div().w(7).h(7).flex_none().rounded(4).bg(task.status === "integrated" ? colors.green : t.muted))
              .child(div().text_color(t.foreground).child(whereNow(task))),
          ),
        )
        .child(
          h_flex()
            .h(H.lg)
            .gap(8)
            .items_center()
            .child(div().flex_1())
            .child(this.button(t, "cancel", WORDS.close, false, () => this.close())),
        );
    }
    if (this.dropping && wired.drop) return this.dropStep(t, colors, task);
    const buttons = [this.button(t, "cancel", WORDS.cancel, false, () => this.close())];
    if (wired.drop) {
      buttons.push(
        this.button(t, "drop", WORDS.dropOpen, false, () => this.openDrop("footer")),
      );
    }
    // Reopen and Override where they apply; a refusal shows on the card as the other verbs' do.
    for (const act of more(task)) {
      if (act.action === "run.sync") continue;
      buttons.push(
        this.button(t, act.id, this.busy === act.action ? `${act.label}…` : act.label, false, () =>
          this.verb(act.action, act.action === "task.override" ? { reason: OVERRIDE_REASON } : {}),
        ),
      );
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
        this.button(t, "accept", this.busy === "task.accept" ? WORDS.accepting : WORDS.accept, true, () => {
          // Landing would refuse: the button is shown but does nothing; the note says why.
          if (wired.acceptBlocked) return;
          this.verb("task.accept");
        }, Boolean(wired.acceptBlocked)),
      );
    }
    return v_flex()
      .flex_none()
      .gap(10)
      .child(
        h_flex()
          .h(H.lg)
          .gap(8)
          .items_center()
          .child(
            div()
              .flex_1()
              .min_w(0)
              .truncate()
              .text_size(TEXT.label)
              .text_color(noteColor)
              .child(this.error ?? wired.note ?? ""),
          )
          .children(buttons),
      );
  }

  /**
   * The Drop step, drawn in place of the footer: the question, what dropping does,
   * the optional reason, and Back (or Cancel) with the one red Drop task.
   */
  dropStep(t, colors, task) {
    const busy = this.busy === "task.drop";
    return v_flex()
      .flex_none()
      .gap(8)
      .pt(14)
      .border_t(1)
      .border_color(t.border)
      .child(div().text_size(TEXT.body).font_medium().text_color(t.foreground).child(WORDS.dropQuestion(task.id)))
      .child(div().text_size(TEXT.label).text_color(t.muted).child(WORDS.dropNote))
      .child(
        Input.new(this.reason)
          .h(28)
          .w_full()
          .px(8)
          .rounded(RADIUS.control)
          .border_1()
          .border_color(t.input)
          .bg(t.background)
          .text_size(TEXT.label)
          .text_color(t.foreground),
      )
      .child(
        h_flex()
          .h(H.lg)
          .gap(8)
          .items_center()
          .child(
            div()
              .flex_1()
              .min_w(0)
              .truncate()
              .text_size(TEXT.label)
              .text_color(colors.red)
              .child(this.error ?? ""),
          )
          .child(this.button(t, "dropback", this.dropFrom === "footer" ? WORDS.back : WORDS.cancel, false, () => this.dropBack()))
          .child(
            panelButton(t, "accept-dropnow", busy ? WORDS.dropping : WORDS.drop, {
              kind: "danger",
              size: "lg",
              disabled: this.busy !== null,
              onClick: () => this.dropNow(),
            }),
          ),
      );
  }

  /** One button: the footer's primary or a secondary with the verb's own words. */
  button(t, id, label, primary, onClick, disabled = false) {
    return h_flex()
      .id(`accept-${id}`)
      .h(H.lg)
      .flex_none()
      .px(12)
      .items_center()
      .rounded(RADIUS.control)
      .text_size(TEXT.label)
      .when(!disabled, (el) => el.cursor_pointer())
      .when(primary, (el) => el.bg(t.solid).text_color(t.solid_foreground).when(!disabled, (b) => b.hover((style) => style.opacity(0.9))))
      .when(!primary, (el) =>
        el
          .border_1()
          .border_color(t.input)
          .bg(t.background)
          .text_color(t.foreground)
          .hover((style) => style.bg(t.hover)),
      )
      // A verb is waiting on the service: its buttons are dimmed (and a second press
      // is ignored in `verb`), the one pressed says what it is doing.
      .when((this.busy !== null && id !== "cancel" && id !== "dropback") || disabled, (el) => el.opacity(0.6))
      // A blocked button is not wired at all: no click, no pointer cursor, no hover.
      .when(disabled, (el) => el.cursor_default())
      .when(!disabled, (el) => el.on_click(onClick))
      .child(label);
  }
}
