// The run's graph: dagr's document, drawn in a terminal block.
//
// Shepherdr has no graph of its own. dagr is the maintainer's own tool and the
// plan puts it here as a terminal block refreshed on every ledger write (M10.7),
// so this mod is the block's *note* half and the terminal is the other half:
//
//   graph (stack, active: graph-note)
//     graph-note  this mod: what dagr is, where it came from, or what to fix
//     graph-term  { "terminal": "shprd-graph" }: `dagr view <repo>/.dagr/run.json`
//
// Why a stack and a note rather than a bare terminal block: a Herdr mod cannot
// draw a terminal (only a layout node or an overlay can), so the terminal is a
// layout node — and a layout terminal block resolves its terminal **once**, by
// name, when it first renders. Two things keep that binding right. The service
// starts the terminal before the window loads the layout, which is what stops
// Herdr pruning the block (see below); and this note is the fallback: it finds
// out where dagr is, starts the terminal if the service did not, waits for it,
// and only then selects the terminal's child, which binds to it. The terminal
// runs a small shell loop, so a dagr that exits is restarted in the same
// terminal: herdr-core keeps a terminal's address for its lifetime, and the
// block is bound to that address.
//
// One thing this cannot do: put the node back if Herdr pruned it. Herdr removes a
// layout terminal block whose terminal it cannot find when the layout loads and
// writes the shortened file back (measured), and a mod may not edit a layout's
// structure on this build (`layout.set` from a mod changes nothing, from
// `herdr-app do` it does). The note says so when it sees it, with the command
// that puts the block back.
//
// Refresh: `dagr view` watches the file itself (dagr's follow mode), and the
// service rewrites `<repo>/.dagr/run.json` atomically after every ledger write
// (`shprd-server`'s after-write step), so the drawing moves without this mod
// doing anything per write. The note re-reads the service on the change file and
// on a timer, so a service that starts (or a setting that changes) after the
// window did is picked up.
//
// With no dagr anywhere the note is what a person sees, and its message names
// `graph.dagr` — the setting that fixes it — because "no graph" without a key to
// type is a dead end. The same note explains any state in which the block cannot
// be shown.
//
// Portable pieces: Shepherdr's panel parts (a card and its header) and the
// client/watch modules shprd already shares. The tokened long poll and the
// change file are shprd's own (`mods/lib/client.js`, `mods/lib/watch.js`).

import { View, div } from "gpui-kit";
import { h_flex, v_flex } from "gpui-base";
import { hc, layout, run, terminals, theme } from "herdr";

import { Client, discover, reason } from "./lib/client.js";
import { card, cardHeader } from "./lib/panel.js";
import { H, RADIUS, SPACE, TEXT } from "./lib/tokens.js";
import { followChanges } from "./lib/watch.js";
import {
  GRAPH_STACK,
  GRAPH_TERM,
  GRAPH_TERMINAL,
  finishedTerminals,
  graphTerminal,
  graphView,
  hasNode,
  startArgv,
} from "./view.js";

/** How often the note looks again even when nothing wakes it. */
const FLOOR_MS = 4000;

/** How long the note waits for a freshly started terminal to appear, ms. */
const START_WAIT_MS = 4000;

/** How often it looks for that terminal while it waits, ms. */
const START_POLL_MS = 200;

export default class Graph extends View {
  init(_props, cx) {
    this.cx = cx;
    this.client = null;
    this.service = null;
    this.graph = null;
    this.failure = null;
    this.starting = false;
    /** The terminal start this copy asked for, so it is asked for once. */
    this.startedFor = null;
    /** Whether this copy has already forgotten the block's dead terminals. */
    this.cleaned = false;
    this.watching = false;
    this.refresh();
    cx.timer.every(FLOOR_MS, (cx) => this.refresh(cx));
  }

  /** Reads the service's answer about the graph, then shows what it says. */
  async refresh(cx = this.cx) {
    try {
      if (!this.client) {
        const found = await discover({ wait: (ms) => cx.sleep(ms) });
        if (!found) {
          this.service = null;
          this.graph = null;
          this.failure = "shprd serve is not running";
          this.show(cx);
          return;
        }
        this.client = new Client(found, { wait: (ms) => cx.sleep(ms) });
        this.follow();
      }
      this.service = this.client.record;
      await this.forgetDead();
      const graph = await this.client.graph();
      if (graph) this.graph = graph;
      this.failure = null;
    } catch (err) {
      // A restarted service answers with another id: drop the client so the next
      // look finds the new port instead of dialling a dead one.
      this.client = null;
      this.service = null;
      this.graph = null;
      this.failure = reason(err);
    }
    this.show(cx);
  }

  /**
   * Forgets finished terminals that carry the block's name.
   *
   * Herdr keeps a finished terminal in the core's list and resolves a layout
   * terminal block's name to the first match, so a dead `shprd-graph` shadows the
   * live one (measured). `hc rm` forgets it; the block's name then means the
   * terminal this copy started.
   */
  async forgetDead() {
    if (this.cleaned) return;
    this.cleaned = true;
    for (const address of finishedTerminals(terminals())) {
      await hc(["rm", address]).catch(() => null);
    }
  }

  /** Follows the change file, so the note hears a write without polling for it. */
  follow() {
    if (this.watching || !this.client) return;
    this.watching = true;
    const cx = this.cx;
    const dir = this.client.dir;
    cx.spawn(async () => {
      await followChanges(dir, () => this.refresh(cx));
    });
  }

  /**
   * Hands the block over to the terminal once there is a dagr to run.
   *
   * Everything before that is the note's own rendering; nothing here changes the
   * layout. The select is what makes the terminal's child render for the first
   * time, and a layout terminal block binds on its first render — so the
   * terminal has to exist before the select, which is why the start is awaited
   * and polled for.
   */
  async show(cx = this.cx) {
    const seen = this.view();
    if (seen.kind !== "ready") {
      cx.notify();
      return;
    }
    if (!this.starting) {
      this.starting = true;
      const found = graphTerminal(terminals(), this.graph?.run_json);
      if (!found.live) {
        await this.start(seen, cx);
      } else {
        this.select();
      }
      this.starting = false;
    }
    cx.notify();
  }

  /**
   * Starts the block's terminal, then selects it.
   *
   * `hc run --bg` starts a herdr-core terminal and returns at once, exactly as
   * the footer starts the service: the terminal outlives the click and the
   * window. It is started once per run and per copy, so a failing `hc` cannot
   * turn the note into a loop of attempts.
   */
  async start(seen, cx = this.cx) {
    const key = `${this.graph?.run_json ?? ""}|${this.graph?.dagr?.path ?? ""}`;
    if (this.startedFor === key) {
      // Asked for already: keep watching for the terminal rather than starting
      // another, and show what happened the second time.
      const found = await this.awaitTerminal(cx);
      if (found.live) this.select();
      return;
    }
    const argv = startArgv(this.graph);
    if (!argv) {
      this.failure = `no program to run: ${seen.message}`;
      return;
    }
    // The name is about to be taken by this copy's terminal: a finished one that
    // still answers to it would shadow the new one in the block.
    for (const address of finishedTerminals(terminals())) {
      await hc(["rm", address]).catch(() => null);
    }
    this.startedFor = key;
    try {
      const answer = await hc(argv);
      if (answer?.code !== 0) {
        this.failure = firstLine(answer?.stderr) || `hc run answered ${answer?.code}`;
        this.startedFor = null;
        return;
      }
    } catch (err) {
      // `hc` rejects when it cannot start a terminal at all: no `hc`, no core.
      this.failure = reason(err);
      this.startedFor = null;
      return;
    }
    const found = await this.awaitTerminal(cx);
    if (found.live) {
      this.select();
    } else {
      this.failure = `\`${GRAPH_TERMINAL}\` did not start; the block will show the graph once it does`;
    }
  }

  /**
   * Waits for the block's terminal to appear, a poll at a time.
   *
   * `terminals()` is Herdr's live list, and a terminal `hc run` has just started
   * may take a moment to appear in it. A terminal that is already there for
   * another repository ends the wait too: there is nothing to start, and the
   * message about it is the next thing to say.
   */
  async awaitTerminal(cx = this.cx) {
    let waited = 0;
    for (;;) {
      const found = graphTerminal(terminals(), this.graph?.run_json);
      if (found.live || found.other) return found;
      if (waited >= START_WAIT_MS) return found;
      await cx.sleep(START_POLL_MS);
      waited += START_POLL_MS;
    }
  }

  /**
   * Shows the terminal half of the stack.
   *
   * The node is in `layouts/shprd.json` and this only selects it: a mod may not
   * edit a layout's structure on this build (`layout.set` from a mod changes
   * nothing — from `herdr-app do` it does), so the block is installed and never
   * written by this file. The service starts the terminal before the window loads
   * the layout, which is what keeps Herdr from pruning the block; the check here
   * is for the case it did anyway, where the note's message is the thing to read.
   */
  select() {
    if (!hasNode(layout(), GRAPH_TERM)) return;
    run("layout.select", { stack: GRAPH_STACK, child: GRAPH_TERM });
  }

  /** The words this copy shows now. */
  view() {
    const found = graphTerminal(terminals(), this.graph?.run_json);
    return graphView({
      graph: this.graph,
      service: this.service,
      failure: this.failure,
      other: found.other,
      blockGone: this.failure === null && !hasNode(layout(), GRAPH_TERM),
    });
  }

  render() {
    const t = theme();
    const seen = this.view();
    const tone =
      seen.kind === "stop" ? t.blocked : seen.kind === "ready" ? t.done : t.working;
    return card(t, [
      cardHeader(t, "Graph", this.chip(t, seen)),
      v_flex()
        .flex_1()
        .px(14)
        .py(12)
        .gap(6)
        .child(
          h_flex()
            .gap(8)
            .items_center()
            .child(div().w(6).h(6).flex_none().rounded(3).bg(tone))
            .child(div().text_size(TEXT.body).text_color(t.foreground).child(seen.title)),
        )
        .child(div().text_size(TEXT.label).text_color(t.muted).child(seen.message))
        .child(div().flex_1())
        .child(this.foot(t, seen)),
    ]);
  }

  /** The header's right-hand word: which probe named dagr, or the setting. */
  chip(t, seen) {
    return h_flex()
      .id("graph-origin")
      .h(H.xs)
      .px(6)
      .flex_none()
      .items_center()
      .rounded(RADIUS.control)
      .text_size(TEXT.id)
      .text_color(t.dim)
      .child(seen.origin);
  }

  /**
   * The note's last line: the one thing a person can act on.
   *
   * It is the setting to type when dagr is missing, the service to start when
   * the service is down, and the command the block runs once it runs — drawn in
   * the app's small dim size and cut at the end, because a path can be wider
   * than the block and the start of it is the part that says what it is.
   */
  foot(t, seen) {
    const hint = seen.hint;
    return h_flex()
      .h(SPACE.row)
      .flex_none()
      .gap(8)
      .items_center()
      .border_t(1)
      .border_color(t.border)
      .child(
        div()
          .flex_1()
          .min_w(0)
          .truncate()
          .text_size(TEXT.id)
          .text_color(t.dim)
          .child(hint),
      )
      .when(seen.kind === "stop", (el) =>
        el.child(
          div()
            .id("graph-retry")
            .h(H.xs)
            .px(8)
            .flex_none()
            .flex()
            .items_center()
            .rounded(RADIUS.control)
            .text_size(TEXT.id)
            .text_color(t.foreground)
            .bg(t.raised)
            .cursor_pointer()
            .hover((style) => style.bg(t.hover))
            .on_click(() => this.refresh(this.cx))
            .child("Look again"),
        ),
      );
  }
}

/** The first line of what a command said, or an empty string. */
function firstLine(text) {
  return String(text ?? "").split("\n")[0].trim();
}
