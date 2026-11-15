// The sidebar's footer: the three rows a person reaches for — the app's own
// settings, shprd's settings panel, and the service that feeds the board.
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/workspace-chrome-shepherdr/main.js` (its `footer` branch: three
//   rows of 28 px, each one a click)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// shprd's own twist: the third row is the service. A window whose board is empty
// because `shprd serve` was never started looks exactly like a run nobody added a
// task to, so this row says which one it is and starts the service when it is
// missing. Everything else in the window reads that service.

import { View, div } from "gpui-kit";
import { h_flex } from "gpui-base";
import { exec, run, spaces, theme } from "herdr";

import { discover, reason } from "./lib/client.js";
import { SPACE, TEXT } from "./lib/tokens.js";

/** How often the footer asks whether the service is up. */
const FLOOR_MS = 8000;

export default class Chrome extends View {
  init(_props, cx) {
    this.cx = cx;
    this.service = null;
    this.failure = null;
    this.check();
    cx.timer.every(FLOOR_MS, (cx) => this.check(cx));
  }

  /** Looks for the service, and remembers what it found. */
  async check(cx = this.cx) {
    this.service = await discover({ wait: (ms) => cx.sleep(ms) });
    cx.notify();
  }

  /**
   * Starts `shprd serve` in the shown space's folder, which in shprd is the
   * repository the run works on.
   *
   * It is fired and not awaited for its whole life: `serve` stays in the
   * foreground until it is stopped, so the click starts it and the round of
   * `check()` afterwards is what sees it answer.
   */
  async start() {
    const folder = spaces().find((space) => space.active)?.folder;
    if (!folder) {
      this.failure = "No space names a repository yet";
      this.cx.notify();
      return;
    }
    try {
      this.failure = null;
      await exec("shprd", ["serve"], { cwd: folder, timeout: 600 });
    } catch (err) {
      // `serve` runs until stopped, so its exit is the interesting event, not
      // its failure to return.
      this.failure = reason(err);
    }
    await this.check();
  }

  render() {
    const t = theme();
    const online = this.service !== null;
    const rows = [
      this.row("settings", t, "Settings", "The app's own", t.dim, () => run("app.settings")),
      this.row("shprd", t, "shprd", "Tasks, agents, roles", t.accent, () => {
        run("layout.select", { stack: "panel-body", child: "settings" });
        run("layout.show", { id: "panel" });
      }),
      this.row(
        "service",
        t,
        online ? `Service on :${this.service.record.port}` : (this.failure ?? "Start the service"),
        online ? "shprd serve" : "click to run it",
        online ? t.done : t.blocked,
        () => (online ? null : this.start()),
      ),
    ];
    return div()
      .size_full()
      .v_flex()
      .px(8)
      .py(6)
      .gap(2)
      .children(rows);
  }

  /** One 28 px row: a dot, a word, and a dim hint at the right. */
  row(id, t, word, hint, dot, onClick) {
    const base = h_flex()
      .id(id)
      .h(SPACE.sidebarRow)
      .flex_none()
      .px(8)
      .gap(8)
      .items_center()
      .rounded(8)
      .text_size(TEXT.label)
      .text_color(t.foreground)
      .hover((s) => s.bg(t.hover));
    const live = onClick ? base.cursor_pointer().on_click(onClick) : base;
    return live
      .child(div().w(6).h(6).flex_none().rounded(3).bg(dot))
      .child(word)
      .child(div().flex_1())
      .child(div().text_size(TEXT.id).text_color(t.dim).child(hint));
  }
}
