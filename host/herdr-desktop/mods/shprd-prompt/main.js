// A text prompt, as Shepherdr's `mods/prompt-chat`: a title, a field with a label
// over it, and the app's one dialog footer. Rename tab is what opens it today.
//
// Opened as a dialog with props `{ title, label, value, placeholder, hint, verb,
// command: { name, args } }`: Enter (or the button) runs `command` with
// `{...args, name: the text}`, and Esc (or Cancel) closes without running
// anything. The field holds `value` and is focused when the dialog opens, so a
// person types over the name they are changing, as they would in a rename box.
//
// Ported from Shepherdr (MIT):
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/prompt-chat/main.js`, `lib/ui.js` (`dialogFooter`, `button`),
//   `lib/coordinator.js` (`renameTab`, the 400 × 196 dialog and its words)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik

import { View, div } from "gpui-kit";
import { Input, InputState, v_flex } from "gpui-base";
import { closeOverlay, props, run, theme } from "herdr";

import { dialogFooter } from "./lib/panel.js";
import { H, RADIUS, TEXT } from "./lib/tokens.js";

export default class Prompt extends View {
  init(_props, cx) {
    this.text = props()?.value ?? "";
    this.input = InputState.new({
      value: this.text,
      placeholder: props()?.placeholder ?? "",
    });
    if (this.text) this.input.set_selected_range?.({ start: 0, end: this.text.length });
    this.input.on("change", (_event, cx) => {
      this.text = this.input.value();
      cx?.notify?.();
    });
    this.input.on("submit", () => this.submit());
    this.inputFocused = true;
    this.input.on("focus", (_event, cx) => {
      this.inputFocused = true;
      cx?.notify?.();
    });
    this.input.on("blur", (_event, cx) => {
      this.inputFocused = false;
      cx?.notify?.();
    });
    this.input.focus();
    // A field focused in `init` is focused before the window has laid the dialog
    // out; the second look, once it exists, is what puts the cursor in it.
    cx.timer.after(300, () => this.input.focus());
    this.handle = cx.focus_handle();
  }

  /** Releases the field's state when the dialog goes away. */
  close() {
    this.input.release();
  }

  /** Runs the command with the typed name, trimmed; an empty name is Herdr's. */
  submit() {
    const wired = props();
    closeOverlay(wired?.overlay);
    if (!wired?.command?.name) return;
    run(wired.command.name, { ...(wired.command.args ?? {}), name: `${wired.prefix ?? ""}${this.text.trim()}` });
  }

  render() {
    const t = theme();
    const wired = props() ?? {};
    return v_flex()
      .id("prompt")
      .track_focus(this.handle)
      .on_key_down((event, cx) => {
        // Esc closes without saving; the app closes a dialog on Esc too, but the
        // field has the keyboard, so the dialog's own keys are handled here.
        if (event.key === "escape") {
          closeOverlay(wired.overlay);
          cx?.stop_propagation?.();
        }
      })
      .size_full()
      .min_h(0)
      .p(18)
      .bg(t.popover)
      .text_size(TEXT.body)
      .text_color(t.foreground)
      .child(div().text_size(TEXT.title).font_semibold().child(wired.title ?? ""))
      .child(
        div()
          .pt(16)
          .pb(4)
          .text_size(TEXT.label)
          .text_color(t.dim)
          .child(wired.label ?? "Name"),
      )
      .child(
        Input.new(this.input)
          .h(H.lg)
          .px(10)
          .rounded(RADIUS.control)
          .border_1()
          .border_color(this.inputFocused ? t.accent : t.input)
          .bg(t.background)
          .text_size(TEXT.body)
          .text_color(t.foreground),
      )
      .child(
        div()
          .h(20)
          .pt(4)
          .text_size(TEXT.label)
          .text_color(t.dim)
          .child(wired.hint ?? ""),
      )
      .child(div().flex_1().min_h(12))
      .child(
        dialogFooter(t, {
          onCancel: () => closeOverlay(wired.overlay),
          primary: {
            id: "prompt-save",
            label: wired.verb ?? "Save",
            onClick: () => this.submit(),
          },
        }),
      );
  }
}
