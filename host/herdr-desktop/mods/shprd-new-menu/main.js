// The tab bar's "+" menu, opened over the bar as a 250 px popup.
//
// Its rows are `lib/menu.js`'s: Terminal, New task…, then a heading and the
// service's roster — the programs this machine can start, installed ones first
// with their 1–9 keys, the rest dimmed and saying *Not installed*. The rows are
// the same data the bar measured the popup with, so the two never disagree.
//
// Ported from Shepherdr's `mods/new-menu-tasks` (MIT):
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/new-menu-tasks/main.js` (the rows, the 1–9 keys, the heading,
//   the dim *Not installed* line), `core/menus.js` (`NEW_MENU`, `agentChoices`,
//   `agentHint`), `lib/ui.js` (the 32 px row and its 16 px icon slot)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// What is shprd's own:
//
// - *New task…* stands where Shepherdr's *New space*, *New worktree…* and *Open
//   worktree…* stand. shprd's spaces are the service's (`shprd serve` makes the
//   repository's, a task gets its own when it starts) and its worktrees are the
//   engine's, so the thing a person adds from this menu is a task — and the row
//   goes to the Tasks panel's own "+" form rather than a dialog of its own.
// - *Start an agent* is `POST /api/agents`: the service opens the tab and starts
//   the program (`shprd-server`'s `agents`), never this mod. Shepherdr's mod runs
//   `tab.new {argv}` itself; a thin mod is the rule here.
// - There is no *Show Assistant* row: shprd's coordinator is the sidebar block
//   (M10.8) until the per-task chat (U4) replaces it.

import { View, div } from "gpui-kit";
import { v_flex } from "gpui-base";
import { closeOverlay, props, run, spaces, theme } from "herdr";

import { attach, reason } from "./lib/client.js";
import { agentChoices, MENU, NEW_WORDS, newMenu } from "./lib/menu.js";
import { menuRow } from "./lib/panel.js";
import { RADIUS, TEXT } from "./lib/tokens.js";

export default class NewMenu extends View {
  init(_props, cx) {
    this.cx = cx;
    this.at = 0;
    this.client = null;
    this.roster = props()?.roster ?? null;
    // A refusal to *start* something is red, under the rows; a roster that could
    // not be read is the group's own dim line, in the place the programs would be
    // (Shepherdr's *Couldn't load agents*, `NEW_MENU.failed`).
    this.failure = null;
    this.loadFailure = null;
    this.handle = cx.focus_handle();
    this.handle.focus();
    // A copy opened without a roster (the window's own click passes one; a bare
    // `mod.open` does not) reads it here, and then draws the same rows the bar
    // would have measured — a second look, never a second answer.
    if (this.roster === null) this.load();
  }

  /**
   * Reads the roster once, so a copy opened without one still offers the programs.
   *
   * The window's own "+" passes the roster it read when the bar started, so this
   * only runs for a copy opened on its own. A service that cannot be reached is
   * said in the group's own place, in the menu's words rather than a sentence
   * about a service a person did not ask about.
   */
  async load() {
    try {
      const next = await attach(this.client, { wait: (ms) => this.cx?.sleep?.(ms) });
      if (!next.client) {
        this.roster = { programs: [] };
        this.loadFailure = NEW_WORDS.failed;
        this.cx?.notify?.();
        return;
      }
      this.client = next.client;
      this.roster = await next.client.roster();
      this.loadFailure = null;
    } catch (err) {
      this.roster = { programs: [] };
      this.loadFailure = `${NEW_WORDS.failed} (${reason(err)})`;
    }
    this.cx?.notify?.();
  }

  /** The rows, as `lib/menu.js` gives them. */
  rows() {
    return newMenu({
      roster: this.roster,
      loading: this.roster === null && !this.loadFailure,
      failure: this.loadFailure,
    });
  }

  /**
   * Runs one row and closes the menu.
   *
   * `Terminal` is Herdr's own `tab.new` in the shown space — the app opens a
   * shell, this mod starts nothing. A program row is the service's: a tab appears
   * where the service put it, and the bar selects it once the answer names it.
   */
  choose(index) {
    const row = this.rows()[index];
    if (!row || row.heading || row.note || row.disabled) return;
    closeOverlay(props()?.overlay);
    if (row.command) {
      const space = spaces().find((entry) => entry.active)?.id;
      run(row.command.name, { ...(row.command.args ?? {}), ...(space ? { space } : {}) });
      return;
    }
    if (row.id === "new-task") {
      run("layout.select", { stack: "panel-body", child: "tasks" });
      run("layout.show", { id: "panel" });
      run("shprd-tasks.add");
      return;
    }
    if (row.action === "agent" && row.program) this.start(row.program);
  }

  /**
   * Starts one program through the service, and selects the tab it opened.
   *
   * A refusal is the service's words, drawn under the rows: a click that opened
   * no tab has to say why (no host, a space that is not there, a program this
   * machine does not have).
   */
  async start(program) {
    try {
      const next = await attach(this.client, { wait: (ms) => this.cx?.sleep?.(ms) });
      if (!next.client) {
        this.failure = "shprd serve is not running";
        this.cx?.notify?.();
        return;
      }
      this.client = next.client;
      const opened = await this.client.openAgent(program);
      this.failure = null;
      if (opened?.tab) run("tab.select", { id: opened.tab });
    } catch (err) {
      this.failure = reason(err);
      this.cx?.notify?.();
    }
  }

  render() {
    const t = theme();
    const list = this.rows();
    return v_flex()
      .id("new-menu")
      .track_focus(this.handle)
      .size_full()
      .min_h(0)
      .p(MENU.pad)
      .bg(t.popover)
      .text_size(TEXT.body)
      .on_key_down((event, cx) => {
        // The number keys start a program, Shepherdr's 1–9: the hint on the row
        // is the key, in the order the rows are drawn.
        const typed = /^[1-9]$/.test(String(event.key ?? "")) ? Number(event.key) - 1 : null;
        if (typed !== null && !event.modifiers?.control && !event.modifiers?.alt) {
          const choices = agentChoices(this.roster?.programs ?? []);
          const program = choices[typed];
          if (program?.installed) {
            closeOverlay(props()?.overlay);
            this.start(program.id);
          }
          return;
        }
        if (event.key === "down") this.at = (this.at + 1) % Math.max(1, list.length);
        else if (event.key === "up")
          this.at = (this.at + Math.max(1, list.length) - 1) % Math.max(1, list.length);
        else if (event.key === "enter") this.choose(this.at);
        else if (event.key === "escape") closeOverlay(props()?.overlay);
        else return;
        cx?.notify?.();
      })
      .children(list.map((row, index) => this.line(t, row, index)))
      .when(Boolean(this.failure), (el) =>
        el.child(
          div()
            .px(8)
            .pt(4)
            .text_size(TEXT.label)
            .text_color(t.blocked)
            .child(this.failure),
        ),
      );
  }

  /** One line of the menu: the heading, a dim note, or a row with its icon. */
  line(t, item, index) {
    if (item?.heading) {
      return div()
        .h(MENU.heading)
        .flex_none()
        .px(8)
        .flex()
        .items_center()
        .text_size(TEXT.label)
        .font_medium()
        .text_color(t.dim)
        .child(item.heading);
    }
    if (item?.note) {
      return div()
        .flex_none()
        .px(8)
        .py(6)
        .text_size(TEXT.label)
        .text_color(t.dim)
        .child(item.note || NEW_WORDS.loading);
    }
    return div()
      .flex_none()
      .when(index === this.at, (el) => el.bg(t.selected).rounded(RADIUS.control))
      .child(
        menuRow(t, `new-${item.id}`, item.label, {
          icon: this.icon(item),
          hint: item.hint ?? null,
          disabled: Boolean(item.disabled),
          onClick: () => this.choose(index),
        }),
      );
  }

  /**
   * A row's glyph, from this mod's own icons: Shepherdr's SVGs (see the header of
   * `icons/*.svg`), which every shprd install carries.
   *
   * A program's row has none: the roster says what a program is called, not what
   * its icon file is (`ProgramDto` has no icon), and drawing the agent's glyph
   * from the app's program table would need a tab running it. A row without an
   * icon keeps its empty 16 px slot, so the labels still line up.
   */
  icon(item) {
    if (item.icon === "terminal") return "mods/shprd-new-menu/icons/terminal.svg";
    if (item.icon === "plus") return "mods/shprd-new-menu/icons/plus.svg";
    return null;
  }
}
