// The sidebar's footer: the three rows a person reaches for — the app's own
// settings, shprd's settings, and the service that feeds the board.
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/workspace-chrome-shepherdr/main.js` (its `footer` branch: three
//   rows of 28 px, each one a click; the middle row opens the settings window
//   with `openSettings()`)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// The middle row is shprd's settings, and it opens them the way Shepherdr's
// "Shepherdr" row does: as a dialog over the window (`openSettings()` in
// `lib/panel.js`), never as a tab of the side panel. It is the only way in, which
// is why the row is the one that says what is behind it.
//
// shprd's own twist: the third row is the service. A window whose board is empty
// because `shprd serve` was never started looks exactly like a run nobody added a
// task to, so this row says which one it is and starts the service when it is
// missing. Everything else in the window reads that service.
//
// The start is `hc run --bg`, not the app's own `exec`, and that is deliberate:
//
// * `exec` is `machine.exec`, documented "no terminal, ≤600 s, **everything it
//   starts dies with it**" (M7ef §2.3). A server started through it would be
//   killed when the call ends, and the board would go dark again a moment after
//   the click — the one failure this row exists to prevent.
// * `hc run --bg` starts a **herdr-core terminal**: it returns at once ("start it
//   and come back at once (a server, several workers)") and the program lives in
//   the core, where terminals outlive the window by design (`docs`: "Close
//   Herdr, update it, or let it crash, and your agents keep going"). Measured in
//   a private session while writing this: the call returned in 0.3 s, the
//   terminal stayed `live`, and its service answered `GET /api/health` minutes
//   later.
// * It also gives the person a place to look: the service's own stderr is in a
//   terminal called `shprd-serve`, not in a pipe nobody reads.

import { View, div } from "gpui-kit";
import { h_flex } from "gpui-base";
import { hc, run, spaces, theme } from "herdr";

import { discover, reason } from "./lib/client.js";
import { openSettings } from "./lib/panel.js";
import { SPACE, TEXT } from "./lib/tokens.js";

/** How often the footer asks whether the service is up. */
const FLOOR_MS = 8000;
/** The name of the terminal the service runs in, so a person can find it twice. */
const TERMINAL = "shprd-serve";

export default class Chrome extends View {
  init(_props, cx) {
    this.cx = cx;
    this.service = null;
    this.failure = null;
    this.starting = false;
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
   * The command is `hc run --bg`, so the call is the *start* of a terminal and
   * not the life of the server: it answers as soon as the core has it, and the
   * process stays in the core. The row then polls for `service.json` itself, so
   * what it reports is the service's own answer and never this button's hope.
   */
  async start() {
    const folder = spaces().find((space) => space.active)?.folder;
    if (!folder) {
      this.failure = "No space names a repository yet";
      this.cx.notify();
      return;
    }
    if (this.starting) return;
    this.starting = true;
    this.failure = null;
    this.cx.notify();
    try {
      // `--keep`: if the program is lost (a crash, a reboot) the core leaves a
      // shell in its place rather than re-running a server nobody asked to
      // restart. `--no-parent`: the service is not a child of whatever terminal
      // the click came from, so that terminal ending must not take it down.
      const answer = await hc([
        "run",
        "--bg",
        "--keep",
        "--no-parent",
        "--name",
        TERMINAL,
        "--cwd",
        folder,
        "--",
        "shprd",
        "serve",
      ]);
      if (answer?.code !== 0) {
        this.failure = firstLine(answer?.stderr) || `hc run answered ${answer?.code}`;
      }
    } catch (err) {
      // `hc` rejects when it cannot start a terminal at all: a missing `hc`, a
      // core that is not running, a folder that is gone.
      this.failure = reason(err);
    }
    this.starting = false;
    await this.check();
    // A service takes a moment to bind and write its record; one more look is
    // worth it, so the click reads as "started" rather than "start failed".
    if (this.service === null && this.failure === null) {
      this.cx.timer.after(1200, (cx) => this.check(cx));
    }
  }

  render() {
    const t = theme();
    return div()
      .size_full()
      .v_flex()
      .px(8)
      .py(6)
      .gap(2)
      .children(this.actions(t).map((action) => this.row(t, action)));
  }

  /**
   * The footer's rows, in order: what each one says and what its click does.
   *
   * The three are Shepherdr's own three (`mods/workspace-chrome-shepherdr`): the
   * app's settings, its own, and — shprd's twist — the service. The middle row is
   * the only way into the settings window, so it opens the dialog rather than
   * moving the side panel.
   */
  actions(t) {
    const online = this.service !== null;
    const word = online
      ? `Service on :${this.service.record.port}`
      : this.starting
        ? "Starting the service…"
        : (this.failure ?? "Start the service");
    const hint = online ? "shprd serve" : this.starting ? TERMINAL : "click to run it";
    const dot = online ? t.done : this.starting ? t.working : t.blocked;
    return [
      {
        id: "settings",
        word: "Settings",
        hint: "The app's own",
        dot: t.dim,
        onClick: () => run("app.settings"),
      },
      {
        id: "shprd",
        word: "shprd",
        hint: "Tasks, agents, roles",
        dot: t.accent,
        onClick: () => openSettings(),
      },
      {
        id: "service",
        word,
        hint,
        dot,
        onClick: online || this.starting ? null : () => this.start(),
      },
    ];
  }

  /** One 28 px row: a dot, a word, and a dim hint at the right. */
  row(t, action) {
    const base = h_flex()
      .id(action.id)
      .h(SPACE.sidebarRow)
      .flex_none()
      .px(8)
      .gap(8)
      .items_center()
      .rounded(8)
      .text_size(TEXT.label)
      .text_color(t.foreground)
      .hover((s) => s.bg(t.hover));
    const live = action.onClick ? base.cursor_pointer().on_click(action.onClick) : base;
    return live
      .child(div().w(6).h(6).flex_none().rounded(3).bg(action.dot))
      .child(action.word)
      .child(div().flex_1())
      .child(div().text_size(TEXT.id).text_color(t.dim).child(action.hint));
  }
}

/** The first line of what a command said, or an empty string. */
function firstLine(text) {
  return String(text ?? "").split("\n")[0].trim();
}
