// Settings: shprd's settings window — roles and the programs they run, the
// workflow a run follows, and the prompts each role starts with.
//
// It is a dialog over the window and not a tab of the side panel, because that is
// what Shepherdr's own settings are: a list of pages on the left, the page on the
// right, and over each page a repository can own, the level its writes go to. A
// write happens where it is made — a pick, a switch, a step, leaving a box — and
// there is no Save button anywhere, which is Shepherdr's rule for every page of
// its settings.
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/shepherdr-settings/main.js` (the self-drawn `frame:"none"`
//   panel, the page list, `levelBar()`, `row()`/`scopeLine()`, the model button,
//   the write-with-no-Save-step rule and the checked-before-closing notes),
//   `mods/shepherdr-settings/core/settings.js` (`SECTIONS`, `LEVEL`, the row
//   words), `mods/shepherdr-settings/settings.js` (the model menu),
//   `lib/ui.js` (`link`, `segmented`, `toggleSwitch`, `openSettings`)
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// Where it differs from Shepherdr is only where the data does:
//
// - The pages are shprd's four (Agents, Tasks, Prompts, Remote). Remote is the web
//   app's Remote page: the tunnel, the devices signed in, and Log out.
// - The layers are shprd's (`You`, `This repo`), and every value says the layer
//   that answered it (`shprd_core::settings::Source`), because the service
//   resolves the layers and the page never guesses.
// - Nothing is written to a file here. `GET /api/settings` resolves, `PUT
//   /api/settings` writes one key, and the prompts are `GET|PUT|DELETE
//   /api/prompts` — the service is the one writer, so what the page shows is
//   always the service's answer.

import { View, div } from "gpui-kit";
import { Input, InputState, Textarea, TextareaState, h_flex, v_flex } from "gpui-base";
import { closeOverlay, knownAgents, openOverlay, props, shared, theme } from "herdr";

import { attach, reason, ServiceError, singleFlight } from "./lib/client.js";
import {
  CHANGED_HERE,
  MENU_W,
  menuHeight,
  PICK_KEY,
  backupChoice,
  boxProblem,
  coordinatorRunning,
  menusOf,
  otherCli,
  priorInScope,
  repoKeysOf,
  restartNotice,
  roleSelection,
  selectionLabel,
  selectionWrites,
  cleanTitle,
  needsCoordinatorRestart,
  sessionRows,
  WORKER_LINES_ABOUT,
  fallbackLabel,
  fallbackWrite,
  lineLabel,
  lineWrites,
  nextLineProgram,
  withLine,
  workerLines,
  tunnelView,
  maskLink,
  LEVEL,
  PAGES,
  RESET,
  SCOPES,
  TITLE,
  forRepo,
  health,
  levelNote,
  outcomeOf,
  pageOf,
  promptLine,
  promptRows,
  refused,
  roles,
  scopeOf,
  stepped,
  withRole,
  withoutRole,
  workflowRows,
} from "./lib/settings.js";
import {
  badge,
  button,
  icon,
  iconFile,
  link,
  note,
  popupAt,
  press,
  segmented,
  selectButton,
  settingsWindowClosed,
  settingsWindowOpened,
  toggleSwitch,
} from "./lib/panel.js";
import { H, RADIUS, SPACE, TEXT, tone } from "./lib/tokens.js";

/** How often the window re-reads the service, so another window's write shows. */
const FLOOR_MS = 10_000;
/** How long "Saved" stays beside a row that just wrote. */
const SAVED_MS = 2100;
/** How long a note waits after the last key before it saves itself. */
const NOTE_MS = 500;
/**
 * How long after a write a read that disagrees with it is treated as the older one.
 *
 * A `GET /api/settings` already in flight when the write landed carries the value
 * from before it. It is the person's own words coming back as if they had been
 * undone, so a box keeps what it wrote for this long, and takes the service's word
 * after — by then any read has seen the write.
 */
const WRITE_SETTLE_MS = 15_000;
/** How long an edited prompt waits before it saves itself. */
const PROMPT_MS = 700;
/**
 * How long closing waits for the last words to be stored, in ms.
 *
 * A write on loopback is milliseconds and a service that is not there refuses at
 * once, so this is only the bound that keeps `Esc` from hanging on a service that
 * stopped answering.
 */
const CLOSE_WAIT_MS = 1500;

export default class Settings extends View {
  init(_props, cx) {
    const p = props() ?? {};
    this.cx = cx;
    this.page = pageOf(p.page).id;
    this.scope = scopeOf(p.scope).id;
    this.settings = null;
    this.roster = null;
    this.restarting = null;
    this.prompts = null;
    this.failure = null;
    this.client = null;
    /** The role whose name is being edited inline, or null. */
    this.renaming = null;
    /** Whether the coordinator's program, model, effort, speed or access changed here, and one is running. */
    this.coordChanged = false;
    this.coordRunning = false;
    /** The popup a dropdown opened (an overlay id), and the last answer it gave. */
    this.popup = null;
    this.lastReply = shared(PICK_KEY)?.at ?? null;
    /** Each dropdown button's bounds, by id, for placing its popup. */
    this.recs = new Map();
    /** Program icons by harness id: `{ icon, tint }`, from Herdr's known agents. */
    this.icons = {};
    /** Counts the batches of writes, so a refusal can drop the rest of its own. */
    this.batchId = 0;
    /** What each batch has written so far, by batch id: what an undo puts back. */
    this.batchDone = {};
    /** The key a write is in flight for. */
    this.writing = null;
    /**
     * Writes waiting their turn: `{ key, value, for_repo }`, oldest first.
     *
     * One write is in flight at a time — the service takes one key per call, and a
     * page that fired two at once would have the second answer overwrite the
     * first's settings — so a second change is queued rather than dropped. In the
     * order they were made: the last words typed for one key are what the queue
     * ends with, and nothing a person typed is ever thrown away on the way.
     */
    this.queue = [];
    /** The one write in flight, so a flush can wait for it. */
    this.inFlight = null;
    /** The service's refusal, verbatim, and the key it was about. */
    this.refusal = null;
    /** What the last write or reset did, in words. */
    this.saved = null;
    this.savedAt = 0;
    /** The prompt the editor is showing, and its retained text. */
    this.prompt = null;
    this.editor = null;
    /** The words the editor last read from the service, so a save knows what moved. */
    this.editorSaved = "";
    this.editorGen = 0;
    this.promptWrite = null;
    this.promptSavedAt = 0;
    /** Whether the last leave could not store the words, so `Esc` may close instead. */
    this.promptSaveFailed = false;
    /**
     * Whether the editor holds words that a *transport* failure kept from landing.
     *
     * A service that did not answer is the window's to retry: the next read that
     * answers writes them. A service that *refused* the words is the person's to act
     * on, so those are never retried behind their back.
     */
    this.promptRetry = false;
    /** The Remote page's reads: `{ data }`, `{ unavailable }` or `{ error }`, each. */
    this.tunnelRead = null;
    this.sessionRead = null;
    /** The provider picked for a start, and whether the link is showing. */
    this.provider = null;
    this.revealed = false;
    /** `start`, `stop`, `logout` or a session id while that call is out. */
    this.remoteBusy = null;
    /** The id a new role gets, typed into a box made on the first Add. */
    this.newRole = null;
    /** The boxes under rows: `key -> { state, saved, gen, savedAt, rows }`. */
    this.boxes = new Map();
    /** The row whose box has the keyboard, so a re-read never rewrites it. */
    this.focusKey = null;
    this.loading = false;
    this.loadedAt = 0;
    this.scheduled = false;
    this.armedAt = 0;
    this.handle = cx.focus_handle();
    this.handle.focus();
    // The window's own copy is the one the footer's row asks for: recording it
    // here is what keeps a second click from opening a second window. Every read
    // renews it, and `close()` (the app's, below) forgets it.
    this.window_ = props()?.overlay ?? null;
    settingsWindowOpened(this.window_, this.page);
    this.refresh();
    this.schedule();
    // No `nextCommand()` loop: a window has no key of its own. Shepherdr's settings
    // are opened from its sidebar's row and from nothing else — its `keys.json`
    // binds no key to them and its palette registers no entry — and this window is
    // the same, so there is no command for a script to send it. What the pages do
    // is driven by the pointer (`toggle`, `step`, `apply`, the boxes' own events),
    // which is what the tests drive them with too.
  }

  /**
   * Reads the settings, the roster and the template list.
   *
   * A service that restarted is a different run's settings: `attach` notices it
   * by the `id` in `service.json`, the window forgets what it read and reads it
   * all again, so a page never shows one service's roles over another run (M10
   * gate bug 5).
   */
  refresh(cx = this.cx) {
    // One read at a time: a refresh asked for while another runs is folded into
    // one more run after it, never started beside it.
    return singleFlight(this, () => this.readOnce(cx));
  }

  /** One read of the service and one repaint; [`refresh`] is what callers use. */
  async readOnce(cx) {
    // The window says it is up before it asks anything: the copy's place is kept
    // by being renewed on every read, and a service that is down must not let the
    // record go stale — a window that says "shprd serve is not running" is still a
    // window, and the footer's next click must keep it rather than stack another.
    settingsWindowOpened(this.window_, this.page);
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
      this.settings = await this.client.call("GET", "/api/settings", { strict: true });
      // The service answered, so the window is not down. The roles are drawn
      // before the roster is asked: its pickers fill in when the programs
      // answer, and a page that waited on four model listings to show a table it
      // already had would look stuck.
      this.failure = null;
      // A read that answers is the moment to write what the service did not answer
      // for: the notes queued while nothing did, and the prompt editor's words.
      await this.retryWrites(cx);
      this.resetBoxes();
      cx.notify();
      // The roster and the template list are each the window's to do without: a
      // listing that timed out leaves the pickers' menus empty and the page
      // drawn, never the whole window blank.
      const roster = await this.client.roster().catch(() => null);
      if (roster) this.roster = roster;
      const live = await this.client.state().catch(() => null);
      this.coordRunning = coordinatorRunning(live);
      await this.readIcons();
      const prompts = await this.client.prompts().catch(() => null);
      if (prompts) this.prompts = prompts;
      if (this.page === "remote") await this.readRemote();
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

  /** The harnesses' icons, from Herdr's own list of agents (once; a failure leaves none). */
  async readIcons() {
    if (this.iconsRead) return;
    this.iconsRead = true;
    try {
      const known = await knownAgents();
      for (const entry of known ?? []) {
        const id = entry.key ?? (entry.run ?? [])[0];
        if (id && entry.icon) this.icons[id] = { icon: entry.icon, tint: Boolean(entry.tint) };
      }
    } catch {
      // No icons: the tabs and rows are words only.
    }
  }

  /**
   * Drops what the window read from the service that went away: its settings, its
   * programs and its templates are that run's, and a new service has its own.
   */
  forget() {
    this.settings = null;
    this.roster = null;
    this.prompts = null;
    this.refusal = null;
    // A prompt's words are the person's until they are stored. A restarted service is
    // a new run's settings, but a prompt is a file in the config directory and the
    // words typed over it are not that run's to throw away: an editor with unsaved
    // words stays open, and the next read — from the service that is back — stores
    // them. An editor with nothing typed in it goes, as it always did.
    const typed = this.editor?.value?.() ?? this.editorSaved;
    if (this.prompt !== null && typed !== this.editorSaved) {
      this.promptRetry = true;
    } else {
      this.prompt = null;
    }
    // The boxes stay: a note typed while the service was down is the person's and
    // is not this window's to drop. Their saved values are refreshed by the next
    // read's `resetBoxes()`, which leaves a changed box alone.
    for (const box of this.boxes.values()) box.saved = null;
  }

  /**
   * Writes what a service that did not answer kept from being written.
   *
   * A read that answers is the moment for it: the notes queued while nothing did,
   * and the prompt editor's words when a transport failure (never a refusal) left
   * them in the box. The read awaits it, so the page is not redrawn before the
   * words it is showing have been stored.
   */
  async retryWrites(cx = this.cx) {
    if (this.queue.length > 0) await this.pump();
    const typed = this.editor?.value?.() ?? this.editorSaved;
    if (this.promptRetry && this.prompt !== null && typed !== this.editorSaved) {
      this.promptRetry = false;
      await this.savePrompt(cx);
    }
  }

  /** Puts the keyboard back on the panel, as Shepherdr's every page change does. */
  keepKeyboard() {
    this.handle?.focus?.();
    this.cx.notify();
  }

  // -- writing ---------------------------------------------------------------

  /**
   * Writes one settings key, and keeps whatever the service answered.
   *
   * `null` removes the key from the scope's layer, which is what Reset is: the
   * value the scope would inherit applies again (Shepherdr's own rule, and
   * `shprd_core::settings` stores nothing when a value equals the inherited one).
   */
  apply(key, value) {
    return this.applyBatch([{ key, value }]);
  }

  /**
   * Writes several keys as one choice (a program, then its model, then its level).
   *
   * No client is not the same as "nothing to write": the words are held in the
   * queue and written when a read finds the service again (see `pump()`), so a
   * transient timeout never costs a person a note. If the service refuses one
   * write of a batch the rest of that batch is dropped, so a role is never left
   * half switched and shown as saved.
   */
  applyBatch(writes) {
    this.batchId += 1;
    const batch = this.batchId;
    const repo = forRepo(this.scope);
    for (const write of writes) {
      // What this scope's layer held before, so a refused later write can undo the earlier ones.
      const prior = priorInScope(this.settings, write.key, repo);
      this.queue.push({ key: write.key, value: write.value, for_repo: repo, batch, prior });
    }
    return this.pump();
  }

  /**
   * Writes what is queued, one key at a time, and answers when the queue is empty.
   *
   * Every write goes through here: a click, a step, a switch, a note closing, the
   * prompt editor saving. A direct write is queued like the rest, so two writes can
   * never race for the same page's settings, and a write that fails leaves its
   * words in the queue (a no-answer failure) or is reported (a refusal).
   */
  pump() {
    if (this.inFlight !== null) return this.inFlight;
    this.inFlight = this.drain().finally(() => {
      this.inFlight = null;
    });
    return this.inFlight;
  }

  /** The body of [`pump`]: one write after another until the queue is empty. */
  async drain() {
    while (this.queue.length > 0) {
      const next = this.queue.shift();
      this.writing = next.key;
      // An undo runs behind the refusal that caused it and must not wipe it.
      if (!next.undo) this.refusal = null;
      this.saved = null;
      this.cx.notify();
      if (!this.client) {
        // The service is not there to ask: the words go back to the front of the
        // queue, and the next read of the service writes them.
        this.queue.unshift(next);
        this.writing = null;
        this.cx.notify();
        return;
      }
      try {
        const reply = await this.client.putSetting(next.key, next.value, next.for_repo);
        if (reply?.settings) this.settings = reply.settings;
        this.saved = outcomeOf(reply);
        this.savedAt = Date.now();
        if (needsCoordinatorRestart(next.key) && this.coordRunning) this.coordChanged = true;
        this.cx.notify();
        this.cx.timer.after(SAVED_MS, (next_timer) => next_timer.notify());
        // A box or the editor whose words this write carried is "saved" now, so a
        // later blur or debounce sees nothing left to do.
        this.markSaved(next.key, next.value);
        if (!next.undo) (this.batchDone[next.batch] ??= []).push(next);
      } catch (err) {
        // A refusal is the service's sentence, shown as it was sent: the person has
        // to read *why* ("they are both anthropic"), not a summary of it. The words
        // stay in the box, unsaved, and the person can try again.
        if (!next.undo) {
          this.refusal = refused(next.key, err);
        } else {
          // Putting a write back was refused too: say so, so the person knows the role
          // is still half switched and which key to look at.
          const base = this.refusal ?? { key: next.key, text: "" };
          this.refusal = {
            key: base.key,
            text: `${base.text} (and putting ${next.key} back was refused too: ${reason(err)}; it still holds the new value)`.trim(),
          };
        }
        // The rest of this choice is not written, and the popup that made it goes.
        this.queue = this.queue.filter((write) => write.batch !== next.batch);
        // ...and what the choice had already written is put back, newest first, so the
        // role is not left half switched (a program changed under its old model).
        if (!next.undo) {
          const done = this.batchDone[next.batch] ?? [];
          for (const write of done) {
            this.queue.unshift({ key: write.key, value: write.prior ?? null, for_repo: write.for_repo, batch: next.batch, undo: true });
          }
        }
        delete this.batchDone[next.batch];
        this.saved = null;
        this.closePopup();
        this.refresh();
      }
    }
    this.writing = null;
    this.cx.notify();
  }

  /**
   * Records that the words of one key are stored, on the box or the editor that
   * held them.
   *
   * It is called from the write's own success — not from the caller — so a write
   * that was queued behind another and landed later still settles the box that
   * typed it, and a box never asks to write the same words twice.
   */
  markSaved(key, value) {
    const box = this.boxes.get(key);
    if (box && typeof value === "string") {
      box.saved = value;
      box.savedAt = Date.now();
      box.wrote = value;
      box.wroteAt = box.savedAt;
    }
    if (this.prompt !== null && this.writing === `prompts.${this.prompt.id}`) {
      this.editorSaved = this.editor?.value?.() ?? "";
      this.promptSavedAt = Date.now();
    }
  }

  /** Steps one number's row by `by`, inside the row's own bounds. */
  step(row, by) {
    return this.apply(row.key, stepped(row.value, by, row.key));
  }

  // -- the boxes under rows --------------------------------------------------

  /**
   * The box under one row, made on first use and kept: a text field for a line
   * (`rows: 1`) or a text area for the notes.
   *
   * Shepherdr's rule is that nothing has a Save button, so a box writes when the
   * person leaves it (`Enter` or a blur) and, for a note, a moment after typing
   * stops — and a value that came from the service is never written back over a
   * box the person is in.
   */
  boxOf(key, value, { rows = 1, placeholder = "" } = {}) {
    const found = this.boxes.get(key);
    if (found) return found;
    const text = typeof value === "string" ? value : "";
    const state = rows > 1 ? TextareaState.new({ rows }) : InputState.new({ value: "", placeholder });
    state.set_value?.(text);
    const box = { state, saved: text, gen: 0, savedAt: 0, rows };
    state.on?.("change", (_event, cx) => {
      box.gen += 1;
      const gen = box.gen;
      if (rows > 1) {
        cx.timer.after(NOTE_MS, (next) => {
          if (gen === box.gen) this.writeBox(key, next);
        });
      }
    });
    state.on?.("submit", (_event, _cx) => {
      this.writeBox(key);
      this.endRename(key);
    });
    state.on?.("focus", (_event, cx) => {
      this.focusKey = key;
      cx.notify();
    });
    state.on?.("blur", (_event, _cx) => {
      if (this.focusKey === key) this.focusKey = null;
      this.writeBox(key);
      this.endRename(key);
    });
    this.boxes.set(key, box);
    return box;
  }

  /**
   * Takes every box's value from the settings the service resolved — except a box
   * whose words are not stored yet.
   *
   * A re-read runs every ten seconds and after every write, so it is exactly the
   * moment a note could be eaten: a box the person has changed (its text is not
   * what was stored), a box they are typing in, and a box whose write is still
   * queued all keep their words, and the queue writes them when the service
   * answers. Only a box that is in step with the service takes the new value.
   */
  resetBoxes() {
    // Nothing has been read yet (or the page was forgotten): there is no value to
    // compare a box with, so no box is rewritten.
    if (this.settings === null) return;
    const queued = new Set(this.queue.map((write) => write.key));
    // The role names are boxes too: a name the person is typing is never rewritten
    // from under them, and one that was stored empty shows nothing (the fallback
    // is its placeholder).
    const names = roles(this.settings, this.roster).map((role) => ({
      key: role.name.key,
      kind: "text",
      value: role.name.value,
    }));
    for (const row of [...workflowRows(this.settings), ...names]) {
      if (row.kind !== "text" && row.kind !== "notes") continue;
      const box = this.boxOf(row.key, row.value, { rows: row.kind === "notes" ? 3 : 1 });
      const text = typeof row.value === "string" ? row.value : "";
      const typed = box.state.value?.() ?? "";
      // A box the person is in, has changed and not stored, or whose words are
      // still queued, is never rewritten from under them.
      if (this.focusKey === row.key) continue;
      if (queued.has(row.key)) continue;
      if (box.saved !== null && typed !== box.saved) {
        // Their words, not the service's: keep them, and make sure they are on
        // their way (a box whose last write failed has no timer left).
        this.writeBox(row.key);
        continue;
      }
      // What this box last put in the service, and when: a read that was already
      // on its way when the write landed answers with the older value, and taking
      // it would show the person their own words as if they had been undone.
      if (box.wrote !== undefined && box.wrote !== text) {
        const fresh = box.wroteAt !== undefined && Date.now() - box.wroteAt < WRITE_SETTLE_MS;
        if (fresh) continue;
      }
      box.saved = text;
      if (typed === text) continue;
      box.state.set_value?.(text);
    }
  }

  /**
   * Writes one box's words to its key, if they changed since the last write.
   *
   * The words are queued, not dropped: whether a write is in flight, or the service
   * is not answering at this instant, the box's text is what gets written — now, or
   * when the next read finds the service again. That is what keeps a note typed
   * during a time-out from being replaced by the value the page last read.
   */
  async writeBox(key, cx = this.cx) {
    const box = this.boxes.get(key);
    if (!box) return;
    const typed = box.state.value?.() ?? "";
    // A role's name is written trimmed and cut, as the service stores it; the box
    // takes the stored form back when the write lands.
    const text = key.startsWith("roles.") && key.endsWith(".title") ? cleanTitle(typed) : typed;
    if (text === box.saved) return;
    // A branch name git would refuse is refused here, in a sentence, before it is stored.
    const problem = boxProblem(key, text);
    if (problem !== null) {
      this.refusal = { key, text: problem };
      this.cx.notify();
      return;
    }
    // The debounce that called this is the one that saves: bump the counter so a
    // timer armed a moment later does not write the same words again.
    box.gen += 1;
    await this.apply(key, text);
    cx.notify();
    cx.timer.after(SAVED_MS, (next) => next.notify());
  }

  // -- the dropdowns ---------------------------------------------------------
  //
  // Every dropdown is a popup of its own (`mods/shprd-models`), opened over the
  // window the way Shepherdr's are, and it answers through the shared
  // `shprd.settings.pick`. Nothing is drawn inside this panel, so no click here can
  // put a list away that the same click opened.

  /** The record a dropdown button keeps its bounds in. */
  rec(id) {
    if (!this.recs.has(id)) this.recs.set(id, { at: null });
    return this.recs.get(id);
  }

  /** Puts the open popup away, if there is one. */
  closePopup() {
    if (this.popup === null) return;
    try {
      closeOverlay(this.popup);
    } catch {
      // Already gone: a click beside it closed it.
    }
    this.popup = null;
  }

  /** Opens a plain pick list under a button: `options` is `[{ id, label, hint?, icon? }]`. */
  openPick(at, key, field, options, current) {
    this.closePopup();
    const W = 260;
    const H = 14 + 31 * Math.min(options.length, 8);
    this.popup = openOverlay({
      mod: "shprd-models",
      kind: "popup",
      frame: "none",
      width: W,
      height: H,
      at: popupAt(at, W, H),
      props: { mode: "pick", key, field, options, current, width: W, height: H },
    });
  }

  /**
   * Opens the model menu under a button. `key` is what the answer comes back on
   * (`role:<id>`, `line:<id>:<n>` or `fb:<id>`), `current` the choice it starts
   * from, and `opts` the menu's `light`, `allowNone` and `taken` (see
   * `shprd-models`).
   */
  openModelMenu(at, key, current, opts = {}) {
    this.closePopup();
    const menus = menusOf(this.roster);
    const MENU_H = menuHeight(menus, { light: opts.light === true, none: opts.allowNone === true });
    const place = popupAt(at, MENU_W, MENU_H);
    const below = place !== "pointer" && at !== null && place.y > at.y;
    this.popup = openOverlay({
      mod: "shprd-models",
      kind: "popup",
      frame: "none",
      width: MENU_W,
      height: MENU_H,
      at: place,
      props: {
        mode: "models",
        key,
        current,
        menus,
        icons: this.icons,
        width: MENU_W,
        height: MENU_H,
        below,
        light: opts.light === true,
        allowNone: opts.allowNone === true,
        taken: opts.taken ?? [],
      },
    });
  }

  /**
   * Takes what a popup answered, once: `{ key, field, value, at }` in the shared
   * `shprd.settings.pick`. The answer is applied after this render, as a write.
   */
  takeReply() {
    const reply = shared(PICK_KEY);
    if (!reply || reply.at === this.lastReply || typeof reply.key !== "string") return;
    this.lastReply = reply.at;
    this.cx.timer.after(0, (cx) => {
      this.handleReply(reply);
      cx.notify();
    });
  }

  /** What each kind of answer does. */
  handleReply(reply) {
    const [kind, ...rest] = reply.key.split(":");
    const id = rest.join(":");
    if (kind === "scope") {
      this.scope = scopeOf(reply.value).id;
      this.refusal = null;
      this.saved = null;
      this.resetBoxes();
      return;
    }
    if (kind === "provider") {
      this.provider = reply.value;
      return;
    }
    if (kind === "wf") {
      const row = workflowRows(this.settings).find((entry) => entry.key === id);
      const chosen = (row?.choices ?? []).find(([value]) => String(value) === String(reply.value));
      if (chosen) this.apply(id, chosen[0]);
      this.closePopup();
      return;
    }
    if (kind === "role" && reply.field === "selection") {
      const before = roleSelection(this.settings, id);
      const writes = selectionWrites(id, before, reply.value ?? before);
      if (writes.length > 0) this.applyBatch(writes);
      return;
    }
    // One of the worker's program lines was chosen: the whole list is written.
    if (kind === "line" && reply.field === "selection" && reply.value) {
      const [roleId, at] = id.split(":");
      this.setLines(roleId, withLine(workerLines(this.settings, roleId), Number(at), reply.value));
      return;
    }
    // The reviewer's fallback was chosen, or None.
    if (kind === "fb" && reply.field === "selection") {
      const writes = fallbackWrite(this.settings, id, reply.value ?? null);
      if (writes.length > 0) this.applyBatch(writes);
    }
  }

  /** Writes the worker's lines as one choice. */
  setLines(roleId, lines) {
    const writes = lineWrites(this.settings, roleId, lines);
    if (writes.length > 0) this.applyBatch(writes);
  }

  /** The "+": one more line, for the first program no line has yet. */
  addLine(roleId) {
    const agent = nextLineProgram(this.settings, this.roster, roleId);
    if (agent === null) return;
    const line = { agent, model: null, effort: null, fast: false, perm: null };
    this.setLines(roleId, [...workerLines(this.settings, roleId), line]);
  }

  /** Starts editing a role's name in place: the field takes the keyboard. */
  startRename(role) {
    const box = this.boxOf(role.name.key, role.name.value, { placeholder: role.name.fallback });
    box.state.set_value?.(role.title);
    box.saved = role.title;
    this.renaming = role.id;
    this.cx.notify();
    this.cx.timer.after(0, () => box.state.focus?.());
  }

  /** Leaves the name field once its words were written (`Enter` or the keyboard going elsewhere). */
  endRename(key) {
    if (this.renaming !== null && key === `roles.${this.renaming}.title`) {
      this.renaming = null;
      this.cx.notify();
    }
  }

  /** `Esc` in the name field: the old name comes back and nothing is written. */
  cancelRename() {
    const id = this.renaming;
    if (id === null) return;
    const box = this.boxes.get(`roles.${id}.title`);
    box?.state?.set_value?.(box.saved ?? "");
    this.renaming = null;
    this.cx.notify();
  }

  /** Opens a role's instructions: the Prompts page, on that template. */
  editInstructions(role) {
    if (!role.promptId) return;
    this.closePopup();
    this.page = "prompts";
    this.refusal = null;
    this.saved = null;
    this.openPrompt({ id: role.promptId });
  }

  /** Puts back what a repository overrode for a role (the keys its own layer holds). */
  resetRole(role) {
    const writes = repoKeysOf(this.settings, role.id).map((key) => ({ key, value: null }));
    if (writes.length > 0) this.applyBatch(writes);
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

  // -- remote ----------------------------------------------------------------

  /**
   * Reads the tunnel and the signed-in devices.
   *
   * A service too old to have a route answers `404`, which is "not available" and
   * not "empty": the page says so rather than showing a list that cannot be real.
   */
  async readRemote() {
    if (!this.client) return;
    const one = async (path) => {
      try {
        return { data: await this.client.call("GET", path, { strict: true }) };
      } catch (err) {
        return err?.status === 404 ? { unavailable: true } : { error: reason(err) };
      }
    };
    const [tunnel, sessions] = await Promise.all([one("/api/tunnels"), one("/api/sessions")]);
    this.tunnelRead = tunnel;
    this.sessionRead = sessions;
    this.cx.notify();
  }

  /** Starts or stops the tunnel; the service's own sentence is shown if it refuses. */
  async tunnelCall(kind) {
    if (!this.client || this.remoteBusy !== null) return;
    this.remoteBusy = kind;
    this.refusal = null;
    this.cx.notify();
    try {
      const view = tunnelView(this.tunnelRead?.data, this.provider);
      const reply =
        kind === "start"
          ? await this.client.call("POST", "/api/tunnels/start", { body: { provider: view?.provider } })
          : await this.client.call("POST", "/api/tunnels/stop", { body: {} });
      this.tunnelRead = { data: reply };
      this.revealed = false;
    } catch (err) {
      this.refusal = refused("tunnel", err);
    }
    this.remoteBusy = null;
    this.cx.notify();
  }

  /** Ends one signed-in device's session. */
  async revokeSession(id) {
    if (!this.client || this.remoteBusy !== null) return;
    this.remoteBusy = id;
    this.refusal = null;
    this.cx.notify();
    try {
      await this.client.call("DELETE", `/api/sessions/${encodeURIComponent(id)}`);
    } catch (err) {
      this.refusal = refused("sessions", err);
    }
    this.remoteBusy = null;
    await this.readRemote();
  }

  /** Log out all: ends every session, one after the other, and reads the list again. */
  async logoutAll() {
    if (!this.client || this.remoteBusy !== null) return;
    this.remoteBusy = "logout";
    this.refusal = null;
    this.cx.notify();
    const ids = (this.sessionRead?.data?.sessions ?? []).map((entry) => entry.id);
    for (const id of ids) {
      try {
        await this.client.call("DELETE", `/api/sessions/${encodeURIComponent(id)}`);
      } catch (err) {
        this.refusal = refused("sessions", err);
        break;
      }
    }
    this.remoteBusy = null;
    await this.readRemote();
  }

  // -- the prompts -----------------------------------------------------------

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
        this.editor = TextareaState.new({ rows: 16 });
        // The box saves itself, as Shepherdr's notes do: a moment after typing
        // stops, and when it loses the keyboard. There is no Save button.
        this.editor.on?.("change", (_event, cx) => {
          this.editorGen += 1;
          const gen = this.editorGen;
          cx.timer.after(PROMPT_MS, (next) => {
            if (gen === this.editorGen) this.savePrompt(next);
          });
        });
        this.editor.on?.("blur", (_event, _cx) => this.savePrompt());
      }
      this.editor.set_value(prompt.text);
      this.editorSaved = prompt.text;
    } catch (err) {
      this.refusal = refused(`prompts.${row.id}`, err);
    }
    this.handle?.focus?.();
    this.cx.notify();
  }

  /**
   * Stores the editor's words over the open prompt, if they changed.
   *
   * A prompt's words are the one write that is not a settings key, so they bypass
   * the queue for settings keys — but not the rule that a save is never dropped: if
   * a settings write is in flight, this one starts when it is done, and if the
   * service is not there to ask the words stay in the box with a reason on screen
   * rather than being thrown away.
   *
   * One write at a time: a caller that asks again while one is in flight gets that
   * write's promise, so `leavePrompt()` and a debounce firing together cannot
   * store the same words twice.
   */
  savePrompt(cx = this.cx) {
    if (this.promptWrite) return this.promptWrite;
    if (this.writing !== null) {
      // A settings write is in flight: the editor's words go right after it.
      this.promptWrite = (this.inFlight ?? Promise.resolve())
        .catch(() => {})
        .then(() => {
          this.promptWrite = null;
          return this.savePrompt(cx);
        });
      return this.promptWrite;
    }
    this.promptWrite = this.writePrompt(cx).finally(() => {
      this.promptWrite = null;
    });
    return this.promptWrite;
  }

  /** The write behind [`savePrompt`]: the words, over the prompt that is open. */
  async writePrompt(cx = this.cx) {
    const prompt = this.prompt;
    if (!prompt) return;
    const text = this.editor?.value?.() ?? "";
    if (text === this.editorSaved) return;
    if (!this.client) {
      // The service is not there: say so over the editor and keep the words in it,
      // so the person sees the note was not stored and Esc closes the window
      // instead of quietly losing it. The next read that answers writes them.
      this.refusal = refused(
        `prompts.${prompt.id}`,
        new ServiceError("the service did not answer, so these words are not stored yet"),
      );
      this.promptSaveFailed = true;
      this.promptRetry = true;
      this.cx.notify();
      return;
    }
    const id = prompt.id;
    this.writing = `prompts.${id}`;
    this.refusal = null;
    this.cx.notify();
    try {
      const saved = await this.client.putPrompt(id, text);
      if (saved) this.prompt = saved;
      this.editorSaved = text;
      this.saved = { outcome: "stored", text: "Saved: your words are what an agent is told." };
      this.savedAt = Date.now();
      this.promptSavedAt = this.savedAt;
      this.promptRetry = false;
      const list = await this.client.prompts();
      if (list) this.prompts = list;
      this.writing = null;
      this.cx.notify();
      cx.timer.after(SAVED_MS, (next) => next.notify());
      return;
    } catch (err) {
      this.refusal = refused(`prompts.${id}`, err);
      // A call that got no answer at all is retried by the next read; a service
      // that answered with a refusal is the person's to read and act on.
      this.promptRetry = err instanceof ServiceError && err.code === "no-answer";
      this.writing = null;
      this.cx.notify();
      return;
    }
    this.writing = null;
    this.cx.notify();
  }

  /**
   * Leaves the prompt editor, storing the words typed since the last pause.
   *
   * The editor is closed only once the write has landed, so its own answer
   * cannot put the editor back (`Back` used to do that: the save resolved after
   * the list was asked for, and the editor with it). A save that a write in
   * flight swallowed is retried, a few times, because leaving is the one moment
   * a note's last keystrokes have nowhere else to go.
   */
  async leavePrompt() {
    const prompt = this.prompt;
    if (prompt === null) return;
    this.promptSaveFailed = false;
    for (let attempt = 0; attempt < 3; attempt += 1) {
      await this.savePrompt();
      if ((this.editor?.value?.() ?? "") === this.editorSaved) break;
    }
    // A prompt opened meanwhile is the person's to look at, not this leaving's to
    // close. (The save's own answer replaces the prompt it wrote, so the two are
    // compared by id and not by identity.)
    if (this.prompt?.id !== prompt.id) return;
    const refusedHere =
      this.refusal !== null && String(this.refusal.key ?? "") === `prompts.${prompt.id}`;
    if (refusedHere) {
      // The words are still only in the box: the editor stays with the service's
      // reason over it, and the next `Esc` closes the window instead (see
      // `escape()`), so a broken service can never trap a person in the editor.
      this.promptSaveFailed = true;
      this.cx.notify();
      return;
    }
    this.prompt = null;
    this.refusal = null;
    this.keepKeyboard();
  }

  /** Puts the shipped words back over the open prompt. */
  async resetPrompt() {
    const prompt = this.prompt;
    if (!this.client || !prompt || this.writing !== null) return;
    const id = prompt.id;
    this.writing = `prompts.${id}`;
    this.refusal = null;
    this.saved = null;
    this.cx.notify();
    try {
      const reset = await this.client.resetPrompt(id);
      if (reset) {
        this.prompt = reset;
        this.editor?.set_value?.(reset.text);
        this.editorSaved = reset.text;
        this.editorGen += 1;
      }
      this.saved = { outcome: "stored", text: "Reset: the shipped words are back." };
      this.savedAt = Date.now();
      const list = await this.client.prompts();
      if (list) this.prompts = list;
      this.writing = null;
      this.cx.notify();
      this.cx.timer.after(SAVED_MS, (next) => next.notify());
      return;
    } catch (err) {
      this.refusal = refused(`prompts.${id}`, err);
    }
    this.writing = null;
    this.cx.notify();
  }

  // -- closing ---------------------------------------------------------------

  /**
   * `Esc`: the first press closes what was open inside the window (a picker, a
   * typed box, the prompt editor), the next closes the window — which is what
   * Shepherdr's dialog does with the key.
   */
  escape() {
    if (this.renaming !== null) {
      this.cancelRename();
      return Promise.resolve();
    }
    if (this.page === "prompts" && this.prompt !== null) {
      // The words typed since the last pause are saved before the editor goes,
      // exactly as `Back` and closing the window save them. A save the service
      // refused keeps the editor — with its reason — and leaves the next `Esc` to
      // close the window.
      if (this.promptSaveFailed) {
        this.promptSaveFailed = false;
        return this.dismiss();
      }
      return this.leavePrompt();
    }
    return this.dismiss();
  }

  /**
   * Closes the window, saving anything typed a moment ago first.
   *
   * A note or a prompt is written on a debounce, so the last keystrokes may still
   * be waiting when the window goes: leaving is one of the moments Shepherdr
   * saves at, and this is that moment.
   */
  async dismiss() {
    this.closePopup();
    await this.flush();
    settingsWindowClosed(this.window_);
    closeOverlay(props()?.overlay);
  }

  /**
   * Stores everything typed since the last pause, and answers when it has been
   * stored — or when the service has had [`CLOSE_WAIT_MS`] to answer.
   *
   * Awaiting this is what makes the last keystrokes reach the service before the
   * window goes: a mod's `fetch` does not outlive the copy that made it, so
   * closing first and writing afterwards is how a note gets lost.
   */
  async flush(cx = this.cx) {
    const writing = (async () => {
      if (this.prompt !== null) await this.savePrompt();
      for (const key of this.boxes.keys()) await this.writeBox(key);
      await this.pump();
    })();
    let wait = Promise.resolve();
    try {
      if (typeof cx?.sleep === "function") wait = cx.sleep(CLOSE_WAIT_MS);
    } catch {
      // A context that cannot give a clock closes at once; the writes go on
      // best-effort either way.
    }
    await Promise.race([writing.catch(() => {}), wait]);
  }

  /**
   * The app's own last word about this copy: it is gone, however it went.
   *
   * Herdr calls `close(cx)` when an overlay is taken away — `Esc`, a click
   * beside it, `overlay.close` from the command line — and this is where the
   * window stops claiming its place, so the footer's next click opens it again.
   */
  async close(cx = this.cx) {
    await this.flush(cx);
    settingsWindowClosed(this.window_);
  }

  // -- reading again ---------------------------------------------------------

  /**
   * Reads again when the window is drawn with an old answer.
   *
   * An overlay's timers are not guaranteed to survive it being hidden, so the
   * moment it is drawn is the moment to catch up: `loading` keeps one read in
   * flight, and the floor keeps a repaint from becoming a poll.
   */
  revalidate() {
    this.schedule();
    if (this.loading) return;
    if (Date.now() - this.loadedAt < FLOOR_MS) return;
    this.loading = true;
    this.refresh().finally(() => {
      this.loading = false;
      this.loadedAt = Date.now();
    });
  }

  /** Polls the service while the window is drawn: one read, then the next. */
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

  // -- drawing: the window ---------------------------------------------------

  /**
   * The window: a self-drawn rounded panel over the dimmed window, a page list on
   * the left and the page on the right.
   *
   * `frame: "none"` means Herdr draws nothing around this but the dimming, so the
   * panel is ours: 860 wide (the overlay's own width), 92% of the window's
   * height capped at 900, centred. A click beside it closes it, and `Esc` does
   * (Herdr closes the top dialog, and this handles the key first so an open
   * picker closes before the window does).
   */
  render() {
    this.revalidate();
    this.takeReply();
    const t = theme();
    const colors = tone(t);
    const card = h_flex()
      .id("shprd-settings")
      .track_focus(this.handle)
      .w_full()
      .h_full()
      .max_h(900)
      .min_h(0)
      .items_start()
      .rounded(RADIUS.bubble)
      .border_1()
      .border_color(t.border)
      .bg(t.background)
      .overflow_hidden()
      .on_click((_event, cx) => {
        // A click inside the panel stays inside: only one beside it closes the
        // window (Shepherdr's own rule for its self-drawn frame). Each control
        // stops its own click first (`press()`), so this is only what no control took:
        // a click on the panel's own blank puts an open dropdown away.
        cx.stop_propagation();
        this.closePopup();
      })
      .on_key_down((event, cx) => {
        if (event?.key !== "escape") return;
        cx.stop_propagation();
        this.escape();
      })
      .child(this.nav(t, colors))
      .child(this.body(t, colors));
    return div()
      .id("shprd-settings-root")
      .size_full()
      .flex()
      .items_center()
      .justify_center()
      .on_click(() => this.dismiss())
      .child(card);
  }

  /** The page list on the left: the title, then one row a page. */
  nav(t, colors) {
    const row = (page) => {
      const on = page.id === this.page;
      const base = h_flex()
        .id(`sh-s-${page.id}`)
        .h(30)
        .flex_none()
        .gap(8)
        .px(10)
        .items_center()
        .rounded(RADIUS.card)
        .text_size(TEXT.body)
        .text_color(on ? t.foreground : t.muted)
        .when(on, (el) => el.bg(t.raised))
        .when(!on, (el) =>
          el
            .cursor_pointer()
            .hover((style) => style.bg(t.hover))
            .on_click(
              press(() => {
                this.closePopup();
                this.page = page.id;
                this.refusal = null;
                this.saved = null;
                this.keepKeyboard();
                if (page.id === "remote") this.readRemote();
              }),
            ),
        )
        .child(icon(page.icon, 14, on ? t.foreground : t.muted))
        .child(div().flex_1().min_w(0).truncate().child(page.title));
      return base;
    };
    return v_flex()
      .flex_none()
      .w(190)
      .h_full()
      .p(10)
      .gap(2)
      .border_r(1)
      .border_color(t.border)
      .child(
        div()
          .px(10)
          .pt(6)
          .pb(12)
          .text_size(TEXT.title)
          .font_semibold()
          .text_color(t.foreground)
          .child(TITLE),
      )
      .children(PAGES.map(row));
  }

  /** The page on the right: the level switch, what went wrong, the page itself. */
  body(t, colors) {
    const blocks = [];
    if (this.failure !== null) {
      blocks.push(note(t, this.failure));
      blocks.push(note(t, "Start it from the sidebar's footer; this window fills in when it answers."));
      return v_flex()
        .flex_1()
        .min_h(0)
        .overflow_y_scrollbar()
        .px(28)
        .py(22)
        .gap(6)
        .children(blocks);
    }
    // No "Saved" line: it pushed the page down on every write. A write shows in
    // the control that made it, and a refusal is the one thing that gets a block.
    if (this.refusal !== null) blocks.push(this.refusalBlock(t, colors));
    const state = health(this.settings);
    for (const problem of state.problems) {
      blocks.push(div().pb(4).text_size(TEXT.label).text_color(colors.red).child(problem));
    }
    if (state.notChecked) {
      blocks.push(
        div()
          .pb(4)
          .text_size(TEXT.label)
          .text_color(colors.amber)
          .child(`the roles were not checked: ${state.notChecked}`),
      );
    }
    if (this.settings === null) blocks.push(note(t, "Reading the settings…"));
    return v_flex()
      .flex_1()
      .min_w(0)
      .min_h(0)
      .overflow_y_scrollbar()
      .child(div().px(28).py(22).child(this.levelBar(t, colors)).children(blocks).children(this.pageBody(t, colors)));
  }

  /** The page that is showing. */
  pageBody(t, colors) {
    if (this.settings === null) return [];
    if (this.page === "tasks") return this.tasksPage(t);
    if (this.page === "prompts") return this.promptsPage(t, colors);
    if (this.page === "remote") return this.remotePage(t, colors);
    return this.agentsPage(t, colors);
  }

  /**
   * The level switch, at the top of each page a repository can own: Shepherdr's
   * `Applies to` and a dropdown (Everywhere, This repo), and under it what the
   * level means. A page that is one for every repository says so instead.
   */
  levelBar(t, colors) {
    const page = PAGES.find((entry) => entry.id === this.page);
    if (!page?.repo) {
      return div()
        .pb(14)
        .text_size(TEXT.label)
        .line_height("17px")
        .text_color(t.dim)
        .child(LEVEL.one);
    }
    const repository = this.settings?.repo ?? null;
    const word = this.scope === "repo" ? "This repo" : "Everywhere";
    const options = [
      { id: "you", label: "Everywhere", icon: "globe" },
      { id: "repo", label: "This repo", hint: repository ?? undefined, icon: "branch" },
    ];
    const rec = this.rec("scope");
    return v_flex()
      .w_full()
      .mb(18)
      .px(12)
      .py(10)
      .gap(6)
      .rounded(RADIUS.card)
      .bg(t.raised)
      .child(
        h_flex()
          .gap(10)
          .items_center()
          .child(div().flex_none().text_size(TEXT.label).text_color(t.muted).child(LEVEL.label))
          .child(
            selectButton(
              t,
              "level",
              [icon(this.scope === "repo" ? "branch" : "globe", 14, t.muted), div().child(word)],
              rec,
              (at) => this.openPick(at, "scope", "pick", options, this.scope),
              { bordered: true },
            ),
          )
          .child(div().flex_1())
          .when(this.writing !== null, (el) =>
            el.child(div().flex_none().text_size(TEXT.id).text_color(colors.amber).child("saving…")),
          ),
      )
      .child(
        div()
          .text_size(TEXT.label)
          .line_height("17px")
          .text_color(t.dim)
          .child(levelNote(this.scope, repository)),
      );
  }

  /**
   * Shepherdr's row: the title and one line about it on the left, the control at
   * the right, and `below` (a box, say) under both. Where the layer matters it is
   * a small `repo` badge (this repository overrides the value), and in `This repo`
   * a plain Reset beside it; a value nothing in the repo sets shows nothing.
   * `status` is the few words that follow a write ("Saved").
   */
  row(t, { title, about, status = null, control = null, scope = null, below = null }) {
    const mine = scope?.mine === true;
    return v_flex()
      .w_full()
      .gap(8)
      .py(12)
      .border_b(1)
      .border_color(t.border)
      .child(
        h_flex()
          .w_full()
          .gap(12)
          .items_center()
          .child(
            v_flex()
              .flex_1()
              .min_w(0)
              .gap(3)
              .child(
                h_flex()
                  .gap(6)
                  .items_center()
                  .child(div().text_size(TEXT.body).font_medium().text_color(t.foreground).child(title))
                  .when(mine, (el) => el.child(badge(t, "repo", t.raised, t.muted))),
              )
              .when(Boolean(about), (el) =>
                el.child(div().text_size(TEXT.label).line_height("17px").text_color(t.dim).child(about)),
              ),
          )
          .when(status !== null, (el) =>
            el.child(div().flex_none().text_size(TEXT.id).text_color(t.dim).child(String(status))),
          )
          .when(mine && this.scope === "repo" && scope?.key, (el) =>
            el.child(link(t, `reset-${scope.key}`, RESET, () => this.apply(scope.key, null), this.writing !== null)),
          )
          .when(control !== null, (el) => el.child(h_flex().flex_none().gap(8).items_center().child(control))),
      )
      .when(below !== null, (el) => el.child(below));
  }

  /** A refusal: the service's own sentence, unedited, in the tone it deserves. */
  refusalBlock(t, colors) {
    return v_flex()
      .mb(10)
      .p(8)
      .gap(2)
      .rounded(RADIUS.card)
      .border_1()
      .border_color(colors.red)
      .child(div().text_size(TEXT.id).text_color(t.dim).child(this.refusal.key ?? "refused"))
      .child(div().text_size(TEXT.label).text_color(colors.red).child(this.refusal.text));
  }

  /** A page's title, at the top of its body (Shepherdr's `TEXT.title`). */
  title(t, text) {
    return div().pb(6).text_size(TEXT.title).font_semibold().text_color(t.foreground).child(text);
  }

  // -- drawing: Agents -------------------------------------------------------

  /**
   * Agents, in Shepherdr's shape: one compact row a role — its name and one line
   * about it, then a single "<Model> · <Effort>" button that opens the model menu
   * and an Edit instructions button — and the toggles shprd has (Backup agent).
   *
   * The roles, their order and their words are the service's answer to
   * `GET /api/settings`; the programs and their models are `GET /api/roster`'s.
   * A refusal is the service's own sentence: a cross-vendor review is refused in
   * words that name both roles and both vendors, and a person needs exactly those.
   */
  agentsPage(t, colors) {
    const list = roles(this.settings, this.roster);
    return [
      this.title(t, "Agents"),
      ...list.map((role) => (role.id === "backup" ? this.backupRow(t, role) : this.roleRow(t, colors, role))),
      this.addRoleRow(t),
    ];
  }

  /** The role's button: "Opus 5.5 · Medium ⌄", opening the model menu. */
  modelButton(t, role) {
    const menus = menusOf(this.roster);
    const sel = roleSelection(this.settings, role.id);
    const word = this.roster === null ? "Loading…" : selectionLabel(menus, sel, this.roster);
    const disabled = this.roster === null || menus.order.length === 0;
    return selectButton(
      t,
      `model-${role.id}`,
      [div().min_w(0).truncate().child(disabled && this.roster !== null ? "No program found" : word)],
      this.rec(`model-${role.id}`),
      (at) => this.openModelMenu(at, `role:${role.id}`, roleSelection(this.settings, role.id)),
      { disabled },
    );
  }

  /** One of the worker's lines: "Claude Code · Opus 5.5 · Medium ⌄", and a × while there is another line. */
  lineRow(t, role, lines, at) {
    const menus = menusOf(this.roster);
    const sel = lines[at];
    const disabled = this.roster === null || menus.order.length === 0;
    const id = `line-${role.id}-${at}`;
    const taken = lines.filter((_, i) => i !== at).map((line) => line.agent);
    return h_flex()
      .gap(4)
      .items_center()
      .child(
        selectButton(
          t,
          id,
          [div().min_w(0).truncate().child(this.roster === null ? "Loading…" : lineLabel(menus, sel, this.roster))],
          this.rec(id),
          (point) => this.openModelMenu(point, `line:${role.id}:${at}`, sel, { taken }),
          { disabled },
        ),
      )
      .when(lines.length > 1, (el) =>
        el.child(
          link(t, `remove-${id}`, "×", () => this.setLines(role.id, withLine(lines, at, null)), this.writing !== null),
        ),
      );
  }

  /** The worker's lines, one picker each, and the "+" that adds another. */
  workerLinesControl(t, role) {
    const lines = workerLines(this.settings, role.id);
    const shown = lines.length > 0 ? lines : [{ agent: null, model: null, effort: null, fast: false, perm: null }];
    const more = this.roster !== null && lines.length > 0 && nextLineProgram(this.settings, this.roster, role.id) !== null;
    return v_flex()
      .flex_none()
      .gap(6)
      .items_end()
      .children(shown.map((_, at) => this.lineRow(t, role, shown, at)))
      .when(more, (el) =>
        el.child(link(t, `add-line-${role.id}`, "+ Add a program", () => this.addLine(role.id), this.writing !== null)),
      );
  }

  /** The reviewer's fallback: "Fallback: Codex · GPT-6-Luna ⌄", the same menu as the main pick, with None. */
  fallbackPicker(t, role) {
    const menus = menusOf(this.roster);
    const sel = role.fallback.value;
    const id = `fallback-${role.id}`;
    const disabled = this.roster === null || menus.order.length === 0;
    return h_flex()
      .gap(6)
      .items_center()
      .child(div().flex_none().text_size(TEXT.label).text_color(t.muted).child("Fallback"))
      .child(
        selectButton(
          t,
          id,
          [div().min_w(0).truncate().child(this.roster === null ? "Loading…" : fallbackLabel(menus, sel, this.roster))],
          this.rec(id),
          (at) =>
            this.openModelMenu(at, `fb:${role.id}`, sel ?? { agent: null, model: null, effort: null, fast: false, perm: null }, {
              light: true,
              allowNone: true,
            }),
          { disabled },
        ),
      );
  }

  /** The role's name: the title (click it, or the pencil, to rename) or, while renaming, its field. */
  roleName(t, role) {
    if (this.renaming === role.id) {
      const box = this.boxOf(role.name.key, role.name.value, { placeholder: role.name.fallback });
      return div()
        .w(190)
        .h(26)
        .flex_none()
        .rounded(RADIUS.control)
        .border_1()
        .border_color(t.accent)
        .bg(t.raised)
        .overflow_hidden()
        .child(Input.new(box.state).w_full().px(8).py(2).text_size(TEXT.body));
    }
    return h_flex()
      .id(`rename-${role.id}`)
      .gap(6)
      .items_center()
      .rounded(RADIUS.control)
      .cursor_pointer()
      .hover((style) => style.bg(t.hover))
      .on_click((_e, cx) => {
        cx.stop_propagation();
        this.startRename(role);
        cx.notify();
      })
      .child(div().text_size(TEXT.body).font_medium().text_color(t.foreground).child(role.title))
      .child(icon("square-pen", 12, t.dim));
  }

  /** One role. */
  roleRow(t, colors, role) {
    const other = role.id === "reviewer" ? otherCli(this.settings, this.roster) : null;
    const mine = repoKeysOf(this.settings, role.id).length > 0;
    const state = this.restarting;
    // Only the coordinator's row, and only once its choice changed (or while a restart it asked for shows its answer).
    const notice =
      role.kind === "coordinator" &&
      (restartNotice({ changed: this.coordChanged, running: this.coordRunning }) || typeof state === "string");
    return v_flex()
      .w_full()
      .gap(6)
      .py(12)
      .border_b(1)
      .border_color(t.border)
      .child(
        h_flex()
          .w_full()
          .gap(12)
          .items_center()
          .child(
            v_flex()
              .flex_1()
              .min_w(0)
              .gap(3)
              .child(
                h_flex()
                  .gap(6)
                  .items_center()
                  .child(this.roleName(t, role))
                  .when(role.readonly, (el) =>
                    el.child(div().flex_none().text_size(TEXT.id).text_color(t.dim).child("reads only")),
                  )
                  .when(role.custom, (el) => el.child(badge(t, "custom", t.raised, t.muted)))
                  .when(mine, (el) => el.child(badge(t, "repo", t.raised, t.muted))),
              )
              .when(role.about !== "", (el) =>
                el.child(div().text_size(TEXT.label).line_height("17px").text_color(t.dim).child(role.about)),
              )
              .when(role.id === "worker", (el) =>
                el.child(div().text_size(TEXT.label).line_height("17px").text_color(t.dim).child(WORKER_LINES_ABOUT)),
              ),
          )
          .child(
            h_flex()
              .flex_none()
              .gap(8)
              .items_center()
              .when(mine && this.scope === "repo", (el) =>
                el.child(link(t, `reset-${role.id}`, RESET, () => this.resetRole(role), this.writing !== null)),
              )
              .child(role.id === "worker" ? this.workerLinesControl(t, role) : this.modelButton(t, role))
              .when(role.id === "reviewer" && role.fallback !== null, (el) => el.child(this.fallbackPicker(t, role)))
              .when(other !== null, (el) =>
                el.child(button(t, `other-${role.id}`, "Use other CLI", { kind: "plain", onClick: () => this.useOtherCli(role, other) })),
              )
              .when(Boolean(role.promptId), (el) =>
                el.child(button(t, `instructions-${role.id}`, "Edit instructions", { onClick: () => this.editInstructions(role) })),
              )
              .when(role.custom, (el) =>
                el.child(button(t, `remove-${role.id}`, "Remove", { kind: "plain", disabled: this.writing !== null, onClick: () => this.removeRole(role) })),
              ),
          ),
      )
      .when(notice, (el) => el.child(this.restartLine(t, colors, state)));
  }

  /**
   * The coordinator keeps running the program it started with: this one line shows
   * only after its program, model, effort, speed or access changed here, and says
   * so with the one button that restarts it in the same tab and chat.
   */
  restartLine(t, colors, state) {
    const word =
      state === "working"
        ? "Restarting…"
        : state === "done"
          ? "Restarted"
          : typeof state === "string" && state.startsWith("failed:")
            ? state.slice(7)
            : "Applies when the coordinator next starts.";
    const failed = typeof state === "string" && state.startsWith("failed:");
    return h_flex()
      .id("restart-notice")
      .w_full()
      .gap(8)
      .items_center()
      .child(div().text_size(TEXT.label).text_color(failed ? colors.red : t.dim).child(word))
      .when(state !== "working" && state !== "done", (el) =>
        el.child(link(t, "restart-coordinator", "Restart the coordinator now", () => this.restartCoordinator())),
      );
  }

  /** Restarts the coordinator through the service, and says how it went. */
  async restartCoordinator() {
    if (!this.client || this.restarting === "working") return;
    this.restarting = "working";
    this.cx.notify();
    try {
      await this.client.restartCoordinator();
      this.restarting = "done";
      this.coordChanged = false;
    } catch (err) {
      this.restarting = `failed:${reason(err)}`;
    }
    this.cx.notify();
    this.cx.timer.after(SAVED_MS * 2, (next) => {
      if (this.restarting === "done") this.restarting = null;
      next.notify();
    });
  }

  /** "Use other CLI": the reviewer runs the program of another vendor than the worker's. */
  useOtherCli(role, program) {
    const before = roleSelection(this.settings, role.id);
    const after = { agent: program, model: null, effort: null, fast: false, perm: null };
    this.applyBatch(selectionWrites(role.id, before, after));
  }

  /**
   * The Backup agent, a switch in Shepherdr's row. On names the other program
   * (another vendor, one that is installed, when there is one) and shows its
   * button beside the switch; off takes the program away again.
   */
  backupRow(t, role) {
    const field = role.backup;
    const flip = (on) => {
      if (!on) return this.apply(field.key, null);
      const program = backupChoice(this.settings, this.roster);
      if (program === null) {
        this.refusal = { key: field.key, text: "there is no other program to take a task over" };
        this.cx.notify();
        return undefined;
      }
      return this.apply(field.key, program);
    };
    return this.row(t, {
      title: role.title,
      about: "The other CLI takes a task over once when its agent can't go on.",
      scope: { key: field.key, layer: field.layer, mine: field.source === "repo_entry" },
      control: h_flex()
        .gap(8)
        .items_center()
        .when(field.on, (el) => el.child(this.modelButton(t, role)))
        .child(toggleSwitch(t, field.key, field.on, flip)),
    });
  }

  /** The one row that makes a new role. */
  addRoleRow(t) {
    if (this.newRole === null) {
      return h_flex()
        .h(SPACE.row)
        .flex_none()
        .py(8)
        .items_center()
        .child(
          link(t, "add-role", "Add a role", () => {
            this.newRole = InputState.new({ placeholder: "role id, e.g. docs", value: "" });
            this.cx.notify();
          }),
        );
    }
    return h_flex()
      .h(H.lg)
      .flex_none()
      .py(8)
      .gap(8)
      .items_center()
      .child(div().flex_1().min_w(0).child(Input.new(this.newRole)))
      .child(link(t, "add", "Add", () => this.addRole(), this.writing !== null))
      .child(
        link(
          t,
          "cancel-add",
          "Cancel",
          () => {
            this.newRole = null;
            this.cx.notify();
          },
          false,
        ),
      );
  }

  // -- drawing: Tasks --------------------------------------------------------

  /**
   * Tasks: the workflow keys the settings API exposes, one row each, written
   * where they are made — a segment picked, a switch flipped, a step taken — and
   * the two notes in a box under their row, saved a moment after typing stops.
   */
  tasksPage(t) {
    const rows = workflowRows(this.settings);
    return [this.title(t, "Tasks"), ...rows.map((row) => this.workflowRow(t, row))];
  }

  /** One workflow row, with the control its kind asks for. */
  workflowRow(t, row) {
    const scope = { key: row.key, layer: row.layer, mine: row.source === "repo_entry" };
    if (row.control === "segmented") {
      const options = row.choices.map(([value, word]) => ({ id: String(value), label: word }));
      const shown = row.choices.find(([value]) => value === row.value)?.[1] ?? row.display;
      return this.row(t, {
        title: row.title,
        about: row.about,
        control: selectButton(
          t,
          `wf-${row.key}`,
          [div().child(shown)],
          this.rec(`wf-${row.key}`),
          (at) => this.openPick(at, `wf:${row.key}`, "choice", options, String(row.value)),
          { bordered: true, disabled: this.writing !== null },
        ),
        scope,
      });
    }
    if (row.control === "switch") {
      return this.row(t, {
        title: row.title,
        about: row.about,
        control: toggleSwitch(t, row.key, row.value === true, (on) => this.apply(row.key, on)),
        scope,
      });
    }
    if (row.control === "stepper") {
      return this.row(t, {
        title: row.title,
        about: row.about,
        control: this.stepper(t, row),
        scope,
      });
    }
    const box = this.boxOf(row.key, row.value, { rows: row.kind === "notes" ? 3 : 1 });
    const justSaved = Date.now() - box.savedAt < SAVED_MS;
    return this.row(t, {
      title: row.title,
      about: row.about,
      status: justSaved ? "Saved" : null,
      scope,
      below: v_flex()
        .w_full()
        .rounded(RADIUS.control)
        .border_1()
        .border_color(this.focusKey === row.key ? t.accent : t.border)
        .bg(t.raised)
        .overflow_hidden()
        .child(
          row.kind === "notes"
            ? Textarea.new(box.state).w_full().px(10).py(8)
            : Input.new(box.state).w_full().px(10).py(6),
        ),
    });
  }

  /** A number's two buttons and its value, inside the row's own bounds. */
  stepper(t, row) {
    const button = (id, glyph, by) =>
      div()
        .id(`${row.key}-${id}`)
        .w(H.sm)
        .h_full()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .text_size(TEXT.body)
        .text_color(t.muted)
        .hover((style) => style.bg(t.hover).text_color(t.foreground))
        .on_click(press(() => this.step(row, by)))
        .child(glyph);
    return h_flex()
      .h(H.sm)
      .flex_none()
      .items_center()
      .rounded(RADIUS.control)
      .border_1()
      .border_color(t.border)
      .bg(t.background)
      .overflow_hidden()
      .child(button("minus", "−", -1))
      .child(
        div()
          .min_w(52)
          .px(6)
          .flex_none()
          .text_size(TEXT.label)
          .text_color(t.foreground)
          .whitespace_nowrap()
          .child(row.value === 0 && row.zero !== undefined ? row.zero : String(row.value ?? row.min ?? 0)),
      )
      .child(button("plus", "+", 1));
  }

  // -- drawing: Remote -------------------------------------------------------

  /** Remote: the tunnel, the devices signed in, and Log out. */
  remotePage(t, colors) {
    return [this.title(t, "Remote"), this.tunnelSection(t, colors), this.sessionsSection(t, colors)];
  }

  /** A section's heading inside Remote. */
  heading(t, text) {
    return div().pt(14).pb(4).text_size(TEXT.body).font_medium().text_color(t.foreground).child(text);
  }

  /** What a section says while it has no data: reading, not available, or the failure. */
  remoteNote(t, colors, read, what) {
    if (read === null) return note(t, `Reading ${what}…`);
    if (read.unavailable) return note(t, `${what[0].toUpperCase()}${what.slice(1)} are not available on this service.`);
    if (read.error) return div().px(12).py(6).text_size(TEXT.label).text_color(colors.red).child(read.error);
    return null;
  }

  /** The tunnel: its state, its link (masked until asked for), and Start or Stop. */
  tunnelSection(t, colors) {
    const read = this.tunnelRead;
    const missing = this.remoteNote(t, colors, read, "the tunnel");
    const blocks = [this.heading(t, "Tunnel")];
    if (missing) return v_flex().w_full().children([...blocks, missing]);
    const view = tunnelView(read.data, this.provider);
    const starting = view.starting || this.remoteBusy === "start";
    blocks.push(
      h_flex()
        .gap(8)
        .items_center()
        .py(6)
        .child(div().w(6).h(6).flex_none().rounded(3).bg(view.on ? colors.green : t.dim))
        .child(div().text_size(TEXT.body).text_color(t.foreground).child(starting && !view.on ? "Starting…" : view.word))
        .when(view.live !== null, (el) => el.child(div().text_size(TEXT.label).text_color(t.muted).child(view.live)))
        .when(view.version !== null, (el) => el.child(div().text_size(TEXT.id).text_color(t.dim).child(view.version))),
    );
    if (view.why) blocks.push(note(t, view.why));
    if (view.tailnetUser) blocks.push(note(t, `Signed in through Tailscale as ${view.tailnetUser}.`));
    if (view.on && view.link) {
      blocks.push(
        h_flex()
          .gap(10)
          .items_center()
          .py(6)
          .child(
            div()
              .flex_1()
              .min_w(0)
              .truncate()
              .font_family("JetBrains Mono")
              .text_size(TEXT.id)
              .text_color(t.muted)
              .child(this.revealed ? view.link : maskLink(view.link)),
          )
          .child(
            link(t, "tunnel-reveal", this.revealed ? "Hide" : "Reveal", () => {
              this.revealed = !this.revealed;
              this.cx.notify();
            }),
          ),
      );
    }
    if (view.on) {
      blocks.push(
        h_flex()
          .py(6)
          .child(button(t, "tunnel-stop", this.remoteBusy === "stop" ? "Stopping…" : "Stop tunnel", { disabled: this.remoteBusy !== null, onClick: () => this.tunnelCall("stop") })),
      );
    } else {
      const options = view.providers.map((entry) => ({ id: entry.id, label: entry.word }));
      const shown = view.providers.find((entry) => entry.id === view.provider)?.word ?? "Choose";
      blocks.push(
        this.row(t, {
          title: "Provider",
          about: "What carries the tunnel.",
          control: selectButton(
            t,
            "provider",
            [div().child(shown)],
            this.rec("provider"),
            (at) => this.openPick(at, "provider", "provider", options, view.provider),
            { bordered: true },
          ),
        }),
      );
      if (view.note) blocks.push(note(t, view.note));
      blocks.push(
        h_flex()
          .py(6)
          .child(button(t, "tunnel-start", starting ? "Starting…" : "Start tunnel", { disabled: starting || this.remoteBusy !== null, onClick: () => this.tunnelCall("start") })),
      );
    }
    return v_flex().w_full().children(blocks);
  }

  /** The devices signed in, each with Revoke, and Log out all. */
  sessionsSection(t, colors) {
    const read = this.sessionRead;
    const missing = this.remoteNote(t, colors, read, "the signed-in devices");
    const blocks = [this.heading(t, "Signed in")];
    if (missing) return v_flex().w_full().children([...blocks, missing]);
    const rows = sessionRows(read.data);
    if (rows.length === 0) blocks.push(note(t, "No device is signed in."));
    for (const entry of rows) {
      blocks.push(
        h_flex()
          .w_full()
          .gap(10)
          .py(8)
          .items_center()
          .border_b(1)
          .border_color(t.border)
          .child(
            v_flex()
              .flex_1()
              .min_w(0)
              .gap(2)
              .child(
                h_flex()
                  .gap(6)
                  .items_center()
                  .child(div().text_size(TEXT.body).text_color(t.foreground).child(entry.label))
                  .when(entry.here, (el) => el.child(badge(t, "this device", t.raised, t.muted)))
                  .when(entry.remembered, (el) => el.child(div().text_size(TEXT.id).text_color(t.dim).child("remembered"))),
              )
              .child(div().text_size(TEXT.id).text_color(t.dim).child(entry.seen)),
          )
          .child(
            link(
              t,
              `revoke-${entry.id}`,
              this.remoteBusy === entry.id ? "Revoking…" : "Revoke",
              () => this.revokeSession(entry.id),
              this.remoteBusy !== null,
            ),
          ),
      );
    }
    if (rows.length > 0) {
      blocks.push(
        h_flex()
          .py(10)
          .child(
            link(
              t,
              "logout-all",
              this.remoteBusy === "logout" ? "Logging out…" : "Log out all",
              () => this.logoutAll(),
              this.remoteBusy !== null,
            ),
          ),
      );
    }
    return v_flex().w_full().children(blocks);
  }

  // -- drawing: Prompts ------------------------------------------------------

  /** Prompts: the list of templates, or the one being edited. */
  promptsPage(t, colors) {
    if (this.prompt !== null) return [this.title(t, this.prompt.id), this.promptEditor(t, colors)];
    const rows = promptRows(this.prompts);
    if (rows.length === 0) {
      return [
        this.title(t, "Prompts"),
        note(t, "No prompts yet."),
        note(t, "The service writes the shipped ones on first read."),
      ];
    }
    return [this.title(t, "Prompts"), ...rows.map((row) => this.promptRow(t, colors, row))];
  }

  /** One template in the list: its id, what it is for, and whether it is edited. */
  promptRow(t, colors, row) {
    const word = row.problem !== null ? colors.red : row.edited ? colors.amber : t.dim;
    return h_flex()
      .id(`prompt-${row.id}`)
      .h(SPACE.rowTwo)
      .w_full()
      .flex_none()
      .px(8)
      .gap(8)
      .items_center()
      .rounded(RADIUS.card)
      .border_b(1)
      .border_color(t.border)
      .cursor_pointer()
      .hover((style) => style.bg(t.hover))
      .on_click(press(() => this.openPrompt(row)))
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
                div().font_family("JetBrains Mono").text_size(TEXT.label).text_color(t.foreground).child(row.id),
              )
              .child(div().flex_none().text_size(TEXT.id).text_color(word).child(row.word)),
          )
          .child(div().text_size(TEXT.id).text_color(t.dim).child(row.problem ?? row.about)),
      )
      .child(div().flex_none().text_size(TEXT.id).text_color(t.dim).child("›"));
  }

  /**
   * The editor: what the file is, its words in a box that saves itself, and the
   * one way back to the shipped words.
   *
   * There is no Save button, as on every page of Shepherdr's settings: the words
   * are written a moment after typing stops, when the box loses the keyboard, and
   * when the window closes. Reset is not an edit — it asks the service to put the
   * words this build ships back (`DELETE /api/prompts/:id`).
   */
  promptEditor(t, colors) {
    const prompt = this.prompt;
    const writing = this.writing === `prompts.${prompt.id}`;
    const justSaved = Date.now() - this.promptSavedAt < SAVED_MS;
    return v_flex()
      .w_full()
      .gap(8)
      .pb(10)
      .child(
        h_flex()
          .gap(8)
          .items_center()
          .child(
            badge(
              t,
              prompt.state === "edited" ? "edited" : "shipped",
              prompt.state === "edited" ? colors.amber : t.raised,
              prompt.state === "edited" ? colors.on : t.muted,
            ),
          )
          .child(div().flex_1())
          .when(writing || justSaved, (el) =>
            el.child(
              div()
                .flex_none()
                .text_size(TEXT.id)
                .text_color(writing ? colors.amber : colors.green)
                .child(writing ? "saving…" : "Saved"),
            ),
          )
          .child(link(t, "prompt-back", "Back", () => this.leavePrompt())),
      )
      .child(div().font_family("JetBrains Mono").text_size(TEXT.id).text_color(t.dim).child(prompt.path))
      .child(div().text_size(TEXT.id).text_color(t.dim).child(promptLine(prompt)))
      .child(
        v_flex()
          .h(320)
          .flex_none()
          .w_full()
          .rounded(RADIUS.control)
          .border_1()
          .border_color(t.border)
          .bg(t.raised)
          .overflow_hidden()
          .child(Textarea.new(this.editor).w_full().h_full().px(10).py(8)),
      )
      .child(
        h_flex()
          .h(H.lg)
          .gap(6)
          .items_center()
          .child(link(t, "prompt-reset", "Reset to the shipped words", () => this.resetPrompt(), writing)),
      );
  }
}
