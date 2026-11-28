// The pane header: one copy per terminal pane, drawn by the layout's pane frame.
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/pane-chat/main.js` (the header branch: the program's icon and
//   name, the folder, the agent's status dot, split/zoom/close)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// In shprd a pane *is* a task's agent terminal, so the header names the task the
// pane belongs to (`shprd.task` label) and the agent's own state, which is what a
// person checks before they click into it. Herdr hands each copy `props().pane`
// and `props().terminal`; everything else comes from `panes()`.
//
// Right-clicking the header opens the pane's menu — Split right, Split down,
// Zoom, Send back to its parent (a kept sub-agent only), Close pane — which is
// Shepherdr's pane menu (`mods/menu-tasks/main.js:29-52`, ported in
// `lib/menu.js`). It opens at the pointer, at the size its own rows need, so a
// menu is never taller than what is in it.
//
// An agent's pane has Shepherdr's **Chat │ Terminal** switch at the right (M11 U4;
// `mods/pane-chat/main.js` `toChat` / `toTerminal`). Chat puts the `shprd-chat` mod
// in the agent pane's exact spot: it splits beside the pane, labels the agent's
// terminal with the chat's pane (`shprd.chat`), and parks the terminal (it keeps
// running, without a pane) — the park is last, because it removes this pane and
// this header with it. Terminal reverses it. A chat pane's ✕ ends its agent, after
// asking in the header's own words.
//
// The Autopilot switch is not here: shprd has no Autopilot.

import { View, div, image, svg } from "gpui-kit";
import { h_flex } from "gpui-base";
import { exec, hc, home, openOverlay, panes, props, run, setShared, shared, spaces, terminals, theme } from "herdr";

import { CHAT_MOD, PARK_LABEL, TERMINAL_KEY, agentLabel, chatLabel, endAgent, paneChat } from "./lib/chat.js";
import { menuHeight, MENU, paneMenu } from "./lib/menu.js";
import { button, iconFile, segmented } from "./lib/panel.js";
import { RADIUS, TEXT } from "./lib/tokens.js";

/** The shared key `shprd-chat` watches to show its card. */
const FOCUS_KEY = "shprd.chat.focus";

/**
 * Runs an app command and answers what it returned (`herdr-app do`): `run` gives
 * nothing back and a swap needs the new pane's id.
 */
async function app(name, args) {
  const r = await exec("herdr-app", [
    "do",
    name,
    ...Object.entries(args).flatMap(([k, v]) => [`--${k}`, typeof v === "string" ? v : JSON.stringify(v)]),
  ]);
  let out = {};
  try {
    out = JSON.parse(r.stdout || "{}");
  } catch {
    out = {};
  }
  if (r.code !== 0 || out.error) throw new Error(out.error || r.stderr.trim() || `${name} failed`);
  return out;
}

/** Under this width the header drops the word "Working…" and keeps its spinner. */
const COMPACT_BELOW = 360;

export default class Pane extends View {
  init(_props, cx) {
    this.cx = cx;
    this.terminalSeen = shared(TERMINAL_KEY)?.at ?? null;
    // The chat asks for its agent's terminal (`shprd.pane.terminal`, from its "Show
    // terminal" button); a header has no command of its own to be called, so it looks.
    cx.timer.every(500, (own) => this.takeTerminalRequest(own));
  }

  /** Switches this pane to Terminal when its chat asked for it. */
  takeTerminalRequest(cx) {
    const want = shared(TERMINAL_KEY);
    if (!want || want.at === this.terminalSeen) return;
    this.terminalSeen = want.at;
    if (Date.now() - want.at > 10_000) return;
    const wired = props();
    const self = panes().find((p) => p.pane === wired?.pane);
    const view = paneChat({ pane: self, terminals: terminals(), spaces: spaces() });
    // Only a pane with an agent parked behind it, in the asked-for space, acts: a
    // task's chat-only pane never takes the coordinator's request.
    if (view.chatMode && view.agent !== null && (view.space ?? "coordinator") === want.space) this.toTerminal(wired.pane, view, cx);
  }

  /** The folder as a person writes it: `~` for home and `~/a/b` under it. */
  short(folder) {
    if (!folder) return "";
    const root = String(home() ?? "").replace(/\/+$/, "");
    if (root && folder === root) return "~";
    return root && folder.startsWith(`${root}/`) ? `~${folder.slice(root.length)}` : folder;
  }

  /**
   * The pane's menu, opened over the header at the pointer.
   *
   * The rows come from `lib/menu.js` and the height is theirs, so the same list
   * is measured here and drawn by `shprd-menu`. *Send back* appears only when
   * the terminal has a parent: a pane that was parked under another one is the
   * only thing that can go back to it.
   */
  menu(wired) {
    const all = terminals();
    const term = all.find((entry) => entry?.address === wired?.terminal);
    const parent = term?.parent ? all.find((entry) => entry?.address === term.parent) : null;
    const rows = paneMenu({
      pane: wired?.pane ?? null,
      sendBack: parent ? { name: parent.name || parent.address, terminal: parent.address } : null,
    });
    if (rows.length === 0) return;
    openOverlay({
      mod: "shprd-menu",
      kind: "popup",
      width: MENU.pane,
      height: menuHeight(rows),
      props: {
        tab: "pane",
        target: { pane: wired?.pane ?? null, terminal: wired?.terminal ?? null },
      },
    });
  }

  /**
   * Terminal → Chat: the chat takes this agent pane's spot.
   *
   * Split beside the agent's pane with the chat mod (props name the space and the
   * terminal), label the terminal with the chat's pane so every window finds its
   * agent, focus the chat, then park the terminal — last, because parking removes
   * this pane and this header.
   */
  toChat(self, view, cx) {
    if (this.busy) return;
    this.busy = true;
    const terminal = self.terminal;
    cx.spawn(async (cx) => {
      try {
        const out = await app("pane.split", {
          direction: "right",
          pane: self.pane,
          mod: CHAT_MOD,
          props: { space: view.space, terminal },
        });
        if (!out.pane) throw new Error("Herdr didn't open the chat");
        await hc(["label", terminal, `${PARK_LABEL}=${chatLabel(out.pane)}`]);
        run("pane.focus", { pane: out.pane });
        await app("terminal.park", { terminal });
      } catch (err) {
        console.error(`shprd-pane: chat did not open: ${err?.message ?? err}`);
      }
      this.busy = false;
      cx.notify();
    });
  }

  /**
   * The Needs you pill: opens the chat (when the terminal is shown) and asks it to
   * scroll to its waiting card (`shprd.chat.focus`, read by `shprd-chat`).
   */
  toCard(self, view, cx) {
    if (view.space === null) return;
    setShared(FOCUS_KEY, { space: view.space, at: Date.now() });
    if (!view.chatMode && view.canSwitch) this.toChat(self, view, cx);
  }

  /**
   * Chat → Terminal: the agent's terminal gets a pane beside the chat, then the
   * chat closes, so the terminal is back in the same spot.
   */
  toTerminal(chat, view, cx) {
    const terminal = view.agent;
    const own = terminal ? panes().find((entry) => entry.terminal === terminal) : null;
    // Not running, or still in a pane of its own: just close the chat.
    if (!terminal || own) {
      run("pane.close", { pane: chat });
      if (terminal) hc(["label", terminal, `${PARK_LABEL}=`]);
      if (own) run("pane.focus", { pane: own.pane });
      return;
    }
    if (this.busy) return;
    this.busy = true;
    cx.spawn(async (cx) => {
      try {
        await app("pane.focus", { pane: chat });
        const out = await app("terminal.keep", { terminal, place: "right" });
        await hc(["label", terminal, `${PARK_LABEL}=`]);
        if (out.pane) run("pane.focus", { pane: out.pane });
        await app("pane.close", { pane: chat });
      } catch (err) {
        console.error(`shprd-pane: terminal did not come back: ${err?.message ?? err}`);
      }
      this.busy = false;
      cx.notify();
    });
  }

  /** Shows or hides the end-agent question in the header. */
  ask(on) {
    this.ending = on;
  }

  /**
   * The answer "End agent": the agent's terminal is killed, then the pane goes (a
   * chat pane's agent is the terminal parked under it; a terminal pane's is its own).
   */
  closeAgent(pane, view, self) {
    this.ending = false;
    // A chat-only pane (a task's chat with no agent behind it) closes itself only.
    if (view.chatMode && view.chatOnly) {
      run("pane.close", { pane });
      return;
    }
    const terminal = view.chatMode ? view.agent : self?.terminal;
    if (terminal) run("terminal.kill", { terminal });
    run("pane.close", { pane });
  }

  render(cx) {
    const t = theme();
    const wired = props();
    const self = panes().find((p) => p.pane === wired?.pane);
    const view = paneChat({ pane: self, terminals: terminals(), spaces: spaces() });
    // A chat pane wears its agent's header: the same name, folder and state.
    const agent = view.chatMode ? terminals().find((term) => term?.address === view.agent) : null;
    const pane = view.chatMode ? { ...(agent ?? {}), title: agent?.title, cwd: agent?.cwd ?? null } : self;
    const status = (view.chatMode ? agent?.status : (self?.agent?.status ?? self?.status)) ?? "idle";
    const program = view.chatMode ? agent?.program : self?.program;
    // An agent's × asks before it ends the agent; a shell's just closes its pane.
    const isAgent = (view.chatMode && !view.chatOnly) || Boolean(program?.agent);
    // shprd's own agent (a task's, the coordinator) says whose it is, as Shepherdr
    // labels its Assistant, not the program's name.
    const name =
      isAgent && view.space !== null
        ? agentLabel(program?.name)
        : view.chatMode
          ? (agent?.program?.name ?? "Chat")
          : (pane?.title ?? pane?.program?.name ?? "Terminal");
    const asking = isAgent && this.ending === true;
    const id = wired?.pane;
    const compact = (this.width ?? Infinity) < COMPACT_BELOW;
    return h_flex()
      .id(`pane-head-${id}`)
      .size_full()
      .px(10)
      .gap(8)
      .items_center()
      .bg(t.background)
      .border_b(1)
      .border_color(t.border)
      .text_size(TEXT.label)
      .text_color(t.muted)
      .on_hover((hovered, own) => {
        this.hovered = hovered;
        if (!hovered) this.ending = false;
        own?.notify?.();
      })
      .on_size_change((size, own) => {
        if (Math.round(size.width) !== this.width) {
          this.width = Math.round(size.width);
          own?.notify?.();
        }
      })
      .on_mouse_down("right", (_event, own) => {
        own?.stop_propagation?.();
        this.menu(wired);
      })
      .when(Boolean(program?.icon), (el) =>
        el.child(
          program.tint
            ? svg(program.icon).size(14).flex_none().text_color(t.dim)
            : image(program.icon).size(14).flex_none(),
        ),
      )
      .child(div().flex_none().max_w(260).min_w(0).truncate().font_medium().text_color(t.foreground).child(name))
      // The folder gives way first; the gap after it pushes the rest right.
      .child(div().flex_shrink_1().min_w(0).truncate().text_color(t.dim).child(this.short(pane?.cwd)))
      .child(div().flex_1())
      // The state in words, at the right with the controls: "Working…" and its
      // spinner, "Needs you" as a pill, nothing at rest.
      .when(status === "working", (el) =>
        el.child(
          h_flex()
            .flex_none()
            .gap(6)
            .items_center()
            .child(svg(iconFile("spinner")).size(12).flex_none().text_color(t.working).spin(900))
            .when(!compact, (inner) => inner.child(div().whitespace_nowrap().child("Working…"))),
        ),
      )
      .when(status === "blocked", (el) =>
        el.child(
          div()
            .id(`pane-needs-${id}`)
            .flex_none()
            .px(6)
            .h(18)
            .flex()
            .items_center()
            .rounded(RADIUS.control)
            .bg(`${t.blocked}26`)
            .text_size(TEXT.id)
            .font_semibold()
            .text_color(t.blocked)
            .cursor_pointer()
            // The pill takes you to what needs you: the chat, scrolled to its card.
            .on_click((_event, own) => {
              own?.stop_propagation?.();
              this.toCard(self, view, own);
              own?.notify?.();
            })
            .child("Needs you"),
        ),
      )
      // Ending an agent asks first, in place of the hover buttons.
      .when(asking, (el) =>
        el
          .child(div().flex_none().whitespace_nowrap().text_color(t.foreground).child(endAgent(name).ask))
          .child(
            button(t, `pane-end-no-${id}`, endAgent(name).cancel, {
              kind: "plain",
              onClick: () => this.ask(false),
            }),
          )
          .child(
            button(t, `pane-end-${id}`, endAgent(name).verb, {
              kind: "primary",
              onClick: () => this.closeAgent(id, view, self),
            }),
          ),
      )
      // The buttons show on hover only, and come before the switch so Chat │
      // Terminal never moves.
      .when(this.hovered === true && !asking, (el) =>
        el
          .child(this.action(t, `split-${id}`, "split-right", () => run("pane.split", { direction: "right", pane: id })))
          .child(this.action(t, `down-${id}`, "split-down", () => run("pane.split", { direction: "down", pane: id })))
          .child(this.action(t, `zoom-${id}`, self?.zoomed ? "unzoom" : "zoom", () => run("pane.zoom", { pane: id })))
          .child(
            this.action(t, `close-${id}`, "x", () =>
              isAgent ? this.ask(true) : run("pane.close", { pane: id }),
            ),
          ),
      )
      .when(view.canSwitch, (el) =>
        el.child(
          segmented(
            t,
            `pane-mode-${id}`,
            [
              ["chat", "Chat"],
              ["terminal", "Terminal"],
            ],
            view.chatMode ? "chat" : "terminal",
            (choice, _event, click) => (choice === "chat" ? this.toChat(self, view, click) : this.toTerminal(id, view, click)),
          ),
        ),
      );
  }

  /** One header button: a 24 px box with a 14 px line icon, and the command it runs. */
  action(t, id, name, onClick) {
    return h_flex()
      .id(`pane-${id}`)
      .size(24)
      .flex_none()
      .items_center()
      .justify_center()
      .rounded(RADIUS.control)
      .text_color(t.dim)
      .cursor_pointer()
      .hover((s) => s.bg(t.hover).text_color(t.foreground))
      .on_click((_event, own) => {
        own?.stop_propagation?.();
        onClick();
        own?.notify?.();
      })
      .child(svg(iconFile(name)).size(14));
  }
}
