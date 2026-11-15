// The Accept card: what a person is approving when a task is ready.
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/accept-card/main.js` (the summary, Changes, Checks, Proof and a
//   folded Details), `mods/chat/main.js` (its `head` branch: the back link, the
//   task's reference and its status line, and the switcher)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// M10.3 ships the two frames the layout mounts — the task head and the card body
// — so the layout passes `herdr-app check` and a person can see where the card
// goes. M10.5 fills them: the attempt's report, its commits and files, its gates,
// its review, and the Accept action. Nothing here pretends to be the card.

import { View, div } from "gpui-kit";
import { v_flex } from "gpui-base";
import { props, run, theme } from "herdr";

import { card, cardHeader, note, placeholder } from "./lib/panel.js";

export default class Accept extends View {
  render() {
    const t = theme();
    const wired = props();
    if (wired?.head) return this.head(t);
    return placeholder(
      t,
      "Accept",
      "The task's summary, its changes, its checks and its review land here in M10.5.",
      "M10.5",
    );
  }

  /** The task's head: back to the list, its reference, and the switcher. */
  head(t) {
    return card(t, [
      cardHeader(
        t,
        "Accept",
        div()
          .id("tasks-back")
          .text_size(12)
          .text_color(t.muted)
          .cursor_pointer()
          .hover((s) => s.text_color(t.foreground))
          .on_click(() => run("layout.select", { stack: "tasks", child: "tasks-list" }))
          .child("‹ Tasks"),
      ),
      v_flex().flex_1().children([note(t, "Pick a task in the list to see what it did.")]),
    ]);
  }
}
