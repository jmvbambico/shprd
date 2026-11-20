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
// shprd's own twist: the third row is the service of the repository whose space
// the window shows (every repository has its own, side by side). A window whose
// board is empty because no service runs looks exactly like a run nobody added a
// task to, so this row says which repository it is and whether it is up, and it
// starts the service itself when the shown space has none. Nobody types
// `shprd serve`. Everything else in the window reads that service.
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
import { exec, hc, home, run, setShared, shared, theme } from "herdr";

import {
  SEND_PROBLEM,
  SERVICE_STATUS,
  activeFolder,
  discover,
  lastReason,
  listServices,
  normalizeFolder,
  pickService,
  reason,
  resolveShprd,
  repoName,
  stateDir,
  stateHome,
  terminalName,
} from "./lib/client.js";
import { icon, openSettings } from "./lib/panel.js";
import { SPACE, TEXT, tone } from "./lib/tokens.js";

/** How often the footer asks whether the service is up. */
const FLOOR_MS = 8000;
/** How often it looks at which space is shown, so a click on a space is answered at once. */
const WATCH_MS = 1200;
/** How long a failed start waits before the footer tries that repository again on its own. */
const RETRY_MS = 60_000;
/** How long a start waits for the service to write its record, ms. */
const UP_WAIT_MS = 12_000;

export default class Chrome extends View {
  init(_props, cx) {
    this.cx = cx;
    this.service = null;
    this.failure = null;
    this.starting = false;
    this.folder = activeFolder();
    this.failedAt = new Map();
    this.look();
    cx.timer.every(FLOOR_MS, (cx) => this.look(cx));
    cx.timer.every(WATCH_MS, (cx) => {
      if ((activeFolder() ?? null) !== (this.folder ?? null)) this.look(cx);
    });
  }

  /**
   * Looks for the service of the shown space, remembers what it found, and starts
   * it when the space names a repository nobody serves yet.
   *
   * The footer is the one owner of starting: every other panel only reads the
   * state it publishes (`downNotice`), so a click on a repo's space just works and
   * nobody types a command.
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
    const folder = activeFolder();
    if ((folder ?? null) !== (this.folder ?? null)) {
      this.folder = folder;
      this.failure = null;
    }
    const found = await discover({ wait: (ms) => cx.sleep(ms), folder });
    // The window moved to another space while this look was in flight: the answer
    // is for the one it left, so look again for the one it shows.
    if ((activeFolder() ?? null) !== (folder ?? null)) return this.inspect(cx);
    this.service = found;
    if (this.service) {
      this.failure = null;
      this.failedMap().delete(normalizeFolder(folder ?? ""));
    }
    this.publish();
    cx.notify();
    if (this.service === null && folder && !this.starting) {
      const failed = this.failedMap().get(normalizeFolder(folder));
      if (failed === undefined || Date.now() - failed > RETRY_MS) await this.start(folder);
    }
  }

  /** Whether a record for `folder` names a process that is still running. */
  async alive(folder) {
    const entries = await listServices(stateDir(home(), await stateHome()));
    const entry = pickService(entries, folder);
    if (!entry || !entry.record.pid) return false;
    const probe = await exec("kill", ["-0", String(entry.record.pid)]).catch(() => null);
    return probe?.code === 0;
  }

  /** When each repository's start last failed (made on first use). */
  failedMap() {
    this.failedAt ??= new Map();
    return this.failedAt;
  }

  /** Tells the other panels how the shown repository's service is doing. */
  publish() {
    const folder = this.folder;
    const phase = this.service ? "up" : this.starting ? "starting" : this.failure ? "failed" : "none";
    setShared(SERVICE_STATUS, {
      folder,
      repo: repoName(folder ?? ""),
      phase,
      reason: this.failure,
      port: this.service?.record.port ?? null,
    });
  }

  /**
   * Starts `shprd serve` for `folder`: the repository a space shows.
   *
   * The command is `hc run --bg`, so the call is the *start* of a terminal and
   * not the life of the server: it answers as soon as the core has it, and the
   * process stays in the core. The footer then waits for `service.json` itself, so
   * what it reports is the service's own answer and never this call's hope. A
   * service that dies at once leaves its words in the terminal; they are read back
   * and shown, and the dead terminal is ended so its name is free for the retry.
   *
   * The terminal is `--keep` (a crash leaves a shell, never a re-run) and named per
   * repository, so two repositories run side by side.
   */
  async start(folder) {
    if (this.starting) return;
    const key = normalizeFolder(folder);
    this.starting = true;
    this.failure = null;
    this.publish();
    this.cx.notify();
    const name = terminalName(folder);
    let launched = false;
    try {
      // A folder that is not a git repository yet is served all the same: the
      // service makes it one when the person sends their first chat message
      // (never because a space was merely shown), so nothing is refused here.
      // A record whose process is alive is a service that did not answer in time
      // (busy), not one that is gone: it is left running, never killed.
      if (await this.alive(folder)) {
        this.starting = false;
        this.failure = null;
        this.publish();
        this.cx.notify();
        return;
      }
      // A kept terminal that outlived its service still holds the name.
      // herdr-core starts the program with its own PATH (the one it was started
      // under, which lacks ~/.cargo/bin), so the start names `shprd` by its path
      // and hands it the PATH the person's login shell has.
      const where = await resolveShprd();
      if (!where.bin) {
        throw new Error(`shprd isn't installed where Herdr can find it (looked in ${shorten(where.looked, home())})`);
      }
      launched = true;
      await hc(["kill", name]).catch(() => null);
      const answer = await hc([
        "run", "--bg", "--keep", "--no-parent", "--name", name, "--cwd", folder, "--",
        "/usr/bin/env", `PATH=${where.path}`, where.bin, "serve",
      ]);
      if (answer?.code !== 0) {
        throw new Error(firstLine(answer?.stderr) || firstLine(answer?.stdout) || `hc run answered ${answer?.code}`);
      }
      for (let waited = 0; ; waited += 500) {
        await this.cx.sleep(500);
        this.service = await discover({ wait: (ms) => this.cx.sleep(ms), folder });
        if (this.service) break;
        if (waited >= UP_WAIT_MS) {
          const tail = await hc(["read", name, "--lines", "30"]).catch(() => null);
          throw new Error(lastReason(tail?.stdout) || "it did not come up in time");
        }
      }
    } catch (err) {
      this.failure = reason(err);
      this.failedMap().set(key, Date.now());
      if (launched) await hc(["kill", name]).catch(() => null);
    }
    this.starting = false;
    this.publish();
    this.cx.notify();
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
    const word = !this.folder
      ? "Add a folder to start"
      : online
        ? `${name}: service up`
        : this.starting
          ? `${name}: starting the service…`
          : this.failure
            ? `${name}: ${this.failure}`
            : `${name}: service not up`;
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
        word: stuck ? `${name}: ${stuck}` : word,
        icon: "plug",
        color: stuck ? colors.red : online ? colors.green : this.starting ? colors.amber : colors.red,
        onClick: online || this.starting || !this.folder ? null : () => this.start(this.folder),
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
