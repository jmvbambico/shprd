// Dictation into the composer, by the mechanism Shepherdr's own chat uses: its recorder
// (`mods/chat/record.ts`: the default microphone, its level, and the words streamed to
// ElevenLabs' speech to text while the person speaks) and its writer (`transcribe.ts`,
// when the live words did not come). Both live in Shepherdr's installed chat mod and need
// its ElevenLabs key (Shepherdr's Settings › Secrets) and `bun`; this mod runs them as
// they are and copies none of their code. Where they are not there, `available()` says
// so and the composer has no mic rather than a dead one.
//
// The text goes into the box to edit: nothing is sent by itself and no audio is kept.

import { appHome, exec, readTextFile } from "herdr";

const SHEPHERDR = () => `${appHome()}/mods/chat`;
const DIR = () => `${appHome()}/data/shprd-chat-dictation`;

/** bun where a Dock-launched window's PATH lacks it. */
const BUN_PATH = 'PATH="$PATH:$HOME/.bun/bin:$HOME/.local/bin:/opt/homebrew/bin:/usr/local/bin"; export PATH; ';

/** Whether Shepherdr's recorder, its writer and `bun` are all here. */
export async function available() {
  const r = await exec("sh", [
    "-c",
    `${BUN_PATH}command -v bun >/dev/null && [ -f "$1/record.ts" ] && [ -f "$1/transcribe.ts" ] && echo yes; true`,
    "x",
    SHEPHERDR(),
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
      BUN_PATH +
        'mkdir -p "$(dirname "$1")" && rm -f "$1" "$2" "$1.txt" && { bun "$3" "$1" "$2" </dev/null >/dev/null 2>&1 & echo $! > "$1.pid"; wait; }',
      "x",
      this.file,
      this.levelFile,
      `${SHEPHERDR()}/record.ts`,
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
      if (typeof said?.text !== "string") {
        const r = await exec("sh", ["-c", BUN_PATH + 'bun "$1" "$2" </dev/null 2>/dev/null', "x", `${SHEPHERDR()}/transcribe.ts`, file]);
        try {
          said = JSON.parse(r.stdout.trim().split("\n").pop());
        } catch {
          said = null;
        }
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
