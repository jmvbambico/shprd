// Toasts: what shprd has to tell a person who is looking somewhere else.
//
// The web's copy of the Herdr mods' `lib/toasts.js`, with Shepherdr's numbers:
// three at a time, a body cut at 140 characters, a dot in the level's colour and
// a timeout per level (20 s for an alert, 12 s for an error, 10 s for a warning,
// 6 s for the rest). Every toast carries the seq of the event that made it, so
// a batch replayed after a reconnect adds nothing: one event, one toast.
//
// - Source: <https://github.com/eliasstravik/shepherdr>
// - Paths: `core/toasts.js` (`SHOWN`, `BODY_MAX`, `TIMEOUT`, `clip`),
//   `mods/toasts/main.js`
// - Commit: `a3f327a` (tag `v3.1.34`)
// - License: MIT — Copyright (c) 2026 Elias Stravik

import type { EventNoticeDto, SnapshotDto } from "../gen";

/** How many toasts are on screen at once. */
export const SHOWN = 3;
/** How many characters a body is cut at. */
export const BODY_MAX = 140;
/** How long each level stays on screen, ms. */
export const TIMEOUT: Record<string, number> = { alert: 20_000, error: 12_000, warning: 10_000 };
/** How long a level with no timeout of its own stays, ms. */
export const TIMEOUT_DEFAULT = 6_000;
/** How many are kept besides the newest one. */
export const KEEP = 4;
/** How long one is kept in the list at all, ms. */
export const KEEP_MS = 120_000;

/** A toast's level. */
export type Level = "success" | "warning" | "error" | "alert" | "info";

/** One toast as the pure functions hold it. */
export interface NewsItem {
  id: string;
  level: Level;
  title: string;
  body: string;
  task: string;
}

/** A kept toast: the news with the time it arrived. */
export interface Toast extends NewsItem {
  at: number;
}

/** How long `item` stays on screen. */
export function timeoutOf(item: Pick<NewsItem, "level">): number {
  return TIMEOUT[item.level] ?? TIMEOUT_DEFAULT;
}

/** The colour a level is drawn in. */
export function levelTone(level: Level): "red" | "amber" | "green" | "accent" {
  if (level === "alert" || level === "error") {
    return "red";
  }
  if (level === "warning") {
    return "amber";
  }
  return level === "success" ? "green" : "accent";
}

/** `text` cut to `max` characters, ending in an ellipsis when it was cut. */
export function clip(text: unknown, max = BODY_MAX): string {
  const value = String(text ?? "").trim();
  return value.length > max ? `${value.slice(0, max - 1).trimEnd()}…` : value;
}

/** The change kinds that are news, and the level each is told at. */
export const NEWS: Record<string, Level> = {
  task_landed: "success",
  escalated: "warning",
  ask_raised: "warning",
  gate_failed: "error",
};

/** The toast id for one event: its seq, which is what dedupes it. */
export function eventId(seq: number): string {
  return `ev:${seq}`;
}

/** The toast id of "a pass failed", keyed by that pass's timestamp. */
export function passId(at: number): string {
  return `tick:${at}`;
}

function taskOf(state: SnapshotDto | null, id: string) {
  return (state?.tasks ?? []).find((entry) => entry.id === id);
}

/** The failed gate a `gate_failed` event named: which one and the first finding's title. */
export function failedGate(state: SnapshotDto | null, task: string): { gate: string; words: string } | null {
  const attempts = taskOf(state, task)?.attempts ?? [];
  const latest = attempts[attempts.length - 1];
  const gates = (latest?.gates ?? []).filter((g) => g.passed === false);
  const gate = gates[gates.length - 1];
  if (!gate) {
    return null;
  }
  return { gate: gate.gate, words: clip(gate.findings[0]?.title ?? `${gate.gate} failed`) };
}

/** The toasts a batch of change events asks for, newest last. */
export function newsItems(changes: EventNoticeDto[], state: SnapshotDto | null): NewsItem[] {
  const items: NewsItem[] = [];
  for (const change of changes) {
    const level = NEWS[change.kind];
    if (!level) {
      continue;
    }
    const task = change.task ?? "";
    const row = taskOf(state, task);
    if (level === "success") {
      const branch = state?.run?.integration_branch ?? "its integration branch";
      items.push({
        id: eventId(change.seq),
        level,
        title: task ? `${task} · Landed` : "Landed",
        body: clip(`${row?.title ? `${row.title} · ` : ""}its work is on ${branch}`),
        task,
      });
    } else if (level === "warning") {
      // A task that is over needs nothing: the board does not show a card for it.
      if (row?.status === "integrated" || row?.status === "dropped") {
        continue;
      }
      const card = (state?.needs ?? []).find((n) => n.task === task);
      items.push({
        id: eventId(change.seq),
        level,
        title: task ? `${task} · Needs you` : "Needs you",
        body: card?.body ? clip(card.body) : row?.derived.reason ? clip(row.derived.reason) : "It waits for you.",
        task,
      });
    } else {
      const failed = failedGate(state, task);
      const what = !failed || failed.gate === "review" ? "Review failed" : `${failed.gate} failed`;
      items.push({
        id: eventId(change.seq),
        level,
        title: task ? `${task} · ${what}` : what,
        body: failed?.words ?? "Nothing moves without you.",
        task,
      });
    }
  }
  return items;
}

/** The toast a failed pass asks for, or null when the last pass was clean. */
export function passItem(state: SnapshotDto | null): NewsItem | null {
  const tick = state?.last_tick;
  const failed = tick?.failed ?? [];
  const first = failed[0];
  if (!tick || !first) {
    return null;
  }
  const where = first.task ? ` on ${first.task}` : "";
  const others = failed.length - 1;
  return {
    id: passId(tick.at_ms),
    level: "error",
    title: "The last pass failed",
    body: clip(`${first.step}${where}: ${first.error}${others > 0 ? ` (+${others})` : ""}`),
    task: first.task ?? "",
  };
}

/** Everything one reading of the service asks for: the news, then the pass. */
export function fromSnapshot(changes: EventNoticeDto[], state: SnapshotDto | null): NewsItem[] {
  const items = newsItems(changes, state);
  const pass = passItem(state);
  if (pass) {
    items.push(pass);
  }
  return items;
}

/**
 * The list with `items` added: newest last, one per id, nothing stale. An item
 * whose id is already there replaces the old one in place.
 */
export function mergeNews(list: Toast[], items: NewsItem[], at = Date.now()): Toast[] {
  const next = list.filter((item) => at - item.at < KEEP_MS);
  for (const item of items) {
    const index = next.findIndex((entry) => entry.id === item.id);
    const existing = next[index];
    if (existing === undefined) {
      next.push({ ...item, at });
    } else {
      next[index] = { ...existing, ...item, at: existing.at };
    }
  }
  return next.slice(-(KEEP + 1));
}

/** The list without `id`. */
export function dismiss(list: Toast[], id: string): Toast[] {
  return list.filter((item) => item.id !== id);
}

/** The toasts on screen at `at`: not dismissed, not expired, at most [`SHOWN`]. */
export function visible(list: Toast[], at = Date.now(), dismissed: ReadonlySet<string> = new Set()): Toast[] {
  return list
    .filter((item) => !dismissed.has(item.id))
    .filter((item) => at - item.at < timeoutOf(item))
    .slice(-SHOWN);
}
