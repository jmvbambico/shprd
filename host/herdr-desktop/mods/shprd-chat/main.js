// The chat: one agent's conversation as bubbles, with the cards that are waiting
// for an answer in the timeline and a composer that sends a turn.
//
// It is mounted twice, because Shepherdr's chat is (`mods/chat/main.js`):
//
// - **in the task thread** (`props.thread`), where it is the thread's default
//   view. Block props are static, so the task it shows is the one the Tasks panel
//   shares (`tasks.thread`), and it follows that as the person moves between tasks;
// - **in a pane** (`props.space`, `props.terminal`), where `shprd-pane`'s
//   Chat · Terminal switch parked the agent's terminal and opened this in its
//   place. That is the coordinator's chat too: its pane has the same switch.
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/chat/main.js` (the bubble rows: the agent's words at the left on
//   `raised`, the person's at the right in an accent tint, markdown in a text view,
//   tool calls folded under the reply, "Show more" / "Show less" under a long
//   reply, the composer and its round send), `mods/chat/cards.js` (the question,
//   permission and plan cards drawn in the timeline), `core/chat-view.js` (the
//   fixed words and `literal`)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// What is shprd's own: the conversation is read from the agent's own record by the
// service (`GET /api/chat`) and a turn is typed into its pane by the service too
// (`POST /api/chat`); the cards are the service's asks and escalations
// (`GET /api/asks`, `GET /api/state`), answered through `POST /api/asks/answer`,
// exactly once. The panel never parses a transcript and never types into a pane.
// Markdown is drawn by the window's own text view, never as HTML: what an agent
// wrote is shown as text, and only a web link may be opened from it.

import { View, div, svg } from "gpui-kit";
import { Input, InputState, ScrollHandle, Textarea, TextareaState, TextView, h_flex, v_flex } from "gpui-base";
import { closeOverlay, exec, knownAgents, nextCommand, openOverlay, props, setShared, shared, theme } from "herdr";

import { RepoCache, SEND_PROBLEM, activeFolder, attach, downNotice, press, reason, singleFlight } from "./lib/client.js";
import * as files from "./attach.js";
import { Dictation, available as dictationAvailable } from "./dictate.js";
import { iconFile, popupAt, press as clickPress } from "./lib/panel.js";
import {
  MENU_W,
  PICK_KEY,
  coordinatorRunning,
  menuHeight,
  menusOf,
  pickLabel,
  priorInScope,
  roleSelection,
  selectionWrites,
} from "./lib/settings.js";
import { answeredAsks, errorLine, noticeLine, overlay, probe, read as readOps } from "./lib/optimistic.js";
import { answerBody, cards as cardsOf, footer, forSpace } from "./lib/asks.js";
import {
  COORDINATOR,
  GUTTER,
  bubbleSpan,
  WORDS,
  emptyWords,
  coordinatorDown,
  echoOf,
  empty,
  firstCard,
  keyAction,
  lineCount,
  fold,
  foldOlder,
  folding,
  markdownOf,
  mine,
  nextCursor,
  normalize,
  outgoing,
  safeLink,
  settleEchoes,
  spaceOf,
  status,
  timeline,
  toolLine,
  toolsSummary,
  when,
  withEchoes,
} from "./lib/chat.js";
import { RADIUS, TEXT, tone } from "./lib/tokens.js";
import { followChanges } from "./lib/watch.js";

/** How often the chat reads the conversation when nothing wakes it, ms. */
const FLOOR_MS = 3000;

/** The longest one read of the service may take before the chat says so, ms. */
const READ_MS = 20000;

/** The longest "Loading…" is drawn for a conversation, ms: after it, the empty chat or the reason. */
const LOADING_MS = 4000;

/** How many pages one refresh takes before leaving the rest to the next look. */
const PAGES = 4;

/** How long the model menu is quiet before its choice is written (it answers on each change), ms. */
const PICK_SETTLE_MS = 1200;

/** How long a restart the person asked for keeps the conversation, ms. */
const KEEP_MS = 10 * 60 * 1000;

/** A second Enter this close to the first is the same press (the key and the chord's submit), ms. */
const ENTER_DEBOUNCE_MS = 120;

/** Where a restart's conversation waits for the chat pane the restart opens (the old pane is closed with the agent). */
const CARRY_KEY = "shprd.chat.carry";

/** The round buttons in the composer's bottom row, px. */
const ROUND = 28;


export default class Chat extends View {
  init(_props, cx) {
    this.cx = cx;
    /** The pane's width as the window reports it; Shepherdr starts at 600 too. */
    this.width = 600;
    this.thread = props()?.thread === true;
    this.fixed = props()?.space ?? null;
    this.space = this.wanted();
    this.chat = empty(this.space ?? COORDINATOR);
    // Each conversation (repository + agent) is kept for the window's life, so
    // coming back to one shows it at once and the read only adds what is new; one
    // never read says "Loading…", not "No messages yet".
    this.keys = new RepoCache();
    this.chats = new Map();
    this.loaded = new Set();
    this.slot = this.slotKey();
    this.slotSince = Date.now();
    this.state = null;
    this.asks = null;
    this.client = null;
    this.watching = false;
    this.failure = null;
    // A refused send keeps its words and its reason until the next send: the
    // reads that follow would otherwise clear the line before a person read it.
    this.refusal = null;
    /** What shprd did first on the last send (made the folder a repository). */
    this.notice = null;
    this.sending = false;
    /** Messages opened with "Show more", by key. */
    this.opened = new Set();
    /** The card whose free-text box is open, the card with an answer in flight,
     * and what the service said to a refused answer. */
    this.typing = null;
    this.answering = null;
    this.answerRefusal = null;
    this.draft = null;
    this.scroll = ScrollHandle.new();
    this.pinned = "bottom";
    this.drawn = { rows: 0, cards: 0 };
    this.loading = false;
    this.loadedAt = Date.now();
    // A text area, because the person pastes whole prompts: Enter sends, Shift+Enter
    // is a new line, and the line breaks they typed reach the agent.
    this.input = TextareaState.new({ placeholder: this.placeholder() });
    this.input.set_rows?.(1);
    this.inputRows = 1;
    // The text area's own `submit` is Ctrl/Cmd+Enter only: a plain Enter never reached
    // it and was a new line (SHPR-84). Enter is taken before the text area sees it, in
    // the composer's `on_key_down`; this stays for the chord, and `enter` makes the two
    // never send twice.
    this.input.on("submit", (e) => {
      if (!e?.shift) this.enter();
    });
    this.input.on?.("focus", (_e, cx) => {
      this.boxFocused = true;
      cx?.notify?.();
    });
    this.input.on?.("blur", (_e, cx) => {
      this.boxFocused = false;
      cx?.notify?.();
    });
    // A paste with no text (an image, copied files) becomes attachments.
    this.input.on?.("paste_other", (_e, cx) => cx.spawn(async (cx2) => {
      try {
        const got = await files.fromClipboard();
        if (got.length > 0) this.addFiles(got);
      } catch (err) {
        this.fileError = String(err?.message ?? err);
      }
      cx2.notify();
    }));
    /** What the person attached to the message being written: `{ path, name, image, temp }`. */
    this.files = [];
    this.fileError = null;
    this.boxFocused = false;
    this.enterAt = 0;
    this.stopping = false;
    // Dictation: the mic is drawn only where Shepherdr's recorder is installed.
    this.dict = new Dictation("chat");
    this.micOn = false;
    this.level = 0;
    dictationAvailable().then((ok) => {
      this.micOn = ok;
      cx.notify();
    });
    // The model picker: the settings the coordinator runs on and the roster of what it
    // may run, read from the service (Settings → Agents → Assistant reads the same).
    this.doc = null;
    this.roster = null;
    this.menus = null;
    this.icons = {};
    this.rosterAsked = false;
    this.pickRec = { at: null };
    this.popup = null;
    this.lastReply = shared(PICK_KEY)?.at ?? null;
    this.picked = null;
    this.switching = null;
    /** Why the next new session continues this conversation (a restart the person asked for). */
    this.keep = null;
    this.refresh();
    cx.timer.every(FLOOR_MS, (cx) => this.refresh(cx));
    // The app's way into a focusable mod (`herdr-app do shprd-chat.<command>`, the palette, a
    // key): one command at a time. A live check has no pointer or keyboard for the window, so
    // these press the composer's own handlers by name; each is the function the key or the
    // click calls, never another path.
    cx.spawn(async (cx2) => {
      for (;;) {
        const command = await nextCommand();
        if (typeof command === "string") this.command(command, cx2);
      }
    });
  }

  /**
   * One command by name: `focus` (the keyboard goes to the box), `type:<words>` (as typing),
   * `enter` (what the Enter key calls), `send` (what the send button calls), `attach:<path>`,
   * `menu` (the model menu), `pick:<agent>:<model>[:<effort>]` (what the menu's answer does),
   * `stop`, `mic`.
   */
  command(command, cx) {
    const [name, ...rest] = command.split(":");
    const arg = rest.join(":");
    if (name === "focus") this.input.focus();
    else if (name === "type") this.input.replace(arg);
    else if (name === "enter") this.enter();
    else if (name === "send") this.send();
    else if (name === "attach") this.addFiles(files.entries([arg]));
    else if (name === "menu") this.openModelMenu(this.pickRec.at ?? null);
    else if (name === "pick") {
      const [agent, model, effort] = arg.split(":");
      this.picked = { value: { agent, model, effort: effort || null, fast: false, perm: null }, stamp: 0 };
      this.applyPick();
    } else if (name === "stop") this.stop();
    else if (name === "mic") this.toggleDictation();
    cx?.notify?.();
  }

  /** The space to show now: the thread's task (it moves), a pane's own, or the coordinator's. */
  wanted() {
    return spaceOf({ props: { ...props(), space: this.fixed, thread: this.thread }, thread: shared("tasks.thread") });
  }

  /** The composer's hint, by whose chat this is. */
  placeholder() {
    return this.space === COORDINATOR || this.space === null ? "Message the coordinator" : "Message the agent";
  }

  /**
   * The thread follows the task the Tasks panel has open. A different task is a
   * different conversation, so what was read is dropped and the first page asked
   * for again; the newest card, if any, is what it opens on.
   */
  follow_space() {
    const next = this.wanted();
    // Opening the same task again (a needs-you row, a toast) is a new open: it
    // lands on the card again.
    const at = this.thread ? (shared("tasks.thread")?.at ?? null) : null;
    if (next === this.space) {
      if (at !== this.openedAt) {
        this.openedAt = at;
        this.pinned = "card";
      }
      return;
    }
    this.openedAt = at;
    this.space = next;
    this.forget();
    this.pinned = "card";
    this.refresh();
  }

  /**
   * Reads the pages after the cursor and the cards, then shows them.
   *
   * A service that restarted has a new `id`, and its agent's record is a new one
   * too: `attach` notices the restart and the chat starts over from its first
   * page instead of asking a new service for a cursor it never wrote (M10 gate
   * bug 5). A read that answers nothing is a failure, so a dead port is looked
   * for again rather than kept.
   */
  refresh(cx = this.cx) {
    // One read at a time: a refresh asked for while another runs is folded into
    // one more run after it, never started beside it.
    return singleFlight(this, () => this.bounded(cx));
  }

  /**
   * One read that cannot hang the chat: a call that never settles would keep the
   * one read in flight for ever (and with it "Loading…"), so after [`READ_MS`] the
   * read is given up as a failure, the client is forgotten and the next beat
   * looks again.
   */
  async bounded(cx) {
    const start = Date.now();
    // Each read has a generation; a read given up on is stale and changes nothing
    // if it ever finishes (a newer read owns the state by then).
    this.gen = (this.gen ?? 0) + 1;
    const read = this.readOnce(cx, this.gen).then(() => "done");
    // A clock that fires early (a stub's) is not a read that took too long.
    const gave = (cx?.sleep?.(READ_MS) ?? new Promise(() => {})).then(() => (Date.now() - start >= READ_MS - 100 ? "late" : "early"));
    let outcome = await Promise.race([read, gave]);
    if (outcome === "early") outcome = await read;
    if (outcome === "late") {
      this.gen += 1;
      this.client = null;
      this.failure = downNotice();
      cx.notify();
    }
  }

  /** One read of the service and one repaint; [`refresh`] is what callers use. */
  async readOnce(cx, gen = this.gen) {
    const space = this.space;
    const stale = () => gen !== this.gen;
    try {
      const next = await attach(this.client, { wait: (ms) => cx.sleep(ms) });
      if (stale()) return;
      if (!next.client) {
        this.client = null;
        this.failure = downNotice();
        cx.notify();
        return;
      }
      if (next.restarted) {
        // The same service shown for another repository keeps every conversation;
        // a service that restarted wrote none of the cursors held.
        const sameService = this.client !== null && this.client.id() === next.client.id();
        this.forget(!sameService);
      }
      this.client = next.client;
      this.follow();
      this.carry();
      // The conversation this read is for: a switch of space while it runs makes
      // its answer another conversation's, which is not folded into this one.
      if (this.slot !== undefined && this.slotKey() !== this.slot) this.forget(false);
      const slot = this.slot;
      // The conversation first: it is what the person is looking at. The board,
      // the asks and the model are read beside each other afterwards, not in series.
      if (space !== null) {
        // `more` means the page filled up: a long conversation arrives over a few
        // calls rather than one slow one, and the next timer catches any left over.
        for (let page = 0; page < PAGES; page += 1) {
          if (!(await this.pull(space, slot, stale))) return;
          if (!this.chat.more) break;
        }
        cx.notify();
      }
      const [board, asks] = await Promise.all([
        this.client.state(),
        this.client.asks(),
        this.readModel(cx, space),
      ]);
      if (stale()) return;
      this.state = board;
      this.asks = asks;
      this.failure = null;
      if (space === this.space && slot === this.slot) this.loaded?.add(slot);
    } catch (err) {
      if (stale()) return;
      // A restarted service answers with another id: forget the client so the
      // next look finds the new port instead of dialling a dead one.
      this.client = null;
      this.failure = reason(err);
    }
    cx.notify();
  }

  /**
   * Reads one page of the conversation and folds it in. It answers `false` when the
   * answer is not for the conversation shown any more (another task, a newer read).
   *
   * A page is only folded when the cursor it was asked from is still the chat's:
   * a read after a send runs beside the timer's, and the same page folded twice
   * would show every message twice.
   */
  async pull(space, slot, stale = () => false) {
    const asked = nextCursor(this.chat);
    const answer = await this.client.call(
      "GET",
      `/api/chat?space=${encodeURIComponent(space)}&since=${encodeURIComponent(asked)}`,
      { strict: true },
    );
    // The person moved to another task while this was in flight: this answer
    // is for the one they left.
    if (space !== this.space || slot !== this.slot || stale()) return false;
    if (nextCursor(this.chat) !== asked) return true;
    const was = this.chat.session;
    const keep = this.keep ?? null;
    this.chat = fold(this.chat, answer, keep !== null && Date.now() < keep.until ? keep.text : null);
    if (keep !== null && (this.chat.session !== was || Date.now() >= keep.until)) this.keep = null;
    // A turn the transcript now holds is no longer an echo; a restart of the agent
    // starts another conversation, which no echo of the old one belongs to.
    // Only this conversation's echoes: another task's echo is not settled by this one's lines.
    const here = (this.echoes ?? []).filter((echo) => echo.slot === slot);
    const others = (this.echoes ?? []).filter((echo) => echo.slot !== slot);
    this.echoes = [...others, ...settleEchoes(here, answer?.messages ?? [])];
    if (this.chat.session !== was && was !== null) this.echoes = (this.echoes ?? []).filter((echo) => echo.slot !== slot);
    return true;
  }

  /**
   * Reads the page of messages before the oldest shown and puts it in front.
   * One at a time; the forward cursor and the echoes are not touched, and a page
   * that arrives after the person moved on is dropped.
   */
  async loadOlder() {
    const space = this.space;
    const slot = this.slot;
    const older = this.chat?.older;
    if (space === null || !this.client || !older || this.loadingOlder) return;
    this.loadingOlder = true;
    try {
      const answer = await this.client.call(
        "GET",
        `/api/chat?space=${encodeURIComponent(space)}&since=${encodeURIComponent(nextCursor(this.chat))}&before=${encodeURIComponent(older)}`,
        { strict: true },
      );
      if (space !== this.space || slot !== this.slot || this.chat?.older !== older) return;
      this.chat = foldOlder(this.chat, answer);
      this.cx.notify();
    } catch {
      // The timer's read reports a service that is down; the row stays to retry.
    } finally {
      this.loadingOlder = false;
    }
  }

  /**
   * After a send the transcript may not hold the turn yet: read just the chat page
   * (not the board and the asks) soon, and again a little later, until the echo is
   * confirmed. The 3 s timer is only the fallback.
   */
  follow_up(delays = [120, 300, 700, 1000]) {
    const slot = this.slot;
    const space = this.space;
    if (space === null || !this.client) return;
    // Fast only while an answer is awaited: the echo is not yet in the record, or
    // the newest message is still the person's (the agent has not replied).
    const waiting = () =>
      (this.echoes ?? []).some((echo) => echo.slot === slot && echo.sent) ||
      (this.chat?.messages ?? []).at(-1)?.role === "user";
    const until = Date.now() + 90_000;
    const step = async (at) => {
      if (Date.now() > until || !waiting() || slot !== this.slot || !this.client) return;
      try {
        if (await this.pull(space, slot)) this.cx.notify();
      } catch {
        // The timer's read reports a service that is down.
      }
      if (waiting()) this.cx.timer?.after?.(delays[Math.min(at + 1, delays.length - 1)], () => step(at + 1));
    };
    this.cx.timer?.after?.(delays[0], () => step(0));
  }

  /** The echoes of the conversation shown. */
  myEchoes() {
    return (this.echoes ?? []).filter((echo) => echo.slot === this.slot);
  }

  /**
   * A restart the person asked for (the model menu) closes the coordinator's pane and opens
   * a new one, so the conversation is handed over in shared state: this takes it, once, into
   * an empty chat of the same folder, followed by the line saying why it changed hands.
   */
  carry() {
    if (this.space !== COORDINATOR || (this.chat?.messages ?? []).length > 0) return;
    const held = shared(CARRY_KEY);
    if (!held || Date.now() - held.at > KEEP_MS || held.folder !== (this.client?.folder ?? null)) return;
    setShared(CARRY_KEY, null);
    const note = normalize({ role: "news", text: String(held.text ?? ""), tools: [], at_ms: held.at });
    this.chat = { ...this.chat, messages: [...(held.messages ?? []), ...(note.text === "" ? [] : [note])] };
  }

  /** Starts the conversation again: a new record names nothing the old cursor did. */
  forget(reset = false) {
    const now = this.slotKey();
    if (this.chats) {
      if (this.slot !== undefined) this.chats.set(this.slot, { chat: this.chat, opened: this.opened });
      if (reset) {
        this.chats.clear();
        this.loaded.clear();
      }
    }
    const kept = this.chats?.get(now) ?? null;
    this.slot = now;
    this.slotSince = Date.now();
    this.chat = kept?.chat ?? empty(this.space ?? COORDINATOR);
    this.opened = kept?.opened ?? new Set();
    this.refusal = null;
    this.answerRefusal = null;
    this.typing = null;
    this.sending = false;
  }

  /** The key of the conversation shown: the repository's folder and the agent's space. */
  slotKey() {
    return `${this.keys?.key() ?? ""}|${this.space ?? ""}`;
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
   * The settings the coordinator runs on, and (once) the roster of programs and models
   * it may run: what the model button shows and its menu lists. Only the coordinator's
   * chat has them; a task's agent is the task's, not a setting of the chat.
   */
  async readModel(cx, space) {
    if (space !== COORDINATOR || !this.client) return;
    try {
      this.doc = await this.client.settings();
    } catch {
      // The button keeps what it had.
    }
    if (this.rosterAsked) return;
    this.rosterAsked = true;
    try {
      this.roster = await this.client.roster();
      this.menus = menusOf(this.roster);
    } catch {
      this.rosterAsked = false;
    }
    try {
      for (const entry of (await knownAgents()) ?? []) {
        const id = entry.key ?? (entry.run ?? [])[0];
        if (id && entry.icon) this.icons[id] = { icon: entry.icon, tint: Boolean(entry.tint) };
      }
    } catch {
      // Words only.
    }
    cx?.notify?.();
  }

  /** What the coordinator runs on now: `{ agent, model, effort, fast, perm }`. */
  selection() {
    return this.doc ? roleSelection(this.doc, "coordinator") : null;
  }

  /** The model button's words: "Opus 5.5 · Medium", what is being switched to, or the program. */
  modelWords() {
    if ((this.switching ?? null) !== null) return `Switching to ${this.switching}…`;
    const sel = this.selection();
    if (sel?.agent && this.menus) return pickLabel(this.menus, sel) || sel.agent;
    return sel?.agent ?? this.chat.program ?? "Model";
  }

  /** Opens the model menu (`shprd-models`, the one Settings uses) over the button. */
  openModelMenu(at) {
    const sel = this.selection();
    if (!sel || !this.menus || (this.switching ?? null) !== null) return;
    this.closePopup();
    const height = menuHeight(this.menus);
    const place = popupAt(at, MENU_W, height);
    this.popup = openOverlay({
      mod: "shprd-models",
      kind: "popup",
      frame: "none",
      width: MENU_W,
      height,
      at: place,
      props: {
        mode: "models",
        key: "chat:coordinator",
        current: sel,
        menus: this.menus,
        icons: this.icons,
        width: MENU_W,
        height,
        below: place !== "pointer" && at !== null && place.y > at.y,
        light: false,
        allowNone: false,
        taken: [],
        blocked: {},
        blockedModels: {},
      },
    });
  }

  /** Puts the open model menu away. */
  closePopup() {
    if (this.popup === null) return;
    try {
      closeOverlay(this.popup);
    } catch {
      // Already gone: a click beside it closed it.
    }
    this.popup = null;
  }

  /**
   * Takes what the model menu answered. The menu answers on every change (a model, then
   * a level), so the switch waits until the answers stop, then writes them once and
   * restarts the coordinator once. Answers of the Settings window are not this chat's.
   */
  takeReply() {
    const reply = shared(PICK_KEY);
    if (!reply || reply.at === (this.lastReply ?? null) || typeof reply.key !== "string") return;
    this.lastReply = reply.at;
    if (reply.key !== "chat:coordinator" || reply.field !== "selection" || !reply.value) return;
    this.pickSeq = (this.pickSeq ?? 0) + 1;
    const stamp = this.pickSeq;
    this.picked = { value: reply.value, stamp };
    this.cx.timer.after(PICK_SETTLE_MS, () => {
      if (this.picked?.stamp === stamp) this.applyPick();
    });
  }

  /**
   * Switches the coordinator to the picked choice: the same keys Settings → Agents →
   * Assistant writes, for this repository (the service serves one: this space), then a
   * restart, because a running agent takes a new model only that way. The conversation
   * stays on screen with one line saying where it changed hands. A refused write puts
   * back the ones before it.
   */
  async applyPick() {
    const after = this.picked?.value;
    this.picked = null;
    const before = this.selection();
    if (!after || !before || !this.client) return;
    const writes = selectionWrites("coordinator", before, after);
    if (writes.length === 0) return;
    const label = pickLabel(this.menus, after) || after.model || after.agent;
    this.switching = label;
    this.refusal = null;
    this.cx.notify();
    const done = [];
    try {
      for (const write of writes) {
        const prior = priorInScope(this.doc, write.key, true);
        await this.client.putSetting(write.key, write.value, true);
        done.push({ key: write.key, prior });
      }
      this.doc = await this.client.settings();
      if (coordinatorRunning(this.state)) {
        this.keep = { text: `Switched the coordinator to ${label}`, until: Date.now() + KEEP_MS };
        setShared(CARRY_KEY, { folder: this.client.folder ?? null, at: Date.now(), text: this.keep.text, messages: this.chat.messages });
        await this.client.restartCoordinator();
      }
    } catch (err) {
      this.refusal = reason(err);
      for (const write of done.reverse()) {
        try {
          await this.client.putSetting(write.key, write.prior, true);
        } catch {
          // Leave it: the Settings window shows what is stored.
        }
      }
      this.keep = null;
    }
    this.switching = null;
    this.cx.notify();
    this.refresh();
  }

  /** Adds files to the message being written (a path once). */
  addFiles(list) {
    for (const file of list) {
      if (!this.files.some((held) => held.path === file.path)) this.files.push(file);
    }
    this.fileError = null;
  }

  /** Takes one file off the message. */
  removeFile(index) {
    const [gone] = this.files.splice(index, 1);
    if (gone) files.forget(gone);
    this.cx.notify();
  }

  /** The system's file picker, for the + button. */
  async pickFiles() {
    try {
      const chosen = await files.pick();
      if (chosen.length > 0) this.addFiles(chosen);
    } catch (err) {
      this.fileError = String(err?.message ?? err);
    }
    this.input.focus();
    this.cx.notify();
  }

  /** Enter: sends, once however many ways the key reached here. */
  enter() {
    if (Date.now() - this.enterAt < ENTER_DEBOUNCE_MS) return;
    this.enterAt = Date.now();
    this.send();
  }

  /** The agent shown, when the service lists it as running: the coordinator, or the task's worker. */
  liveAgent() {
    const list = Array.isArray(this.state?.live_agents) ? this.state.live_agents : [];
    return (
      list.find((agent) =>
        !agent.ended_ms && (this.space === COORDINATOR || this.space === null ? agent.role === "coordinator" : agent.task === this.space),
      ) ?? null
    );
  }

  /** Whether the agent shown is working (Esc and the Stop button stop it). */
  working() {
    return this.liveAgent()?.state === "working";
  }

  /** Presses Escape in the agent's pane: what every program takes as "stop". */
  async stop() {
    if (!this.client || this.stopping) return;
    this.stopping = true;
    this.cx.notify();
    try {
      await this.client.call("POST", "/api/chat/stop", { body: { space: this.space ?? COORDINATOR } });
    } catch (err) {
      this.refusal = reason(err);
    }
    this.stopping = false;
    this.cx.notify();
    this.refresh();
  }

  /** The mic: starts a dictation, or ends it and puts its words in the box. */
  toggleDictation() {
    if (this.dict.state === "idle") {
      this.dict.start();
      this.cx.notify();
      this.cx.spawn(async (cx) => {
        while (this.dict.state === "recording") {
          this.level = await this.dict.level();
          cx.notify();
          await cx.sleep(150);
        }
        cx.notify();
      });
      return;
    }
    if (this.dict.state === "recording") this.finishDictation();
  }

  /** Ends the recording and writes it into the box to edit; nothing is sent by itself. */
  async finishDictation() {
    this.cx.notify();
    const words = await this.dict.stop();
    if (words.trim() !== "") {
      const have = this.input.value();
      this.input.replace(`${have !== "" && !/\s$/.test(have) ? " " : ""}${words.trim()} `);
    }
    this.input.focus();
    this.cx.notify();
  }

  /**
   * Types the composer's words, and any files, into the agent, as the person.
   *
   * The turn shows in the chat and the composer empties before the service has
   * answered (the optimistic echo); the transcript's own copy replaces the echo
   * when it is read back. A refusal takes the echo away and gives the words and
   * files back to the composer, with the reason.
   */
  async send() {
    const body = outgoing(this.space, this.input.value(), this.files ?? []);
    if (body === null) return;
    if (!this.client) {
      this.refusal = downNotice();
      this.cx.notify();
      return;
    }
    const slot = this.slot;
    const taken = this.files ?? [];
    const typed = this.input.value();
    const echo = { ...echoOf(body), slot };
    this.echoes = [...(this.echoes ?? []), echo];
    this.inflight = (this.inflight ?? 0) + 1;
    this.sending = true;
    this.refusal = null;
    this.input.set_value("");
    this.files = [];
    this.fileError = null;
    this.pinned = "bottom";
    this.cx.notify();
    try {
      const reply = await this.client.call("POST", "/api/chat", { body });
      echo.sent = true;
      // Sent: the service holds its own copies, so a pasted image's temporary file goes.
      for (const file of taken) files.forget(file);
      // One plain line about what shprd did first (it made the folder a git
      // repository); it stays above the composer for this conversation.
      if (typeof reply?.notice === "string" && reply.notice !== "") this.notice = reply.notice;
      setShared(SEND_PROBLEM, null);
    } catch (err) {
      // The echo goes, and the words stay in the composer: a refused line is retyped, not lost.
      this.echoes = (this.echoes ?? []).filter((held) => held !== echo);
      if (slot === this.slot) {
        // Words typed while the send was in flight stay; the refused words go back before them.
        const now = this.input.value();
        this.input.set_value(now.trim() === "" ? typed : `${typed}\n${now}`);
        this.files = [...taken, ...(this.files ?? [])];
      }
      const said = reason(err);
      // Only the conversation the send came from shows the refusal; the sidebar's
      // foot says the sentence for the folder.
      if (slot === this.slot) this.refusal = said;
      setShared(SEND_PROBLEM, { folder: this.client?.folder ?? activeFolder(), text: said });
    }
    this.inflight -= 1;
    this.sending = this.inflight > 0;
    this.cx.notify();
    if (echo.sent) this.follow_up();
    this.refresh();
  }

  /** Opens or folds one long reply. */
  toggle(key) {
    if (this.opened.has(key)) this.opened.delete(key);
    else this.opened.add(key);
    this.cx.notify();
  }

  /** Opens a link a bubble holds: web pages only, through the system. */
  link(url) {
    if (!safeLink(url)) return;
    exec("open", [url]).catch(() => exec("xdg-open", [url]).catch(() => {}));
  }

  /** Opens the free-text box on a card, making the one input on first use. */
  type(card) {
    this.typing = this.typing === card.id ? null : card.id;
    if (this.typing !== null && !this.draft) {
      this.draft = InputState.new({ placeholder: "Type your answer", value: "" });
    }
    this.answerRefusal = null;
    this.cx.notify();
    // Opening the box gives it the keyboard: without this the keys keep going to
    // the composer that had it. The box is drawn by the render this notify asks
    // for, so the focus is taken again once it is on screen (a mod has no
    // `setTimeout`; the app's timer is the way, as the add-task form does it).
    if (this.typing !== null) {
      const id = this.typing;
      this.draft.focus();
      this.cx.timer?.after?.(50, () => {
        if (this.typing === id) this.draft?.focus();
      });
    }
  }

  /** Answers one card; the service takes it exactly once. */
  async answer(card, text) {
    const body = answerBody(card, text);
    if (!body || !this.client || this.answering !== null) return;
    this.answering = card.id;
    this.answerRefusal = null;
    this.cx.notify();
    // The card is gone and its task resumes at once; a refusal puts it back.
    const remaining = (this.asks?.asks ?? []).filter((ask) => ask.task === body.task).length;
    const sent = await press(this, {
      kind: "ask.answer",
      tasks: [body.task],
      extra: { ask: body.ask, remaining },
      send: () => this.client.answerAsk(body.task, body.ask, body.answer),
    });
    if (sent.ok) {
      this.typing = null;
      this.draft?.set_value?.("");
      this.answering = null;
      await this.refresh();
      return;
    }
    if (!sent.ignored) this.answerRefusal = { card: card.id, text: sent.reason };
    this.answering = null;
    this.cx.notify();
  }

  /** Runs the one action the service has for an escalation, when it has one. */
  async act(card) {
    if (!card.action || !this.client || this.answering !== null) return;
    this.answering = card.id;
    this.answerRefusal = null;
    this.cx.notify();
    try {
      await this.client.action(card.action);
      this.answering = null;
      await this.refresh();
      return;
    } catch (err) {
      this.answerRefusal = { card: card.id, text: reason(err) };
    }
    this.answering = null;
    this.cx.notify();
  }

  /**
   * Reads again when the chat is drawn with an old answer.
   *
   * A block that is hidden in a stack (the thread's chat, while its terminal is
   * shown) is not guaranteed to keep the timer `init` started, so being drawn is
   * also when a stale chat catches up. `loading` keeps one read in flight and the
   * floor keeps a repaint from becoming a poll.
   */
  revalidate() {
    if (this.loading || Date.now() - this.loadedAt < FLOOR_MS) return;
    this.loading = true;
    this.refresh().finally(() => {
      this.loading = false;
      this.loadedAt = Date.now();
    });
  }

  render() {
    if (this.thread || this.fixed === null) this.follow_space();
    // Another repository's space: its conversation as last read is drawn now and
    // the read adds what is new (the same service, so its cursors still hold).
    if (this.slot !== undefined && this.slotKey() !== this.slot) {
      this.forget(false);
      this.refresh();
    }
    this.revalidate();
    this.takeReply();
    const t = theme();
    const colors = tone(t);
    // What the person just pressed shows at once: the board with the pending actions
    // applied, and the questions they answered already gone (`tasks.optimistic`).
    const folder = this.client?.folder ?? activeFolder();
    const board = overlay(this.state, folder, readOps().ops);
    probe("Chat", board);
    const gone = answeredAsks(folder);
    const open = this.asks ? { ...this.asks, asks: (this.asks.asks ?? []).filter((ask) => !gone.has(`${ask.task}/${ask.id}`)) } : this.asks;
    const list = this.space === null ? [] : forSpace(cardsOf(board, open), this.space);
    const rows = timeline(withEchoes(this.chat, this.myEchoes()), list);
    this.reveal(rows, list);
    const said =
      this.refusal ??
      this.failure ??
      errorLine(board) ??
      noticeLine(board) ??
      coordinatorDown(this.space, this.state) ??
      (rows.length === 0 ? status(this.chat) : null);
    return v_flex()
      .size_full()
      .bg(t.background)
      .on_size_change((size, cx) => {
        const w = size?.width ?? 0;
        if (Math.abs(w - this.width) > 1) {
          this.width = w;
          cx?.notify?.();
        }
      })
      .child(
        v_flex()
          .id("chat-rows")
          .flex_1()
          .min_h(0)
          .overflow_y_scrollbar()
          .track_scroll(this.scroll)
          .children(
            rows.length === 0
              ? [this.blank(t, said)]
              : [...rows.map((row) => this.row(t, colors, row)), div().h(10).flex_none()],
          ),
      )
      .when(this.space === COORDINATOR && Boolean(this.notice), (el) =>
        el.child(this.column(div().pb(4).text_size(TEXT.label).text_color(t.muted).child(this.notice))),
      )
      .when(said !== null && rows.length > 0, (el) =>
        el.child(this.column(div().pb(4).text_size(TEXT.label).text_color(t.muted).child(said))),
      )
      .when((this.fileError ?? null) !== null, (el) =>
        el.child(this.column(div().pb(4).text_size(TEXT.label).text_color(colors.red).child(this.fileError))),
      )
      .when(this.working(), (el) => el.child(this.workingLine(t)))
      .child(this.composer(t, colors));
  }

  /**
   * Scrolls to what the person came for. A new task opens on its first waiting
   * card (the "needs you" row and the toast land there), anything else on the
   * newest row; a card that arrived while it was shown is looked at too.
   */
  reveal(rows, list) {
    const cards = list.length;
    const grew = rows.length > this.drawn.rows || cards > this.drawn.cards;
    if (!grew && this.pinned === null) {
      this.drawn = { rows: rows.length, cards };
      return;
    }
    if (rows.length > 0 && (grew || this.pinned !== null)) {
      const card = firstCard(rows);
      // The newest is the spacer after the last row, so a tall last reply shows its end.
      const at = (this.pinned === "card" || cards > this.drawn.cards) && card !== -1 ? card : rows.length;
      this.scroll.scroll_to_item(at);
      // A "card" request waits for the rows it names; the rest are spent now.
      if (this.pinned !== "card" || card !== -1 || this.chat.messages.length > 0) this.pinned = null;
    }
    this.drawn = { rows: rows.length, cards };
  }

  /**
   * One row's place: the pane's full width, `GUTTER` px in from each edge (as
   * Shepherdr's 36). Every row, card and the composer sit in it, so the edges line
   * up at every pane width.
   */
  column(el) {
    return h_flex()
      .w_full()
      .flex_none()
      .justify_center()
      .child(div().w_full().px(GUTTER).child(el));
  }

  /** What an empty timeline says, in the panel and not in a tooltip. */
  blank(t, said) {
    // "Loading…" is for the first moments of a conversation never read: once the
    // read has failed, or taken longer than LOADING_MS, the chat is the empty one
    // (or says why), never a spinner for ever.
    if (this.loaded && !this.loaded.has(this.slot) && this.failure === null) {
      if (Date.now() - this.slotSince < LOADING_MS) {
        // Repaint when the wait is over, whatever the read did.
        if (this.timed !== this.slotSince) {
          this.timed = this.slotSince;
          this.cx.spawn(async () => {
            await this.cx.sleep(LOADING_MS);
            this.cx.notify();
          });
        }
        return v_flex().w_full().flex_1().px(24).py(32).items_center().child(div().text_size(TEXT.label).text_color(t.muted).child("Loading…"));
      }
    }
    return v_flex()
      .w_full()
      .flex_1()
      .px(24)
      .py(32)
      .gap(6)
      .items_center()
      .child(div().text_size(TEXT.body).font_medium().text_color(t.foreground).child(WORDS.emptyTitle))
      .child(div().text_size(TEXT.label).text_color(t.muted).child(said ?? emptyWords(this.space)));
  }

  /** One row of the timeline. */
  row(t, colors, row) {
    if (row.kind === "top") {
      return div()
        .w_full()
        .flex_none()
        .py(8)
        .flex()
        .justify_center()
        .text_size(TEXT.label)
        .text_color(t.dim)
        .child(WORDS.top);
    }
    if (row.kind === "older") {
      return div()
        .w_full()
        .flex_none()
        .py(8)
        .flex()
        .justify_center()
        .text_size(TEXT.label)
        .text_color(t.muted)
        .cursor_pointer()
        .on_click(clickPress(() => void this.loadOlder()))
        .child(WORDS.older);
    }
    if (row.kind === "card") return this.card(t, colors, row.card);
    if (row.kind === "tools") return this.tools(t, row);
    return this.message(t, row);
  }

  /**
   * One message: the agent's at the left, the person's at the right.
   *
   * A tool call is a dim mono line under the reply that ran it rather than a
   * bubble of its own (Shepherdr's rule); a long reply folds under a "Show more".
   */
  message(t, row) {
    const message = row.message;
    const open = this.opened.has(row.key);
    const shown = folding(message.text, open);
    const parts = [];
    if (message.text.trim() !== "") {
      parts.push(
        div()
          .px(12)
          .py(8)
          .min_w(0)
          .rounded(RADIUS.bubble)
          .text_size(TEXT.body)
          .text_color(t.foreground)
          // The agent's words sit in a raised bubble; the person's are an accent
          // tint, which is how a long column of one voice stays readable.
          .when(!mine(message) && message.role !== "news", (el) => el.bg(t.raised))
          // What the service told the agent is a quiet news line, not a bubble.
          .when(message.role === "news", (el) => el.px(4).py(2).text_color(t.muted).text_size(TEXT.label))
          .when(mine(message), (el) => el.bg(t.accent + "33").border_1().border_color(t.accent + "4d"))
          .child(
            TextView.markdown(`chat-md-${row.key}${open ? "" : shown.folded ? "-f" : ""}`, markdownOf(message, shown.text) || " ")
              .selectable(true)
              .on_link_click((url) => this.link(url)),
          ),
      );
    }
    if (shown.foldable) {
      parts.push(
        div()
          .id(`chat-fold-${row.key}`)
          .px(4)
          .text_size(TEXT.label)
          .text_color(t.muted)
          .cursor_pointer()
          .hover((s) => s.text_color(t.foreground))
          .on_click(() => this.toggle(row.key))
          .child(open ? WORDS.showLess : WORDS.showMore),
      );
    }
    if (parts.length === 0) return div();
    return this.column(
      h_flex()
      .w_full()
      .py(5)
      .justify_end()
      .when(!mine(message), (el) => el.justify_start())
      .child(
        v_flex()
          // A definite width: sized by its content alone, a short message collapsed to one
          // letter per row (see `bubbleSpan`).
          .w(bubbleSpan(shown.text, this.width, mine(message)))
          .min_w(0)
          .gap(3)
          .children(parts)
          .when(message.at_ms !== null, (el) =>
            el.child(
              div()
                .w_full()
                .flex()
                .px(4)
                .when(mine(message), (inner) => inner.justify_end())
                .text_size(TEXT.id)
                .text_color(t.dim)
                .child(when(message.at_ms) || ""),
            ),
          ),
      ),
    );
  }

  /**
   * A run of tool calls as one folded row (Shepherdr's "tools" row): "Ran a
   * command", "Read a file, ran 2 commands", opening to each call on a click.
   */
  tools(t, row) {
    const open = this.opened.has(row.key);
    return this.column(
      v_flex()
      .id(`chat-tools-${row.key}`)
      .w_full()
      .py(3)
      .child(
        v_flex()
          .min_w(0)
          .px(4)
          .gap(3)
          .child(
            h_flex()
              .gap(6)
              .items_center()
              .cursor_pointer()
              .text_size(TEXT.label)
              .text_color(t.dim)
              .hover((s) => s.text_color(t.foreground))
              .on_click(() => this.toggle(row.key))
              .child(div().flex_none().w(10).child(open ? "⌄" : "›"))
              .child(div().flex_1().min_w(0).truncate().child(toolsSummary(row.calls))),
          )
          .when(open, (el) =>
            el.child(
              v_flex()
                .pl(16)
                .gap(2)
                .children(
                  row.calls.map((call) =>
                    div()
                      .text_size(TEXT.id)
                      .font_family("JetBrains Mono")
                      .text_color(t.muted)
                      .truncate()
                      .child(toolLine(call)),
                  ),
                ),
            ),
          ),
      ),
    );
  }

  /**
   * One waiting card, in the timeline (Shepherdr's `chat/cards.js`): what the agent
   * asks, the choices as buttons, and a box for words where the card takes them.
   * An escalation is the engine's, with the verb a person still has to run.
   */
  card(t, colors, card_) {
    const tone_ = card_.from === "ask" ? colors.red : colors.amber;
    const refused = this.answerRefusal?.card === card_.id;
    const busy = this.answering === card_.id;
    return this.column(
      div()
      .w_full()
      .py(5)
      .child(
        v_flex()
          .id(`chat-card-${card_.id}`)
          .p(10)
          .gap(6)
          .rounded(RADIUS.card)
          .border_1()
          .border_color(refused ? colors.red : tone_)
          .bg(t.raised)
          .child(
            h_flex()
              .gap(6)
              .items_center()
              .child(div().flex_none().text_size(TEXT.id).font_medium().text_color(tone_).child(card_.title))
              .child(div().flex_1())
              .when(footer(card_) !== "", (el) =>
                el.child(div().flex_none().text_size(TEXT.id).text_color(t.dim).child(footer(card_))),
              ),
          )
          .child(div().text_size(TEXT.body).text_color(t.foreground).child(card_.text))
          .when(card_.verb !== null, (el) =>
            el.child(
              h_flex()
                .gap(6)
                .items_center()
                .child(
                  div()
                    .flex_1()
                    .min_w(0)
                    .font_family("JetBrains Mono")
                    .text_size(TEXT.id)
                    .text_color(colors.amber)
                    .child(card_.verb),
                )
                .when(card_.action !== null, (inner) => inner.child(this.button(t, card_.actionLabel ?? "Run", () => this.act(card_), busy))),
            ),
          )
          .when(card_.verb === null && card_.action !== null, (el) =>
            el.child(
              h_flex()
                .gap(6)
                .child(this.button(t, card_.actionLabel ?? "Run", () => this.act(card_), busy)),
            ),
          )
          .when(card_.options.length > 0, (el) =>
            el.child(
              h_flex()
                .gap(6)
                .flex_wrap()
                .children(
                  card_.options.map((option) =>
                    this.button(t, option.label, () => this.answer(card_, option.answer), busy),
                  ),
                ),
            ),
          )
          .when(card_.freeText, (el) =>
            el.child(
              this.typing === card_.id
                ? h_flex()
                    .gap(6)
                    .items_center()
                    .child(div().flex_1().min_w(0).child(Input.new(this.draft)))
                    .child(this.button(t, "Send", () => this.answer(card_, this.draft?.value?.() ?? ""), busy))
                    .child(this.button(t, "Cancel", () => this.type(card_), false))
                : this.button(t, "Type an answer", () => this.type(card_), busy),
            ),
          )
          .when(refused, (el) =>
            el.child(div().text_size(TEXT.label).text_color(colors.red).child(this.answerRefusal.text)),
          ),
      ),
    );
  }

  /** One small text button: the card's only act. */
  button(t, word, onClick, busy) {
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
      .when(!busy, (el) => el.cursor_pointer().hover((style) => style.bg(t.hover)).on_click(() => onClick()))
      .child(word);
  }

  /** "Working · Esc to stop", with a Stop button, above the box while the agent works. */
  workingLine(t) {
    return this.column(
      h_flex()
        .h(28)
        .gap(8)
        .items_center()
        .child(div().size(6).flex_none().rounded(3).bg(t.accent))
        .child(div().text_size(TEXT.label).text_color(t.muted).child("Working"))
        .child(div().flex_1())
        .child(
          h_flex()
            .id("chat-stop")
            .h(24)
            .px(8)
            .gap(5)
            .flex_none()
            .items_center()
            .rounded(RADIUS.control)
            .border_1()
            .border_color(t.input ?? t.border)
            .text_size(TEXT.label)
            .text_color(this.stopping ? t.dim : t.foreground)
            .cursor_pointer()
            .hover((s) => s.bg(t.hover))
            .on_click(clickPress(() => this.stop()))
            .child(svg(iconFile("stop-fill")).size(10).flex_none())
            .child("Stop")
            .child(div().text_color(t.dim).child("Esc")),
        ),
    );
  }

  /**
   * One round, 28 px button of the composer's bottom row (Shepherdr's shape: Send, the +,
   * the mic never change size, only their icon and fill). A press takes the click for
   * itself, so no ancestor's handler runs on it.
   */
  round(t, id, icon, fill, fn) {
    const el = div()
      .id(id)
      .size(ROUND)
      .flex_none()
      .flex()
      .items_center()
      .justify_center()
      .rounded(ROUND / 2)
      .when(fill === "solid", (e) => e.bg(t.solid ?? t.foreground).text_color(t.solid_foreground ?? t.background))
      .when(fill === "off", (e) => e.bg(t.raised).text_color(t.dim))
      .when(fill === "rec", (e) => e.bg(t.blocked ? `${t.blocked}22` : t.raised).text_color(t.blocked ?? t.foreground))
      .when(fill === "ghost", (e) => e.text_color(t.dim).hover((s) => s.bg(t.hover).text_color(t.foreground)))
      .child(svg(iconFile(icon)).size(14).flex_none());
    if (!fn) return el;
    return el.cursor_pointer().on_click(clickPress(fn));
  }

  /** The attached files, as chips above the words: a name and a × to take it off. */
  tiles(t) {
    if ((this.files ?? []).length === 0) return null;
    return h_flex()
      .w_full()
      .px(10)
      .pt(8)
      .gap(6)
      .flex_wrap()
      .children(
        this.files.map((file, index) =>
          h_flex()
            .id(`chat-file-${index}`)
            .h(24)
            .max_w(220)
            .pl(8)
            .pr(4)
            .gap(5)
            .items_center()
            .rounded(RADIUS.control)
            .bg(t.raised)
            .text_size(TEXT.label)
            .text_color(t.foreground)
            .child(svg(iconFile(file.image ? "files" : "file-text")).size(12).flex_none().text_color(t.dim))
            .child(div().min_w(0).truncate().child(file.name))
            .child(
              div()
                .id(`chat-file-x-${index}`)
                .size(16)
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(8)
                .text_color(t.dim)
                .cursor_pointer()
                .hover((s) => s.bg(t.hover).text_color(t.foreground))
                .on_click(clickPress(() => this.removeFile(index)))
                .child(svg(iconFile("x")).size(10).flex_none()),
            ),
        ),
      );
  }

  /** The row under the words while recording: cancel, the level, how long, done. */
  recordingRow(t) {
    const writing = this.dict.state === "writing";
    const secs = Math.max(0, Math.floor((Date.now() - this.dict.n) / 1000));
    const bar = (i) => div().w(3).h(3 + Math.round(Math.min(1, this.level * (0.5 + (i % 3) * 0.25)) * 18)).flex_none().rounded(2).bg(t.foreground);
    return h_flex()
      .h(36)
      .pl(6)
      .pr(6)
      .gap(8)
      .items_center()
      .child(writing ? div().size(ROUND).flex_none() : this.round(t, "chat-mic-cancel", "x", "ghost", () => this.dict.cancel()))
      .child(
        h_flex()
          .flex_1()
          .min_w(0)
          .gap(2)
          .items_center()
          .justify_center()
          .children(writing ? [] : [0, 1, 2, 3, 4, 5, 6, 7, 8, 9].map(bar))
          .child(div().pl(8).text_size(TEXT.label).text_color(t.muted).child(writing ? "Writing it down…" : `Recording ${Math.floor(secs / 60)}:${String(secs % 60).padStart(2, "0")}`)),
      )
      .child(writing ? div().size(ROUND).flex_none() : this.round(t, "chat-mic-done", "mic", "rec", () => this.toggleDictation()));
  }

  /**
   * The composer, as Shepherdr draws its own: one rounded box holding the words, with the
   * + on the left of its bottom row, then what the coordinator runs on (the model button),
   * the mic and the round Send at the right. It grows with what is typed, to eight lines.
   *
   * Keys are taken here, before the text area has them: Enter sends, Alt+Enter and
   * Shift+Enter are new lines, Esc stops a working agent while the box is empty.
   */
  composer(t, colors) {
    // The box grows with what is in it, up to eight lines, then scrolls.
    const chars = Math.max(20, Math.floor((this.width - 2 * GUTTER - 24) / 7.5));
    const lines = Math.min(8, Math.max(1, lineCount(this.input.value(), chars)));
    if (lines !== this.inputRows) {
      this.inputRows = lines;
      this.input.set_rows?.(lines);
    }
    const sendable = (this.input.value().trim() !== "" || (this.files ?? []).length > 0) && this.space !== null;
    const recording = (this.dict?.state ?? "idle") !== "idle";
    const coordinator = this.space === COORDINATOR && this.selection() !== null;
    return this.column(
      div()
        .w_full()
        .pt(6)
        .pb(12)
        .child(
          v_flex()
            .id("chat-composer")
            .w_full()
            .rounded(10)
            .border_1()
            .border_color((this.boxFocused ?? false) ? t.accent : (t.input ?? t.border))
            .bg(t.background)
            .on_key_down((e, cx) => {
              const action = keyAction(e);
              if (action === "send") {
                cx.stop_propagation();
                this.enter();
                return;
              }
              if (action === "newline") {
                cx.stop_propagation();
                this.input.replace("\n");
                cx.notify();
                return;
              }
              if (String(e?.key ?? "").toLowerCase() === "escape" && this.working() && this.input.value().trim() === "") {
                cx.stop_propagation();
                this.stop();
              }
            })
            .on_drop((e, cx) => {
              this.addFiles(files.entries(e.paths ?? []));
              this.input.focus();
              cx.notify();
            })
            .child(this.tiles(t) ?? div())
            .child(
              div()
                .id("chat-words")
                .w_full()
                .px(12)
                .pt(9)
                .pb(4)
                .on_click(() => this.input.focus())
                .child(Textarea.new(this.input).w_full().text_size(TEXT.body).text_color(t.foreground)),
            )
            .child(
              recording
                ? this.recordingRow(t)
                : h_flex()
                    .h(36)
                    .pl(6)
                    .pr(6)
                    .gap(4)
                    .items_center()
                    .child(this.round(t, "chat-attach", "plus", "ghost", () => this.pickFiles()))
                    .child(
                      div()
                        .flex_1()
                        .min_w(0)
                        .truncate()
                        .text_size(TEXT.label)
                        .text_color(colors.red)
                        .child(this.dict?.error ?? ""),
                    )
                    .when(coordinator, (el) =>
                      el.child(
                        h_flex()
                          .id("chat-model-pick")
                          .h(ROUND)
                          .pl(8)
                          .pr(5)
                          .gap(4)
                          .min_w(0)
                          .max_w(280)
                          .items_center()
                          .rounded(6)
                          .text_size(TEXT.label)
                          .font_medium()
                          .text_color(t.muted)
                          .cursor_pointer()
                          .hover((s) => s.bg(t.hover).text_color(t.foreground))
                          .on_mouse_down("left", (event) => {
                            this.pickRec.at =
                              event?.bounds ??
                              (event?.position ? { x: event.position.x - 40, y: event.position.y - 14, width: 80, height: 28 } : null);
                          })
                          .on_click(clickPress(() => this.openModelMenu(this.pickRec.at ?? null)))
                          .child(div().min_w(0).whitespace_nowrap().truncate().child(this.modelWords()))
                          .child(svg(iconFile("chevron-down")).size(12).flex_none()),
                      ),
                    )
                    .when(this.micOn === true, (el) => el.child(this.round(t, "chat-mic", "mic", "ghost", () => this.toggleDictation())))
                    .child(
                      this.round(t, "chat-send", this.sending ? "spinner" : "send", sendable ? "solid" : "off", () => this.send()),
                    ),
            ),
        ),
    );
  }
}
