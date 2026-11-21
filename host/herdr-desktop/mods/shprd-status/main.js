// The status bar: which run this window is showing, how the board stands word by
// word, the last pass's failure when there is one, and whether the service is up.
//
// It is the most faithfully portable mod in Shepherdr (M7b §4.2), and this is the
// same shape: the machine or run at the left, a *Needs you* badge when something
// does, and the palette hint at the right from the binding the app really has.
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/workspace-status-chat/main.js` (the shown space at the left, the
//   red needs-you count, the palette key built from `keys()`), `core/tasks.js`
//   (`STATUS`, the words a count is named by)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// It reads the service, not Herdr's own space list: in shprd a space is a task,
// and what a person needs on this line is how the *run* is doing. Without a
// service it says so in words and never pretends a run is idle; a run whose last
// pass failed says which step, because a bar that hid it would leave a stuck run
// looking like an idle one.

import { View, div } from "gpui-kit";
import { h_flex } from "gpui-base";
import { keys, run, spaces, theme, window } from "herdr";

import { adoptCache, attach, downNotice, forgetAll, reason, remember, singleFlight, syncRepo, watchSwitch } from "./lib/client.js";
import { counts, lastFailure, runShort, since } from "./lib/board.js";
import { badge, countBadge, icon } from "./lib/panel.js";
import { RADIUS, TEXT, tone } from "./lib/tokens.js";
import { followChanges } from "./lib/watch.js";

/** How often the bar reads the board even when nothing wakes it. */
const FLOOR_MS = 5000;
/**
 * How wide the last pass's failure may be: one line of it, cut, never wider.
 *
 * The bar is as wide as a laptop window, and its counts and the palette hint
 * both come first: the failure gets what is left, and the words it cannot fit are
 * on the toast and in `shprd status`.
 */
const FAILURE_W = 200;

export default class Status extends View {
  init(_props, cx) {
    this.state = null;
    adoptCache(this);
    this.failure = null;
    this.service = null;
    this.client = null;
    this.cx = cx;
    this.watching = false;
    this.refresh();
    // The change file is the live arm, exactly as in the panels: a bar that only
    // polled would be a minute behind the board beside it. The timer is the
    // floor, so a service that started after the window did is found anyway.
    cx.timer.every(FLOOR_MS, (cx) => this.refresh(cx));
    watchSwitch(this, cx);
  }

  /**
   * Reads the board once, or records why it could not.
   *
   * A service that restarted is noticed before the read (`attach` compares the
   * id in `service.json` with this bar's client) and the bar re-reads the whole
   * board rather than keeping the old counts; a read that answers nothing is a
   * failure, not a board to keep painting (M10 gate bug 5).
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
        this.service = null;
        forgetAll(this);
        this.failure = downNotice();
        cx.notify();
        return;
      }
      this.client = next.client;
      this.follow();
      this.service = this.client.record;
      remember(this, this.client, await this.client.state());
      this.failure = null;
    } catch (err) {
      // A service that restarted or went away: forget the client so the next
      // look finds the new one instead of dialling a dead port.
      this.client = null;
      this.service = null;
      this.failure = reason(err);
    }
    cx.notify();
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
   * The palette key as actually bound, not a hardcoded one.
   *
   * `keys()` is the app's own answer, and on this build it lists the bindings the
   * keymap file holds — never the palette's own default, which the app keeps
   * inside itself. So the hint shows the key when a person bound one and the word
   * alone when they did not: "Commands" is what the click does, and a guessed
   * glyph would be a key that may not work.
   */
  hint() {
    const mac = window()?.platform === "macos";
    const mod = {
      cmd: mac ? "⌘" : "Ctrl+",
      ctrl: mac ? "⌃" : "Ctrl+",
      shift: mac ? "⇧" : "Shift+",
      alt: mac ? "⌥" : "Alt+",
      option: mac ? "⌥" : "Alt+",
    };
    const found = keys().find((key) => key.section === "global" && key.action === "app.palette");
    const parts =
      found && found.keys.endsWith("--")
        ? [...found.keys.slice(0, -2).split("-"), "-"]
        : found?.keys.split("-");
    const key = parts && parts.map((part) => mod[part.toLowerCase()] ?? part.toUpperCase()).join("");
    return key ? `${key}  Commands` : "Commands";
  }

  /** An icon and its words, as Shepherdr's bar writes a machine and a branch. */
  label(t, name, text, color) {
    return h_flex()
      .flex_none()
      .gap(6)
      .items_center()
      .child(icon(name, 14, t.muted))
      .child(div().text_color(color ?? t.muted).child(text));
  }

  render() {
    syncRepo(this);
    const t = theme();
    const colors = tone(t);
    const live = this.service !== null;
    const failure = this.failure ?? lastFailure(this.state);
    const need = counts(this.state).needs;
    const branch = spaces().find((space) => space.active)?.branch;
    // Shepherdr's bar (`mods/workspace-status-chat/main.js:37-44`): the machine,
    // the shown space's branch, the red badge and "Needs you", then the key hint.
    // shprd adds the run beside the machine and the service's state when it is
    // down; the failure line stays.
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
      .child(this.label(t, "monitor", "This computer"))
      .when(live && Boolean(this.state), (el) =>
        el.child(div().flex_none().text_color(t.foreground).child(this.failure ?? runShort(this.state))),
      )
      .when(!live, (el) =>
        el.child(div().flex_none().text_color(colors.red).child(this.failure ?? "shprd · down")),
      )
      .when(Boolean(branch), (el) => el.child(this.label(t, "branch", branch)))
      .when(live && need > 0, (el) =>
        el.child(
          h_flex()
            .flex_none()
            .gap(6)
            .items_center()
            .child(badge(t, countBadge(need), colors.red, colors.on))
            .child(div().child("Needs you")),
        ),
      )
      .child(div().flex_1())
      // The last pass's failure sits at the right, before the key hint and never
      // instead of it: it has a width of its own and is cut inside it, so a long
      // error cannot push the way out of the window off the edge.
      .when(failure !== null && live, (el) =>
        el.child(
          h_flex()
            .flex_none()
            .w(FAILURE_W)
            .gap(6)
            .items_center()
            .overflow_hidden()
            .child(
              div()
                .flex_1()
                .min_w(0)
                .truncate()
                .text_color(colors.red)
                .child(`⚠ ${failure}`),
            )
            .when(this.state?.last_tick?.at_ms, (inner) =>
              inner.child(
                div()
                  .flex_none()
                  .text_size(TEXT.id)
                  .text_color(t.dim)
                  .child(since(this.state.last_tick.at_ms)),
              ),
            ),
        ),
      )
      .child(
        div()
          .id("palette")
          .px(6)
          .h(20)
          .flex_none()
          .flex()
          .items_center()
          .rounded(RADIUS.control)
          .text_color(t.dim)
          .cursor_pointer()
          .hover((style) => style.bg(t.hover).text_color(t.foreground))
          .on_click(() => run("app.palette"))
          .child(this.hint()),
      );
  }
}
