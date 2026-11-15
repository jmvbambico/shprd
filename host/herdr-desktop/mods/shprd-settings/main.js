// The settings panel: the roles table with the program, model and effort each
// role runs, and the role prompts (templates) a person can edit.
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/shepherdr-settings/main.js` (the page's row anatomy: title, one
//   line about, the value and the scope line at the right, the box underneath;
//   the role row's picker button; the write-with-no-Save-step rule),
//   `mods/shepherdr-settings/settings.js` (the model menu opened from a role)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// shprd's settings are layered — your own file, this repository's entry, the
// repository's committed defaults and what shprd ships — so every value is drawn
// with the layer that answered it, and a refused write shows the service's own
// sentence verbatim: a cross-vendor review is refused in words that name both
// roles and both vendors, and a person needs exactly those words.
//
// The page has two halves, because they answer to different routes:
//
// - **Roles** reads `GET /api/settings` (each value and its layer) and
//   `GET /api/roster` (the programs a role may run, and their models), and every
//   click writes one key with `PUT /api/settings`.
// - **Templates** lists `GET /api/prompts`, reads one with
//   `GET /api/prompts/:id`, saves it with `PUT /api/prompts/:id` and puts the
//   shipped words back with `DELETE /api/prompts/:id`.
//
// Nothing here is written to a file by this mod: the service is the one writer,
// and what the page shows is always the service's answer.

import { View, div } from "gpui-kit";
import { Input, InputState, Textarea, TextareaState, h_flex, v_flex } from "gpui-base";
import { nextCommand, theme } from "herdr";

import { attach, reason } from "./lib/client.js";
import {
  health,
  outcomeOf,
  promptLine,
  promptRows,
  refused,
  roles,
  withoutRole,
  withRole,
} from "./lib/settings.js";
import { badge, card, cardHeader, keycap, note } from "./lib/panel.js";
import { RADIUS, SPACE, TEXT, tone } from "./lib/tokens.js";

/** How often the panel re-reads the service, so another window's write shows. */
const FLOOR_MS = 10_000;

/** The two halves of the page. */
const TABS = [
  { id: "roles", word: "Roles" },
  { id: "templates", word: "Templates" },
];

export default class Settings extends View {
  init(_props, cx) {
    this.cx = cx;
    this.settings = null;
    this.roster = null;
    this.prompts = null;
    this.failure = null;
    this.client = null;
    this.tab = "roles";
    /** The picker showing its choices, as `<role>.<field>`, or null. */
    this.open = null;
    /** The picker whose "use an id typed" box is showing, as `<role>.<field>`. */
    this.typedKey = null;
    /** The key a write is in flight for. */
    this.writing = null;
    /** Where a write goes: the person's own defaults, or this repository's entry. */
    this.scope = "you";
    /** The service's refusal, verbatim, and the key it was about. */
    this.refusal = null;
    /** What the last write or reset did, in words. */
    this.saved = null;
    /** The prompt the editor is showing, as the service last answered it. */
    this.prompt = null;
    /** The template editor's retained state, made on the first prompt opened. */
    this.editor = null;
    /** The id a new role gets, typed into a box made on the first Add. */
    this.newRole = null;
    /** The id a picker's typed value goes in. */
    this.typed = null;
    this.loading = false;
    this.loadedAt = 0;
    this.scheduled = false;
    this.armedAt = 0;
    this.refresh();
    this.schedule();
    // The keys.json commands a person (or a script) may send this page:
    // `shprd-settings.roles`, `.templates`, `.save`, `.reset`.
    cx.spawn(async () => {
      for (;;) {
        const command = await nextCommand();
        this.command(command);
        cx.notify();
      }
    });
  }

  /** What one of the page's own commands does. */
  command(name) {
    if (name === "roles") {
      this.tab = "roles";
      this.prompt = null;
      this.saved = null;
    } else if (name === "templates") {
      this.tab = "templates";
      const rows = promptRows(this.prompts);
      if (this.prompt === null && rows.length > 0) {
        this.openPrompt(rows[0]);
      } else if (rows.length === 0) {
        // The list has not been read yet: the next look opens its first prompt,
        // so a key pressed while the service is still answering is not lost.
        this.wantFirst = true;
      }
    } else if (name === "save") {
      if (this.prompt !== null) this.savePrompt();
    } else if (name === "reset") {
      if (this.prompt !== null) this.resetPrompt();
    }
  }

  /**
   * Reads the settings, the roster and the template list.
   *
   * A service that restarted is a different run's settings: `attach` notices it
   * by the `id` in `service.json`, the page forgets what it read and reads all of
   * it again, so a page never shows one service's roles over another's run
   * (M10 gate bug 5).
   */
  async refresh(cx = this.cx) {
    try {
      const next = await attach(this.client, { wait: (ms) => cx.sleep(ms) });
      if (!next.client) {
        this.client = null;
        this.settings = null;
        this.failure = "shprd serve is not running";
        cx.notify();
        return;
      }
      if (next.restarted) this.forget();
      this.client = next.client;
      const settings = await this.client.call("GET", "/api/settings", { strict: true });
      this.settings = settings;
      // The service answered, so the page is not down. The roles are drawn
      // before the roster is asked: its pickers fill in when the programs
      // answer, and a page that waited on four model listings to show a table it
      // already had would look stuck.
      this.failure = null;
      cx.notify();
      // The roster and the template list are each the page's to do without: a
      // listing that timed out leaves the pickers' menus empty and the tables
      // drawn, never the whole page blank.
      const roster = await this.client.roster().catch(() => null);
      if (roster) this.roster = roster;
      const prompts = await this.client.prompts().catch(() => null);
      if (prompts) this.prompts = prompts;
      if (this.wantFirst) {
        this.wantFirst = false;
        const rows = promptRows(this.prompts);
        if (this.prompt === null && rows.length > 0) this.openPrompt(rows[0]);
      }
    } catch (err) {
      // A restarted service answers with another id: forget the client so the
      // next look finds the new port instead of dialling a dead one.
      this.client = null;
      this.failure = reason(err);
    }
    cx.notify();
  }

  /**
   * Drops what the page read from the service that went away: its settings, its
   * programs and its templates are that run's, and a new service has its own.
   */
  forget() {
    this.settings = null;
    this.roster = null;
    this.prompts = null;
    this.prompt = null;
    this.open = null;
    this.refusal = null;
  }

  /** Shows one picker's choices, or closes it. */
  toggle(key) {
    this.open = this.open === key ? null : key;
    this.typedKey = null;
    this.refusal = null;
    this.cx.notify();
  }

  /** Shows the box a picker's typed value goes in, making it on first use. */
  typePick(key) {
    this.typedKey = key;
    if (!this.typed) {
      this.typed = InputState.new({ placeholder: "Type an id", value: "" });
    }
    this.cx.notify();
  }

  /** Writes one settings key, and keeps whatever the service answered. */
  async apply(key, value) {
    if (!this.client || this.writing !== null) return;
    this.writing = key;
    this.refusal = null;
    this.saved = null;
    this.cx.notify();
    try {
      const reply = await this.client.putSetting(key, value, this.scope === "repo");
      if (reply?.settings) this.settings = reply.settings;
      this.saved = outcomeOf(reply);
      this.open = null;
      this.typedKey = null;
      this.writing = null;
      this.cx.notify();
      return;
    } catch (err) {
      // A refusal is the service's sentence, shown as it was sent: the person
      // has to read *why* ("both anthropic"), not a summary of it.
      this.refusal = refused(key, err);
    }
    this.writing = null;
    this.cx.notify();
  }

  /** Writes the value typed into an open picker. */
  applyTyped(key) {
    const value = (this.typed?.value?.() ?? "").trim();
    if (value === "") return;
    return this.apply(key, value);
  }

  /** Adds a role by putting its id in `roles.order`. */
  addRole() {
    const order = withRole(this.settings, this.newRole?.value?.() ?? "");
    if (!order) {
      this.refusal = {
        key: "roles.order",
        text: "a role id is letters, digits, `-` or `_`, and one no role has yet",
      };
      this.cx.notify();
      return;
    }
    return this.apply("roles.order", order);
  }

  /** Takes a role the person made out of `roles.order`. */
  removeRole(role) {
    return this.apply("roles.order", withoutRole(this.settings, role.id));
  }

  /** Opens one template in the editor, making the editor on first use. */
  async openPrompt(row) {
    if (!this.client) return;
    this.saved = null;
    this.refusal = null;
    this.cx.notify();
    try {
      const prompt = await this.client.prompt(row.id);
      if (!prompt) return;
      this.prompt = prompt;
      if (!this.editor) {
        this.editor = TextareaState.new({ rows: 14 });
      }
      this.editor.set_value(prompt.text);
    } catch (err) {
      this.refusal = refused(`prompts.${row.id}`, err);
    }
    this.cx.notify();
  }

  /** Stores the editor's words over the open prompt. */
  async savePrompt() {
    if (!this.client || !this.prompt || this.writing !== null) return;
    const id = this.prompt.id;
    this.writing = `prompts.${id}`;
    this.refusal = null;
    this.saved = null;
    this.cx.notify();
    try {
      const saved = await this.client.putPrompt(id, this.editor?.value?.() ?? "");
      if (saved) this.prompt = saved;
      this.saved = { outcome: "stored", text: "Saved: your words are what an agent is told." };
      const list = await this.client.prompts();
      if (list) this.prompts = list;
      this.writing = null;
      this.cx.notify();
      return;
    } catch (err) {
      this.refusal = refused(`prompts.${id}`, err);
    }
    this.writing = null;
    this.cx.notify();
  }

  /** Puts the shipped words back over the open prompt. */
  async resetPrompt() {
    if (!this.client || !this.prompt || this.writing !== null) return;
    const id = this.prompt.id;
    this.writing = `prompts.${id}`;
    this.refusal = null;
    this.saved = null;
    this.cx.notify();
    try {
      const reset = await this.client.resetPrompt(id);
      if (reset) {
        this.prompt = reset;
        this.editor?.set_value?.(reset.text);
      }
      this.saved = { outcome: "stored", text: "Reset: the shipped words are back." };
      const list = await this.client.prompts();
      if (list) this.prompts = list;
      this.writing = null;
      this.cx.notify();
      return;
    } catch (err) {
      this.refusal = refused(`prompts.${id}`, err);
    }
    this.writing = null;
    this.cx.notify();
  }

  /**
   * Reads again when the panel is drawn with an old answer.
   *
   * A block that is hidden in a stack is not guaranteed to keep its timer, so a
   * panel shown after a while must not sit on what it read before: the moment
   * it is drawn is the moment to catch up. `loading` keeps one read in flight,
   * and the floor keeps a repaint from becoming a poll.
   */
  revalidate() {
    // A block hidden in a stack is not guaranteed to keep the timer `init`
    // started, so being drawn is also when the poll is (re)started.
    this.schedule();
    // No watcher here: a settings write is the service's own file, not a ledger
    // write, so the change file says nothing about it. The floor is the read.
    this.follow?.();
    if (this.loading) return;
    if (Date.now() - this.loadedAt < FLOOR_MS) return;
    this.loading = true;
    this.refresh().finally(() => {
      this.loading = false;
      this.loadedAt = Date.now();
    });
  }

  /**
   * Polls the service while this copy is drawn: one read, then the next.
   *
   * A `timer.every` made in `init` is not guaranteed to survive the block being
   * hidden in a stack, and a poll that stopped is a panel that never updates
   * again — the live pass showed exactly that, with an answered card still on
   * the glass. So the next read is armed from whichever context is at hand (the
   * draw, or the read before it), and a timer that should have fired by now and
   * did not is treated as gone and armed again.
   */
  schedule(cx = this.cx) {
    const now = Date.now();
    if (this.scheduled && now - this.armedAt < FLOOR_MS * 2) return;
    this.scheduled = true;
    this.armedAt = now;
    cx.timer.after(FLOOR_MS, (next) => {
      this.scheduled = false;
      this.loadedAt = Date.now();
      this.refresh(next).finally(() => this.schedule(next));
    });
  }

  render() {
    this.revalidate();
    const t = theme();
    const colors = tone(t);
    const head = h_flex()
      .gap(6)
      .items_center()
      .child(div().text_size(TEXT.body).font_medium().text_color(t.foreground).child("Settings"))
      .child(div().flex_1())
      .child(keycap(t, "M10.6"));
    const tabs = h_flex()
      .h(SPACE.panelHeader)
      .flex_none()
      .px(12)
      .gap(6)
      .items_center()
      .border_b(1)
      .border_color(t.border)
      .children(TABS.map((tab) => this.tabButton(t, tab)));
    const body = v_flex()
      .flex_1()
      .min_h(0)
      .overflow_y_scrollbar()
      .py(6)
      .gap(6)
      .children(this.content(t, colors));
    return card(t, [cardHeader(t, head), tabs, body]);
  }

  /** One of the page's two halves. */
  tabButton(t, tab) {
    const active = this.tab === tab.id;
    return h_flex()
      .h(24)
      .flex_none()
      .px(8)
      .items_center()
      .rounded(RADIUS.control)
      .text_size(TEXT.label)
      .text_color(active ? t.foreground : t.muted)
      .when(active, (el) => el.bg(t.raised))
      .when(!active, (el) =>
        el.cursor_pointer().hover((style) => style.bg(t.hover)).on_click(() => {
          this.tab = tab.id;
          this.refusal = null;
          // The line a save left is about the half it happened in.
          this.saved = null;
          this.cx.notify();
        }),
      )
      .child(tab.word);
  }

  /** The body's blocks: what went wrong, then the half that is showing. */
  content(t, colors) {
    const blocks = [];
    if (this.failure !== null) {
      blocks.push(note(t, this.failure));
      blocks.push(
        note(t, "Start it from the sidebar's footer; this panel fills in when it answers."),
      );
      return blocks;
    }
    if (this.refusal !== null) blocks.push(this.refusalBlock(t, colors));
    if (this.saved !== null) {
      blocks.push(
        div().px(12).py(2).text_size(TEXT.label).text_color(colors.green).child(this.saved.text),
      );
    }
    const state = health(this.settings);
    for (const problem of state.problems) {
      blocks.push(div().px(12).py(2).text_size(TEXT.label).text_color(colors.red).child(problem));
    }
    if (state.notChecked) {
      blocks.push(
        div()
          .px(12)
          .py(2)
          .text_size(TEXT.label)
          .text_color(colors.amber)
          .child(`the roles were not checked: ${state.notChecked}`),
      );
    }
    if (this.settings === null) {
      blocks.push(note(t, "Reading the settings…"));
      return blocks;
    }
    blocks.push(...(this.tab === "roles" ? this.rolesPage(t, colors) : this.templatesPage(t, colors)));
    return blocks;
  }

  /** A refusal: the service's own sentence, unedited, in the tone it deserves. */
  refusalBlock(t, colors) {
    return v_flex()
      .mx(8)
      .p(8)
      .gap(2)
      .rounded(RADIUS.card)
      .border_1()
      .border_color(colors.red)
      .child(
        div().text_size(TEXT.id).text_color(t.dim).child(this.refusal.key ?? "refused"),
      )
      .child(div().text_size(TEXT.label).text_color(colors.red).child(this.refusal.text));
  }

  // -- roles ---------------------------------------------------------------

  /** The roles page: where a write goes, then one block per role. */
  rolesPage(t, colors) {
    const list = roles(this.settings, this.roster);
    return [
      this.scopeRow(t),
      ...list.map((role) => this.roleBlock(t, colors, role)),
      this.addRoleRow(t),
    ];
  }

  /** The two-scope switch: the person's own defaults, or this repository's entry. */
  scopeRow(t) {
    const chip = (id, word) => {
      const active = this.scope === id;
      return h_flex()
        .h(24)
        .px(8)
        .flex_none()
        .items_center()
        .rounded(RADIUS.control)
        .text_size(TEXT.label)
        .text_color(active ? t.foreground : t.muted)
        .when(active, (el) => el.bg(t.raised))
        .when(!active, (el) =>
          el.cursor_pointer().hover((style) => style.bg(t.hover)).on_click(() => {
            this.scope = id;
            this.cx.notify();
          }),
        )
        .child(word);
    };
    return h_flex()
      .h(SPACE.row)
      .flex_none()
      .px(12)
      .gap(6)
      .items_center()
      .text_size(TEXT.label)
      .child(div().text_color(t.dim).child("Write to"))
      .child(chip("you", "You"))
      .child(chip("repo", "This repo"))
      .child(div().flex_1())
      .when(this.roster?.note, (el) =>
        el.child(div().flex_none().text_size(TEXT.id).text_color(t.dim).child("no programs")),
      );
  }

  /** One role: its title and line, then the three picks. */
  roleBlock(t, colors, role) {
    const header = h_flex()
      .gap(6)
      .items_center()
      .child(div().text_size(TEXT.body).text_color(t.foreground).child(role.title))
      .child(
        div()
          .flex_none()
          .text_size(TEXT.id)
          .text_color(t.dim)
          .child(role.kind),
      )
      .when(role.custom, (el) => el.child(badge(t, "custom", t.raised, t.muted)))
      .when(role.readonly, (el) =>
        el.child(div().flex_none().text_size(TEXT.id).text_color(t.dim).child("reads only")),
      )
      .child(div().flex_1())
      .when(role.custom, (el) =>
        el.child(this.link(t, "Remove", () => this.removeRole(role), this.writing !== null)),
      );
    return v_flex()
      .flex_none()
      .mx(8)
      .p(8)
      .gap(2)
      .rounded(RADIUS.card)
      .border_1()
      .border_color(t.border)
      .child(header)
      .when(role.about !== "", (el) =>
        el.child(div().text_size(TEXT.id).text_color(t.dim).child(role.about)),
      )
      .children(role.picks.map((pick) => this.pickRow(t, colors, role, pick)));
  }

  /** One pick: its word, its value, the layer that set it, and its choices. */
  pickRow(t, colors, role, pick) {
    const key = `${role.id}.${pick.field}`;
    const open = this.open === key;
    const writing = this.writing === pick.key;
    const refusedHere = this.refusal?.key === pick.key;
    const row = h_flex()
      .h(SPACE.row)
      .flex_none()
      .px(6)
      .gap(8)
      .items_center()
      .rounded(RADIUS.control)
      .when(!open, (el) =>
        el.cursor_pointer().hover((style) => style.bg(t.hover)).on_click(() => this.toggle(key)),
      )
      .child(
        div()
          .w(64)
          .flex_none()
          .text_size(TEXT.id)
          .text_color(t.muted)
          .child(pick.label),
      )
      .child(
        div()
          .flex_1()
          .min_w(0)
          .truncate()
          .text_size(TEXT.label)
          .text_color(refusedHere ? colors.red : pick.value === null ? t.muted : t.foreground)
          .child(pick.display),
      )
      .child(
        div()
          .w(96)
          .flex_none()
          .text_size(TEXT.id)
          .text_color(writing ? colors.amber : t.dim)
          .child(writing ? "saving…" : pick.layer),
      )
      .child(div().flex_none().text_size(TEXT.id).text_color(t.dim).child(open ? "▴" : "▾"));

    return v_flex()
      .flex_none()
      .child(row)
      .when(open, (el) => el.child(this.choices(t, colors, pick)));
  }

  /** The choices one pick offers: the inherit row, then every value. */
  choices(t, colors, pick) {
    const blocks = [];
    if (pick.choices.length === 0) {
      blocks.push(
        div()
          .px(6)
          .py(4)
          .text_size(TEXT.id)
          .text_color(t.dim)
          .child(pick.note ?? "nothing to pick"),
      );
    }
    for (const choice of pick.choices) {
      const current = (pick.value ?? null) === choice.value;
      blocks.push(
        h_flex()
          .h(24)
          .flex_none()
          .px(6)
          .gap(6)
          .items_center()
          .rounded(RADIUS.control)
          .when(current, (el) => el.bg(t.selected ?? t.raised))
          .cursor_pointer()
          .hover((style) => style.bg(t.hover))
          .on_click(() => this.apply(pick.key, choice.value))
          .child(
            div()
              .text_size(TEXT.label)
              .text_color(choice.value === null ? t.muted : t.foreground)
              .child(choice.label),
          )
          .when(choice.hint !== null && choice.hint !== undefined, (el) =>
            el.child(div().text_size(TEXT.id).text_color(t.dim).child(choice.hint)),
          )
          .when(current, (el) =>
            el.child(div().flex_1()).child(div().text_size(TEXT.id).text_color(t.dim).child("✓")),
          ),
      );
    }
    // A program that lists no models, or a model its catalog has not heard of,
    // is still a value the person may type: Shepherdr's "use the id typed".
    if (pick.field !== "agent") {
      if (this.typedKey === pick.key) {
        blocks.push(
          h_flex()
            .h(28)
            .flex_none()
            .px(6)
            .gap(6)
            .items_center()
            .child(div().flex_1().min_w(0).child(Input.new(this.typed)))
            .child(this.link(t, "Use", () => this.applyTyped(pick.key), this.writing !== null)),
        );
      } else {
        blocks.push(
          h_flex()
            .h(24)
            .flex_none()
            .px(6)
            .items_center()
            .child(this.link(t, "Use an id typed", () => this.typePick(pick.key), false)),
        );
      }
    }
    if (pick.note && pick.choices.length > 0) {
      blocks.push(
        div().px(6).py(2).text_size(TEXT.id).text_color(t.dim).child(pick.note),
      );
    }
    return v_flex().flex_none().pl(72).gap(0).children(blocks);
  }

  /** The one row that makes a new role. */
  addRoleRow(t) {
    if (this.newRole === null) {
      return h_flex()
        .h(SPACE.row)
        .flex_none()
        .px(12)
        .items_center()
        .child(this.link(t, "Add a role", () => {
          this.newRole = InputState.new({ placeholder: "role id, e.g. docs", value: "" });
          this.cx.notify();
        }, false));
    }
    return h_flex()
      .h(32)
      .flex_none()
      .px(12)
      .gap(6)
      .items_center()
      .child(div().flex_1().min_w(0).child(Input.new(this.newRole)))
      .child(this.link(t, "Add", () => this.addRole(), this.writing !== null))
      .child(this.link(t, "Cancel", () => {
        this.newRole = null;
        this.cx.notify();
      }, false));
  }

  // -- templates -----------------------------------------------------------

  /** The templates page: the list, or the one prompt being edited. */
  templatesPage(t, colors) {
    const rows = promptRows(this.prompts);
    if (this.prompt !== null) return [this.promptEditor(t, colors)];
    if (rows.length === 0) {
      return [note(t, "No prompts yet."), note(t, "The service writes the shipped ones on first read.")];
    }
    return rows.map((row) => this.promptRow(t, colors, row));
  }

  /** One template in the list: its id, what it is for, and whether it is edited. */
  promptRow(t, colors, row) {
    const tone_ = row.problem !== null ? colors.red : row.edited ? colors.amber : t.dim;
    return h_flex()
      .h(SPACE.rowTwo)
      .flex_none()
      .mx(8)
      .px(8)
      .gap(8)
      .items_center()
      .rounded(RADIUS.card)
      .cursor_pointer()
      .hover((style) => style.bg(t.hover))
      .on_click(() => this.openPrompt(row))
      .child(
        v_flex()
          .flex_1()
          .min_w(0)
          .gap(2)
          .child(
            h_flex()
              .gap(6)
              .items_center()
              .child(
                div()
                  .font_family("JetBrains Mono")
                  .text_size(TEXT.label)
                  .text_color(t.foreground)
                  .child(row.id),
              )
              .child(div().flex_none().text_size(TEXT.id).text_color(tone_).child(row.word)),
          )
          .child(
            div()
              .text_size(TEXT.id)
              .text_color(t.dim)
              .child(row.problem ?? row.about),
          ),
      )
      .child(div().flex_none().text_size(TEXT.id).text_color(t.dim).child("›"));
  }

  /** The editor: what the file is, its words, and Save / Reset. */
  promptEditor(t, colors) {
    const prompt = this.prompt;
    const writing = this.writing === `prompts.${prompt.id}`;
    return v_flex()
      .flex_none()
      .mx(8)
      .p(8)
      .gap(6)
      .rounded(RADIUS.card)
      .border_1()
      .border_color(t.border)
      .child(
        h_flex()
          .gap(6)
          .items_center()
          .child(
            div()
              .font_family("JetBrains Mono")
              .text_size(TEXT.label)
              .text_color(t.foreground)
              .child(prompt.id),
          )
          .child(
            badge(
              t,
              prompt.state === "edited" ? "edited" : "shipped",
              prompt.state === "edited" ? colors.amber : t.raised,
              prompt.state === "edited" ? colors.on : t.muted,
            ),
          )
          .child(div().flex_1())
          .child(this.link(t, "Back", () => {
            this.prompt = null;
            this.refusal = null;
            this.saved = null;
            this.cx.notify();
          }, false)),
      )
      .child(div().text_size(TEXT.id).text_color(t.dim).child(prompt.path))
      .child(div().text_size(TEXT.id).text_color(t.dim).child(promptLine(prompt)))
      .child(
        v_flex()
          .h(240)
          .flex_none()
          .rounded(RADIUS.control)
          .border_1()
          .border_color(t.border)
          .overflow_hidden()
          .child(Textarea.new(this.editor)),
      )
      .child(
        h_flex()
          .h(28)
          .gap(6)
          .items_center()
          .child(this.link(t, writing ? "Saving…" : "Save", () => this.savePrompt(), writing))
          .child(this.link(t, "Reset to shipped", () => this.resetPrompt(), writing)),
      );
  }

  /** One small text button. */
  link(t, word, onClick, busy) {
    return h_flex()
      .h(24)
      .flex_none()
      .px(8)
      .items_center()
      .rounded(RADIUS.control)
      .border_1()
      .border_color(t.input ?? t.border)
      .text_size(TEXT.label)
      .text_color(busy ? t.dim : t.foreground)
      .when(!busy, (el) =>
        el.cursor_pointer().hover((style) => style.bg(t.hover)).on_click(() => onClick()),
      )
      .child(word);
  }
}
