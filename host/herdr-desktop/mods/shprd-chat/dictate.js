// Dictation into the composer, on this Mac and with no key: a small helper of shprd's own
// (`dictate-helper.js`, built once with the Mac's `swiftc`) records the default microphone while
// Apple's speech recognizer writes the words down on-device. No account, no key, no audio leaves
// the machine and none is kept. Where that cannot run (not a Mac, no `swiftc`), `available()` says
// so and the composer has no mic rather than a dead one.
//
// The text goes into the box to edit: nothing is sent by itself.

import { appHome, exec, readTextFile } from "herdr";
import { INFO_PLIST, SOURCE } from "./dictate-helper.js";

const DIR = () => `${appHome()}/data/shprd-chat-dictation`;

/** A short stable name for this version of the helper, so a changed helper is rebuilt. */
function stamp(text) {
  let h = 5381;
  for (let i = 0; i < text.length; i += 1) h = ((h * 33) ^ text.charCodeAt(i)) >>> 0;
  return h.toString(16);
}

/** Where the built helper lives (one folder per version of its source). */
const HELPER_DIR = () => `${DIR()}/helper-${stamp(SOURCE + INFO_PLIST)}`;

/**
 * A shell snippet that builds the helper into "$H" when it is not there ($H, $SRC and $PLIST are
 * its arguments' names); it leaves the build's complaint in "$H/build.log".
 */
const BUILD =
  '[ -x "$H/shprd-dictate" ] || { mkdir -p "$H" && printf %s "$SRC" > "$H/dictate.swift" && printf %s "$PLIST" > "$H/Info.plist" && ' +
  'swiftc -O -o "$H/shprd-dictate.tmp" "$H/dictate.swift" -Xlinker -sectcreate -Xlinker __TEXT -Xlinker __info_plist -Xlinker "$H/Info.plist" >"$H/build.log" 2>&1 && ' +
  'mv "$H/shprd-dictate.tmp" "$H/shprd-dictate"; }';

/** Whether this is a Mac whose `swiftc` has built the helper (builds it the first time: about half a minute). */
export async function available() {
  const r = await exec("sh", [
    "-c",
    `[ "$(uname)" = Darwin ] && command -v swiftc >/dev/null && H="$1" SRC="$2" PLIST="$3" && ${BUILD}; [ -x "$1/shprd-dictate" ] && echo yes; true`,
    "x",
    HELPER_DIR(),
    SOURCE,
    INFO_PLIST,
  ]).catch(() => null);
  return r?.stdout?.trim() === "yes";
}

/** One dictation: `idle`, `recording` or `writing`. */
export class Dictation {
  constructor(key = "chat") {
    this.key = String(key).replace(/[^\w.-]/g, "_");
    this.state = "idle";
    this.error = null;
    this.run = null;
    this.n = 0;
  }

  get file() {
    return `${DIR()}/${this.key}-${this.n}.wav`;
  }

  get levelFile() {
    return `${DIR()}/${this.key}-${this.n}.level`;
  }

  /** Starts recording. */
  start() {
    if (this.state !== "idle") return;
    this.error = null;
    this.state = "recording";
    this.n = Date.now();
    this.run = exec("sh", [
      "-c",
      `H="$3" SRC="$4" PLIST="$5"; ${BUILD}; ` +
        'mkdir -p "$(dirname "$1")" && rm -f "$1" "$2" "$1.txt" && { [ -x "$3/shprd-dictate" ] || { printf "!%s" "The dictation helper could not be built on this Mac." > "$2"; exit 0; }; ' +
        '"$3/shprd-dictate" "$1" "$2" </dev/null >/dev/null 2>&1 & echo $! > "$1.pid"; wait; }',
      "x",
      this.file,
      this.levelFile,
      HELPER_DIR(),
      SOURCE,
      INFO_PLIST,
    ]).catch(() => null);
  }

  /** How loud the microphone is, 0..1; a microphone that cannot work cancels and says why. */
  async level() {
    const text = (await readTextFile(this.levelFile).catch(() => null)) ?? "0";
    if (text.startsWith("!") && this.state === "recording") {
      this.cancel();
      this.error = text.slice(1).trim() || "The microphone didn't start.";
      return 0;
    }
    const value = Number(text);
    return Number.isFinite(value) ? value : 0;
  }

  /** Stops and writes it down: the words ("" when nothing was heard, or it was cancelled). */
  async stop() {
    if (this.state !== "recording") return "";
    this.state = "writing";
    const n = this.n;
    const file = this.file;
    const levelFile = this.levelFile;
    const run = this.run;
    const mine = () => this.n === n && this.state === "writing";
    try {
      await exec("sh", ["-c", 'p=$(cat "$1.pid" 2>/dev/null) && kill -INT "$p" 2>/dev/null; rm -f "$1.pid"; true', "x", file]);
      await run;
      if (!mine()) return "";
      let said = null;
      try {
        said = JSON.parse((await readTextFile(`${file}.txt`).catch(() => null)) ?? "");
      } catch {
        said = null;
      }
      if (!mine()) return "";
      if (typeof said?.text === "string") return said.text;
      throw new Error(typeof said?.error === "string" && said.error ? said.error : "Couldn't write that down. Try again.");
    } catch (err) {
      if (mine()) this.error = String(err?.message ?? err);
      return "";
    } finally {
      if (this.n === n) this.state = "idle";
      exec("rm", ["-f", file, levelFile, `${file}.txt`]).catch(() => {});
    }
  }

  /** Throws the recording away. */
  cancel() {
    if (this.state === "recording") {
      exec("sh", [
        "-c",
        'p=$(cat "$1.pid" 2>/dev/null) && kill -TERM "$p" 2>/dev/null; rm -f "$1.pid" "$1" "$2"; true',
        "x",
        this.file,
        this.levelFile,
      ]).catch(() => {});
    }
    if (this.state !== "idle") this.n += 1;
    this.state = "idle";
  }
}
