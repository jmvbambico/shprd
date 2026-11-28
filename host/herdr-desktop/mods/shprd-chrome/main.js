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
// shprd's own twist: the third row is the one shprd service. There is one for every
// repository (SHPR-77): it starts once, when this layout loads, and then keeps
// running across space switches and window restarts. Switching spaces only
// changes which repository the panels read, so the row says "service up" plus the
// repository the window shows, and the failure in one plain sentence when there
// is one. The footer also tells the service every space's folder, so a repository
// whose space is added gets its own run, tick and coordinator, and one whose space
// is closed stops being ticked. Nobody types `shprd serve`.
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
import { exec, hc, home, readTextFile, run, setShared, shared, spaces, theme } from "herdr";

import {
  Client,
  SEND_PROBLEM,
  SERVICE_STATUS,
  SERVICE_TERMINAL,
  activeFolder,
  discover,
  lastReason,
  normalizeFolder,
  readServiceRecord,
  reason,
  resolveShprd,
  repoName,
  serviceFile,
  stateDir,
  stateHome,
  terminalName,
} from "./lib/client.js";
import { icon, openSettings } from "./lib/panel.js";
import { SPACE, TEXT, tone } from "./lib/tokens.js";

/** How often the footer asks whether the service is up. */
const FLOOR_MS = 8000;
/** How often it looks at which space is shown and which spaces there are. */
const WATCH_MS = 1200;
/** How long a failed start waits before the footer tries again on its own. */
const RETRY_MS = 60_000;
/** How long a start waits for the service to write its record, ms. */
const UP_WAIT_MS = 12_000;
/** How long a self-update's `restarting` mark holds the footer's start back, s. */
const RESTARTING_FRESH_S = 150;

export default class Chrome extends View {
  init(_props, cx) {
    this.cx = cx;
    this.service = null;
    this.failure = null;
    this.starting = false;
    this.reconnecting = false;
    this.failedAt = null;
    this.folder = activeFolder();
    this.synced = "";
    this.cleaned = false;
    this.look();
    cx.timer.every(FLOOR_MS, (cx) => this.look(cx));
    cx.timer.every(WATCH_MS, (cx) => {
      // A switch of space is a repaint of the footer's words and, when the
      // spaces themselves changed, a word to the service: never a start.
      const folder = activeFolder();
      if ((folder ?? null) !== (this.folder ?? null)) {
        this.folder = folder;
        this.publish();
        cx.notify();
      }
      if (this.service && spaceFolders().join("\n") !== this.synced) this.sync();
    });
  }

  /**
   * Looks for the service, remembers what it found, and starts it when there is
   * none. The footer is the one owner of starting: every other panel only reads
   * the state it publishes (`downNotice`), so the window just works and nobody
   * types a command.
   */
  async check(cx = this.cx) {
    try {
      await this.inspect(cx);
    } catch (err) {
      // A look that throws must never be silent: the row says why.
      this.starting = false;
      this.service = null;
      this.failure = reason(err);
      this.publish();
      cx.notify();
    }
  }

  /** `check` fired from a timer or `init`: nothing is left unhandled. */
  look(cx = this.cx) {
    return this.check(cx).catch(() => {});
  }

  /** The look itself (see `check`). */
  async inspect(cx) {
    this.folder = activeFolder();
    const before = this.service?.record.id ?? null;
    const found = await discover({ wait: (ms) => cx.sleep(ms) });
    this.reconnecting = false;
    if (found) {
      this.service = found;
      this.failure = null;
      this.failedAt = null;
    } else if (!this.starting && (await this.alive() || (await this.replacing()))) {
      // One look missed (a window in the background answers late) or a
      // self-update is replacing the service: the recorded process is alive, or
      // being replaced, so the service is not down. The footer keeps what it
      // showed, or says "reconnecting", and neither starts nor kills anything.
      this.reconnecting = true;
      this.failure = null;
    } else {
      this.service = null;
    }
    if (this.service && this.service.record.id !== before) this.synced = "";
    this.publish();
    cx.notify();
    if (this.service === null && !this.reconnecting && !this.starting) {
      if (this.failedAt == null || Date.now() - this.failedAt > RETRY_MS) await this.start();
    }
    if (this.service && spaceFolders().join("\n") !== this.synced) await this.sync();
  }

  /**
   * Whether the recorded service's process is still running. A probe that could
   * not be run says nothing about the process, so it counts as running: only a
   * pid that is really gone starts a new service.
   */
  async alive() {
    const record = await readServiceRecord(serviceFile(stateDir(home(), await stateHome())));
    if (!record || !record.pid) return false;
    const probe = await exec("kill", ["-0", String(record.pid)]).catch(() => null);
    return probe === null || probe === undefined ? true : probe.code === 0;
  }

  /**
   * Whether a self-update is replacing the service right now (it leaves a fresh
   * `restarting` file in the state directory and starts the new service itself),
   * so the footer must not start one too.
   */
  async replacing() {
    const dir = stateDir(home(), await stateHome());
    const text = await readTextFile(`${dir}/restarting`).catch(() => null);
    const at = Number.parseInt(String(text ?? "").trim(), 10);
    return Number.isFinite(at) && Date.now() / 1000 - at < RESTARTING_FRESH_S;
  }

  /**
   * Tells the service the folder of every space the window has, so each one has
   * its own run, tick and coordinator before it is ever shown, and a closed one
   * stops being ticked. A failure is not the footer's to report: the panels
   * register the space they show themselves.
   */
  async sync() {
    const folders = spaceFolders();
    const key = folders.join("\n");
    try {
      const client = new Client(this.service, { wait: (ms) => this.cx.sleep(ms) });
      await client.syncRepos(folders);
      this.synced = key;
    } catch {
      // The next look tries again.
    }
  }

  /** Tells the other panels how the service is doing. */
  publish() {
    const folder = this.folder;
    const phase = this.service ? "up" : this.reconnecting ? "reconnecting" : this.starting ? "starting" : this.failure ? "failed" : "none";
    setShared(SERVICE_STATUS, {
      folder,
      repo: repoName(folder ?? ""),
      phase,
      reason: this.failure,
      port: this.service?.record.port ?? null,
    });
  }

  /**
   * Starts the one `shprd serve`.
   *
   * The command is `hc run --bg`, so the call is the *start* of a terminal and
   * not the life of the server: it answers as soon as the core has it, and the
   * process stays in the core, where terminals outlive the window. The footer then
   * waits for `service.json` itself, so what it reports is the service's own
   * answer and never this call's hope. A service that dies at once leaves its
   * words in the terminal; they are read back and shown, and the dead terminal is
   * ended so its name is free for the retry.
   *
   * The terminal is `--keep` and has one fixed name. Its folder is the service's
   * own state directory, never a repository: the service registers repositories
   * as it is told about them.
   */
  async start() {
    if (this.starting) return;
    this.starting = true;
    this.failure = null;
    this.publish();
    this.cx.notify();
    const name = SERVICE_TERMINAL;
    let launched = false;
    try {
      // A record whose process is alive is a service that did not answer in time
      // (busy), not one that is gone: it is left running, never killed — it
      // serves every repository, so ending it would stop them all.
      if ((await this.alive()) || (await this.replacing())) {
        this.starting = false;
        this.failure = null;
        this.publish();
        this.cx.notify();
        return;
      }
      // herdr-core starts the program with its own PATH (the one it was started
      // under, which lacks ~/.cargo/bin), so the start names `shprd` by its path
      // and hands it the PATH the person's login shell has.
      const where = await resolveShprd();
      if (!where.bin) {
        throw new Error(`shprd isn't installed where Herdr can find it (looked in ${shorten(where.looked, home())})`);
      }
      const dir = stateDir(home(), await stateHome());
      await exec("mkdir", ["-p", dir]).catch(() => null);
      launched = true;
      // A kept terminal that outlived its service still holds the name.
      await hc(["kill", name]).catch(() => null);
      const answer = await hc([
        "run", "--bg", "--keep", "--no-parent", "--name", name, "--cwd", dir, "--",
        "/usr/bin/env", `PATH=${where.path}`, where.bin, "serve",
      ]);
      if (answer?.code !== 0) {
        throw new Error(firstLine(answer?.stderr) || firstLine(answer?.stdout) || `hc run answered ${answer?.code}`);
      }
      for (let waited = 0; ; waited += 500) {
        await this.cx.sleep(500);
        this.service = await discover({ wait: (ms) => this.cx.sleep(ms) });
        if (this.service) break;
        if (waited >= UP_WAIT_MS) {
          const tail = await hc(["read", name, "--lines", "30"]).catch(() => null);
          throw new Error(lastReason(tail?.stdout) || "it did not come up in time");
        }
      }
      await this.endOldTerminals();
    } catch (err) {
      this.failure = reason(err);
      this.failedAt = Date.now();
      if (launched) await hc(["kill", name]).catch(() => null);
    }
    this.starting = false;
    this.publish();
    this.cx.notify();
  }

  /**
   * Ends the terminals an earlier version ran a repository's own service in: the
   * one service stopped those services when it started, and each left a shell.
   */
  async endOldTerminals() {
    if (this.cleaned) return;
    this.cleaned = true;
    for (const folder of spaceFolders()) await hc(["kill", terminalName(folder)]).catch(() => null);
  }

  render() {
    const t = theme();
    // Shepherdr's foot: `border_t`, `pt 4 pb 8`, three 28 px rows
    // (`mods/workspace-chrome-shepherdr/main.js:59-69`).
    return div()
      .size_full()
      .v_flex()
      .px(8)
      .pt(4)
      .pb(8)
      .border_t(1)
      .border_color(t.border)
      .children(this.actions(t).map((action) => this.row(t, action)));
  }

  /**
   * The footer's rows, in order: what each one says and what its click does.
   *
   * The first two are Shepherdr's (`mods/workspace-chrome-shepherdr`): the app's
   * settings and shprd's own; the third — shprd's twist — is the service. A row is
   * an icon and a word and nothing else, so the service's state is the word and
   * the icon's colour: green when it answers, amber while it starts, red when it
   * does not.
   */
  actions(t) {
    const online = this.service !== null;
    const name = repoName(this.folder ?? "");
    const word = online
      ? name
        ? `shprd: service up · ${name}`
        : "shprd: service up"
      : this.reconnecting
        ? "shprd: reconnecting…"
        : this.starting
        ? "shprd: starting the service…"
        : this.failure
          ? `shprd: ${this.failure}`
          : "shprd: service not up";
    const colors = tone(t);
    // A chat line that could not be sent (a folder git would not take) says why
    // here too, in the chat's own words, while the service itself is fine.
    const problem = shared(SEND_PROBLEM);
    const stuck =
      online && problem?.text && normalizeFolder(problem.folder ?? "") === normalizeFolder(this.folder ?? "")
        ? String(problem.text).slice(0, 140)
        : null;
    return [
      { id: "settings", word: "Settings", icon: "settings", color: t.muted, onClick: () => run("app.settings") },
      { id: "shprd", word: "shprd", icon: "crook", color: t.muted, onClick: () => openSettings() },
      {
        id: "service",
        word: stuck ? `${name || "shprd"}: ${stuck}` : word,
        icon: "plug",
        color: stuck ? colors.red : online ? colors.green : this.reconnecting ? t.muted : this.starting ? colors.amber : colors.red,
        onClick: online || this.starting || this.reconnecting ? null : () => this.start(),
      },
    ];
  }

  /** One 28 px row: a 14 px line icon, then the word at 13 px. */
  row(t, action) {
    const base = h_flex()
      .id(action.id)
      .h(SPACE.sidebarRow)
      .flex_none()
      .px(8)
      .gap(8)
      .items_center()
      .rounded(8)
      .text_size(TEXT.body)
      .text_color(t.foreground)
      .hover((s) => s.bg(t.hover));
    const live = action.onClick ? base.cursor_pointer().on_click(action.onClick) : base;
    return live.child(icon(action.icon, 14, action.color)).child(action.word);
  }
}

/**
 * The folder of every space the window has, one each, in a stable order. The
 * folder is sent as the window names it: the service compares it with the spaces'
 * own folders to adopt the one that is already there (SHPR-65), so a symlink
 * prefix must not be rewritten.
 */
function spaceFolders() {
  const seen = new Map();
  try {
    for (const space of spaces() ?? []) {
      const raw = space?.folder ? String(space.folder) : "";
      const key = normalizeFolder(raw);
      if (key && !seen.has(key)) seen.set(key, raw.replace(/\/+$/, ""));
    }
  } catch {
    // no spaces to list: nothing to tell the service.
  }
  return [...seen.entries()].sort(([a], [b]) => (a < b ? -1 : 1)).map(([, raw]) => raw);
}

/** Directories for a sentence: the home folder as `~`, a few of them. */
function shorten(dirs, base) {
  const home = String(base ?? "").replace(/\/+$/, "");
  const names = dirs.map((dir) => (home && dir.startsWith(`${home}/`) ? `~${dir.slice(home.length)}` : dir));
  return names.length > 4 ? `${names.slice(0, 4).join(", ")} and ${names.length - 4} more` : names.join(", ");
}

/** The first line of what a command said, or an empty string. */
function firstLine(text) {
  return String(text ?? "").split("\n")[0].trim();
}
