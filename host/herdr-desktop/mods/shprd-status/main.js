// The status bar: which run this window is showing, how the board stands, and the
// last pass's failure when there is one.
//
// It is the most faithfully portable mod in Shepherdr (M7b §4.2), and this is the
// same shape: the machine or run at the left, a *Needs you* badge when something
// does, and the palette hint at the right from the binding the app really has.
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/workspace-status-chat/main.js`
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// It reads the service, not Herdr's own space list: in shprd a space is a task,
// and what a person needs on this line is how the *run* is doing. Without a
// service it says so in words and never pretends a run is idle.

import { View, div } from "gpui-kit";
import { h_flex } from "gpui-base";
import { props, run, theme, keys, window } from "herdr";

import { Client, discover, reason } from "./lib/client.js";
import { counts, lastFailure, runLabel } from "./lib/board.js";
import { countBadge, label } from "./lib/panel.js";
import { RADIUS, TEXT, tone } from "./lib/tokens.js";

/** How often the bar reads the board even when nothing wakes it. */
const FLOOR_MS = 5000;

export default class Status extends View {
  init(_props, cx) {
    this.state = null;
    this.failure = null;
    this.client = null;
    this.cx = cx;
    this.refresh();
    // A change file a mod watches is the live arm; this timer is the floor, so a
    // service that started after the window did is found without a click.
    cx.timer.every(FLOOR_MS, (cx) => this.refresh(cx));
  }

  /** Reads the board once, or records why it could not. */
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
      }
      const state = await this.client.state();
      if (state) this.state = state;
      this.failure = null;
    } catch (err) {
      // A service that restarted answers with another id: forget the client so
      // the next look finds the new port instead of dialling a dead one.
      this.client = null;
      this.failure = reason(err);
    }
    cx.notify();
  }

  /** The palette key as actually bound, not a hardcoded one. */
  hint() {
    const mac = window()?.platform === "macos";
    const mod = {
      cmd: mac ? "⌘" : "Ctrl+",
      ctrl: mac ? "⌃" : "Ctrl+",
      shift: mac ? "⇧" : "Shift+",
      alt: mac ? "⌥" : "Alt+",
      option: mac ? "⌥" : "Alt+",
    };
    const found = keys().find((k) => k.section === "global" && k.action === "app.palette");
    const parts =
      found && found.keys.endsWith("--")
        ? [...found.keys.slice(0, -2).split("-"), "-"]
        : found?.keys.split("-");
    const key = parts && parts.map((p) => mod[p.toLowerCase()] ?? p.toUpperCase()).join("");
    return key ? `${key}  Commands` : "Commands";
  }

  render() {
    const t = theme();
    const colors = tone(t);
    const tally = counts(this.state);
    const failure = this.failure ?? lastFailure(this.state);
    return h_flex()
      .size_full()
      .px(14)
      .gap(16)
      .items_center()
      .bg(t.chrome)
      .border_t(1)
      .border_color(t.border)
      .text_size(TEXT.label)
      .text_color(t.muted)
      .child(label(t, t.accent, "shprd"))
      .child(div().text_color(this.state ? t.foreground : t.dim).child(runLabel(this.state)))
      .when(tally.working > 0, (el) =>
        el.child(label(t, colors.amber, `${tally.working} working`)),
      )
      .when(tally.ready > 0, (el) =>
        el.child(
          h_flex()
            .gap(6)
            .items_center()
            .child(countBadge(t, countBadge(tally.ready), colors.green, colors.on))
            .child("Ready"),
        ),
      )
      .when(tally.needs > 0, (el) =>
        el.child(
          h_flex()
            .gap(6)
            .items_center()
            .child(countBadge(t, countBadge(tally.needs), colors.red, colors.on))
            .child("Needs you"),
        ),
      )
      .when(failure !== null, (el) =>
        el.child(div().text_color(colors.red).child(`⚠ ${failure}`)),
      )
      .child(div().flex_1())
      .child(
        div()
          .id("palette")
          .px(6)
          .h(20)
          .flex()
          .items_center()
          .rounded(RADIUS.control)
          .text_color(t.dim)
          .cursor_pointer()
          .hover((s) => s.bg(t.hover).text_color(t.foreground))
          .on_click(() => run("app.palette"))
          .child(this.hint()),
      );
  }
}
