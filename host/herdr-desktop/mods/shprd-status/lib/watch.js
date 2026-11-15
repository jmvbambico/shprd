// Watching a change file instead of polling blind.
//
// A Herdr mod cannot use `setTimeout` (calling it throws) and cannot read a
// stream (`fetch` is buffered, and `WebSocket`/`EventSource` are undefined), so
// the live arm of a panel is `watchFolder` plus `cx.timer.every` as the floor.
// The M7ef spike measured both on a real window (§2.11): `watchFolder` woke 14–20
// times over 20 s on a file rewritten every 2 s, and `cx.timer.every` has
// ~50–125 ms granularity.
//
// The shape is Shepherdr's inbox watcher (`mods/tabs-tasks/inbox.js`): watch a
// folder, read what changed, keep a timer as the fallback arm.
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `mods/tabs-tasks/inbox.js`
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik
//
// shprd's own change file: `shprd serve` rewrites `<state>/shprd/changes` after
// every ledger write, and the file holds `{"seq": <n>, "at_ms": <n>}`
// (`crates/shprd-server/src/changes.rs`). It is the one thing a mod can follow
// with no HTTP call at all, and a write by `shprd` on a command line — a worker's
// `shprd task status`, a program's ask hook — bumps it too.
//
// Shared: the source is `mods/lib/watch.js`; `bin/sync-shared` copies it into
// every mod folder as `lib/watch.js`. Edit the source, never a copy.

import { readTextFile, watchFolder } from "herdr";

import { changeFile } from "./client.js";

/** What the change file holds, or null when it is absent or unreadable. */
export function parseChange(text) {
  if (typeof text !== "string" || text.trim() === "") return null;
  try {
    const value = JSON.parse(text);
    if (!value || typeof value !== "object") return null;
    const seq = Number.isInteger(value.seq) ? value.seq : null;
    const at = Number.isInteger(value.at_ms) ? value.at_ms : null;
    if (seq === null && at === null) return null;
    return { seq, at: at === null ? null : at };
  } catch {
    return null;
  }
}

/** Whether two readings of the change file say the same thing. */
export function sameChange(left, right) {
  if (left === null || right === null) return left === right;
  return left.seq === right.seq && left.at === right.at;
}

/**
 * Whether a batch of changed paths is worth a re-read.
 *
 * `watchFolder` answers absolute paths, and the state directory holds the token
 * and the discovery record too, so a batch that names only those is not a ledger
 * write. An empty batch, and a batch naming the folder itself, both mean "look
 * again", which is what `watchFolder` documents.
 */
export function changeMatters(paths, dir, name = "changes") {
  if (!Array.isArray(paths) || paths.length === 0) return true;
  const full = `${dir}/${name}`.replace(/\/+/g, "/");
  return paths.some((path) => {
    const text = String(path ?? "").replace(/\/+$/, "");
    if (text === dir || text === full) return true;
    // macOS reports a `/var/...` path in its resolved `/private/var/...` form
    // (the M7ef spike saw exactly that), so the tail is what is portable.
    return text.endsWith(`/${name}`);
  });
}

/**
 * Reads the change file once: `{seq, at}` or null.
 *
 * `read` is Herdr's `readTextFile` unless a caller supplies its own, which is how
 * the unit test drives this without an app.
 */
export async function readChange(dir, read = readTextFile) {
  return parseChange(await read(changeFile(dir)).catch(() => null));
}

/**
 * Follows the change file for as long as the copy lives.
 *
 * `onChange(change)` is called whenever the file really moved. The loop ends when
 * the copy is closed and the runtime stops its `cx` uses, which is how every
 * Herdr mod task ends; a watch that rejects (a tree too large, a folder not made
 * yet) ends the loop rather than the panel, since the timer is the floor.
 */
export async function followChanges(dir, onChange, io = {}) {
  const read = io.read ?? readTextFile;
  const watch = io.watch ?? watchFolder;
  let last = await readChange(dir, read);
  if (last !== null) onChange(last);
  try {
    for (;;) {
      const paths = await watch(dir);
      if (!changeMatters(paths, dir)) continue;
      const change = await readChange(dir, read);
      if (change === null || sameChange(change, last)) continue;
      last = change;
      onChange(change);
    }
  } catch {
    return;
  }
}
