// The New space dialog: a folder, Browse… and Create. It makes the space in
// Herdr (`space.new`) and says why when it cannot; a folder outside git is
// offered "Make it a git repo", on by default, as Shepherdr's is.
//
// Ported from Shepherdr's New space dialog (MIT; see `lib/newspace.js` for the
// source, paths and commit). Left out on purpose: another machine (shprd serves
// this computer's repos), the recent-folder suggestions and the icon picker.
//
// It opens on `props.folder` when given (a script's or a check's way to start it
// on a folder), else on the person's home.
//
// A space here is a place in Herdr. shprd's own per-repo space (`shprd · <repo>`,
// where the coordinator runs) is made by `shprd serve` for the repo it serves, so
// a space made here shows in the sidebar as a plain row until a service serves it.

import { View, div } from "gpui-kit";
import { Input, InputState, h_flex, v_flex } from "gpui-base";
import * as herdr from "herdr";

import { NEW_REPO, WORDS, assistantTabArgs, base, madeOf, offersRepo, pathOf, spaceNewArgs, tilde } from "./lib/newspace.js";
import { H, RADIUS, TEXT } from "./lib/tokens.js";

const { closeOverlay, exec, home, props, theme } = herdr;

export default class NewSpace extends View {
  init(_props, cx) {
    this.cx = cx;
    this.home = String(home() ?? "").replace(/\/+$/, "");
    this.exists = new Map();
    this.repo = new Map();
    this.makeRepo = true;
    this.busy = false;
    this.error = "";
    this.input = InputState.new({ placeholder: WORDS.placeholder, value: tilde(String(props()?.folder ?? this.home), this.home) });
    this.text = this.input.value();
    this.input.on("change", (_e, cx) => {
      const value = this.input.value();
      if (value === this.text) return;
      this.text = value;
      this.error = "";
      this.check(cx);
      cx.notify();
    });
    this.input.on("submit", (_e, cx) => this.create(cx));
    this.handle = cx.focus_handle();
    this.input.focus();
    cx.timer.after(300, () => this.input.focus());
    this.check(cx);
  }

  /** The folder the field names, once it is known whether it exists. */
  folder() {
    const path = pathOf(this.text, this.home);
    return path && typeof this.exists.get(path) === "boolean" ? path : null;
  }

  /** Asks once per path whether the folder exists and is inside a git repo. */
  check(cx) {
    const path = pathOf(this.text, this.home);
    if (!path || this.repo.has(path)) return;
    this.repo.set(path, undefined);
    cx.spawn(async (cx) => {
      const there = (await exec("test", ["-d", path]).catch(() => ({ code: 1 }))).code === 0;
      this.exists.set(path, there);
      if (!there) {
        this.repo.set(path, false);
        return cx.notify();
      }
      const git = await exec("git", ["rev-parse", "--is-inside-work-tree"], { cwd: path }).catch(() => null);
      this.repo.set(path, git ? git.code === 0 && git.stdout.trim() === "true" : null);
      cx.notify();
    });
  }

  /** Browse…: the system's folder picker, into the field. */
  browse(cx) {
    if (typeof herdr.pickFolder !== "function") return;
    cx.spawn(async (cx) => {
      const picked = await herdr.pickFolder({ prompt: "Choose" });
      if (picked) {
        this.exists.set(picked, true);
        this.text = tilde(picked, this.home);
        this.input.set_value(this.text);
        this.error = "";
        this.check(cx);
      }
      this.input.focus();
      cx.notify();
    });
  }

  close() {
    closeOverlay(props()?.overlay);
  }

  /** Makes the folder and the repo when asked to, then the space; closes on success. */
  create(cx) {
    const folder = this.folder();
    if (this.busy || !folder) return;
    this.busy = true;
    this.error = "";
    cx.notify();
    cx.spawn(async (cx) => {
      try {
        if (this.exists.get(folder) === false) {
          const made = await exec("mkdir", ["-p", "--", folder]);
          if (made.code !== 0) throw new Error(made.stderr.trim().split("\n").pop() || WORDS.cantMake(tilde(folder, this.home)));
          this.exists.set(folder, true);
        }
        if (offersRepo(folder, this.repo.get(folder), this.home) && this.makeRepo) {
          for (const args of NEW_REPO) {
            const done = await exec("git", args, { cwd: folder });
            if (done.code !== 0) throw new Error(WORDS.cantRepo(done.stderr.trim().split("\n").pop()));
          }
          this.repo.set(folder, true);
        }
        const out = await exec("herdr-app", spaceNewArgs(folder));
        let answer = {};
        try {
          answer = JSON.parse(out.stdout || "{}");
        } catch {
          answer = {};
        }
        if (out.code !== 0 || answer.error) {
          throw new Error(WORDS.cantSpace(String(answer.error ?? out.stderr ?? "").trim().split("\n").pop()));
        }
        await this.openAssistant(madeOf(answer));
        this.close();
        return;
      } catch (err) {
        this.error = String(err?.message ?? err);
      }
      this.busy = false;
      cx.notify();
    });
  }

  /**
   * Opens the new space on the assistant's chat view and closes the shell tab the
   * window made in it, at once: the chat says it is starting until the service is
   * up. Only the one tab `space.new` named is closed, and only after the assistant
   * tab exists, so the space is never left with none and a tab the person opens is
   * never touched. Best effort: a failure leaves the window's own tab, which
   * `shprd serve` tidies later.
   */
  async openAssistant(made) {
    if (!made) return;
    try {
      const tab = await exec("herdr-app", assistantTabArgs(made.id));
      if (tab.code !== 0 || !made.tab) return;
      await exec("herdr-app", ["do", "tab.close", "--id", made.tab, "--confirmed", "true"]);
    } catch {
      // The window's own tab stays; serve tidies it.
    }
  }

  /** A dialog button: plain Cancel, one solid primary. */
  button(t, id, label, { primary = false, disabled = false, onClick }) {
    return h_flex()
      .id(id)
      .h(H.lg)
      .flex_none()
      .px(primary ? 12 : 8)
      .items_center()
      .rounded(RADIUS.control)
      .text_size(primary ? TEXT.body : TEXT.label)
      .font_medium()
      .when(!disabled, (el) => el.cursor_pointer())
      .when(primary, (el) => el.bg(t.solid).text_color(t.solid_foreground).opacity(disabled ? 0.45 : 1))
      .when(primary && !disabled, (el) => el.hover((style) => style.opacity(0.88)))
      .when(!primary, (el) => el.text_color(t.muted).hover((style) => style.bg(t.hover).text_color(t.foreground)))
      .on_click((_e, cx) => {
        cx.stop_propagation();
        if (!disabled) onClick(cx);
      })
      .child(label);
  }

  render() {
    const t = theme();
    const field = pathOf(this.text, this.home);
    const folder = this.folder();
    const note = field && this.exists.get(field) === false ? WORDS.missing : "";
    const offered = offersRepo(folder, this.repo.get(folder), this.home);
    const line = h_flex().h(20).flex_none().gap(8).items_center().text_size(TEXT.label);
    const repoLine = offered
      ? line
          .id("make-repo")
          .cursor_pointer()
          .on_click((_e, cx) => {
            this.makeRepo = !this.makeRepo;
            cx.notify();
          })
          .child(div().flex_none().text_color(t.foreground).child(`${this.makeRepo ? "☑" : "☐"} ${WORDS.makeRepo}`))
          .child(div().min_w(0).truncate().text_color(t.dim).child(WORDS.makeRepoWhy))
      : folder && this.repo.get(folder) === true
        ? line.text_color(t.dim).child(div().min_w(0).truncate().child(WORDS.isRepo))
        : line;
    return v_flex()
      .id("newspace")
      .track_focus(this.handle)
      .on_key_down((e, cx) => {
        if (e.key !== "escape") return;
        this.close();
        cx.stop_propagation();
      })
      .size_full()
      .min_h(0)
      .p(18)
      .gap(8)
      .bg(t.popover)
      .text_size(TEXT.body)
      .child(div().h(22).flex_none().text_size(TEXT.title).font_semibold().text_color(t.foreground).child(WORDS.title))
      .child(div().h(16).flex_none().text_size(TEXT.label).text_color(t.dim).child(WORDS.label))
      .child(
        h_flex()
          .h(H.lg)
          .flex_none()
          .gap(8)
          .items_center()
          .child(
            Input.new(this.input)
              .flex_1()
              .min_w(0)
              .h(H.lg)
              .px(10)
              .items_center()
              .rounded(RADIUS.control)
              .border_1()
              .border_color(t.accent)
              .bg(t.background)
              .text_color(t.foreground),
          )
          .child(this.button(t, "browse", WORDS.browse, { onClick: (cx) => this.browse(cx) })),
      )
      .child(div().h(18).flex_none().text_size(TEXT.label).text_color(t.dim).child(note))
      .child(repoLine)
      .child(div().flex_1())
      .child(
        h_flex()
          .h(H.lg)
          .w_full()
          .flex_none()
          .gap(8)
          .items_center()
          .child(div().flex_1().min_w(0).truncate().text_size(TEXT.label).text_color(t.red ?? "#e5484d").child(this.error))
          .child(this.button(t, "dialog-cancel", WORDS.cancel, { onClick: () => this.close() }))
          .child(
            this.button(t, "dialog-primary", this.busy ? WORDS.creating : WORDS.create, {
              primary: true,
              disabled: !folder || this.busy,
              onClick: (cx) => this.create(cx),
            }),
          ),
      );
  }
}
